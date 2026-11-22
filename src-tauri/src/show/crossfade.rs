//! What a Fade Cue drives — and the crossfade it may dissolve its targets into.
//!
//! The transport calls [`on_fade_go`] right after it GOes a Fade Cue; the event
//! loop calls [`advance_pending`] every tick to finish the crossfades that had
//! to wait — for the Fade's own pre-wait, or for the incoming cue to show a
//! picture (its pre-wait, a camera that is still opening).
//!
//! A **plain fade** gets its targets' voices: audio voices with their current
//! gain and pan, visual layers with their current opacity.
//!
//! A **crossfade** (`crossfade_into`) starts the incoming visual cue and, once
//! it has a picture, hands the output engine the targets' layers to dissolve
//! away (`engine/output_engine/crossfade.rs`).  The incoming sound starts silent
//! and rises with the dissolve; the outgoing targets' sound falls to silence;
//! the Fade stops them when it lands.  Targets with no picture (an audio cue)
//! are faded the plain way, on the same clock.  When there is nothing to
//! dissolve into — the cue is missing, not visual, already on screen, or fails
//! to start — the Fade falls back to a plain fade of all its targets.

use crate::cue::context::CueContext;
use crate::cue::fade_cue::{FadeCue, PictureFadeRevertResult};
use crate::cue::traits::Cue;
use crate::cue::types::{CrossfadeLinked, CueId, CueType, EffectDestination, FadeAction, FadeCrossfade};
use crate::engine::output_engine::CrossfadeRequest;
use crate::engine::ring_command::VoiceId;
use crate::engine::OutputEngineApi;

use super::cue_list::CueList;

/// Restore the latest completed picture Fade on its original live playback.
pub fn revert_picture_fade(
    list: &mut CueList,
    output: &dyn OutputEngineApi,
    fade_id: CueId,
) -> anyhow::Result<PictureFadeRevertResult> {
    let fade = list.get_recursive(&fade_id)
        .and_then(|cue| (cue as &dyn std::any::Any).downcast_ref::<FadeCue>())
        .ok_or_else(|| anyhow::anyhow!("Choose a picture Fade in this cue list."))?;
    let live_voice = fade.completed_picture_target()
        .and_then(|target| list.cue_with_arming(&target))
        .filter(|(cue, armed)| *armed && (cue.is_running() || cue.is_paused()))
        .and_then(|(cue, _)| cue.playing_voice_id());
    let fade = list.get_mut_recursive(&fade_id)
        .and_then(|cue| (cue as &mut dyn std::any::Any).downcast_mut::<FadeCue>())
        .ok_or_else(|| anyhow::anyhow!("Choose a picture Fade in this cue list."))?;
    fade.revert_completed_picture(live_voice, output)
}

/// Validate the exact programmed destination and playback selected in the desk.
pub fn resolve_picture_saturation_preview(
    list: &CueList, fade_id: CueId, target_id: CueId, voice_id: VoiceId, saturation: f64,
) -> anyhow::Result<f32> {
    let fade = list.get_recursive(&fade_id)
        .and_then(|cue| (cue as &dyn std::any::Any).downcast_ref::<FadeCue>())
        .ok_or_else(|| anyhow::anyhow!("Choose a picture Fade in this cue list."))?;
    anyhow::ensure!(!fade.is_running() && !fade.is_paused(), "Wait for the picture Fade to finish.");
    let spec = fade.fade_specification().ok_or_else(|| anyhow::anyhow!("Choose a picture Fade."))?;
    anyhow::ensure!(spec.target_cue_ids.as_slice() == [target_id] && spec.crossfade_into.is_none()
        && !spec.has_silent_speed_goal && spec.effect_destination.is_none() && spec.target_map_motion.is_none(),
        "Choose one picture target for the saturation Fade.");
    anyhow::ensure!((0.0..=2.0).contains(&saturation) && fade.target_saturation == Some(saturation),
        "The Fade's destination changed. Check the saved saturation and try again.");
    anyhow::ensure!(list.cue_with_arming(&target_id).is_some_and(|(target, armed)| {
        armed && supports_geometry_fade(target) && target.is_running() && target.playing_voice_id() == Some(voice_id)
    }), "Start the armed picture target before previewing its saturation.");
    Ok(saturation as f32)
}

