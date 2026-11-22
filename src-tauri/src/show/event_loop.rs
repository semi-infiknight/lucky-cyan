//! Background event loop running at ~30 fps.
//!
//! This task bridges the engines and the Tauri frontend:
//! - Drains [`AudioStatus`] messages from the audio engine's ring buffer.
//! - Drains [`OutputStatus`] messages from the output engine's channel.
//! - Marks cues as completed when their voice ends.
//! - Applies video duration updates to the owning cue.
//! - Fires Auto-Continue chains (Post-Wait based).
//! - Emits `cue-state-changed`, `cue-time-update`, and `master-level`
//!   Tauri events so the UI stays in sync without polling.
//! - Calls [`AudioEngine::gc_voices`] to release stopped audio voice memory.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use tauri::{Emitter, Manager};

use crate::{
    cue::{
        context::{CueContext, CueEvent},
        group_cue::GroupCue,
        traits::Cue,
        types::{ContinueMode, CueId, CueState, CueType, GroupMode},
    },
    engine::{
        midi_trigger::MidiDeskInput,
        output_engine::{OutputEngine, OutputStatus},
        ring_command::AudioStatus,
        timecode_receiver::TimecodeReceiver,
        timecode_types::{TcEvent, TcOnStop, TcPosition, TcRate, TcTriggerWindow},
        AudioEngine, DmxEngine,
    },
    show::{
        cue_list::CueList,
        timecode_loss::{transition_device_hold, ClockCue, ClockEdge, ClockLock},
        transport::Transport,
        workspace::Workspace,
    },
};
#[cfg(test)]
use crate::cue::wait_cue::WaitCue;

/// Target tick interval for the main event loop (~30 fps).
/// Timer overlay refresh interval — fast enough for smooth millisecond display.
const TIMER_TICK_MS: u64 = 16;
/// If the output stream produces no callback for this long, the audio is frozen
/// (device lost / mid-switch) and running audio cues are paused so their
/// timeline does not drift past the frozen audio.  Above a planned switch's
/// ~tens-of-ms gap, below a perceptible delay.
const AUDIO_FREEZE_MS: u64 = 250;

#[derive(Default, Clone, Copy)]
enum TimecodeTriggerMode {
    #[default]
    Live,
    Suspended,
    RestoreAfter(Instant),
    BaselineAt(Instant),
}

#[derive(Default)]
pub(crate) struct TimecodeDispatch {
    mode: TimecodeTriggerMode,
    receiver: Weak<TimecodeReceiver>,
    previous: Option<TcPosition>,
    locks: HashMap<uuid::Uuid, (TcRate, ClockLock)>,
}

#[derive(Default)]
struct TimecodeBatch {
    status: Option<TcEvent>,
    fire: Vec<(uuid::Uuid, CueId)>,
    join: Vec<(uuid::Uuid, CueId, Duration)>,
    edges: Vec<(uuid::Uuid, ClockEdge)>,
    detached: bool,
}

impl TimecodeBatch {
    fn lose_lock(&mut self, list: &CueList, edge: ClockEdge) {
        self.join.retain(|(list_id, _, _)| *list_id != list.id);
        if list.tc_config.on_stop != TcOnStop::Continue {
            self.fire.retain(|(list_id, cue_id)| {
                *list_id != list.id
                    || !list.get(cue_id).is_some_and(|cue| ClockCue::supports(cue.cue_type()))
            });
        }
        self.edges.push((list.id, edge));
    }

    /// Apply observed edges to existing runs before natural completion can
    /// schedule a follow. Keep the transition order for the operator display.
    fn apply_existing_runs(
        &mut self,
        lists: &mut [CueList],
        context: &CueContext,
        device_frozen: bool,
        device_holds: &mut std::collections::HashSet<CueId>,
    ) -> Vec<(CueId, CueState, CueState)> {
        if self.detached {
            for list in lists.iter_mut() {
                list.detach_timecode_runs();
            }
        }
        let mut changes = Vec::new();
        for (list_id, edge) in self.edges.drain(..) {
            let Some(list) = lists.iter_mut().find(|list| list.id == list_id) else { continue };
            let policy = list.tc_config.on_stop;
            for cue in &mut list.cues {
                let id = cue.id();
                let old = cue.state();
                let Some(cue) = ClockCue::from_cue_mut(cue.as_mut()) else { continue };
                match cue.apply_edge(list_id, edge, policy, context, device_frozen, device_holds) {
                    Ok(Some(new)) => changes.push((id, old, new)),
                    Ok(None) => {}
                    Err(error) => log::warn!("Clock loss transition for cue {id}: {error}"),
                }
            }
        }
        changes
    }
}

impl TimecodeDispatch {
    pub(crate) fn suspended(&self) -> bool {
        matches!(self.mode, TimecodeTriggerMode::Suspended)
    }

    pub(crate) fn set_suspended(&mut self, suspended: bool, lists: &mut [CueList], now: Instant) {
        if self.suspended() == suspended { return; }
        self.mode = if suspended { TimecodeTriggerMode::Suspended } else { TimecodeTriggerMode::RestoreAfter(now) };
        self.previous = None;
        self.locks.clear();
        if suspended {
            for list in lists {
                list.detach_timecode_runs();
            }
        }
    }

    fn expire_locks(&mut self, lists: &[CueList], now: Instant, batch: &mut TimecodeBatch) {
        for list in lists {
            if let Some((_, lock)) = self.locks.get_mut(&list.id) {
                if let Some(edge) = lock.expire(now, list.tc_config.freewheel_ms) {
                    batch.lose_lock(list, edge);
                }
            }
        }
    }

    fn collect(
        &mut self,
        receiver: Option<&Arc<TimecodeReceiver>>,
        lists: &[CueList],
        now: Instant,
    ) -> TimecodeBatch {
        let current = receiver.map(Arc::downgrade).unwrap_or_default();
        let mut batch = TimecodeBatch::default();
        if !Weak::ptr_eq(&self.receiver, &current) {
            self.receiver = current;
            self.previous = None;
            self.locks.clear();
            batch.status = Some(TcEvent::Stopped);
            batch.detached = true;
        }
        self.locks.retain(|id, (rate, _)| lists.iter().any(|list| {
            &list.id == id && list.tc_config.enabled && list.tc_config.rate == *rate
        }));

        if let Some(receiver) = receiver {
            for observation in receiver.events() {
                let event = observation.event;
                batch.status = Some(event);
                if self.suspended() || matches!(self.mode, TimecodeTriggerMode::RestoreAfter(at) if observation.at <= at) {
                    continue;
                }
                if matches!(event, TcEvent::Stopped | TcEvent::Located(_)) {
                    // Account for a loss that already happened before the barrier.
                    // A locate then suspends deadline inference until another frame.
                    self.expire_locks(lists, observation.at, &mut batch);
                    // These starts were collected for an earlier clock run.
                    batch.fire.clear();
                    batch.join.clear();
                }
                let mut reopened = Vec::new();
                let mut first_position = None;
                match event {
                    TcEvent::Position(pos, step) if pos.is_valid() => {
                        if self.previous.is_none_or(|prior| !prior.rate.same_address_format(pos.rate)) {
                            first_position = Some(pos);
                        }
                        for list in lists.iter().filter(|list| {
                            list.tc_config.enabled && list.tc_config.rate.same_address_format(pos.rate)
                        }) {
                            let (_, lock) = self.locks.entry(list.id)
                                .or_insert_with(|| (list.tc_config.rate, ClockLock::default()));
                            if let Some(edge) = lock.expire(observation.at, list.tc_config.freewheel_ms) {
                                batch.lose_lock(list, edge);
                            }
                            if let Some(edge) = lock.receive(observation.at, pos.rate, step) {
                                reopened.push(list.id);
                                batch.edges.push((list.id, edge));
                            }
                        }
                    }
                    TcEvent::Located(_) => self.locks.values_mut().for_each(|(_, lock)| lock.locate()),
                    _ => {}
                }
                if let Some(window) = TcTriggerWindow::advance(&mut self.previous, event) {
                    // A quarter-frame message can span restore. LTC can also
                    // publish several decoded addresses from one input block.
                    // Use the whole first observation timestamp as a baseline.
                    match self.mode {
                        TimecodeTriggerMode::RestoreAfter(_) => {
                            self.mode = TimecodeTriggerMode::BaselineAt(observation.at);
                            continue;
                        }
                        TimecodeTriggerMode::BaselineAt(at) if observation.at == at => continue,
                        TimecodeTriggerMode::BaselineAt(_) => self.mode = TimecodeTriggerMode::Live,
                        _ => {}
                    }
                    for list in lists {
                        let crossed = list.tc_triggers_crossed(window);
                        if let Some(pos) = first_position.or_else(|| match event {
                            TcEvent::Position(pos, _) if reopened.contains(&list.id) => Some(pos),
                            _ => None,
                        }) {
                            batch.join.extend(list.tc_joins_at(pos).into_iter()
                                .filter(|(cue_id, _)| !crossed.contains(cue_id))
                                .map(|(cue_id, elapsed)| (list.id, cue_id, elapsed)));
                        }
                        batch.fire.extend(crossed.into_iter().map(|id| (list.id, id)));
                    }
                }
            }
        }
        self.expire_locks(lists, now, &mut batch);
        batch
    }
}

