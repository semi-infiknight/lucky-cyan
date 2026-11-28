//! [`FadeCue`] — fades the volume/brightness of one or more running cues, or
//! crossfades them into another visual cue.
//!
//! On GO the show layer (`show/crossfade.rs`) hands the Fade its targets'
//! voices and the Fade interpolates them in `tick()`:
//! - **audio** voices (audio cues, video sound tracks, group children): gain
//!   and/or pan;
//! - **pictures** (video, image, camera layers — a group's included): their own
//!   layer opacity, so other layers are untouched.
//!
//! `target_gain_linear = 0.0` → fade to black/silence.
//! `target_gain_linear = 1.0` → fade to full brightness/unity volume.
//!
//! With `crossfade_into`, the Fade starts that cue and the output engine
//! dissolves the targets' pictures into it.  The Fade's action clock starts on
//! the incoming picture's first frame (and pauses with the dissolve), the
//! incoming sound rises from silence while the targets' sound falls to it, and
//! the dissolved targets are stopped at the end — a crossfade replaces them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    cue::types::db_to_linear,
    engine::{
        cue_effects::{EQ_GAIN_RANGE, MAX_FREQUENCY, MIN_FREQUENCY},
        output_engine::{
            CrossfadePhase, PictureColorParameter, SilentSpeedOrigin, VideoGeometry, VideoGeometryUpdate, VideoTranslation,
        },
        rate_fader::{normalize_rate, RateFade, RateFader, ScalarFade, ScalarFader},
        ring_command::FadeCurve as EngineFadeCurve,
        OutputEngineApi,
    },
};

use super::{
    context::{CueContext, CueEvent},
    curve::{CurveKind, FadeShapes},
    traits::{Cue, CueFactory},
    types::{ContinueMode, CueColor, CueId, CueState, CueType, CutoffDestination, EffectDestination, EqGainDestination, FadeAction, FadeCrossfade, FadeCurve, CUE_TICK_INTERVAL},
};

// ---------------------------------------------------------------------------
// FadeCue
// ---------------------------------------------------------------------------

const MAX_TRANSLATION_WAYPOINTS: usize = 16;
const SCALE_DESTINATION_RANGE: std::ops::RangeInclusive<f64> = 0.05..=8.0;
const ROTATION_TURN_RANGE: std::ops::RangeInclusive<f64> = -3600.0..=3600.0;
const ROTATION_TARGET_REQUIRED: &str = "A Rotation Fade needs one still PNG or JPEG Image with continuous still rotation enabled";

#[derive(Default)]
enum VisualGeometryRun {
    #[default]
    Idle,
    Pending {
        target: CueId,
        destination: VideoGeometryUpdate,
        rotation_delta_degrees: Option<f64>,
        relative: Option<RelativeGeometry>,
        waypoints: Vec<[f64; 2]>,
        smooth_path: bool,
        color_destinations: [Option<f32>; 4],
    },
    Declined,
    Active {
        target: CueId,
        voice: CueId,
        owner: Uuid,
        start: Option<VideoGeometry>,
        destination: VideoGeometryUpdate,
        rotation_delta_degrees: Option<f64>,
        waypoints: Vec<[f64; 2]>,
        smooth_path: bool,
        color: [Option<(f32, f32)>; 4],
    },
}

impl VisualGeometryRun {
    fn update(&mut self, context: &CueContext, rising: f64, falling: f64, elapsed: f64) -> bool {
        let Self::Active { voice, owner, start, destination, rotation_delta_degrees,
            waypoints, smooth_path, color, .. } = self else { return false };
        if start.is_some_and(|origin| {
            let update = sample_visual_geometry(
                origin, *destination, *rotation_delta_degrees, waypoints, *smooth_path, (rising, falling, elapsed),
            );
            !context.output_engine.set_voice_geometry_fade(*voice, *owner, update)
        }) {
            *start = None;
        }
        if color.iter().any(Option::is_some) {
            let values = color.map(|field| field.map(|(origin, end)| {
                sample_picture_value(origin.into(), end.into(), rising, falling, elapsed) as f32
            }));
            let written = context.output_engine.set_voice_color_fade(*voice, *owner, values);
            for (field, written) in color.iter_mut().zip(written) {
                if !written { *field = None; }
            }
        }
        start.is_some() || color.iter().any(Option::is_some)
    }

    fn completed_picture(&self) -> Option<CompletedPictureFade> {
        let Self::Active { target, voice, owner, start, destination, color, .. } = self else {
            return None;
        };
        let geometry = start.and_then(|origin| {
            let saved = |value: f64, end: Option<f64>| {
                end.filter(|&end| value != end && value.is_finite()).map(|_| value)
            };
            let translation = destination.translation.and_then(|position| {
                let x = saved(origin.pan_x, position.x);
                let y = saved(origin.pan_y, position.y);
                (x.is_some() || y.is_some()).then_some(VideoTranslation { x, y })
            });
            let scale = saved(origin.scale, destination.scale).filter(|scale| *scale > 0.0);
            let rotation_degrees = origin.source_rotation_degrees
                .filter(|&angle| VideoGeometryUpdate::valid_rotation(angle))
                .and_then(|angle| saved(angle, destination.rotation_degrees));
            (translation.is_some() || scale.is_some() || rotation_degrees.is_some())
                .then_some(VideoGeometryUpdate { translation, scale, rotation_degrees })
        });
        let color = color.map(|field| field.filter(|(start, end)| start != end).map(|(start, _)| start));
        (geometry.is_some() || color.iter().any(Option::is_some)).then_some(CompletedPictureFade {
            target: *target,
            voice: *voice,
            owner: *owner,
            geometry,
            color,
        })
    }
}

struct CompletedPictureFade {
    target: CueId,
    voice: CueId,
    owner: Uuid,
    geometry: Option<VideoGeometryUpdate>,
    color: [Option<f32>; 4],
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct PictureFadeRevertResult {
    pub geometry: bool,
    pub saturation: bool,
    pub other_color: Vec<PictureColorParameter>,
}

/// Changes to apply to one picture's geometry when its Fade starts.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelativeGeometry {
    pub translation: Option<VideoTranslation>,
    pub scale_factor: Option<f64>,
}

impl RelativeGeometry {
    fn active(self) -> bool {
        self.translation.is_some() || self.scale_factor.is_some()
    }

    fn resolve(self, start: VideoGeometry) -> VideoGeometryUpdate {
        VideoGeometryUpdate {
            translation: self.translation.map(|change| VideoTranslation {
                x: change.x.map(|offset| (start.pan_x + offset).clamp(-1.0, 1.0)),
                y: change.y.map(|offset| (start.pan_y + offset).clamp(-1.0, 1.0)),
            }),
            scale: self.scale_factor.map(|factor| {
                (start.scale.max(0.01) * factor)
                    .clamp(*SCALE_DESTINATION_RANGE.start(), *SCALE_DESTINATION_RANGE.end())
            }),
            ..Default::default()
        }
    }
}

fn sample_picture_value(start: f64, end: f64, rising: f64, falling: f64, elapsed: f64) -> f64 {
    if elapsed >= 1.0 { return end; }
    let progress = if end >= start { rising } else { falling };
    start + (end - start) * progress
}

fn wrap_rotation(angle: f64) -> f64 {
    angle.rem_euclid(360.0) % 360.0
}

fn sample_absolute_rotation(start: f64, end: f64, rising: f64, falling: f64, elapsed: f64) -> f64 {
    if elapsed >= 1.0 { return end; }
    let start = start.rem_euclid(360.0);
    let clockwise = (end - start).rem_euclid(360.0);
    let rotation = if clockwise > 180.0 { clockwise - 360.0 } else { clockwise };
    wrap_rotation(sample_picture_value(start, start + rotation, rising, falling, elapsed))
}

fn sample_visual_geometry(
    start: VideoGeometry, destination: VideoGeometryUpdate, rotation_delta_degrees: Option<f64>,
    waypoints: &[[f64; 2]], smooth_path: bool, (rising, falling, elapsed): (f64, f64, f64),
) -> VideoGeometryUpdate {
    if elapsed >= 1.0 { return destination; }
    let sample = |start, end| sample_picture_value(start, end, rising, falling, elapsed);
    let translation = destination.translation.map(|position| {
        if let (false, Some(x), Some(y)) = (waypoints.is_empty(), position.x, position.y) {
            let [x, y] = sample_translation_path([start.pan_x, start.pan_y], waypoints, [x, y], elapsed, smooth_path);
            return VideoTranslation { x: Some(x), y: Some(y) };
        }
        VideoTranslation {
            x: position.x.map(|x| sample(start.pan_x, x)),
            y: position.y.map(|y| sample(start.pan_y, y)),
        }
    });
    VideoGeometryUpdate {
        translation,
        scale: destination.scale.map(|scale| sample(start.scale.max(0.01), scale)),
        rotation_degrees: start.source_rotation_degrees.zip(destination.rotation_degrees)
            .map(|(from, to)| {
                if let Some(delta) = rotation_delta_degrees {
                    let from = wrap_rotation(from);
                    wrap_rotation(sample_picture_value(from, from + delta, rising, falling, elapsed))
                } else {
                    sample_absolute_rotation(from, to, rising, falling, elapsed)
                }
            }),
    }
}

fn sample_translation_path(
    start: [f64; 2], waypoints: &[[f64; 2]], end: [f64; 2], progress: f64, smooth: bool,
) -> [f64; 2] {
    if progress >= 1.0 { return end; }
    // Both axes use the same progress along an equal-duration leg. Applying
    // independent rising/falling slopes would draw a different path.
    let travelled = progress.max(0.0) * (waypoints.len() + 1) as f64;
    let leg = (travelled.floor() as usize).min(waypoints.len());
    let from = if leg == 0 { start } else { waypoints[leg - 1] };
    let to = waypoints.get(leg).copied().unwrap_or(end);
    let fraction = travelled - leg as f64;
    let lerp = |a, b| a + (b - a) * fraction;
    if !smooth {
        return std::array::from_fn(|axis| lerp(from[axis], to[axis]));
    }

    // Snap only rounding error at an exact leg boundary. A repeated point
    // holds still for this leg, with a zero tangent on both adjoining legs.
    let tolerance = f64::EPSILON * (waypoints.len() + 1) as f64;
    if fraction <= tolerance || from == to { return from; }
    if 1.0 - fraction <= tolerance { return to; }
    let previous = if leg < 2 { start } else { waypoints[leg - 2] };
    let next = waypoints.get(leg + 1).copied().unwrap_or(end);
    let outgoing = translation_handle(previous, from, to);
    let incoming = translation_handle(from, to, next);
    std::array::from_fn(|axis| {
        let a = lerp(from[axis], from[axis] + outgoing[axis]);
        let b = lerp(from[axis] + outgoing[axis], to[axis] - incoming[axis]);
        let c = lerp(to[axis] - incoming[axis], to[axis]);
        lerp(lerp(a, b), lerp(b, c))
    })
}

/// Both legs share this Bezier handle so their velocity meets smoothly. Limit
/// the handles on both sides of the point to the supported coordinate range.
fn translation_handle(previous: [f64; 2], point: [f64; 2], next: [f64; 2]) -> [f64; 2] {
    if previous == point || point == next { return [0.0; 2]; }
    std::array::from_fn(|axis| {
        let room = (1.0 - point[axis].abs()).max(0.0);
        ((next[axis] - previous[axis]) / 6.0).clamp(-room, room)
    })
}

fn path_loop_progress(elapsed: Duration, period: Duration) -> f64 {
    let period_ns = period.as_nanos();
    let phase_ns = elapsed.as_nanos() % period_ns;
    if phase_ns == 0 && !elapsed.is_zero() {
        // Preserve the exact endpoint at the boundary. The next instant is
        // back at the start of the same path, without a hold or reverse leg.
        1.0
    } else {
        phase_ns as f64 / period_ns as f64
    }
}

#[derive(Default)]
enum MapMotionState {
    #[default]
    Idle,
    Pending(CueId, super::point_mix::MapMotion),
    Running(Box<super::point_mix::MapMotionRun>),
    Done,
}

#[derive(Default)]
enum EffectRun {
    #[default]
    Idle,
    Pending { target: CueId, destination: EffectDestination },
    Active(ScalarFade),
    HeldEqGain,
    Unavailable,
    Finished,
}

#[derive(Default)]
enum SilentSpeedRun {
    #[default]
    Idle,
    Pending { target: CueId, rate: f64 },
    WaitingForOrigin { voice: CueId, owner: Uuid, rate: f64 },
    Active { voice: CueId, owner: Uuid, start: f64, rate: f64 },
    Done,
}

pub struct FadeCue {
    id: CueId,
    name: String,
    number: Option<String>,
    notes: String,
    color: CueColor,
    state: CueState,
    continue_mode: ContinueMode,
    pre_wait: Duration,
    post_wait: Duration,
    started_at: Option<Instant>,
    action_started_at: Option<Instant>,
    start_reported: bool,
    in_pre_wait: bool,
    auto_continue_fired: bool,
    elapsed_before_pause: Duration,
    action_elapsed_before_pause: Duration,

