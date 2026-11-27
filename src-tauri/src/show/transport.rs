//! Show transport — GO, STOP, PAUSE, and continue mode chaining.
//!
//! The [`Transport`] struct is the primary interface between the UI commands
//! and the cue execution system.  It holds a reference to the active
//! [`CueList`] and a [`CueContext`] so it can drive cue lifecycle methods.

use std::any::Any;
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::cue::{
    context::CueContext,
    audio_cue::AudioCue,
    control_cue::{ControlAction, ControlCue},
    devamp_cue::{DevampCue, DevampScope},
    group_cue::GroupCue,
    traits::Cue,
    types::{ContinueMode, CueId, CueState, CueType, GroupMode},
    video_cue::VideoCue,
    wait_cue::WaitCue,
};

use super::{cue_list::{CueList, CueListMode}, timecode_loss::ClockCue};
use crate::engine::timecode_types::TcOnStop;

/// Hard cap on how many cues a single GO call will chain through via
/// Auto-Continue / Auto-Follow before giving up. Guards against a cyclic
/// chain (e.g. Goto routing back into itself) spinning forever.
const MAX_CHAIN_STEPS: u32 = 1000;

pub(crate) fn stage_media_load_time(cue: &mut dyn Cue, offset_ms: i64) -> Result<()> {
    let cue = cue as &mut dyn Any;
    if let Some(audio) = cue.downcast_mut::<AudioCue>() {
        audio.load_to_time(offset_ms)
    } else if let Some(video) = cue.downcast_mut::<VideoCue>() {
        video.load_to_time(offset_ms)
    } else {
        Err(anyhow!("Select one top-level File Audio or Video cue for a Load Time."))
    }
}

/// Result returned by [`Transport::go`].
#[derive(Default)]
pub struct GoResult {
    /// IDs of all cues triggered by this GO (including chained Auto-Continue /
    /// Auto-Follow cues).  The first element is always the primary cue.
    pub triggered: Vec<CueId>,
    /// IDs of cues stopped by a Stop Cue's action during this GO.
    pub stopped: Vec<CueId>,
    /// A Load cue prepared a target whose visible state or duration changed.
    pub prepared: bool,
}

/// Manages playback state for a single [`CueList`].
pub struct Transport {
    pub context: CueContext,
}

impl Transport {
    /// Create a new transport bound to the given context.
    pub fn new(context: CueContext) -> Self {
        Self { context }
    }

    // -----------------------------------------------------------------------
    // GO
    // -----------------------------------------------------------------------

    /// Trigger the cue at the Playhead.
    ///
    /// Returns a [`GoResult`] containing:
    /// - `triggered`: IDs of **all** cues fired in this call (primary + chains).
    /// - `stopped`: IDs of cues stopped by a Stop Cue action.
    ///
    /// Sequence:
    /// 1. Read the cue at the Playhead.
    /// 2. Advance the Playhead to the next cue.
    /// 3. Stop any running cues with `stop_on_next_go()` (visual-only logic).
    /// 4. Call `cue.go()`.
    /// 5. Execute any stop action declared by `cue.stop_specification()` — this
    ///    runs **before** chain evaluation so Auto-Follow cannot start a cue that
    ///    the Stop Cue would immediately kill.
    /// 6. Chain via Auto-Continue (post_wait = 0) or instant Auto-Follow.
    pub fn go(&mut self, cue_list: &mut CueList) -> Result<GoResult> {
        self.go_in_list(cue_list, true)
    }

    pub(super) fn go_automatically(&mut self, cue_list: &mut CueList) -> Result<GoResult> {
        self.go_in_list(cue_list, false)
    }