/// Set up what the Fade Cue `fade_id` drives, right after its GO.  Returns the
/// cues it started (a crossfade's incoming cue), for the caller to report as
/// triggered.
pub fn on_fade_go(cue_list: &mut CueList, ctx: &CueContext, fade_id: CueId) -> Vec<CueId> {
    let Some(spec) = cue_list.get(&fade_id).and_then(|c| c.fade_specification()) else {
        return Vec::new();
    };
    if spec.has_silent_speed_goal {
        bind_silent_speed(cue_list, ctx, fade_id);
        return Vec::new();
    }
    if spec.target_map_motion.is_some() {
        bind_map_motion(cue_list, ctx, fade_id);
        return Vec::new();
    }
    if spec.effect_destination.is_some() {
        bind_effect_target(cue_list, ctx, fade_id);
        return Vec::new();
    }
    match spec.crossfade_into.filter(|&into| into != fade_id) {
        None => {
            if !spec.has_picture_goal {
                set_plain_fade(cue_list, ctx, fade_id, &spec, &[]);
            }
            bind_rate_targets(cue_list, ctx, fade_id);
            bind_geometry_target(cue_list, ctx, fade_id);
            Vec::new()
        }
        Some(incoming_cue) => {
            if let Some(fade) = cue_list.get_mut(&fade_id) {
                fade.set_crossfade(FadeCrossfade::Pending { incoming_cue, incoming_started: false });
            }
            advance(cue_list, ctx, fade_id)
        }
    }
}

/// Push every running Fade Cue whose crossfade is still being set up one step
/// further.  Returns the cues started on the way.
pub fn advance_pending(cue_list: &mut CueList, ctx: &CueContext) -> Vec<CueId> {
    let pending: Vec<CueId> = cue_list
        .cues
        .iter()
        .filter(|c| c.is_running() && c.pending_crossfade().is_some())
        .map(|c| c.id())
        .collect();
    let started = pending.into_iter().flat_map(|fade_id| advance(cue_list, ctx, fade_id)).collect();
    let mut target_fades = Vec::new();
    collect_pending_target_fades(&cue_list.cues, &mut target_fades);
    for fade_id in target_fades {
        bind_rate_targets(cue_list, ctx, fade_id);
        bind_geometry_target(cue_list, ctx, fade_id);
        bind_map_motion(cue_list, ctx, fade_id);
        bind_effect_target(cue_list, ctx, fade_id);
        bind_silent_speed(cue_list, ctx, fade_id);
    }
    started
}

fn collect_pending_target_fades(cues: &[Box<dyn Cue>], pending: &mut Vec<CueId>) {
    for cue in cues.iter().filter(|cue| cue.is_running()) {
        if cue.rate_fade_targets_pending() || cue.geometry_target_pending().is_some()
            || cue.map_motion_pending().is_some()
            || cue.effect_target_pending().is_some()
            || cue.silent_speed_target_pending().is_some()
        {
            pending.push(cue.id());
        }
        if let Some(children) = cue.child_cues() {
            collect_pending_target_fades(children, pending);
        }
    }
}

fn bind_silent_speed(cue_list: &mut CueList, ctx: &CueContext, fade_id: CueId) {
    let Some(target_id) = cue_list.get_recursive(&fade_id).and_then(|fade| fade.silent_speed_target_pending()) else {
        return;
    };
    let voice = cue_list.cue_with_arming(&target_id)
        .filter(|(target, armed)| *armed && target.supports_silent_speed()
            && (target.is_running() || target.is_paused()))
        .and_then(|(target, _)| target.playing_voice_id());
    if let Some(fade) = cue_list.get_mut_recursive(&fade_id) {
        fade.set_silent_speed_voice(voice, ctx);
    }
}