    /// UUIDs of cues to fade (empty = no-op).
    pub target_cue_ids: Vec<CueId>,
    /// Display labels kept in sync with target_cue_ids (for inspector).
    pub target_cue_numbers: Vec<String>,
    /// Temporary target set by a Target cue or OSC `tempCueTargetNumber/ID`.
    /// Never part of the saved document.
    temporary_target: Option<CueId>,
    /// Target audio volume in dB (−60 = silence, 0 = unity).
    pub target_volume_db: f64,
    pub patch_target_id: Option<Uuid>,
    pub patch_target_db: f64,
    pending_patch: Option<(std::sync::Arc<crate::engine::patch_fader::PatchFader>, f32)>,
    patch_run: Option<crate::engine::patch_fader::PatchFade>,
    /// Target visual brightness in percent (0 = black overlay, 100 = fully visible).
    /// Independent from `target_volume_db`.
    pub target_brightness_pct: f64,
    /// Leave the current picture level alone for an audio-only or geometry-only fade.
    pub fade_brightness: bool,
    /// Target stereo pan (-1 = left, 0 = center, +1 = right). `None` = leave pan
    /// untouched (a volume/brightness-only fade); `Some` = fade every audio
    /// target's pan from its current position to this value.
    pub target_pan: Option<f32>,
    /// Absolute playback-rate destination for decoded Audio targets.
    pub target_rate: Option<f64>,
    pub target_silent_speed: Option<f64>,
    pub target_cutoff: Option<CutoffDestination>,
    pub target_eq_gain: Option<EqGainDestination>,
    /// Optional absolute on-screen position for one Video, Image or Camera cue.
    pub target_translation: Option<VideoTranslation>,
    /// Intermediate X/Y coordinates, in playback order between live start and destination.
    pub translation_waypoints: Vec<[f64; 2]>,
    pub smooth_translation_path: bool,
    pub loop_translation_path: bool,
    /// Absolute uniform scale in Geometry units. None leaves scale alone.
    pub target_scale: Option<f64>,
    /// Signed position offsets and a scale factor against live geometry at action start.
    pub target_relative_geometry: Option<RelativeGeometry>,
    /// Absolute clockwise angle for an opted-in still PNG or JPEG.
    pub target_rotation_degrees: Option<f64>,
    /// Signed travel in degrees, starting from this playback's current angle.
    pub target_rotation_delta_degrees: Option<f64>,
    /// Absolute layer saturation between monochrome and twice the original saturation.
    pub target_saturation: Option<f64>,
    pub target_picture_brightness: Option<f64>,
    pub target_contrast: Option<f64>,
    pub target_hue_degrees: Option<f64>,
    pub target_map_motion: Option<super::point_mix::MapMotion>,
    /// When `true` (default) the fade drives the target volume toward
    /// `target_volume_db`. Set `false` for a **pan-only** fade that must leave the
    /// level untouched (mirrors QLab's pan-crosspoint fade, which doesn't move the
    /// master).
    pub fade_volume: bool,
    /// Fade duration in milliseconds.
    pub fade_duration_ms: u64,
    /// Rising and falling envelopes. Which one applies is decided per target:
    /// in one Fade Cue some voices may be coming up while others go down, and
    /// QLab shapes those differently on purpose.
    pub shapes: FadeShapes,
    /// Stop the target cue(s) after the fade completes.
    pub stop_at_end: bool,
    /// Crossfade: a visual cue this Fade starts, so the picture dissolves from
    /// the targets into it (`show/crossfade.rs`, `engine/output_engine/crossfade.rs`).
    pub crossfade_into: Option<CueId>,
    is_disabled: bool,

    // Runtime — injected by transport after go()
    /// (audio_voice_id, start_gain, start_pan) for each audio/video audio-track target.
    target_voices: Vec<(Uuid, f32, f32)>,
    unbound_rate: Option<f32>,
    rate_needs_tick: bool,
    rate_runs: Vec<RateFade>,
    effect_run: EffectRun,
    eq_gain_admission: bool,
    eq_gain_playhead_held: bool,
    silent_speed_run: SilentSpeedRun,
    geometry_run: VisualGeometryRun,
    loop_path_period: Option<Duration>,
    completed_picture_fade: Option<CompletedPictureFade>,
    map_motion_run: MapMotionState,
    /// (output_voice_id, start_opacity) for each visual target's layer.
    visual_targets: Vec<(Uuid, f32)>,
    /// The crossfade this run drives, once the show layer has set it up.
    crossfade: FadeCrossfade,
    /// A crossfade holds the whole Fade until the incoming picture is on
    /// screen: the action clock then starts on that very frame, so sound and
    /// picture move together.
    awaiting_picture: bool,
    /// Layer opacity at fade completion (0.0 = black, 1.0 = fully visible).
    visual_target_opacity: f32,
    fade_complete: bool,
    /// Target cue ids to hard-stop once the fade finishes (set on completion when
    /// `stop_at_end` is on).  The event loop drains this via
    /// [`take_fade_stop_targets`](crate::cue::traits::Cue::take_fade_stop_targets)
    /// and stops the actual cues — the fade itself can't reach the cue list.
    stop_targets_pending: Vec<CueId>,
}

impl FadeCue {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::from("Fade"),
            number: None,
            notes: String::new(),
            color: CueColor::Pink,
            state: CueState::Standby,
            continue_mode: ContinueMode::AutoFollow,
            pre_wait: Duration::ZERO,
            post_wait: Duration::ZERO,
            started_at: None,
            action_started_at: None,
            start_reported: false,
            in_pre_wait: false,
            auto_continue_fired: false,
            elapsed_before_pause: Duration::ZERO,
            action_elapsed_before_pause: Duration::ZERO,
            target_cue_ids: Vec::new(),
            target_cue_numbers: Vec::new(),
            temporary_target: None,
            target_volume_db: -60.0,
            patch_target_id: None,
            patch_target_db: 0.0,
            pending_patch: None,
            patch_run: None,
            target_brightness_pct: 0.0,
            fade_brightness: true,
            target_pan: None,
            target_rate: None,
            target_silent_speed: None,
            target_cutoff: None,
            target_eq_gain: None,
            target_translation: None,
            translation_waypoints: Vec::new(),
            smooth_translation_path: false,
            loop_translation_path: false,
            target_scale: None,
            target_relative_geometry: None,
            target_rotation_degrees: None,
            target_rotation_delta_degrees: None,
            target_saturation: None,
            target_picture_brightness: None,
            target_contrast: None,
            target_hue_degrees: None,
            target_map_motion: None,
            fade_volume: true,
            fade_duration_ms: 2000,
            shapes: FadeShapes::default(),
            stop_at_end: false,
            crossfade_into: None,
            is_disabled: false,
            target_voices: Vec::new(),
            unbound_rate: None,
            rate_needs_tick: false,
            rate_runs: Vec::new(),
            effect_run: EffectRun::Idle,
            eq_gain_admission: false,
            eq_gain_playhead_held: false,
            silent_speed_run: SilentSpeedRun::Idle,
            geometry_run: VisualGeometryRun::Idle,
            loop_path_period: None,
            completed_picture_fade: None,
            map_motion_run: MapMotionState::Idle,
            visual_targets: Vec::new(),
            crossfade: FadeCrossfade::None,
            awaiting_picture: false,
            visual_target_opacity: 0.0,
            fade_complete: false,
            stop_targets_pending: Vec::new(),
        }
    }

    fn engine_curve(c: FadeCurve) -> EngineFadeCurve {
        match c {
            FadeCurve::Linear => EngineFadeCurve::Linear,
            FadeCurve::SCurve => EngineFadeCurve::SCurve,
            FadeCurve::Exponential => EngineFadeCurve::Exponential,
        }
    }

    pub(crate) fn completed_picture_target(&self) -> Option<CueId> {
        self.completed_picture_fade.as_ref().map(|record| record.target)
    }

    pub(crate) fn revert_completed_picture(
        &mut self,
        live_voice: Option<CueId>,
        output: &dyn OutputEngineApi,
    ) -> Result<PictureFadeRevertResult> {
        anyhow::ensure!(!self.is_running() && !self.is_paused(), "Wait for the picture Fade to finish.");
        let record = self.completed_picture_fade.take()
            .ok_or_else(|| anyhow::anyhow!("Run a picture Fade to completion before reverting it."))?;
        anyhow::ensure!(
            self.target_cue_ids.as_slice() == [record.target],
            "The Fade's target has changed since it ran."
        );
        anyhow::ensure!(live_voice == Some(record.voice), "The original picture playback is no longer active and armed.");
        let geometry = record.geometry.is_some_and(|original| {
            output.set_voice_geometry_fade(record.voice, record.owner, original)
        });
        let written = if record.color.iter().any(Option::is_some) {
            output.set_voice_color_fade(record.voice, record.owner, record.color)
        } else {
            [false; 4]
        };
        anyhow::ensure!(geometry || written.contains(&true), "A later edit or Fade took control of the picture settings.");
        let saturation = written[PictureColorParameter::Saturation as usize];
        let other_color = PictureColorParameter::ALL.into_iter()
            .filter(|&parameter| parameter != PictureColorParameter::Saturation && written[parameter as usize])
            .collect();
        Ok(PictureFadeRevertResult { geometry, saturation, other_color })
    }

    /// The legacy single-curve name for this cue, so an older Inkue opening the
    /// file still gets approximately the right shape.
    fn legacy_curve(&self) -> FadeCurve {
        match self.shapes.up.kind {
            CurveKind::Linear => FadeCurve::Linear,
            CurveKind::Exponential => FadeCurve::Exponential,
            _ => FadeCurve::SCurve,
        }
    }

    fn validate_translation_waypoints(&self) -> Result<()> {
        if self.translation_waypoints.is_empty() {
            anyhow::ensure!(!self.loop_translation_path, "Add at least one waypoint to loop a picture path");
            return Ok(());
        }
        anyhow::ensure!(self.target_relative_geometry.is_none(), "Position waypoints need absolute position values");
        anyhow::ensure!(self.translation_waypoints.len() <= MAX_TRANSLATION_WAYPOINTS,
            "Add at most {MAX_TRANSLATION_WAYPOINTS} position waypoints");
        anyhow::ensure!(self.translation_waypoints.iter().flatten().all(|value| (-1.0..=1.0).contains(value)),
            "Position waypoint coordinates must be between -1 and +1");
        anyhow::ensure!(matches!(self.target_translation, Some(VideoTranslation { x: Some(_), y: Some(_) })),
            "Position waypoints need both destination axes");
        anyhow::ensure!(self.fade_duration_ms > 0,
            "Position waypoints need a Fade duration greater than zero");
        if self.loop_translation_path {
            anyhow::ensure!(self.target_cue_ids.len() == 1 && self.crossfade_into.is_none(),
                "A looping path needs one picture target and Crossfade set to None");
            anyhow::ensure!(self.continue_mode == ContinueMode::DoNotContinue && !self.stop_at_end,
                "Loop path needs Do Not Continue and Stop at end off");
            anyhow::ensure!(self.target_scale.is_none() && !self.has_rotation_goal()
                && !self.has_color_goal() && !self.fade_volume && !self.fade_brightness
                && self.target_pan.is_none() && self.target_rate.is_none() && self.effect_destination().is_none()
                && self.target_map_motion.is_none() && self.target_silent_speed.is_none() && self.patch_target_id.is_none(),
                "Use a dedicated position-only Fade for Loop path");
        }
        Ok(())
    }

    fn has_picture_goal(&self) -> bool {
        self.target_translation.is_some() || self.target_scale.is_some()
            || self.target_relative_geometry.is_some_and(RelativeGeometry::active)
            || self.has_rotation_goal() || self.has_color_goal()
    }

    fn color_destinations(&self) -> [Option<f64>; 4] {
        [self.target_saturation, self.target_picture_brightness, self.target_contrast, self.target_hue_degrees]
    }

    fn has_color_goal(&self) -> bool {
        self.color_destinations().iter().any(Option::is_some)
    }

    fn has_rotation_goal(&self) -> bool {
        self.target_rotation_degrees.is_some() || self.target_rotation_delta_degrees.is_some()
    }

    fn effect_destination(&self) -> Option<EffectDestination> {
        self.target_cutoff.map(EffectDestination::Cutoff)
            .or(self.target_eq_gain.map(EffectDestination::EqGain))
    }

    fn validate_effect_destination(&self) -> Result<()> {
        anyhow::ensure!(self.target_cutoff.is_none() || self.target_eq_gain.is_none(),
            "Choose one effect parameter per Fade cue");
        if let Some(destination) = self.target_cutoff {
            anyhow::ensure!((MIN_FREQUENCY..=MAX_FREQUENCY).contains(&destination.frequency_hz),
                "Filter cutoff must be between 20 Hz and 20 kHz");
        }
        if let Some(destination) = self.target_eq_gain {
            anyhow::ensure!(EQ_GAIN_RANGE.contains(&destination.gain_db),
                "Peaking EQ gain must be between −12 and +12 dB");
        }
        Ok(())
    }

    fn validate_picture_goals(&self) -> Result<()> {
        anyhow::ensure!(self.target_scale.is_none_or(|scale| SCALE_DESTINATION_RANGE.contains(&scale)),
            "Choose a scale between 5% and 800%");
        if let Some(relative) = self.target_relative_geometry {
            anyhow::ensure!(self.target_translation.is_none() && self.target_scale.is_none(),
                "Choose either absolute or relative position and scale values");
            anyhow::ensure!(relative.translation.is_none_or(VideoTranslation::is_valid),
                "Choose a relative position between -1 and +1 on at least one axis");
            anyhow::ensure!(relative.scale_factor.is_none_or(|factor| SCALE_DESTINATION_RANGE.contains(&factor)),
                "Choose a scale multiplier between 5% and 800%");
        }
        anyhow::ensure!(self.target_saturation.is_none_or(|value| (0.0..=2.0).contains(&value)),
            "Choose saturation between 0% and 200%");
        anyhow::ensure!(self.target_picture_brightness.is_none_or(|value| PictureColorParameter::Brightness.range().contains(&value)),
            "Choose picture brightness between −1 and +1");
        anyhow::ensure!(self.target_contrast.is_none_or(|value| PictureColorParameter::Contrast.range().contains(&value)),
            "Choose contrast between 0.25× and 4×");
        anyhow::ensure!(self.target_hue_degrees.is_none_or(|value| PictureColorParameter::Hue.range().contains(&value)),
            "Choose a hue angle between −180° and +180°");
        anyhow::ensure!(self.target_rotation_degrees.is_none_or(VideoGeometryUpdate::valid_rotation),
            "Choose a rotation angle from 0° up to, but excluding, 360°");
        anyhow::ensure!(self.target_rotation_delta_degrees.is_none_or(|delta| ROTATION_TURN_RANGE.contains(&delta)),
            "Choose a rotation turn between −3,600° and +3,600°");
        anyhow::ensure!(self.target_rotation_delta_degrees.is_none() || self.target_rotation_degrees.is_none(),
            "Choose a destination angle or a signed turn");
        anyhow::ensure!(!self.has_rotation_goal() || self.effect_destination().is_none(),
            "Use separate Fade cues for rotation and audio effects");
        Ok(())
    }

    fn target_brightness_alpha(&self) -> u8 {
        ((1.0 - self.target_brightness_pct.clamp(0.0, 100.0) / 100.0) * 255.0).round() as u8
    }

    /// The incoming picture of the crossfade the output engine is rendering.
    fn linked_incoming_voice(&self) -> Option<Uuid> {
        match &self.crossfade {
            FadeCrossfade::Linked(link) => Some(link.incoming_voice),
            _ => None,
        }
    }

    /// While a crossfade waits for its incoming picture: start the action
    /// clock on the very frame the dissolve started, or hand a link whose
    /// incoming content vanished back to the show layer (which links it again
    /// or falls back to a plain fade).  `true` once the action runs.
    fn picture_arrived(&mut self, context: &CueContext) -> bool {
        let FadeCrossfade::Linked(link) = &self.crossfade else {
            // Pending: the show layer is still setting it up.
            return false;
        };
        match context.output_engine.crossfade_phase(link.incoming_voice) {
            CrossfadePhase::Waiting => false,
            CrossfadePhase::Started { started_at } => {
                self.awaiting_picture = false;
                self.action_started_at = Some(started_at);
                self.report_action_start(context);
                true
            }
            CrossfadePhase::None => {
                self.crossfade = FadeCrossfade::Pending {
                    incoming_cue: link.incoming_cue,
                    incoming_started: true,
                };
                false
            }
        }
    }

    /// A dissolve whose incoming content left mid-way is over: the crossfaded
    /// targets are back on screen (the mix stopped), so this Fade no longer
    /// drives their sound nor stops them at its end.
    fn drop_abandoned_crossfade(&mut self, context: &CueContext) {
        let Some(incoming) = self.linked_incoming_voice() else { return };
        if context.output_engine.crossfade_phase(incoming) == CrossfadePhase::None {
            log::info!("[fade {}] crossfade abandoned — its incoming cue left", self.id);
            self.crossfade = FadeCrossfade::None;
        }
    }

    fn tick_effect(&mut self) {
        if matches!(self.effect_run, EffectRun::Pending { .. } | EffectRun::HeldEqGain | EffectRun::Finished) {
            return;
        }
        let elapsed = self.action_elapsed();
        let duration = Duration::from_millis(self.fade_duration_ms);
        if elapsed >= duration {
            if let EffectRun::Active(fade) = std::mem::replace(&mut self.effect_run, EffectRun::Finished) {
                fade.finish();
            }
            self.fade_complete = true;
        } else if let EffectRun::Active(fade) = &self.effect_run {
            let span = CUE_TICK_INTERVAL.min(duration.saturating_sub(elapsed));
            let end = (elapsed + span).as_secs_f64() / duration.as_secs_f64();
            fade.update(self.shapes.sample(end, true) as f32, self.shapes.sample(end, false) as f32, span);
        }
    }

    /// At the end of the fade: a crossfade always removes what it dissolved
    /// away; `stop_at_end` stops every other target too.  The targets are
    /// queued as **cues** for the event loop — stopping voices alone would
    /// leave them (and group children) in the Running state.
    fn queue_stops_at_end(&mut self, context: &CueContext, picture_landed: bool) {
        let mut stops: Vec<CueId> = match &self.crossfade {
            FadeCrossfade::Linked(link) => link.targets.clone(),
            _ => Vec::new(),
        };
        if self.stop_at_end && matches!(self.geometry_run, VisualGeometryRun::Idle) {
            // Immediate cut: the fade already reached the target level.
            for &(vid, _, _) in &self.target_voices {
                let _ = context.audio_engine.stop_voice(vid, 0, Self::engine_curve(self.legacy_curve()));
            }
            for &(vid, _) in &self.visual_targets {
                let _ = context.output_engine.stop_voice(vid, 0);
            }
            stops.extend(self.target_cue_ids.iter().copied());
        } else if self.stop_at_end && picture_landed {
            if let VisualGeometryRun::Active { target, voice, .. } = self.geometry_run {
                let _ = context.output_engine.stop_voice(voice, 0);
                stops.push(target);
            }
        }
        // Never the cue this Fade dissolves into, even if listed as a target.
        stops.retain(|&id| Some(id) != self.crossfade_into);
        stops.sort_unstable();
        stops.dedup();
        self.stop_targets_pending = stops;
    }
}

