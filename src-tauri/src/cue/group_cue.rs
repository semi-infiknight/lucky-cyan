//! [`GroupCue`] — contains and fires a list of child cues.
//!
//! ## Modes
//! - **Simultaneous**: all children fire at once; the Group completes when
//!   every child has finished.
//! - **Sequential**: children fire one after another using each child's own
//!   Continue Mode (Auto-Continue, Auto-Follow, Do Not Continue) exactly like
//!   a mini Cue List.
//! - **Start First**: the outer playhead moves on while the children execute
//!   their own sequence.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use crate::engine::voice::PlaylistCut;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    audio_cue::AudioCue,
    context::{CueContext, CueEvent},
    registry::CueRegistry,
    traits::{Cue, CueFactory},
    types::{
        ContinueMode, CueColor, CueId, CueState, CueType, GroupMode, PlaylistSecondTrigger,
    },
};

// ---------------------------------------------------------------------------
// GroupCue
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct BoundaryWaitRun {
    generation: u64,
    post_wait: Duration,
    completed: bool,
}

/// A cue that contains other cues and fires them simultaneously or sequentially.
pub struct GroupCue {
    // ── Identity ──────────────────────────────────────────────────────────
    pub id: CueId,
    name: String,
    number: Option<String>,
    notes: String,
    color: CueColor,

    // ── State ─────────────────────────────────────────────────────────────
    state: CueState,

    // ── Timing ────────────────────────────────────────────────────────────
    pre_wait: Duration,
    post_wait: Duration,
    started_at: Option<Instant>,
    action_started_at: Option<Instant>,
    start_reported: bool,
    in_pre_wait: bool,

    // ── Continue ──────────────────────────────────────────────────────────
    continue_mode: ContinueMode,
    auto_continue_fired: bool,
    output_withdrawn: bool,
    delayed_start_admission: bool,

    is_disabled: bool,

    // ── Group-specific ────────────────────────────────────────────────────
    pub mode: GroupMode,
    /// Direct child cues (any type, including nested Groups).
    pub children: Vec<Box<dyn Cue>>,
    /// Playlist mode only: wrap from the last child back to the first instead of
    /// ending (QLab's looping playlist). Persisted.
    playlist_loop: bool,
    /// Move main-list standby beyond this Playlist after GO.
    playlist_advance_playhead: bool,
    playlist_second_trigger: PlaylistSecondTrigger,
    playlist_auto_shuffle: bool,
    /// Let the outer cue list advance after GO in Start Random mode.
    random_advance_playhead: bool,

    // ── Sequential / Playlist mode state (not persisted) ──────────────────
    /// ID of the child currently at the internal playhead in Sequential/Playlist.
    seq_current_id: Option<CueId>,
    /// Set when the sequential chain has finished (last child completed with
    /// DoNotContinue, or all children exhausted).
    seq_done: bool,
    /// When Some, we are waiting for this instant before firing the next child
    /// (AutoContinue post-wait).
    seq_post_wait_until: Option<Instant>,
    /// A protected entrance temporarily held outer standby.
    playlist_playhead_held: bool,
    /// One traversal order for the current run, including its loop passes.
    playlist_run_order: Option<PlaylistOrder>,

    // ── Start First mode state (not persisted) ───────────────────────────
    /// Frozen clock for timed groups, including the Simultaneous Timeline.
    start_first_paused_at: Option<Instant>,
    start_first_playhead_held: bool,
    start_first_child_changed: bool,
    /// Final Wait eligible for a boundary handoff from this list GO.
    enter_boundary_wait: Option<(CueId, ContinueMode)>,
    /// Generation of the final Wait actually started by this group, and whether
    /// its own timer completed naturally before it was reset to Standby.
    enter_wait_run: Option<BoundaryWaitRun>,
    enter_paused_children: HashMap<CueId, PausedChildPlayback>,
    /// Child and play generation awaiting exact output admission.
    pending_child_admission: Option<(CueId, u64)>,
    playlist_cut: Option<PendingPlaylistCut>,

    // ── StartRandom mode state (not persisted) ────────────────────────────
    /// Child identities already drawn in this rotation, including through Stop.
    random_played: HashSet<CueId>,
    /// xorshift64* PRNG state; 0 = unseeded (seeded lazily on first draw).
    rng_state: u64,
    /// Identity of the most recently started child that has not been reaped.
    random_current_id: Option<CueId>,
    /// A required output admission temporarily held the outer playhead.
    random_playhead_held: bool,
}

struct PendingPlaylistCut {
    outgoing: CueId,
    outgoing_generation: u64,
    incoming: CueId,
    generation: u64,
    voice: CueId,
    cut: Arc<PlaylistCut>,
    requested: Instant,
    adopted: bool,
}

impl Drop for PendingPlaylistCut {
    fn drop(&mut self) {
        if !self.adopted { self.cut.abandon(); }
    }
}

struct PlaylistOrder {
    entries: Vec<PlaylistOrderEntry>,
    // An explicitly parked entrance survives an unsuccessful start followed
    // by a change back to authored order.
    entrance: Option<CueId>,
}

struct PlaylistOrderEntry {
    id: CueId,
    authored_index: usize,
}

#[derive(PartialEq, Eq)]
struct PausedChildPlayback {
    generation: u64,
    voice: Option<CueId>,
}

impl PausedChildPlayback {
    fn of(child: &dyn Cue) -> Self {
        let voice = child.playing_voice_id().or_else(|| {
            // File Audio hides a staged voice from playback controls. A Load
            // can replace a paused Pre-Wait without changing its launch generation.
            (child as &dyn std::any::Any).downcast_ref::<AudioCue>()
                .and_then(AudioCue::prepared_voice_id)
        });
        Self { generation: child.play_generation(), voice }
    }
}