/// One step of setting up a crossfade: start the incoming cue, wait for its
/// picture, link the dissolve — or give up and fade the plain way.
fn advance(cue_list: &mut CueList, ctx: &CueContext, fade_id: CueId) -> Vec<CueId> {
    let Some(fade) = cue_list.get(&fade_id) else { return Vec::new() };
    // The Fade's own pre-wait comes first.
    if !fade.is_action_started() {
        return Vec::new();
    }
    let (Some((incoming_cue, incoming_started)), Some(spec)) =
        (fade.pending_crossfade(), fade.fade_specification())
    else {
        return Vec::new();
    };

    let mut started = Vec::new();
    match incoming_state(cue_list, incoming_cue) {
        Incoming::Unusable => return give_up(cue_list, ctx, fade_id, &spec),
        // Already on screen before this Fade: there is no entrance to dissolve.
        Incoming::Running if !incoming_started => return give_up(cue_list, ctx, fade_id, &spec),
        Incoming::Running => {}
        Incoming::Idle if incoming_started => {
            // It stopped (or failed) while this Fade was waiting for it.
            return give_up(cue_list, ctx, fade_id, &spec);
        }
        Incoming::Idle => {
            let Some(incoming) = cue_list.get_mut_recursive(&incoming_cue) else {
                return give_up(cue_list, ctx, fade_id, &spec);
            };
            let launched = incoming.go(ctx).is_ok() && incoming.is_running();
            if !launched {
                log::warn!("[crossfade] cue {incoming_cue} did not start — plain fade instead");
                return give_up(cue_list, ctx, fade_id, &spec);
            }
            started.push(incoming_cue);
            if let Some(fade) = cue_list.get_mut(&fade_id) {
                fade.set_crossfade(FadeCrossfade::Pending { incoming_cue, incoming_started: true });
            }
        }
    }

    // No picture yet (its pre-wait): try again on the next tick.
    let Some(incoming_voice) = cue_list.get_recursive(&incoming_cue).and_then(|c| c.playing_voice_id()) else {
        return started;
    };
    if !link(cue_list, ctx, fade_id, &spec, incoming_cue, incoming_voice) {
        give_up(cue_list, ctx, fade_id, &spec);
    }
    started
}

enum Incoming {
    /// Missing, or not a picture to dissolve into.
    Unusable,
    Idle,
    Running,
}

fn incoming_state(cue_list: &CueList, incoming_cue: CueId) -> Incoming {
    match cue_list.cue_with_arming(&incoming_cue) {
        Some((cue, _)) if !cue.is_visual() => Incoming::Unusable,
        Some((cue, _)) if cue.is_running() || cue.is_paused() => Incoming::Running,
        Some((_, true)) => Incoming::Idle,
        _ => Incoming::Unusable,
    }
}

/// Hand the dissolve to the output engine and the rest of the targets to the
/// Fade.  `false` when the engine has no picture for the incoming voice.
fn link(
    cue_list: &mut CueList,
    ctx: &CueContext,
    fade_id: CueId,
    spec: &FadeAction,
    incoming_cue: CueId,
    incoming_voice: VoiceId,
) -> bool {
    let mut dissolved: Vec<CueId> = Vec::new();
    let mut outgoing_voices: Vec<VoiceId> = Vec::new();
    let mut outgoing_audio: Vec<(VoiceId, f32)> = Vec::new();
    for &target_id in spec.target_cue_ids.iter().filter(|&&t| t != incoming_cue) {
        let Some(target) = cue_list.get_recursive(&target_id) else { continue };
        let pictures = target.visual_voice_ids();
        if pictures.is_empty() {
            continue;
        }
        dissolved.push(target_id);
        for voice in audio_voices_of(target, ctx, &pictures) {
            outgoing_audio.push((voice, ctx.audio_engine.get_voice_gain(voice)));
        }
        outgoing_voices.extend(pictures);
    }

    let request = CrossfadeRequest {
        incoming: incoming_voice,
        outgoing: outgoing_voices,
        duration_ms: u32::try_from(spec.duration_ms).unwrap_or(u32::MAX),
        up: spec.up_curve,
        down: spec.down_curve,
    };
    if !ctx.output_engine.link_crossfade(&request) {
        return false;
    }

    let incoming_audio = silence_incoming_sound(ctx, incoming_voice);
    let mut not_plain = dissolved.clone();
    not_plain.push(incoming_cue);
    set_plain_fade(cue_list, ctx, fade_id, spec, &not_plain);
    if let Some(fade) = cue_list.get_mut(&fade_id) {
        fade.set_crossfade(FadeCrossfade::Linked(CrossfadeLinked {
            incoming_cue,
            incoming_voice,
            incoming_audio,
            outgoing_audio,
            targets: dissolved,
        }));
    }
    true
}