impl Default for FadeCue {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Cue trait
// ---------------------------------------------------------------------------

impl FadeCue {
    fn check_silent_speed(&self) -> Result<()> {
        anyhow::ensure!(self.target_silent_speed.is_some_and(|rate| (0.25..=4.0).contains(&rate)),
            "Choose a silent movie speed between 0.25× and 4×.");
        anyhow::ensure!(self.target_cue_ids.len() == 1,
            "A silent movie speed Fade needs one Video target.");
        anyhow::ensure!(self.continue_mode == ContinueMode::DoNotContinue && !self.stop_at_end
            && !self.fade_volume && !self.fade_brightness && self.target_pan.is_none()
            && self.target_rate.is_none() && !self.has_picture_goal() && self.effect_destination().is_none()
            && self.target_map_motion.is_none() && self.crossfade_into.is_none() && self.patch_target_id.is_none(),
            "Use a dedicated manual Fade for silent movie speed.");
        Ok(())
    }

    fn check_map_motion(&self) -> Result<()> {
        anyhow::ensure!(self.effect_destination().is_none(), "Use separate Fade cues for map motion and audio effects");
        anyhow::ensure!(self.target_cue_ids.len() == 1, "A map motion needs one Audio cue target");
        anyhow::ensure!(self.continue_mode == ContinueMode::DoNotContinue && !self.stop_at_end,
            "Use manual continuation and leave Stop at end off for map motion");
        anyhow::ensure!(self.crossfade_into.is_none() && !self.has_picture_goal()
            && self.patch_target_id.is_none() && self.target_rate.is_none() && self.target_pan.is_none()
            && !self.fade_volume, "Use a dedicated Fade for moving the Audio cue on its map");
        Ok(())
    }
}

impl FadeCue {
    fn report_action_start(&mut self, context: &CueContext) {
        if !self.start_reported {
            self.start_reported = context.mark_action_started(self.id);
        }
    }
}

impl Cue for FadeCue {
    inherit_playback_fields!(
        state, started_at, action_started_at, start_reported, in_pre_wait, auto_continue_fired,
        elapsed_before_pause, action_elapsed_before_pause, target_voices, visual_targets,
        crossfade, awaiting_picture, visual_target_opacity, fade_complete,
        stop_targets_pending, pending_patch, patch_run, unbound_rate, rate_needs_tick, rate_runs, geometry_run, loop_path_period,
        completed_picture_fade, map_motion_run,
        effect_run, eq_gain_admission, eq_gain_playhead_held, silent_speed_run,
    );

    fn id(&self) -> CueId { self.id }
    fn cue_type(&self) -> CueType { CueType::Fade }
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    fn number(&self) -> Option<&str> { self.number.as_deref() }
    fn set_number(&mut self, number: Option<String>) { self.number = number; }
    fn notes(&self) -> &str { &self.notes }
    fn set_notes(&mut self, notes: String) { self.notes = notes; }
    fn color(&self) -> CueColor { self.color }
    fn set_color(&mut self, color: CueColor) { self.color = color; }
    fn is_disabled(&self) -> bool { self.is_disabled }
    fn set_disabled(&mut self, d: bool) { self.is_disabled = d; }
    fn state(&self) -> CueState { self.state }

    fn load(&mut self, _context: &CueContext) -> Result<()> { Ok(()) }

    fn go(&mut self, context: &CueContext) -> Result<()> {
        self.validate_effect_destination()?;
        self.validate_translation_waypoints()?;
        self.validate_picture_goals()?;
        crate::health::clear(&format!("rotation-fade-{}", self.id));
        let silent_speed_run = if let Some(rate) = self.target_silent_speed {
            self.check_silent_speed()?;
            SilentSpeedRun::Pending { target: self.target_cue_ids[0], rate }
        } else {
            SilentSpeedRun::Idle
        };
        let map_motion_run = if let Some(motion) = self.target_map_motion {
            self.check_map_motion()?;
            MapMotionState::Pending(self.target_cue_ids[0], motion)
        } else {
            MapMotionState::Idle
        };
        let effect_run = if let Some(destination) = self.effect_destination() {
            let [target] = self.target_cue_ids.as_slice() else {
                anyhow::bail!("An effect Fade needs one Audio cue target");
            };
            EffectRun::Pending { target: *target, destination }
        } else {
            EffectRun::Idle
        };
        let geometry_run = if self.has_picture_goal() && self.effect_destination().is_none() {
            let [target] = self.target_cue_ids.as_slice() else {
                if self.has_rotation_goal() { anyhow::bail!(ROTATION_TARGET_REQUIRED); }
                anyhow::bail!("A position, scale, or color Fade needs one Video, Image, or Camera target");
            };
            anyhow::ensure!(self.crossfade_into.is_none(), "Use a separate Fade cue for the dissolve");
            anyhow::ensure!(self.target_translation.is_none_or(VideoTranslation::is_valid),
                "Choose a position between -1 and +1 on at least one axis");
            let destination = VideoGeometryUpdate {
                translation: self.target_translation,
                scale: self.target_scale,
                rotation_degrees: self.target_rotation_degrees,
            };
            VisualGeometryRun::Pending {
                target: *target, destination, relative: self.target_relative_geometry,
                rotation_delta_degrees: self.target_rotation_delta_degrees,
                waypoints: self.translation_waypoints.clone(),
                smooth_path: self.smooth_translation_path,
                color_destinations: self.color_destinations().map(|field| field.map(|value| value as f32)),
            }
        } else {
            VisualGeometryRun::Idle
        };
        self.patch_run = None;
        self.unbound_rate = self.target_rate.filter(|_| self.effect_destination().is_none()).map(|rate| rate as f32);
        self.rate_needs_tick = false;
        self.rate_runs.clear();
        self.pending_patch = if let Some(id) = self.patch_target_id.filter(|_| self.effect_destination().is_none()) {
            let patch = context.resolve_patch(Some(id)).ok_or_else(|| anyhow::anyhow!("Fade target output patch {id} is missing"))?;
            let target = db_to_linear(self.patch_target_db) as f32;
            Some((context.patch_fader(patch), target))
        } else { None };
        self.geometry_run = geometry_run;
        self.loop_path_period = self.loop_translation_path.then(|| Duration::from_millis(self.fade_duration_ms));
        self.completed_picture_fade = None;
        self.map_motion_run = map_motion_run;
        self.effect_run = effect_run;
        self.eq_gain_admission = self.target_eq_gain.is_some();
        self.eq_gain_playhead_held = false;
        self.silent_speed_run = silent_speed_run;
        if self.pre_wait.is_zero() {
            self.patch_run = self.pending_patch.take().and_then(|(fader, target)| fader.begin(target));
        }
        self.auto_continue_fired = false;
        self.elapsed_before_pause = Duration::ZERO;
        self.action_elapsed_before_pause = Duration::ZERO;
        self.target_voices = Vec::new();
        self.visual_targets = Vec::new();
        self.crossfade = FadeCrossfade::None;
        self.awaiting_picture = false;
        self.visual_target_opacity = 0.0;
        self.fade_complete = false;
        self.stop_targets_pending.clear();
        self.started_at = Some(Instant::now());
        self.start_reported = false;

        if self.pre_wait.is_zero() {
            self.in_pre_wait = false;
            self.action_started_at = (!self.eq_gain_admission).then(Instant::now);
        } else {
            self.in_pre_wait = true;
            self.action_started_at = None;
        }

        self.state = CueState::Running;
        if self.action_started_at.is_some() && self.crossfade_into.is_none() {
            self.report_action_start(context);
        }
        Ok(())
    }

    fn stop(&mut self, context: &CueContext) -> Result<()> {
        if let SilentSpeedRun::Active { voice, owner, .. } = self.silent_speed_run {
            context.output_engine.hold_silent_speed(voice, owner);
        }
        // Like any stopped fade, a dissolve in progress stays where it is; one
        // that has not started yet is called off.
        if let Some(incoming) = self.linked_incoming_voice() {
            context.output_engine.release_crossfade(incoming);
        }
        self.pending_patch = None;
        self.patch_run = None;
        self.unbound_rate = None;
        self.rate_needs_tick = false;
        self.rate_runs.clear();
        self.effect_run = EffectRun::Idle;
        if self.eq_gain_admission {
            crate::health::clear(&format!("eq-gain-fade-{}", self.id));
        }
        self.eq_gain_admission = false;
        self.eq_gain_playhead_held = false;
        self.silent_speed_run = SilentSpeedRun::Idle;
        self.geometry_run = VisualGeometryRun::Idle;
        if self.loop_path_period.take().is_some() {
            context.request_cue_refresh();
        }
        self.map_motion_run = MapMotionState::Idle;
        self.state = CueState::Standby;
        self.started_at = None;
        self.action_started_at = None;
        self.in_pre_wait = false;
        self.elapsed_before_pause = Duration::ZERO;
        self.action_elapsed_before_pause = Duration::ZERO;
        context.emit(CueEvent::Stopped { cue_id: self.id });
        Ok(())
    }

    fn pause(&mut self, context: &CueContext) -> Result<()> {
        if self.state == CueState::Running {
            if let SilentSpeedRun::Active { voice, owner, .. } = self.silent_speed_run {
                context.output_engine.hold_silent_speed(voice, owner);
            }
            if let Some(t) = self.started_at.take() {
                self.elapsed_before_pause += t.elapsed();
            }
            if !self.in_pre_wait {
                if let Some(t) = self.action_started_at.take() {
                    self.action_elapsed_before_pause += t.elapsed();
                }
            }
            for rate in &self.rate_runs {
                rate.hold();
            }
            if let EffectRun::Active(fade) = &self.effect_run {
                fade.hold();
            }
            if let Some(incoming) = self.linked_incoming_voice() {
                context.output_engine.hold_crossfade(incoming, true);
            }
            self.state = CueState::Paused;
        }
        Ok(())
    }

    fn resume(&mut self, context: &CueContext) -> Result<()> {
        if self.state == CueState::Paused {
            let now = Instant::now();
            self.started_at = Some(now);
            if !self.in_pre_wait
                && !self.awaiting_picture
                && !self.pending_start_admission()
                && !matches!(self.silent_speed_run, SilentSpeedRun::WaitingForOrigin { .. })
            {
                self.action_started_at = Some(now);
            }
            if let Some(incoming) = self.linked_incoming_voice() {
                context.output_engine.hold_crossfade(incoming, false);
            }
            self.state = CueState::Running;
        }
        Ok(())
    }

    fn hard_stop(&mut self, context: &CueContext) -> Result<()> {
        self.stop(context)
    }

    fn clear_temporary_targets(&mut self) {
        self.set_temporary_target(None);
        self.completed_picture_fade = None;
    }

    fn reset(&mut self) -> Result<()> {
        self.pending_patch = None;
        self.patch_run = None;
        self.unbound_rate = None;
        self.rate_needs_tick = false;
        self.rate_runs.clear();
        self.effect_run = EffectRun::Idle;
        if self.eq_gain_admission {
            crate::health::clear(&format!("eq-gain-fade-{}", self.id));
        }
        self.eq_gain_admission = false;
        self.eq_gain_playhead_held = false;
        self.silent_speed_run = SilentSpeedRun::Idle;
        self.geometry_run = VisualGeometryRun::Idle;
        self.loop_path_period = None;
        self.map_motion_run = MapMotionState::Idle;
        self.state = CueState::Standby;
        self.started_at = None;
        self.action_started_at = None;
        self.in_pre_wait = false;
        self.elapsed_before_pause = Duration::ZERO;
        self.action_elapsed_before_pause = Duration::ZERO;
        self.target_voices = Vec::new();
        self.visual_targets = Vec::new();
        self.crossfade = FadeCrossfade::None;
        self.awaiting_picture = false;
        self.visual_target_opacity = 0.0;
        self.fade_complete = false;
        self.stop_targets_pending.clear();
        Ok(())
    }

