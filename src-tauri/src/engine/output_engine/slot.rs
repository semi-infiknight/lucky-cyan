//! Video slot pool — one mpv context per simultaneously-visible visual cue,
//! one pool per output.
//!
//! QLab-style layering: every Video / Image / Camera cue gets its own
//! [`VideoSlot`] (mpv context + event thread + FBO on the render thread), and
//! the compositor in `render.rs` stacks the slot textures in layer order with
//! per-slot opacity and blend mode.  Slots are created lazily up to
//! [`MAX_VIDEO_SLOTS`] **per output** and live as long as their output — an
//! idle slot costs one idle mpv context.  When the pool is exhausted the oldest
//! content is stolen (hard stop, `Withdrawn` status so the owning cue resets).
//!

use std::ffi::{c_char, c_void, CString};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use uuid::Uuid;

use crate::engine::mpv_sys::{
    MpvEventEndFile, MpvEventLogMessage, MpvEventProperty, MpvLib,
    MPV_END_FILE_REASON_EOF, MPV_END_FILE_REASON_ERROR,
    MPV_EVENT_END_FILE, MPV_EVENT_FILE_LOADED, MPV_EVENT_LOG_MESSAGE, MPV_EVENT_NONE,
    MPV_EVENT_PLAYBACK_RESTART, MPV_EVENT_PROPERTY_CHANGE, MPV_EVENT_SHUTDOWN,
    MPV_EVENT_VIDEO_RECONFIG, MPV_FORMAT_DOUBLE, MPV_FORMAT_FLAG,
};
use crate::engine::AudioEngine;

use super::blend::BlendMode;
use super::crossfade::{CrossfadeLink, Departure, Outgoing};
use super::output::{all_slots, slot_for_voice, wake_signal, Output};
use super::types::{
    CrossfadePhase, CrossfadeRequest, LayerStyle, MpvCtx, OutputId, OutputStatus, PictureColorParameter, SilentSpeedOrigin,
    VideoGeometry, VoiceId,
};
use super::{cs, opt_str, try_apply_crop, OUTPUT_STATUS_TX};

/// Maximum simultaneously-open video slots (excluding the overlay context).
/// Each active slot is a full decode pipeline; 8 covers any realistic stage.
pub(super) const MAX_VIDEO_SLOTS: usize = 8;

/// Stacking sequence for automatic layering (newest on top) and steal order.
static LAYER_SEQ: AtomicU64 = AtomicU64::new(1);

/// Set once a slot has seen libmpv fail to initialise hardware decoding.
/// From then on every slot decodes in software — see [`fall_back_to_software`].
static SOFTWARE_DECODE_ONLY: AtomicBool = AtomicBool::new(false);

/// Operator override for the `hwdec` mode, from the `INKUE_HWDEC` environment
/// variable (any value libmpv accepts: `no`, `auto-copy`, `d3d11va-copy`, …).
///
/// An explicit choice is **pinned**: it also disables the automatic fallback
/// below, so setting a backend on purpose — to reproduce a decoder bug, or to
/// work around one — is not silently undone. Read once, on first use.
static HWDEC_OVERRIDE: OnceLock<Option<String>> = OnceLock::new();

fn hwdec_override() -> Option<&'static str> {
    HWDEC_OVERRIDE
        .get_or_init(|| {
            let mode = std::env::var("INKUE_HWDEC")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            if let Some(m) = &mode {
                log::info!("[slot] INKUE_HWDEC={m} — hwdec pinned, automatic fallback disabled");
            }
            mode
        })
        .as_deref()
}

/// The `hwdec` mode new slots are created with.
///
/// `auto-copy` reads decoded frames back into system memory, which is the only
/// hwdec family that composites correctly through our own GL render context.
/// Once [`SOFTWARE_DECODE_ONLY`] is latched, slots stop asking for hardware
/// decoding altogether.
fn hwdec_mode() -> &'static str {
    resolve_hwdec_mode(hwdec_override(), SOFTWARE_DECODE_ONLY.load(Ordering::Relaxed))
}