impl GroupCue {
    /// Create a new, empty Group with a fresh UUID.
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: "Group".to_string(),
            number: None,
            notes: String::new(),
            color: CueColor::Yellow,
            state: CueState::Standby,
            pre_wait: Duration::ZERO,
            post_wait: Duration::ZERO,
            started_at: None,
            action_started_at: None,
            start_reported: false,
            in_pre_wait: false,
            continue_mode: ContinueMode::DoNotContinue,
            auto_continue_fired: false,
            output_withdrawn: false,
            delayed_start_admission: false,
            is_disabled: false,
            mode: GroupMode::Simultaneous,
            children: Vec::new(),
            playlist_loop: false,
            playlist_advance_playhead: true,
            playlist_second_trigger: PlaylistSecondTrigger::Ignore,
            playlist_auto_shuffle: false,
            playlist_playhead_held: false,
            playlist_run_order: None,
            random_advance_playhead: true,
            seq_current_id: None,
            seq_done: false,
            seq_post_wait_until: None,
            start_first_paused_at: None,
            start_first_playhead_held: false,
            start_first_child_changed: false,
            enter_boundary_wait: None,
            enter_wait_run: None,
            enter_paused_children: HashMap::new(),
            pending_child_admission: None,
            playlist_cut: None,
            random_played: HashSet::new(),
            rng_state: 0,
            random_current_id: None,
            random_playhead_held: false,
        }
    }

    /// Deserialise a GroupCue from JSON, using `registry` to reconstruct children.
    pub fn from_json_with_registry(value: &Value, registry: &CueRegistry) -> Result<Box<dyn Cue>> {
        let mut cue = GroupCue::new();

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
            if let Ok(m) = serde_json::from_value(cm.clone()) {
                cue.continue_mode = m;
            }
        }
        if let Some(col) = value.get("color") {
            if let Ok(c) = serde_json::from_value(col.clone()) {
                cue.color = c;
            }
        }
        if let Some(gm) = value.get("group_mode") {
            if let Ok(m) = serde_json::from_value(gm.clone()) {
                cue.mode = m;
            }
        }
        if let Some(b) = value.get("playlist_loop").and_then(|v| v.as_bool()) {
            cue.playlist_loop = b;
        }
        cue.playlist_second_trigger = serde_json::from_value(value["playlist_second_trigger"].clone())
            .unwrap_or_default();
        cue.playlist_auto_shuffle = value["playlist_auto_shuffle"].as_bool().unwrap_or(false);
        cue.playlist_advance_playhead = value["playlist_advance_playhead"].as_bool()
            .unwrap_or(cue.mode != GroupMode::Playlist);
        cue.random_advance_playhead = value["random_advance_playhead"]
            .as_bool()
            .unwrap_or(cue.mode != GroupMode::StartRandom);

        // Deserialise children recursively.
        if let Some(arr) = value.get("children").and_then(|v| v.as_array()) {
            for child_val in arr {
                match registry.from_json(child_val.clone()) {
                    Ok(child) => cue.children.push(child),
                    Err(e) => log::warn!("[group] skipping unrecognised child: {e}"),
                }
            }
        }
        if let Some(b) = value.get("is_disabled").and_then(|v| v.as_bool()) {
            cue.is_disabled = b;
        }

        Ok(Box::new(cue))
    }

    // ── Private helpers ───────────────────────────────────────────────────

    fn start_action(&mut self, ctx: &CueContext) -> Result<()> {
        self.in_pre_wait = false;
        self.action_started_at = Some(Instant::now());
        self.state = CueState::Running;
        self.seq_done = false;
        self.seq_post_wait_until = None;

        match self.mode {
            GroupMode::Simultaneous => {
                let mut held = None;
                ctx.start_simultaneous_audio(|context| {
                    for child in self.children.iter_mut().filter(|child| !child.is_disabled()) {
                        if let Err(e) = child.go(context) {
                            log::warn!("Group simultaneous: child '{}' failed to start: {e}", child.name());
                        }
                        self.delayed_start_admission |= child.pending_start_admission();
                        if child.has_withdrawn_output() {
                            held = Some(child.id());
                        }
                    }
                });
                if let Some(id) = held {
                    self.child_output_withdrawn(id);
                }
            }
            GroupMode::Sequential | GroupMode::Playlist => {
                // Fire from the inner playhead (set by `set_active_child` when the
                // user parks the Playhead on a specific child); `None` starts from
                // the first child.  Playlist adds exclusivity + loop inside
                // `fire_next_sequential`.
                let start_after = self.seq_current_id;
                if let Err(error) = self.fire_next_sequential(ctx, start_after) {
                    if self.seq_current_id == start_after && !self.children.iter().any(|child| child.is_running() || child.is_paused()) {
                        self.state = CueState::Standby;
                        self.started_at = None;
                        self.action_started_at = None;
                    }
                    return Err(error);
                }
            }
            GroupMode::StartFirst | GroupMode::StartFirstAndEnter => {
                if self.mode == GroupMode::StartFirst {
                    self.seq_current_id = None;
                }
                if let Err(error) = self.start_first_chain(ctx) {
                    self.seq_done = true;
                    if !self.children.iter().any(|child| child.is_running() || child.is_paused()) {
                        self.state = CueState::Standby;
                        self.started_at = None;
                        self.action_started_at = None;
                    }
                    return Err(error);
                }
            }
            GroupMode::StartRandom => {
                self.start_random_child(ctx);
            }
        }
        self.report_action_start(ctx);
        Ok(())
    }

    fn report_action_start(&mut self, ctx: &CueContext) {
        if !self.start_reported && self.is_action_started() && !self.output_withdrawn {
            self.start_reported = ctx.mark_action_started(self.id);
        }
    }

    fn uses_start_first_timing(&self) -> bool {
        matches!(self.mode, GroupMode::StartFirst | GroupMode::StartFirstAndEnter)
    }

    fn advances_playlist(&self) -> bool {
        self.mode == GroupMode::Playlist && self.playlist_advance_playhead
    }

    fn go_group(&mut self, ctx: &CueContext) -> Result<()> {
        if self.advances_playlist() && (self.is_running() || self.is_paused()) {
            if self.is_running() && !self.in_pre_wait && self.output_withdrawn
                && !self.pending_start_admission()
            {
                // Retrying a failed route starts that child. A trigger during
                // ordinary playback leaves the playlist running as programmed.
                self.output_withdrawn = false;
                self.seq_done = false;
                self.fire_next_sequential(ctx, self.seq_current_id)?;
                self.report_action_start(ctx);
            }
            return Ok(());
        }
        if self.uses_start_first_timing() && (self.is_running() || self.is_paused()) {
            if self.mode == GroupMode::StartFirst {
                return Ok(());
            }
            anyhow::ensure!(!self.is_paused(), "Resume the group before starting another child");
            if self.in_pre_wait || self.pending_start_admission() || !self.has_next_sequential_child() {
                return Ok(());
            }
            self.start_first_chain(ctx)?;
            self.report_action_start(ctx);
            return Ok(());
        }

        if self.state == CueState::Running && !self.in_pre_wait {
            if self.pending_start_admission() {
                return Ok(());
            }
            match self.mode {
                // Sequential and Playlist absorb a GO by advancing their inner
                // sequence.  Playlist adds exclusivity/loop inside
                // `fire_next_sequential`; the advance logic is otherwise identical.
                GroupMode::Sequential | GroupMode::Playlist => {
                    // Sequence paused (DoNotContinue child finished) → fire next child.
                    if self.seq_done && self.has_next_sequential_child() {
                        self.seq_done = false;
                        let prev_id = self.seq_current_id;
                        return self.fire_next_sequential(ctx, prev_id);
                    }
                    // A child is still running → advance to the next child.  In
                    // Sequential the previous child keeps playing (audio overlap
                    // like top-level cues); in Playlist `fire_next_sequential`
                    // stops it (exclusivity).
                    if let Some(current_id) = self.seq_current_id {
                        self.seq_done = false;
                        return self.fire_next_sequential(ctx, Some(current_id));
                    }
                }
                GroupMode::StartRandom => {
                    self.start_random_child(ctx);
                    return Ok(());
                }
                GroupMode::Simultaneous | GroupMode::StartFirst | GroupMode::StartFirstAndEnter => {}
            }
        }

        if self.advances_playlist() {
            for child in self.children.iter().filter(|child| !child.is_disabled()) {
                Self::check_start_first_child(child.as_ref())?;
            }
            self.prepare_playlist_shuffle();
        }
        self.auto_continue_fired = false;
        self.output_withdrawn = false;
        self.delayed_start_admission = false;
        self.start_first_playhead_held = false;
        self.playlist_playhead_held = false;
        self.start_reported = false;
        self.started_at = Some(Instant::now());

        if self.pre_wait > Duration::ZERO {
            self.delayed_start_admission = self.requires_eq_gain_admission()
                || self.pre_wait_requires_named_output(ctx);
            self.in_pre_wait = true;
            self.state = CueState::Running;
            return Ok(());
        }

        self.start_action(ctx)
    }

    fn start_first_chain(&mut self, ctx: &CueContext) -> Result<()> {
        let after = self.seq_current_id;
        if let Some(first) = self.next_armed_child(after) {
            for child in self.children[first..].iter().filter(|child| !child.is_disabled()) {
                Self::check_start_first_child(child.as_ref())?;
                if child.continue_mode() == ContinueMode::DoNotContinue {
                    break;
                }
            }
        }
        self.seq_post_wait_until = None;
        self.output_withdrawn = false;
        self.seq_done = false;
        self.fire_next_sequential(ctx, after)?;
        self.advance_start_first(ctx)
    }

    fn boundary_wait(&self) -> Option<(CueId, ContinueMode)> {
        if self.mode != GroupMode::StartFirstAndEnter { return None; }
        let last = self.children.iter().rfind(|child| !child.is_disabled())?;
        if last.cue_type() != CueType::Wait { return None; }
        match last.continue_mode() {
            ContinueMode::AutoContinue if last.pre_wait().is_zero() => Some((last.id(), ContinueMode::AutoContinue)),
            ContinueMode::AutoFollow if last.duration().is_some_and(|d| !d.is_zero()) => Some((last.id(), ContinueMode::AutoFollow)),
            _ => None,
        }
    }

    pub(crate) fn check_enter_boundary(&self, allow_wait: bool) -> Result<()> {
        if self.mode != GroupMode::StartFirstAndEnter {
            return Ok(());
        }
        anyhow::ensure!(self.continue_mode == ContinueMode::DoNotContinue,
            "Set the Start First And Enter group to Do Not Continue.");
        if let Some(last) = self.children.iter().rfind(|child| !child.is_disabled()) {
            anyhow::ensure!(last.continue_mode() == ContinueMode::DoNotContinue
                || (allow_wait && self.boundary_wait().is_some()),
                "Set the last armed child '{}' to Do Not Continue. For main-list GO, a final Wait can Auto-Continue after its Post-Wait with zero Pre-Wait, or Auto-Follow after its positive duration and Post-Wait.",
                last.name());
        }
        Ok(())
    }

    /// Authorize this run only when a main sequential GO actually starts work.
    pub(crate) fn go_from_main_list(&mut self, ctx: &CueContext) -> Result<Option<CueId>> {
        self.check_enter_boundary(true)?;
        self.reconcile_child_admission();
        let starts = !self.is_paused() && (!self.is_running()
            || (!self.in_pre_wait && !self.pending_start_admission() && self.has_next_sequential_child()));
        let boundary_wait = self.boundary_wait().filter(|_| starts);
        if starts {
            self.enter_boundary_wait = boundary_wait;
            self.enter_wait_run = None;
        }
        self.go_group(ctx)?;
        Ok(boundary_wait.map(|(id, _)| id))
    }

    /// After standby leaves the group, report Pending (false), Ready (true)
    /// or revoked (None). A stopped or independently restarted Wait cannot
    /// stand in for the generation owned by this passage.
    pub(crate) fn enter_wait_ready(&self, wait_id: CueId) -> Option<bool> {
        let binding = self.enter_boundary_wait?;
        if binding.0 != wait_id || self.boundary_wait() != Some(binding)
            || self.seq_current_id != Some(wait_id) || self.output_withdrawn {
            return None;
        }
        let child = self.children.iter().find(|child| child.id() == wait_id)?;
        let run = self.enter_wait_run?;
        if run.generation != child.play_generation()
            || !(run.completed || child.is_running() || child.is_paused()) {
            return None;
        }
        match binding.1 {
            ContinueMode::AutoContinue | ContinueMode::AutoFollow if run.completed => self.seq_post_wait_until
                .map(|due| self.is_running() && Instant::now() >= due),
            ContinueMode::AutoContinue => Some(self.is_running() && child.is_running()
                && child.is_action_started() && child.action_elapsed() >= run.post_wait),
            ContinueMode::AutoFollow => Some(false),
            _ => None,
        }
    }

    /// Fire the next sequential child after `after_id` (or the first child if
    /// `after_id` is `None`).  Handles Auto-Follow chaining recursively.
    fn fire_next_sequential(&mut self, ctx: &CueContext, after_id: Option<CueId>) -> Result<()> {
        let Some(next_idx) = self.next_armed_child(after_id) else {
            self.seq_done = true;
            return Ok(());
        };

        let start_first_timing = self.uses_start_first_timing();
        let enter = self.mode == GroupMode::StartFirstAndEnter;
        let boundary_check = self.check_enter_boundary(
            self.enter_boundary_wait.is_some() && self.enter_boundary_wait == self.boundary_wait(),
        );
        let advancing_playlist = self.advances_playlist();
        let child_id = self.children[next_idx].id();
        let child = &mut self.children[next_idx];
        let result = if start_first_timing || advancing_playlist {
            boundary_check.and_then(|()| Self::check_start_first_child(child.as_ref())).and_then(|()| {
                anyhow::ensure!(!enter || !child.is_running(),
                    "Stop the child before returning standby to it");
                child.go(ctx)
            })
        } else {
            child.go(ctx)
        };
        if start_first_timing {
            self.start_first_child_changed = true;
        }
        if let Err(error) = result {
            // Park immediately before the rejected child, including when a
            // looping playlist wrapped. Nothing retries until the next GO.
            self.seq_current_id = after_id;
            self.seq_done = true;
            if self.children[next_idx].has_withdrawn_output() {
                self.child_output_withdrawn(child_id);
            }
            return Err(anyhow!("Could not start '{}': {error:#}", self.children[next_idx].name()));
        }
        if self.enter_boundary_wait.is_some_and(|(id, _)| id == child_id) {
            let child = &self.children[next_idx];
            self.enter_wait_run = Some(BoundaryWaitRun {
                generation: child.play_generation(), post_wait: child.post_wait(), completed: false,
            });
        }
        self.seq_current_id = Some(child_id);
        let pending = self.children[next_idx].pending_start_admission();
        self.delayed_start_admission |= pending;
        if (enter || advancing_playlist) && pending {
            self.pending_child_admission = Some((child_id, self.children[next_idx].play_generation()));
        }
        if self.children[next_idx].has_withdrawn_output() {
            self.child_output_withdrawn(child_id);
            return Ok(());
        }

        // Playlist is exclusive: starting this child stops any other still playing.
        if self.mode == GroupMode::Playlist {
            self.stop_other_children(ctx, child_id);
        }

        // Auto-Follow: fire the child after this one immediately when this one
        // starts.  Playlist mode plays each child to completion (auto-advancing
        // in tick), so it must NOT chain here — chaining would immediately stop
        // the just-started child via exclusivity.
        if self.mode == GroupMode::Sequential
            && !self.children[next_idx].uses_action_clock_for_continue()
            && self.children[next_idx].is_action_started()
            && self.children[next_idx].continue_mode() == ContinueMode::AutoFollow
        {
            // Mark this child's Auto-Follow as fired so the event loop does not
            // double-chain on the main list level.
            self.children[next_idx].mark_auto_continue_fired();
            let fired_id = child_id;
            self.fire_next_sequential(ctx, Some(fired_id))?;
        }

        Ok(())
    }

    fn tick_running_child(&mut self, idx: usize, ctx: &CueContext) -> Result<()> {
        let child = &mut self.children[idx];
        if child.state() == CueState::Running {
            if let Err(error) = child.tick(ctx) {
                if child.has_withdrawn_output() {
                    let id = child.id();
                    self.child_output_withdrawn(id);
                }
                return Err(error);
            }
        }
        Ok(())
    }

    /// Tick a child at `idx` and return whether it is now complete.
    fn tick_child_at(&mut self, idx: usize, ctx: &CueContext) -> Result<bool> {
        self.tick_running_child(idx, ctx)?;
        let child = &self.children[idx];
        let done = matches!(child.state(), CueState::Completed | CueState::Standby)
            || child.is_time_complete();

        Ok(done)
    }

    fn start_first_finished(child: &dyn Cue) -> bool {
        child.state() == CueState::Completed || (child.is_running()
            && child.playing_voice_id().is_none()
            && (child.is_time_complete() || child.is_complete()))
    }

    fn check_start_first_child(child: &dyn Cue) -> Result<()> {
        // These actions need cue-list routing; a direct child GO cannot supply it.
        anyhow::ensure!(
            !matches!(child.cue_type(),
                CueType::Group | CueType::Fade | CueType::Stop | CueType::Devamp
                    | CueType::Start | CueType::Pause | CueType::Resume | CueType::Load
                    | CueType::Reset | CueType::Goto | CueType::Arm | CueType::Disarm
                    | CueType::Target),
            "This group mode cannot run '{}' ({}) as a child. Place it in the main cue list.",
            child.name(), child.cue_type()
        );
        Ok(())
    }

    /// Hold the outer standby when a Group's own Pre-Wait delays admission of
    /// a protected Audio child. Route readiness is checked by the child at GO.
    fn pre_wait_requires_named_output(&self, ctx: &CueContext) -> bool {
        if self.advances_playlist() {
            return self.next_armed_child(self.seq_current_id).is_some_and(|index| {
                (self.children[index].as_ref() as &dyn std::any::Any).downcast_ref::<AudioCue>()
                    .is_some_and(|audio| audio.requires_named_output(ctx))
            });
        }
        let start_first = self.uses_start_first_timing();
        if !start_first && !(self.mode == GroupMode::StartRandom && self.random_advance_playhead) {
            return false;
        }
        let first = if self.mode == GroupMode::StartFirstAndEnter {
            self.next_armed_child(self.seq_current_id).unwrap_or(self.children.len())
        } else {
            0
        };
        for child in self.children[first..].iter().filter(|child| !child.is_disabled()) {
            if (child.as_ref() as &dyn std::any::Any).downcast_ref::<AudioCue>()
                .is_some_and(|audio| audio.requires_named_output(ctx))
            {
                return true;
            }
            if start_first && child.continue_mode() == ContinueMode::DoNotContinue {
                break;
            }
        }
        false
    }

    fn finish_start_first_child(&mut self, idx: usize) -> Result<()> {
        let child = &mut self.children[idx];
        self.start_first_child_changed = true;
        if self.seq_current_id == Some(child.id()) && !self.seq_done {
            if child.is_disabled() || child.has_withdrawn_output() {
                self.seq_done = true;
            } else {
                let wait = match child.continue_mode() {
                    ContinueMode::AutoFollow => Some(Duration::ZERO),
                    ContinueMode::AutoContinue =>
                        Some(child.post_wait().saturating_sub(child.action_elapsed())),
                    ContinueMode::DoNotContinue => None,
                };
                self.seq_post_wait_until = wait.and_then(|wait| {
                    self.start_first_paused_at.unwrap_or_else(Instant::now).checked_add(wait)
                });
                self.seq_done = self.seq_post_wait_until.is_none();
            }
        }
        // Transfer the remaining Post-Wait to the group's pause-aware clock
        // before resetting the child. Count any time a late tick has already
        // spent past the programmed action end toward its Auto-Follow delay.
        if let Some((id, mode)) = self.enter_boundary_wait {
            if id == child.id() && child.is_running() && child.is_time_complete() {
                if let Some(run) = self.enter_wait_run.as_mut().filter(|run| run.generation == child.play_generation()) {
                    run.completed = true;
                    let origin = if mode == ContinueMode::AutoFollow {
                        child.duration().unwrap_or_default()
                    } else {
                        Duration::ZERO
                    };
                    let elapsed = child.action_elapsed().saturating_sub(origin);
                    self.seq_post_wait_until = Instant::now().checked_add(run.post_wait.saturating_sub(elapsed));
                }
            }
        }
        child.reset()
    }

    /// The event loop reports natural engine completion here before resetting
    /// a child. A stopped child's Standby state alone must never start a follow.
    pub(crate) fn reap_start_first_children(&mut self, completed: &[CueId], withdrawn: &[CueId]) -> bool {
        if completed.is_empty() && withdrawn.is_empty() {
            return std::mem::take(&mut self.start_first_child_changed);
        }
        for idx in 0..self.children.len() {
            let child = &self.children[idx];
            if !child.is_running() && !child.is_paused() {
                continue;
            }
            let Some(voice) = child.playing_voice_id() else { continue };
            if withdrawn.contains(&voice) {
                self.child_output_withdrawn(self.children[idx].id());
                let _ = self.children[idx].reset();
                self.start_first_child_changed = true;
            } else if completed.contains(&voice) {
                let _ = self.finish_start_first_child(idx);
            }
        }
        std::mem::take(&mut self.start_first_child_changed)
    }

    /// Reap a withdrawn voice before it can look like a completed track.
    pub(crate) fn reap_playlist_withdrawals(&mut self, withdrawn: &[CueId]) -> bool {
        let Some(id) = self.seq_current_id else { return false };
        let Some(child) = self.children.iter_mut().find(|child| child.id() == id
            && child.playing_voice_id().is_some_and(|voice| withdrawn.contains(&voice)))
        else { return false };
        let _ = child.reset();
        self.child_output_withdrawn(id);
        true
    }

    fn tick_start_first(&mut self, ctx: &CueContext) -> Result<()> {
        self.reconcile_child_admission();
        for idx in 0..self.children.len() {
            if let Err(error) = self.tick_running_child(idx, ctx) {
                if self.seq_current_id == Some(self.children[idx].id()) {
                    self.seq_done = true;
                }
                return Err(error);
            }
            if !self.output_withdrawn && self.children[idx].has_withdrawn_output() {
                self.child_output_withdrawn(self.children[idx].id());
            }
            if self.pending_child_admission.is_some_and(|(id, _)| id == self.children[idx].id()) {
                self.reconcile_child_admission();
            }
            if Self::start_first_finished(self.children[idx].as_ref()) {
                self.finish_start_first_child(idx)?;
            }
        }
        self.advance_start_first(ctx)
    }

    /// Losing the pending run before it renders must leave that child in standby.
    fn reconcile_child_admission(&mut self) {
        let Some((id, generation)) = self.pending_child_admission else { return };
        let current = self.children.iter().find(|child| child.id() == id)
            .filter(|child| child.play_generation() == generation);
        if current.is_some_and(|child| child.pending_start_admission()) {
            return;
        }
        let admitted = current.is_some_and(|child| {
            (child.is_running() || child.is_paused()) && child.is_action_started()
                && child.playing_voice_id().is_some()
        });
        self.pending_child_admission = None;
        if !admitted {
            self.child_output_withdrawn(id);
        }
    }

    /// Advance from the current child while its continuation is due. This
    /// also handles zero-wait chains on GO without recursing through children.
    fn advance_start_first(&mut self, ctx: &CueContext) -> Result<()> {
        while !self.seq_done && !self.output_withdrawn {
            let Some(idx) = self.seq_current_id.and_then(|id| {
                self.children.iter().position(|child| child.id() == id)
            }) else {
                self.seq_done = true;
                break;
            };
            let child = &self.children[idx];
            if child.continue_mode() == ContinueMode::DoNotContinue || child.is_disabled()
                || !self.has_next_sequential_child()
            {
                self.seq_done = true;
                break;
            }

            if let Some(deadline) = self.seq_post_wait_until {
                if Instant::now() < deadline {
                    break;
                }
            } else if Self::start_first_finished(child.as_ref()) {
                self.finish_start_first_child(idx)?;
                continue;
            } else if child.state() == CueState::Standby {
                self.seq_done = true;
                break;
            } else if !child.is_running() || child.continue_mode() != ContinueMode::AutoContinue
                || !child.is_action_started() || child.action_elapsed() < child.post_wait()
            {
                break;
            }

            self.seq_post_wait_until = None;
            self.fire_next_sequential(ctx, self.seq_current_id)?;
        }
        Ok(())
    }

    /// Drive one tick of a Sequential (or Playlist) group: tick running children,
    /// advance the inner sequence when the current child completes (respecting its
    /// Continue Mode), and honour AutoContinue post-waits.  Playlist's exclusivity
    /// and loop are handled inside `fire_next_sequential`.
    fn tick_sequential(&mut self, ctx: &CueContext) -> Result<()> {
        self.settle_playlist_cut(ctx, false);
        if self.playlist_cut.is_some() { return Ok(()); }
        if self.advances_playlist() {
            self.reconcile_child_admission();
        }
        // Tick every running child so overlapping cues (a manual GO or Auto-Follow
        // fired the next child while a previous one is still playing) keep
        // progressing and finish on their own.  Reset any child that finishes
        // EXCEPT the current sequence driver — its completion advances the sequence.
        let current_id_opt = self.seq_current_id;
        let mut current_done = false;
        for i in 0..self.children.len() {
            if self.children[i].state() == CueState::Running {
                let done = match self.tick_child_at(i, ctx) {
                    Ok(done) => done,
                    Err(error) => {
                        let id = self.children[i].id();
                        // A timed start rejected after pre-wait is a hold, not
                        // natural completion of the child driving this list.
                        if Some(id) == current_id_opt && self.children[i].state() == CueState::Standby {
                            self.set_active_child(&id);
                        }
                        return Err(error);
                    }
                };
                if self.advances_playlist()
                    && self.pending_child_admission.is_some_and(|(id, _)| id == self.children[i].id())
                {
                    self.reconcile_child_admission();
                }
                if done {
                    if Some(self.children[i].id()) == current_id_opt {
                        current_done = true;
                    } else {
                        let _ = self.children[i].reset();
                    }
                }
            }
        }

        // Post-wait before firing the next child.
        if let Some(deadline) = self.seq_post_wait_until {
            if Instant::now() >= deadline {
                self.seq_post_wait_until = None;
                self.fire_next_sequential(ctx, current_id_opt)?;
            }
            return Ok(());
        }

        let current_id = match current_id_opt {
            Some(id) => id,
            None => return Ok(()),
        };
        let idx = match self.children.iter().position(|c| c.id() == current_id) {
            Some(i) => i,
            None => return Ok(()),
        };
        // The driver child may have finished in a previous tick before we got here.
        if matches!(self.children[idx].state(), CueState::Completed | CueState::Standby) {
            current_done = true;
        }

        let child = &mut self.children[idx];
        let uses_action_clock = child.uses_action_clock_for_continue();
        if current_done && self.seq_done && uses_action_clock {
            child.reset()?;
        }
        if self.mode == GroupMode::Sequential && !self.seq_done && uses_action_clock
            && child.continue_mode() == ContinueMode::AutoContinue && !child.is_auto_continue_fired()
        {
            if child.is_running() && child.is_action_started() && child.action_elapsed() >= child.post_wait() {
                child.mark_auto_continue_fired();
                if current_done {
                    child.reset()?;
                }
                self.fire_next_sequential(ctx, Some(current_id))?;
            }
            return Ok(());
        }

        // `seq_done` means the sequence is intentionally paused at this child
        // (a DoNotContinue boundary, or a manual Playhead placement) — wait for a
        // GO rather than auto-advancing.
        if current_done && !self.seq_done {
            let cm = self.children[idx].continue_mode();
            let pw = self.children[idx].post_wait();
            let _ = self.children[idx].reset();

            if self.mode == GroupMode::Playlist {
                // A Playlist plays one child at a time and auto-advances through
                // ALL of them regardless of each child's Continue Mode (it is a
                // playlist, not a manual sequence).  `fire_next_sequential` wraps
                // to the first child when looping, or sets `seq_done` at the end.
                if pw == Duration::ZERO {
                    self.fire_next_sequential(ctx, Some(current_id))?;
                } else {
                    self.seq_post_wait_until = Some(Instant::now() + pw);
                }
            } else {
                match cm {
                    ContinueMode::DoNotContinue => {
                        self.seq_done = true;
                    }
                    ContinueMode::AutoContinue => {
                        if pw == Duration::ZERO {
                            self.fire_next_sequential(ctx, Some(current_id))?;
                        } else {
                            self.seq_post_wait_until = Some(Instant::now() + pw);
                        }
                    }
                    ContinueMode::AutoFollow => {
                        // The fired child completed — fire the cue AFTER the one
                        // that Auto-Followed.
                        if uses_action_clock && !pw.is_zero() {
                            self.seq_post_wait_until = Some(Instant::now() + pw);
                        } else {
                            self.fire_next_sequential(ctx, Some(current_id))?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn shuffled_order(&self) -> Option<&[PlaylistOrderEntry]> {
        if self.advances_playlist() && self.playlist_auto_shuffle {
            self.playlist_run_order.as_ref().map(|order| order.entries.as_slice())
        } else {
            None
        }
    }

    fn traversal_len(&self) -> usize {
        self.shuffled_order().map_or(self.children.len(), <[_]>::len)
    }

    fn traversal_position(&self, id: CueId) -> Option<usize> {
        match self.shuffled_order() {
            Some(order) => order.iter().position(|entry| entry.id == id),
            None => self.children.iter().position(|child| child.id() == id),
        }
    }

    fn authored_index_at(&self, position: usize) -> Option<usize> {
        let Some(order) = self.shuffled_order() else {
            return (position < self.children.len()).then_some(position);
        };
        let entry = order.get(position)?;
        if self.children.get(entry.authored_index).is_some_and(|child| child.id() == entry.id) {
            return Some(entry.authored_index);
        }
        // Even a tree replacement outside the editing guard must resolve the
        // exact child ID before a playback action can reach it.
        self.children.iter().position(|child| child.id() == entry.id)
    }

    /// Next eligible child in the run's order. A looping Playlist scans at
    /// most one circuit, including when every child has been disarmed.
    fn next_armed_child(&self, after: Option<CueId>) -> Option<usize> {
        let start = match after {
            Some(id) => self.traversal_position(id)? + 1,
            None => 0,
        };
        let wrap = if self.mode == GroupMode::Playlist && self.playlist_loop { start } else { 0 };
        (start..self.traversal_len()).chain(0..wrap)
            .filter_map(|position| self.authored_index_at(position))
            .find(|&index| !self.children[index].is_disabled())
    }

    fn has_next_sequential_child(&self) -> bool {
        self.next_armed_child(self.seq_current_id).is_some()
    }

    /// Reset child playback to Standby and retain the open show's rotation.
    fn reset_children(&mut self) {
        self.playlist_cut = None;
        for child in &mut self.children {
            let _ = child.reset();
        }
        self.seq_current_id = None;
        self.seq_done = false;
        self.seq_post_wait_until = None;
        self.start_first_paused_at = None;
        self.start_first_playhead_held = false;
        self.start_first_child_changed = false;
        self.enter_boundary_wait = None;
        self.enter_wait_run = None;
        self.enter_paused_children.clear();
        self.pending_child_admission = None;
        self.playlist_playhead_held = false;
        self.playlist_run_order = None;
        self.output_withdrawn = false;
        self.delayed_start_admission = false;
        self.random_current_id = None;
        self.random_playhead_held = false;
    }

    // ── Playlist exclusivity ──────────────────────────────────────────────

    pub(crate) fn playlist_second_trigger(&self) -> Option<PlaylistSecondTrigger> {
        self.advances_playlist().then_some(self.playlist_second_trigger)
    }

    pub(crate) fn owns_playlist_standby(&self) -> bool {
        self.advances_playlist() && self.playlist_playhead_held
    }

    pub(crate) fn set_playlist_second_trigger(&mut self, trigger: PlaylistSecondTrigger) {
        self.playlist_second_trigger = trigger;
    }

    pub(crate) fn playlist_auto_shuffle(&self) -> Option<bool> {
        self.advances_playlist().then_some(self.playlist_auto_shuffle)
    }

    pub(crate) fn set_playlist_auto_shuffle(&mut self, enabled: bool) {
        if self.playlist_auto_shuffle != enabled {
            self.clear_playlist_shuffle();
            self.playlist_auto_shuffle = enabled;
        }
    }

    fn clear_playlist_shuffle(&mut self) {
        let Some(order) = self.playlist_run_order.take() else { return };
        self.seq_current_id = None;
        self.seq_done = false;
        if let Some(id) = order.entrance {
            self.set_active_child(&id);
        }
    }

    pub(crate) fn has_shuffled_playlist_run(&self) -> bool {
        self.shuffled_order().is_some()
    }

    pub(crate) fn playlist_run_order(&self) -> Option<Vec<CueId>> {
        self.shuffled_order().map(|order| order.iter().map(|entry| entry.id).collect())
    }

    fn prepare_playlist_shuffle(&mut self) {
        if !self.playlist_auto_shuffle {
            return;
        }
        if let Some(order) = &self.playlist_run_order {
            if order.entries.len() == self.children.len() && order.entries.iter().all(|entry| {
                self.children.get(entry.authored_index).is_some_and(|child| child.id() == entry.id)
            }) {
                return;
            }
            self.clear_playlist_shuffle();
        }
        let preferred = self.seq_done.then(|| self.next_armed_child(self.seq_current_id))
            .flatten().map(|index| self.children[index].id());
        let mut order: Vec<_> = self.children.iter().enumerate().map(|(authored_index, child)| {
            PlaylistOrderEntry { id: child.id(), authored_index }
        }).collect();
        self.ensure_rng_seeded();
        for last in (1..order.len()).rev() {
            let choice = (self.next_rand() % (last as u64 + 1)) as usize;
            order.swap(last, choice);
        }
        if let Some(position) = preferred.and_then(|id| order.iter().position(|entry| entry.id == id)) {
            order.rotate_left(position);
        }
        self.seq_current_id = None;
        self.playlist_run_order = Some(PlaylistOrder { entries: order, entrance: preferred });
    }

    fn previous_armed_playlist_child(&self, id: CueId) -> Option<usize> {
        let current = self.traversal_position(id)?;
        let wrap = if self.playlist_loop { self.traversal_len() } else { current + 1 };
        (0..current).rev().chain((current + 1..wrap).rev())
            .filter_map(|position| self.authored_index_at(position))
            .find(|&index| !self.children[index].is_disabled())
    }

    /// The outgoing child remains the automatic driver until the mixer accepts
    /// the replacement. The control thread never waits for an audio callback.
    fn settle_playlist_cut(&mut self, ctx: &CueContext, cancel: bool) {
        let Some(pending) = self.playlist_cut.as_ref() else { return };
        let incoming = self.children.iter().position(|child| {
            child.id() == pending.incoming
                && child.playing_voice_id() == Some(pending.voice)
                && child.play_generation() == pending.generation
        });
        if cancel
            || incoming.is_none_or(|i| !self.children[i].is_running() || self.children[i].is_disabled())
            || pending.requested.elapsed() >= Duration::from_secs(1)
        {
            pending.cut.cancel_pending();
        }
        if pending.cut.pending() {
            return;
        }
        let mut pending = self.playlist_cut.take().unwrap();
        if let Some(index) = incoming {
            if pending.cut.started() {
                // The mixer already cut the outgoing Audio voice. A Wait has
                // no output, but its countdown must stop at the handoff.
                if let Some(old) = self.children.iter_mut().find(|child| {
                    child.id() == pending.outgoing && child.play_generation() == pending.outgoing_generation
                }) {
                    if old.cue_type() == CueType::Wait {
                        let _ = old.stop(ctx);
                    } else {
                        ctx.emit(CueEvent::Stopped { cue_id: old.id() });
                    }
                    let _ = old.reset();
                }
                if let Some(audio) =
                    (self.children[index].as_mut() as &mut dyn std::any::Any).downcast_mut::<AudioCue>()
                {
                    audio.report_file_start(ctx, false);
                }
                self.seq_current_id = Some(pending.incoming);
                pending.adopted = true;
            } else {
                let _ = self.children[index].hard_stop(ctx);
                let _ = self.children[index].reset();
            }
        }
        if !cancel && !pending.adopted {
            crate::health::set(crate::health::HealthAlert::new(
                format!("playlist-step-{}", self.id),
                crate::health::HealthLevel::Warning,
                format!(
                    "{}: Audio step did not start. Check the main output and trigger the Playlist again.",
                    self.name
                ),
            ));
        }
        ctx.request_cue_refresh();
    }

    /// A directly addressed re-trigger can step an existing timed sequence
    /// without asking the main list for another standby.
    pub(crate) fn trigger_playlist_again(&mut self, ctx: &CueContext) -> Result<()> {
        if self.playlist_cut.is_some() {
            self.settle_playlist_cut(ctx, false);
            if self.playlist_cut.is_some() {
                return Ok(());
            }
        }
        self.reconcile_child_admission();
        if self.output_withdrawn || self.playlist_second_trigger == PlaylistSecondTrigger::Ignore {
            return self.go(ctx);
        }
        if !self.is_running()
            || self.in_pre_wait
            || self.pending_start_admission()
            || self.seq_post_wait_until.is_some()
            || self.seq_done
        {
            return Ok(());
        }
        let Some(current_id) = self.seq_current_id else { return Ok(()) };
        let Some(current) = self.children.iter().position(|child| child.id() == current_id) else {
            return Ok(());
        };
        if !self.children[current].is_running()
            || !self.children[current].is_action_started()
            || self.children[current].is_time_complete()
        {
            return Ok(());
        }
        let destination = match self.playlist_second_trigger {
            PlaylistSecondTrigger::Ignore => None,
            PlaylistSecondTrigger::Next => self.next_armed_child(Some(current_id)),
            PlaylistSecondTrigger::Previous => self.previous_armed_playlist_child(current_id),
        };
        let Some(destination) = destination.filter(|&index| index != current) else { return Ok(()) };
        let current_child = self.children[current].as_ref();
        let next_child = self.children[destination].as_ref();
        let simple_audio = |cue: &dyn Cue| {
            (cue as &dyn std::any::Any)
                .downcast_ref::<AudioCue>()
                .is_some_and(|audio| audio.supports_playlist_cut(ctx))
        };
        anyhow::ensure!((current_child.cue_type() == CueType::Wait || simple_audio(current_child))
            && ((next_child.cue_type() == CueType::Wait && next_child.pre_wait().is_zero()
                && next_child.duration().is_some_and(|duration| !duration.is_zero())) || simple_audio(next_child)),
            "Second Trigger needs a direct Wait or decoded mono/stereo File Audio on the main output, with zero destination Pre-Wait. Use a plain file played once at normal speed.");
        anyhow::ensure!(
            !next_child.is_paused()
                && !self
                    .children
                    .iter()
                    .enumerate()
                    .any(|(index, child)| { index != current && child.is_running() }),
            "Leave only the current Playlist child running before using Second Trigger."
        );

        if next_child.cue_type() == CueType::Audio {
            let outgoing_voice = current_child.playing_voice_id();
            let outgoing_generation = current_child.play_generation();
            anyhow::ensure!(
                current_child.cue_type() == CueType::Wait || outgoing_voice.is_some(),
                "The current Audio cue has no active voice."
            );
            let cut = Arc::new(PlaylistCut::new(outgoing_voice));
            let mut context = ctx.clone();
            context.playlist_cut = Some(Arc::clone(&cut));
            let next = &mut self.children[destination];
            next.go(&context)?;
            let voice =
                next.playing_voice_id().ok_or_else(|| anyhow!("Playlist Audio did not submit a voice"))?;
            self.playlist_cut = Some(PendingPlaylistCut {
                outgoing: current_id,
                outgoing_generation,
                incoming: next.id(),
                generation: next.play_generation(),
                voice,
                requested: Instant::now(),
                cut,
                adopted: false,
            });
            crate::health::clear(&format!("playlist-step-{}", self.id));
            return Ok(());
        }
        // A Wait accepts its entrance synchronously.
        self.children[destination].go(ctx)?;
        self.children[current].hard_stop(ctx)?;
        self.children[current].reset()?;
        self.seq_current_id = Some(self.children[destination].id());
        ctx.request_cue_refresh();
        Ok(())
    }

    /// Stop (and reset) every running/paused child except `keep_id`.  Used by
    /// Playlist mode so only one child is ever audible at a time.
    fn stop_other_children(&mut self, ctx: &CueContext, keep_id: CueId) {
        for child in &mut self.children {
            if child.id() != keep_id && (child.is_running() || child.is_paused()) {
                let _ = child.stop(ctx);
                let _ = child.reset();
            }
        }
    }

    // ── StartRandom PRNG (xorshift64*) + rotation ────────────────────────

    /// Lazily seed the PRNG.  xorshift cannot escape a zero state, so the seed is
    /// forced non-zero and mixed from a per-process counter and the wall clock.
    fn ensure_rng_seeded(&mut self) {
        if self.rng_state != 0 {
            return;
        }
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        // Golden-ratio mix so consecutive groups seeded in the same tick diverge.
        self.rng_state = (nanos ^ (n.wrapping_mul(0x9E37_79B9_7F4A_7C15))) | 1;
    }

    /// Advance the xorshift64* generator and return the next pseudo-random u64.
    fn next_rand(&mut self) -> u64 {
        let mut x = self.rng_state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng_state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn random_child_busy(cue: &dyn Cue) -> bool {
        cue.is_running() || cue.is_paused() || cue.playing_voice_id().is_some()
            || cue.child_cues().is_some_and(|children| {
                children.iter().any(|child| Self::random_child_busy(child.as_ref()))
            })
    }

    /// Keep unplayed busy children in the rotation until they can run.
    fn draw_random_child(&mut self) -> Option<usize> {
        if self.children.iter().all(|child| child.is_disabled()) {
            return None;
        }
        if self.children.iter().filter(|child| !child.is_disabled())
            .all(|child| self.random_played.contains(&child.id()))
        {
            self.random_played.clear();
        }
        let eligible: Vec<_> = self.children.iter().enumerate()
            .filter(|(_, child)| {
                !child.is_disabled() && !self.random_played.contains(&child.id())
                    && !Self::random_child_busy(child.as_ref())
            })
            .map(|(index, _)| index)
            .collect();
        if eligible.is_empty() {
            return None;
        }
        self.ensure_rng_seeded();
        let index = eligible[(self.next_rand() % eligible.len() as u64) as usize];
        self.random_played.insert(self.children[index].id());
        Some(index)
    }

    fn start_random_child(&mut self, ctx: &CueContext) {
        let Some(index) = self.draw_random_child() else { return };
        let child = &mut self.children[index];
        let id = child.id();
        match child.go(ctx) {
            Ok(()) => self.random_current_id = Some(id),
            Err(error) => {
                log::warn!("Group start_random: child '{}' failed to start: {error}", child.name());
                self.random_played.remove(&id);
            }
        }
        self.delayed_start_admission |= child.pending_start_admission();
        if child.has_withdrawn_output() {
            self.child_output_withdrawn(id);
        }
    }

    #[cfg(test)]
    fn seed_rng_for_test(&mut self, seed: u64) {
        self.rng_state = seed | 1;
        self.random_played.clear();
    }
}

impl Default for GroupCue {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Cue trait implementation
// ---------------------------------------------------------------------------

impl Cue for GroupCue {
    inherit_playback_fields!(
        state, started_at, action_started_at, start_reported, in_pre_wait, auto_continue_fired,
        seq_current_id, seq_done, seq_post_wait_until, start_first_paused_at,
        start_first_playhead_held, start_first_child_changed, enter_boundary_wait, enter_wait_run,
        enter_paused_children, pending_child_admission, playlist_cut,
        playlist_playhead_held, playlist_run_order,
        random_played, rng_state,
        random_current_id, random_playhead_held, delayed_start_admission, output_withdrawn,
    );

    // ── Identity ──────────────────────────────────────────────────────────

    fn id(&self) -> CueId { self.id }
    fn cue_type(&self) -> CueType { CueType::Group }
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

    // ── State ─────────────────────────────────────────────────────────────

    fn state(&self) -> CueState { self.state }

    // ── Lifecycle ─────────────────────────────────────────────────────────

    fn load(&mut self, ctx: &CueContext) -> Result<()> {
        for child in &mut self.children {
            child.load(ctx)?;
        }
        Ok(())
    }

    fn go(&mut self, ctx: &CueContext) -> Result<()> {
        self.check_enter_boundary(false)?;
        self.reconcile_child_admission();
        self.go_group(ctx)
    }

    fn stop(&mut self, ctx: &CueContext) -> Result<()> {
        self.settle_playlist_cut(ctx, true);
        for child in &mut self.children {
            if child.is_running() || child.is_paused() {
                let _ = child.stop(ctx);
            }
        }
        self.reset_children();
        self.state = CueState::Standby;
        self.started_at = None;
        self.action_started_at = None;
        self.in_pre_wait = false;
        self.auto_continue_fired = false;
        ctx.emit(CueEvent::Stopped { cue_id: self.id });
        Ok(())
    }

    fn pause(&mut self, ctx: &CueContext) -> Result<()> {
        self.settle_playlist_cut(ctx, true);
        let advancing_playlist = self.advances_playlist();
        if (self.uses_start_first_timing() || advancing_playlist || self.mode == GroupMode::Simultaneous) && self.is_running() {
            self.start_first_paused_at = Some(Instant::now());
            self.enter_paused_children.clear();
        }
        for child in &mut self.children {
            if child.is_running() {
                let _ = child.pause(ctx);
                if (self.mode == GroupMode::StartFirstAndEnter || advancing_playlist) && child.is_paused() {
                    self.enter_paused_children.insert(
                        child.id(), PausedChildPlayback::of(child.as_ref()),
                    );
                }
            }
        }
        self.state = CueState::Paused;
        Ok(())
    }

    fn resume(&mut self, ctx: &CueContext) -> Result<()> {
        if let Some(paused_at) = self.start_first_paused_at.take() {
            let pause = paused_at.elapsed();
            for instant in [&mut self.started_at, &mut self.action_started_at, &mut self.seq_post_wait_until] {
                *instant = instant.and_then(|instant| instant.checked_add(pause));
            }
        }
        let paused_children = std::mem::take(&mut self.enter_paused_children);
        let owns_pause = self.mode == GroupMode::StartFirstAndEnter || self.advances_playlist();
        for child in &mut self.children {
            // Another cue or a Load can replace the run this Group paused.
            if child.is_paused()
                && (!owns_pause
                    || paused_children.get(&child.id()).is_some_and(|paused| {
                        *paused == PausedChildPlayback::of(child.as_ref())
                    }))
            {
                let _ = child.resume(ctx);
            }
        }
        self.state = CueState::Running;
        Ok(())
    }

    fn hard_stop(&mut self, ctx: &CueContext) -> Result<()> {
        self.settle_playlist_cut(ctx, true);
        for child in &mut self.children {
            if child.is_running() || child.is_paused() {
                let _ = child.hard_stop(ctx);
            }
        }
        self.reset_children();
        self.state = CueState::Standby;
        self.started_at = None;
        self.action_started_at = None;
        self.in_pre_wait = false;
        self.auto_continue_fired = false;
        ctx.emit(CueEvent::Stopped { cue_id: self.id });
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.reset_children();
        self.state = CueState::Standby;
        self.started_at = None;
        self.action_started_at = None;
        self.in_pre_wait = false;
        self.auto_continue_fired = false;
        Ok(())
    }

    fn tick(&mut self, ctx: &CueContext) -> Result<()> {
        // ── Pre-wait ──────────────────────────────────────────────────────
        if self.in_pre_wait {
            if (self.uses_start_first_timing() || self.advances_playlist() || self.mode == GroupMode::Simultaneous)
                && !self.is_running() {
                return Ok(());
            }
            if self.started_at.map(|t| t.elapsed()).unwrap_or(Duration::ZERO) >= self.pre_wait {
                self.start_action(ctx)?;
            }
            return Ok(());
        }

        if self.state != CueState::Running {
            return Ok(());
        }

        match self.mode {
            // ── Simultaneous ──────────────────────────────────────────────
            GroupMode::Simultaneous => {
                for i in 0..self.children.len() {
                    // A finished child MUST be reset here: the event loop's
                    // completion detector is top-level only and never descends
                    // into group children.  Without this a child that plays out
                    // lingers in Running forever, so is_complete() never fires
                    // and the group (and its children) stay stuck.
                    if self.children[i].state() == CueState::Running && self.tick_child_at(i, ctx)? {
                        let _ = self.children[i].reset();
                    }
                }
            }

            // ── Sequential / Playlist ─────────────────────────────────────
            // Same driver logic; Playlist's exclusivity + loop live inside
            // `fire_next_sequential`, so no per-mode branch is needed here.
            GroupMode::Sequential | GroupMode::Playlist => {
                self.tick_sequential(ctx)?;
            }

            GroupMode::StartFirst | GroupMode::StartFirstAndEnter => self.tick_start_first(ctx)?,

            // ── StartRandom ───────────────────────────────────────────────
            // Exactly one child runs per GO; just tick it and clean up when it
            // finishes (never auto-advances — the next GO draws again).
            GroupMode::StartRandom => {
                for i in 0..self.children.len() {
                    let completed = match self.children[i].state() {
                        CueState::Running => self.tick_child_at(i, ctx)?,
                        CueState::Completed => true,
                        _ => false,
                    };
                    if completed {
                        let _ = self.children[i].reset();
                    }
                }
                if self.random_current_id.is_some_and(|id| {
                    !self.children.iter().any(|child| child.id() == id && child.state() != CueState::Standby)
                }) {
                    self.random_current_id = None;
                }
            }
        }

        // A nested sequence may have reached a protected child since this
        // group started. Give the same admission gate to its ancestors.
        self.delayed_start_admission = self.delayed_start_admission
            || self.children.iter().any(|child| child.pending_start_admission());
        self.report_action_start(ctx);
        Ok(())
    }

    fn is_action_started(&self) -> bool {
        !self.in_pre_wait && !self.pending_start_admission()
    }

    fn uses_action_clock_for_continue(&self) -> bool {
        self.uses_start_first_timing() || self.advances_playlist()
            || self.delayed_start_admission || self.output_withdrawn
    }

    fn pending_start_admission(&self) -> bool {
        self.pending_child_admission.is_some()
            || (self.delayed_start_admission && (self.in_pre_wait
                || self.children.iter().any(|child| child.pending_start_admission())))
    }

    fn requires_eq_gain_admission(&self) -> bool {
        self.children.iter().any(|child| !child.is_disabled() && child.requires_eq_gain_admission())
    }

    // ── Timing ────────────────────────────────────────────────────────────

    fn pre_wait(&self) -> Duration { self.pre_wait }
    fn set_pre_wait(&mut self, d: Duration) { self.pre_wait = d; }
    fn post_wait(&self) -> Duration { self.post_wait }
    fn set_post_wait(&mut self, d: Duration) { self.post_wait = d; }

    fn duration(&self) -> Option<Duration> {
        // Return None; the event loop uses is_complete() for Group completion.
        None
    }

    fn elapsed(&self) -> Duration {
        self.started_at.map(|t| self.start_first_paused_at.unwrap_or_else(Instant::now).saturating_duration_since(t))
            .unwrap_or(Duration::ZERO)
    }

    fn action_elapsed(&self) -> Duration {
        self.action_started_at.map(|t| self.start_first_paused_at.unwrap_or_else(Instant::now).saturating_duration_since(t))
            .unwrap_or(Duration::ZERO)
    }

    // ── Continue ──────────────────────────────────────────────────────────

    fn continue_mode(&self) -> ContinueMode { self.continue_mode }
    fn set_continue_mode(&mut self, mode: ContinueMode) { self.continue_mode = mode; }

    fn is_auto_continue_fired(&self) -> bool { self.auto_continue_fired }
    fn mark_auto_continue_fired(&mut self) { self.auto_continue_fired = true; }
    fn clear_auto_continue_fired(&mut self) { self.auto_continue_fired = false; }

    // ── Group support ─────────────────────────────────────────────────────

    fn is_complete(&self) -> bool {
        if self.state != CueState::Running || self.in_pre_wait {
            return false;
        }
        match self.mode {
            GroupMode::Simultaneous => {
                self.children.iter().all(|c| !c.is_running())
            }
            GroupMode::Sequential | GroupMode::Playlist => {
                self.seq_done
                    && !self.has_next_sequential_child()
                    && self.children.iter().all(|c| !c.is_running())
            }
            GroupMode::StartFirst | GroupMode::StartFirstAndEnter => self.seq_done
                && (self.mode == GroupMode::StartFirst || !self.has_next_sequential_child())
                && self.children.iter().all(|child| !child.is_running() && !child.is_paused()),
            GroupMode::StartRandom => {
                self.children.iter().all(|child| !Self::random_child_busy(child.as_ref()))
            }
        }
    }

    fn all_voice_ids(&self) -> Vec<CueId> {
        // A group owns the voices of every child, recursively (nested groups
        // included).  This makes a Group a valid target for a volume/pan Fade or
        // a Stop that needs the actual voice handles.
        self.children.iter().flat_map(|c| c.all_voice_ids()).collect()
    }

    fn child_output_withdrawn(&mut self, child_id: CueId) {
        if let Some(pending) = &self.playlist_cut {
            if pending.incoming == child_id && !pending.cut.started() {
                pending.cut.cancel_pending();
                return;
            }
            if pending.outgoing == child_id { pending.cut.cancel_pending(); }
        }
        if self.pending_child_admission.is_some_and(|(id, _)| id == child_id) {
            self.pending_child_admission = None;
        }
        self.output_withdrawn = true;
        self.mark_auto_continue_fired();
        if self.uses_start_first_timing() {
            self.seq_done = true;
            self.seq_post_wait_until = None;
            if self.mode == GroupMode::StartFirstAndEnter && self.seq_current_id == Some(child_id) {
                self.set_active_child(&child_id);
            }
            return;
        }
        if self.seq_current_id == Some(child_id) {
            self.set_active_child(&child_id);
        }
        if self.random_current_id == Some(child_id) {
            self.random_current_id = None;
        }
    }

    fn has_withdrawn_output(&self) -> bool {
        self.output_withdrawn
    }

    fn visual_voice_ids(&self) -> Vec<CueId> {
        // The pictures of every child, recursively — what a Fade on the group
        // (or a crossfade away from it) acts on.
        self.children.iter().flat_map(|c| c.visual_voice_ids()).collect()
    }

    fn child_cues(&self) -> Option<&[Box<dyn Cue>]> {
        Some(&self.children)
    }

    fn child_cues_mut(&mut self) -> Option<&mut Vec<Box<dyn Cue>>> {
        Some(&mut self.children)
    }

    fn take_children(&mut self) -> Option<Vec<Box<dyn Cue>>> {
        Some(std::mem::take(&mut self.children))
    }

    fn add_child(&mut self, child: Box<dyn Cue>, position: i32) -> Result<()> {
        if position < 0 || position as usize >= self.children.len() {
            self.children.push(child);
        } else {
            self.children.insert(position as usize, child);
        }
        Ok(())
    }

    fn remove_child(&mut self, id: &CueId) -> Result<Box<dyn Cue>> {
        let idx = self
            .children
            .iter()
            .position(|c| c.id() == *id)
            .ok_or_else(|| anyhow!("Child cue {:?} not found in group", id))?;
        Ok(self.children.remove(idx))
    }

    fn group_mode(&self) -> Option<GroupMode> {
        Some(self.mode)
    }

    fn set_group_mode(&mut self, mode: GroupMode) {
        if self.mode != mode {
            self.clear_playlist_shuffle();
        }
        self.mode = mode;
    }

    fn playlist_loop(&self) -> Option<bool> {
        Some(self.playlist_loop)
    }

    fn set_playlist_loop(&mut self, on: bool) {
        self.playlist_loop = on;
    }

    fn playlist_advance_playhead(&self) -> Option<bool> {
        (self.mode == GroupMode::Playlist).then_some(self.playlist_advance_playhead)
    }

    fn set_playlist_advance_playhead(&mut self, advance: bool) {
        if !advance {
            self.clear_playlist_shuffle();
        }
        self.playlist_advance_playhead = advance;
    }

    fn absorbs_go(&self) -> bool {
        if self.mode == GroupMode::StartFirstAndEnter || self.advances_playlist() {
            return self.is_paused() || self.holds_playhead();
        }
        if self.state != CueState::Running || self.in_pre_wait {
            return false;
        }
        match self.mode {
            GroupMode::Simultaneous | GroupMode::StartFirst | GroupMode::StartFirstAndEnter => false,
            GroupMode::Sequential | GroupMode::Playlist => self.has_next_sequential_child(),
            GroupMode::StartRandom => !self.random_advance_playhead
                && self.children.iter().any(|child| !child.is_disabled()),
        }
    }

    fn holds_playhead(&self) -> bool {
        if self.state != CueState::Running {
            return false;
        }
        if self.output_withdrawn || self.pending_start_admission() {
            return true;
        }
        match self.mode {
            GroupMode::Simultaneous | GroupMode::StartFirst => false,
            GroupMode::Sequential | GroupMode::StartFirstAndEnter =>
                self.in_pre_wait || self.has_next_sequential_child(),
            GroupMode::Playlist => !self.playlist_advance_playhead
                && (self.in_pre_wait || self.has_next_sequential_child()),
            GroupMode::StartRandom => !self.random_advance_playhead
                && (self.in_pre_wait || self.children.iter().any(|child| !child.is_disabled())),
        }
    }

    fn released_playhead(&self) -> bool {
        // Release once no eligible child remains, including after a pre-wait
        // in a group whose children are now all disarmed.
        if self.state != CueState::Running || self.in_pre_wait {
            return false;
        }
        if self.output_withdrawn || self.pending_start_admission() {
            return false;
        }
        match self.mode {
            GroupMode::Sequential => !self.has_next_sequential_child(),
            GroupMode::Playlist => !self.playlist_advance_playhead && !self.has_next_sequential_child(),
            GroupMode::StartRandom => !self.random_advance_playhead
                && !self.children.iter().any(|child| !child.is_disabled()),
            GroupMode::Simultaneous => self.delayed_start_admission,
            GroupMode::StartFirst | GroupMode::StartFirstAndEnter => false,
        }
    }

    fn mark_playhead_held(&mut self) {
        if self.mode == GroupMode::StartRandom && self.random_advance_playhead {
            self.random_playhead_held = true;
        } else if self.advances_playlist() {
            self.playlist_playhead_held = true;
        } else if self.uses_start_first_timing() {
            self.start_first_playhead_held = true;
        }
    }

    fn take_playhead_release(&mut self) -> bool {
        self.reconcile_child_admission();
        match self.mode {
            GroupMode::StartRandom if self.random_advance_playhead
                && self.is_action_started() && !self.output_withdrawn =>
                std::mem::take(&mut self.random_playhead_held),
            GroupMode::Playlist if self.playlist_advance_playhead && self.is_running()
                && self.is_action_started() && !self.output_withdrawn =>
                std::mem::take(&mut self.playlist_playhead_held),
            GroupMode::StartFirst | GroupMode::StartFirstAndEnter if self.is_running()
                && !self.in_pre_wait && !self.output_withdrawn && !self.pending_start_admission()
                && (self.mode == GroupMode::StartFirst || !self.has_next_sequential_child()) =>
                std::mem::take(&mut self.start_first_playhead_held),
            _ => false,
        }
    }

    fn active_child_id(&self) -> Option<CueId> {
        // Only the ordered modes have a meaningful "next child a GO will fire".
        // StartRandom has none (nothing armed in the UI); Simultaneous fires all.
        if !matches!(self.mode, GroupMode::Sequential | GroupMode::Playlist | GroupMode::StartFirstAndEnter) {
            return None;
        }
        if (self.mode == GroupMode::StartFirstAndEnter || self.advances_playlist())
            && self.pending_start_admission() && !self.in_pre_wait
        {
            if let Some(child) = self.children.iter().find(|child| child.pending_start_admission()) {
                return Some(child.id());
            }
        }
        if self.advances_playlist() && (self.is_running() || self.is_paused())
            && !self.output_withdrawn && !self.in_pre_wait
        {
            return None;
        }
        self.next_armed_child(self.seq_current_id).map(|i| self.children[i].id())
    }

    fn validate_programming(&self, _ctx: &super::validation::ValidationContext) -> Vec<super::validation::CueIssue> {
        self.check_enter_boundary(true).err().map(|error| {
            super::validation::CueIssue::error(error.to_string())
        }).into_iter().collect()
    }

    fn set_active_child(&mut self, child_id: &CueId) -> bool {
        if !matches!(self.mode, GroupMode::Sequential | GroupMode::Playlist | GroupMode::StartFirstAndEnter) {
            return false;
        }
        let Some(position) = self.traversal_position(*child_id) else { return false };
        if self.authored_index_at(position).is_none() {
            return false;
        }
        // Park immediately before the exact destination in traversal order,
        // including when a failed shuffled admission is retried.
        self.seq_current_id = (0..position).rev().find_map(|prior| self.authored_index_at(prior))
            .map(|index| self.children[index].id());
        // Pause here: a running sequence waits for the next GO instead of
        // auto-advancing.  A standby group ignores this — start_action clears
        // seq_done and fires from seq_current_id.
        self.seq_done = true;
        self.seq_post_wait_until = None;
        true
    }

    // ── Serialisation ─────────────────────────────────────────────────────

    fn serialize(&self) -> Value {
        let children: Vec<Value> = self.children.iter().map(|c| c.serialize()).collect();
        json!({
            "type": "group",
            "cue_type": "group",
            "id": self.id,
            "number": self.number,
            "name": self.name,
            "notes": self.notes,
            "color": self.color,
            "pre_wait_ms": self.pre_wait.as_millis() as u64,
            "post_wait_ms": self.post_wait.as_millis() as u64,
            "continue_mode": self.continue_mode,
            "group_mode": self.mode,
            "playlist_loop": self.playlist_loop,
            "playlist_advance_playhead": self.playlist_advance_playhead,
            "playlist_second_trigger": self.playlist_second_trigger,
            "playlist_auto_shuffle": self.playlist_auto_shuffle,
            "random_advance_playhead": self.random_advance_playhead,
            "children": children,
            "is_disabled": self.is_disabled,
        })
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Factory for [`GroupCue`].  Register this in [`super::registry::CueRegistry`].
pub struct GroupCueFactory;

impl CueFactory for GroupCueFactory {
    fn create(&self) -> Box<dyn Cue> {
        Box::new(GroupCue::new())
    }

    /// NOTE: This factory's `from_json` is intentionally never called.
    /// [`CueRegistry::from_json`] special-cases `CueType::Group` and calls
    /// [`GroupCue::from_json_with_registry`] directly so that children are
    /// deserialised with the registry.
    fn from_json(&self, _value: Value) -> Result<Box<dyn Cue>> {
        Ok(Box::new(GroupCue::new()))
    }
}

// ---------------------------------------------------------------------------
// Tests — playhead-handling logic (pure, no CueContext required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::memo_cue::MemoCue;

    /// A Sequential group with `n` memo children, marked Running.
    fn running_seq_group(n: usize) -> GroupCue {
        let mut g = GroupCue::new();
        g.mode = GroupMode::Sequential;
        for _ in 0..n {
            g.children.push(Box::new(MemoCue::new()));
        }
        g.state = CueState::Running;
        g
    }

    #[test]
    fn absorbs_and_holds_while_more_children_remain() {
        let mut g = running_seq_group(3);
        // Inner playhead on the first child → another child remains.
        g.seq_current_id = Some(g.children[0].id());
        assert!(g.absorbs_go());
        assert!(g.holds_playhead());
        assert!(!g.released_playhead());
    }

    #[test]
    fn enter_boundary_post_wait_after_child_completion_uses_the_group_pause_clock() {
        use crate::cue::wait_cue::WaitCue;
        let (ctx, _, _) = crate::test_support::recording_context();
        for mode in [ContinueMode::AutoContinue, ContinueMode::AutoFollow] {
            let mut wait = WaitCue::new(); // five-second action
            wait.set_continue_mode(mode);
            wait.set_post_wait(Duration::from_secs(30));
            let id = wait.id();
            let mut group = GroupCue::new();
            group.mode = GroupMode::StartFirstAndEnter;
            group.children.push(Box::new(wait));
            assert_eq!(group.go_from_main_list(&ctx).unwrap(), Some(id));
            WaitCue::from_cue_mut(group.children[0].as_mut()).unwrap()
                .advance_timer_for_test(Duration::from_secs(6));
            group.tick(&ctx).unwrap();
            assert_eq!(group.children[0].state(), CueState::Standby);
            assert_eq!(group.enter_wait_ready(id), Some(false));

            group.pause(&ctx).unwrap();
            let pause = Duration::from_secs(40);
            group.start_first_paused_at = group.start_first_paused_at.map(|at| at - pause);
            group.seq_post_wait_until = group.seq_post_wait_until.map(|at| at - pause);
            assert_eq!(group.enter_wait_ready(id), Some(false));
            group.resume(&ctx).unwrap();
            assert_eq!(group.enter_wait_ready(id), Some(false));
            group.seq_post_wait_until = Some(Instant::now());
            assert_eq!(group.enter_wait_ready(id), Some(true));
        }
    }

    #[test]
    fn start_first_only_releases_an_actual_admission_hold_once() {
        let mut group = running_seq_group(2);
        group.mode = GroupMode::StartFirst;
        group.in_pre_wait = true;
        group.delayed_start_admission = true;
        assert!(group.holds_playhead());
        group.mark_playhead_held();
        assert!(!group.take_playhead_release());

        group.in_pre_wait = false;
        assert!(!group.holds_playhead());
        assert!(group.take_playhead_release());
        assert!(!group.take_playhead_release());
        assert!(!group.released_playhead(), "reparking on an active group is deliberate");
    }

    #[test]
    fn start_first_preserves_late_post_wait_after_the_child_finishes() {
        let (ctx, _, _) = crate::test_support::recording_context();
        let mut group = GroupCue::new();
        group.mode = GroupMode::StartFirst;
        let mut first = MemoCue::new();
        first.set_continue_mode(ContinueMode::AutoContinue);
        first.set_post_wait(Duration::from_secs(30));
        group.children.push(Box::new(first));
        group.children.push(Box::new(MemoCue::new()));
        group.go(&ctx).unwrap();

        assert_eq!(group.children[0].state(), CueState::Standby);
        assert_eq!(group.children[1].state(), CueState::Standby);
        assert!(!group.is_complete());
        let deadline = group.seq_post_wait_until.expect("remaining Post-Wait");
        group.pause(&ctx).unwrap();
        group.start_first_paused_at = Instant::now().checked_sub(Duration::from_secs(2));
        group.resume(&ctx).unwrap();
        assert!(group.seq_post_wait_until.unwrap() >= deadline + Duration::from_secs(2));
        group.stop(&ctx).unwrap();
        assert!(group.seq_post_wait_until.is_none());
    }

    #[test]
    fn current_output_loss_parks_ordered_groups_for_manual_retry() {
        for mode in [GroupMode::Sequential, GroupMode::Playlist] {
            let mut group = running_seq_group(2);
            group.mode = mode;
            let lost = group.children[0].id();
            group.seq_current_id = Some(lost);
            group.seq_post_wait_until = Some(Instant::now());
            group.child_output_withdrawn(lost);
            assert_eq!(group.active_child_id(), Some(lost));
            assert!(group.seq_done);
            assert!(group.seq_post_wait_until.is_none());
            assert!(group.has_withdrawn_output());
            assert!(group.holds_playhead());
        }
    }

    #[test]
    fn releases_playhead_on_last_child() {
        let mut g = running_seq_group(3);
        g.seq_current_id = Some(g.children[2].id()); // last child fired
        assert!(!g.absorbs_go());
        assert!(!g.holds_playhead());
        assert!(g.released_playhead());
    }

    #[test]
    fn single_child_group_releases_immediately() {
        let mut g = running_seq_group(1);
        g.seq_current_id = Some(g.children[0].id());
        assert!(!g.holds_playhead());
        assert!(g.released_playhead());
        assert!(!g.absorbs_go());
    }

    #[test]
    fn holds_playhead_during_pre_wait() {
        let mut g = running_seq_group(2);
        g.in_pre_wait = true;
        g.seq_current_id = None;
        assert!(g.holds_playhead());
        assert!(!g.released_playhead());
        assert!(!g.absorbs_go()); // a GO during pre-wait is not absorbed
    }

    #[test]
    fn active_child_id_is_next_after_inner_playhead() {
        let mut g = running_seq_group(3);
        g.seq_current_id = Some(g.children[0].id());
        let expected = g.children[1].id();
        assert_eq!(g.active_child_id(), Some(expected));
        g.seq_current_id = Some(g.children[2].id());
        assert_eq!(g.active_child_id(), None); // no child after the last
    }

    #[test]
    fn group_timeline_uses_a_frozen_native_clock_while_paused() {
        use crate::cue::wait_cue::WaitCue;
        let (ctx, _, _) = crate::test_support::recording_context();
        let mut group = GroupCue::new();
        group.mode = GroupMode::Simultaneous;
        group.pre_wait = Duration::from_secs(15);
        group.children.push(Box::new(WaitCue::new()));
        group.go(&ctx).unwrap();
        group.pause(&ctx).unwrap();

        let now = Instant::now();
        group.started_at = Some(now - Duration::from_secs(30));
        group.start_first_paused_at = Some(now - Duration::from_secs(20));
        group.tick(&ctx).unwrap();
        assert_eq!(group.elapsed(), Duration::from_secs(10));
        assert!(!group.is_action_started());
        assert!(!group.children[0].is_running());

        group.resume(&ctx).unwrap();
        group.tick(&ctx).unwrap();
        assert!(!group.is_action_started());
        group.started_at = Some(now - Duration::from_secs(20));
        group.tick(&ctx).unwrap();
        assert!(group.children[0].is_running());
        group.pause(&ctx).unwrap();
        group.action_started_at = Some(now - Duration::from_secs(12));
        group.start_first_paused_at = Some(now - Duration::from_secs(9));
        assert_eq!(group.action_elapsed(), Duration::from_secs(3));
    }

    #[test]
    fn simultaneous_group_never_holds_playhead() {
        let mut g = running_seq_group(2);
        g.mode = GroupMode::Simultaneous;
        assert!(!g.holds_playhead());
        assert!(!g.released_playhead());
        assert!(!g.absorbs_go());
        assert_eq!(g.active_child_id(), None);
    }

    #[test]
    fn set_active_child_parks_inner_playhead() {
        let mut g = running_seq_group(3);
        let second = g.children[1].id();
        assert!(g.set_active_child(&second));
        assert_eq!(g.active_child_id(), Some(second));

        // Parking on the first child clears the inner playhead.
        let first = g.children[0].id();
        assert!(g.set_active_child(&first));
        assert_eq!(g.seq_current_id, None);
        assert_eq!(g.active_child_id(), Some(first));
    }

    #[test]
    fn set_active_child_rejected_for_simultaneous() {
        let mut g = running_seq_group(2);
        g.mode = GroupMode::Simultaneous;
        let child = g.children[0].id();
        assert!(!g.set_active_child(&child));
    }

    // ── New modes: Playlist + StartRandom ─────────────────────────────────

    fn running_group(mode: GroupMode, n: usize) -> GroupCue {
        let mut g = GroupCue::new();
        g.mode = mode;
        for _ in 0..n {
            g.children.push(Box::new(MemoCue::new()));
        }
        g.state = CueState::Running;
        g
    }

    #[test]
    fn playlist_shuffle_drives_completion_addressed_steps_and_wrap() {
        use crate::cue::wait_cue::{WaitCue, WaitCueFactory};

        let (ctx, _, _) = crate::test_support::recording_context();
        let mut group = GroupCue::new();
        group.mode = GroupMode::Playlist;
        group.playlist_loop = true;
        group.set_playlist_auto_shuffle(true);
        group.seed_rng_for_test(5);
        for _ in 0..4 {
            group.children.push(Box::new(WaitCue::new()));
        }
        let saved = group.serialize();
        let authored: Vec<_> = group.children.iter().map(|child| child.id()).collect();
        let [north, south, east, west] = [authored[0], authored[1], authored[2], authored[3]];
        let expected = [west, south, east, north];

        group.go(&ctx).unwrap();
        assert_eq!(group.playlist_run_order().unwrap(), expected);
        assert_eq!(group.seq_current_id, Some(west));
        group.children[3].stop(&ctx).unwrap();
        group.tick(&ctx).unwrap();
        assert_eq!(group.seq_current_id, Some(south));

        group.set_playlist_second_trigger(PlaylistSecondTrigger::Previous);
        group.trigger_playlist_again(&ctx).unwrap();
        assert_eq!(group.seq_current_id, Some(west));
        group.trigger_playlist_again(&ctx).unwrap();
        assert_eq!(group.seq_current_id, Some(north), "Previous wraps the same order");

        group.set_playlist_second_trigger(PlaylistSecondTrigger::Next);
        group.trigger_playlist_again(&ctx).unwrap();
        assert_eq!(group.seq_current_id, Some(west), "Next wraps the same order");
        assert_eq!(group.serialize()["children"], saved["children"]);
        assert_eq!(group.serialize()["playlist_auto_shuffle"], true);
        assert!(group.serialize().get("playlist_run_order").is_none());
        let mut registry = CueRegistry::new();
        registry.register(CueType::Wait, Box::new(WaitCueFactory));
        let reopened = GroupCue::from_json_with_registry(&group.serialize(), &registry).unwrap();
        let reopened = (reopened.as_ref() as &dyn std::any::Any).downcast_ref::<GroupCue>().unwrap();
        assert_eq!(reopened.playlist_auto_shuffle(), Some(true));
        assert!(reopened.playlist_run_order().is_none());
        assert_eq!(reopened.children.iter().map(|child| child.id()).collect::<Vec<_>>(), authored);

        group.stop(&ctx).unwrap();
        assert!(group.playlist_run_order().is_none());
        group.seed_rng_for_test(3);
        group.go(&ctx).unwrap();
        assert_eq!(group.playlist_run_order().unwrap(), [east, north, south, west]);
        group.stop(&ctx).unwrap();
        assert!(group.set_active_child(&south));
        group.seed_rng_for_test(3);
        group.go(&ctx).unwrap();
        assert_eq!(group.playlist_run_order().unwrap(), [south, west, east, north]);
        assert_eq!(group.seq_current_id, Some(south));

        group.stop(&ctx).unwrap();
        group.set_playlist_auto_shuffle(false);
        group.go(&ctx).unwrap();
        assert!(group.playlist_run_order().is_none());
        assert_eq!(group.seq_current_id, Some(north));
    }

    #[test]
    fn playlist_audio_step_keeps_the_driver_until_admitted_and_pause_cancels_it() {
        use crate::cue::wait_cue::WaitCue;

        let (ctx, _, _) = crate::test_support::recording_context();
        let mut group = GroupCue::new();
        group.mode = GroupMode::Playlist;
        group.set_playlist_second_trigger(PlaylistSecondTrigger::Next);
        let wait = WaitCue::new();
        let mut audio = AudioCue::new();
        audio.accept_preloaded_audio(Arc::new(vec![0.1; 96_000]), 1, 48_000, Duration::from_secs(2));
        let (old, new) = (wait.id(), audio.id());
        group.children = vec![Box::new(wait), Box::new(audio)];
        group.go(&ctx).unwrap();
        let old_generation = group.children[0].play_generation();
        group.trigger_playlist_again(&ctx).unwrap();
        let first = group.playlist_cut.as_ref().unwrap().cut.clone();
        group.pause(&ctx).unwrap();
        assert!(!first.admit(&[], (0, 1)), "a cancelled entrance cannot sound late");
        assert_eq!(group.seq_current_id, Some(old));
        assert!(group.children[0].is_paused());
        assert_eq!(group.children[1].state(), CueState::Standby);
        group.resume(&ctx).unwrap();
        assert_eq!(group.children[0].play_generation(), old_generation);

        group.trigger_playlist_again(&ctx).unwrap();
        let retry = group.playlist_cut.as_ref().unwrap().cut.clone();
        assert_eq!(group.seq_current_id, Some(old));
        assert!(retry.admit(&[], (0, 1)));
        group.tick(&ctx).unwrap();
        assert_eq!(group.seq_current_id, Some(new));
        assert_eq!(group.children[0].state(), CueState::Standby);
        assert!(group.children[1].is_running());

        group.set_playlist_second_trigger(PlaylistSecondTrigger::Previous);
        group.trigger_playlist_again(&ctx).unwrap();
        assert_eq!(group.seq_current_id, Some(old));
        assert_eq!(group.children[1].state(), CueState::Standby);
    }

    #[test]
    fn shuffled_playlist_holds_the_exact_protected_entrance_and_retries_it() {
        use crate::cue::{audio_cue::AudioCue, wait_cue::WaitCue};
        use crate::engine::device_manager::OutputPatch;

        let (mut ctx, _, _) = crate::test_support::recording_context();
        let mut patch = OutputPatch::new("Venue", "hw:CARD=Venue", vec![0, 1]);
        patch.require_named_device = true;
        ctx.default_patch_id = Some(patch.id);
        ctx.output_patches = std::sync::Arc::new(vec![patch]);
        let mut audio = AudioCue::new();
        audio.accept_preloaded_audio(
            std::sync::Arc::new(vec![0.0; 4_800]), 1, 48_000, Duration::from_millis(100),
        );
        audio.set_pre_wait(Duration::from_secs(30));
        let wait = WaitCue::new();
        let [wait_id, audio_id] = [wait.id(), audio.id()];
        let mut group = GroupCue::new();
        group.mode = GroupMode::Playlist;
        group.set_playlist_auto_shuffle(true);
        group.seed_rng_for_test(5);
        group.set_pre_wait(Duration::from_secs(30));
        group.children = vec![Box::new(wait), Box::new(audio)];

        group.go(&ctx).unwrap();
        assert_eq!(group.playlist_run_order().unwrap(), [audio_id, wait_id]);
        assert!(group.pending_start_admission());
        assert!(group.holds_playhead(), "Group Pre-Wait checks the shuffled first child");
        group.mark_playhead_held();
        group.set_pre_wait(Duration::ZERO);
        group.tick(&ctx).unwrap();
        assert_eq!(group.active_child_id(), Some(audio_id));
        assert!(!group.take_playhead_release());

        group.children[1].stop(&ctx).unwrap();
        group.tick(&ctx).unwrap();
        assert_eq!(group.active_child_id(), Some(audio_id));
        assert!(!group.children[0].is_running());
        assert!(!group.take_playhead_release());
        group.go(&ctx).unwrap();
        assert_eq!(group.playlist_run_order().unwrap(), [audio_id, wait_id]);
        group.children[1].set_pre_wait(Duration::ZERO);
        group.tick(&ctx).unwrap();
        assert!(group.children[1].playing_voice_id().is_some());
        assert!(group.take_playhead_release());
        assert!(!group.take_playhead_release());
    }

    #[test]
    fn start_random_plays_each_child_once_before_repeating() {
        let mut g = running_group(GroupMode::StartRandom, 4);
        g.seed_rng_for_test(12345);
        let cycle1: Vec<usize> = (0..4).map(|_| g.draw_random_child().unwrap()).collect();
        let mut s1 = cycle1.clone();
        s1.sort();
        assert_eq!(s1, vec![0, 1, 2, 3], "every child drawn exactly once per cycle");
        // The bag refills → the next cycle is another full permutation.
        let cycle2: Vec<usize> = (0..4).map(|_| g.draw_random_child().unwrap()).collect();
        let mut s2 = cycle2.clone();
        s2.sort();
        assert_eq!(s2, vec![0, 1, 2, 3]);
    }

    #[test]
    fn start_random_resets_the_rotation_after_all_armed_children_play() {
        let mut g = running_group(GroupMode::StartRandom, 3);
        g.seed_rng_for_test(7);
        for _ in 0..3 {
            g.draw_random_child();
        }
        assert_eq!(g.random_played.len(), 3);
        assert!(g.draw_random_child().is_some());
        assert_eq!(g.random_played.len(), 1);
    }

    #[test]
    fn start_random_is_deterministic_for_a_seed() {
        let mut a = running_group(GroupMode::StartRandom, 5);
        let mut b = running_group(GroupMode::StartRandom, 5);
        a.seed_rng_for_test(999);
        b.seed_rng_for_test(999);
        let sa: Vec<usize> = (0..10).map(|_| a.draw_random_child().unwrap()).collect();
        let sb: Vec<usize> = (0..10).map(|_| b.draw_random_child().unwrap()).collect();
        assert_eq!(sa, sb, "same seed → same draw sequence");
    }

    #[test]
    fn playlist_trait_methods_mirror_sequential() {
        let mut g = running_group(GroupMode::Playlist, 3);
        g.set_playlist_advance_playhead(false);
        g.seq_current_id = Some(g.children[0].id()); // more children remain
        assert!(g.absorbs_go());
        assert!(g.holds_playhead());
        assert!(!g.released_playhead());
        let expected = g.children[1].id();
        assert_eq!(g.active_child_id(), Some(expected));
        // Last child, no loop → releases the Playhead like Sequential.
        g.seq_current_id = Some(g.children[2].id());
        assert!(g.released_playhead());
        assert!(!g.holds_playhead());
    }

    #[test]
    fn looping_playlist_never_releases_playhead() {
        let mut g = running_group(GroupMode::Playlist, 3);
        g.set_playlist_advance_playhead(false);
        g.playlist_loop = true;
        g.seq_current_id = Some(g.children[2].id()); // last child fired
        assert!(g.absorbs_go(), "a looping playlist keeps absorbing GO");
        assert!(g.holds_playhead());
        assert!(!g.released_playhead(), "never releases — loops until stopped");
    }

    #[test]
    fn start_random_standby_modes_can_both_repark_and_fire() {
        let mut g = running_group(GroupMode::StartRandom, 3);
        assert!(!g.absorbs_go());
        assert!(!g.holds_playhead());
        assert!(!g.released_playhead(), "a later manual park must stay parked");
        g.random_advance_playhead = false;
        assert!(g.absorbs_go(), "each GO fires another random child");
        assert!(g.holds_playhead());
        assert!(!g.released_playhead());
        assert_eq!(g.active_child_id(), None, "nothing armed for a random group");
        let child = g.children[0].id();
        assert!(!g.set_active_child(&child), "cannot park a random group");
    }

    #[test]
    fn advancing_random_group_releases_required_admission_hold_once() {
        let mut group = running_group(GroupMode::StartRandom, 2);
        group.in_pre_wait = true;
        group.delayed_start_admission = true;
        assert!(group.holds_playhead());
        group.mark_playhead_held();
        assert!(!group.take_playhead_release());
        group.in_pre_wait = false;
        assert!(group.take_playhead_release());
        assert!(!group.take_playhead_release());
    }

    #[test]
    fn externally_reaped_child_releases_the_random_group() {
        use crate::cue::wait_cue::WaitCue;
        let (context, _, _) = crate::test_support::recording_context();
        let mut group = running_group(GroupMode::StartRandom, 0);
        group.children.push(Box::new(WaitCue::new()));
        group.go(&context).unwrap();
        assert!(group.random_current_id.is_some());
        assert!(!group.is_complete());
        group.children[0].reset().unwrap();
        group.tick(&context).unwrap();
        assert_eq!(group.random_current_id, None);
        assert!(group.is_complete());
    }

    #[test]
    fn random_group_tracks_played_children_across_reordering_and_removal() {
        let mut group = running_group(GroupMode::StartRandom, 4);
        group.seed_rng_for_test(7);
        let first = group.draw_random_child().unwrap();
        let played = group.children[first].id();
        group.random_current_id = Some(played);
        let moved = (first + 1) % group.children.len();
        group.children.swap(first, moved);
        for _ in 0..3 {
            let index = group.draw_random_child().unwrap();
            assert_ne!(group.children[index].id(), played);
        }
        group.remove_child(&played).unwrap();
        assert!(group.is_complete());
    }

    #[test]
    fn start_random_busy_unplayed_children_keep_their_turn() {
        let mut random = running_group(GroupMode::StartRandom, 2);
        let mut busy = GroupCue::new();
        let busy_id = busy.id;
        busy.state = CueState::Paused;
        random.children.push(Box::new(busy));

        for _ in 0..2 {
            let index = random.draw_random_child().unwrap();
            assert_ne!(random.children[index].id(), busy_id);
        }
        assert_eq!(random.draw_random_child(), None);
        assert!(!random.random_played.contains(&busy_id));
        random.children[2].reset().unwrap();
        assert_eq!(random.draw_random_child(), Some(2));
        assert_eq!(random.random_played.len(), 3);
    }

    #[test]
    fn start_random_keeps_its_rotation_until_the_group_is_reopened() {
        let (context, _, _) = crate::test_support::recording_context();
        let mut group = running_group(GroupMode::StartRandom, 4);
        for expected in 1..=4 {
            group.go(&context).unwrap();
            group.tick(&context).unwrap();
            assert!(group.is_complete());
            group.reset().unwrap(); // The event loop resets after ordinary completion.
            assert_eq!(group.random_played.len(), expected);
        }
        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(crate::cue::memo_cue::MemoCueFactory));
        let reopened = GroupCue::from_json_with_registry(&group.serialize(), &registry).unwrap();
        let reopened = (reopened.as_ref() as &dyn std::any::Any).downcast_ref::<GroupCue>().unwrap();
        assert!(reopened.random_played.is_empty());
        assert!(reopened.random_advance_playhead);
    }

    #[test]
    fn start_random_transport_preserves_saved_legacy_and_advances_new_groups() {
        use crate::{cue::wait_cue::{WaitCue, WaitCueFactory}, show::{cue_list::CueList, transport::Transport}};
        let mut registry = CueRegistry::new();
        registry.register(CueType::Wait, Box::new(WaitCueFactory));
        for legacy in [false, true] {
            let (context, _, _) = crate::test_support::recording_context();
            let mut group = GroupCue::new();
            group.mode = GroupMode::StartRandom;
            group.children.push(Box::new(WaitCue::new()));
            let group_id = group.id;
            let mut json = group.serialize();
            if legacy {
                json.as_object_mut().unwrap().remove("random_advance_playhead");
            }
            let group = GroupCue::from_json_with_registry(&json, &registry).unwrap();
            assert_eq!(group.serialize()["random_advance_playhead"], !legacy);
            let following = MemoCue::new();
            let next = following.id();
            let mut list = CueList::new("Start Random practice");
            list.push(group);
            list.push(Box::new(following));
            let mut transport = Transport::new(context);
            transport.go(&mut list).unwrap();
            assert_eq!(list.playhead_cue_id, Some(if legacy { group_id } else { next }));
            assert!(list.get(&group_id).unwrap().child_cues().unwrap()[0].is_running());
            if !legacy {
                list.set_playhead(Some(group_id)).unwrap();
                assert!(!crate::show::event_loop::release_admitted_playhead(&mut list));
                assert_eq!(list.playhead_cue_id, Some(group_id));
            }
        }
    }

    #[test]
    fn random_group_completes_after_an_instant_child() {
        let (context, _, _) = crate::test_support::recording_context();
        let mut group = running_group(GroupMode::StartRandom, 1);
        let played = group.children[0].id();

        group.go(&context).unwrap();
        assert_eq!(group.random_current_id, Some(played));
        assert_eq!(group.children[0].state(), CueState::Completed);
        group.tick(&context).unwrap();
        assert_eq!(group.children[0].state(), CueState::Standby);
        assert!(group.is_complete());
    }

    #[test]
    fn is_complete_playlist_loop_never_completes() {
        let mut g = running_group(GroupMode::Playlist, 3);
        g.playlist_loop = true;
        g.seq_done = true; // even if the sequence thinks it's exhausted
        assert!(!g.is_complete(), "a looping playlist never completes on its own");
    }

    #[test]
    fn is_complete_playlist_no_loop_matches_sequential() {
        let mut g = running_group(GroupMode::Playlist, 2);
        g.seq_current_id = Some(g.children[1].id()); // last child
        g.seq_done = true;
        assert!(g.is_complete());
    }
}