    fn go_in_list(&mut self, cue_list: &mut CueList, explicit_go: bool) -> Result<GoResult> {
        if cue_list.independent_pads() {
            return Ok(GoResult::default());
        }
        let mut result = GoResult::default();

        // Each loop iteration processes one cue. Auto-Continue (post_wait = 0)
        // and Auto-Follow-on-an-instant-cue chain straight into the next
        // iteration instead of returning, so a cycle — e.g. a Goto cue that
        // routes an Auto-Continue chain back into itself — no longer recurses
        // the call stack without limit. MAX_CHAIN_STEPS bounds it outright:
        // a real show chain is at most a few dozen cues long, so this is
        // generous headroom before it's unambiguously a cycle.
        for _ in 0..MAX_CHAIN_STEPS {
            let cue_id = match cue_list.playhead_cue_id {
                Some(id) => id,
                None => return Ok(result),
            };

            // If the cue at the playhead is disabled (e.g., toggled while the
            // playhead was parked on it), advance past it and retry.
            if cue_list.get(&cue_id).is_some_and(|c| c.is_disabled()) {
                cue_list.cancel_enter_follow();
                cue_list.advance_playhead();
                continue;
            }

            let cue = cue_list.get(&cue_id).ok_or_else(|| anyhow!("Cue not found: {cue_id}"))?;
            let allow_enter_follow = explicit_go && result.triggered.is_empty()
                && cue_list.mode == CueListMode::Sequential
                && cue.group_mode() == Some(GroupMode::StartFirstAndEnter);
            if let Some(group) = (cue as &dyn Any).downcast_ref::<GroupCue>() {
                group.check_enter_boundary(allow_enter_follow)?;
            }
            let successor = allow_enter_follow.then(|| cue_list.next_armed_after(cue_id)).flatten();
            let replacement = cue_list.target_destination(cue)?;

            // If the cue wants to absorb this GO (e.g., a Sequential Group paused
            // mid-sequence), delegate to the cue and skip outer Playhead advancement.
            if cue_list.get(&cue_id).is_some_and(|c| c.absorbs_go()) {
                let (released, boundary_wait) = if let Some(cue) = cue_list.get_mut(&cue_id) {
                    let release_owned = cue.group_mode() == Some(GroupMode::StartFirstAndEnter)
                        || cue.playlist_advance_playhead() == Some(true);
                    if release_owned {
                        cue.mark_playhead_held();
                    }
                    let boundary_wait = self.start_action(cue, allow_enter_follow)?;
                    ((release_owned && cue.take_playhead_release()) || cue.released_playhead(), boundary_wait)
                } else {
                    (false, None)
                };
                // Pre-Wait or pending output can absorb a GO without starting
                // anything. Keep the original handoff in that case.
                if boundary_wait.is_some() {
                    cue_list.arm_enter_follow(cue_id, boundary_wait, successor);
                }
                // If that GO fired the group's last child, release the outer Playhead
                // to the cue after the group so the next GO continues the outer list.
                result.triggered.push(cue_id);
                if released {
                    cue_list.advance_playhead();
                    if cue_list.take_enter_follow(cue_id).is_some() {
                        continue;
                    }
                }
                return Ok(result);
            }

            // Advance playhead before triggering (matches QLab behaviour).
            cue_list.advance_playhead();

            // Stop any running cues that should automatically stop on the next GO
            // (only the Text Cue opts in — visual cues are layers and never
            // auto-stop each other).  If a visual type ever opts back in, it only
            // stops when the incoming cue is also visual.
            if let Some(incoming) = cue_list.get(&cue_id).filter(|cue| cue.cue_type() != CueType::Load) {
                let incoming_is_visual = incoming.is_visual();
                for cue in &mut cue_list.cues {
                    if cue.is_running() && cue.id() != cue_id && cue.stop_on_next_go()
                        && (!cue.is_visual() || incoming_is_visual)
                    {
                        let _ = cue.stop(&self.context);
                    }
                }
            }

            let boundary_wait = match self.fire_cue(cue_list, cue_id, replacement, allow_enter_follow, &mut result) {
                Ok(wait) => wait,
                Err(error) => {
                    // A rejected cue stays in Standby for the next manual GO.
                    if cue_list.get(&cue_id).is_some_and(|cue| cue.state() == CueState::Standby || cue.holds_playhead()) {
                        cue_list.playhead_cue_id = Some(cue_id);
                    } else {
                        cue_list.cancel_enter_follow();
                    }
                    return Err(error);
                }
            };
            cue_list.arm_enter_follow(cue_id, boundary_wait, successor);

            // Read continue-mode metadata after go() (state may have changed for
            // instant cues that complete synchronously).
            let (continue_mode, post_wait, is_still_running, holds_playhead, defer_immediate_continue) = cue_list
                .cues
                .iter()
                .find(|c| c.id() == cue_id)
                .map(|c| (c.continue_mode(), c.post_wait(), c.state() == CueState::Running,
                    c.holds_playhead(), c.uses_action_clock_for_continue()
                        && (!c.is_action_started() || c.is_auto_continue_fired()
                            || (c.group_mode() == Some(GroupMode::StartFirstAndEnter) && c.holds_playhead()))))
                .ok_or_else(|| anyhow!("Cue not found after go: {:?}", cue_id))?;

            // Sequential groups retain the outer Playhead while running so that
            // subsequent GOs are routed into the group's internal sequence via
            // absorbs_go().  The advance_playhead() already moved the Playhead
            // forward; we move it back here.  The event loop will advance it again
            // once the group completes.
            let independent_run = cue_list.independent_runs.contains(&cue_id);
            if is_still_running && holds_playhead && !independent_run {
                cue_list.playhead_cue_id = Some(cue_id);
                if let Some(cue) = cue_list.get_mut(&cue_id) {
                    cue.mark_playhead_held();
                }
            }

            // Determine whether to chain immediately:
            //
            // • AutoContinue + post_wait = 0 → chain now unless this cue's
            //   action clock is waiting on Pre-Wait. Mark it to avoid a second GO.
            // • AutoFollow on an instant cue  → chain now (cue already completed).
            //   Audio AutoFollow is handled by the event loop on voice completion.
            let immediate_continue = continue_mode == ContinueMode::AutoContinue
                && post_wait.is_zero() && !defer_immediate_continue && !independent_run;
            let enter_follow = !holds_playhead && cue_list.take_enter_follow(cue_id).is_some();
            let chain_now = immediate_continue || enter_follow
                || (!independent_run && !is_still_running && continue_mode == ContinueMode::AutoFollow);

            if immediate_continue {
                if let Some(cue) = cue_list.get_mut(&cue_id) {
                    cue.mark_auto_continue_fired();
                }
            }

            if !chain_now {
                return Ok(result);
            }
            // else: loop around and process the next cue in this same GO call.
        }

        log::error!(
            "GO chain exceeded {MAX_CHAIN_STEPS} steps without stopping — likely an \
             Auto-Continue/Goto cycle in the show; breaking the chain here instead of \
             hanging or overflowing the stack."
        );
        Ok(result)
    }