/// Precedence, as an executable spec: an operator's pin wins over everything,
/// then a latched failure, then the default.
fn resolve_hwdec_mode(pinned: Option<&'static str>, software_only: bool) -> &'static str {
    match (pinned, software_only) {
        (Some(mode), _) => mode,
        (None, true) => "no",
        (None, false) => "auto-copy",
    }
}

/// `true` when an mpv log line reports that hardware decoding could not be set
/// up — the GPU/driver refuses the codec profile, or the hwaccel device cannot
/// be created at all.  mpv words it the same way for every backend (`d3d11` on
/// Windows, `vulkan`/`cuda`/`vaapi` elsewhere), so one pattern covers them all;
/// the reason string is matched too in case the wording of the first half ever
/// changes.
///
/// mpv retries the failure per frame and can hand the renderer partially
/// decoded frames, which is what shows up as a green/torn picture; the cure is
/// to stop asking for hardware decoding (issue #5).
pub(super) fn reports_hwdec_failure(text: &str) -> bool {
    text.contains("Failed setup for format")
        || text.contains("hwaccel initialisation returned error")
}

// ---------------------------------------------------------------------------
// Opacity animation
// ---------------------------------------------------------------------------

/// Per-slot opacity animation (fade in/out, EOF fade, Fade Cue).
#[derive(Debug, Clone)]
pub(super) struct OpacityAnim {
    pub current: f32,
    pub start: f32,
    pub target: f32,
    pub duration_ms: u32,
    pub started_at: Instant,
}

impl OpacityAnim {
    fn resting(v: f32) -> Self {
        Self { current: v, start: v, target: v, duration_ms: 0, started_at: Instant::now() }
    }

    fn animate_to(&mut self, target: f32, duration_ms: u32) {
        self.start = self.current;
        self.target = target.clamp(0.0, 1.0);
        self.duration_ms = duration_ms;
        self.started_at = Instant::now();
    }

    fn set(&mut self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        self.current = v;
        self.start = v;
        self.target = v;
        self.duration_ms = 0;
    }

    /// Advance one frame.  Returns `(current, just_completed)`.
    fn tick(&mut self) -> (f32, bool) {
        if (self.current - self.target).abs() < f32::EPSILON {
            return (self.current, false);
        }
        let t = if self.duration_ms == 0 {
            1.0
        } else {
            (self.started_at.elapsed().as_millis() as f32 / self.duration_ms as f32).min(1.0)
        };
        self.current = self.start + (self.target - self.start) * t;
        let done = t >= 1.0;
        if done {
            self.current = self.target;
        }
        (self.current, done)
    }

    pub(super) fn is_animating(&self) -> bool {
        (self.current - self.target).abs() > f32::EPSILON
    }
}

// ---------------------------------------------------------------------------
// SlotState
// ---------------------------------------------------------------------------

/// Everything about the content currently in a slot.  Locked briefly by the
/// engine (GO/stop/live edits), the slot's event thread, and the render
/// thread's per-frame tick — never held across blocking calls.
pub(super) struct SlotState {
    /// The output voice occupying this slot (`None` = idle).
    pub voice_id: Option<VoiceId>,
    /// The AudioEngine voice carrying this content's audio track, if any.
    pub audio_voice_id: Option<VoiceId>,
    /// Cue geometry, kept for the pixel-crop at `VIDEO_RECONFIG`.
    pub geometry: VideoGeometry,
    /// An opted-in still whose texture keeps the entire source extent.
    pub still_raster: Option<super::source_placement::StillRaster>,
    jpeg_admission: Option<JpegAdmission>,
    /// A later geometry fade or inspector edit takes over the playback layer.
    pub geometry_owner: Option<Uuid>,
    /// `true` once the crop has been resolved for the current load.
    pub crop_applied: bool,
    /// Compositor sort key: explicit layers order below automatic ones,
    /// sequence breaks ties (newest on top).  See [`resolve_layer_key`].
    pub layer_key: u64,
    pub blend_mode: BlendMode,
    live_color: [f32; 4],
    /// Each source color parameter can be held by a different Fade.
    color_owners: [Option<Uuid>; 4],
    saturation_preview: Option<SaturationPreview>,
    pub silent_speed: Option<SilentSpeed>,
    whole_loop: Option<WholeLoop>,
    whole_loop_release_failed: bool,
    /// A retired movie's outstanding native operation fences reuse of its core.
    silent_worker_busy: bool,
    silent_stop: SilentStop,
    silent_speed_failed: bool,
    /// The cue's base opacity; the animation multiplies against it.
    pub base_opacity: f32,
    /// Runtime opacity animation (0 → base on reveal, → 0 on stop).
    pub anim: OpacityAnim,
    /// Set while a paused video load waits for its first frame
    /// (`PLAYBACK_RESTART`); carries the fade-in duration.
    pub pending_reveal: Option<u32>,
    /// Failsafe deadline for the reveal (mpv event missing/late).
    pub reveal_deadline: Option<Instant>,
    positioned_start: Option<PositionedStart>,
    /// Preloaded (Load Cue): the file is open and frame 0 is decoded, but the
    /// reveal is **held back** until the cue is actually started. Both the
    /// `PLAYBACK_RESTART` path and the reveal watchdog respect it, so nothing
    /// reaches the screen in the meantime.
    pub preloaded: bool,
    pub first_frame_ready: bool,
    /// `true` when the fade-out completes and mpv should be stopped + the
    /// slot released.  Drained by the render thread tick.
    pub pending_unload: bool,
    /// Freeze on last frame at EOF (`keep-open=yes` was set for this load).
    pub hold_last_frame: bool,
    /// QLab-style slice plan (`None` = plain playback).  Segments loop via
    /// mpv's ab-loop; the event thread advances `current` when `time-pos`
    /// crosses a segment boundary.
    pub slice_plan: Option<SlicePlan>,
    /// Monotonic per-slot generation guard (a slow event for load N must not
    /// touch load N+1).
    pub generation: u64,
    /// Set while this (incoming) content dissolves other layers away — see
    /// `crossfade.rs`.  Dropped once the dissolve has landed and its outgoing
    /// layers are gone, or when this content leaves the slot.
    pub crossfade: Option<CrossfadeLink>,
}

struct PositionedStart {
    source_ms: u64,
    file_loaded: bool,
    held: bool,
}

enum RevealExpiry {
    None,
    Fallback,
    Failed { voice: VoiceId, audio: Option<VoiceId> },
}

/// Slice program for one slot: `(start_s, end_s, play_count)` per segment;
/// `u32::MAX` = vamp.
pub(super) struct SlicePlan {
    pub segments: Vec<(f64, f64, u32)>,
    /// Index of the segment currently playing.
    pub current: usize,
    /// Devamp "stop at end of current slice" armed.
    pub stop_at_end: bool,
}

struct SaturationPreview {
    token: Uuid,
    value: f32,
    expires_at: Instant,
}

const SATURATION: usize = PictureColorParameter::Saturation as usize;

impl SlotState {
    pub(super) fn preview_saturation(&mut self, voice: VoiceId, token: Uuid, value: f32, expires_at: Instant) -> bool {
        if self.voice_id != Some(voice) || self.preloaded || self.pending_unload
            || self.pending_reveal.is_some() || !self.first_frame_ready || self.anim.current <= 0.0
            || !(0.0..=2.0).contains(&value)
        {
            return false;
        }
        self.saturation_preview = Some(SaturationPreview { token, value, expires_at });
        true
    }

    pub(super) fn cancel_saturation_preview(&mut self, voice: VoiceId, token: Uuid) -> bool {
        if self.voice_id != Some(voice) || !self.saturation_preview.as_ref().is_some_and(|preview| preview.token == token) {
            return false;
        }
        self.saturation_preview = None;
        true
    }

    pub(super) fn clear_saturation_preview(&mut self) {
        self.saturation_preview = None;
    }

    pub(super) fn effective_saturation(&self, now: Instant) -> f32 {
        self.saturation_preview.as_ref().filter(|preview| now < preview.expires_at)
            .map_or(self.live_color[SATURATION], |preview| preview.value)
    }

    pub(super) fn color_uniform(&self, now: Instant) -> [f32; 4] {
        let [_, brightness, contrast, hue] = self.live_color;
        [self.effective_saturation(now), brightness, contrast, hue / 360.0]
    }

    pub(super) fn claim_color(&mut self, voice: VoiceId, owner: Uuid, fields: [bool; 4]) -> [Option<f32>; 4] {
        if self.voice_id != Some(voice) || self.preloaded || self.pending_unload {
            return [None; 4];
        }
        if fields[SATURATION] {
            self.saturation_preview = None;
        }
        std::array::from_fn(|index| {
            fields[index].then(|| {
                self.color_owners[index] = Some(owner);
                self.live_color[index]
            })
        })
    }

    pub(super) fn write_color(&mut self, voice: VoiceId, owner: Uuid, values: [Option<f32>; 4]) -> [bool; 4] {
        if self.voice_id != Some(voice) || self.pending_unload {
            return [false; 4];
        }
        PictureColorParameter::ALL.map(|parameter| {
            let index = parameter as usize;
            let Some(value) = values[index] else { return false };
            if self.color_owners[index] != Some(owner) || !parameter.range().contains(&f64::from(value)) {
                return false;
            }
            if index == SATURATION { self.saturation_preview = None; }
            self.live_color[index] = value;
            true
        })
    }

    pub(super) fn accepts_rotation_fade(&self) -> bool {
        self.still_raster.is_some()
            && self.first_frame_ready
            && self.geometry.source_rotation_degrees.is_some_and(f64::is_finite)
    }

    pub(super) fn claim_geometry(
        &mut self, voice_id: VoiceId, owner: Uuid, require_still_rotation: bool,
    ) -> Option<VideoGeometry> {
        if self.voice_id != Some(voice_id) || self.preloaded || self.pending_unload
            || (require_still_rotation && !self.accepts_rotation_fade())
        {
            return None;
        }
        self.geometry_owner = Some(owner);
        Some(self.geometry)
    }

    fn idle() -> Self {
        Self {
            voice_id: None,
            audio_voice_id: None,
            geometry: VideoGeometry::default(),
            still_raster: None,
            jpeg_admission: None,
            geometry_owner: None,
            crop_applied: true,
            layer_key: 0,
            blend_mode: BlendMode::Normal,
            live_color: LayerStyle::default().source_color(),
            color_owners: [None; 4],
            saturation_preview: None,
            silent_speed: None,
            whole_loop: None,
            whole_loop_release_failed: false,
            silent_worker_busy: false,
            silent_stop: SilentStop::None,
            silent_speed_failed: false,
            base_opacity: 1.0,
            anim: OpacityAnim::resting(0.0),
            pending_reveal: None,
            reveal_deadline: None,
            positioned_start: None,
            preloaded: false,
            first_frame_ready: false,
            pending_unload: false,
            hold_last_frame: false,
            slice_plan: None,
            generation: 0,
            crossfade: None,
        }
    }

    fn apply_layer_style(&mut self, style: &LayerStyle, edit_color: [bool; 4]) {
        self.blend_mode = style.blend_mode;
        let source = style.source_color();
        for (index, edited) in edit_color.into_iter().enumerate() {
            if edited {
                self.live_color[index] = source[index];
                self.color_owners[index] = None;
            }
        }
        if edit_color[SATURATION] {
            self.saturation_preview = None;
        }
        // Preserve the GO sequence part when an explicit layer has been edited.
        self.layer_key = resolve_layer_key(style.layer, self.layer_key & 0xFF_FFFF_FFFF);
        let new_base = style.opacity.clamp(0.0, 1.0) as f32;
        if new_base != self.base_opacity
            && !self.anim.is_animating()
            && !self.pending_unload
            && self.pending_reveal.is_none()
            && !self.preloaded
        {
            self.anim.set(new_base);
        }
        self.base_opacity = new_base;
    }

    fn silent_worker_drained(&self) -> bool {
        !self.silent_worker_busy && self.silent_stop == SilentStop::None
    }

    // A live decoder that needs a fenced Stop cannot be replaced in the same
    // GO. Skip it before selecting a victim so a failed GO leaves it playing.
    fn can_steal(&self) -> bool {
        self.silent_speed.is_none() && self.whole_loop.is_none()
            && !self.jpeg_waiting() && self.silent_worker_drained()
    }

    fn jpeg_waiting(&self) -> bool {
        matches!(self.jpeg_admission, Some(JpegAdmission::Loading | JpegAdmission::Check(_)))
    }

    pub(super) fn jpeg_lifts_blackout_on_reveal(&self) -> bool {
        self.jpeg_admission.is_some()
    }

    fn take_jpeg_check(&mut self) -> Option<(u64, VoiceId, String)> {
        if !self.silent_worker_drained() || self.pending_unload { return None; }
        let Some(JpegAdmission::Check(path)) = self.jpeg_admission.as_ref() else { return None; };
        let voice = self.voice_id?;
        self.silent_worker_busy = true;
        Some((self.generation, voice, path.clone()))
    }

    fn matches_native_end(&self, entry: i64) -> bool {
        self.whole_loop.as_ref().is_none_or(|whole| match whole.phase {
            WholeLoopPhase::Ready(owner) | WholeLoopPhase::Queued(owner)
            | WholeLoopPhase::Released(owner) => owner == entry,
            _ => true,
        })
    }

    fn queue_whole_release(&mut self, voice: VoiceId) -> bool {
        if self.voice_id != Some(voice) || !self.first_frame_ready || self.preloaded
            || self.pending_reveal.is_some() || self.pending_unload
        {
            return false;
        }
        let Some(whole) = self.whole_loop.as_mut() else { return false };
        match whole.phase {
            WholeLoopPhase::Ready(entry) => whole.phase = WholeLoopPhase::Queued(entry),
            WholeLoopPhase::Queued(_) | WholeLoopPhase::Released(_) => {}
            WholeLoopPhase::Probe | WholeLoopPhase::Unsupported => return false,
        }
        true
    }

    fn defer_silent_stop(&mut self) -> bool {
        // A whole loop must retire through the worker from its initial probe,
        // including after a failed release has returned to Ready.
        let whole_loop = self.whole_loop.take().is_some();
        let waited_for_jpeg = self.jpeg_waiting();
        self.jpeg_admission = None;
        let opted_in = self.silent_speed.take().is_some() || whole_loop;
        if opted_in || waited_for_jpeg || !self.silent_worker_drained() {
            if self.silent_stop == SilentStop::None {
                self.silent_stop = SilentStop::Requested;
            }
            return true;
        }
        false
    }

    fn take_silent_work(&mut self) -> Option<(u64, SilentWork)> {
        if self.silent_worker_busy { return None; }
        let work = if self.silent_stop == SilentStop::Requested {
            self.silent_stop = SilentStop::None;
            SilentWork::Stop
        } else if self.voice_id.is_some() && !self.pending_unload {
            let revealed = self.pending_reveal.is_none() && !self.preloaded;
            if let Some(whole) = self.whole_loop.as_ref() {
                if !revealed || !self.first_frame_ready { return None; }
                let work = match whole.phase {
                    WholeLoopPhase::Probe => SilentWork::CheckWholeLoop(whole.path.clone()),
                    WholeLoopPhase::Queued(entry) => SilentWork::ReleaseWholeLoop(entry),
                    _ => return None,
                };
                self.silent_worker_busy = true;
                return Some((self.generation, work));
            }
            let speed = self.silent_speed.as_mut()?;
            let pause = if revealed { speed.requested_pause.take() } else { None };
            if let Some(paused) = pause {
                SilentWork::Pause(paused)
            } else if self.first_frame_ready && speed.file_loaded && speed.eligible.is_none() {
                SilentWork::Check(speed.path.clone())
            } else if speed.eligible == Some(true) {
                let rate = speed.requested.take()?;
                if rate == speed.rate { return None; }
                speed.rate_in_flight = true;
                SilentWork::SetRate(rate)
            } else {
                return None;
            }
        } else {
            return None;
        };
        self.silent_worker_busy = true;
        Some((self.generation, work))
    }

    fn finish_whole_loop_probe(&mut self, generation: u64, entry: Option<i64>) {
        self.silent_worker_busy = false;
        if self.generation == generation && self.voice_id.is_some() && !self.pending_unload {
            if let Some(whole) = self.whole_loop.as_mut() {
                whole.phase = entry.map_or(WholeLoopPhase::Unsupported, WholeLoopPhase::Ready);
            }
        }
    }

    /// Returns whether a native write failed on the live playback.
    fn finish_silent_work(&mut self, generation: u64, work: SilentWork, success: bool) -> bool {
        self.silent_worker_busy = false;
        if matches!(work, SilentWork::Stop) {
            if self.silent_stop == SilentStop::None {
                self.silent_stop = if success { SilentStop::AwaitingIdle } else { SilentStop::Requested };
            }
            return false;
        }
        if self.generation != generation || self.voice_id.is_none() || self.pending_unload {
            return false;
        }
        if let SilentWork::ReleaseWholeLoop(entry) = work {
            let Some(whole) = self.whole_loop.as_mut() else { return false };
            whole.phase = if success { WholeLoopPhase::Released(entry) } else { WholeLoopPhase::Ready(entry) };
            return !success;
        }
        let Some(speed) = self.silent_speed.as_mut() else { return false };
        match work {
            SilentWork::Check(_) => speed.eligible = Some(success),
            SilentWork::SetRate(rate) => {
                speed.rate_in_flight = false;
                if success {
                    speed.rate = rate;
                } else {
                    speed.eligible = Some(false);
                    speed.requested = None;
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    fn silent_stop_queue_empty(&mut self, idle_confirmed: bool) {
        self.silent_stop = match self.silent_stop {
            SilentStop::AwaitingIdle if idle_confirmed => SilentStop::DrainingEvents,
            SilentStop::DrainingEvents => SilentStop::None,
            other => other,
        };
    }

    fn take_reveal(&mut self) -> Option<(u32, Option<u64>)> {
        if self.preloaded || self.pending_unload || self.voice_id.is_none() || self.jpeg_waiting() {
            return None;
        }
        if self.positioned_start.as_ref().is_some_and(|start| start.held || !self.first_frame_ready) {
            return None;
        }
        let fade = self.pending_reveal.take()?;
        self.reveal_deadline = None;
        let silent_generation = self.silent_speed.as_mut().map(|speed| {
            // Preroll held mpv paused. The first frame releases that hold
            // through the same mailbox as Pause and Resume, in that order.
            speed.requested_pause = Some(speed.operator_paused);
            self.generation
        });
        Some((fade, silent_generation))
    }

    pub(super) fn positioned_reveal_pending(&self, voice: VoiceId) -> bool {
        self.voice_id == Some(voice) && self.positioned_start.is_some() && self.pending_reveal.is_some()
    }

    fn record_first_frame(&mut self, source_secs: Option<f64>) -> bool {
        if let Some(start) = &self.positioned_start {
            let matches = source_secs.is_some_and(|secs| secs.is_finite() && secs >= 0.0
                && secs * 1000.0 >= start.source_ms.saturating_sub(2) as f64);
            if !start.file_loaded || !matches {
                return false;
            }
        }
        self.first_frame_ready = true;
        self.pending_reveal.is_some() && !self.preloaded && !self.jpeg_waiting()
            && self.positioned_start.as_ref().is_none_or(|start| !start.held)
    }

    fn expire_reveal(&mut self, now: Instant) -> RevealExpiry {
        if self.preloaded || self.jpeg_waiting() || self.pending_reveal.is_none()
            || !self.reveal_deadline.is_some_and(|deadline| now >= deadline)
        {
            return RevealExpiry::None;
        }
        self.reveal_deadline = None;
        if self.positioned_start.is_none() {
            return RevealExpiry::Fallback;
        }
        let Some(voice) = self.voice_id else { return RevealExpiry::None };
        if self.first_frame_ready {
            return RevealExpiry::None; // A Pause can hold an admitted frame off-screen.
        }
        self.pending_reveal = None;
        self.pending_unload = true;
        self.anim.set(0.0);
        RevealExpiry::Failed { voice, audio: self.audio_voice_id.take() }
    }
}

pub(super) struct SilentSpeed {
    pub path: String,
    pub rate: f64,
    pub owner: Option<Uuid>,
    pub eligible: Option<bool>,
    pub requested: Option<f64>,
    rate_in_flight: bool,
    pub position_ms: Option<u64>,
    file_loaded: bool,
    requested_pause: Option<bool>,
    operator_paused: bool,
}

impl SilentSpeed {
    fn pending(path: String) -> Self {
        Self { path, rate: 1.0, owner: None, eligible: None, requested: None, rate_in_flight: false,
            position_ms: None, file_loaded: false, requested_pause: None, operator_paused: false }
    }

    pub(super) fn request_pause(&mut self, paused: bool) {
        self.operator_paused = paused;
        self.requested_pause = Some(paused);
    }

    pub(super) fn claim(&mut self, owner: Uuid) -> Option<SilentSpeedOrigin> {
        if self.eligible != Some(true) { return None; }
        self.owner = Some(owner);
        // Work still in the mailbox has no effect on the new owner's origin.
        self.requested = None;
        self.origin(owner)
    }

    pub(super) fn origin(&self, owner: Uuid) -> Option<SilentSpeedOrigin> {
        if self.eligible != Some(true) || self.owner != Some(owner) { return None; }
        Some(if self.rate_in_flight { SilentSpeedOrigin::Settling } else { SilentSpeedOrigin::Ready(self.rate) })
    }
}

struct WholeLoop {
    phase: WholeLoopPhase,
    path: String,
}

#[derive(Clone, Copy)]
enum WholeLoopPhase {
    Probe,
    Unsupported,
    Ready(i64),
    Queued(i64),
    Released(i64),
}

enum JpegAdmission {
    Loading,
    Check(String),
    Verified,
}

enum SilentWork {
    Check(String),
    CheckWholeLoop(String),
    ReleaseWholeLoop(i64),
    SetRate(f64),
    Pause(bool),
    Stop,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SilentStop {
    #[default]
    None,
    Requested,
    AwaitingIdle,
    DrainingEvents,
}

/// Sort key so explicit layers (0–1000) stack below automatic ones, newest
/// automatic content on top, ties broken by GO order.
pub(super) fn resolve_layer_key(layer: Option<u32>, seq: u64) -> u64 {
    match layer {
        // Explicit layer L → band L, ordered by seq inside the band.
        Some(l) => (l.min(1000) as u64) << 40 | (seq & 0xFF_FFFF_FFFF),
        // Automatic → above every explicit band.
        None => (1001u64 << 40) | (seq & 0xFF_FFFF_FFFF),
    }
}

// ---------------------------------------------------------------------------
// VideoSlot
// ---------------------------------------------------------------------------

/// One mpv context of the video pool.
pub(super) struct VideoSlot {
    pub index: usize,
    /// The output whose pool this slot belongs to.
    pub output_id: OutputId,
    output: Weak<Output>,
    /// That output's render signal — how the slot wakes its render thread.
    signal: Arc<(Mutex<bool>, Condvar)>,
    pub lib: Arc<MpvLib>,
    pub mpv_ctx: Arc<MpvCtx>,
    pub audio_engine: Arc<AudioEngine>,
    pub state: Mutex<SlotState>,
    /// mpv_render_context, created by the render thread (GL context needed);
    /// null until then.
    pub render_ctx: AtomicPtr<c_void>,
    /// Set at creation; cleared by the render thread once `render_ctx` is up.
    pub needs_render_init: AtomicBool,
    /// Tells the event thread to end (the output is being destroyed).
    shutdown: AtomicBool,
    /// The event thread, joined by [`retire`].
    event_thread: Mutex<Option<JoinHandle<()>>>,
    /// Set by [`retire`]: the mpv core is released with the slot's last user.
    destroy_on_drop: AtomicBool,
}

// SAFETY: the raw mpv pointers are only used through the thread-safe libmpv
// client API; render_ctx is only touched by the render thread.
unsafe impl Send for VideoSlot {}
unsafe impl Sync for VideoSlot {}

impl Drop for VideoSlot {
    fn drop(&mut self) {
        if self.destroy_on_drop.load(Ordering::Acquire) {
            // SAFETY: `retire` sets the flag only after the event thread has
            // returned, and the output's render thread frees `render_ctx`
            // before it exits — which happens before the output, and with it
            // this slot, can be dropped.  Nobody else holds the handle.
            unsafe { (self.lib.mpv_terminate_destroy)(self.mpv_ctx.0) };
        }
    }
}

impl VideoSlot {
    /// Wake the render thread of this slot's output.
    pub(super) fn wake(&self) {
        wake_signal(&self.signal);
    }

    pub(super) fn wake_event_worker(&self) {
        unsafe { (self.lib.mpv_wakeup)(self.mpv_ctx.0) };
    }

    /// Send an OutputStatus to the show event loop.
    fn send_status(&self, status: OutputStatus) {
        if let Some(tx) = OUTPUT_STATUS_TX.get() {
            let _ = tx.send(status);
        }
    }

    fn silent_speed_alert(&self) -> String {
        format!("silent-speed-write-{}-{}", self.output_id, self.index)
    }

    fn whole_loop_alert(&self) -> String {
        format!("whole-video-devamp-{}-{}", self.output_id, self.index)
    }

    fn clear_movie_worker_failures(&self, state: &mut SlotState) {
        if std::mem::take(&mut state.silent_speed_failed) {
            crate::health::clear(&self.silent_speed_alert());
        }
        self.clear_whole_loop_failure(state);
    }

    fn clear_whole_loop_failure(&self, state: &mut SlotState) {
        if std::mem::take(&mut state.whole_loop_release_failed) {
            crate::health::clear(&self.whole_loop_alert());
        }
    }
}

// ---------------------------------------------------------------------------
// Pool operations
// ---------------------------------------------------------------------------

/// Acquire a slot for new content: reuse an idle one, create one below the
/// cap, or steal the oldest occupied slot (hard stop + `Withdrawn`).
pub(super) fn acquire_slot(
    output: &Arc<Output>,
    lib: &Arc<MpvLib>,
    audio_engine: &Arc<AudioEngine>,
    preload: bool,
) -> Result<Arc<VideoSlot>> {
    // 1. Reuse an idle slot.
    for slot in output.slots_snapshot() {
        if let Ok(st) = slot.state.lock() {
            if st.voice_id.is_none() && !st.pending_unload && st.silent_worker_drained() {
                return Ok(Arc::clone(&slot));
            }
        }
    }

    // 2. Create a new one below the cap.
    let count = output.slots_snapshot().len();
    if count < MAX_VIDEO_SLOTS {
        return create_slot(output, lib, audio_engine);
    }

    if preload {
        return Err(anyhow!("All {MAX_VIDEO_SLOTS} video layers on '{}' are occupied; the Load cue left them playing", output.name()));
    }

    // 3. Steal the oldest (lowest sequence) occupied slot.
    let victim = output
        .slots_snapshot()
        .into_iter()
        .filter_map(|slot| {
            let st = slot.state.lock().ok()?;
            let key = st.layer_key & 0xFF_FFFF_FFFF;
            let available = st.can_steal();
            drop(st);
            available.then_some((key, slot))
        })
        .min_by_key(|(key, _)| *key)
        .map(|(_, slot)| slot)
        .ok_or_else(|| anyhow!("All video layers on '{}' are busy; wait for a stopped movie to release its layer", output.name()))?;
    log::warn!(
        "[slot] output '{}' pool exhausted ({MAX_VIDEO_SLOTS} slots) — stealing slot {}",
        output.name(),
        victim.index
    );
    hard_unload(&victim, UnloadReport::Withdrawn);
    Ok(victim)
}

/// Create a new mpv context + event thread and register the slot in `output`.
fn create_slot(
    output: &Arc<Output>,
    lib: &Arc<MpvLib>,
    audio_engine: &Arc<AudioEngine>,
) -> Result<Arc<VideoSlot>> {
    let ctx = unsafe { (lib.mpv_create)() };
    if ctx.is_null() {
        return Err(anyhow!("mpv_create() returned null for video slot"));
    }

    unsafe {
        opt_str(lib, ctx, "vo", "libmpv");
        opt_str(lib, ctx, "hwdec", hwdec_mode());
        opt_str(lib, ctx, "osc", "no");
        opt_str(lib, ctx, "osd-level", "0");
        opt_str(lib, ctx, "input-default-bindings", "no");
        opt_str(lib, ctx, "input-vo-keyboard", "no");
        opt_str(lib, ctx, "input-cursor", "no");
        opt_str(lib, ctx, "keep-open", "no");
        opt_str(lib, ctx, "idle", "yes");
        opt_str(lib, ctx, "ao", "null");
        opt_str(lib, ctx, "audio", "no");
        opt_str(lib, ctx, "video-sync", "desync");
        // Transparent background: where the slot has no pixels (letterbox,
        // idle) the FBO alpha is 0 so lower layers show through.
        // `background=none` is mpv ≥ 0.38; `alpha=yes` covers older libmpv
        // (Ubuntu 22.04 ships 0.34) — setting both is harmless.
        opt_str(lib, ctx, "background", "none");
        opt_str(lib, ctx, "alpha", "yes");

        let ret = (lib.mpv_initialize)(ctx);
        if ret < 0 {
            (lib.mpv_terminate_destroy)(ctx);
            return Err(anyhow!("mpv_initialize() failed for video slot: {ret}"));
        }

        // Observe time-pos so the event thread can advance slice plans when
        // playback crosses a segment boundary (fires ~once per video frame;
        // the handler is a cheap lock + compare when no plan is active).
        let time_pos = cs("time-pos");
        (lib.mpv_observe_property)(ctx, 0, time_pos.as_ptr(), MPV_FORMAT_DOUBLE);

        // Decoder diagnostics reach the event thread as log messages — that is
        // how a failed hwdec init is detected and worked around.
        (lib.mpv_request_log_messages)(ctx, cs("warn").as_ptr());
    }

    let index = output.slots_snapshot().len();
    let slot = Arc::new(VideoSlot {
        index,
        output_id: output.id,
        output: Arc::downgrade(output),
        signal: Arc::clone(&output.signal),
        lib: Arc::clone(lib),
        mpv_ctx: Arc::new(MpvCtx(ctx)),
        audio_engine: Arc::clone(audio_engine),
        state: Mutex::new(SlotState::idle()),
        render_ctx: AtomicPtr::new(std::ptr::null_mut()),
        needs_render_init: AtomicBool::new(true),
        shutdown: AtomicBool::new(false),
        event_thread: Mutex::new(None),
        destroy_on_drop: AtomicBool::new(false),
    });

    {
        let event_slot = Arc::clone(&slot);
        let handle = std::thread::Builder::new()
            .name(format!("inkue-slot-{}-{index}-events", output.name()))
            .spawn(move || slot_event_loop(event_slot))
            .map_err(|e| anyhow!("spawn slot event thread: {e}"))?;
        if let Ok(mut thread) = slot.event_thread.lock() {
            *thread = Some(handle);
        }
    }

    output
        .slots
        .write()
        .map_err(|_| anyhow!("slot registry poisoned"))?
        .push(Arc::clone(&slot));
    // The render thread creates the mpv_render_context + FBO on next wake.
    slot.wake();

    // Block until the render context exists (normally a few ms).  With
    // `vo=libmpv`, a `loadfile` whose track selection runs before the render
    // context is attached fails with NOTHING_TO_PLAY (-16): mpv sees a video
    // track but no VO to put it on, and with `audio=no` nothing is left —
    // the cue errors out at GO (observed in the field on fast local files).
    let deadline = Instant::now() + Duration::from_secs(2);
    while slot.render_ctx.load(Ordering::Acquire).is_null() {
        if Instant::now() >= deadline {
            log::warn!("[slot {index}] render context not ready after 2 s — loading anyway");
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    log::info!("[slot] created video slot {index} on output '{}'", output.name());
    Ok(slot)
}

/// What [`hard_unload`] tells the show about the voice it cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UnloadReport {
    /// Nothing (panic: the transport resets every cue itself).
    Silent,
    /// The content ended on its own terms (a Devamp stopping at its slice).
    Completed,
    /// The content was taken away (slot stolen, test pattern, output deleted).
    Withdrawn,
}

/// Immediately cut a slot's content: stop mpv, stop its audio voice, clear
/// the state, and tell the show as `report` says so the owning cue resets.
pub(super) fn hard_unload(slot: &Arc<VideoSlot>, report: UnloadReport) {
    let (voice, audio, crossfade, defer_stop) = {
        let Ok(mut st) = slot.state.lock() else { return };
        st.generation = st.generation.wrapping_add(1);
        slot.clear_movie_worker_failures(&mut st);
        let voice = st.voice_id.take();
        let audio = st.audio_voice_id.take();
        st.pending_reveal = None;
        st.reveal_deadline = None;
        st.preloaded = false;
        st.pending_unload = false;
        st.slice_plan = None;
        let crossfade = st.crossfade.take();
        st.anim.set(0.0);
        (voice, audio, crossfade, st.defer_silent_stop())
    };
    abandon_crossfade(crossfade);
    if defer_stop {
        slot.wake_event_worker();
    } else {
        stop_movie(slot);
    }
    if let Some(aid) = audio {
        let _ = slot.audio_engine.stop_voice(aid, 0, crate::engine::ring_command::FadeCurve::Linear);
    }
    if let Some(voice_id) = voice {
        match report {
            UnloadReport::Silent => {}
            UnloadReport::Completed => slot.send_status(OutputStatus::Completed { voice_id }),
            UnloadReport::Withdrawn => slot.send_status(OutputStatus::Withdrawn { voice_id }),
        }
    }
    slot.wake();
}

/// Parameters for [`load_into_slot`].
pub(super) struct SlotLoad {
    pub voice_id: VoiceId,
    pub audio_voice_id: Option<VoiceId>,
    pub url: String,
    pub is_image: bool,
    pub allow_silent_speed: bool,
    pub allow_whole_loop_release: bool,
    pub fade_in_ms: u32,
    pub loop_count: u32,
    pub start_ms: Option<u64>,
    pub require_positioned_frame: bool,
    pub end_ms: Option<u64>,
    pub display_duration_ms: Option<u64>,
    pub hold_last_frame: bool,
    pub live_source: bool,
    pub geometry: VideoGeometry,
    pub still_raster: Option<super::source_placement::StillRaster>,
    pub layer_style: LayerStyle,
    /// QLab-style slice segments `(start_s, end_s, play_count)`; empty = none.
    pub slices: Vec<(f64, f64, u32)>,
    /// Decode into the slot but hold the reveal (Load Cue) — see
    /// [`SlotState::preloaded`].
    pub preload: bool,
}

/// Load content into an (idle) slot.
pub(super) fn load_into_slot(slot: &Arc<VideoSlot>, load: SlotLoad) -> Result<()> {
    let path_cstr = CString::new(load.url.as_str()).map_err(|_| anyhow!("Video path contains a NUL byte"))?;
    let lib = &slot.lib;
    let ctx = slot.mpv_ctx.0;
    let seq = LAYER_SEQ.fetch_add(1, Ordering::Relaxed);
    let verify_jpeg = load.still_raster.is_some() && super::source_placement::is_jpeg_still(
        std::path::Path::new(&load.url)
    );

    let previous_crossfade = {
        let mut st = slot.state.lock().map_err(|_| anyhow!("Video layer is unavailable"))?;
        anyhow::ensure!(st.voice_id.is_none() && !st.pending_unload && st.silent_worker_drained(),
            "Video layer is still releasing a previous playback");
        st.generation = st.generation.wrapping_add(1);
        slot.clear_movie_worker_failures(&mut st);
        st.voice_id = Some(load.voice_id);
        st.audio_voice_id = load.audio_voice_id;
        st.geometry = load.geometry;
        st.still_raster = load.still_raster;
        st.jpeg_admission = verify_jpeg.then_some(JpegAdmission::Loading);
        st.geometry_owner = None;
        st.crop_applied = load.still_raster.is_some() || !load.geometry.has_crop();
        st.layer_key = resolve_layer_key(load.layer_style.layer, seq);
        st.blend_mode = load.layer_style.blend_mode;
        st.live_color = load.layer_style.source_color();
        st.color_owners = [None; 4];
        st.saturation_preview = None;
        st.silent_speed = load.allow_silent_speed.then(|| SilentSpeed::pending(load.url.clone()));
        st.whole_loop = load.allow_whole_loop_release.then(|| WholeLoop {
            phase: WholeLoopPhase::Probe,
            path: load.url.clone(),
        });
        st.base_opacity = load.layer_style.opacity.clamp(0.0, 1.0) as f32;
        st.pending_unload = false;
        let previous_crossfade = st.crossfade.take();
        st.hold_last_frame = load.hold_last_frame;
        st.slice_plan = if load.slices.is_empty() {
            None
        } else {
            Some(SlicePlan { segments: load.slices.clone(), current: 0, stop_at_end: false })
        };
        st.preloaded = load.preload;
        st.first_frame_ready = false;
        st.positioned_start = load.start_ms.filter(|_| load.require_positioned_frame)
            .map(|source_ms| PositionedStart { source_ms, file_loaded: false, held: false });
        if load.preload {
            // Preload: decode into the slot but keep it dark and paused. The
            // reveal is armed here and released by `start_preloaded`, so an
            // image gets the same treatment as a video — it must not appear
            // just because it decodes instantly.
            st.anim.set(0.0);
            st.pending_reveal = Some(load.fade_in_ms);
            st.reveal_deadline = None;
        } else if load.is_image && load.still_raster.is_none() {
            // Images decode near-instantly: start the reveal fade right away.
            st.pending_reveal = None;
            st.reveal_deadline = None;
            let base = st.base_opacity;
            if load.fade_in_ms > 0 {
                st.anim.set(0.0);
                st.anim.animate_to(base, load.fade_in_ms);
            } else {
                st.anim.set(base);
            }
        } else {
            // Wait for PLAYBACK_RESTART before revealing fresh source pixels.
            st.anim.set(0.0);
            st.pending_reveal = Some(load.fade_in_ms);
            st.reveal_deadline = st.positioned_start.as_ref()
                .map(|_| Instant::now() + Duration::from_secs(5));
        }
        previous_crossfade
    };
    abandon_crossfade(previous_crossfade);

    if load.still_raster.is_some() {
        // The layer pass owns fit, crop and placement. Ask mpv to decode the
        // whole PNG into this slot's bounded texture without stage clipping.
        super::apply_scalar_geometry(lib, ctx, &VideoGeometry {
            fit_mode: super::FitMode::Stretch,
            ..VideoGeometry::default()
        });
        unsafe { super::prop_str(lib, ctx, "video-crop", ""); }
    } else {
        // Legacy scalar geometry and deferred pixel crop.
        super::apply_scalar_geometry(lib, ctx, &load.geometry);
        let _ = try_apply_crop(lib, ctx, &load.geometry);
    }

    unsafe {
        let keep_open = if load.hold_last_frame && !load.is_image { "yes" } else { "no" };
        (lib.mpv_set_property_string)(ctx, cs("keep-open").as_ptr(), cs(keep_open).as_ptr());

        let mut opts: Vec<String> = vec!["audio=no".to_string(), "speed=1".to_string()];
        if verify_jpeg {
            // The metadata check requires the intended orientation to be in the
            // pixels. Only the compositor may rotate this JPEG during this run.
            opts.push("video-rotate=no".to_string());
        }
        if load.is_image {
            let duration_val = load
                .display_duration_ms
                .map(|ms| format!("{:.3}", ms as f64 / 1000.0))
                .unwrap_or_else(|| "inf".to_string());
            opts.push(format!("image-display-duration={duration_val}"));
            // A preloaded image stays paused like a video: its display
            // duration must start counting when the cue is started, not when
            // it was loaded.
            let pause = if load.preload || verify_jpeg { "yes" } else { "no" };
            (lib.mpv_set_property_string)(ctx, cs("pause").as_ptr(), cs(pause).as_ptr());
        } else {
            if let Some(start) = load.start_ms {
                opts.push(format!("start={:.3}", start as f64 / 1000.0));
            }
            if load.require_positioned_frame {
                opts.push("hr-seek=yes".to_string());
            }
            if let Some(end) = load.end_ms {
                opts.push(format!("end={:.3}", end as f64 / 1000.0));
            }
            if load.slices.is_empty() {
                let loop_val = if load.loop_count == u32::MAX {
                    "inf".to_string()
                } else if load.loop_count == 0 {
                    "no".to_string()
                } else {
                    load.loop_count.to_string()
                };
                opts.push(format!("loop-file={loop_val}"));
            } else {
                // Sliced playback: the segments own all looping (via ab-loop);
                // program segment 0's loop as loadfile options so it is active
                // before the first frame plays.
                opts.push("loop-file=no".to_string());
                let (a, b, count) = load.slices[0];
                if count != 1 {
                    opts.push(format!("ab-loop-a={a:.3}"));
                    opts.push(format!("ab-loop-b={b:.3}"));
                    let count_val = if count == u32::MAX {
                        "inf".to_string()
                    } else {
                        count.saturating_sub(1).to_string()
                    };
                    opts.push(format!("ab-loop-count={count_val}"));
                }
            }
            // Live sources: playback is timestamp-paced, so any backlog
            // buffered during device-open + the paused-load window would
            // persist as a *constant* glass-to-glass delay — `untimed`
            // displays frames as soon as they decode (safe: audio=no),
            // draining that backlog and pinning the feed to the live edge.
            if load.live_source {
                opts.push("cache=no".to_string());
                opts.push("untimed=yes".to_string());
                opts.push("demuxer-readahead-secs=0".to_string());
                opts.push("demuxer-lavf-analyzeduration=0.1".to_string());
                // lavf options from mpv's built-in low-latency profile.
                opts.push("demuxer-lavf-o-add=fflags=+nobuffer".to_string());
                opts.push("demuxer-lavf-probe-info=nostreams".to_string());
                // Frame-threaded decode adds one frame of delay per thread
                // (~270 ms for an 8-thread MJPEG webcam at 30 fps).
                opts.push("vd-lavc-threads=1".to_string());
                opts.push("video-latency-hacks=yes".to_string());
            }
            // Paused load: frame 0 decoded → PLAYBACK_RESTART → reveal.
            (lib.mpv_set_property_string)(ctx, cs("pause").as_ptr(), cs("yes").as_ptr());
        }

        let opts_cstr = cs(&opts.join(","));
        let cmd = cs("loadfile");
        let flags = cs("replace");
        let idx = cs("0");
        // loadfile signature: <url> <flags> <index> <options> (see fade.rs).
        let args: [*const c_char; 6] = [
            cmd.as_ptr(), path_cstr.as_ptr(), flags.as_ptr(),
            idx.as_ptr(), opts_cstr.as_ptr(), std::ptr::null(),
        ];
        let ret = (lib.mpv_command)(ctx, args.as_ptr());
        if ret < 0 {
            log::warn!("[slot {}] loadfile failed: {ret} ({})", slot.index, load.url);
            if let Some(voice_id) = vacate_after_end(slot, EndedPlayback::RejectedLoad(load.voice_id)) {
                slot.send_status(OutputStatus::Error {
                    voice_id,
                    message: format!("mpv rejected loadfile (code {ret})"),
                });
            }
        } else {
            if verify_jpeg {
                if let Ok(mut state) = slot.state.lock() {
                    if state.voice_id == Some(load.voice_id) {
                        state.jpeg_admission = Some(JpegAdmission::Check(load.url.clone()));
                    }
                }
                slot.wake_event_worker();
            }
            log::info!("[slot {}] loadfile: {} opts=[{}]", slot.index, load.url, opts.join(","));
        }
    }
    slot.wake();
    Ok(())
}

/// Begin the stop fade for a voice.  The render thread finishes the unload
/// once the opacity reaches 0.  Audio is stopped by the caller (engine).
pub(super) fn begin_stop(slot: &Arc<VideoSlot>, visual_fade_ms: u32) {
    let crossfade = {
        let Ok(mut st) = slot.state.lock() else { return };
        st.pending_reveal = None;
        st.reveal_deadline = None;
        st.preloaded = false;
        st.pending_unload = true;
        st.saturation_preview = None;
        st.audio_voice_id = None; // caller owns the audio stop
        if visual_fade_ms == 0 {
            st.anim.set(0.0);
        } else {
            st.anim.animate_to(0.0, visual_fade_ms);
        }
        st.crossfade.take()
    };
    abandon_crossfade(crossfade);
    slot.wake();
}

/// Per-frame slot maintenance, called by the render thread: advance the
/// opacity animation and finish pending unloads.  Returns the current
/// opacity and whether the slot still needs animation frames.
pub(super) fn tick_slot(slot: &Arc<VideoSlot>) -> (f32, bool) {
    // Reveal watchdog (mpv never signalled the first frame).
    let expired = {
        let Ok(mut st) = slot.state.lock() else { return (0.0, false) };
        st.expire_reveal(Instant::now())
    };
    match expired {
        RevealExpiry::None => {}
        RevealExpiry::Fallback => {
            log::warn!("[slot {}] reveal watchdog fired — forcing unpause", slot.index);
            reveal(slot, None);
        }
        RevealExpiry::Failed { voice, audio } => {
            if let Some(audio) = audio {
                let _ = slot.audio_engine.stop_voice(audio, 0, crate::engine::ring_command::FadeCurve::Linear);
            }
            slot.send_status(OutputStatus::Error {
                voice_id: voice, message: "Video did not decode a frame at the timecode entry position.".into(),
            });
        }
    }

    let Ok(mut st) = slot.state.lock() else { return (0.0, false) };
    if st.saturation_preview.as_ref().is_some_and(|preview| Instant::now() >= preview.expires_at) {
        st.saturation_preview = None;
    }
    let (opacity, _completed) = st.anim.tick();
    let animating = st.anim.is_animating();
    // Unload as soon as the stop fade has landed.  Do NOT require the tick to
    // report "just completed": a zero-fade stop (`begin_stop(0)` — hard cut,
    // the default for a Camera Cue) parks the animation at 0 already *resting*,
    // so `tick()` never completes — requiring it left the slot occupied and
    // mpv playing forever, which kept the capture device open (the next GO on
    // the same camera then failed with `dshow: device already in use`).
    let unload_now = st.pending_unload && opacity <= 0.0 && !animating;
    if unload_now {
        slot.clear_movie_worker_failures(&mut st);
        st.pending_unload = false;
        let crossfade = st.crossfade.take();
        st.voice_id = None;
        st.generation = st.generation.wrapping_add(1);
        let defer_stop = st.defer_silent_stop();
        drop(st);
        abandon_crossfade(crossfade);
        if defer_stop {
            slot.wake_event_worker();
        } else {
            stop_movie(slot);
        }
        return (0.0, false);
    }

    let Some(link) = st.crossfade.clone() else { return (opacity, animating) };
    let generation = st.generation;
    let base = st.base_opacity;
    let own_anim_busy = st.anim.is_animating();
    drop(st);
    drive_crossfade(slot, &link, generation, base, own_anim_busy, opacity, animating)
}

/// One frame of the crossfade this slot's content dissolves in with.
///
/// Outgoing layers on other outputs follow the falling curve; this content's
/// own opacity follows the rising one when nothing dissolves on its output
/// (otherwise it stays at full and the compositor mixes the pictures).  The
/// link is dropped once it has landed and every outgoing layer is gone — the
/// Fade Cue that started it stops them at its end.
fn drive_crossfade(
    slot: &Arc<VideoSlot>,
    link: &CrossfadeLink,
    generation: u64,
    base: f32,
    own_anim_busy: bool,
    opacity: f32,
    animating: bool,
) -> (f32, bool) {
    let now = Instant::now();
    let started = link.phase() != CrossfadePhase::Waiting;
    let mut any_outgoing_left = false;
    for outgoing in &link.outgoing {
        let Some(out_slot) = slot_for_voice(outgoing.voice) else { continue };
        any_outgoing_left = true;
        if let (true, Departure::Faded { from }) = (started, outgoing.departure) {
            settle_opacity(&out_slot, from * link.outgoing_factor(now));
        }
    }

    let mut opacity = opacity;
    if started && !link.dissolves_on_its_output() && !own_anim_busy {
        opacity = base * link.incoming_weight(now);
        if let Ok(mut st) = slot.state.lock() {
            if st.generation == generation {
                st.anim.set(opacity);
            }
        }
    }

    if link.has_landed(now) && !any_outgoing_left {
        if let Ok(mut st) = slot.state.lock() {
            if st.generation == generation {
                st.crossfade = None;
                log::info!("[slot {}] crossfade landed", slot.index);
            }
        }
    }
    (opacity, animating || link.is_animating(now))
}

/// Set a slot's opacity unless it is already there or busy with an animation
/// of its own (a stop fade must not be fought).
fn settle_opacity(slot: &Arc<VideoSlot>, opacity: f32) {
    let changed = {
        let Ok(mut st) = slot.state.lock() else { return };
        if st.anim.is_animating() || (st.anim.current - opacity).abs() < 1e-4 {
            false
        } else {
            st.anim.set(opacity);
            true
        }
    };
    if changed {
        slot.wake();
    }
}

/// A crossfade whose incoming content left before the dissolve landed gives
/// the outgoing pictures on other outputs back their opacity (those on the
/// incoming output need nothing: they reappear as soon as the mix stops).
fn abandon_crossfade(link: Option<CrossfadeLink>) {
    let Some(link) = link else { return };
    if link.has_landed(Instant::now()) {
        return;
    }
    for outgoing in &link.outgoing {
        if let Departure::Faded { from } = outgoing.departure {
            if let Some(out_slot) = slot_for_voice(outgoing.voice) {
                set_opacity_direct(&out_slot, from);
            }
        }
    }
}

/// Release a preloaded slot: the file is already open and decoded, so this
/// only lifts the hold and reveals it.  Returns `false` when the slot was not
/// preloaded (an ordinary resume then applies).
pub(super) fn start_preloaded(slot: &Arc<VideoSlot>) -> bool {
    let ready = {
        let Ok(mut st) = slot.state.lock() else { return false };
        if !st.preloaded {
            return false;
        }
        st.preloaded = false;
        if !st.first_frame_ready && st.reveal_deadline.is_some() {
            // GO can arrive before the paused load finishes. Keep both picture
            // and paired audio held until the normal first-frame signal.
            st.reveal_deadline = Some(Instant::now() + Duration::from_millis(2500));
        }
        st.first_frame_ready
    };
    if ready {
        reveal(slot, None);
    }
    log::info!("[slot {}] preloaded content started", slot.index);
    true
}

pub(super) fn hold_positioned_reveal(slot: &Arc<VideoSlot>, voice: VoiceId, paused: bool) -> bool {
    let (ready, generation) = {
        let Ok(mut state) = slot.state.lock() else { return false };
        if !state.positioned_reveal_pending(voice) { return false; }
        if let Some(start) = &mut state.positioned_start {
            start.held = paused;
        }
        (!paused && state.first_frame_ready, state.generation)
    };
    if ready {
        reveal(slot, Some(generation));
    }
    true
}

/// Reveal a paused video load: resume its audio voice, unpause mpv, start the
/// opacity fade-in.  Called on `PLAYBACK_RESTART` (or the watchdog).
fn reveal(slot: &Arc<VideoSlot>, expected_generation: Option<u64>) {
    let (fade_in_ms, base, audio, silent_generation, jpeg_generation, positioned_generation, owns_jpeg_worker) = {
        let Ok(mut st) = slot.state.lock() else { return };
        if expected_generation.is_some_and(|expected| expected != st.generation) { return; }
        let Some((fade, generation)) = st.take_reveal() else { return };
        let jpeg_generation = st.jpeg_lifts_blackout_on_reveal().then_some(st.generation);
        let positioned_generation = st.positioned_start.is_some().then_some(st.generation);
        // Pin the approved JPEG's decoder until its native unpause returns.
        let owns_jpeg_worker = jpeg_generation.is_some() && !st.silent_worker_busy;
        st.silent_worker_busy |= owns_jpeg_worker;
        (fade, st.base_opacity, st.audio_voice_id, generation, jpeg_generation, positioned_generation, owns_jpeg_worker)
    };

    if silent_generation.is_some() {
        slot.wake_event_worker();
    } else {
        if let Some(aid) = audio {
            let _ = slot.audio_engine.resume_voice(aid);
        }
        unsafe {
            (slot.lib.mpv_set_property_string)(
                slot.mpv_ctx.0, cs("pause").as_ptr(), cs("no").as_ptr(),
            );
        }
    }
    if let Ok(mut guard) = slot.state.lock() {
        let st = &mut *guard;
        if owns_jpeg_worker {
            st.silent_worker_busy = false;
            if st.silent_stop == SilentStop::Requested {
                slot.wake_event_worker();
            }
        }
        if silent_generation.or(jpeg_generation).or(positioned_generation)
            .is_some_and(|generation| generation != st.generation)
            || ((jpeg_generation.is_some() || positioned_generation.is_some()) && st.pending_unload)
        {
            return;
        }
        if jpeg_generation.is_some() {
            if let Some(output) = slot.output.upgrade() {
                output.set_overlay_alpha(0);
            }
        }
        if let Some(link) = st.crossfade.as_mut() {
            // A crossfade replaces the configured fade-in: the dissolve starts
            // on this very frame.
            link.clock.reveal(Instant::now());
            let start = if link.dissolves_on_its_output() { base } else { 0.0 };
            st.anim.set(start);
        } else if fade_in_ms > 0 {
            st.anim.set(0.0);
            st.anim.animate_to(base, fade_in_ms);
        } else {
            st.anim.set(base);
        }
        if st.whole_loop.is_some() {
            slot.wake_event_worker();
        }
    }
    slot.wake();
}

/// Link a crossfade into `incoming`'s content: every outgoing voice still on
/// screen dissolves away — in the compositor when it shares the incoming
/// output, by its own opacity on another one.  The dissolve starts with the
/// incoming content's first visible frame (now, when it is already showing).
///
/// Returns `false` when `incoming` no longer holds the request's voice.
pub(super) fn link_crossfade(incoming: &Arc<VideoSlot>, request: &CrossfadeRequest) -> bool {
    let outgoing: Vec<Outgoing> = request
        .outgoing
        .iter()
        .copied()
        .filter(|&voice| voice != request.incoming)
        .filter_map(|voice| {
            let slot = slot_for_voice(voice)?;
            let departure = if slot.output_id == incoming.output_id {
                Departure::Mixed
            } else {
                Departure::Faded { from: opacity_of(&slot) }
            };
            Some(Outgoing { voice, departure })
        })
        .collect();
    let mut link = CrossfadeLink::new(outgoing, request.duration_ms, request.up, request.down);

    let previous = {
        let Ok(mut guard) = incoming.state.lock() else { return false };
        let st = &mut *guard;
        if st.voice_id != Some(request.incoming) {
            return false;
        }
        if st.pending_reveal.is_none() && !st.preloaded {
            // Already on screen (an image, or preloaded content GO just
            // started): the dissolve starts now, from wherever its own fade-in
            // had got to — which the dissolve now replaces.
            link.clock.reveal(Instant::now());
            st.anim.set(if link.dissolves_on_its_output() { st.base_opacity } else { 0.0 });
        }
        st.crossfade.replace(link)
    };
    abandon_crossfade(previous);
    log::info!(
        "[slot {}] crossfade linked ({} ms, {} outgoing)",
        incoming.index,
        request.duration_ms,
        request.outgoing.len(),
    );
    incoming.wake();
    true
}

/// Where the crossfade into this slot's content stands.
pub(super) fn crossfade_phase(slot: &Arc<VideoSlot>) -> CrossfadePhase {
    slot.state
        .lock()
        .ok()
        .and_then(|st| st.crossfade.as_ref().map(CrossfadeLink::phase))
        .unwrap_or(CrossfadePhase::None)
}

/// Pause (`true`) or resume the dissolve with the Fade Cue driving it.
pub(super) fn hold_crossfade(slot: &Arc<VideoSlot>, paused: bool) {
    if let Ok(mut st) = slot.state.lock() {
        if let Some(link) = st.crossfade.as_mut() {
            let now = Instant::now();
            if paused {
                link.clock.pause(now);
            } else {
                link.clock.resume(now);
            }
        }
    }
    slot.wake();
}

/// The Fade Cue driving the dissolve was stopped.  A dissolve that has not
/// started is cancelled — the incoming content then appears as it would have
/// on its own — and one in progress freezes where it is, like any stopped fade.
pub(super) fn release_crossfade(slot: &Arc<VideoSlot>) {
    let cancelled = {
        let Ok(mut guard) = slot.state.lock() else { return };
        let st = &mut *guard;
        match st.crossfade.as_mut() {
            Some(link) if link.phase() == CrossfadePhase::Waiting => {
                // Revealed while paused: it was being held back by the mix, so
                // it simply shows now.  Not revealed yet: its own fade-in is
                // still armed in `pending_reveal`.
                if link.clock.is_revealed() {
                    st.anim.set(st.base_opacity);
                }
                st.crossfade.take()
            }
            Some(link) => {
                link.clock.pause(Instant::now());
                None
            }
            None => None,
        }
    };
    abandon_crossfade(cancelled);
    slot.wake();
}

/// The dissolve this slot's content renders on its own output: the voices it
/// mixes away there and the incoming weight.  `None` when nothing dissolves on
/// this output.
pub(super) fn dissolve_of(slot: &Arc<VideoSlot>, now: Instant) -> Option<(Vec<VoiceId>, f32)> {
    let st = slot.state.lock().ok()?;
    let link = st.crossfade.as_ref()?;
    if !link.dissolves_on_its_output() {
        return None;
    }
    let mixed: Vec<VoiceId> = link
        .outgoing
        .iter()
        .filter(|o| o.departure == Departure::Mixed)
        .map(|o| o.voice)
        .collect();
    Some((mixed, link.incoming_weight(now)))
}

// ---------------------------------------------------------------------------
// Live property updates
// ---------------------------------------------------------------------------

/// Keep the active opacity when editing the layer's color or stacking.
pub(super) fn set_layer_style(slot: &Arc<VideoSlot>, voice: VoiceId, style: &LayerStyle, edit_color: [bool; 4]) {
    if let Ok(mut st) = slot.state.lock() {
        if st.voice_id != Some(voice) { return; }
        st.apply_layer_style(style, edit_color);
    }
    slot.wake();
}

/// The slot needs frames: an opacity animation or a dissolve in progress.
pub(super) fn is_animating(slot: &Arc<VideoSlot>) -> bool {
    slot.state
        .lock()
        .map(|st| {
            st.anim.is_animating()
                || st.crossfade.as_ref().is_some_and(|link| link.is_animating(Instant::now()))
        })
        .unwrap_or(false)
}

/// Directly drive a slot's opacity (Fade Cue tick, ~30 fps).
pub(super) fn set_opacity_direct(slot: &Arc<VideoSlot>, opacity: f32) {
    if let Ok(mut st) = slot.state.lock() {
        st.anim.set(opacity);
    }
    slot.wake();
}

/// Current animated opacity of a voice's slot.
pub(super) fn opacity_of(slot: &Arc<VideoSlot>) -> f32 {
    slot.state.lock().map(|st| st.anim.current).unwrap_or(0.0)
}

/// Animate a slot's opacity to a target (EOF fade-out).
pub(super) fn animate_opacity(slot: &Arc<VideoSlot>, target: f32, duration_ms: u32) {
    if let Ok(mut st) = slot.state.lock() {
        st.anim.animate_to(target, duration_ms.max(1));
    }
    slot.wake();
}

// ---------------------------------------------------------------------------
// Slot event loop
// ---------------------------------------------------------------------------

fn slot_event_loop(slot: Arc<VideoSlot>) {
    let lib = Arc::clone(&slot.lib);
    let ctx = slot.mpv_ctx.0 as usize; // Send-safe copy for this thread only.

    loop {
        service_jpeg_check(&slot);
        service_silent_speed(&slot);
        let wait = slot.state.lock().map_or(1.0, |state| match state.silent_stop {
            SilentStop::DrainingEvents => 0.0,
            SilentStop::AwaitingIdle => 0.05,
            _ => 1.0,
        });
        let event = unsafe { (lib.mpv_wait_event)(ctx as *mut c_void, wait) };
        if slot.shutdown.load(Ordering::Acquire) {
            break;
        }
        if event.is_null() {
            continue;
        }
        let event_id = unsafe { (*event).event_id };

        match event_id {
            MPV_EVENT_SHUTDOWN => break,

            MPV_EVENT_NONE => confirm_silent_stop(&slot),

            MPV_EVENT_PLAYBACK_RESTART => {
                let (generation, positioned) = slot.state.lock().map_or((0, false), |state| {
                    (state.generation, state.positioned_start.is_some())
                });
                let source_secs = if positioned {
                    let mut seconds: f64 = 0.0;
                    let ret = unsafe {
                        (lib.mpv_get_property)(ctx as *mut c_void, cs("time-pos").as_ptr(), MPV_FORMAT_DOUBLE,
                            &mut seconds as *mut f64 as *mut c_void)
                    };
                    (ret >= 0).then_some(seconds)
                } else {
                    None
                };
                let ready = slot.state.lock().is_ok_and(|mut st| st.generation == generation && st.record_first_frame(source_secs));
                if ready {
                    reveal(&slot, Some(generation));
                    log::info!("[slot {}] first frame — revealed", slot.index);
                }
            }

            MPV_EVENT_FILE_LOADED => {
                // Report the media duration to the show event loop.
                let mut duration_secs: f64 = 0.0;
                let ret = unsafe {
                    let name = cs("duration");
                    (lib.mpv_get_property)(
                        ctx as *mut c_void, name.as_ptr(), MPV_FORMAT_DOUBLE,
                        &mut duration_secs as *mut f64 as *mut c_void,
                    )
                };
                if let Ok(mut st) = slot.state.lock() {
                    if let Some(vid) = st.voice_id {
                        if let Some(start) = &mut st.positioned_start {
                            start.file_loaded = true;
                        }
                        if let Some(speed) = st.silent_speed.as_mut() {
                            speed.file_loaded = true;
                        }
                        if ret == 0 {
                            slot.send_status(OutputStatus::Duration {
                                voice_id: vid,
                                duration_ms: (duration_secs * 1000.0) as u64,
                            });
                        }
                        // tick_slot ignores the deadline until a held Load is released.
                        if st.pending_reveal.is_some() {
                            st.reveal_deadline =
                                Some(Instant::now() + Duration::from_millis(2500));
                        }
                    }
                }
                slot.wake();
            }

            MPV_EVENT_VIDEO_RECONFIG => {
                // Source dimensions are now known — resolve the pixel crop.
                let geometry = slot.state.lock().ok().and_then(|mut st| {
                    if st.crop_applied {
                        None
                    } else {
                        st.crop_applied = true;
                        Some(st.geometry)
                    }
                });
                if let Some(g) = geometry {
                    let _ = try_apply_crop(&lib, ctx as *mut c_void, &g);
                }
                slot.wake();
            }

            MPV_EVENT_END_FILE => {
                let data_ptr = unsafe { (*event).data };
                let Some(end_data) = (unsafe { (data_ptr as *mut MpvEventEndFile).as_ref() })
                else {
                    continue;
                };
                match end_data.reason {
                    MPV_END_FILE_REASON_EOF => {
                        if let Some(vid) = vacate_after_end(&slot, EndedPlayback::Native(end_data.playlist_entry_id)) {
                            slot.send_status(OutputStatus::Completed { voice_id: vid });
                        }
                    }
                    MPV_END_FILE_REASON_ERROR => {
                        if let Some(vid) = vacate_after_end(&slot, EndedPlayback::Native(end_data.playlist_entry_id)) {
                            slot.send_status(OutputStatus::Error {
                                voice_id: vid,
                                message: format!("mpv error (code {})", end_data.error),
                            });
                        }
                    }
                    _ => {}
                }
            }

            MPV_EVENT_PROPERTY_CHANGE => {
                // time-pos update — advance the slice plan when playback
                // crossed the current segment's end (ab-loop keeps time *below*
                // the boundary while a segment still loops, so crossing it
                // means the segment is done).
                let data = unsafe { ((*event).data as *const MpvEventProperty).as_ref() };
                let Some(prop) = data else { continue };
                if prop.format != MPV_FORMAT_DOUBLE || prop.data.is_null() {
                    continue;
                }
                let time = unsafe { *(prop.data as *const f64) };

                enum SliceAction {
                    Advance((f64, f64, u32)),
                    Stop,
                }
                let action = {
                    let Ok(mut st) = slot.state.lock() else { continue };
                    if st.first_frame_ready && time.is_finite() && time >= 0.0 {
                        if let Some(speed) = st.silent_speed.as_mut().filter(|speed| speed.file_loaded) {
                            speed.position_ms = Some((time * 1000.0) as u64);
                        }
                    }
                    let Some(plan) = st.slice_plan.as_mut() else { continue };
                    let (_, end, _) = plan.segments[plan.current];
                    if time < end - 0.010 {
                        None
                    } else if plan.stop_at_end {
                        Some(SliceAction::Stop)
                    } else if plan.current + 1 < plan.segments.len() {
                        // Skip every boundary the clock already passed (a slow
                        // event must not re-program a stale segment).
                        while plan.current + 1 < plan.segments.len()
                            && time >= plan.segments[plan.current].1 - 0.010
                        {
                            plan.current += 1;
                        }
                        Some(SliceAction::Advance(plan.segments[plan.current]))
                    } else {
                        None // Last segment — natural EOF completes the cue.
                    }
                };
                match action {
                    Some(SliceAction::Advance(seg)) => {
                        apply_segment_loop(&lib, ctx as *mut c_void, seg);
                        log::info!(
                            "[slot {}] slice → [{:.3}s, {:.3}s) ×{}",
                            slot.index, seg.0, seg.1,
                            if seg.2 == u32::MAX { "∞".into() } else { seg.2.to_string() },
                        );
                    }
                    Some(SliceAction::Stop) => {
                        log::info!("[slot {}] devamp stop at slice boundary", slot.index);
                        hard_unload(&slot, UnloadReport::Completed);
                    }
                    None => {}
                }
            }

            MPV_EVENT_LOG_MESSAGE => {
                let data = unsafe { (*event).data as *const MpvEventLogMessage };
                if data.is_null() {
                    continue;
                }
                let level = unsafe { std::ffi::CStr::from_ptr((*data).level) }.to_string_lossy();
                let text = unsafe { std::ffi::CStr::from_ptr((*data).text) }.to_string_lossy();
                let trimmed = text.trim_end_matches('\n');
                if trimmed.is_empty() {
                    continue;
                }
                if matches!(level.as_ref(), "fatal" | "error") {
                    log::error!("[slot {}] [mpv] {trimmed}", slot.index);
                }
                // Belt and braces: libavcodec's messages (`h264: Failed setup
                // for format …`) reach the *first* mpv core created — the
                // overlay context, which is where the detection actually
                // fires (see `mpv_events`).  A slot only sees them if mpv ever
                // changes that routing, or if a slot happens to be first.
                if reports_hwdec_failure(trimmed) {
                    fall_back_to_software(&format!("slot {}", slot.index));
                }
            }

            _ => {}
        }
    }
}

/// Release a stopped silent core only after it has no current file and every
/// queued lifecycle event from that file has passed through this worker.
fn confirm_silent_stop(slot: &Arc<VideoSlot>) {
    let phase = slot.state.lock().map_or(SilentStop::None, |state| state.silent_stop);
    let idle_confirmed = match phase {
        SilentStop::AwaitingIdle => {
            let mut idle = 0_i32;
            let result = unsafe {
                (slot.lib.mpv_get_property)(
                    slot.mpv_ctx.0,
                    c"idle-active".as_ptr(),
                    MPV_FORMAT_FLAG,
                    (&mut idle as *mut i32).cast(),
                )
            };
            result == 0 && idle != 0
        }
        SilentStop::DrainingEvents => false,
        _ => return,
    };
    if let Ok(mut state) = slot.state.lock() {
        if state.silent_stop == phase && state.voice_id.is_none() {
            state.silent_stop_queue_empty(idle_confirmed);
        }
    }
}

fn service_jpeg_check(slot: &Arc<VideoSlot>) {
    let Some((generation, voice, path)) = slot.state.lock().ok()
        .and_then(|mut state| state.take_jpeg_check())
    else { return; };
    let result = super::still_jpeg::verify_file(std::path::Path::new(&path), || {
        slot.shutdown.load(Ordering::Acquire)
    });
    let ready_to_show = {
        let Ok(mut state) = slot.state.lock() else { return; };
        if state.generation != generation || state.voice_id != Some(voice) || state.pending_unload
            || slot.shutdown.load(Ordering::Acquire)
        {
            state.silent_worker_busy = false;
            return;
        }
        if result.is_ok() {
            state.jpeg_admission = Some(JpegAdmission::Verified);
            state.first_frame_ready && !state.preloaded
        } else {
            // Keep the slot occupied until its retired decoder is idle and
            // every old native event has drained.
            state.silent_stop = SilentStop::Requested;
            false
        }
    };
    match result {
        Ok(()) if ready_to_show => reveal(slot, Some(generation)),
        Err(error) => {
            if let Some(voice_id) = vacate_after_end(slot, EndedPlayback::RejectedLoad(voice)) {
                slot.send_status(OutputStatus::Error { voice_id, message: error.to_string() });
            }
        }
        _ => {}
    }
    if let Ok(mut state) = slot.state.lock() {
        state.silent_worker_busy = false;
    }
}

fn service_silent_speed(slot: &Arc<VideoSlot>) {
    let Some((generation, work)) = slot.state.lock().ok().and_then(|mut state| state.take_silent_work()) else { return };
    let success = match &work {
        SilentWork::Check(path) => super::silent_local_movie(&slot.lib, slot.mpv_ctx.0, path),
        SilentWork::CheckWholeLoop(path) => {
            let path_on_disk = std::path::Path::new(path);
            let eligible = path_on_disk.extension().and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4")
                    || extension.eq_ignore_ascii_case("mov"))
                && path_on_disk.is_file()
                && super::silent_local_movie(&slot.lib, slot.mpv_ctx.0, path)
                && unsafe { super::get_prop_i64(&slot.lib, slot.mpv_ctx.0, "remaining-file-loops") } == Some(-1);
            let entry = if eligible { movie_entry_id(slot) } else { None };
            if let Ok(mut state) = slot.state.lock() {
                state.finish_whole_loop_probe(generation, entry);
            }
            return;
        }
        SilentWork::ReleaseWholeLoop(entry) => {
            // The mailbox pins the slot, and this guard names the exact
            // loadfile playlist entry even if the same path is reused.
            // remaining-file-loops is read-only. This local option changes
            // that counter; the next loadfile supplies its own value.
            movie_entry_id(slot) == Some(*entry) && unsafe {
                (slot.lib.mpv_set_property_string)(slot.mpv_ctx.0, c"loop-file".as_ptr(), c"no".as_ptr()) >= 0
            }
        }
        SilentWork::SetRate(rate) => {
            let value = cs(&rate.to_string());
            unsafe { (slot.lib.mpv_set_property_string)(slot.mpv_ctx.0, c"speed".as_ptr(), value.as_ptr()) >= 0 }
        }
        SilentWork::Pause(paused) => {
            let value = if *paused { c"yes" } else { c"no" };
            unsafe { (slot.lib.mpv_set_property_string)(slot.mpv_ctx.0, c"pause".as_ptr(), value.as_ptr()) >= 0 }
        }
        SilentWork::Stop => stop_movie(slot),
    };
    let releasing_loop = matches!(work, SilentWork::ReleaseWholeLoop(_));
    if let Ok(mut state) = slot.state.lock() {
        let failed = state.finish_silent_work(generation, work, success);
        if releasing_loop {
            if failed {
                state.whole_loop_release_failed = true;
                crate::health::set(crate::health::HealthAlert::new(
                    slot.whole_loop_alert(),
                    crate::health::HealthLevel::Warning,
                    "The decoder refused the Whole cue Video release. Retry the Devamp or stop the Video cue.",
                ));
            } else if state.generation == generation && success {
                slot.clear_whole_loop_failure(&mut state);
            }
        } else if failed {
            state.silent_speed_failed = true;
            let file_name = state.silent_speed.as_ref()
                .and_then(|speed| std::path::Path::new(&speed.path).file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("the active movie");
            crate::health::set(crate::health::HealthAlert::new(
                slot.silent_speed_alert(),
                crate::health::HealthLevel::Warning,
                format!("The decoder refused a speed change for {file_name}. Restart the Video cue to retry."),
            ));
        }
    }
}

fn movie_entry_id(slot: &VideoSlot) -> Option<i64> {
    // This core is dedicated to a slot and only receives single-file replace
    // requests. Require that exactly that entry is active.
    let lib = &slot.lib;
    let ctx = slot.mpv_ctx.0;
    unsafe {
        if super::get_prop_i64(lib, ctx, "playlist-count") != Some(1)
            || super::get_prop_i64(lib, ctx, "playlist-pos") != Some(0)
        {
            return None;
        }
        super::get_prop_i64(lib, ctx, "playlist/0/id")
    }
}

fn stop_movie(slot: &Arc<VideoSlot>) -> bool {
    let args = [c"stop".as_ptr(), std::ptr::null()];
    (unsafe { (slot.lib.mpv_command)(slot.mpv_ctx.0, args.as_ptr()) }) >= 0
}

enum EndedPlayback {
    RejectedLoad(VoiceId),
    Native(i64),
}

/// Empty a slot whose file mpv has finished with (end of file or error): its
/// audio voice stops, a crossfade into it is abandoned.  Returns the voice that
/// was playing, if any.
fn vacate_after_end(slot: &Arc<VideoSlot>, playback: EndedPlayback) -> Option<VoiceId> {
    let (voice, audio, crossfade) = {
        let mut st = slot.state.lock().ok()?;
        let current = match playback {
            EndedPlayback::RejectedLoad(voice) => st.voice_id == Some(voice),
            EndedPlayback::Native(entry) => st.matches_native_end(entry),
        };
        if !current { return None; }
        slot.clear_movie_worker_failures(&mut st);
        let voice = st.voice_id.take();
        let audio = st.audio_voice_id.take();
        st.pending_reveal = None;
        st.reveal_deadline = None;
        st.preloaded = false;
        st.silent_speed = None;
        let owned_whole_loop = st.whole_loop.take().is_some();
        let waiting_jpeg = st.jpeg_waiting();
        st.jpeg_admission = None;
        if (owned_whole_loop || waiting_jpeg) && st.silent_stop == SilentStop::None {
            // END_FILE has arrived, but properties and lifecycle notifications
            // from this file can still be queued on the same mpv client.
            st.silent_stop = SilentStop::DrainingEvents;
        }
        st.saturation_preview = None;
        st.pending_unload = false;
        st.slice_plan = None;
        let crossfade = st.crossfade.take();
        st.anim.set(0.0);
        (voice, audio, crossfade)
    };
    abandon_crossfade(crossfade);
    if let Some(aid) = audio {
        let _ = slot.audio_engine.stop_voice(aid, 0, crate::engine::ring_command::FadeCurve::Linear);
    }
    slot.wake();
    voice
}

/// Drop every slot back to software decoding after a failed hwdec init.
///
/// libmpv does not reliably recover on its own: it retries the hardware
/// decoder frame after frame and can hand the compositor half-decoded frames,
/// which shows up as a green cast with a torn band across the picture (issue
/// #5).  Setting `hwdec=no` reinitialises the decoder in place, so the picture
/// corrects itself — usually before the first frame is even revealed, because
/// video loads start paused.  The switch is latched for the session and
/// applied to every existing slot: one GPU that refuses a codec profile
/// refuses it in every slot.
///
/// `origin` names the context that saw the message, for the log line only —
/// the failure is a property of the machine, not of one slot.  Skipped when
/// the operator pinned a mode with `INKUE_HWDEC`.
pub(super) fn fall_back_to_software(origin: &str) {
    if hwdec_override().is_some() {
        return;
    }
    if SOFTWARE_DECODE_ONLY.swap(true, Ordering::SeqCst) {
        return; // Already software-only — mpv repeats the message per frame.
    }
    log::warn!(
        "[{origin}] hardware decoding failed — switching every video slot to \
         software decoding for this session",
    );
    for slot in all_slots() {
        unsafe {
            (slot.lib.mpv_set_property_string)(
                slot.mpv_ctx.0,
                cs("hwdec").as_ptr(),
                cs("no").as_ptr(),
            );
        }
        slot.wake();
    }
}

/// Program mpv's ab-loop for `seg` — or clear it when the segment plays once.
fn apply_segment_loop(lib: &Arc<MpvLib>, ctx: *mut c_void, seg: (f64, f64, u32)) {
    let (a, b, count) = seg;
    unsafe {
        if count == 1 {
            (lib.mpv_set_property_string)(ctx, cs("ab-loop-a").as_ptr(), cs("no").as_ptr());
            (lib.mpv_set_property_string)(ctx, cs("ab-loop-b").as_ptr(), cs("no").as_ptr());
        } else {
            let count_val = if count == u32::MAX {
                "inf".to_string()
            } else {
                count.saturating_sub(1).to_string()
            };
            (lib.mpv_set_property_string)(ctx, cs("ab-loop-count").as_ptr(), cs(&count_val).as_ptr());
            (lib.mpv_set_property_string)(ctx, cs("ab-loop-a").as_ptr(), cs(&format!("{a:.3}")).as_ptr());
            (lib.mpv_set_property_string)(ctx, cs("ab-loop-b").as_ptr(), cs(&format!("{b:.3}")).as_ptr());
        }
    }
}

/// Arm the exact revealed voice without making any decoder call under GO.
pub(super) fn release_whole_loop(slot: &Arc<VideoSlot>, voice: VoiceId) -> bool {
    let accepted = slot.state.lock().is_ok_and(|mut state| state.queue_whole_release(voice));
    if accepted {
        slot.wake_event_worker();
    }
    accepted
}

/// Devamp: release the slot's current slice loop.  The pass in progress
/// finishes (ab-loop-count → 0 lets playback continue past B), then the plan
/// advances normally — or the slot stops at the boundary when `stop_at_end`.
/// No-op for unsliced content.
pub(super) fn devamp_slot(slot: &Arc<VideoSlot>, stop_at_end: bool) {
    {
        let Ok(mut st) = slot.state.lock() else { return };
        let Some(plan) = st.slice_plan.as_mut() else { return };
        if stop_at_end {
            plan.stop_at_end = true;
        }
    }
    unsafe {
        let key = cs("ab-loop-count");
        let val = cs("0");
        (slot.lib.mpv_set_property_string)(slot.mpv_ctx.0, key.as_ptr(), val.as_ptr());
    }
    log::info!("[slot {}] devamp (stop_at_end={stop_at_end})", slot.index);
}

// ---------------------------------------------------------------------------
// Pool-wide operations
// ---------------------------------------------------------------------------

/// Take a slot out of service for good (its output is being destroyed): its
/// content stops — the owning cue resets through `Withdrawn` — and its event
/// thread ends.  The mpv core itself goes when the last reference to the slot
/// does, after the render thread has freed the render context.
pub(super) fn retire(slot: &Arc<VideoSlot>) {
    hard_unload(slot, UnloadReport::Withdrawn);
    slot.shutdown.store(true, Ordering::Release);
    // SAFETY: the core is alive (we hold the slot); mpv_wakeup only interrupts
    // the event thread's mpv_wait_event.
    unsafe { (slot.lib.mpv_wakeup)(slot.mpv_ctx.0) };
    let handle = slot.event_thread.lock().ok().and_then(|mut t| t.take());
    if let Some(handle) = handle {
        let _ = handle.join();
    }
    slot.destroy_on_drop.store(true, Ordering::Release);
}

/// Panic: hard-unload every slot (double-Escape backstop).
pub(super) fn panic_all() {
    for slot in all_slots() {
        hard_unload(&slot, UnloadReport::Silent);
    }
}

/// Current playback position of a voice's video, in ms.
pub(super) fn position_ms(slot: &Arc<VideoSlot>) -> Option<u64> {
    let mut secs: f64 = 0.0;
    let name = cs("time-pos");
    let ret = unsafe {
        (slot.lib.mpv_get_property)(
            slot.mpv_ctx.0, name.as_ptr(), MPV_FORMAT_DOUBLE,
            &mut secs as *mut f64 as *mut c_void,
        )
    };
    (ret == 0 && secs >= 0.0).then_some((secs * 1000.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecode_positioned_entry_waits_for_the_requested_frame_and_respects_pause() {
        let now = Instant::now();
        let voice = Uuid::new_v4();
        let audio = Uuid::new_v4();
        let prepare = || {
            let mut state = SlotState::idle();
            state.voice_id = Some(voice);
            state.audio_voice_id = Some(audio);
            state.pending_reveal = Some(0);
            state.reveal_deadline = Some(now);
            state.positioned_start = Some(PositionedStart { source_ms: 4_000, file_loaded: false, held: true });
            state
        };
        let mut state = prepare();
        assert!(!state.record_first_frame(Some(4.0))); // Ignore a restart before this load arrived.
        state.positioned_start.as_mut().unwrap().file_loaded = true;
        assert!(!state.record_first_frame(Some(0.0)));
        assert!(state.take_reveal().is_none());
        assert!(!state.record_first_frame(Some(4.0))); // Verify it while still held.
        assert!(state.first_frame_ready);
        assert!(matches!(state.expire_reveal(now), RevealExpiry::None));
        assert!(state.take_reveal().is_none());
        state.positioned_start.as_mut().unwrap().held = false;
        assert!(state.take_reveal().is_some());

        let mut failed = prepare();
        assert!(matches!(failed.expire_reveal(now), RevealExpiry::Failed { voice: id, audio: Some(sound) } if id == voice && sound == audio));
        assert!(failed.pending_unload);
        assert!(failed.take_reveal().is_none());
    }

    fn visible_picture(voice: VoiceId) -> SlotState {
        let mut state = SlotState::idle();
        state.voice_id = Some(voice);
        state.first_frame_ready = true;
        state.anim.set(1.0);
        state
    }

    #[test]
    fn saturation_preview_expires_without_ever_taking_fade_ownership() {
        let voice = Uuid::new_v4();
        let previous = Uuid::new_v4();
        let next = Uuid::new_v4();
        let token = Uuid::new_v4();
        let mut state = visible_picture(voice);
        let now = Instant::now();
        let deadline = now + Duration::from_secs(2);

        assert_eq!(state.claim_color(voice, previous, [true, false, false, false])[SATURATION], Some(1.0));
        assert!(state.write_color(voice, previous, [Some(1.25), None, None, None])[SATURATION]);
        assert!(state.preview_saturation(voice, token, 0.0, deadline));
        assert_eq!(state.effective_saturation(now), 0.0);
        assert_eq!(state.effective_saturation(deadline), 1.25);
        assert_eq!(state.color_owners[SATURATION], Some(previous));

        assert!(state.write_color(voice, previous, [Some(1.0), None, None, None])[SATURATION]);
        assert_eq!(state.effective_saturation(now), 1.0);
        assert!(state.preview_saturation(voice, token, 0.25, deadline));
        assert_eq!(state.claim_color(voice, next, [true, false, false, false])[SATURATION], Some(1.0));
        assert!(state.write_color(voice, next, [Some(1.75), None, None, None])[SATURATION]);
        assert!(!state.cancel_saturation_preview(voice, token));
        assert!(!state.write_color(voice, previous, [Some(0.0), None, None, None])[SATURATION]);
        assert_eq!(state.effective_saturation(now), 1.75);
    }

    #[test]
    fn only_the_visible_voice_and_latest_preview_token_can_change_the_overlay() {
        let voice = Uuid::new_v4();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(2);
        let mut state = visible_picture(voice);

        assert!(!state.preview_saturation(Uuid::new_v4(), first, 0.0, deadline));
        assert!(!state.preview_saturation(voice, first, f32::NAN, deadline));
        state.preloaded = true;
        assert!(!state.preview_saturation(voice, first, 0.0, deadline));
        state.preloaded = false;
        state.pending_reveal = Some(0);
        assert!(!state.preview_saturation(voice, first, 0.0, deadline));
        state.pending_reveal = None;
        state.first_frame_ready = false;
        assert!(!state.preview_saturation(voice, first, 0.0, deadline));
        state.first_frame_ready = true;
        state.pending_unload = true;
        assert!(!state.preview_saturation(voice, first, 0.0, deadline));
        state.pending_unload = false;
        assert!(state.preview_saturation(voice, first, 0.0, deadline));
        assert!(state.preview_saturation(voice, second, 0.5, deadline));
        assert!(!state.cancel_saturation_preview(voice, first));
        assert_eq!(state.effective_saturation(now), 0.5);

        state.apply_layer_style(&LayerStyle::default(), [false; 4]);
        assert_eq!(state.effective_saturation(now), 0.5);
        state.apply_layer_style(&LayerStyle { saturation: 1.5, ..LayerStyle::default() }, [true, false, false, false]);
        assert_eq!(state.effective_saturation(now), 1.5);
        assert!(!state.cancel_saturation_preview(voice, second));
    }

    #[test]
    fn jpeg_waits_for_verified_markers_and_stop_fences_a_retired_scan() {
        let mut state = SlotState::idle();
        let voice = Uuid::new_v4();
        state.voice_id = Some(voice);
        state.first_frame_ready = true;
        state.pending_reveal = Some(0);
        state.jpeg_admission = Some(JpegAdmission::Loading);
        assert!(state.take_reveal().is_none());
        assert!(state.take_jpeg_check().is_none(), "loadfile has not returned");

        state.jpeg_admission = Some(JpegAdmission::Check("rehearsal.jpg".into()));
        assert!(state.take_reveal().is_none());
        assert!(!state.can_steal());
        let (generation, owner, _) = state.take_jpeg_check().unwrap();
        assert_eq!(generation, state.generation);
        assert_eq!(owner, voice);
        assert!(state.take_jpeg_check().is_none());

        state.generation += 1;
        state.voice_id = None;
        assert!(state.defer_silent_stop());
        assert!(state.jpeg_admission.is_none());
        assert!(!state.silent_worker_drained());
    }

    #[test]
    fn whole_loop_release_requires_the_verified_exposed_voice_and_exact_native_entry() {
        let mut state = SlotState::idle();
        let voice = Uuid::new_v4();
        state.voice_id = Some(voice);
        state.whole_loop = Some(WholeLoop {
            phase: WholeLoopPhase::Probe,
            path: "rehearsal.mp4".into(),
        });

        assert!(!state.queue_whole_release(voice));
        assert!(state.take_silent_work().is_none());
        state.first_frame_ready = true;
        state.preloaded = true;
        assert!(state.take_silent_work().is_none());
        assert!(!state.queue_whole_release(voice));
        state.preloaded = false;

        let (generation, probe) = state.take_silent_work().unwrap();
        assert!(matches!(probe, SilentWork::CheckWholeLoop(_)));
        state.finish_whole_loop_probe(generation, Some(42));

        assert!(!state.queue_whole_release(Uuid::new_v4()));
        assert!(state.queue_whole_release(voice));
        assert!(state.queue_whole_release(voice));
        let (generation, release) = state.take_silent_work().unwrap();
        assert!(matches!(release, SilentWork::ReleaseWholeLoop(42)));
        assert!(!state.matches_native_end(41));
        assert!(state.matches_native_end(42));
        assert!(state.finish_silent_work(generation, release, false), "report a failed native write");

        assert!(state.queue_whole_release(voice), "allow retry after rejection");
        let (generation, release) = state.take_silent_work().unwrap();
        assert!(!state.finish_silent_work(generation, release, true));
        assert!(state.take_silent_work().is_none(), "release is one-shot");
        assert!(!state.matches_native_end(41));
        assert!(state.matches_native_end(42));
    }

    #[test]
    fn whole_loop_stop_fences_reuse_before_a_release_can_be_queued() {
        for phase in [WholeLoopPhase::Probe, WholeLoopPhase::Unsupported, WholeLoopPhase::Ready(42)] {
            let mut state = SlotState::idle();
            state.voice_id = Some(Uuid::new_v4());
            state.whole_loop = Some(WholeLoop { phase, path: "rehearsal.mp4".into() });
            assert!(!state.can_steal(), "a GO at capacity must leave the movie playing");

            // Match render-thread Stop: retire the voice before posting the
            // decoder command. Idle acquisition still must not reuse this slot.
            state.voice_id = None;
            state.generation += 1;
            assert!(state.defer_silent_stop());
            assert!(!state.silent_worker_drained());
            assert!(!state.can_steal());

            let (generation, stop) = state.take_silent_work().unwrap();
            assert!(matches!(stop, SilentWork::Stop));
            assert!(!state.silent_worker_drained());
            assert!(!state.finish_silent_work(generation, stop, true));
            state.silent_stop_queue_empty(false);
            assert!(!state.silent_worker_drained(), "decoder is still stopping");
            state.silent_stop_queue_empty(true);
            assert!(!state.silent_worker_drained(), "old events are still queued");
            state.silent_stop_queue_empty(false);
            assert!(state.silent_worker_drained());
            assert!(state.can_steal());
        }
    }

    #[test]
    fn stop_during_whole_loop_release_pins_the_retired_core_until_events_drain() {
        let mut state = SlotState::idle();
        let voice = Uuid::new_v4();
        state.voice_id = Some(voice);
        state.first_frame_ready = true;
        state.whole_loop = Some(WholeLoop {
            phase: WholeLoopPhase::Ready(60),
            path: "rehearsal.mp4".into(),
        });
        assert!(state.queue_whole_release(voice));

        let (generation, release) = state.take_silent_work().unwrap();
        state.generation += 1;
        state.voice_id = None;
        assert!(state.defer_silent_stop());
        assert!(!state.silent_worker_drained());
        assert!(!state.finish_silent_work(generation, release, false), "ignore a retired write result");
        assert!(state.whole_loop.is_none());
        assert!(!state.silent_worker_drained());

        let (generation, stop) = state.take_silent_work().unwrap();
        assert!(matches!(stop, SilentWork::Stop));
        assert!(!state.finish_silent_work(generation, stop, true));
        state.silent_stop_queue_empty(true);
        assert!(!state.silent_worker_drained());
        state.silent_stop_queue_empty(false);
        assert!(state.silent_worker_drained());
        assert!(!state.queue_whole_release(voice));
    }

    #[test]
    fn a_successor_gets_the_applied_rate_after_the_previous_write_settles() {
        let mut state = SlotState::idle();
        state.voice_id = Some(Uuid::new_v4());
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut speed = SilentSpeed::pending("movie.mp4".into());
        speed.eligible = Some(true);
        speed.claim(first);
        speed.requested = Some(2.0);
        state.silent_speed = Some(speed);
        let (generation, write) = state.take_silent_work().unwrap();

        let speed = state.silent_speed.as_mut().unwrap();
        speed.requested = Some(3.0);
        assert_eq!(speed.claim(second), Some(SilentSpeedOrigin::Settling));
        assert_eq!(speed.origin(first), None);
        assert_eq!(speed.requested, None);
        assert!(!state.finish_silent_work(generation, write, true));
        assert_eq!(state.silent_speed.as_ref().unwrap().origin(second), Some(SilentSpeedOrigin::Ready(2.0)));
        assert!(state.take_silent_work().is_none());
    }

    #[test]
    fn a_late_rate_failure_invalidates_the_live_claim_and_discards_a_queued_reset() {
        let mut state = SlotState::idle();
        state.voice_id = Some(Uuid::new_v4());
        let reset = Uuid::new_v4();
        let mut speed = SilentSpeed::pending("movie.mp4".into());
        speed.eligible = Some(true);
        speed.requested = Some(2.0);
        state.silent_speed = Some(speed);
        let (generation, write) = state.take_silent_work().unwrap();
        let speed = state.silent_speed.as_mut().unwrap();
        assert_eq!(speed.claim(reset), Some(SilentSpeedOrigin::Settling));
        speed.requested = Some(1.0);

        assert!(state.finish_silent_work(generation, write, false));
        let speed = state.silent_speed.as_ref().unwrap();
        assert_eq!(speed.origin(reset), None);
        assert_eq!(speed.requested, None);
        assert_eq!(speed.rate, 1.0);
    }

    #[test]
    fn rotation_claim_requires_a_decoded_opted_in_png_without_stealing_another_geometry_claim() {
        let mut state = SlotState::idle();
        let voice = Uuid::new_v4();
        let owner = Uuid::new_v4();
        let rotation_owner = Uuid::new_v4();
        state.voice_id = Some(voice);
        assert!(state.claim_geometry(voice, owner, false).is_some());
        assert!(state.claim_geometry(voice, rotation_owner, true).is_none());
        assert_eq!(state.geometry_owner, Some(owner));

        state.still_raster = Some(super::super::source_placement::StillRaster { width: 64, height: 32 });
        state.geometry.source_rotation_degrees = Some(350.5);
        assert!(state.claim_geometry(voice, rotation_owner, true).is_none());
        state.first_frame_ready = true;
        state.preloaded = true;
        assert!(state.claim_geometry(voice, rotation_owner, true).is_none());
        state.preloaded = false;
        state.pending_unload = true;
        assert!(state.claim_geometry(voice, rotation_owner, true).is_none());
        state.pending_unload = false;
        assert!(state.claim_geometry(Uuid::new_v4(), rotation_owner, true).is_none());
        assert_eq!(state.geometry_owner, Some(owner));
        assert_eq!(state.claim_geometry(voice, rotation_owner, true).unwrap().source_rotation_degrees, Some(350.5));
    }

    #[test]
    fn silent_movie_pause_during_preroll_survives_first_frame_and_preload_hold() {
        let mut state = SlotState::idle();
        state.voice_id = Some(Uuid::new_v4());
        state.pending_reveal = Some(0);
        state.silent_speed = Some(SilentSpeed::pending("movie.mp4".into()));

        state.silent_speed.as_mut().unwrap().request_pause(true);
        assert!(state.take_silent_work().is_none());

        state.first_frame_ready = true;
        state.preloaded = true;
        assert!(state.take_reveal().is_none());
        assert!(state.take_silent_work().is_none());
        state.preloaded = false;

        assert_eq!(state.take_reveal(), Some((0, Some(state.generation))));
        let (generation, pause) = state.take_silent_work().unwrap();
        assert!(matches!(pause, SilentWork::Pause(true)));
        state.finish_silent_work(generation, pause, true);
        assert!(state.silent_speed.as_ref().unwrap().operator_paused);
        assert!(state.take_silent_work().is_none());

        state.silent_speed.as_mut().unwrap().request_pause(false);
        let (_, resume) = state.take_silent_work().unwrap();
        assert!(matches!(resume, SilentWork::Pause(false)));
    }

    #[test]
    fn a_pause_racing_silent_reveal_replaces_its_queued_unpause() {
        let mut state = SlotState::idle();
        state.voice_id = Some(Uuid::new_v4());
        state.first_frame_ready = true;
        state.pending_reveal = Some(0);
        state.silent_speed = Some(SilentSpeed::pending("movie.mp4".into()));
        assert!(state.take_reveal().is_some());
        state.silent_speed.as_mut().unwrap().request_pause(true);
        assert!(matches!(state.take_silent_work(), Some((_, SilentWork::Pause(true)))));
    }

    #[test]
    fn saturation_edits_preserve_live_and_preloaded_opacity() {
        let mut state = SlotState::idle();
        state.anim.set(0.35); // the last value written by a Fade cue
        let mut style = LayerStyle {
            saturation: 0.0,
            ..LayerStyle::default()
        };
        let geometry_owner = Uuid::new_v4();
        state.geometry_owner = Some(geometry_owner);
        state.color_owners[SATURATION] = Some(Uuid::new_v4());
        state.apply_layer_style(&style, [true, false, false, false]);
        assert_eq!(state.live_color[SATURATION], 0.0);
        assert_eq!(state.anim.current, 0.35);
        assert_eq!(state.color_owners[SATURATION], None);
        assert_eq!(state.geometry_owner, Some(geometry_owner));

        let saturation_owner = Uuid::new_v4();
        state.color_owners[SATURATION] = Some(saturation_owner);
        style.opacity = 0.7;
        style.saturation = 1.0;
        state.apply_layer_style(&style, [false; 4]);
        assert_eq!(state.anim.current, 0.7);
        assert_eq!(state.live_color[SATURATION], 0.0);
        assert_eq!(state.color_owners[SATURATION], Some(saturation_owner));
        style.picture_brightness = 0.125;
        style.contrast = 1.5;
        style.hue_degrees = 90.0;
        state.apply_layer_style(&style, [false, true, true, true]);
        assert_eq!(state.color_uniform(Instant::now()), [0.0, 0.125, 1.5, 0.25]);
        assert_eq!(state.anim.current, 0.7);
        assert_eq!(state.color_owners[SATURATION], Some(saturation_owner));
        state.anim.set(0.0);
        state.preloaded = true;
        style.opacity = 0.4;
        state.apply_layer_style(&style, [false; 4]);
        assert_eq!(state.anim.current, 0.0);
        assert_eq!(state.base_opacity, 0.4);
    }

    #[test]
    fn hwdec_failure_detected_from_the_real_mpv_log_lines() {
        // Verbatim from issue #5 (Windows 11, libmpv, H.264 in MP4).
        assert!(reports_hwdec_failure(
            "h264: Failed setup for format d3d11: hwaccel initialisation returned error."
        ));
        // Linux/macOS wording of the same failure.
        assert!(reports_hwdec_failure(
            "hevc: Failed setup for format vaapi: hwaccel initialisation returned error."
        ));
    }

    #[test]
    fn ordinary_mpv_warnings_do_not_trigger_the_software_fallback() {
        assert!(!reports_hwdec_failure(
            "mov,mp4,m4a,3gp,3g2,mj2: Detected creation time before 1970, parsing as unix timestamp."
        ));
        assert!(!reports_hwdec_failure("h264: no frame!"));
        assert!(!reports_hwdec_failure("Using hardware decoding (d3d11va-copy)."));
    }

    #[test]
    fn hwdec_mode_falls_back_to_software_once_latched() {
        assert_eq!(resolve_hwdec_mode(None, false), "auto-copy");
        assert_eq!(
            resolve_hwdec_mode(None, true),
            "no",
            "a latched failure disables hwdec for new slots",
        );
    }

    #[test]
    fn an_operator_pin_outranks_the_automatic_fallback() {
        // INKUE_HWDEC is set on purpose (bug repro, or a known-bad GPU path):
        // the automatic fallback must not silently undo it.
        assert_eq!(resolve_hwdec_mode(Some("d3d11va-copy"), true), "d3d11va-copy");
        assert_eq!(resolve_hwdec_mode(Some("no"), false), "no");
    }

    #[test]
    fn layer_key_explicit_bands_order_below_automatic() {
        let explicit_low = resolve_layer_key(Some(1), 100);
        let explicit_high = resolve_layer_key(Some(1000), 1);
        let auto_old = resolve_layer_key(None, 2);
        let auto_new = resolve_layer_key(None, 3);
        assert!(explicit_low < explicit_high);
        assert!(explicit_high < auto_old, "automatic stacks above every explicit layer");
        assert!(auto_old < auto_new, "newer automatic content stacks on top");
    }

    #[test]
    fn layer_key_same_band_ordered_by_sequence() {
        assert!(resolve_layer_key(Some(500), 1) < resolve_layer_key(Some(500), 2));
    }

    #[test]
    fn layer_key_clamps_out_of_range_layers() {
        assert!(resolve_layer_key(Some(0), 7) < resolve_layer_key(Some(1), 7));
        assert_eq!(resolve_layer_key(Some(5000), 7), resolve_layer_key(Some(1000), 7));
    }

    #[test]
    fn opacity_anim_snaps_and_animates() {
        let mut anim = OpacityAnim::resting(0.0);
        assert!(!anim.is_animating());
        anim.set(0.7);
        assert_eq!(anim.current, 0.7);
        anim.animate_to(0.0, 200);
        assert!(anim.is_animating());
        // Zero-duration animation completes on the first tick.
        anim.animate_to(1.0, 0);
        let (v, done) = anim.tick();
        assert_eq!(v, 1.0);
        assert!(done);
        assert!(!anim.is_animating());
    }

    #[test]
    fn opacity_anim_clamps_targets() {
        let mut anim = OpacityAnim::resting(0.5);
        anim.animate_to(7.0, 0);
        let (v, _) = anim.tick();
        assert_eq!(v, 1.0);
    }

    #[test]
    fn resting_anim_never_reports_completed() {
        // begin_stop(fade=0) parks the animation at 0 already resting.  tick()
        // then never reports "just completed", so tick_slot's unload decision
        // must not require it — requiring it left the slot occupied and the
        // capture device open forever (camera relaunch failed with
        // "dshow: device already in use").
        let mut anim = OpacityAnim::resting(1.0);
        anim.set(0.0);
        let (v, completed) = anim.tick();
        assert_eq!(v, 0.0);
        assert!(!completed);
        assert!(!anim.is_animating());
    }
}

    #[test]
    fn superseded_rate_requests_coalesce_and_a_stopped_slot_waits_for_its_worker() {
        let mut state = SlotState::idle();
        state.voice_id = Some(Uuid::new_v4());
        state.first_frame_ready = true;
        let mut speed = SilentSpeed::pending("movie.mp4".into());
        speed.eligible = Some(true);
        speed.requested = Some(1.25);
        state.silent_speed = Some(speed);
        state.silent_speed.as_mut().unwrap().requested = Some(1.75);

        let (generation, update) = state.take_silent_work().unwrap();
        assert!(matches!(update, SilentWork::SetRate(1.75)));
        assert!(!state.silent_worker_drained());

        state.generation += 1;
        state.voice_id = None;
        assert!(state.defer_silent_stop());
        assert!(state.take_silent_work().is_none());
        state.finish_silent_work(generation, update, true);
        assert!(state.silent_speed.is_none());
        assert!(!state.silent_worker_drained());
        let (generation, stop) = state.take_silent_work().unwrap();
        assert!(matches!(stop, SilentWork::Stop));
        state.finish_silent_work(generation, stop, true);
        assert!(!state.silent_worker_drained(), "Stop's return does not fence queued native events");
        state.silent_stop_queue_empty(false);
        assert!(!state.silent_worker_drained(), "the decoder still owns the retired file");
        state.silent_stop_queue_empty(true);
        assert!(!state.silent_worker_drained(), "the queue must drain after idle is confirmed");
        state.silent_stop_queue_empty(false);
        assert!(state.silent_worker_drained());
        state.silent_speed = Some(SilentSpeed::pending("next.mp4".into()));
        assert_eq!(state.silent_speed.unwrap().rate, 1.0);
    }

    #[test]
    fn a_late_track_check_cannot_approve_another_playback() {
        let mut state = SlotState::idle();
        state.voice_id = Some(Uuid::new_v4());
        state.first_frame_ready = true;
        let mut speed = SilentSpeed::pending("movie.mp4".into());
        speed.file_loaded = true;
        state.silent_speed = Some(speed);
        let (generation, check) = state.take_silent_work().unwrap();
        assert!(matches!(&check, SilentWork::Check(path) if path == "movie.mp4"));
        state.generation += 1;
        state.silent_speed = Some(SilentSpeed::pending("next.mp4".into()));
        state.finish_silent_work(generation, check, true);
        assert!(state.silent_speed.unwrap().eligible.is_none());
    }