/// Nothing to dissolve into: every target is faded the plain way, from now —
/// except the cue the crossfade would have brought in, never faded by it.
fn give_up(cue_list: &mut CueList, ctx: &CueContext, fade_id: CueId, spec: &FadeAction) -> Vec<CueId> {
    let incoming: Vec<CueId> = spec.crossfade_into.into_iter().collect();
    set_plain_fade(cue_list, ctx, fade_id, spec, &incoming);
    if let Some(fade) = cue_list.get_mut(&fade_id) {
        fade.set_crossfade(FadeCrossfade::None);
    }
    Vec::new()
}

/// Hand the Fade the voices of its targets (all but `skip`): audio voices with
/// their current gain and pan, visual layers with their current opacity.
fn set_plain_fade(cue_list: &mut CueList, ctx: &CueContext, fade_id: CueId, spec: &FadeAction, skip: &[CueId]) {
    let mut audio: Vec<(VoiceId, f32, f32)> = Vec::new();
    let mut visual: Vec<(VoiceId, f32)> = Vec::new();
    for target_id in spec.target_cue_ids.iter().filter(|t| !skip.contains(t)) {
        // Recursive lookup so a Fade can target a cue nested in a group.
        let Some((target, _)) = cue_list.cue_with_arming(target_id) else { continue };
        let pictures = target.visual_voice_ids();
        for voice in audio_voices_of(target, ctx, &pictures) {
            let gain = ctx.audio_engine.get_voice_gain(voice);
            let pan = ctx.audio_engine.get_voice_pan(voice);
            audio.push((voice, gain, pan));
        }
        for voice in pictures {
            visual.push((voice, ctx.output_engine.get_voice_opacity(voice)));
        }
    }

    // Brightness % → layer opacity; without an explicit visual target the
    // audio gain doubles as the opacity target (legacy semantics).
    let visual_target_opacity = spec
        .target_visual_alpha
        .map(|alpha| 1.0 - alpha as f32 / 255.0)
        .unwrap_or_else(|| spec.target_gain_linear.clamp(0.0, 1.0));

    if let Some(fade) = cue_list.get_mut(&fade_id) {
        fade.set_fade_voices(audio, visual, visual_target_opacity);
    }
}

fn supports_geometry_fade(cue: &dyn Cue) -> bool {
    matches!(cue.cue_type(), CueType::Video | CueType::Image | CueType::Camera)
}

fn bind_geometry_target(cue_list: &mut CueList, ctx: &CueContext, fade_id: CueId) {
    let Some((target_id, needs_still_rotation)) = cue_list.get_recursive(&fade_id)
        .and_then(|fade| fade.geometry_target_pending()) else {
        return;
    };
    let voice = cue_list.cue_with_arming(&target_id)
        .filter(|(target, armed)| {
            *armed && supports_geometry_fade(*target) && (target.is_running() || target.is_paused())
                && (!needs_still_rotation || target.is_still_rotation_target())
        })
        .and_then(|(target, _)| target.playing_voice_id());
    if let Some(fade) = cue_list.get_mut_recursive(&fade_id) {
        fade.set_geometry_voice(voice, ctx);
    }
}

fn bind_map_motion(cue_list: &mut CueList, ctx: &CueContext, fade_id: CueId) {
    let Some((target, motion)) = cue_list.get_recursive(&fade_id).and_then(|fade| fade.map_motion_pending()) else {
        return;
    };
    let result = cue_list.cue_with_arming(&target)
        .filter(|(cue, armed)| *armed && cue.cue_type() == CueType::Audio && (cue.is_running() || cue.is_paused()))
        .ok_or_else(|| "Start the armed target Audio cue before calling its movement.".to_owned())
        .and_then(|(cue, _)| crate::cue::point_mix::MapMotionRun::bind(motion, cue, ctx));
    let alert_id = format!("map-motion-{fade_id}");
    let run = match result {
        Ok(run) => { crate::health::clear(&alert_id); Some(run) }
        Err(error) => {
            crate::health::set(crate::health::HealthAlert::new(
                &alert_id, crate::health::HealthLevel::Warning, error,
            ));
            None
        }
    };
    if let Some(fade) = cue_list.get_mut_recursive(&fade_id) {
        fade.set_map_motion_run(run);
    }
}