/// Entry point for the event loop thread.  Loops indefinitely.
pub(crate) fn run(
    handle: tauri::AppHandle,
    audio_engine: Arc<AudioEngine>,
    output_engine: Arc<OutputEngine>,
    dmx_engine: Arc<DmxEngine>,
    workspace: Arc<Mutex<Workspace>>,
    tc_receiver: Arc<Mutex<Option<Arc<TimecodeReceiver>>>>,
    timecode_dispatch: Arc<Mutex<TimecodeDispatch>>,
    midi_listener: Arc<Mutex<Option<Arc<crate::engine::midi_trigger::MidiTriggerListener>>>>,
) {
    // Spawn a dedicated thread that refreshes the OSD timer overlay at ~60 fps.
    // This is independent of the main 30 fps tick so the millisecond display
    // stays smooth even when the workspace lock is briefly held by a command.
    {
        let ws2 = Arc::clone(&workspace);
        let oe2 = Arc::clone(&output_engine);
        std::thread::Builder::new()
            .name("inkue-timer-refresh".into())
            .spawn(move || timer_refresh_loop(ws2, oe2))
            .expect("Failed to spawn timer refresh thread");
    }

    // Maps a completed cue's ID to the deadline and the ID of its cue list.
    let mut auto_follow_pending: HashMap<CueId, (Instant, uuid::Uuid)> = HashMap::new();
    let mut midi_workspace = None;
    // Per-group snapshot: (active_child_id, any_child_running).
    // Used to detect inner-sequence progress and emit cue-list-refresh.
    let mut prev_group_state: HashMap<CueId, (Option<CueId>, bool)> = HashMap::new();
    let mut prev_monitor_structure = MonitorStructure::default();
    // Cue sets tracked for OSC feedback (compared each tick to detect changes).
    let mut prev_running_cues: Vec<CueId>  = Vec::new();
    let mut prev_playhead_cue: Option<CueId> = None;
    // Fingerprint of the full cue list (number+name). Sending on change.
    let mut prev_cue_list_hash: u64 = 0;
    // Audio-freeze guard state (device-loss timeline pause).
    let mut last_callback_stamp: u64 = audio_engine.last_callback_us();
    let mut cb_last_advance: Instant = Instant::now();
    let mut audio_frozen = false;
    let mut auto_paused: std::collections::HashSet<CueId> = std::collections::HashSet::new();
    let mut pending_playback = PendingPlayback::default();

    loop {
        std::thread::sleep(crate::cue::types::CUE_TICK_INTERVAL);
        tick(
            &handle,
            &audio_engine,
            &output_engine,
            &dmx_engine,
            &workspace,
            &tc_receiver,
            &midi_listener,
            &mut midi_workspace,
            &timecode_dispatch,
            &mut auto_follow_pending,
            &mut prev_group_state,
            &mut prev_monitor_structure,
            &mut prev_running_cues,
            &mut prev_playhead_cue,
            &mut prev_cue_list_hash,
            &mut last_callback_stamp,
            &mut cb_last_advance,
            &mut audio_frozen,
            &mut auto_paused,
            &mut pending_playback,
        );
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn make_context(
    audio_engine: &Arc<AudioEngine>,
    output_engine: &Arc<OutputEngine>,
    dmx_engine: &Arc<DmxEngine>,
    stop_fade_ms: u32,
    output_patches: Vec<crate::engine::device_manager::OutputPatch>,
    default_patch_id: Option<uuid::Uuid>,
    osc_patches: Vec<crate::engine::osc_patch::OscPatch>,
    fixtures: Vec<crate::engine::fixture::PatchedFixture>,
    fixture_groups: Vec<crate::engine::fixture::FixtureGroup>,
    input_patches: Vec<crate::engine::audio_input::InputPatch>,
    audio_buffer_size: u32,
) -> CueContext {
    let (tx, _rx) = crossbeam_channel::unbounded::<CueEvent>();
    CueContext::new(
        audio_engine.clone(),
        output_engine.clone(),
        tx,
        stop_fade_ms,
        output_patches,
        default_patch_id,
        osc_patches,
        dmx_engine.clone(),
        fixtures,
        fixture_groups,
        input_patches,
        audio_buffer_size,
    )
}

/// Action-elapsed time for UI / OSC displays, in ms.
///
/// Sliced cues report the engine's **media position** instead of wall-clock
/// time: a vamping segment holds its file position while the clock runs on,
/// so wall-clock bars kept sweeping and looping (and stayed wrong after a
/// devamp, which only the engine knows about). Silent movies with speed
/// control also use file time, since it can advance faster than wall time.
pub(crate) fn media_elapsed_ms(
    cue: &dyn crate::cue::traits::Cue,
    audio_engine: &Arc<AudioEngine>,
    output_engine: &Arc<OutputEngine>,
) -> u64 {
    if cue.uses_silent_speed_clock() {
        return cue.playing_voice_id().and_then(|voice| output_engine.silent_speed_position_ms(voice)).unwrap_or(0);
    }
    if cue.uses_sliced_playback() {
        let media_pos = if cue.is_visual() {
            cue.playing_voice_id().and_then(|v| output_engine.voice_position_ms(v))
        } else {
            cue.playing_voice_id().and_then(|v| audio_engine.voice_position_ms(v))
        };
        if let Some(pos) = media_pos {
            return pos;
        }
    }
    cue.action_elapsed().as_millis() as u64
}

/// Remaining play time uses the live PCM position and rate for Audio.
pub(crate) fn remaining_ms(
    cue: &dyn crate::cue::traits::Cue,
    elapsed: u64,
    audio_engine: &AudioEngine,
) -> Option<u64> {
    if cue.awaits_voice_completion() && !cue.is_visual() && !cue.uses_sliced_playback() {
        return cue.playing_voice_id().and_then(|voice| audio_engine.voice_remaining_ms(voice));
    }
    cue.duration().map(|duration| (duration.as_millis() as u64).saturating_sub(elapsed))
}

/// OSC progress values for one cue: `(progress 0..1, elapsed s, remaining s,
/// duration s)`; `remaining`/`duration` are `-1.0` when unknown.
///
/// Without a total duration (vamp, ∞ loop, live feed) the progress falls back
/// to the position within one file pass — a gauge then follows the loop just
/// like the in-app bars.
fn progress_values(
    elapsed_ms: u64,
    duration_ms: Option<u64>,
    file_duration_ms: Option<u64>,
) -> (f32, f32, f32, f32) {
    let elapsed_s = elapsed_ms as f32 / 1000.0;
    match duration_ms {
        Some(d) if d > 0 => {
            let progress = (elapsed_ms as f32 / d as f32).clamp(0.0, 1.0);
            let remaining_s = d.saturating_sub(elapsed_ms) as f32 / 1000.0;
            (progress, elapsed_s, remaining_s, d as f32 / 1000.0)
        }
        _ => {
            let progress = match file_duration_ms {
                Some(fd) if fd > 0 => ((elapsed_ms % fd) as f32 / fd as f32).clamp(0.0, 1.0),
                _ => 0.0,
            };
            (progress, elapsed_s, -1.0, -1.0)
        }
    }
}

/// Collect `cue-time-update` snapshots recursively, including children of
/// running Group cues.
struct CueTimeSnapshot {
    cue_id: CueId,
    elapsed_ms: u64,
    action_elapsed_ms: u64,
    remaining_ms: Option<u64>,
    nested_state: Option<CueState>,
}

fn collect_time_snapshots(
    cues: &[Box<dyn crate::cue::traits::Cue>],
    audio_engine: &Arc<AudioEngine>,
    output_engine: &Arc<OutputEngine>,
    nested: bool,
    result: &mut Vec<CueTimeSnapshot>,
) {
    for cue in cues {
        let state = cue.state();
        if matches!(state, CueState::Running | CueState::Paused) {
            let action_elapsed_ms = media_elapsed_ms(cue.as_ref(), audio_engine, output_engine);
            result.push(CueTimeSnapshot {
                cue_id: cue.id(),
                elapsed_ms: cue.elapsed().as_millis() as u64,
                action_elapsed_ms,
                remaining_ms: remaining_ms(cue.as_ref(), action_elapsed_ms, audio_engine),
                nested_state: nested.then_some(state),
            });
        }
        if let Some(children) = cue.child_cues() {
            collect_time_snapshots(children, audio_engine, output_engine, true, result);
        }
    }
}

/// Playback changes that per-cue events can miss.
#[derive(Default)]
struct MonitorStructure {
    playheads: Vec<(uuid::Uuid, Option<CueId>)>,
    nested_active: Vec<(CueId, CueState)>,
}

impl MonitorStructure {
    /// Compare playheads and nested active rows separately. The main cue list
    /// needs the nested projection, while the monitor also tracks other lists' standby.
    fn update(&mut self, ws: &Workspace, snapshots: &[CueTimeSnapshot]) -> (bool, bool) {
        let playheads = ws.cue_lists.iter().map(|list| (list.id, list.playhead_cue_id));
        let nested = snapshots.iter().filter_map(|time| Some((time.cue_id, time.nested_state?)));
        let playheads_changed = !playheads.clone().eq(self.playheads.iter().copied());
        let nested_changed = !nested.clone().eq(self.nested_active.iter().copied());
        if playheads_changed {
            self.playheads.clear();
            self.playheads.extend(playheads);
        }
        if nested_changed {
            self.nested_active.clear();
            self.nested_active.extend(nested);
        }
        (playheads_changed, nested_changed)
    }
}

/// Collect the ids of `cue` and every descendant that is Running or Paused.
/// Used when a Fade force-stops a target so the UI can be told which cues
/// (including group children) are no longer active.
fn collect_running_ids(cue: &dyn crate::cue::traits::Cue, out: &mut Vec<CueId>) {
    if cue.state() == CueState::Running || cue.state() == CueState::Paused {
        out.push(cue.id());
    }
    if let Some(children) = cue.child_cues() {
        for ch in children {
            collect_running_ids(ch.as_ref(), out);
        }
    }
}

/// Reset any RUNNING group child whose audio voice has completed.
///
/// The top-level completion detector (step 6) does not descend into group
/// children, and audio cues never self-complete, so a group must reap its own
/// children.  A group can only detect a child via wall-clock `duration()`, which
/// is `None` while the child is still preloading (its buffer is played but
/// `cached_duration` isn't set yet).  Such a child would linger in Running
/// forever — stalling the group.  This closes the gap by resetting children
/// whose voice the engine reported completed, at any nesting depth.
fn reap_voice_completed_children(
    cues: &mut [Box<dyn crate::cue::traits::Cue>],
    completed: &[CueId],
    withdrawn: &[CueId],
    before_group_tick: bool,
) -> bool {
    let mut changed = false;
    for cue in cues.iter_mut() {
        let start_first = matches!(cue.group_mode(), Some(GroupMode::StartFirst | GroupMode::StartFirstAndEnter));
        if start_first {
            if let Some(group) = (cue.as_mut() as &mut dyn std::any::Any).downcast_mut::<GroupCue>() {
                changed |= group.reap_start_first_children(completed, withdrawn);
            }
        } else if before_group_tick && cue.playlist_advance_playhead() == Some(true) {
            if let Some(group) = (cue.as_mut() as &mut dyn std::any::Any).downcast_mut::<GroupCue>() {
                changed |= group.reap_playlist_withdrawals(withdrawn);
            }
        }
        if let Some(children) = cue.child_cues_mut() {
            if !start_first && !before_group_tick {
                for child in children.iter_mut() {
                    let voice_done = child.state() == CueState::Running
                        && child
                            .playing_voice_id()
                            .map(|v| completed.contains(&v))
                            .unwrap_or(false);
                    if voice_done {
                        changed |= child.reset().is_ok();
                    }
                }
            }
            changed |= reap_voice_completed_children(children, completed, withdrawn, before_group_tick);
        }
    }
    changed
}

pub(crate) fn reset_withdrawn_output(
    cue: &mut dyn crate::cue::traits::Cue,
    withdrawals: &HashSet<CueId>,
    changed: &mut Vec<(CueId, CueState)>,
) -> bool {
    let state = cue.state();
    if matches!(state, CueState::Running | CueState::Paused)
        && cue.playing_voice_id().is_some_and(|voice| withdrawals.contains(&voice))
    {
        let _ = cue.reset();
        changed.push((cue.id(), state));
        return true;
    }

    let affected_children: Vec<_> = cue.child_cues_mut().into_iter().flatten()
        .filter_map(|child| {
            reset_withdrawn_output(child.as_mut(), withdrawals, changed).then(|| child.id())
        })
        .collect();
    for &id in &affected_children {
        cue.child_output_withdrawn(id);
    }
    !affected_children.is_empty()
}

fn collect_output_start_holds(cue: &mut dyn crate::cue::traits::Cue, changes: &mut Vec<(CueId, CueState)>) {
    if cue.take_output_start_hold() {
        changes.push((cue.id(), CueState::Running));
    }
    if let Some(children) = cue.child_cues_mut() {
        for child in children {
            collect_output_start_holds(child.as_mut(), changes);
        }
    }
}

/// Engine notifications that must survive a tick with a busy workspace.
#[derive(Default)]
struct PendingPlayback {
    completed: Vec<CueId>,
    withdrawn: Vec<CueId>,
    resets: HashSet<CueId>,
    video_durations: Vec<(CueId, Duration)>,
}

#[allow(clippy::too_many_arguments)]
fn tick(
    handle: &tauri::AppHandle,
    audio_engine: &Arc<AudioEngine>,
    output_engine: &Arc<OutputEngine>,
    dmx_engine: &Arc<DmxEngine>,
    workspace: &Arc<Mutex<Workspace>>,
    tc_receiver:    &Arc<Mutex<Option<Arc<TimecodeReceiver>>>>,
    midi_listener:   &Arc<Mutex<Option<Arc<crate::engine::midi_trigger::MidiTriggerListener>>>>,
    midi_workspace: &mut Option<uuid::Uuid>,
    timecode_dispatch: &Mutex<TimecodeDispatch>,
    auto_follow_pending: &mut HashMap<CueId, (Instant, uuid::Uuid)>,
    prev_group_state:    &mut HashMap<CueId, (Option<CueId>, bool)>,
    prev_monitor_structure: &mut MonitorStructure,
    prev_running_cues:   &mut Vec<CueId>,
    prev_playhead_cue:   &mut Option<CueId>,
    prev_cue_list_hash:  &mut u64,
    last_callback_stamp:       &mut u64,
    cb_last_advance:     &mut Instant,
    audio_frozen:        &mut bool,
    auto_paused:         &mut std::collections::HashSet<CueId>,
    pending_playback:     &mut PendingPlayback,
) {
    // ------------------------------------------------------------------
    // 1. Drain the audio status ring buffer.
    // ------------------------------------------------------------------
    let audio_statuses = audio_engine.drain_status();

    let mut master_peak_l = 0.0_f32;
    let mut master_peak_r = 0.0_f32;
    let mut has_master = false;
    // Per-Output-Patch peaks for the mixer VUs, keyed by patch slot.
    let mut patch_peaks: [(f32, f32); crate::engine::audio_engine::PATCH_VU_SLOTS] =
        [(0.0, 0.0); crate::engine::audio_engine::PATCH_VU_SLOTS];
    let mut has_patch_levels = false;

    for s in audio_statuses {
        match s {
            AudioStatus::StartExpired { voice_id } => {
                crate::engine::audio_engine::report_held_audio();
                crate::media_usage::output_error(voice_id, "Audio start expired");
                pending_playback.withdrawn.push(voice_id);
                pending_playback.completed.push(voice_id);
            }
            AudioStatus::OutputWithdrawn { voice_id } => {
                crate::health::set(crate::health::HealthAlert::new(
                    "output-patch-device",
                    crate::health::HealthLevel::Error,
                    "An Audio cue lost its required output. Check the Output Patch before restarting the cue.",
                ));
                crate::media_usage::output_error(voice_id, "Required audio output lost");
                pending_playback.resets.insert(voice_id);
            }
            AudioStatus::Completed { voice_id } => {
                pending_playback.completed.push(voice_id);
            }
            AudioStatus::MasterLevels { peak_l, peak_r } => {
                master_peak_l = master_peak_l.max(peak_l);
                master_peak_r = master_peak_r.max(peak_r);
                has_master = true;
            }
            AudioStatus::PatchLevels { slot, peak_l, peak_r } => {
                if let Some(p) = patch_peaks.get_mut(slot as usize) {
                    p.0 = p.0.max(peak_l);
                    p.1 = p.1.max(peak_r);
                    has_patch_levels = true;
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // 2. Drain the output engine status channel.
    // ------------------------------------------------------------------
    let output_statuses = output_engine.drain_status();

    // Voices taken off their output (not ended): their cue resets without
    // chaining, like a stopped cue.

    for s in output_statuses {
        match s {
            OutputStatus::Completed { voice_id } => {
                pending_playback.completed.push(voice_id);
                output_engine.gc_voice(voice_id);
            }
            OutputStatus::Withdrawn { voice_id } => {
                pending_playback.completed.push(voice_id);
                pending_playback.withdrawn.push(voice_id);
            }
            OutputStatus::Duration { voice_id, duration_ms } => {
                pending_playback.video_durations.push((voice_id, Duration::from_millis(duration_ms)));
            }
            OutputStatus::Error { voice_id, message } => {
                log::warn!("Output voice {voice_id} error: {message}");
                crate::media_usage::output_error(voice_id, &message);
                pending_playback.resets.insert(voice_id);
            }
        }
    }

    // Emit master-level whenever there is any active signal.
    if has_master {
        let _ = handle.emit(
            "master-level",
            serde_json::json!({ "peak_l": master_peak_l, "peak_r": master_peak_r }),
        );
    }

    // Per-patch levels for the mixer VUs (slot = index in ws.output_patches).
    if has_patch_levels {
        let levels: Vec<serde_json::Value> = patch_peaks
            .iter()
            .enumerate()
            .filter(|(_, (l, r))| *l > 0.0 || *r > 0.0)
            .map(|(slot, (l, r))| serde_json::json!({ "slot": slot, "peak_l": l, "peak_r": r }))
            .collect();
        let _ = handle.emit("patch-levels", serde_json::json!({ "levels": levels }));
    }

    // ------------------------------------------------------------------
    // 3. Lock the workspace (non-blocking; skip tick if a command holds it).
    // ------------------------------------------------------------------
    let mut ws = match workspace.try_lock() {
        Ok(w) => w,
        Err(_) => return,
    };
    let PendingPlayback {
        completed: completed_voice_ids,
        withdrawn: withdrawn_voice_ids,
        resets: exact_output_withdrawals,
        video_durations: video_duration_updates,
    } = std::mem::take(pending_playback);
    let emit_workspace_modified = !video_duration_updates.is_empty();

    // Leave hardware messages queued when the desk lock is busy. Discard the
    // queued batch at a workspace boundary before using the new show's bindings.
    let midi_messages = midi_listener.try_lock().ok().and_then(|slot| {
        let same_show = midi_workspace.replace(ws.instance_id) == Some(ws.instance_id);
        let messages = slot.as_ref()?.drain();
        same_show.then_some(messages)
    }).unwrap_or_default();
    // Retain required-device losses and fatal playback errors until the owning show can be locked.
    // Reset before device-loss resume or any group and follow clocks tick.
    let mut exact_output_resets = Vec::new();
    if !exact_output_withdrawals.is_empty() {
        for list in &mut ws.cue_lists {
            for cue in &mut list.cues {
                reset_withdrawn_output(cue.as_mut(), &exact_output_withdrawals, &mut exact_output_resets);
            }
        }
        for (id, _) in &exact_output_resets {
            auto_paused.remove(id);
            auto_follow_pending.remove(id);
        }
    }

    let stop_fade_ms      = ws.preferences.audio.default_fade_out_ms;
    let ws_patches        = ws.output_patches.clone();
    let ws_default_patch  = ws.default_output_patch_id;
    // Keep the engine's projector-alignment mirror in sync with the loaded
    // workspace (no-op unless it actually changed — covers open/new/recovery
    // without hooking every load path).
    output_engine.sync_outputs_config(&ws.outputs_config());
    let ws_osc_patches    = ws.osc_patches.clone();
    let ws_fixtures       = ws.fixtures.clone();
    let ws_fixture_groups = ws.fixture_groups.clone();
    let ws_input_patches  = ws.input_patches.clone();
    let ws_buffer_size    = ws.preferences.audio.audio_buffer_size;
    let active_list_id    = ws.active_cue_list_id;

    if ws.cue_lists.is_empty() {
        return;
    }

    let mut tick_ctx = make_context(audio_engine, output_engine, dmx_engine, stop_fade_ms, ws_patches.clone(), ws_default_patch, ws_osc_patches.clone(), ws_fixtures.clone(), ws_fixture_groups.clone(), ws_input_patches.clone(), ws_buffer_size);
    tick_ctx.set_workspace(&ws);

    // ------------------------------------------------------------------
    // 3b. Audio-freeze guard.  When the output stream stops producing
    //     callbacks (device pulled, or briefly during a switch), pause every
    //     running audio cue so its wall-clock timeline freezes in sync with
    //     the frozen (preserved) audio — otherwise the clock keeps advancing,
    //     hits `duration`, and the cue completes while its audio is still
    //     queued and unstoppable.  Resume them when callbacks return.
    // ------------------------------------------------------------------
    let cb = audio_engine.last_callback_us();
    if cb != 0 && cb != *last_callback_stamp {
        *last_callback_stamp = cb;
        *cb_last_advance = Instant::now();
    }
    let frozen_now = cb_last_advance.elapsed() >= Duration::from_millis(AUDIO_FREEZE_MS);

    // Clock holds take precedence over automatic device resumes. Apply the
    // observed edges before recovering a device that returned on this tick.
    let mut timecode = tc_receiver.lock().ok()
        .and_then(|slot| timecode_dispatch.lock().ok().map(|mut dispatch| {
            dispatch.collect(slot.as_ref(), &ws.cue_lists, Instant::now())
        }))
        .unwrap_or_default();
    let tc_status = timecode.status;
    let clock_changes = timecode.apply_existing_runs(&mut ws.cue_lists, &tick_ctx, frozen_now, auto_paused);

    let mut just_paused:  Vec<CueId> = Vec::new();
    let mut just_resumed: Vec<CueId> = Vec::new();
    if frozen_now && !*audio_frozen {
        for cl in ws.cue_lists.iter_mut() {
            for cue in cl.cues.iter_mut() {
                if cue.state() == CueState::Running && cue.playing_voice_id().is_some()
                    && !cue.uses_silent_speed_clock()
                    && transition_device_hold(cue.as_mut(), true, &tick_ctx).is_ok_and(|changed| changed)
                {
                    auto_paused.insert(cue.id());
                    just_paused.push(cue.id());
                }
            }
        }
    } else if !frozen_now && *audio_frozen {
        for cl in ws.cue_lists.iter_mut() {
            for cue in cl.cues.iter_mut() {
                if !(auto_paused.contains(&cue.id()) && cue.state() == CueState::Paused) {
                    continue;
                }
                if transition_device_hold(cue.as_mut(), false, &tick_ctx).is_ok_and(|changed| changed) {
                    just_resumed.push(cue.id());
                }
            }
        }
        auto_paused.clear();
    }
    *audio_frozen = frozen_now;

    // ------------------------------------------------------------------
    // 4. Apply video duration updates — search every cue list.
    // ------------------------------------------------------------------
    'duration_update: for (voice_id, duration) in &video_duration_updates {
        for cl in ws.cue_lists.iter_mut() {
            for cue in cl.cues.iter_mut() {
                if cue.playing_voice_id() == Some(*voice_id) {
                    cue.set_runtime_duration(*duration);
                    continue 'duration_update;
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // 5-9. Per-list: tick, completion, auto-continue/follow, GO.
    //      We collect cue list IDs first to avoid borrow issues.
    // ------------------------------------------------------------------
    let cue_list_ids: Vec<uuid::Uuid> = ws.cue_lists.iter().map(|cl| cl.id).collect();

    // Aggregate results across all lists for event emission.
    let mut all_newly_completed: Vec<(CueId, ContinueMode, Duration)> = Vec::new();
    let mut all_time_snapshots = Vec::new();
    let mut all_go_triggered:    Vec<CueId>                            = Vec::new();
    let mut all_crossfade_started: Vec<CueId>                          = Vec::new();
    let mut all_go_stopped:      Vec<CueId>                            = Vec::new();
    let mut all_seq_group_playheads: Vec<Option<CueId>>                = Vec::new();
    let mut group_child_changed = !exact_output_resets.is_empty();
    let mut cue_prepared = false;

    // Lists whose auto-continue chain needs a GO this tick.
    let mut should_go_lists: std::collections::HashSet<uuid::Uuid> = std::collections::HashSet::new();
    let mut enter_follows = Vec::new();

    // Resolve pending auto-follow delays and collect which lists need a GO.
    let now = Instant::now();
    auto_follow_pending.retain(|_id, (due, list_id)| {
        if !ws.cue_list_by_id(*list_id).is_some_and(|cl| !cl.independent_pads()) {
            return false;
        }
        if now >= *due {
            should_go_lists.insert(*list_id);
            false
        } else {
            true
        }
    });

    for &list_id in &cue_list_ids {
        // 5. Tick all Running cues.
        if let Some(cl) = ws.cue_list_by_id_mut(list_id) {
            // A withdrawn Start First output must cancel its child chain before
            // this tick can make a scheduled child due.
            if !withdrawn_voice_ids.is_empty() {
                group_child_changed |= reap_voice_completed_children(
                    &mut cl.cues, &[], &withdrawn_voice_ids, true,
                );
            }
            for cue in cl.cues.iter_mut() {
                if cue.state() == CueState::Running && cue.tick(&tick_ctx).is_err() {
                    let previous = exact_output_resets.len();
                    collect_output_start_holds(cue.as_mut(), &mut exact_output_resets);
                    group_child_changed |= exact_output_resets.len() != previous;
                }
            }
            // Crossfades that had to wait (the Fade's pre-wait, the incoming
            // cue's pre-wait or first frame) are set up as soon as they can be.
            let started = crate::show::crossfade::advance_pending(cl, &tick_ctx);
            if !started.is_empty() {
                // The incoming cue may sit in a group: resync the whole tree.
                group_child_changed = true;
                all_crossfade_started.extend(started);
            }
        }

        // 5a. A `stop_at_end` Fade that just finished asks (via take_fade_stop_targets)
        //     for its target CUES to be stopped — stopping voices alone leaves the
        //     targets (and group children) in the Running state.  hard_stop resets
        //     their state and recurses into groups.  We record every id that was
        //     running (target + descendants) so step 11 emits `cue-state-changed`
        //     for them; otherwise the UI keeps showing them RUNNING until the next
        //     GO forces a full refresh.
        let mut fade_stopped_ids: Vec<CueId> = Vec::new();
        let fade_stop_targets: Vec<CueId> = ws
            .cue_list_by_id_mut(list_id)
            .map(|cl| cl.cues.iter_mut().flat_map(|c| c.take_fade_stop_targets()).collect())
            .unwrap_or_default();
        if !fade_stop_targets.is_empty() {
            if let Some(cl) = ws.cue_list_by_id_mut(list_id) {
                for tid in fade_stop_targets {
                    cl.cancel_enter_follow_for(tid);
                    if let Some(target) = cl.get_mut_recursive(&tid) {
                        if target.is_running() || target.is_paused() {
                            collect_running_ids(target, &mut fade_stopped_ids);
                            let _ = target.hard_stop(&tick_ctx);
                        }
                    }
                }
            }
        }

        // 5b. Release standby when a delayed action starts or a Sequential
        //     group fires its last child, while the cues continue playing.
        if let Some(cl) = ws.cue_list_by_id_mut(list_id) {
            let (released, follow) = release_and_take_enter_follow(cl);
            if let Some(target) = follow {
                enter_follows.push((list_id, target));
            } else if released {
                all_seq_group_playheads.push(cl.playhead_cue_id);
            }
        }

        // 6. Detect completions.
        let mut newly_completed: Vec<(CueId, ContinueMode, Duration)> = Vec::new();
        let mut advance_playhead_ids: Vec<CueId> = Vec::new();

        if let Some(cl) = ws.cue_list_by_id_mut(list_id) {
            // Reap nested children whose voice completed first, so a group whose
            // last child just finished can be detected complete this same tick.
            group_child_changed |= reap_voice_completed_children(
                &mut cl.cues, &completed_voice_ids, &withdrawn_voice_ids, false,
            );

            let pending_group = cl.released_enter_group();
            let current_playhead = cl.playhead_cue_id;
            let chains_enabled = !cl.independent_pads();
            for cue in cl.cues.iter_mut() {
                if cue.state() != CueState::Running {
                    continue;
                }
                let voice_done = cue
                    .playing_voice_id()
                    .map(|vid| completed_voice_ids.contains(&vid))
                    .unwrap_or(false);
                let time_done = cue.is_time_complete();
                let group_done = cue.is_complete();
                let independent_run = cl.independent_runs.contains(&cue.id());
                // Leave an action-clock cue available to the normal Auto-Continue
                // scheduler even when Post-Wait lands on or after its final output.
                if (voice_done || time_done || group_done) && pending_group != Some(cue.id())
                    && !(chains_enabled && !independent_run && waits_for_auto_continue(cue.as_ref()))
                {
                    let id = cue.id();
                    let withdrawn = cue.has_withdrawn_output() || cue
                        .playing_voice_id()
                        .is_some_and(|vid| withdrawn_voice_ids.contains(&vid));
                    let cm = if withdrawn || independent_run { ContinueMode::DoNotContinue } else { cue.continue_mode() };
                    let pw = cue.post_wait();
                    if !independent_run && !cue.has_withdrawn_output() && cue.holds_playhead() && current_playhead == Some(id) {
                        advance_playhead_ids.push(id);
                    }
                    let _ = cue.reset();
                    group_child_changed |= cue.child_cues().is_some();
                    newly_completed.push((id, cm, pw));
                }
            }
        }

        // Fade-stopped cues (target + descendants) are reported as completed so
        // the UI clears them.  DoNotContinue so they never chain a follow-on cue.
        if !fade_stopped_ids.is_empty() {
            // Force a full cue-list refresh so nested state + green playhead
            // highlights resync (they derive from the whole cue tree).
            group_child_changed = true;
            for id in fade_stopped_ids {
                newly_completed.push((id, ContinueMode::DoNotContinue, Duration::ZERO));
            }
        }

        // Advance playhead for sequential groups that held it.
        let mut playhead_advanced = false;
        if let Some(cl) = ws.cue_list_by_id_mut(list_id) {
            for id in &advance_playhead_ids {
                if cl.playhead_cue_id == Some(*id) {
                    cl.advance_playhead();
                    playhead_advanced = true;
                }
            }
        }
        if playhead_advanced {
            let ph = ws.cue_list_by_id(list_id).and_then(|cl| cl.playhead_cue_id);
            all_seq_group_playheads.push(ph);
        }

        // 7. Time snapshots.
        if let Some(cl) = ws.cue_list_by_id(list_id) {
            collect_time_snapshots(&cl.cues, audio_engine, output_engine, false, &mut all_time_snapshots);
        }

        // 8. Auto-Continue / Auto-Follow detection.
        if let Some(cl) = ws.cue_list_by_id_mut(list_id) {
            if schedule_list_follows(cl, &newly_completed, auto_follow_pending, Instant::now()) {
                should_go_lists.insert(list_id);
            }
            let cues = &cl.cues;
            cl.independent_runs.retain(|id| {
                cues.iter().any(|cue| cue.id() == *id && (cue.is_running() || cue.is_paused()))
            });
        }

        all_newly_completed.extend(newly_completed);

        // Group-child change detection (for cue-list-refresh event).
        if let Some(cl) = ws.cue_list_by_id(list_id) {
            let current: Vec<(CueId, Option<CueId>, bool)> = cl
                .cues
                .iter()
                .filter(|c| c.child_cues().is_some() && c.state() == CueState::Running)
                .map(|c| {
                    let active = c.active_child_id();
                    let any_running = c
                        .child_cues()
                        .map(|ch| ch.iter().any(|child| child.state() == CueState::Running))
                        .unwrap_or(false);
                    (c.id(), active, any_running)
                })
                .collect();
            for (id, active, any_running) in &current {
                let (prev_active, prev_running) =
                    prev_group_state.get(id).copied().unwrap_or((None, false));
                if *active != prev_active || *any_running != prev_running {
                    group_child_changed = true;
                }
            }
            for (id, active, any_running) in current {
                prev_group_state.insert(id, (active, any_running));
            }
        }
    }

    // 9. Dispatch an Enter handoff only while its exact successor is on
    // standby. One GO satisfies coincident unscoped follows on the same list.
    for (list_id, target) in enter_follows {
        if let Some(cl) = ws.cue_list_by_id_mut(list_id) {
            if cl.playhead_cue_id == Some(target) && cl.get(&target).is_some_and(|cue| !cue.is_disabled()) {
                should_go_lists.remove(&list_id);
                let mut transport = Transport::new(tick_ctx.clone());
                if let Ok(result) = transport.go_automatically(cl) {
                    cue_prepared |= result.prepared;
                    all_go_triggered.extend(result.triggered);
                    all_go_stopped.extend(result.stopped);
                }
            }
            if list_id == active_list_id {
                all_seq_group_playheads.push(cl.playhead_cue_id);
            }
        }
    }
    for &list_id in &should_go_lists {
        if let Some(cl) = ws.cue_list_by_id_mut(list_id) {
            let context = tick_ctx.clone();
            let mut transport = Transport::new(context);
            if let Ok(result) = transport.go_automatically(cl) {
                cue_prepared |= result.prepared;
                all_go_triggered.extend(result.triggered);
                all_go_stopped.extend(result.stopped);
            }
        }
    }

    // 9b. Dispatch the new marks kept after the clock barriers.
    let mut transport = Transport::new(tick_ctx.clone());
    for &(list_id, cue_id, elapsed) in &timecode.join {
        if let Some(list) = ws.cue_list_by_id_mut(list_id) {
            match transport.join_from_timecode(list, cue_id, elapsed) {
                Ok(result) => {
                    cue_prepared |= result.prepared;
                    all_go_triggered.extend(result.triggered);
                    all_go_stopped.extend(result.stopped);
                }
                Err(error) => log::warn!("Could not enter cue {cue_id} at the new clock position: {error}"),
            }
        }
    }
    for &(list_id, cue_id) in &timecode.fire {
        if let Some(list) = ws.cue_list_by_id_mut(list_id) {
            if let Ok(result) = transport.go_from_timecode(list, &cue_id) {
                cue_prepared |= result.prepared;
                all_go_triggered.extend(result.triggered);
                all_go_stopped.extend(result.stopped);
            }
        }
    }

    let mut msc_changed = false;
    for message in midi_messages {
        match message {
            MidiDeskInput::Voice(bytes) => {
                for list in &mut ws.cue_lists {
                    for id in list.midi_triggers_matching(&bytes) {
                        if let Ok(result) = transport.go_by_id(list, &id) {
                            cue_prepared |= result.prepared;
                            all_go_triggered.extend(result.triggered);
                            all_go_stopped.extend(result.stopped);
                        }
                    }
                }
            }
            MidiDeskInput::ShowControl(command) => {
                let protection = ws.preferences.general.double_go_protection_ms;
                let result = super::msc_transport::apply(&mut transport, &mut ws, command, || {
                    handle.try_state::<crate::state::AppState>().is_some_and(|state| {
                        crate::commands::transport_cmds::accept_chained_go(&state, protection)
                    })
                });
                match result {
                    Ok(changed) => msc_changed |= changed,
                    Err(error) => {
                        msc_changed = true;
                        log::warn!("MIDI Show Control action failed: {error}");
                    }
                }
            }
        }
    }

    if ws.cue_lists.iter().any(|list| list.wall_clock_session.has_armed()) {
        let now = super::wall_clock::Observation::now();
        let transport = Transport::new(tick_ctx.clone());
        for list in &mut ws.cue_lists {
            for cue_id in list.collect_wall_clock(now) {
                if !now.is_fresh() {
                    continue;
                }
                let alert_id = format!("wall-clock-{cue_id}");
                match transport.start_independent(list, cue_id) {
                    Ok(result) => {
                        crate::health::clear(&alert_id);
                        cue_prepared |= result.prepared;
                        all_go_triggered.extend(result.triggered);
                        all_go_stopped.extend(result.stopped);
                    }
                    Err(error) => crate::health::set(crate::health::HealthAlert::new(
                        &alert_id, crate::health::HealthLevel::Warning,
                        format!("Wall clock could not start cue {cue_id}: {error}"),
                    )),
                }
            }
        }
    }

    // Capture final playhead for the GO event (active list only, matching QLab).
    let go_final_playhead: Option<CueId> = if !all_go_triggered.is_empty() {
        ws.active_cue_list().and_then(|cl| cl.playhead_cue_id)
    } else {
        None
    };

    // ------------------------------------------------------------------
    // 10. Detect running-cue-set / playhead changes for OSC feedback
    //     (active cue list only — matches QLab OSC behavior).
    //
    // This is the ONLY consumer of the per-tick cue-list flatten + fingerprint,
    // which is O(cues) and, in a large show (200+ cues), a real 30 fps drag.
    // Skip it entirely unless OSC feedback is on (or a one-shot refresh was
    // requested) — the common case pays nothing.
    // ------------------------------------------------------------------
    // Non-consuming peek — the inner block still consumes the flags when it sends.
    let osc_feedback_active = crate::engine::osc_feedback::is_enabled()
        || crate::engine::osc_feedback::any_request_pending();
    let (running_payload, progress_payload, playhead_payload, cue_list_payload) =
        if !osc_feedback_active {
            (None, None, None, None)
        } else if let Some(active_cl) = ws.cue_list_by_id(active_list_id) {
            let running_now = all_running_cues_info(&active_cl.cues, audio_engine, output_engine);
            let playhead_now = active_cl
                .playhead_cue_id
                .and_then(|ph_id| find_cue_info(&active_cl.cues, ph_id));

            // Media progress: a continuous stream (rate-gated), unlike the
            // change-driven payloads below.  Same slot order as send_running.
            let progress_p: Option<Vec<crate::engine::osc_feedback::ProgressSlot>> =
                if crate::engine::osc_feedback::progress_due() {
                    Some(running_now.iter().map(|(_, _, _, p)| *p).collect())
                } else {
                    None
                };

            let running_ids: Vec<CueId> = running_now.iter().map(|(id, _, _, _)| *id).collect();
            let running_p: Option<Vec<(String, String)>> = if running_ids != *prev_running_cues {
                *prev_running_cues = running_ids;
                Some(running_now.into_iter().map(|(_, n, name, _)| (n, name)).collect())
            } else {
                None
            };

            let playhead_p: Option<(String, String)> = {
                let id = playhead_now.as_ref().map(|(id, _, _)| *id);
                if id != *prev_playhead_cue || crate::engine::osc_feedback::is_playhead_requested() {
                    *prev_playhead_cue = id;
                    Some(playhead_now.map(|(_, n, name)| (n, name)).unwrap_or_default())
                } else {
                    None
                }
            };

            let all_cues = all_cues_flat(&active_cl.cues);
            let cue_list_hash = fingerprint_cue_list(&all_cues);
            let cue_list_p: Option<Vec<(String, String)>> =
                if cue_list_hash != *prev_cue_list_hash
                    || crate::engine::osc_feedback::is_cue_list_requested()
                {
                    *prev_cue_list_hash = cue_list_hash;
                    Some(all_cues)
                } else {
                    None
                };

            (running_p, progress_p, playhead_p, cue_list_p)
        } else {
            (None, None, None, None)
        };

    // Rename for clarity in the rest of the function.
    let newly_completed   = all_newly_completed;
    let time_snapshots    = all_time_snapshots;
    let go_triggered      = all_go_triggered;
    let go_stopped        = all_go_stopped;
    let workspace_id      = ws.instance_id;
    let (monitor_playheads_changed, nested_rows_changed) =
        prev_monitor_structure.update(&ws, &time_snapshots);
    let cue_projection_changed = ws.cue_starts.lock().is_ok_and(|mut starts| starts.take_changed());

    drop(ws);

    if let Some(cues) = running_payload {
        crate::engine::osc_feedback::send_running(&cues);
    }
    if let Some(slots) = progress_payload {
        crate::engine::osc_feedback::send_progress(&slots);
    }
    if let Some((number, name)) = playhead_payload {
        crate::engine::osc_feedback::send_playhead(&number, &name);
    }
    if let Some(cues) = cue_list_payload {
        crate::engine::osc_feedback::send_cue_list(&cues);
    }

    // ------------------------------------------------------------------
    // 11. Emit all events.
    // ------------------------------------------------------------------

    match tc_status {
        Some(TcEvent::Position(pos, _)) | Some(TcEvent::Located(pos)) => {
            let _ = handle.emit("timecode", serde_json::json!({
                "h": pos.hours, "m": pos.minutes, "s": pos.seconds, "f": pos.frames,
                "rate": pos.rate.to_string(),
                "running": matches!(tc_status, Some(TcEvent::Position(..))),
            }));
        }
        Some(TcEvent::Stopped) => {
            let _ = handle.emit("timecode-stopped", serde_json::json!({}));
        }
        None => {}
    }

    for (cue_id, old, new) in clock_changes {
        let _ = handle.emit("cue-state-changed", serde_json::json!({
            "cue_id": cue_id, "old_state": old, "new_state": new,
        }));
    }

    for (cue_id, _, _) in &newly_completed {
        let _ = handle.emit(
            "cue-state-changed",
            serde_json::json!({
                "cue_id": cue_id,
                "old_state": "running",
                "new_state": "standby",
            }),
        );
    }
    for (cue_id, old_state) in &exact_output_resets {
        let _ = handle.emit("cue-state-changed", serde_json::json!({
            "cue_id": cue_id, "old_state": old_state, "new_state": "standby",
        }));
    }

    // Audio-freeze pause and resume transitions.
    for cue_id in &just_paused {
        let _ = handle.emit("cue-state-changed", serde_json::json!({
            "cue_id": cue_id, "old_state": "running", "new_state": "paused",
        }));
    }
    for cue_id in &just_resumed {
        let _ = handle.emit("cue-state-changed", serde_json::json!({
            "cue_id": cue_id, "old_state": "paused", "new_state": "running",
        }));
    }

    // Cues a crossfade started once its Fade's pre-wait was over.
    for cue_id in &all_crossfade_started {
        let _ = handle.emit("cue-state-changed", serde_json::json!({
            "cue_id": cue_id, "old_state": "standby", "new_state": "running",
        }));
    }

    // Emit playhead-moved for each sequential-group completion advance.
    for new_ph in &all_seq_group_playheads {
        let _ = handle.emit("playhead-moved", serde_json::json!({ "cue_id": new_ph }));
    }

    for time in &time_snapshots {
        let _ = handle.emit(
            "cue-time-update",
            serde_json::json!({
                "cue_id": time.cue_id,
                "workspace_id": workspace_id,
                "elapsed_ms": time.elapsed_ms,
                "action_elapsed_ms": time.action_elapsed_ms,
                "remaining_ms": time.remaining_ms,
            }),
        );
    }

    if monitor_playheads_changed || nested_rows_changed {
        let _ = handle.emit("cue-monitor-structure-changed", serde_json::json!({
            "workspace_id": workspace_id,
        }));
    }

    // Refresh summary attributes that are not part of the cue-state event.
    if !msc_changed && (group_child_changed || nested_rows_changed || cue_projection_changed || cue_prepared) {
        let _ = handle.emit("cue-list-refresh", serde_json::json!({}));
    }

    for stopped_id in &go_stopped {
        let _ = handle.emit(
            "cue-state-changed",
            serde_json::json!({
                "cue_id": stopped_id,
                "old_state": "running",
                "new_state": "standby",
            }),
        );
    }

    if !go_triggered.is_empty() {
        if let Some(phid) = go_final_playhead {
            let _ = handle.emit("playhead-moved", serde_json::json!({ "cue_id": phid }));
        }
        for triggered_id in &go_triggered {
            let _ = handle.emit(
                "cue-state-changed",
                serde_json::json!({
                    "cue_id": triggered_id,
                    "old_state": "standby",
                    "new_state": "running",
                }),
            );
        }
    }

    // Refresh from the final native state after the ordinary event deltas.
    // A batch can include a GO followed by a Pause or ALL_OFF.
    if msc_changed {
        let _ = handle.emit("cue-list-refresh", serde_json::json!({}));
    }

    if emit_workspace_modified {
        let _ = handle.emit("workspace-modified", serde_json::json!({}));
    }

    // ------------------------------------------------------------------
    // 12. Garbage-collect finished audio voices.
    // ------------------------------------------------------------------
    audio_engine.gc_voices();
}

fn waits_for_auto_continue(cue: &dyn crate::cue::traits::Cue) -> bool {
    cue.uses_action_clock_for_continue() && cue.continue_mode() == ContinueMode::AutoContinue
        && !cue.is_auto_continue_fired()
}

/// Check standby against this tick's admissions and consume one-shot releases.
pub fn release_admitted_playhead(list: &mut CueList) -> bool {
    let mut should_advance = false;
    for cue in &mut list.cues {
        let admission_released = cue.take_playhead_release();
        let id = cue.id();
        if list.playhead_cue_id == Some(id) && !list.independent_runs.contains(&id) {
            should_advance = admission_released || cue.released_playhead();
        }
    }
    if should_advance {
        list.advance_playhead();
    }
    should_advance
}

fn release_and_take_enter_follow(list: &mut CueList) -> (bool, Option<CueId>) {
    let released = release_admitted_playhead(list);
    let target = list.take_released_enter_follow();
    (released, target)
}

/// Record timed follow-ons for a list and return whether a GO is due now.
pub fn schedule_list_follows(
    list: &mut CueList,
    completed: &[(CueId, ContinueMode, Duration)],
    pending: &mut HashMap<CueId, (Instant, uuid::Uuid)>,
    now: Instant,
) -> bool {
    if list.independent_pads() {
        return false;
    }
    let mut go_due = false;
    for cue in &mut list.cues {
        if cue.state() == CueState::Running && cue.continue_mode() == ContinueMode::AutoContinue
            && !cue.is_auto_continue_fired() && cue.is_action_started() && cue.action_elapsed() >= cue.post_wait()
            && !list.independent_runs.contains(&cue.id())
            && cue.group_mode() != Some(GroupMode::StartFirstAndEnter)
        {
            cue.mark_auto_continue_fired();
            go_due = true;
        }
    }
    for &(id, mode, wait) in completed {
        if mode == ContinueMode::AutoFollow && !list.independent_runs.contains(&id)
            && !list.get(&id).is_some_and(|cue| cue.group_mode() == Some(GroupMode::StartFirstAndEnter))
        {
            if wait.is_zero() {
                go_due = true;
            } else {
                pending.insert(id, (now + wait, list.id));
            }
        }
    }
    go_due
}

// ---------------------------------------------------------------------------
// Fast timer refresh (runs on its own thread at ~60 fps)
// ---------------------------------------------------------------------------

/// Runs on a dedicated thread.  Reads the current running-cue position at
/// `TIMER_TICK_MS` intervals and updates the mpv OSD timer overlay.
///
/// Using a separate thread (rather than doing it inside the 30 fps main tick)
/// lets the millisecond display update smoothly without coupling the refresh
/// rate to all the other, heavier work the main tick performs.
fn timer_refresh_loop(workspace: Arc<Mutex<Workspace>>, output_engine: Arc<OutputEngine>) {
    loop {
        std::thread::sleep(Duration::from_millis(TIMER_TICK_MS));

        // Non-blocking lock — skip this frame if a command handler holds the lock.
        let Ok(ws) = workspace.try_lock() else { continue; };

        let show     = ws.preferences.display.show_output_timer;
        let floating = ws.preferences.display.timer_floating;
        let countdn  = ws.preferences.display.timer_count_down;
        let show_ms  = ws.preferences.display.timer_show_ms;

        // Preview mode overrides live cue time — show placeholder regardless of
        // whether a cue is playing or the show_output_timer setting.
        let preview   = output_engine.get_timer_preview();
        let live_text = ws.active_cue_list()
            .and_then(|cl| first_running_timer_text(&cl.cues, countdn, show_ms));
        let text = if preview.is_some() { preview } else if show { live_text.clone() } else { None };
        drop(ws); // release workspace lock before calling into mpv

        if show && floating {
            // Floating mode: drive the Win32 window, silence the OSD.
            output_engine.set_output_timer(None);
            output_engine.update_floating_timer(text.as_deref());
        } else {
            // Normal mode: drive the OSD, clear the floating window.
            output_engine.set_output_timer(text.as_deref());
            output_engine.update_floating_timer(None);
        }
    }
}

/// Find the first running cue with time data (recursive — checks group children)
/// and format its position as a timer string.
fn first_running_timer_text(
    cues: &[Box<dyn crate::cue::traits::Cue>],
    count_down: bool,
    show_ms: bool,
) -> Option<String> {
    for cue in cues {
        if cue.state() == CueState::Running {
            let ms = if count_down {
                let remaining = cue.duration()?.as_millis()
                    .saturating_sub(cue.action_elapsed().as_millis());
                remaining as u64
            } else {
                cue.action_elapsed().as_millis() as u64
            };
            return Some(format_timer(ms, show_ms));
        }
        if let Some(children) = cue.child_cues() {
            if let Some(text) = first_running_timer_text(children, count_down, show_ms) {
                return Some(text);
            }
        }
    }
    None
}

/// Return `(id, number, name)` for the cue with the given ID (recursive lookup).
fn find_cue_info(
    cues: &[Box<dyn crate::cue::traits::Cue>],
    target: CueId,
) -> Option<(CueId, String, String)> {
    for cue in cues {
        if cue.id() == target {
            return Some((
                cue.id(),
                cue.number().unwrap_or("").to_owned(),
                cue.name().to_owned(),
            ));
        }
        if let Some(children) = cue.child_cues() {
            if let Some(found) = find_cue_info(children, target) {
                return Some(found);
            }
        }
    }
    None
}

/// Collect `(id, number, name)` for every running cue (recursive, ordered).
fn all_running_cues_info(
    cues: &[Box<dyn crate::cue::traits::Cue>],
    audio_engine: &Arc<AudioEngine>,
    output_engine: &Arc<OutputEngine>,
) -> Vec<(CueId, String, String, crate::engine::osc_feedback::ProgressSlot)> {
    let mut out = Vec::new();
    collect_running(cues, audio_engine, output_engine, &mut out);
    out
}

fn collect_running(
    cues: &[Box<dyn crate::cue::traits::Cue>],
    audio_engine: &Arc<AudioEngine>,
    output_engine: &Arc<OutputEngine>,
    out: &mut Vec<(CueId, String, String, crate::engine::osc_feedback::ProgressSlot)>,
) {
    for cue in cues {
        if cue.state() == CueState::Running {
            let elapsed = media_elapsed_ms(cue.as_ref(), audio_engine, output_engine);
            let progress = progress_values(
                elapsed,
                remaining_ms(cue.as_ref(), elapsed, audio_engine).map(|remaining| elapsed.saturating_add(remaining)),
                cue.file_duration().map(|d| d.as_millis() as u64),
            );
            out.push((
                cue.id(),
                cue.number().unwrap_or("").to_owned(),
                cue.name().to_owned(),
                progress,
            ));
        }
        if let Some(children) = cue.child_cues() {
            collect_running(children, audio_engine, output_engine, out);
        }
    }
}

/// Collect `(number, name)` for every cue in display order (recursive).
fn all_cues_flat(cues: &[Box<dyn crate::cue::traits::Cue>]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    collect_all_flat(cues, &mut out);
    out
}

fn collect_all_flat(
    cues: &[Box<dyn crate::cue::traits::Cue>],
    out: &mut Vec<(String, String)>,
) {
    for cue in cues {
        out.push((
            cue.number().unwrap_or("").to_owned(),
            cue.name().to_owned(),
        ));
        if let Some(children) = cue.child_cues() {
            collect_all_flat(children, out);
        }
    }
}

/// Cheap fingerprint of the full cue list (number + name pairs).
fn fingerprint_cue_list(cues: &[(String, String)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cues.hash(&mut h);
    h.finish()
}

fn format_timer(ms: u64, show_ms: bool) -> String {
    let total_secs = ms / 1000;
    let mins  = total_secs / 60;
    let secs  = total_secs % 60;
    if show_ms {
        let millis = ms % 1000;
        format!("{mins:02}:{secs:02}.{millis:03}")
    } else {
        format!("{mins:02}:{secs:02}")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::timecode_receiver::TcObservation;

    fn enter_boundary_list(first: Option<WaitCue>) -> (CueList, CueId, CueId, CueId, CueId) {
        use crate::cue::traits::Cue;

        let mut group = GroupCue::new();
        group.mode = GroupMode::StartFirstAndEnter;
        if let Some(first) = first {
            group.children.push(Box::new(first));
        }
        let mut tail = WaitCue::new();
        tail.set_continue_mode(ContinueMode::AutoContinue);
        let tail_id = tail.id();
        group.children.push(Box::new(tail));
        let group_id = group.id();

        let mut disarmed = WaitCue::new();
        disarmed.set_disabled(true);
        let mut successor = WaitCue::new();
        successor.set_continue_mode(ContinueMode::DoNotContinue);
        let successor_id = successor.id();
        let unrelated = WaitCue::new();
        let unrelated_id = unrelated.id();
        let mut list = CueList::new("Enter follow rehearsal");
        list.push(Box::new(group));
        list.push(Box::new(disarmed));
        list.push(Box::new(successor));
        list.push(Box::new(unrelated));
        (list, group_id, tail_id, successor_id, unrelated_id)
    }

    #[test]
    fn enter_boundary_wait_follows_exactly_once_from_initial_or_absorbed_go() {
        use crate::cue::traits::Cue;

        for use_manual_first in [false, true] {
            let mut first = WaitCue::new();
            first.set_continue_mode(ContinueMode::DoNotContinue);
            let (mut list, group, tail, successor, unrelated) =
                enter_boundary_list(use_manual_first.then_some(first));
            let (ctx, _, _) = crate::test_support::recording_context();
            let mut transport = Transport::new(ctx);
            if use_manual_first {
                assert_eq!(transport.go(&mut list).unwrap().triggered, [group]);
                assert!(!list.get_recursive(&tail).unwrap().is_running());
                assert_eq!(list.playhead_cue_id, Some(group));
            }
            assert_eq!(transport.go(&mut list).unwrap().triggered, [group, successor]);
            assert!(list.get_recursive(&tail).unwrap().is_action_started());
            assert!(list.get(&successor).unwrap().is_running());
            assert!(!list.get(&unrelated).unwrap().is_running());
            assert_eq!(list.playhead_cue_id, Some(unrelated));
            assert_eq!(release_and_take_enter_follow(&mut list), (false, None));
        }
    }

    fn advance_wait_timer(list: &mut CueList, id: CueId, seconds: u64) {
        WaitCue::from_cue_mut(list.get_mut_recursive(&id).unwrap()).unwrap()
            .advance_timer_for_test(Duration::from_secs(seconds));
    }

    #[test]
    fn enter_boundary_final_wait_follows_its_natural_end_once_with_outer_standby_released() {
        use crate::cue::traits::Cue;
        for prior in ["none", "manual", "automatic"] {
            let (ctx, _, _) = crate::test_support::recording_context();
            let mut transport = Transport::new(ctx.clone());
            let mut first = WaitCue::new();
            first.set_continue_mode(if prior == "manual" { ContinueMode::DoNotContinue } else { ContinueMode::AutoFollow });
            let first_id = first.id();
            let (mut list, group, tail, successor, unrelated) = enter_boundary_list((prior != "none").then_some(first));
            let wait = list.get_mut_recursive(&tail).unwrap();
            wait.set_continue_mode(ContinueMode::AutoFollow);
            wait.set_pre_wait(Duration::from_secs(10));
            assert_eq!(transport.go(&mut list).unwrap().triggered, [group]);
            if prior != "none" {
                assert_eq!(list.playhead_cue_id, Some(group));
                if prior == "manual" {
                    assert_eq!(transport.go(&mut list).unwrap().triggered, [group]);
                } else {
                    advance_wait_timer(&mut list, first_id, 6);
                    list.get_mut(&group).unwrap().tick(&ctx).unwrap();
                    assert_eq!(release_and_take_enter_follow(&mut list), (true, None));
                }
            }
            assert_eq!(list.playhead_cue_id, Some(successor));
            assert!(list.get_recursive(&tail).unwrap().is_running());
            assert!(!list.get(&successor).unwrap().is_running());
            advance_wait_timer(&mut list, tail, 11);
            list.get_mut(&group).unwrap().tick(&ctx).unwrap();
            assert_eq!(release_and_take_enter_follow(&mut list), (false, None));

            transport.pause_cue(&mut list, &group).unwrap();
            advance_wait_timer(&mut list, tail, 30);
            assert_eq!(release_and_take_enter_follow(&mut list), (false, None));
            transport.resume_cue(&mut list, &group).unwrap();
            list.get_mut(&group).unwrap().tick(&ctx).unwrap();
            assert_eq!(release_and_take_enter_follow(&mut list), (false, None));
            advance_wait_timer(&mut list, tail, 5);
            list.get_mut(&group).unwrap().tick(&ctx).unwrap();
            assert!(!list.get_recursive(&tail).unwrap().is_running());
            assert_eq!(release_and_take_enter_follow(&mut list), (false, Some(successor)));
            assert_eq!(transport.go_automatically(&mut list).unwrap().triggered, [successor]);
            assert_eq!(list.playhead_cue_id, Some(unrelated));
            assert_eq!(release_and_take_enter_follow(&mut list), (false, None));
        }
    }

    #[test]
    fn enter_boundary_timed_wait_cannot_follow_a_revoked_or_restarted_run() {
        for action in ["repark", "stop", "panic", "disarm", "restart-child", "early-go", "edit-order"] {
            let (ctx, _, _) = crate::test_support::recording_context();
            let mut transport = Transport::new(ctx.clone());
            let (mut list, group, tail, successor, unrelated) = enter_boundary_list(None);
            list.get_mut_recursive(&tail).unwrap().set_continue_mode(ContinueMode::AutoFollow);
            assert_eq!(transport.go(&mut list).unwrap().triggered, [group]);
            match action {
                "repark" => { list.set_playhead(Some(unrelated)).unwrap(); list.set_playhead(Some(successor)).unwrap(); }
                "stop" => transport.stop_cue(&mut list, &group).unwrap(),
                "panic" => transport.hard_stop_all(&mut list).unwrap(),
                "disarm" => list.get_mut(&successor).unwrap().set_disabled(true),
                "restart-child" => list.get_mut_recursive(&tail).unwrap().go(&ctx).unwrap(),
                "early-go" => assert_eq!(transport.go(&mut list).unwrap().triggered, [successor]),
                "edit-order" => { list.remove(&unrelated).unwrap(); }
                _ => unreachable!(),
            }
            advance_wait_timer(&mut list, tail, 10);
            list.get_mut(&group).unwrap().tick(&ctx).unwrap();
            assert_eq!(release_and_take_enter_follow(&mut list).1, None, "{action}");
        }
    }

    #[test]
    fn enter_boundary_tick_checks_navigation_target_and_stop_authority() {
        use crate::cue::traits::Cue;

        for action in ["none", "repark", "disarm", "panic"] {
            let mut first = WaitCue::new();
            first.set_continue_mode(ContinueMode::AutoContinue);
            first.set_pre_wait(Duration::from_secs(30));
            let first_id = first.id();
            let (mut list, group, tail, successor, unrelated) = enter_boundary_list(Some(first));
            let (ctx, _, _) = crate::test_support::recording_context();
            let mut transport = Transport::new(ctx.clone());
            assert_eq!(transport.go(&mut list).unwrap().triggered, [group]);
            assert_eq!(release_and_take_enter_follow(&mut list), (false, None));

            match action {
                "none" => {}
                "repark" => {
                    list.set_playhead(Some(unrelated)).unwrap();
                    list.set_playhead(Some(group)).unwrap();
                }
                "disarm" => list.get_mut(&successor).unwrap().set_disabled(true),
                "panic" => transport.hard_stop_all(&mut list).unwrap(),
                _ => unreachable!(),
            }
            list.get_mut_recursive(&first_id).unwrap().set_pre_wait(Duration::ZERO);
            list.get_mut(&group).unwrap().tick(&ctx).unwrap();
            let (released, follow) = release_and_take_enter_follow(&mut list);
            if action == "panic" {
                assert!(!released);
                assert!(!list.get_recursive(&tail).unwrap().is_running());
            } else {
                assert!(released);
                assert!(list.get_recursive(&tail).unwrap().is_running());
            }
            assert_eq!(follow, (action == "none").then_some(successor));
            if follow.is_some() {
                assert_eq!(transport.go_automatically(&mut list).unwrap().triggered, [successor]);
            }
            assert_eq!(list.get(&successor).unwrap().is_running(), action == "none");
            assert!(!list.get(&unrelated).unwrap().is_running());
            assert_eq!(release_and_take_enter_follow(&mut list), (false, None));
        }
    }

    #[test]
    fn enter_boundary_ignores_go_that_does_not_start_or_move_standby() {
        for action in ["manual", "automatic", "addressed", "paused"] {
            let (mut list, group, tail, successor, unrelated) = enter_boundary_list(None);
            let (ctx, _, _) = crate::test_support::recording_context();
            let mut transport = Transport::new(ctx.clone());
            list.get_mut(&group).unwrap().set_pre_wait(Duration::from_secs(30));
            assert_eq!(transport.go(&mut list).unwrap().triggered, [group]);

            match action {
                "manual" => assert_eq!(transport.go(&mut list).unwrap().triggered, [group]),
                "automatic" => assert!(transport.go_automatically(&mut list).is_err()),
                "addressed" => assert!(transport.go_by_id(&mut list, &group).is_err()),
                "paused" => {
                    transport.pause_cue(&mut list, &group).unwrap();
                    assert!(transport.go(&mut list).is_err());
                    transport.resume_cue(&mut list, &group).unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(list.playhead_cue_id, Some(group));
            assert!(!list.get_recursive(&tail).unwrap().is_running());
            assert!(!list.get(&successor).unwrap().is_running());

            let group_cue = list.get_mut(&group).unwrap();
            group_cue.set_pre_wait(Duration::ZERO);
            group_cue.tick(&ctx).unwrap();
            assert_eq!(release_and_take_enter_follow(&mut list), (true, Some(successor)));
            assert_eq!(transport.go_automatically(&mut list).unwrap().triggered, [successor]);
            assert_eq!(list.playhead_cue_id, Some(unrelated));
            assert_eq!(release_and_take_enter_follow(&mut list), (false, None));
        }
    }

    #[test]
    fn enter_boundary_revokes_only_committed_arming_changes_to_its_cues() {
        use crate::{
            commands::cue_cmds::{apply_cue_properties, MergePolicy},
            cue::{group_cue::GroupCueFactory, registry::CueRegistry, wait_cue::WaitCueFactory},
        };
        use serde_json::json;

        let mut registry = CueRegistry::new();
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        registry.register(CueType::Wait, Box::new(WaitCueFactory));
        for edit in ["group", "final-wait", "successor", "group-child", "unrelated", "no-op", "rejected"] {
            let (list, group, tail, successor, unrelated) = enter_boundary_list(None);
            let mut workspace = Workspace::new("Enter arming rehearsal");
            workspace.active_cue_list_id = list.id;
            workspace.cue_lists = vec![list];
            let list = workspace.active_cue_list_mut().unwrap();
            let (ctx, _, _) = crate::test_support::recording_context();
            let mut transport = Transport::new(ctx.clone());
            list.get_mut(&group).unwrap().set_pre_wait(Duration::from_secs(30));
            assert_eq!(transport.go(list).unwrap().triggered, [group]);

            let target = match edit {
                "group" | "group-child" => group,
                "final-wait" => tail,
                "unrelated" => unrelated,
                _ => successor,
            };
            // Change arming back before the boundary. Its final value alone
            // cannot reveal that the original authority was revoked.
            for disabled in [true, false] {
                let properties = match edit {
                    "group-child" => {
                        let mut children = workspace.active_cue_list().unwrap().get(&group).unwrap()
                            .serialize()["children"].clone();
                        children[0]["is_disabled"] = json!(disabled);
                        json!({ "children": children })
                    }
                    "no-op" => json!({ "is_disabled": false, "name": "Edited successor" }),
                    "rejected" => json!({ "is_disabled": true, "id": uuid::Uuid::new_v4() }),
                    _ => json!({ "is_disabled": disabled }),
                };
                let policy = if edit == "group-child" { MergePolicy::AllKeys } else { MergePolicy::KnownKeysOnly };
                let result = apply_cue_properties(
                    ctx.audio_engine.as_ref(), ctx.output_engine.as_ref(), &registry,
                    &mut workspace, target, &properties, policy,
                );
                if edit == "rejected" {
                    assert!(result.is_err());
                } else {
                    assert!(result.unwrap());
                }
            }

            let list = workspace.active_cue_list_mut().unwrap();
            let group_cue = list.get_mut(&group).unwrap();
            group_cue.set_pre_wait(Duration::ZERO);
            group_cue.tick(&ctx).unwrap();
            let should_follow = matches!(edit, "unrelated" | "no-op" | "rejected");
            assert_eq!(release_and_take_enter_follow(list), (true, should_follow.then_some(successor)), "{edit}");
            assert!(list.get_recursive(&tail).unwrap().is_running());
            assert!(!list.get(&successor).unwrap().is_running());
        }
    }

    #[test]
    fn enter_boundary_wait_requires_main_sequential_go() {
        let (ctx, _, _) = crate::test_support::recording_context();
        let mut transport = Transport::new(ctx.clone());
        let (mut list, group, tail, successor, _) = enter_boundary_list(None);
        assert!(list.get_mut(&group).unwrap().go(&ctx).is_err());
        assert!(transport.go_by_id(&mut list, &group).is_err());
        assert!(transport.go_automatically(&mut list).is_err());
        let independent = transport.start_independent(&mut list, group).unwrap();
        assert!(independent.triggered.is_empty());
        assert!(!list.get_recursive(&tail).unwrap().is_running());
        assert!(!list.get(&successor).unwrap().is_running());
    }

    #[test]
    fn enter_boundary_post_wait_dispatches_once_or_drops_the_captured_successor() {
        for (post, cancel) in [(2, false), (8, false), (8, true)] {
            let (ctx, _, _) = crate::test_support::recording_context();
            let mut transport = Transport::new(ctx.clone());
            let (mut list, group, tail, successor, unrelated) = enter_boundary_list(None);
            list.get_mut_recursive(&tail).unwrap().set_post_wait(Duration::from_secs(post));
            assert_eq!(transport.go(&mut list).unwrap().triggered, [group]);
            assert_eq!(list.playhead_cue_id, Some(successor));
            assert_eq!(release_and_take_enter_follow(&mut list).1, None);
            if cancel {
                advance_wait_timer(&mut list, tail, 6);
                list.get_mut(&group).unwrap().tick(&ctx).unwrap();
                assert_eq!(release_and_take_enter_follow(&mut list).1, None);
                assert_eq!(list.released_enter_group(), Some(group));
                list.set_playhead(Some(unrelated)).unwrap();
                list.set_playhead(Some(successor)).unwrap();
                assert_eq!(list.released_enter_group(), None);
                assert_eq!(release_and_take_enter_follow(&mut list).1, None);
            } else {
                advance_wait_timer(&mut list, tail, post);
                list.get_mut(&group).unwrap().tick(&ctx).unwrap();
                assert_eq!(list.get_recursive(&tail).unwrap().is_running(), post < 5);
                assert_eq!(release_and_take_enter_follow(&mut list), (false, Some(successor)));
                assert_eq!(transport.go_automatically(&mut list).unwrap().triggered, [successor]);
                assert_eq!(list.playhead_cue_id, Some(unrelated));
                assert_eq!(release_and_take_enter_follow(&mut list).1, None);
            }
        }
    }

    #[test]
    fn enter_boundary_auto_follow_waits_past_the_natural_end_for_the_exact_standby() {
        for (action_seconds, repark) in [(6, false), (9, false), (6, true)] {
            let (ctx, _, _) = crate::test_support::recording_context();
            let mut transport = Transport::new(ctx.clone());
            let (mut list, group, tail, successor, unrelated) = enter_boundary_list(None);
            let wait = list.get_mut_recursive(&tail).unwrap();
            wait.set_continue_mode(ContinueMode::AutoFollow);
            wait.set_pre_wait(Duration::from_secs(10));
            wait.set_post_wait(Duration::from_secs(4));
            assert_eq!(transport.go(&mut list).unwrap().triggered, [group]);
            assert_eq!(list.playhead_cue_id, Some(successor));
            advance_wait_timer(&mut list, tail, 10 + action_seconds);
            list.get_mut(&group).unwrap().tick(&ctx).unwrap();
            assert_eq!(list.get_recursive(&tail).unwrap().state(), CueState::Standby);
            if action_seconds == 6 {
                assert_eq!(release_and_take_enter_follow(&mut list).1, None);
                assert_eq!(list.released_enter_group(), Some(group));
                if repark {
                    list.set_playhead(Some(unrelated)).unwrap();
                    list.set_playhead(Some(successor)).unwrap();
                    assert_eq!(list.released_enter_group(), None);
                }
            } else {
                assert_eq!(release_and_take_enter_follow(&mut list).1, Some(successor));
                assert_eq!(transport.go_automatically(&mut list).unwrap().triggered, [successor]);
                assert_eq!(list.playhead_cue_id, Some(unrelated));
            }
            assert_eq!(release_and_take_enter_follow(&mut list).1, None);
        }
    }

    #[test]
    fn enter_boundary_auto_continue_with_pre_wait_remains_review_only() {
        let (ctx, _, _) = crate::test_support::recording_context();
        let mut transport = Transport::new(ctx);
        let (mut list, group, tail, successor, _) = enter_boundary_list(None);
        list.get_mut_recursive(&tail).unwrap().set_pre_wait(Duration::from_secs(1));
        assert!(transport.go(&mut list).is_err());
        assert_eq!(list.playhead_cue_id, Some(group));
        assert!(!list.get_recursive(&tail).unwrap().is_running());
        assert!(!list.get(&successor).unwrap().is_running());
    }

    #[test]
    fn enter_boundary_timed_wait_requires_positive_duration_and_main_list_go() {
        use crate::cue::{traits::CueFactory, wait_cue::WaitCueFactory};
        for (duration, post) in [(0, 0), (100, 1), (100, 0)] {
            let (ctx, _, _) = crate::test_support::recording_context();
            let mut transport = Transport::new(ctx.clone());
            let (mut list, group, tail, successor, _) = enter_boundary_list(None);
            let wait = WaitCueFactory.from_json(serde_json::json!({
                "id": tail, "wait_duration_ms": duration, "post_wait_ms": post, "continue_mode": "auto_follow"
            })).unwrap();
            assert!(list.replace_cue_recursive(&tail, wait));
            assert!(list.get_mut(&group).unwrap().go(&ctx).is_err());
            assert!(transport.go_by_id(&mut list, &group).is_err());
            assert!(transport.go_automatically(&mut list).is_err());
            assert!(transport.start_independent(&mut list, group).unwrap().triggered.is_empty());
            if duration == 0 {
                assert!(transport.go(&mut list).is_err());
                assert!(!list.get_recursive(&tail).unwrap().is_running());
            } else {
                assert_eq!(transport.go(&mut list).unwrap().triggered, [group]);
            }
            assert!(!list.get(&successor).unwrap().is_running());
        }
    }

    fn received(event: TcEvent) -> TcObservation {
        TcObservation { event, at: Instant::now() }
    }

    #[test]
    fn playback_projection_distinguishes_overlapping_children_from_other_list_playhead() {
        let mut ws = Workspace::new("Rehearsal");
        ws.cue_lists.push(CueList::new("Auxiliary"));
        let first = uuid::Uuid::new_v4();
        let second = uuid::Uuid::new_v4();
        let sample = |cue_id, action_elapsed_ms| CueTimeSnapshot {
            cue_id,
            elapsed_ms: action_elapsed_ms,
            action_elapsed_ms,
            remaining_ms: None,
            nested_state: Some(CueState::Running),
        };
        let mut previous = MonitorStructure::default();
        assert_eq!(previous.update(&ws, &[sample(first, 0), sample(second, 0)]), (true, true));
        assert_eq!(previous.update(&ws, &[sample(first, 100), sample(second, 100)]), (false, false));
        assert_eq!(previous.update(&ws, &[sample(second, 200)]), (false, true));
        assert_eq!(previous.update(&ws, &[sample(second, 300)]), (false, false));

        ws.cue_lists[1].playhead_cue_id = Some(first);
        assert_eq!(previous.update(&ws, &[sample(second, 400)]), (true, false));
        assert_eq!(previous.update(&ws, &[sample(second, 500)]), (false, false));
        let paused = CueTimeSnapshot {
            nested_state: Some(CueState::Paused),
            ..sample(second, 500)
        };
        assert_eq!(previous.update(&ws, &[paused]), (false, true));
    }

    #[test]
    fn timecode_dispatch_switches_receivers_and_preserves_a_locate_in_a_batch() {
        use crate::cue::{memo_cue::MemoCue, traits::Cue};
        use crate::engine::timecode_types::{TcRate, TcTrigger};

        let mut list = CueList::new("Timecode marks");
        list.tc_config.enabled = true;
        list.tc_config.rate = TcRate::Fps25;
        let at = |frame| TcPosition::from_frame_number(frame, TcRate::Fps25);
        let mut marks = Vec::new();
        for frame in [101, 103, 104, 106] {
            let cue = Box::new(MemoCue::new());
            marks.push(cue.id());
            list.tc_triggers.insert(cue.id(), TcTrigger {
                position: at(frame),
                real_time: false,
            });
            list.push(cue);
        }
        let id = list.id;
        let lists = [list];
        let mut dispatch = TimecodeDispatch::default();
        let empty = dispatch.collect(None, &lists, Instant::now());
        assert_eq!(empty.status, None);
        assert!(empty.fire.is_empty());

        let (initial, initial_tx) = TimecodeReceiver::test_channel();
        initial_tx.send(received(TcEvent::Position(at(100), 2))).unwrap();
        assert!(dispatch.collect(Some(&initial), &lists, Instant::now()).fire.is_empty());
        initial_tx.send(received(TcEvent::Position(at(101), 2))).unwrap();

        let (replacement, tx) = TimecodeReceiver::test_channel();
        tx.send(received(TcEvent::Position(at(102), 2))).unwrap();
        let changed = dispatch.collect(Some(&replacement), &lists, Instant::now());
        assert!(changed.detached);
        assert!(changed.fire.is_empty());
        assert!(changed.edges.is_empty());
        tx.send(received(TcEvent::Located(at(103)))).unwrap();
        tx.send(received(TcEvent::Position(at(104), 2))).unwrap();
        tx.send(received(TcEvent::Position(at(106), 2))).unwrap();
        let batch = dispatch.collect(Some(&replacement), &lists, Instant::now());
        assert_eq!(batch.status, Some(TcEvent::Position(at(106), 2)));
        assert_eq!(batch.fire, vec![(id, marks[2]), (id, marks[3])]);
        assert!(batch.edges.is_empty());
        let removed = dispatch.collect(None, &lists, Instant::now());
        assert_eq!(removed.status, Some(TcEvent::Stopped));
        assert!(removed.detached);
        assert!(removed.fire.is_empty());
        assert!(removed.edges.is_empty());
    }

    #[test]
    fn timecode_positioned_video_starts_picture_and_audio_at_the_same_source_offset() {
        use crate::{cue::video_cue::VideoCue, test_support::EngineCall};

        let (mut context, _, calls) = crate::test_support::recording_context();
        let frames = crate::test_support::record_audio_launch_positions(&mut context, &calls);
        let transport = Transport::new(context);
        let mut list = CueList::new("Rehearsal video");
        let standby = WaitCue::new();
        let parked = standby.id();
        list.push(Box::new(standby));
        let mut video = VideoCue::new();
        video.file_path = Some("show.mov".into());
        video.set_runtime_duration(Duration::from_secs(8));
        video.set_continue_mode(ContinueMode::DoNotContinue);
        video.start_time = Some(Duration::from_secs(1));
        video.end_time = Some(Duration::from_secs(5));
        video.fade_out = Some(crate::cue::types::FadeSpec {
            duration_ms: 2_000, curve: crate::cue::types::FadeCurve::Linear,
        });
        let id = video.id();
        list.push(Box::new(video));
        list.set_playhead(Some(parked)).unwrap();
        assert!(transport.join_from_timecode(&mut list, id, Duration::from_secs(3)).unwrap().triggered.is_empty());
        list.get_mut(&id).unwrap().accept_preloaded_audio(Arc::new(vec![0.0; 384_000]), 1, 48_000, Duration::from_secs(8));
        assert!(transport.join_from_timecode(&mut list, id, Duration::from_secs(4)).unwrap().triggered.is_empty());
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(transport.join_from_timecode(&mut list, id, Duration::from_secs(3)).unwrap().triggered, [id]);
        assert_eq!(list.playhead_cue_id, Some(parked));
        assert_eq!(*frames.lock().unwrap(), [192_000]);
        // Before the output reveals its first frame, even an entry inside an
        // authored end fade must leave the paired native voice paused.
        list.get_mut(&id).unwrap().tick(&transport.context).unwrap();
        assert!(matches!(calls.lock().unwrap().as_slice(), [
            EngineCall::AudioPlayPausedRouted { .. },
            EngineCall::OutputShowContent { start_ms: Some(4000), positioned: true, .. },
        ]));
    }

    #[test]
    fn timecode_lookback_joins_at_real_offsets_and_keeps_standby_parked() {
        use crate::cue::audio_cue::AudioCue;
        use crate::engine::timecode_types::{TcOnStart, TcTrigger};

        let (mut context, _, calls) = crate::test_support::recording_context();
        let launched = crate::test_support::record_audio_launch_positions(&mut context, &calls);
        let transport = Transport::new(context.clone());
        let mut list = CueList::new("Rehearsal locate");
        list.tc_config.enabled = true;
        list.tc_config.rate = TcRate::Fps25;
        list.tc_config.on_start = TcOnStart::JoinActive;
        list.tc_config.on_stop = TcOnStop::Pause;
        let at = |frame| TcPosition::from_frame_number(frame, TcRate::Fps25);
        let mut put = |mut cue: Box<dyn Cue>, frame| {
            cue.set_continue_mode(ContinueMode::DoNotContinue);
            let id = cue.id();
            list.tc_triggers.insert(id, TcTrigger { position: at(frame), real_time: false });
            list.push(cue);
            id
        };
        let expired = put(Box::new(WaitCue::new()), 0);
        let ongoing = put(Box::new(WaitCue::new()), 75);
        let mut audio = AudioCue::new();
        audio.start_time = Some(Duration::from_secs(2));
        audio.end_time = Some(Duration::from_secs(8));
        audio.accept_preloaded_audio(Arc::new(vec![0.0; 480_000]), 1, 48_000, Duration::from_secs(10));
        let audio_id = put(Box::new(audio), 50);
        let exact = put(Box::new(WaitCue::new()), 150);
        let mut short = AudioCue::new();
        short.end_time = Some(Duration::from_micros(1_500));
        short.accept_preloaded_audio(Arc::new(vec![0.0; 96]), 1, 48_000, Duration::from_millis(2));
        let short_id = put(Box::new(short), 151);
        assert!(transport.join_from_timecode(&mut list, short_id, Duration::from_micros(1_700))
            .unwrap().triggered.is_empty());
        list.set_playhead(Some(expired)).unwrap();
        let list_id = list.id;
        let (receiver, tx) = TimecodeReceiver::test_channel();
        let mut dispatch = TimecodeDispatch::default();
        for event in [TcEvent::Position(at(24), 2), TcEvent::Located(at(150))] {
            tx.send(received(event)).unwrap();
        }
        let located = dispatch.collect(Some(&receiver), std::slice::from_ref(&list), Instant::now());
        assert!(located.join.is_empty() && located.fire.is_empty());
        tx.send(received(TcEvent::Position(at(150), 2))).unwrap();
        let batch = dispatch.collect(Some(&receiver), std::slice::from_ref(&list), Instant::now());
        assert_eq!(batch.fire, [(list_id, exact)]);
        assert_eq!(batch.join, [
            (list_id, expired, Duration::from_secs(6)),
            (list_id, audio_id, Duration::from_secs(4)),
            (list_id, ongoing, Duration::from_secs(3)),
        ]);
        for (_, cue, elapsed) in batch.join {
            transport.join_from_timecode(&mut list, cue, elapsed).unwrap();
        }
        assert_eq!(list.playhead_cue_id, Some(expired));
        assert_eq!(list.get(&expired).unwrap().state(), CueState::Standby);
        assert_eq!(*launched.lock().unwrap(), [6 * 48_000]);
        let elapsed = list.get(&ongoing).unwrap().elapsed();
        assert!((Duration::from_secs(3)..Duration::from_secs(4)).contains(&elapsed));

        tx.send(received(TcEvent::Position(at(152), 2))).unwrap();
        assert!(dispatch.collect(Some(&receiver), std::slice::from_ref(&list), Instant::now()).join.is_empty());
        let mut lost = TimecodeBatch {
            edges: vec![(list_id, ClockEdge::Lost(Instant::now()))], ..Default::default()
        };
        assert_eq!(lost.apply_existing_runs(std::slice::from_mut(&mut list), &context, false, &mut Default::default()),
            [(ongoing, CueState::Running, CueState::Paused), (audio_id, CueState::Running, CueState::Paused)]);
    }

    #[test]
    fn timecode_override_monitors_frames_and_restores_only_fresh_clock_ownership() {
        use crate::engine::timecode_types::TcTrigger;
        let (context, _, _) = crate::test_support::recording_context();
        let mut transport = Transport::new(context.clone());
        let mut list = CueList::new("Clock override");
        list.tc_config.enabled = true;
        list.tc_config.rate = TcRate::Fps25;
        list.tc_config.freewheel_ms = 200;
        list.tc_config.on_stop = TcOnStop::Pause;
        let pos = |frame| TcPosition::from_frame_number(frame, TcRate::Fps25);
        let mut marks = Vec::new();
        for frame in [20, 22, 23, 24, 25, 26] {
            let wait = WaitCue::new();
            marks.push((list.id, wait.id()));
            list.tc_triggers.insert(wait.id(), TcTrigger { position: pos(frame), real_time: false });
            list.push(Box::new(wait));
        }
        let mut lists = [list];
        let (receiver, tx) = TimecodeReceiver::test_channel();
        let mut dispatch = TimecodeDispatch::default();
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let send = |frame, ms| {
            tx.send(TcObservation { event: TcEvent::Position(pos(frame), 2), at: at(ms) }).unwrap();
        };

        send(20, 0);
        let mut first = dispatch.collect(Some(&receiver), &lists, start);
        first.apply_existing_runs(&mut lists, &context, false, &mut Default::default());
        assert_eq!(first.fire, [marks[0]]);
        transport.go_from_timecode(&mut lists[0], &marks[0].1).unwrap();
        dispatch.set_suspended(true, &mut lists, at(10));
        send(22, 400);
        let muted = dispatch.collect(Some(&receiver), &lists, at(900));
        assert_eq!(muted.status, Some(TcEvent::Position(pos(22), 2)));
        assert!(muted.fire.is_empty() && muted.edges.is_empty());

        // The desk was busy: mark 23 is still queued at the instant of restore.
        send(23, 950);
        dispatch.set_suspended(false, &mut lists, at(1000));
        send(24, 1040);
        send(25, 1040); // another decoded position from the same capture block
        send(26, 1120);
        let resumed = dispatch.collect(Some(&receiver), &lists, at(1120));
        assert_eq!(resumed.fire, [marks[5]]);
        assert!(resumed.edges.is_empty());
        transport.go_from_timecode(&mut lists[0], &marks[5].1).unwrap();
        let mut loss = dispatch.collect(Some(&receiver), &lists, at(1400));
        assert_eq!(loss.apply_existing_runs(&mut lists, &context, false, &mut Default::default()), [(marks[5].1, CueState::Running, CueState::Paused)]);
        assert!(lists[0].get(&marks[0].1).unwrap().is_running());
    }

    #[test]
    fn timecode_dispatch_cancels_pending_marks_before_a_stop_or_locate() {
        use crate::cue::{memo_cue::MemoCue, traits::Cue};
        use crate::engine::timecode_types::{TcRate, TcTrigger};

        let mut list = CueList::new("Timecode marks");
        list.tc_config.enabled = true;
        list.tc_config.rate = TcRate::Fps25;
        let at = |frame| TcPosition::from_frame_number(frame, TcRate::Fps25);
        let mut marks = Vec::new();
        for frame in [101, 102, 104, 105, 106] {
            let cue = Box::new(MemoCue::new());
            marks.push(cue.id());
            list.tc_triggers.insert(cue.id(), TcTrigger {
                position: at(frame),
                real_time: false,
            });
            list.push(cue);
        }
        let id = list.id;
        let lists = [list];

        for barrier in [TcEvent::Stopped, TcEvent::Located(at(103))] {
            let mut dispatch = TimecodeDispatch::default();
            let (receiver, tx) = TimecodeReceiver::test_channel();
            for event in [
                TcEvent::Position(at(100), 2),
                TcEvent::Position(at(102), 2),
                barrier,
                TcEvent::Position(at(104), 2),
                TcEvent::Position(at(106), 2),
            ] {
                tx.send(received(event)).unwrap();
            }

            let batch = dispatch.collect(Some(&receiver), &lists, Instant::now());
            assert_eq!(batch.status, Some(TcEvent::Position(at(106), 2)));
            assert_eq!(
                batch.fire, vec![(id, marks[2]), (id, marks[3]), (id, marks[4])],
                "{barrier:?} should discard pending marks from before the clock changed",
            );
            assert!(batch.edges.is_empty());
        }
    }

    #[test]
    fn timecode_dispatch_keeps_ordered_list_loss_edges_separate_from_locate() {
        let mut short = CueList::new("Short hold");
        short.tc_config.enabled = true;
        short.tc_config.rate = TcRate::Fps25;
        short.tc_config.freewheel_ms = 200;
        short.tc_config.on_stop = TcOnStop::Pause;
        let mut long = CueList::new("Long hold");
        long.tc_config = short.tc_config.clone();
        long.tc_config.freewheel_ms = 1200;
        let short_id = short.id;
        let long_id = long.id;
        let lists = [short, long];
        let mut dispatch = TimecodeDispatch::default();
        let (receiver, tx) = TimecodeReceiver::test_channel();
        let start = Instant::now();
        let position = |frame| TcPosition::from_frame_number(frame, TcRate::Fps25);
        let sample = |event, offset| TcObservation { event, at: start + Duration::from_millis(offset) };
        tx.send(sample(TcEvent::Position(position(20), 2), 0)).unwrap();
        tx.send(sample(TcEvent::Position(position(22), 2), 375)).unwrap();
        let batch = dispatch.collect(Some(&receiver), &lists, start + Duration::from_millis(375));
        assert_eq!(batch.edges, vec![
            (short_id, ClockEdge::Lost(start + Duration::from_millis(200))),
            (short_id, ClockEdge::Returned(start + Duration::from_millis(375))),
        ]);

        tx.send(sample(TcEvent::Located(position(300)), 460)).unwrap();
        let located = dispatch.collect(Some(&receiver), &lists, start + Duration::from_secs(2));
        assert!(located.edges.is_empty());
        tx.send(sample(TcEvent::Position(position(302), 2), 2100)).unwrap();
        let waiting = dispatch.collect(Some(&receiver), &lists, start + Duration::from_millis(3100));
        assert_eq!(waiting.edges, vec![(short_id, ClockEdge::Lost(start + Duration::from_millis(2300)))]);
        let expired = dispatch.collect(Some(&receiver), &lists, start + Duration::from_millis(3300));
        assert_eq!(expired.edges, vec![(long_id, ClockEdge::Lost(start + Duration::from_millis(3300)))]);
    }

    #[test]
    fn buffered_locate_preserves_loss_due_before_it_and_suspends_later_deadlines() {
        let mut short = CueList::new("Short hold");
        short.tc_config.enabled = true;
        short.tc_config.rate = TcRate::Fps25;
        short.tc_config.freewheel_ms = 200;
        short.tc_config.on_stop = TcOnStop::Pause;
        let mut long = CueList::new("Long hold");
        long.tc_config = short.tc_config.clone();
        long.tc_config.freewheel_ms = 1200;
        let short_id = short.id;
        let lists = [short, long];
        let mut dispatch = TimecodeDispatch::default();
        let (receiver, tx) = TimecodeReceiver::test_channel();
        let start = Instant::now();
        let position = |frame| TcPosition::from_frame_number(frame, TcRate::Fps25);
        let locate = TcEvent::Located(position(300));
        tx.send(TcObservation { event: TcEvent::Position(position(20), 2), at: start }).unwrap();
        tx.send(TcObservation { event: locate, at: start + Duration::from_millis(350) }).unwrap();

        let batch = dispatch.collect(Some(&receiver), &lists, start + Duration::from_secs(2));
        assert_eq!(batch.status, Some(locate));
        assert_eq!(batch.edges, vec![(short_id, ClockEdge::Lost(start + Duration::from_millis(200)))]);
        assert!(batch.fire.is_empty());

        let resumed = start + Duration::from_millis(2100);
        tx.send(TcObservation { event: TcEvent::Position(position(302), 2), at: resumed }).unwrap();
        let batch = dispatch.collect(Some(&receiver), &lists, resumed);
        assert_eq!(batch.edges, vec![(short_id, ClockEdge::Returned(resumed))]);
    }

    #[test]
    fn timecode_loss_drops_pending_wait_marks_and_keeps_later_motion() {
        use crate::engine::timecode_types::TcTrigger;

        let start = Instant::now();
        let pos = |frame| TcPosition::from_frame_number(frame, TcRate::Fps25);
        let make_list = |policy| {
            let mut list = CueList::new("Clocked waits");
            list.tc_config.enabled = true;
            list.tc_config.rate = TcRate::Fps25;
            list.tc_config.freewheel_ms = 200;
            list.tc_config.on_stop = policy;
            let marks: Vec<_> = [20, 30, 32].into_iter().map(|frame| {
                let wait = WaitCue::new();
                let id = wait.id();
                list.tc_triggers.insert(id, TcTrigger { position: pos(frame), real_time: false });
                list.push(Box::new(wait));
                (list.id, id)
            }).collect();
            (list, marks)
        };
        let (paused, pause_marks) = make_list(TcOnStop::Pause);
        let (continued, continue_marks) = make_list(TcOnStop::Continue);
        let lists = [paused, continued];
        let mut dispatch = TimecodeDispatch::default();
        let (receiver, tx) = TimecodeReceiver::test_channel();
        for (frame, ms) in [(20, 0), (30, 375), (32, 455)] {
            tx.send(TcObservation { event: TcEvent::Position(pos(frame), 2),
                at: start + Duration::from_millis(ms) }).unwrap();
        }
        let batch = dispatch.collect(Some(&receiver), &lists, start + Duration::from_millis(455));
        assert_eq!(batch.fire, vec![
            continue_marks[0], pause_marks[1], continue_marks[1], pause_marks[2], continue_marks[2],
        ]);
    }

    #[test]
    fn loss_on_the_completion_tick_holds_wait_and_preserves_order_of_transitions() {
        use crate::cue::{traits::CueFactory, wait_cue::WaitCueFactory};

        let (context, _, _) = crate::test_support::recording_context();
        let mut transport = Transport::new(context.clone());
        let mut list = CueList::new("Clocked follow");
        list.tc_config.on_stop = TcOnStop::Pause;
        let wait = WaitCueFactory.from_json(serde_json::json!({ "wait_duration_ms": 0 })).unwrap();
        let id = wait.id();
        list.push(wait);
        list.push(Box::new(WaitCue::new()));
        transport.go_from_timecode(&mut list, &id).unwrap();
        assert!(list.get(&id).is_some_and(|cue| cue.is_running() && cue.is_time_complete()));

        let now = Instant::now();
        let mut batch = TimecodeBatch {
            edges: vec![
                (list.id, ClockEdge::Lost(now)),
                (list.id, ClockEdge::Returned(now)),
                (list.id, ClockEdge::Lost(now)),
            ],
            ..Default::default()
        };
        assert_eq!(
            batch.apply_existing_runs(std::slice::from_mut(&mut list), &context, false, &mut Default::default()),
            vec![
                (id, CueState::Running, CueState::Paused),
                (id, CueState::Paused, CueState::Running),
                (id, CueState::Running, CueState::Paused),
            ],
        );
        let completed: Vec<_> = list.cues.iter().filter(|cue| cue.is_running() && cue.is_time_complete())
            .map(|cue| (cue.id(), cue.continue_mode(), cue.post_wait())).collect();
        assert!(!schedule_list_follows(&mut list, &completed, &mut HashMap::new(), now));
        assert_eq!(list.get(&id).unwrap().state(), CueState::Paused);
    }

    #[test]
    fn clock_started_media_respects_manual_playback_and_audio_device_holds() {
        use crate::cue::{audio_cue::AudioCue, video_cue::VideoCue};

        let (context, _, _) = crate::test_support::recording_context_with_video_audio();
        let mut transport = Transport::new(context.clone());
        let mut list = CueList::new("Clocked media");
        list.tc_config.on_stop = TcOnStop::Pause;
        let audio = || {
            let mut cue = AudioCue::new();
            cue.accept_preloaded_audio(Arc::new(vec![0.0; 480_000]), 1, 48_000, Duration::from_secs(10));
            cue.set_continue_mode(ContinueMode::DoNotContinue);
            Box::new(cue)
        };
        let manual = audio();
        let manual_id = manual.id();
        let clock_audio = audio();
        let audio_id = clock_audio.id();
        let mut video = VideoCue::new();
        video.file_path = Some("stage.mov".into());
        video.set_continue_mode(ContinueMode::DoNotContinue);
        let video_id = video.id();
        list.push(manual);
        list.push(clock_audio);
        list.push(Box::new(video));

        transport.go_by_id(&mut list, &manual_id).unwrap();
        let generation = list.get(&manual_id).unwrap().play_generation();
        assert!(transport.go_from_timecode(&mut list, &manual_id).unwrap().triggered.is_empty());
        assert_eq!(list.get(&manual_id).unwrap().play_generation(), generation);
        transport.go_from_timecode(&mut list, &audio_id).unwrap();
        transport.go_from_timecode(&mut list, &video_id).unwrap();

        let mut device_holds = std::collections::HashSet::from([audio_id]);
        assert!(transition_device_hold(list.get_mut(&audio_id).unwrap(), true, &context).unwrap());
        let now = Instant::now();
        let list_id = list.id;
        let mut loss = TimecodeBatch { edges: vec![(list_id, ClockEdge::Lost(now))], ..Default::default() };
        assert_eq!(loss.apply_existing_runs(std::slice::from_mut(&mut list), &context, true, &mut device_holds),
            [(video_id, CueState::Running, CueState::Paused)]);
        assert!(list.get(&manual_id).unwrap().is_running());
        assert!(!transition_device_hold(list.get_mut(&audio_id).unwrap(), false, &context).unwrap());

        // The operator takes over the video hold. The audio device remains
        // away on clock return, so it must still gate audio recovery.
        list.get_mut(&video_id).unwrap().pause(&context).unwrap();
        assert!(!transition_device_hold(list.get_mut(&video_id).unwrap(), false, &context).unwrap());
        let mut returned = TimecodeBatch { edges: vec![(list_id, ClockEdge::Returned(now))], ..Default::default() };
        assert!(returned.apply_existing_runs(std::slice::from_mut(&mut list), &context, true, &mut device_holds).is_empty());
        assert!(list.get(&audio_id).unwrap().is_paused());
        assert!(device_holds.contains(&audio_id));
        assert!(transition_device_hold(list.get_mut(&audio_id).unwrap(), false, &context).unwrap());
        assert!(list.get(&audio_id).unwrap().is_running());
        assert!(list.get(&video_id).unwrap().is_paused());

        list.tc_config.on_stop = TcOnStop::Stop;
        let mut loss = TimecodeBatch { edges: vec![(list_id, ClockEdge::Lost(now))], ..Default::default() };
        assert_eq!(loss.apply_existing_runs(std::slice::from_mut(&mut list), &context, false, &mut device_holds),
            [(audio_id, CueState::Running, CueState::Standby)]);
        assert!(device_holds.is_empty());
        assert!(list.get(&manual_id).unwrap().is_running());
        assert!(list.get(&video_id).unwrap().is_paused());
    }

    #[test]
    fn continue_policy_retriggers_running_and_paused_waits_without_claiming_ownership() {
        let (context, _, _) = crate::test_support::recording_context();
        let mut transport = Transport::new(context.clone());
        let mut list = CueList::new("Legacy clock starts");
        let wait = WaitCue::new();
        let id = wait.id();
        list.push(Box::new(wait));

        transport.go_by_id(&mut list, &id).unwrap();
        transport.go_from_timecode(&mut list, &id).unwrap();
        assert_eq!(list.get(&id).unwrap().play_generation(), 2);
        transport.pause_cue(&mut list, &id).unwrap();
        transport.go_from_timecode(&mut list, &id).unwrap();
        assert_eq!(list.get(&id).unwrap().play_generation(), 3);
        assert!(list.get(&id).unwrap().is_running());

        list.tc_config.on_stop = TcOnStop::Stop;
        let mut batch = TimecodeBatch {
            edges: vec![(list.id, ClockEdge::Lost(Instant::now()))],
            ..Default::default()
        };
        assert!(batch.apply_existing_runs(std::slice::from_mut(&mut list), &context, false, &mut Default::default()).is_empty());
    }

    #[test]
    fn completed_cart_pads_schedule_no_immediate_or_delayed_follow() {
        use crate::show::cue_list::CueListMode;
        let mut list = CueList::new("Pads");
        list.set_mode(CueListMode::Cart, None);
        let delayed = uuid::Uuid::new_v4();
        let now = Instant::now();
        let delay = Duration::from_millis(250);
        let completed = [
            (uuid::Uuid::new_v4(), ContinueMode::AutoFollow, Duration::ZERO),
            (delayed, ContinueMode::AutoFollow, delay),
        ];
        let mut pending = HashMap::new();
        assert!(!schedule_list_follows(&mut list, &completed, &mut pending, now));
        assert!(pending.is_empty());
        list.set_mode(CueListMode::Cart, Some(true));
        assert!(schedule_list_follows(&mut list, &completed, &mut pending, now));
        assert_eq!(pending.get(&delayed), Some(&(now + delay, list.id)));
    }

    #[test]
    fn progress_with_known_duration() {
        let (progress, elapsed, remaining, duration) =
            progress_values(12_600, Some(30_000), Some(30_000));
        assert!((progress - 0.42).abs() < 1e-3);
        assert!((elapsed - 12.6).abs() < 1e-3);
        assert!((remaining - 17.4).abs() < 1e-3);
        assert!((duration - 30.0).abs() < 1e-3);
    }

    #[test]
    fn progress_clamps_past_the_end() {
        let (progress, _, remaining, _) = progress_values(31_000, Some(30_000), None);
        assert_eq!(progress, 1.0);
        assert_eq!(remaining, 0.0);
    }

    #[test]
    fn unknown_duration_falls_back_to_file_position() {
        // Vamp / infinite loop: no total duration — the gauge follows the
        // position within one file pass; remaining/duration report -1.
        let (progress, elapsed, remaining, duration) =
            progress_values(75_000, None, Some(60_000));
        assert!((progress - 0.25).abs() < 1e-3, "75s % 60s = 15s / 60s");
        assert!((elapsed - 75.0).abs() < 1e-3);
        assert_eq!(remaining, -1.0);
        assert_eq!(duration, -1.0);
    }

    #[test]
    fn nothing_known_reports_zero_progress() {
        // Live feed: no duration at all.
        let (progress, elapsed, remaining, duration) = progress_values(5_000, None, None);
        assert_eq!(progress, 0.0);
        assert!((elapsed - 5.0).abs() < 1e-3);
        assert_eq!(remaining, -1.0);
        assert_eq!(duration, -1.0);
    }
}