    fn start_action(&self, cue: &mut dyn Cue, allow_enter_follow: bool) -> Result<Option<CueId>> {
        if allow_enter_follow {
            if let Some(group) = (cue as &mut dyn Any).downcast_mut::<GroupCue>() {
                return group.go_from_main_list(&self.context);
            }
        }
        cue.go(&self.context)?;
        Ok(None)
    }

    /// Execute one cue and its explicit target actions. List navigation and
    /// follow rules belong to the caller, so a cart pad can use the same effects.
    fn fire_cue(
        &self, cue_list: &mut CueList, cue_id: CueId, replacement: Option<CueId>,
        allow_enter_follow: bool, result: &mut GoResult,
    ) -> Result<Option<CueId>> {
        // A later manual start from standby is a new run. A GO on an
        // already-playing cue may be a no-op, so retain that run's origin.
        if cue_list.get(&cue_id).is_some_and(|cue| !cue.is_running() && !cue.is_paused()) {
            cue_list.independent_runs.remove(&cue_id);
        }
        let timed_load = self.fire_timed_load(cue_list, cue_id)?;
        result.prepared |= timed_load;
        let boundary_wait = if timed_load || self.fire_whole_cue_devamp(cue_list, cue_id)? {
            None
        } else {
            let cue = cue_list
                .get_mut(&cue_id)
                .ok_or_else(|| anyhow!("Cue not found: {:?}", cue_id))?;
            self.start_action(cue, allow_enter_follow)?
        };

        // Fade Cues: hand them their targets' voices — and start the cue a
        // crossfade dissolves into, which the UI must see as triggered.
        let crossfade_started = if cue_list.get(&cue_id).is_some_and(|c| c.fade_specification().is_some()) {
            super::crossfade::on_fade_go(cue_list, &self.context, cue_id)
        } else {
            Vec::new()
        };

        // Devamp Cues: release the current slice loop on every target voice.
        // The engines act at the next slice boundary, so ordering with
        // Auto-Follow below is not timing-sensitive.
        let devamp_spec = cue_list.get(&cue_id).and_then(|c| c.devamp_specification());
        if let Some((stop_at_end, target_ids)) = devamp_spec {
            // A boundary successor waits for the first target's voice to stop
            // at the released slice end before the next cue fires.
            let boundary_voice = cue_list.get(&cue_id)
                .and_then(|c| (c as &dyn Any).downcast_ref::<DevampCue>())
                .filter(|d| d.start_next_at_boundary && d.continue_mode() == ContinueMode::AutoFollow)
                .and_then(|_| target_ids.first().copied())
                .and_then(|tid| cue_list.get_recursive(&tid))
                .and_then(|t| t.playing_voice_id());
            for target_id in &target_ids {
                let Some(target) = cue_list.get_recursive(target_id) else { continue };
                if target.is_visual() {
                    // Video: devamp the mpv slice plan and the paired audio
                    // voice together so picture and sound release in step.
                    let Some(voice) = target.playing_voice_id() else { continue };
                    self.context.output_engine.devamp_voice(voice, stop_at_end);
                    if let Some(aid) = self.context.output_engine.video_audio_voice(voice) {
                        let _ = self.context.audio_engine.devamp_voice(aid, stop_at_end);
                    }
                } else {
                    // Audio / Group — devamp every audio voice the target owns.
                    for vid in target.all_voice_ids() {
                        let _ = self.context.audio_engine.devamp_voice(vid, stop_at_end);
                    }
                }
            }
            if let Some(voice) = boundary_voice {
                if let Some(devamp) = cue_list.get_mut(&cue_id)
                    .and_then(|c| (c as &mut dyn Any).downcast_mut::<DevampCue>())
                {
                    devamp.set_boundary_voice(voice);
                }
            }
        }

        // Execute the stop action declared by Stop Cues **before** evaluating
        // Auto-Follow, so the chained cue is not immediately killed.
        let stop_spec = cue_list.get(&cue_id).and_then(|c| c.stop_specification());
        if let Some((hard, target_ids)) = stop_spec {
            let ids_to_stop: Vec<CueId> = if target_ids.is_empty() {
                // Empty = stop all running/paused cues (except the Stop Cue itself).
                cue_list
                    .cues
                    .iter()
                    .filter(|c| (c.is_running() || c.is_paused() || c.loaded_time_ms().is_some()) && c.id() != cue_id)
                    .map(|c| c.id())
                    .collect()
            } else {
                target_ids.iter().copied()
                    .filter(|&tid| tid != cue_id)
                    .collect()
            };
            for id in &ids_to_stop {
                cue_list.cancel_enter_follow_for(*id);
                if let Some(c) = cue_list.get_mut_recursive(id) {
                    if hard {
                        let _ = c.hard_stop(&self.context);
                    } else {
                        let _ = c.stop(&self.context);
                    }
                }
            }
            result.stopped.extend(ids_to_stop);
        }

        // Command cues (Start, Pause, Resume, Load, Reset, Goto, Arm, Disarm)
        // act on other cues, so the transport — which owns the cue list and the
        // playhead — carries them out. Like the stop action above, this runs
        // before Auto-Follow is evaluated.
        let control_spec = if timed_load { None } else { cue_list.get(&cue_id).and_then(|c| c.control_specification()) };
        let mut commanded: Vec<CueId> = Vec::new();
        if let Some((action, target_ids)) = control_spec {
            // Never let a Command cue address itself: a Start targeting itself
            // would recurse, and the rest are meaningless on an instant cue.
            for target_id in target_ids.iter().copied().filter(|&t| t != cue_id) {
                if action == ControlAction::Goto {
                    // The Playhead is a position in *this* list, so a nested
                    // cue cannot hold it.
                    if !cue_list.independent_pads() && cue_list.index_of(&target_id).is_some() {
                        cue_list.set_playhead(Some(target_id))?;
                    }
                    continue;
                }
                if action == ControlAction::Start && !cue_list.cue_with_arming(&target_id).is_some_and(|(_, armed)| armed) {
                    continue;
                }
                if matches!(action, ControlAction::Reset | ControlAction::Disarm) {
                    cue_list.cancel_enter_follow_for(target_id);
                }
                let Some(target) = cue_list.get_mut_recursive(&target_id) else { continue };
                match action {
                    ControlAction::Start => {
                        if target.go(&self.context).is_err() {
                            continue;
                        }
                        cue_list.cancel_enter_follow_for(target_id);
                    }
                    ControlAction::Pause => {
                        let _ = target.pause(&self.context);
                    }
                    ControlAction::Resume => {
                        let _ = target.resume(&self.context);
                    }
                    ControlAction::Load => {
                        let alert_id = format!("load-{}", target.id());
                        let before = (target.state(), target.file_duration());
                        match target.preload(&self.context) {
                            Ok(()) => {
                                crate::health::clear(&alert_id);
                                result.prepared |= before != (target.state(), target.file_duration());
                            }
                            Err(err) => crate::health::set(crate::health::HealthAlert::new(
                                &alert_id, crate::health::HealthLevel::Warning,
                                format!("Could not load cue {} ({}): {err}", target.number().unwrap_or(""), target.name()),
                            )),
                        }
                        continue;
                    }
                    ControlAction::Target => target.set_temporary_target(replacement),
                    ControlAction::Reset => {
                        let _ = self.reset_cue_state(target);
                    }
                    ControlAction::Arm => target.set_disabled(false),
                    ControlAction::Disarm => target.set_disabled(true),
                    ControlAction::Goto => unreachable!("handled above"),
                }
                commanded.push(target_id);
            }
        }

        result.triggered.push(cue_id);
        result.triggered.extend(commanded);
        result.triggered.extend(crossfade_started);
        Ok(boundary_wait)
    }