fn bind_rate_targets(cue_list: &mut CueList, ctx: &CueContext, fade_id: CueId) {
    let Some(spec) = cue_list.get_recursive(&fade_id)
        .filter(|fade| fade.rate_fade_targets_pending())
        .and_then(|fade| fade.fade_specification())
    else {
        return;
    };
    let mut voices = std::collections::HashSet::new();
    // Rate also applies to Audio children of a dissolved Group. Never alter
    // the independently clocked soundtrack of a Video cue.
    for target_id in spec.target_cue_ids.iter().filter(|&&id| Some(id) != spec.crossfade_into) {
        if let Some(target) = cue_list.get_recursive(target_id) {
            if !spec.has_picture_goal {
                collect_rate_voices(target, &mut voices);
            }
        }
    }
    let faders = voices.into_iter().filter_map(|voice| ctx.audio_engine.voice_rate_fader(voice)).collect();
    if let Some(fade) = cue_list.get_mut_recursive(&fade_id) {
        fade.set_rate_fade_targets(faders);
    }
}

fn bind_effect_target(cue_list: &mut CueList, ctx: &CueContext, fade_id: CueId) {
    let Some((target_id, destination)) = cue_list.get_recursive(&fade_id)
        .and_then(|cue| cue.effect_target_pending())
    else {
        return;
    };
    let fader = cue_list.cue_with_arming(&target_id)
        .filter(|(cue, armed)| *armed && (cue.is_running() || cue.is_paused())
            && cue.accepts_effect_destination(destination))
        .and_then(|(cue, _)| cue.playing_voice_id())
        .and_then(|voice| match destination {
            EffectDestination::Cutoff(filter) => ctx.audio_engine.voice_cutoff_fader(voice, filter.filter_id),
            EffectDestination::EqGain(eq) => ctx.audio_engine.voice_eq_gain_fader(voice, eq.filter_id),
        });
    if let Some(fade) = cue_list.get_mut_recursive(&fade_id) {
        fade.set_effect_fader(fader, ctx);
    }
}

fn collect_rate_voices(target: &dyn Cue, voices: &mut std::collections::HashSet<VoiceId>) {
    if target.cue_type() == CueType::Audio {
        voices.extend(target.playing_voice_id());
    }
    if let Some(children) = target.child_cues() {
        for child in children {
            collect_rate_voices(child.as_ref(), voices);
        }
    }
}

/// The audio voices a target owns: its own (recursively, for a group) and the
/// sound track of each of its videos.  `pictures` are its visual voices, which
/// `all_voice_ids` also reports but which carry no sound themselves.
fn audio_voices_of(target: &dyn crate::cue::traits::Cue, ctx: &CueContext, pictures: &[VoiceId]) -> Vec<VoiceId> {
    let mut voices: Vec<VoiceId> =
        target.all_voice_ids().into_iter().filter(|v| !pictures.contains(v)).collect();
    voices.extend(pictures.iter().filter_map(|&picture| ctx.output_engine.video_audio_voice(picture)));
    voices
}

/// Mute the incoming video's sound track and return it with the level the
/// fade must bring it back to.  Images and cameras have none.
fn silence_incoming_sound(ctx: &CueContext, incoming_voice: VoiceId) -> Vec<(VoiceId, f32)> {
    let Some(audio) = ctx.output_engine.video_audio_voice(incoming_voice) else {
        return Vec::new();
    };
    let level = ctx.audio_engine.get_voice_gain(audio);
    let _ = ctx.audio_engine.set_voice_gain(audio, 0.0);
    vec![(audio, level)]
}