    fn tick(&mut self, context: &CueContext) -> Result<()> {
        if self.state != CueState::Running {
            return Ok(());
        }

        if self.in_pre_wait {
            if self.elapsed() < self.pre_wait {
                return Ok(());
            }
            self.in_pre_wait = false;
            // A crossfade's action starts with its picture, not here.
            if !self.eq_gain_admission {
                if !self.awaiting_picture {
                    self.action_started_at = Some(Instant::now());
                }
                context.emit(CueEvent::ActionStarted { cue_id: self.id });
            }
        }
        if self.action_started_at.is_some() && !self.awaiting_picture {
            self.report_action_start(context);
        }

        if !matches!(self.silent_speed_run, SilentSpeedRun::Idle) {
            if let SilentSpeedRun::WaitingForOrigin { voice, owner, rate } = self.silent_speed_run {
                self.silent_speed_run = match context.output_engine.silent_speed_origin(voice, owner) {
                    Some(SilentSpeedOrigin::Ready(start)) => {
                        self.action_started_at = Some(Instant::now());
                        SilentSpeedRun::Active { voice, owner, start, rate }
                    }
                    Some(SilentSpeedOrigin::Settling) => return Ok(()),
                    None => SilentSpeedRun::Done,
                };
            }
            if let SilentSpeedRun::Active { voice, owner, start, rate } = self.silent_speed_run {
                let duration = Duration::from_millis(self.fade_duration_ms);
                let elapsed = self.action_elapsed();
                let t = if elapsed >= duration { 1.0 } else { elapsed.as_secs_f64() / duration.as_secs_f64() };
                let value = sample_picture_value(start, rate, self.shapes.sample(t, true), self.shapes.sample(t, false), t);
                if !context.output_engine.set_silent_speed(voice, owner, value) || t >= 1.0 {
                    self.silent_speed_run = SilentSpeedRun::Done;
                }
            }
            return Ok(());
        }

        if !matches!(self.map_motion_run, MapMotionState::Idle) {
            if let MapMotionState::Running(run) = &mut self.map_motion_run {
                let duration = Duration::from_millis(self.fade_duration_ms);
                let elapsed = self.action_elapsed_before_pause + self.action_started_at.map_or(Duration::ZERO, |at| at.elapsed());
                let complete = elapsed >= duration;
                let progress = if complete { 1.0 } else { self.shapes.sample(elapsed.as_secs_f64() / duration.as_secs_f64(), true) };
                if !run.update(context, progress) || complete {
                    self.map_motion_run = MapMotionState::Done;
                }
            }
            return Ok(());
        }

        if self.awaiting_picture && !self.picture_arrived(context) {
            return Ok(());
        }
        if self.action_started_at.is_none() || self.unbound_rate.is_some()
            || matches!(self.geometry_run, VisualGeometryRun::Pending { .. })
        {
            return Ok(());
        }

        if let Some(period) = self.loop_path_period {
            let progress = path_loop_progress(self.action_elapsed(), period);
            // Both axes use path progress. A looping path owns no other Fade
            // parameters, so each pass bypasses ordinary completion effects.
            if !self.geometry_run.update(context, 0.0, 0.0, progress) {
                self.geometry_run = VisualGeometryRun::Declined;
                context.request_cue_refresh();
            }
            return Ok(());
        }

        if !matches!(self.effect_run, EffectRun::Idle) {
            self.tick_effect();
            return Ok(());
        }

        if let Some((fader, target)) = self.pending_patch.take() {
            self.patch_run = fader.begin(target);
        }

        self.rate_needs_tick = false;
        // No targets → nothing to drive; just wait for duration to expire.
        let crossfading = matches!(self.crossfade, FadeCrossfade::Linked(_));
        if self.target_voices.is_empty() && self.visual_targets.is_empty() && !crossfading
            && self.patch_run.is_none() && self.rate_runs.is_empty()
            && matches!(self.geometry_run, VisualGeometryRun::Idle)
        {
            return Ok(());
        }

        let elapsed = self.action_elapsed();
        let duration = Duration::from_millis(self.fade_duration_ms);
        let t = if duration.is_zero() { 1.0 } else { (elapsed.as_millis() as f64 / self.fade_duration_ms as f64).min(1.0) };
        if t < 1.0 {
            self.drop_abandoned_crossfade(context);
        }
        // Both directions sampled once; each target picks the one that matches
        // the way its own value is travelling.
        let rising_t = self.shapes.sample(t, true) as f32;
        let falling_t = self.shapes.sample(t, false) as f32;
        let picture_applied = self.geometry_run.update(context, rising_t.into(), falling_t.into(), t);
        if !picture_applied && matches!(self.geometry_run, VisualGeometryRun::Active { .. }) {
            self.geometry_run = VisualGeometryRun::Declined;
        }

        if let Some(run) = &self.patch_run {
            run.update(rising_t, falling_t);
        }

        if t >= 1.0 {
            for rate in self.rate_runs.drain(..) {
                rate.finish();
            }
        } else if !self.rate_runs.is_empty() {
            // Schedule a segment of the same Fade curve through the next cue
            // tick. The audio callback interpolates it while other Fade
            // parameters continue to use the existing control-rate clock.
            let span = CUE_TICK_INTERVAL.min(duration.saturating_sub(elapsed));
            let end = (elapsed + span).as_secs_f64() / duration.as_secs_f64();
            let rising = self.shapes.sample(end, true) as f32;
            let falling = self.shapes.sample(end, false) as f32;
            for rate in &self.rate_runs {
                rate.update(rising, falling, span);
            }
        }

        // Interpolate gain for each audio voice (skipped for a pan-only fade so
        // the level is left exactly where it was).
        if self.fade_volume {
            let target_gain = db_to_linear(self.target_volume_db) as f32;
            for &(vid, start_gain, _) in &self.target_voices {
                let progress = if target_gain >= start_gain { rising_t } else { falling_t };
                let gain = start_gain + (target_gain - start_gain) * progress;
                let _ = context.audio_engine.set_voice_gain(vid, gain);
            }
        }

        // Interpolate pan for each audio voice when a pan target is set.
        if let Some(target_pan) = self.target_pan {
            for &(vid, _, start_pan) in &self.target_voices {
                let progress = if target_pan >= start_pan { rising_t } else { falling_t };
                let pan = start_pan + (target_pan - start_pan) * progress;
                let _ = context.audio_engine.set_voice_pan(vid, pan);
            }
        }

        // Crossfade: the incoming sound rises and the outgoing sound falls to
        // silence on the same clock as the picture.
        if let FadeCrossfade::Linked(link) = &self.crossfade {
            for &(vid, level) in &link.incoming_audio {
                let _ = context.audio_engine.set_voice_gain(vid, level * rising_t);
            }
            for &(vid, start) in &link.outgoing_audio {
                let _ = context.audio_engine.set_voice_gain(vid, start * (1.0 - falling_t));
            }
        }

        // Interpolate each visual target's layer opacity (per-slot — a Fade
        // on one video no longer dips the whole output to black).
        if self.fade_brightness {
            for &(vid, start_opacity) in &self.visual_targets {
                let progress = if self.visual_target_opacity >= start_opacity { rising_t } else { falling_t };
                let opacity = start_opacity + (self.visual_target_opacity - start_opacity) * progress;
                context.output_engine.set_voice_opacity(vid, opacity);
            }
        }

        if t >= 1.0 && !self.fade_complete {
            self.fade_complete = true;
            if !self.stop_at_end {
                self.completed_picture_fade = self.geometry_run.completed_picture();
            }
            self.queue_stops_at_end(context, picture_applied);
        }

        Ok(())
    }

    fn is_action_started(&self) -> bool {
        !self.in_pre_wait && !self.pending_start_admission()
    }

    fn pending_start_admission(&self) -> bool {
        self.eq_gain_admission && matches!(self.effect_run, EffectRun::Pending { .. } | EffectRun::HeldEqGain)
    }

    fn requires_eq_gain_admission(&self) -> bool {
        self.target_eq_gain.is_some()
    }

    fn uses_action_clock_for_continue(&self) -> bool {
        self.eq_gain_admission
    }

    fn holds_playhead(&self) -> bool {
        self.pending_start_admission()
    }

    fn mark_playhead_held(&mut self) {
        self.eq_gain_playhead_held = self.pending_start_admission();
    }

    fn take_playhead_release(&mut self) -> bool {
        self.eq_gain_playhead_held && self.is_action_started() && self.state == CueState::Running
            && std::mem::take(&mut self.eq_gain_playhead_held)
    }

    fn is_time_complete(&self) -> bool {
        if self.loop_path_period.is_some() {
            return self.state == CueState::Running && matches!(self.geometry_run, VisualGeometryRun::Declined);
        }
        if !matches!(self.silent_speed_run, SilentSpeedRun::Idle) {
            return matches!(self.silent_speed_run, SilentSpeedRun::Done);
        }
        if !matches!(self.map_motion_run, MapMotionState::Idle) {
            return matches!(self.map_motion_run, MapMotionState::Done);
        }
        !self.in_pre_wait && !self.awaiting_picture && self.unbound_rate.is_none()
            && !self.rate_needs_tick && self.action_elapsed() >= Duration::from_millis(self.fade_duration_ms)
            && (matches!(self.geometry_run, VisualGeometryRun::Idle) || self.fade_complete)
            && (matches!(self.effect_run, EffectRun::Idle) || self.fade_complete)
    }

    fn pre_wait(&self) -> Duration { self.pre_wait }
    fn set_pre_wait(&mut self, d: Duration) { self.pre_wait = d; }
    fn post_wait(&self) -> Duration { self.post_wait }
    fn set_post_wait(&mut self, d: Duration) { self.post_wait = d; }

    fn duration(&self) -> Option<Duration> {
        let looping = if self.is_running() || self.is_paused() {
            self.loop_path_period.is_some()
        } else {
            self.loop_translation_path
        };
        (!looping).then(|| Duration::from_millis(self.fade_duration_ms))
    }

    fn set_duration(&mut self, d: Option<Duration>) {
        if let Some(d) = d {
            // The fade engine requires a positive duration; 0 means instant.
            self.fade_duration_ms = (d.as_millis() as u64).max(1);
        }
    }

    fn file_duration(&self) -> Option<Duration> {
        // The existing loop progress projection uses this as one pass.
        Some(self.loop_path_period.unwrap_or_else(|| Duration::from_millis(self.fade_duration_ms)))
    }

    fn elapsed(&self) -> Duration {
        match self.state {
            CueState::Running => {
                self.elapsed_before_pause
                    + self.started_at.map(|t| t.elapsed()).unwrap_or(Duration::ZERO)
            }
            CueState::Paused => self.elapsed_before_pause,
            _ => Duration::ZERO,
        }
    }

    fn action_elapsed(&self) -> Duration {
        if self.in_pre_wait {
            return Duration::ZERO;
        }
        match self.state {
            CueState::Running => {
                self.action_elapsed_before_pause
                    + self.action_started_at.map(|t| t.elapsed()).unwrap_or(Duration::ZERO)
            }
            CueState::Paused => self.action_elapsed_before_pause,
            _ => Duration::ZERO,
        }
    }

    fn continue_mode(&self) -> ContinueMode {
        if self.loop_path_period.is_none() && matches!(self.map_motion_run, MapMotionState::Idle)
            && matches!(self.silent_speed_run, SilentSpeedRun::Idle) {
            self.continue_mode
        } else {
            ContinueMode::DoNotContinue
        }
    }
    fn set_continue_mode(&mut self, mode: ContinueMode) { self.continue_mode = mode; }

    fn is_auto_continue_fired(&self) -> bool { self.auto_continue_fired }
    fn mark_auto_continue_fired(&mut self) { self.auto_continue_fired = true; }
    fn clear_auto_continue_fired(&mut self) { self.auto_continue_fired = false; }

    fn take_fade_stop_targets(&mut self) -> Vec<CueId> {
        std::mem::take(&mut self.stop_targets_pending)
    }

    fn set_temporary_target(&mut self, id: Option<CueId>) { self.temporary_target = id; }
    fn temporary_target(&self) -> Option<CueId> { self.temporary_target }

    fn target_cue_id(&self) -> Option<CueId> { self.target_cue_ids.first().copied() }
    fn set_target_cue(&mut self, id: CueId, label: Option<String>) {
        self.target_cue_ids = vec![id];
        self.target_cue_numbers = vec![label.unwrap_or_default()];
    }

    fn fade_specification(&self) -> Option<FadeAction> {
        Some(FadeAction {
            target_cue_ids: self.temporary_target.map(|id| vec![id]).unwrap_or_else(|| self.target_cue_ids.clone()),
            target_gain_linear: db_to_linear(self.target_volume_db) as f32,
            target_visual_alpha: Some(self.target_brightness_alpha()),
            target_rate: self.target_rate.filter(|_| self.effect_destination().is_none()),
            has_silent_speed_goal: self.target_silent_speed.is_some(),
            effect_destination: self.effect_destination(),
            has_picture_goal: self.has_picture_goal() && self.effect_destination().is_none(),
            target_map_motion: self.target_map_motion,
            duration_ms: self.fade_duration_ms,
            curve: self.legacy_curve(),
            stop_at_end: self.stop_at_end && self.effect_destination().is_none(),
            crossfade_into: self.crossfade_into.filter(|_| self.effect_destination().is_none()),
            up_curve: self.shapes.up.to_engine(),
            down_curve: self.shapes.for_direction(false).to_engine(),
        })
    }

    fn set_crossfade(&mut self, crossfade: FadeCrossfade) {
        let waits = !matches!(crossfade, FadeCrossfade::None);
        if waits && !self.awaiting_picture {
            // The action clock starts with the incoming picture, not at GO.
            self.awaiting_picture = true;
            self.action_started_at = None;
        } else if !waits && self.awaiting_picture {
            // No picture to wait for after all: a plain fade, from now.
            self.awaiting_picture = false;
            if !self.in_pre_wait && self.state == CueState::Running {
                self.action_started_at = Some(Instant::now());
            }
        }
        self.crossfade = crossfade;
    }

    fn pending_crossfade(&self) -> Option<(CueId, bool)> {
        match self.crossfade {
            FadeCrossfade::Pending { incoming_cue, incoming_started } => Some((incoming_cue, incoming_started)),
            _ => None,
        }
    }