    fn fire_timed_load(&self, list: &mut CueList, cue_id: CueId) -> Result<bool> {
        let Some(load) = list.get(&cue_id)
            .and_then(|cue| (cue as &dyn Any).downcast_ref::<ControlCue>())
            .filter(|load| load.has_load_time())
        else { return Ok(false) };
        let request = load.timed_load_target();
        let load = list.get_mut(&cue_id).ok_or_else(|| anyhow!("Timed Load cue no longer exists."))?;
        load.reset()?;
        let (target, offset) = request?;
        let target = list.get_mut(&target)
            .ok_or_else(|| anyhow!("Select one top-level File Audio or Video cue for a Load Time."))?;
        stage_media_load_time(target, offset)?;
        let load = list.get_mut(&cue_id)
            .and_then(|cue| (cue as &mut dyn Any).downcast_mut::<ControlCue>())
            .ok_or_else(|| anyhow!("Timed Load cue no longer exists."))?;
        load.complete(&self.context);
        Ok(true)
    }

    fn fire_whole_cue_devamp(&self, cue_list: &mut CueList, cue_id: CueId) -> Result<bool> {
        const TARGET_REQUIRED: &str = "Start an armed File Audio or looping Video cue before using Whole cue release.";
        const AUDIO_REQUIRED: &str = "Start an armed, unsliced File Audio loop before using Whole cue release.";
        const VIDEO_REQUIRED: &str = "Start an armed MP4 or MOV looping indefinitely, with no trim, slices, end fade, held frame or automatic continuation. Wait for the picture before using Whole cue release.";
        let Some(devamp) = cue_list.get(&cue_id)
            .and_then(|cue| (cue as &dyn Any).downcast_ref::<DevampCue>())
            .filter(|devamp| devamp.scope == DevampScope::WholeCue)
        else { return Ok(false); };

        let target_voice = devamp.whole_release_target().and_then(|target| {
            let (cue, _) = cue_list.cue_with_arming(&target)
                .filter(|(cue, armed)| *armed && cue.is_running())
                .ok_or_else(|| anyhow!(TARGET_REQUIRED))?;
            match cue.cue_type() {
                CueType::Audio => cue.playing_voice_id()
                    .map(|voice| (CueType::Audio, voice))
                    .ok_or_else(|| anyhow!(AUDIO_REQUIRED)),
                CueType::Video => {
                    let configured = (cue as &dyn Any).downcast_ref::<VideoCue>()
                        .is_some_and(VideoCue::supports_whole_loop_release);
                    cue.playing_voice_id().filter(|_| configured && cue.is_action_started())
                        .map(|voice| (CueType::Video, voice))
                        .ok_or_else(|| anyhow!(VIDEO_REQUIRED))
                }
                _ => Err(anyhow!(TARGET_REQUIRED)),
            }
        });
        let devamp = cue_list.get_mut(&cue_id)
            .and_then(|cue| (cue as &mut dyn Any).downcast_mut::<DevampCue>())
            .ok_or_else(|| anyhow!("Devamp cue not found"))?;
        devamp.reset()?;
        let (kind, voice) = target_voice?;
        if kind == CueType::Audio {
            anyhow::ensure!(self.context.audio_engine.release_whole_audio_loop(voice), "{AUDIO_REQUIRED}");
        } else {
            anyhow::ensure!(self.context.output_engine.release_whole_video_loop(voice), "{VIDEO_REQUIRED}");
            // A movie with a soundtrack carries its audio on a paired voice:
            // release that loop too, so sound and picture finish the same pass.
            if let Some(audio_voice) = self.context.output_engine.video_audio_voice(voice) {
                let _ = self.context.audio_engine.release_whole_audio_loop(audio_voice);
            }
        }
        if devamp.start_next_at_boundary && devamp.continue_mode() == ContinueMode::AutoFollow {
            // QLab "start next cue when the loop ends": stay Running until the
            // released voice finishes its pass, then fire the successor.
            devamp.begin_boundary_wait(voice);
        } else {
            devamp.complete(&self.context);
        }
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // CART MODE — trigger by ID
    // -----------------------------------------------------------------------

    /// Trigger a specific cue by ID (Cart Mode).
    ///
    /// Independent carts fire exactly the addressed pad and its explicit actions.
    /// Sequential lists and legacy carts route through their playhead.
    pub fn go_by_id(&mut self, cue_list: &mut CueList, cue_id: &CueId) -> Result<GoResult> {
        let (_, armed) = cue_list.cue_with_arming(cue_id).ok_or_else(|| anyhow!("Unknown cue {cue_id}"))?;
        let mut result = GoResult::default();
        if !armed {
            return Ok(result);
        }
        if !cue_list.independent_pads() {
            // An addressed second GO on an advancing Playlist is an operation
            // on its child sequence. A held initial admission still uses the
            // main transport's existing retry and release path.
            let preserve = cue_list.mode == CueListMode::Sequential
                && cue_list.get(cue_id)
                    .and_then(|cue| (cue as &dyn Any).downcast_ref::<GroupCue>())
                    .is_some_and(|group| {
                        group.playlist_advance_playhead() == Some(true)
                            && (group.is_running() || group.is_paused())
                            && (cue_list.playhead_cue_id != Some(*cue_id) || !group.owns_playlist_standby())
                    });
            if preserve {
                let group = cue_list.get_mut(cue_id)
                    .and_then(|cue| (cue as &mut dyn Any).downcast_mut::<GroupCue>())
                    .ok_or_else(|| anyhow!("Playlist Group not found"))?;
                group.trigger_playlist_again(&self.context)?;
                result.triggered.push(*cue_id);
                return Ok(result);
            }
            if cue_list.playhead_cue_id != Some(*cue_id) {
                cue_list.set_playhead(Some(*cue_id))?;
            }
            return self.go_automatically(cue_list);
        }
        let pad_id = cue_list.prepare_trigger(*cue_id)?;
        let pad = cue_list.get(&pad_id).ok_or_else(|| anyhow!("Unknown pad {pad_id}"))?;
        let replacement = cue_list.target_destination(pad)?;
        let _ = self.fire_cue(cue_list, pad_id, replacement, false, &mut result)?;
        Ok(result)
    }

    /// Clock ownership is confined to the directly started top-level run.
    pub(crate) fn go_from_timecode(&mut self, cue_list: &mut CueList, cue_id: &CueId) -> Result<GoResult> {
        if cue_list.tc_config.on_stop == TcOnStop::Continue {
            return self.go_by_id(cue_list, cue_id);
        }
        let generation = if let Some(cue) = cue_list.get_mut(cue_id).and_then(ClockCue::from_cue_mut) {
            let Some(generation) = cue.next_generation() else { return Ok(GoResult::default()) };
            Some(generation)
        } else {
            None
        };
        let result = self.go_by_id(cue_list, cue_id);
        let list_id = cue_list.id;
        if let Some((generation, cue)) = generation.zip(cue_list.get_mut(cue_id)
            .and_then(ClockCue::from_cue_mut))
        {
            cue.follow_clock(list_id, generation);
        }
        result
    }

    /// Enter an eligible cue partway through from a newly acquired clock,
    /// without parking or advancing the operator's playhead.
    pub(crate) fn join_from_timecode(&self, list: &mut CueList, id: CueId, elapsed: Duration) -> Result<GoResult> {
        let skipped = GoResult::default();
        let Some(cue) = list.get_mut(&id) else { return Ok(skipped) };
        if cue.is_disabled() || cue.state() != CueState::Standby
            || cue.continue_mode() != ContinueMode::DoNotContinue || elapsed.is_zero() {
            return Ok(skipped);
        }
        let generation = cue.play_generation().wrapping_add(1);
        let joined = match cue.cue_type() {
            CueType::Wait => WaitCue::from_cue_mut(cue)
                .map(|wait| wait.join_from_timecode(elapsed, &self.context)),
            CueType::Video => (cue as &mut dyn Any).downcast_mut::<VideoCue>()
                .map(|video| video.join_from_timecode(elapsed, &self.context)).transpose()?,
            _ => None,
        };
        let result = if let Some(joined) = joined {
            if !joined {
                return Ok(skipped);
            }
            list.cancel_enter_follow_for(id);
            list.independent_runs.insert(id);
            Ok(GoResult { triggered: vec![id], ..GoResult::default() })
        } else if let Some(audio) = list.get_mut(&id)
            .and_then(|cue| (cue as &mut dyn Any).downcast_mut::<AudioCue>()) {
            // The clip-offset path checks the decoded clip window, play rate,
            // slices and Pre-Wait before preparing the very first PCM frame.
            if audio.loaded_time_ms().is_some() || audio.stage_timecode_offset(elapsed).is_err() {
                return Ok(skipped);
            }
            let result = self.start_independent(list, id);
            if !list.get(&id).is_some_and(|cue| cue.is_running()) {
                // A held output must not leave an implicit offset for a later
                // manual GO. Preserve only an operator's existing Load to Time.
                if let Some(audio) = list.get_mut(&id).and_then(|cue| (cue as &mut dyn Any).downcast_mut::<AudioCue>()) {
                    audio.clear_loaded_time(&self.context);
                }
            }
            result
        } else {
            return Ok(skipped);
        };
        if list.tc_config.on_stop != TcOnStop::Continue {
            let owner = list.id;
            if let Some(cue) = list.get_mut(&id).and_then(ClockCue::from_cue_mut) {
                cue.follow_clock(owner, generation);
            }
        }
        result
    }

    /// Start one addressed action without shifting the desk's standby cue.
    /// Constrain starts to actions independent of list sequencing.
    pub fn start_independent(&self, list: &mut CueList, id: CueId) -> Result<GoResult> {
        let mut result = GoResult::default();
        if !list.can_start_independently(id) || !list.cue_with_arming(&id).is_some_and(|(_, armed)| armed) {
            return Ok(result);
        }
        let cue = list.get(&id).ok_or_else(|| anyhow!("Cue {id} no longer exists"))?;
        if cue.is_running() || cue.is_paused() {
            return Ok(result);
        }
        let replacement = list.target_destination(cue)?;
        let fired = self.fire_cue(list, id, replacement, false, &mut result);
        if fired.is_ok() {
            list.cancel_enter_follow_for(id);
        }
        if list.get(&id).is_some_and(|cue| cue.is_running() || cue.is_paused()) {
            list.independent_runs.insert(id);
        }
        let _ = fired?;
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // STOP / PAUSE / RESUME
    // -----------------------------------------------------------------------

    /// Stop a specific cue (with soft fade-out).
    pub fn stop_cue(&mut self, cue_list: &mut CueList, cue_id: &CueId) -> Result<()> {
        let cue = cue_list
            .get_mut(cue_id)
            .ok_or_else(|| anyhow!("Cue not found: {:?}", cue_id))?;
        let result = cue.stop(&self.context);
        cue_list.cancel_enter_follow_for(*cue_id);
        result
    }

    /// Hard-stop a specific cue (immediate cut, no fade).
    pub fn hard_stop_cue(&mut self, cue_list: &mut CueList, cue_id: &CueId) -> Result<()> {
        let cue = cue_list
            .get_mut(cue_id)
            .ok_or_else(|| anyhow!("Cue not found: {:?}", cue_id))?;
        let result = cue.hard_stop(&self.context);
        cue_list.cancel_enter_follow_for(*cue_id);
        result
    }

    /// Stop all running cues with a soft fade-out.
    pub fn stop_all(&mut self, cue_list: &mut CueList) -> Result<()> {
        cue_list.cancel_enter_follow();
        let running_ids: Vec<CueId> = cue_list
            .cues
            .iter()
            .filter(|c| c.is_running() || c.is_paused() || c.loaded_time_ms().is_some())
            .map(|c| c.id())
            .collect();

        for id in running_ids {
            if let Some(cue) = cue_list.get_mut(&id) {
                let _ = cue.stop(&self.context);
            }
        }
        Ok(())
    }

    /// Panic stop (double-Escape): hard-stop all running cues, then cut
    /// everything the engines are outputting regardless of cue bookkeeping.
    ///
    /// The per-cue pass alone is not enough: a cue whose state desynced from
    /// its engine voice (state says Standby/Completed but the voice still
    /// plays) is invisible to the `is_running()` filter and unstoppable
    /// through its own `hard_stop()`.  The engine-level backstop silences
    /// those too, and the final reset pass clears their stale bookkeeping.
    pub fn hard_stop_all(&mut self, cue_list: &mut CueList) -> Result<()> {
        cue_list.cancel_enter_follow();
        cue_list.wall_clock_session.disarm_all();
        let running_ids: Vec<CueId> = cue_list
            .cues
            .iter()
            .filter(|c| c.is_running() || c.is_paused() || c.loaded_time_ms().is_some())
            .map(|c| c.id())
            .collect();

        for id in running_ids {
            if let Some(cue) = cue_list.get_mut(&id) {
                let _ = cue.hard_stop(&self.context);
            }
        }

        let _ = self.context.audio_engine.panic_stop_all();
        self.context.output_engine.panic_stop();

        for cue in cue_list.cues.iter_mut() {
            cue.clear_temporary_targets();
            let _ = cue.reset();
        }
        cue_list.independent_runs.clear();
        Ok(())
    }

    fn reset_cue_state(&self, cue: &mut dyn Cue) -> Result<()> {
        cue.clear_temporary_targets();
        let stopped = cue.hard_stop(&self.context);
        let reset = cue.reset();
        self.context.reset_action_colors(cue);
        stopped.and(reset)
    }

    /// Devamp the addressed cue's voices (QLab `/cue/{n}/devamp`).
    pub fn devamp_cue(&self, cue_list: &mut CueList, cue_id: CueId) -> Result<bool> {
        self.fire_whole_cue_devamp(cue_list, cue_id)
    }

    pub(crate) fn reset_cue(&self, list: &mut CueList, id: CueId) -> Result<()> {
        list.cancel_enter_follow_for(id);
        let cue = list.get_mut(&id).ok_or_else(|| anyhow!("Cue {id} no longer exists"))?;
        self.reset_cue_state(cue)
    }

    pub(crate) fn reset_workspace(&mut self, lists: &mut [CueList]) -> Result<()> {
        // Stop every list before preparing any cue. The engine panic backstop
        // is shared and would otherwise discard cues loaded in an earlier list.
        for list in &mut *lists {
            self.hard_stop_all(list)?;
        }
        for list in lists {
            for cue in &list.cues {
                self.context.reset_action_colors(cue.as_ref());
            }
            if list.independent_pads() {
                list.set_playhead(None)?;
                continue;
            }
            let first = list.cues.first().map(|cue| cue.id());
            list.set_playhead(first)?;
            if let Some(cue) = first.and_then(|id| list.get_mut(&id)).filter(|cue| !cue.is_disabled()) {
                let alert_id = format!("load-{}", cue.id());
                match cue.preload(&self.context) {
                    Ok(()) => crate::health::clear(&alert_id),
                    Err(error) => crate::health::set(crate::health::HealthAlert::new(
                        &alert_id, crate::health::HealthLevel::Warning,
                        format!("Could not load {} after Reset Workspace: {error}", cue.name()),
                    )),
                }
            }
        }
        Ok(())
    }

    /// Pause a specific cue.
    pub fn pause_cue(&mut self, cue_list: &mut CueList, cue_id: &CueId) -> Result<()> {
        let cue = cue_list
            .get_mut(cue_id)
            .ok_or_else(|| anyhow!("Cue not found: {:?}", cue_id))?;
        cue.pause(&self.context)
    }

    /// Resume a paused cue.
    pub fn resume_cue(&mut self, cue_list: &mut CueList, cue_id: &CueId) -> Result<()> {
        let cue = cue_list
            .get_mut(cue_id)
            .ok_or_else(|| anyhow!("Cue not found: {:?}", cue_id))?;
        cue.resume(&self.context)
    }
}