    fn set_fade_voices(
        &mut self,
        voices: Vec<(CueId, f32, f32)>,
        visual_targets: Vec<(CueId, f32)>,
        visual_target_opacity: f32,
    ) {
        self.target_voices = voices;
        self.visual_targets = visual_targets;
        self.visual_target_opacity = visual_target_opacity.clamp(0.0, 1.0);
    }

    fn rate_fade_targets_pending(&self) -> bool {
        self.state == CueState::Running && !self.in_pre_wait && !self.awaiting_picture && self.unbound_rate.is_some()
    }

    fn silent_speed_target_pending(&self) -> Option<CueId> {
        if self.state != CueState::Running || self.in_pre_wait { return None; }
        match self.silent_speed_run {
            SilentSpeedRun::Pending { target, .. } => Some(target),
            _ => None,
        }
    }

    fn set_silent_speed_voice(&mut self, voice: Option<CueId>, context: &CueContext) {
        let SilentSpeedRun::Pending { rate, .. } = self.silent_speed_run else { return };
        let owner = Uuid::new_v4();
        let active = voice.and_then(|voice| {
            Some(match context.output_engine.claim_silent_speed(voice, owner)? {
                SilentSpeedOrigin::Ready(start) => SilentSpeedRun::Active { voice, owner, start, rate },
                SilentSpeedOrigin::Settling => {
                    self.action_started_at = None;
                    SilentSpeedRun::WaitingForOrigin { voice, owner, rate }
                }
            })
        });
        let alert = format!("silent-speed-{}", self.id);
        if active.is_some() {
            crate::health::clear(&alert);
        } else {
            crate::health::set(crate::health::HealthAlert::new(
                &alert, crate::health::HealthLevel::Warning,
                "Start the armed Video first. Silent speed needs an opted-in local MP4, MOV or linear MKV with no audio tracks.",
            ));
        }
        self.silent_speed_run = active.unwrap_or(SilentSpeedRun::Done);
    }

    fn set_rate_fade_targets(&mut self, faders: Vec<Arc<RateFader>>) {
        if let Some(target) = self.unbound_rate.take() {
            self.rate_runs = faders.iter().map(|fader| fader.begin(target)).collect();
            self.rate_needs_tick = true;
        }
    }

    fn effect_target_pending(&self) -> Option<(CueId, EffectDestination)> {
        if self.state == CueState::Running && !self.in_pre_wait {
            if let EffectRun::Pending { target, destination } = self.effect_run {
                return Some((target, destination));
            }
        }
        None
    }

    fn set_effect_fader(&mut self, fader: Option<Arc<ScalarFader>>, context: &CueContext) {
        let EffectRun::Pending { destination, .. } = self.effect_run else { return };
        if self.eq_gain_admission {
            let key = format!("eq-gain-fade-{}", self.id);
            match &fader {
                Some(_) => {
                    crate::health::clear(&key);
                    self.action_started_at = Some(Instant::now());
                    self.report_action_start(context);
                    if !self.pre_wait.is_zero() {
                        context.emit(CueEvent::ActionStarted { cue_id: self.id });
                    }
                }
                None => {
                    self.effect_run = EffectRun::HeldEqGain;
                    crate::health::set(crate::health::HealthAlert::new(
                        &key, crate::health::HealthLevel::Warning,
                        format!("EQ Fade '{}' is held. Start its Audio cue with the assigned peaking EQ enabled, then retry the Fade.", self.name),
                    ));
                    return;
                }
            }
        }
        self.effect_run = match fader {
            Some(fader) => EffectRun::Active(fader.begin(destination.value())),
            None => {
                log::warn!("Effect Fade '{}' needs its enabled effect in a running Audio cue", self.name);
                EffectRun::Unavailable
            }
        };
    }

    fn geometry_target_pending(&self) -> Option<(CueId, bool)> {
        if self.state != CueState::Running || self.in_pre_wait || self.awaiting_picture {
            return None;
        }
        match self.geometry_run {
            VisualGeometryRun::Pending { target, destination, rotation_delta_degrees, .. } => {
                Some((target, destination.rotation_degrees.is_some() || rotation_delta_degrees.is_some()))
            }
            _ => None,
        }
    }

    fn set_geometry_voice(&mut self, voice: Option<CueId>, context: &CueContext) {
        let VisualGeometryRun::Pending { target, destination, relative, rotation_delta_degrees,
            waypoints, smooth_path, color_destinations } = &mut self.geometry_run else { return };
        let (target, mut destination, relative, rotation_delta_degrees, waypoints, smooth_path) =
            (*target, *destination, *relative, *rotation_delta_degrees, std::mem::take(waypoints), *smooth_path);
        let owner = Uuid::new_v4();
        let rotation = destination.rotation_degrees.is_some() || rotation_delta_degrees.is_some();
        let start = voice.and_then(|voice| {
            if rotation {
                context.output_engine.begin_voice_rotation_fade(voice, owner)
            } else if destination.translation.is_some() || destination.scale.is_some()
                || relative.is_some_and(RelativeGeometry::active)
            {
                context.output_engine.begin_voice_geometry_fade(voice, owner)
            } else {
                None
            }
        });
        if rotation && start.is_none() {
            crate::health::set(crate::health::HealthAlert::new(
                format!("rotation-fade-{}", self.id), crate::health::HealthLevel::Warning,
                format!("{}: Start the armed PNG or JPEG Image target with continuous still rotation enabled before this Fade.", self.name),
            ));
            self.geometry_run = VisualGeometryRun::Declined;
            return;
        }
        let Some(voice) = voice else {
            self.geometry_run = VisualGeometryRun::Declined;
            return;
        };
        let fields = color_destinations.map(|value| value.is_some());
        let origins = if fields.contains(&true) {
            context.output_engine.begin_voice_color_fade(voice, owner, fields)
        } else {
            [None; 4]
        };
        let color: [Option<(f32, f32)>; 4] = std::array::from_fn(|index| origins[index].zip(color_destinations[index]));
        if start.is_none() && color.iter().all(Option::is_none) {
            self.geometry_run = VisualGeometryRun::Declined;
            return;
        }
        if let Some((relative, origin)) = relative.zip(start) {
            destination = VideoGeometryUpdate {
                rotation_degrees: destination.rotation_degrees,
                ..relative.resolve(origin)
            };
        }

        if let Some((angle, delta)) =
            start.and_then(|origin| origin.source_rotation_degrees).zip(rotation_delta_degrees)
        {
            // Keep full turns in the path but make a whole turn land exactly
            // on its starting angle, including when checking for Revert.
            destination.rotation_degrees = Some(wrap_rotation(wrap_rotation(angle) + delta % 360.0));
        }

        // Snapshot every enabled part from the same playback after pre-wait.
        if self.loop_path_period.is_none() {
            self.visual_targets = vec![(voice, context.output_engine.get_voice_opacity(voice))];
            self.visual_target_opacity = 1.0 - self.target_brightness_alpha() as f32 / 255.0;
            self.target_voices = context.output_engine.video_audio_voice(voice).map(|audio| {
                (audio, context.audio_engine.get_voice_gain(audio), context.audio_engine.get_voice_pan(audio))
            }).into_iter().collect();
        }
        self.geometry_run = VisualGeometryRun::Active {
            target, voice, owner, start, destination, rotation_delta_degrees, waypoints, smooth_path, color,
        };
    }

    fn map_motion_pending(&self) -> Option<(CueId, super::point_mix::MapMotion)> {
        if self.state != CueState::Running || self.in_pre_wait { return None; }
        match self.map_motion_run {
            MapMotionState::Pending(target, motion) => Some((target, motion)),
            _ => None,
        }
    }

    fn set_map_motion_run(&mut self, run: Option<super::point_mix::MapMotionRun>) {
        if matches!(self.map_motion_run, MapMotionState::Pending(..)) {
            self.map_motion_run = run.map_or(MapMotionState::Done, |run| MapMotionState::Running(Box::new(run)));
        }
    }

    fn resolve_fade_targets(&mut self, number_to_id: &std::collections::HashMap<String, CueId>) {
        if self.target_cue_ids.is_empty() {
            for num in &self.target_cue_numbers {
                if let Some(&id) = number_to_id.get(num) {
                    self.target_cue_ids.push(id);
                }
            }
        }
    }

    fn validate_programming(
        &self,
        ctx: &crate::cue::validation::ValidationContext,
    ) -> Vec<crate::cue::validation::CueIssue> {
        use crate::cue::validation::CueIssue;
        let mut issues: Vec<CueIssue> = self
            .target_cue_ids
            .iter()
            .filter(|id| !ctx.all_cue_ids.contains(id))
            .map(|_| CueIssue::warning("Fade target not found (cue deleted)"))
            .collect();
        if self.target_silent_speed.is_some() {
            if let Err(error) = self.check_silent_speed() {
                issues.push(CueIssue::error(error.to_string()));
            }
        }
        if self.target_map_motion.is_some() {
            if let Err(error) = self.check_map_motion() {
                issues.push(CueIssue::error(error.to_string()));
            }
        }
        if self.effect_destination().is_some() {
            if let Err(error) = self.validate_effect_destination() {
                issues.push(CueIssue::error(error.to_string()));
            }
            if self.target_cue_ids.len() != 1 {
                issues.push(CueIssue::error("An effect Fade needs one Audio cue target"));
            }
            return issues;
        }
        if self.patch_target_id.is_some_and(|id| !ctx.output_patch_ids.contains(&id)) {
            issues.push(CueIssue::error("Fade target output patch is missing"));
        }
        if self.target_cue_ids.is_empty() && self.target_cue_numbers.is_empty() && self.patch_target_id.is_none() {
            issues.push(CueIssue::warning("No target selected"));
        }
        if self.has_picture_goal() {
            if self.has_rotation_goal() {
                if !matches!(self.target_cue_ids.as_slice(), [target] if ctx.rotation_cue_ids.contains(target)) {
                    issues.push(CueIssue::error(ROTATION_TARGET_REQUIRED));
                }
            } else if !matches!(self.target_cue_ids.as_slice(), [target] if ctx.visual_cue_ids.contains(target)) {
                issues.push(CueIssue::error("A position, scale, or color Fade needs one Video, Image, or Camera target"));
            }
            if self.crossfade_into.is_some() {
                issues.push(CueIssue::error("Use a separate Fade cue for the dissolve"));
            }
        }
        if let Err(error) = self.validate_translation_waypoints() {
            issues.push(CueIssue::error(error.to_string()));
        }
        if let Err(error) = self.validate_picture_goals() {
            issues.push(CueIssue::error(error.to_string()));
        }
        if let Some(id) = self.crossfade_into {
            if !ctx.all_cue_ids.contains(&id) {
                issues.push(CueIssue::error("Crossfade cue not found (cue deleted)"));
            } else if !ctx.visual_cue_ids.contains(&id) {
                issues.push(CueIssue::error(
                    "Crossfade cue shows no picture — pick a Video, Image or Camera cue",
                ));
            }
            if self.target_cue_ids.contains(&id) {
                issues.push(CueIssue::warning(
                    "The crossfade cue is also a target — it is left out of the fade",
                ));
            }
        }
        issues
    }

    fn serialize(&self) -> Value {
        let mut saved = json!({
            "type": "fade",
            "cue_type": "fade",
            "id": self.id,
            "number": self.number,
            "name": self.name,
            "notes": self.notes,
            "color": self.color,
            "pre_wait_ms": self.pre_wait.as_millis() as u64,
            "post_wait_ms": self.post_wait.as_millis() as u64,
            "continue_mode": self.continue_mode,
            "target_cue_ids": self.target_cue_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
            "target_cue_numbers": self.target_cue_numbers,
            "target_volume_db": self.target_volume_db,
            "patch_target_id": self.patch_target_id,
            "patch_target_db": self.patch_target_db,
            "target_brightness_pct": self.target_brightness_pct,
            "fade_brightness": self.fade_brightness,
            "target_pan": self.target_pan,
            "target_rate": self.target_rate,
            "target_silent_speed": self.target_silent_speed,
            "target_cutoff": self.target_cutoff,
            "target_eq_gain": self.target_eq_gain,
            "target_translation": self.target_translation,
            "translation_waypoints": self.translation_waypoints,
            "smooth_translation_path": self.smooth_translation_path && !self.translation_waypoints.is_empty(),
            "loop_translation_path": self.loop_translation_path && !self.translation_waypoints.is_empty(),
            "target_scale": self.target_scale,
            "target_relative_geometry": self.target_relative_geometry,
            "target_rotation_degrees": self.target_rotation_degrees,
            "target_rotation_delta_degrees": self.target_rotation_delta_degrees,
            "target_saturation": self.target_saturation,
            "target_map_motion": self.target_map_motion,
            "fade_volume": self.fade_volume,
            "fade_duration_ms": self.fade_duration_ms,
            "fade_curve": self.legacy_curve(),
            "fade_shapes": self.shapes,
            "stop_at_end": self.stop_at_end,
            "crossfade_into_id": self.crossfade_into.map(|id| id.to_string()),
            "is_disabled": self.is_disabled,
        });
        for (key, destination) in [
            ("target_picture_brightness", self.target_picture_brightness),
            ("target_contrast", self.target_contrast),
            ("target_hue_degrees", self.target_hue_degrees),
        ] {
            saved[key] = json!(destination);
        }
        // Decoders predating EQ automation ignore its destination. Leave them
        // no other action to apply to the target when they read this Fade.
        if self.target_eq_gain.is_some() {
            for field in [
                "patch_target_id", "target_pan", "target_rate", "target_silent_speed",
                "target_translation", "target_scale", "target_rotation_degrees", "target_rotation_delta_degrees",
                "target_saturation", "target_picture_brightness", "target_contrast", "target_hue_degrees",
                "target_map_motion", "crossfade_into_id",
            ] {
                saved[field] = Value::Null;
            }
            for field in ["fade_volume", "fade_brightness", "stop_at_end"] {
                saved[field] = json!(false);
            }
            saved["translation_waypoints"] = json!([]);
            saved["smooth_translation_path"] = json!(false);
            saved["loop_translation_path"] = json!(false);
        }
        saved
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct FadeCueFactory;

impl CueFactory for FadeCueFactory {
    fn create(&self) -> Box<dyn Cue> {
        Box::new(FadeCue::new())
    }

    fn from_json(&self, value: Value) -> anyhow::Result<Box<dyn Cue>> {
        let mut cue = FadeCue::new();

        if let Some(id) = value.get("patch_target_id").filter(|v| !v.is_null()) {
            cue.patch_target_id = Some(serde_json::from_value(id.clone())?);
        }
        cue.patch_target_db = value["patch_target_db"].as_f64().unwrap_or(0.0).clamp(-60.0, 12.0);
        if let Some(s) = value.get("id").and_then(|v| v.as_str()) {
            cue.id = s.parse().unwrap_or_else(|_| Uuid::new_v4());
        }
        if let Some(s) = value.get("name").and_then(|v| v.as_str()) {
            cue.name = s.to_string();
        }
        if let Some(s) = value.get("number").and_then(|v| v.as_str()) {
            cue.number = Some(s.to_string());
        }
        if let Some(s) = value.get("notes").and_then(|v| v.as_str()) {
            cue.notes = s.to_string();
        }
        if let Some(ms) = value.get("pre_wait_ms").and_then(|v| v.as_u64()) {
            cue.pre_wait = Duration::from_millis(ms);
        }
        if let Some(ms) = value.get("post_wait_ms").and_then(|v| v.as_u64()) {
            cue.post_wait = Duration::from_millis(ms);
        }
        if let Some(cm) = value.get("continue_mode") {
            if let Ok(mode) = serde_json::from_value(cm.clone()) {
                cue.continue_mode = mode;
            }
        }
        if let Some(col) = value.get("color") {
            if let Ok(color) = serde_json::from_value(col.clone()) {
                cue.color = color;
            }
        }
        // New format: target_cue_ids array.
        if let Some(arr) = value.get("target_cue_ids").and_then(|v| v.as_array()) {
            cue.target_cue_ids = arr.iter()
                .filter_map(|v| v.as_str()?.parse().ok())
                .collect();
        }
        // target_cue_numbers array.
        if let Some(arr) = value.get("target_cue_numbers").and_then(|v| v.as_array()) {
            cue.target_cue_numbers = arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        } else if let Some(s) = value.get("target_cue_number").and_then(|v| v.as_str()) {
            // Backward compat: old single cue-number field.
            cue.target_cue_numbers = vec![s.to_string()];
        }
        if let Some(db) = value.get("target_volume_db").and_then(|v| v.as_f64()) {
            cue.target_volume_db = db;
        }
        if let Some(pct) = value.get("target_brightness_pct").and_then(|v| v.as_f64()) {
            cue.target_brightness_pct = pct;
        }
        cue.target_pan = value.get("target_pan").and_then(|v| v.as_f64()).map(|x| x as f32);
        cue.target_rate = value.get("target_rate").and_then(Value::as_f64).map(normalize_rate);
        if let Some(speed) = value.get("target_silent_speed") {
            cue.target_silent_speed = serde_json::from_value(speed.clone())?;
            anyhow::ensure!(cue.target_silent_speed.is_none_or(|rate| (0.25..=4.0).contains(&rate)),
                "Choose a silent movie speed between 0.25× and 4×.");
        }
        if let Some(motion) = value.get("target_map_motion").filter(|value| !value.is_null()) {
            let motion: super::point_mix::MapMotion = serde_json::from_value(motion.clone())?;
            anyhow::ensure!(motion.valid(), "Choose two different calibration marks");
            cue.target_map_motion = Some(motion);
        }
        if let Some(cutoff) = value.get("target_cutoff").filter(|value| !value.is_null()) {
            cue.target_cutoff = Some(serde_json::from_value(cutoff.clone())?);
        }
        if let Some(gain) = value.get("target_eq_gain").filter(|value| !value.is_null()) {
            cue.target_eq_gain = Some(serde_json::from_value(gain.clone())?);
        }
        cue.validate_effect_destination()?;
        if let Some(translation) = value.get("target_translation").filter(|value| !value.is_null()) {
            let translation: VideoTranslation = serde_json::from_value(translation.clone())?;
            anyhow::ensure!(translation.is_valid(), "Choose a position between -1 and +1 on at least one axis");
            cue.target_translation = Some(translation);
        }
        if let Some(points) = value.get("translation_waypoints").filter(|value| !value.is_null()) {
            let points = points.as_array().ok_or_else(|| anyhow::anyhow!("Expected a list of position waypoints"))?;
            anyhow::ensure!(points.len() <= MAX_TRANSLATION_WAYPOINTS,
                "Add at most {MAX_TRANSLATION_WAYPOINTS} position waypoints");
            cue.translation_waypoints = serde_json::from_value(Value::Array(points.clone()))?;
        }
        if let Some(smooth) = value.get("smooth_translation_path") {
            cue.smooth_translation_path = serde_json::from_value::<bool>(smooth.clone())?
                && !cue.translation_waypoints.is_empty();
        }
        if let Some(looping) = value.get("loop_translation_path") {
            cue.loop_translation_path = serde_json::from_value::<bool>(looping.clone())?
                && !cue.translation_waypoints.is_empty();
        }
        if let Some(scale) = value.get("target_scale") {
            cue.target_scale = serde_json::from_value(scale.clone())?;
        }
        if let Some(relative) = value.get("target_relative_geometry") {
            cue.target_relative_geometry = serde_json::from_value(relative.clone())?;
        }
        if let Some(rotation) = value.get("target_rotation_degrees") {
            cue.target_rotation_degrees = serde_json::from_value(rotation.clone())?;
        }
        if let Some(delta) = value.get("target_rotation_delta_degrees") {
            cue.target_rotation_delta_degrees = serde_json::from_value(delta.clone())?;
        }
        if let Some(saturation) = value.get("target_saturation") {
            cue.target_saturation = serde_json::from_value(saturation.clone())?;
        }
        for (key, target) in [
            ("target_picture_brightness", &mut cue.target_picture_brightness),
            ("target_contrast", &mut cue.target_contrast),
            ("target_hue_degrees", &mut cue.target_hue_degrees),
        ] {
            if let Some(value) = value.get(key) {
                *target = serde_json::from_value(value.clone())?;
            }
        }
        cue.validate_picture_goals()?;
        if let Some(b) = value.get("fade_brightness").and_then(Value::as_bool) {
            cue.fade_brightness = b;
        }
        if let Some(b) = value.get("fade_volume").and_then(|v| v.as_bool()) {
            cue.fade_volume = b;
        }
        if let Some(ms) = value.get("fade_duration_ms").and_then(|v| v.as_u64()) {
            cue.fade_duration_ms = ms;
        }
        // A file written before curve shapes existed carries only the name;
        // turn it into the equivalent locked pair so nothing changes for it.
        if let Some(c) = value.get("fade_curve") {
            if let Ok(curve) = serde_json::from_value::<FadeCurve>(c.clone()) {
                cue.shapes = FadeShapes::of_kind(match curve {
                    FadeCurve::Linear => CurveKind::Linear,
                    FadeCurve::SCurve => CurveKind::SCurve,
                    FadeCurve::Exponential => CurveKind::Exponential,
                });
            }
        }
        if let Some(s) = value.get("fade_shapes") {
            if let Ok(shapes) = serde_json::from_value(s.clone()) {
                cue.shapes = shapes;
            }
        }
        if let Some(b) = value.get("stop_at_end").and_then(|v| v.as_bool()) {
            cue.stop_at_end = b;
        }
        cue.crossfade_into = value
            .get("crossfade_into_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok());
        if let Some(b) = value.get("is_disabled").and_then(|v| v.as_bool()) {
            cue.is_disabled = b;
        }

        cue.validate_translation_waypoints()?;
        Ok(Box::new(cue))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::types::CrossfadeLinked;

    #[test]
    fn video_translation_round_trips_and_rejects_invalid_destinations() {
        let mut fade = FadeCue::new();
        assert!(fade.serialize()["target_translation"].is_null());
        fade.target_translation = Some(VideoTranslation { x: None, y: Some(-0.23) });
        let saved = fade.serialize();
        let restored = FadeCueFactory.from_json(saved.clone()).unwrap().serialize();
        assert_eq!(restored["target_translation"], saved["target_translation"]);
        for invalid in [json!({}), json!({"x": 1.1}), json!({"x": 0.0, "position": 0.0})] {
            let mut bad = saved.clone();
            bad["target_translation"] = invalid;
            assert!(FadeCueFactory.from_json(bad).is_err());
        }
    }

    #[test]
    fn scale_fade_round_trips_and_legacy_cues_leave_scale_alone() {
        let mut fade = FadeCue::new();
        fade.target_scale = Some(1.37);
        let saved = fade.serialize();
        let restored = FadeCueFactory.from_json(saved.clone()).unwrap().serialize();
        assert_eq!(restored["target_scale"], json!(1.37));

        for invalid in [json!(0.049), json!(8.01), json!("2.5")] {
            let mut bad = saved.clone();
            bad["target_scale"] = invalid;
            assert!(FadeCueFactory.from_json(bad).is_err());
        }
        let mut legacy = saved;
        legacy.as_object_mut().unwrap().remove("target_scale");
        assert!(FadeCueFactory.from_json(legacy).unwrap().serialize()["target_scale"].is_null());
    }

    #[test]
    fn relative_picture_round_trips_and_rejects_mixed_modes_and_waypoints() {
        let relative = json!({"translation":{"x":0.25,"y":null},"scale_factor":0.5});
        let data = json!({"target_relative_geometry":relative});
        let saved = FadeCueFactory.from_json(data.clone()).unwrap().serialize();
        assert_eq!(saved["target_relative_geometry"], relative);
        assert!(saved["target_translation"].is_null());
        assert!(saved["target_scale"].is_null());
        for (field, value) in [
            ("target_translation", json!({"x":0.25})),
            ("target_scale", json!(0.5)),
            ("translation_waypoints", json!([[0.0, 0.0]])),
            ("target_relative_geometry", json!({"translation":{"x":1.01}})),
            ("target_relative_geometry", json!({"scale_factor":0.049})),
        ] {
            let mut bad = data.clone();
            bad[field] = value;
            assert!(FadeCueFactory.from_json(bad).is_err(), "{field}");
        }
    }

    #[test]
    fn relative_picture_resolves_against_live_geometry_with_limits_and_no_unused_axis() {
        let start = VideoGeometry { pan_x: 0.875, pan_y: -0.5, scale: 4.0, ..Default::default() };
        let relative = RelativeGeometry {
            translation: Some(VideoTranslation { x: Some(0.25), y: None }),
            scale_factor: Some(4.0),
        };
        let end = relative.resolve(start);
        assert_eq!(end.translation, Some(VideoTranslation { x: Some(1.0), y: None }));
        assert_eq!(end.scale, Some(8.0));
        let midpoint = sample_visual_geometry(start, end, None, &[], false, (0.5, 0.5, 0.5));
        assert_eq!(midpoint.translation, Some(VideoTranslation { x: Some(0.9375), y: None }));
        assert_eq!(midpoint.scale, Some(6.0));
    }

    #[test]
    fn saturation_fade_round_trips_and_old_fades_leave_colour_unchanged() {
        let mut fade = FadeCue::new();
        fade.target_saturation = Some(0.0);
        let saved = fade.serialize();
        assert_eq!(FadeCueFactory.from_json(saved.clone()).unwrap().serialize()["target_saturation"], json!(0.0));
        for invalid in [json!(-0.01), json!(2.001), json!("0")] {
            let mut bad = saved.clone();
            bad["target_saturation"] = invalid;
            assert!(FadeCueFactory.from_json(bad).is_err());
        }
        let mut legacy = saved;
        legacy.as_object_mut().unwrap().remove("target_saturation");
        assert!(FadeCueFactory.from_json(legacy).unwrap().serialize()["target_saturation"].is_null());
    }

    #[test]
    fn picture_values_use_the_enabled_curve_and_exact_destination() {
        assert_eq!(sample_picture_value(1.0, 0.0, 0.25, 0.75, 0.5), 0.25);
        assert_eq!(sample_picture_value(1.0, 2.0, 0.25, 0.75, 0.5), 1.25);
        assert_eq!(sample_picture_value(1.0, 0.0, 0.0, 0.0, 1.0), 0.0);
    }

    #[test]
    fn live_scale_uses_the_directional_envelope_alongside_an_unchanged_position_path() {
        let start = VideoGeometry { pan_x: -0.5, pan_y: 0.0, scale: 0.5, ..VideoGeometry::default() };
        let destination = VideoGeometryUpdate {
            translation: Some(VideoTranslation { x: Some(0.5), y: Some(0.0) }),
            scale: Some(1.5),
            ..Default::default()
        };
        let waypoints = [[0.0, -0.25]];
        let midpoint = sample_visual_geometry(start, destination, None, &waypoints, false, (0.25, 0.75, 0.5));
        assert_eq!(midpoint.translation, Some(VideoTranslation { x: Some(0.0), y: Some(-0.25) }));
        assert_eq!(midpoint.scale, Some(0.75));
        assert_eq!(sample_visual_geometry(start, destination, None, &waypoints, false, (0.0, 0.0, 1.0)), destination);

        let shrink = VideoGeometryUpdate { scale: Some(0.25), ..Default::default() };
        assert_eq!(sample_visual_geometry(start, shrink, None, &[], false, (0.25, 0.75, 0.5)),
            VideoGeometryUpdate { scale: Some(0.3125), ..Default::default() });
        let position_only = VideoGeometryUpdate { scale: None, ..destination };
        assert_eq!(sample_visual_geometry(start, position_only, None, &waypoints, false, (0.25, 0.75, 0.5)).scale, None);
    }

    #[test]
    fn absolute_rotation_takes_the_short_turn_and_lands_on_the_fractional_destination() {
        assert_eq!(sample_absolute_rotation(350.0, 10.0, 0.5, 0.25, 0.5), 0.0);
        assert_eq!(sample_absolute_rotation(10.0, 350.0, 0.5, 0.25, 0.5), 5.0);
        assert_eq!(sample_absolute_rotation(0.0, 180.0, 0.5, 0.25, 0.5), 90.0);
        assert_eq!(sample_absolute_rotation(350.0, 10.5, 0.0, 0.0, 1.0), 10.5);
        let start = VideoGeometry { source_rotation_degrees: Some(350.5), ..Default::default() };
        let destination = VideoGeometryUpdate { rotation_degrees: Some(10.5), ..Default::default() };
        let midpoint = sample_visual_geometry(start, destination, None, &[], false, (0.5, 0.5, 0.5));
        assert_eq!(midpoint.rotation_degrees, Some(0.5));
        assert_eq!((midpoint.translation, midpoint.scale), (None, None));
    }

    #[test]
    fn signed_turns_keep_full_revolutions_and_use_the_directional_curve() {
        let origin = VideoGeometry { source_rotation_degrees: Some(350.5), ..Default::default() };
        let whole_turn = VideoGeometryUpdate { rotation_degrees: Some(350.5), ..Default::default() };
        let positive = sample_visual_geometry(origin, whole_turn, Some(720.0), &[], false,
            (0.125, 0.25, 0.125));
        let negative = sample_visual_geometry(origin, whole_turn, Some(-720.0), &[], false,
            (0.25, 0.125, 0.125));
        assert_eq!(positive.rotation_degrees, Some(80.5));
        assert_eq!(negative.rotation_degrees, Some(260.5));
        assert_eq!(sample_visual_geometry(origin, whole_turn, Some(720.0), &[], false,
            (0.25, 0.75, 1.0)), whole_turn);

        let extra_quarter = VideoGeometryUpdate { rotation_degrees: Some(80.5), ..Default::default() };
        assert_eq!(sample_visual_geometry(origin, extra_quarter, Some(450.0), &[], false,
            (0.8, 0.8, 0.8)).rotation_degrees, Some(350.5));
        assert_eq!(sample_visual_geometry(origin, extra_quarter, Some(450.0), &[], false,
            (0.9, 0.9, 1.0)), extra_quarter);
    }

    #[test]
    fn signed_turn_round_trips_and_cannot_share_an_absolute_angle() {
        let saved = json!({"target_rotation_delta_degrees": -720.5});
        let loaded = FadeCueFactory.from_json(saved.clone()).unwrap().serialize();
        assert_eq!(loaded["target_rotation_delta_degrees"], -720.5);
        assert!(loaded["target_rotation_degrees"].is_null());
        for value in [json!(-3600.1), json!(3600.1), json!("360")] {
            let mut bad = saved.clone();
            bad["target_rotation_delta_degrees"] = value;
            assert!(FadeCueFactory.from_json(bad).is_err());
        }
        let mut mixed = saved;
        mixed["target_rotation_degrees"] = json!(60.0);
        assert!(FadeCueFactory.from_json(mixed).is_err());
        let old = FadeCueFactory.from_json(json!({"target_rotation_degrees": 30.0})).unwrap();
        assert!(old.serialize()["target_rotation_delta_degrees"].is_null());
    }

    #[test]
    fn rotation_fade_round_trips_and_legacy_cues_leave_the_angle_alone() {
        let fade = FadeCue::new();
        let mut saved = fade.serialize();
        saved["target_rotation_degrees"] = json!(359.75);
        assert_eq!(FadeCueFactory.from_json(saved.clone()).unwrap().serialize()["target_rotation_degrees"], json!(359.75));
        for angle in [json!(-0.01), json!(360), json!("20.0")] {
            saved["target_rotation_degrees"] = angle;
            assert!(FadeCueFactory.from_json(saved.clone()).is_err());
        }
        saved.as_object_mut().unwrap().remove("target_rotation_degrees");
        assert!(FadeCueFactory.from_json(saved).unwrap().serialize()["target_rotation_degrees"].is_null());
    }

    #[test]
    fn waypoint_path_uses_ordered_straight_legs_with_equal_duration() {
        let start = [-0.5, 0.0];
        let waypoints = [[0.0, -0.5]];
        let end = [0.75, 0.25];
        for (progress, expected) in [
            (0.0, start),
            (0.25, [-0.25, -0.25]),
            (0.5, waypoints[0]),
            (0.75, [0.375, -0.125]),
            (1.0, end),
        ] {
            assert_eq!(sample_translation_path(start, &waypoints, end, progress, false), expected);
        }
        let two_waypoints = [[-0.75, -0.75], [0.75, 0.0]];
        assert_eq!(sample_translation_path(start, &two_waypoints, end, 1.0 / 3.0, false), two_waypoints[0]);
        assert_eq!(sample_translation_path(start, &two_waypoints, end, 2.0 / 3.0, false), two_waypoints[1]);
    }

    #[test]
    fn smooth_path_crosses_the_same_waypoint_with_matching_velocity() {
        let start = [-0.5, 0.0];
        let waypoint = [0.0, -0.5];
        let end = [0.75, 0.25];
        let sample = |progress| sample_translation_path(start, &[waypoint], end, progress, true);
        assert_eq!(sample(0.0), start);
        assert_eq!(sample(0.25), [-0.328125, -0.265625]);
        assert_eq!(sample(0.5), waypoint);
        assert_eq!(sample(1.0), end);

        let time_step = 0.000_001;
        let before = sample(0.5 - time_step);
        let after = sample(0.5 + time_step);
        for axis in 0..2 {
            let incoming = (waypoint[axis] - before[axis]) / time_step;
            let outgoing = (after[axis] - waypoint[axis]) / time_step;
            assert!((incoming - outgoing).abs() < 0.000_02);
        }
    }

    #[test]
    fn smooth_path_stays_bounded_and_holds_a_repeated_point() {
        let start = [-1.0, -1.0];
        let waypoints = [[1.0, -1.0], [1.0, -1.0], [-1.0, 1.0]];
        let end = [1.0, 1.0];
        for step in 0..=400 {
            let point = sample_translation_path(start, &waypoints, end, f64::from(step) / 400.0, true);
            assert!(point.iter().all(|axis| (-1.0..=1.0).contains(axis)));
            if (100..=200).contains(&step) {
                assert_eq!(point, waypoints[0]);
            }
        }
        for count in 1..=MAX_TRANSLATION_WAYPOINTS {
            let points: Vec<_> = (0..count).map(|index| [(index as f64 + 1.0) / 20.0, 0.5]).collect();
            for (index, point) in points.iter().enumerate() {
                let progress = (index + 1) as f64 / (count + 1) as f64;
                assert_eq!(&sample_translation_path(start, &points, end, progress, true), point);
            }
        }
    }

    #[test]
    fn loop_path_samples_the_destination_then_restarts_without_resetting_the_clock() {
        let period = Duration::from_secs(4);
        let sample = |elapsed| {
            let progress = path_loop_progress(elapsed, period);
            sample_translation_path([-0.5, 0.0], &[[0.0, -0.5]], [0.75, 0.25], progress, true)
        };
        assert_eq!(sample(Duration::ZERO), [-0.5, 0.0]);
        assert_eq!(sample(Duration::from_secs(2)), [0.0, -0.5]);
        assert_eq!(sample(period), [0.75, 0.25]);
        assert_eq!(sample(period * 17), [0.75, 0.25]);
        assert_eq!(sample(period + Duration::from_secs(2)), [0.0, -0.5]);
        let after_wrap = sample(period + Duration::from_millis(1));
        assert!((after_wrap[0] + 0.5).abs() < 0.000_001);
    }

    #[test]
    fn saved_loop_path_requires_a_dedicated_absolute_xy_waypoint_fade() {
        let mut fade = FadeCue::new();
        fade.target_cue_ids = vec![Uuid::new_v4()];
        fade.target_translation = Some(VideoTranslation { x: Some(0.37), y: Some(0.23) });
        fade.translation_waypoints = vec![[0.0, -0.5]];
        fade.smooth_translation_path = true;
        fade.loop_translation_path = true;
        fade.fade_volume = false;
        fade.fade_brightness = false;
        fade.stop_at_end = false;
        fade.set_continue_mode(ContinueMode::DoNotContinue);
        let saved = fade.serialize();
        let restored = FadeCueFactory.from_json(saved.clone()).unwrap();
        assert_eq!(restored.serialize()["loop_translation_path"], json!(true));
        assert_eq!(restored.serialize()["smooth_translation_path"], json!(true));
        assert_eq!(restored.duration(), None);
        assert_eq!(restored.file_duration(), Some(Duration::from_secs(2)));
        for (field, invalid) in [
            ("stop_at_end", json!(true)),
            ("fade_volume", json!(true)),
            ("fade_brightness", json!(true)),
            ("target_scale", json!(0.5)),
            ("target_saturation", json!(0.0)),
            ("continue_mode", json!("auto_follow")),
            ("crossfade_into_id", json!(Uuid::new_v4())),
            ("target_cue_ids", json!([])),
            ("target_translation", json!({"x": 0.37})),
        ] {
            let mut bad = saved.clone();
            bad[field] = invalid;
            assert!(FadeCueFactory.from_json(bad).is_err(), "unexpectedly accepted {field}");
        }
        let mut legacy = saved;
        legacy.as_object_mut().unwrap().remove("loop_translation_path");
        let restored = FadeCueFactory.from_json(legacy.clone()).unwrap();
        assert_eq!(restored.duration(), Some(Duration::from_secs(2)));
        assert_eq!(restored.serialize()["loop_translation_path"], json!(false));
        legacy["translation_waypoints"] = json!([]);
        legacy["loop_translation_path"] = json!(true);
        assert_eq!(FadeCueFactory.from_json(legacy).unwrap().serialize()["loop_translation_path"], json!(false));
    }

    #[test]
    fn saved_waypoints_require_two_axes_a_positive_duration_and_bounded_points() {
        let mut fade = FadeCue::new();
        fade.target_translation = Some(VideoTranslation { x: Some(0.37), y: Some(0.23) });
        fade.translation_waypoints = vec![[0.0, -0.5], [0.3, 0.5]];
        fade.smooth_translation_path = true;
        let valid = fade.serialize();
        let restored = FadeCueFactory.from_json(valid.clone()).unwrap().serialize();
        assert_eq!(restored["translation_waypoints"], valid["translation_waypoints"]);
        assert_eq!(restored["smooth_translation_path"], json!(true));
        for (field, invalid) in [
            ("target_translation", json!({"x": 0.37})),
            ("fade_duration_ms", json!(0)),
            ("translation_waypoints", json!([[0.0, 1.2]])),
            ("translation_waypoints", json!([[0.0]])),
            ("translation_waypoints", json!(vec![[0.0, 0.0]; MAX_TRANSLATION_WAYPOINTS + 1])),
        ] {
            let mut bad = valid.clone();
            bad[field] = invalid;
            assert!(FadeCueFactory.from_json(bad).is_err());
        }
        let mut legacy = valid;
        legacy.as_object_mut().unwrap().remove("smooth_translation_path");
        assert_eq!(FadeCueFactory.from_json(legacy.clone()).unwrap().serialize()["smooth_translation_path"], json!(false));
        legacy.as_object_mut().unwrap().remove("translation_waypoints");
        legacy["target_translation"]["y"] = Value::Null;
        legacy["fade_duration_ms"] = json!(0);
        legacy["smooth_translation_path"] = json!(true);
        assert_eq!(FadeCueFactory.from_json(legacy).unwrap().serialize()["smooth_translation_path"], json!(false));
    }

    #[test]
    fn a_map_motion_and_cutoff_cannot_share_one_fade() {
        let mut cue = FadeCue::new();
        cue.target_cue_ids = vec![Uuid::new_v4()];
        cue.fade_volume = false;
        cue.set_continue_mode(ContinueMode::DoNotContinue);
        assert!(cue.check_map_motion().is_ok());
        cue.target_cutoff = Some(CutoffDestination {
            filter_id: Uuid::new_v4(),
            frequency_hz: 120.0,
        });
        assert!(cue.check_map_motion().is_err());
    }

    #[test]
    fn eq_gain_destination_round_trips_and_requires_one_bounded_effect_parameter() {
        let filter_id = Uuid::new_v4();
        let mut cue = FadeCue::new();
        cue.target_cue_ids = vec![Uuid::new_v4()];
        let mut data = cue.serialize();
        data["target_eq_gain"] = json!({"filter_id": filter_id, "gain_db": -4.5});
        let loaded = FadeCueFactory.from_json(data.clone()).unwrap();
        assert_eq!(loaded.serialize()["target_eq_gain"], data["target_eq_gain"]);
        for invalid in [json!(-12.01), json!(12.01), json!("4.5")] {
            let mut bad = data.clone();
            bad["target_eq_gain"]["gain_db"] = invalid;
            assert!(FadeCueFactory.from_json(bad).is_err());
        }
        data["target_cutoff"] = json!({"filter_id": filter_id, "frequency_hz": 600.0});
        assert!(FadeCueFactory.from_json(data.clone()).is_err());
        data.as_object_mut().unwrap().remove("target_eq_gain");
        assert_eq!(FadeCueFactory.from_json(data.clone()).unwrap().serialize()["target_cutoff"],
            data["target_cutoff"]);
    }

    #[test]
    fn a_saved_eq_fade_has_no_legacy_actions_when_its_destination_is_ignored() {
        let mut cue = FadeCue::new();
        let target = Uuid::new_v4();
        cue.target_cue_ids = vec![target];
        cue.target_pan = Some(0.5);
        cue.target_rate = Some(1.5);
        cue.target_volume_db = -18.0;
        cue.fade_duration_ms = 4200;
        cue.stop_at_end = true;
        cue.patch_target_id = Some(Uuid::new_v4());
        cue.crossfade_into = Some(Uuid::new_v4());
        let ordinary = cue.serialize();
        let filter_id = Uuid::new_v4();
        cue.target_cutoff = Some(CutoffDestination { filter_id, frequency_hz: 400.0 });
        assert_eq!(cue.serialize()["fade_volume"], json!(true));
        assert_eq!(cue.serialize()["target_pan"], ordinary["target_pan"]);
        assert_eq!(cue.serialize()["stop_at_end"], json!(true));

        cue.target_cutoff = None;
        cue.target_eq_gain = Some(EqGainDestination { filter_id, gain_db: 6.0 });
        let saved = cue.serialize();
        assert_eq!(saved["target_cue_ids"], ordinary["target_cue_ids"]);
        assert_eq!(saved["fade_duration_ms"], ordinary["fade_duration_ms"]);
        assert_eq!(saved["target_volume_db"], ordinary["target_volume_db"]);
        assert_eq!(saved["target_eq_gain"]["gain_db"], json!(6.0));
        for field in ["fade_volume", "fade_brightness", "stop_at_end"] {
            assert_eq!(saved[field], json!(false), "{field}");
        }
        for field in ["patch_target_id", "target_pan", "target_rate", "crossfade_into_id"] {
            assert!(saved[field].is_null(), "{field}");
        }

        let mut old_reader = saved;
        old_reader.as_object_mut().unwrap().remove("target_eq_gain");
        let loaded = FadeCueFactory.from_json(old_reader).unwrap();
        let actions = loaded.fade_specification().unwrap();
        assert!(actions.effect_destination.is_none());
        assert!(actions.target_rate.is_none());
        assert!(actions.crossfade_into.is_none());
        assert!(!actions.stop_at_end);
        assert_eq!(loaded.serialize()["fade_volume"], json!(false));
        assert_eq!(loaded.serialize()["fade_brightness"], json!(false));
    }

    #[test]
    fn rate_destination_round_trips_and_old_fades_leave_rate_alone() {
        let factory = FadeCueFactory;
        let mut cue = FadeCue::new();
        cue.target_rate = Some(0.35);
        let mut data = cue.serialize();
        let loaded = factory.from_json(data.clone()).unwrap();
        assert_eq!(loaded.fade_specification().unwrap().target_rate, Some(0.35));
        data.as_object_mut().unwrap().remove("target_rate");
        let legacy = factory.from_json(data.clone()).unwrap();
        assert_eq!(legacy.fade_specification().unwrap().target_rate, None);
        data["target_rate"] = json!(40.0);
        let beyond_range = factory.from_json(data).unwrap();
        assert_eq!(beyond_range.fade_specification().unwrap().target_rate, Some(33.0));
    }

    #[test]
    fn cue_type_is_fade() {
        assert_eq!(FadeCue::new().cue_type(), CueType::Fade);
    }

    #[test]
    fn default_values() {
        let c = FadeCue::new();
        assert_eq!(c.fade_duration_ms, 2000);
        assert!((c.target_volume_db - (-60.0)).abs() < 1e-9);
        assert!(!c.stop_at_end);
        assert!(c.target_cue_ids.is_empty());
    }

    #[test]
    fn serialize_roundtrip() {
        let factory = FadeCueFactory;
        let mut cue = FadeCue::new();
        cue.set_name("My Fade".to_string());
        let target_id = Uuid::new_v4();
        cue.target_cue_ids = vec![target_id];
        cue.target_cue_numbers = vec!["3".to_string()];
        cue.target_volume_db = -6.0;
        cue.fade_duration_ms = 3000;
        cue.stop_at_end = true;

        let json = cue.serialize();
        assert_eq!(json["name"], "My Fade");
        assert_eq!(json["target_volume_db"], -6.0);
        assert_eq!(json["fade_duration_ms"], 3000u64);
        assert_eq!(json["stop_at_end"], true);

        let rebuilt = factory.from_json(json).unwrap();
        assert_eq!(rebuilt.name(), "My Fade");
    }

    #[test]
    fn default_fade_has_no_pan_target_and_fades_volume() {
        let c = FadeCue::new();
        assert!(c.target_pan.is_none());
        assert!(c.fade_volume);
    }

    #[test]
    fn pan_fade_serialize_roundtrip() {
        let factory = FadeCueFactory;
        let mut cue = FadeCue::new();
        cue.target_pan = Some(1.0);   // full right
        cue.fade_volume = false;      // pan-only fade
        let json = cue.serialize();
        assert_eq!(json["target_pan"], 1.0);
        assert_eq!(json["fade_volume"], false);

        // Re-parse and re-serialize: the pan target and pan-only flag survive.
        let rebuilt = factory.from_json(json).unwrap().serialize();
        assert_eq!(rebuilt["target_pan"], 1.0);
        assert_eq!(rebuilt["fade_volume"], false);
    }

    #[test]
    fn absent_pan_target_deserializes_to_none() {
        let factory = FadeCueFactory;
        let json = serde_json::json!({
            "type": "fade", "cue_type": "fade",
            "id": Uuid::new_v4().to_string(),
            "name": "Vol Fade", "notes": "", "color": "pink",
            "pre_wait_ms": 0u64, "post_wait_ms": 0u64, "continue_mode": "auto_follow",
            "target_cue_ids": [], "target_cue_numbers": [],
            "target_volume_db": -6.0, "fade_duration_ms": 1000u64,
            "fade_curve": "s_curve", "stop_at_end": false, "is_disabled": false,
        });
        // No target_pan / fade_volume keys → pan untouched, volume still fades.
        let restored = factory.from_json(json).unwrap().serialize();
        assert!(restored["target_pan"].is_null());
        assert_eq!(restored["fade_brightness"], true, "legacy fades keep their original visual behavior");
        let mut picture_untouched = restored;
        picture_untouched["fade_brightness"] = serde_json::json!(false);
        assert_eq!(factory.from_json(picture_untouched).unwrap().serialize()["fade_brightness"], false);
    }

    #[test]
    fn fade_specification_returns_action() {
        let mut cue = FadeCue::new();
        let id = Uuid::new_v4();
        cue.target_cue_ids = vec![id];
        cue.target_volume_db = 0.0;
        cue.fade_duration_ms = 1000;
        cue.stop_at_end = true;

        let spec = cue.fade_specification().unwrap();
        assert_eq!(spec.target_cue_ids, vec![id]);
        assert!((spec.target_gain_linear - 1.0).abs() < 1e-4);
        assert_eq!(spec.duration_ms, 1000);
        assert!(spec.stop_at_end);
    }

    #[test]
    fn backward_compat_single_target_number() {
        let factory = FadeCueFactory;
        // Simulate old-format JSON with target_cue_number (not _ids).
        let old_json = serde_json::json!({
            "type": "fade", "cue_type": "fade",
            "id": Uuid::new_v4().to_string(),
            "name": "Old Fade", "notes": "", "color": "blue",
            "pre_wait_ms": 0u64, "post_wait_ms": 0u64,
            "continue_mode": "auto_follow",
            "target_cue_number": "5",
            "target_volume_db": -60.0, "fade_duration_ms": 2000u64,
            "fade_curve": "s_curve", "stop_at_end": false, "is_disabled": false,
        });
        let cue = factory.from_json(old_json).unwrap();
        let spec = cue.fade_specification().unwrap();
        // UUIDs not resolved yet (no workspace loaded), but number is stored.
        assert!(spec.target_cue_ids.is_empty());
    }

    #[test]
    fn a_show_written_before_curve_shapes_still_loads_its_curve() {
        // Only "fade_curve" — the pre-shapes format. It must come back as the
        // same shape, locked, so the fade behaves exactly as it always did.
        let json = serde_json::json!({
            "type": "fade", "id": Uuid::new_v4().to_string(), "name": "Old",
            "fade_duration_ms": 2000, "fade_curve": "exponential",
        });
        let rebuilt = FadeCueFactory.from_json(json).unwrap();
        let out = rebuilt.serialize();
        assert_eq!(out["fade_shapes"]["up"]["kind"], "exponential");
        assert_eq!(out["fade_shapes"]["mirrored"], true);
        assert_eq!(out["fade_curve"], "exponential", "legacy name still written");
    }

    #[test]
    fn curve_shapes_survive_a_roundtrip_and_win_over_the_legacy_name() {
        use crate::cue::curve::{CurvePoint, CurveShape};
        let mut cue = FadeCue::new();
        cue.shapes.mirrored = false;
        cue.shapes.up.kind = CurveKind::Linear;
        cue.shapes.up.points = vec![CurvePoint::new(0.3, 0.8)];
        cue.shapes.down = CurveShape::of_kind(CurveKind::Parametric);

        let rebuilt = FadeCueFactory.from_json(cue.serialize()).unwrap();
        let out = rebuilt.serialize();
        assert_eq!(out["fade_shapes"]["up"]["kind"], "linear");
        assert_eq!(out["fade_shapes"]["up"]["points"][0]["t"], 0.3);
        assert_eq!(out["fade_shapes"]["down"]["kind"], "parametric");
        assert_eq!(out["fade_shapes"]["mirrored"], false);
    }

    #[test]
    fn the_legacy_name_written_out_reflects_the_shape_that_is_set() {
        let mut cue = FadeCue::new();
        cue.shapes = FadeShapes::of_kind(CurveKind::Linear);
        assert_eq!(cue.serialize()["fade_curve"], "linear");
        // A parametric shape has no legacy equivalent — S-Curve is the honest
        // approximation for an older build reading the file.
        cue.shapes = FadeShapes::of_kind(CurveKind::Parametric);
        assert_eq!(cue.serialize()["fade_curve"], "s_curve");
    }

    #[test]
    fn crossfade_target_survives_a_serialize_roundtrip() {
        let mut cue = FadeCue::new();
        let into = Uuid::new_v4();
        cue.crossfade_into = Some(into);
        let back = FadeCueFactory.from_json(cue.serialize()).unwrap();
        assert_eq!(back.serialize()["crossfade_into_id"], into.to_string());
    }

    #[test]
    fn a_plain_fade_has_no_crossfade() {
        let cue = FadeCue::new();
        assert!(cue.serialize()["crossfade_into_id"].is_null());
        assert!(cue.fade_specification().unwrap().crossfade_into.is_none());
    }

    #[test]
    fn crossfade_into_is_exposed_to_the_transport() {
        let mut cue = FadeCue::new();
        let into = Uuid::new_v4();
        cue.crossfade_into = Some(into);
        assert_eq!(cue.fade_specification().unwrap().crossfade_into, Some(into));
    }

    #[test]
    fn the_crossfade_curves_follow_the_fade_shapes() {
        let mut cue = FadeCue::new();
        cue.shapes = FadeShapes {
            up: crate::cue::curve::CurveShape::of_kind(CurveKind::Exponential),
            down: crate::cue::curve::CurveShape::of_kind(CurveKind::Linear),
            mirrored: false,
        };
        let spec = cue.fade_specification().unwrap();
        assert_eq!(spec.up_curve, EngineFadeCurve::Exponential);
        assert_eq!(spec.down_curve, EngineFadeCurve::Linear);
        cue.shapes.mirrored = true;
        assert_eq!(
            cue.fade_specification().unwrap().down_curve,
            EngineFadeCurve::Exponential,
            "locked shapes: one curve drives both directions",
        );
    }

    fn validation_context(all: &[Uuid], visual: &[Uuid]) -> crate::cue::validation::ValidationContext {
        crate::cue::validation::ValidationContext {
            all_cue_ids: all.iter().copied().collect(),
            fixture_ids: Default::default(),
            fixture_group_ids: Default::default(),
            osc_patch_ids: Default::default(),
            output_patch_ids: Default::default(),
            midi_ports: Vec::new(),
            video_output_ids: Default::default(),
            visual_cue_ids: visual.iter().copied().collect(),
            rotation_cue_ids: Default::default(),
        }
    }

    fn crossfade_issues(cue: &FadeCue, ctx: &crate::cue::validation::ValidationContext) -> Vec<String> {
        cue.validate(ctx).into_iter().map(|i| i.message).filter(|m| m.contains("rossfade")).collect()
    }

    #[test]
    fn a_crossfade_into_a_picture_is_valid() {
        let (target, into) = (Uuid::new_v4(), Uuid::new_v4());
        let mut cue = FadeCue::new();
        cue.target_cue_ids = vec![target];
        cue.crossfade_into = Some(into);
        assert!(crossfade_issues(&cue, &validation_context(&[target, into], &[target, into])).is_empty());
    }

    #[test]
    fn a_crossfade_into_a_cue_without_picture_is_an_error() {
        let into = Uuid::new_v4();
        let mut cue = FadeCue::new();
        cue.crossfade_into = Some(into);
        let issues = cue.validate(&validation_context(&[into], &[]));
        assert!(issues.iter().any(|i| i.severity == crate::cue::validation::Severity::Error
            && i.message.contains("no picture")));
    }

    #[test]
    fn a_crossfade_into_a_deleted_cue_is_an_error() {
        let mut cue = FadeCue::new();
        cue.crossfade_into = Some(Uuid::new_v4());
        let issues = cue.validate(&validation_context(&[], &[]));
        assert!(issues.iter().any(|i| i.severity == crate::cue::validation::Severity::Error
            && i.message.contains("not found")));
    }

    #[test]
    fn crossfading_into_one_of_the_targets_is_flagged() {
        let into = Uuid::new_v4();
        let mut cue = FadeCue::new();
        cue.target_cue_ids = vec![into];
        cue.crossfade_into = Some(into);
        let issues = crossfade_issues(&cue, &validation_context(&[into], &[into]));
        assert_eq!(issues.len(), 1, "{issues:?}");
    }

    #[test]
    fn a_running_rate_fade_keeps_its_bound_run_after_inspector_rebuild() {
        let mut cue = FadeCue::new();
        cue.target_rate = Some(0.5);
        cue.state = CueState::Running;
        cue.unbound_rate = Some(0.5);
        cue.set_rate_fade_targets(vec![Arc::new(RateFader::default())]);
        let mut rebuilt = FadeCue::new();
        rebuilt.target_rate = Some(2.0);
        rebuilt.inherit_playback(&mut cue);
        assert_eq!(rebuilt.state(), CueState::Running);
        assert_eq!(rebuilt.rate_runs.len(), 1);
        assert!(rebuilt.rate_needs_tick);
        assert!(cue.rate_runs.is_empty());
        assert_eq!(rebuilt.target_rate, Some(2.0), "the edited destination is saved for the next run");
    }

    #[test]
    fn a_running_crossfade_survives_the_inspector_rebuild() {
        // Every inspector edit rebuilds the cue from JSON: a Fade renamed
        // mid-crossfade must still remove what it dissolved away at its end.
        let target = Uuid::new_v4();
        let mut cue = FadeCue::new();
        cue.target_cue_ids = vec![target];
        cue.state = CueState::Running;
        cue.set_crossfade(FadeCrossfade::Linked(CrossfadeLinked {
            incoming_cue: Uuid::new_v4(),
            incoming_voice: Uuid::new_v4(),
            incoming_audio: Vec::new(),
            outgoing_audio: Vec::new(),
            targets: vec![target],
        }));
        let incoming_voice = cue.linked_incoming_voice();

        let mut rebuilt = FadeCue::new();
        rebuilt.inherit_playback(&mut cue);

        assert_eq!(rebuilt.linked_incoming_voice(), incoming_voice);
        assert!(rebuilt.awaiting_picture, "still waiting for its picture");
        assert!(!rebuilt.in_pre_wait, "waiting for a picture is not a pre-wait");
    }

    #[test]
    fn temporary_target_overrides_fade_targets_until_cleared() {
        use crate::cue::traits::Cue;
        let mut fade = FadeCue::new();
        let saved = Uuid::new_v4();
        fade.target_cue_ids = vec![saved];
        assert_eq!(fade.fade_specification().unwrap().target_cue_ids, vec![saved]);
        let temp = Uuid::new_v4();
        fade.set_temporary_target(Some(temp));
        assert_eq!(fade.temporary_target(), Some(temp));
        assert_eq!(fade.fade_specification().unwrap().target_cue_ids, vec![temp]);
        fade.clear_temporary_targets();
        assert_eq!(fade.temporary_target(), None);
        assert_eq!(fade.fade_specification().unwrap().target_cue_ids, vec![saved]);
    }

    #[test]
    fn set_duration_overrides_the_fade_time_and_clamps_to_positive() {
        use crate::cue::traits::Cue;
        let mut fade = FadeCue::new();
        assert_eq!(fade.duration(), Some(Duration::from_millis(2000)));
        fade.set_duration(Some(Duration::from_millis(750)));
        assert_eq!(fade.duration(), Some(Duration::from_millis(750)));
        fade.set_duration(Some(Duration::ZERO));
        assert_eq!(fade.duration(), Some(Duration::from_millis(1)));
    }
}
