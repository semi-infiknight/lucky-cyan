//! [`WaitCue`] — pauses the cue sequence for a configurable duration.
//!
//! When triggered, the Wait cue starts a countdown timer and stays in
//! [`CueState::Running`] until the timer expires.  The 30 fps event loop
//! detects completion via [`Cue::duration`] / [`Cue::action_elapsed`] — no
//! engine interaction required.
//!
//! Pause and resume are supported: the timer freezes on pause and resumes
//! exactly where it left off.

use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    engine::timecode_types::TcOnStop,
    show::timecode_loss::{ClockEdge, ClockRun},
};

use super::{
    context::{CueContext, CueEvent},
    traits::{Cue, CueFactory},
    types::{ContinueMode, CueColor, CueId, CueState, CueType},
};

/// A cue that waits for a configurable duration before the sequence continues.
pub struct WaitCue {
    id: CueId,
    name: String,
    number: Option<String>,
    notes: String,
    color: CueColor,
    state: CueState,
    play_generation: u64,
    start_reported: bool,
    clock_run: Option<(Uuid, ClockRun)>,
    auto_continue_fired: bool,
    continue_mode: ContinueMode,
    pre_wait: Duration,
    post_wait: Duration,
    /// The user-configured wait duration.
    wait_duration: Duration,
    /// Elapsed time accumulated before the most recent pause.
    elapsed_before_pause: Duration,
    is_disabled: bool,
    /// Wall-clock instant when `go()` or `resume()` was last called.
    started_at: Option<Instant>,
}

impl WaitCue {
    /// Create a new Wait cue with a 5-second default duration.
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::from("Wait"),
            number: None,
            notes: String::new(),
            color: CueColor::Orange,
            state: CueState::Standby,
            play_generation: 0,
            start_reported: false,
            clock_run: None,
            auto_continue_fired: false,
            continue_mode: ContinueMode::AutoFollow,
            pre_wait: Duration::ZERO,
            post_wait: Duration::ZERO,
            wait_duration: Duration::from_secs(5),
            elapsed_before_pause: Duration::ZERO,
            is_disabled: false,
            started_at: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn advance_timer_for_test(&mut self, by: Duration) {
        if let Some(started) = &mut self.started_at { *started -= by; }
    }

    #[cfg(test)]
    pub(crate) fn running_for_test() -> Self {
        Self {
            state: CueState::Running,
            started_at: Some(Instant::now()),
            ..Self::new()
        }
    }

    pub(crate) fn from_cue_mut(cue: &mut dyn Cue) -> Option<&mut Self> {
        (cue as &mut dyn std::any::Any).downcast_mut()
    }

    pub(crate) fn follow_clock(&mut self, list: Uuid, generation: u64) {
        if self.is_running() && self.play_generation == generation {
            self.clock_run = Some((list, ClockRun::Controlled));
        }
    }

    pub(crate) fn detach_clock(&mut self) {
        self.clock_run = None;
    }

    pub(crate) fn join_from_timecode(&mut self, elapsed: Duration, context: &CueContext) -> bool {
        if elapsed >= self.pre_wait.saturating_add(self.wait_duration) {
            return false;
        }
        self.start_with_elapsed(elapsed, context);
        true
    }

    fn start_with_elapsed(&mut self, elapsed: Duration, context: &CueContext) {
        self.detach_clock();
        self.play_generation = self.play_generation.wrapping_add(1);
        self.auto_continue_fired = false;
        self.elapsed_before_pause = elapsed;
        self.started_at = Some(Instant::now());
        self.state = CueState::Running;
        context.emit(CueEvent::ActionStarted { cue_id: self.id });
        self.start_reported = elapsed >= self.pre_wait && context.mark_action_started(self.id);
    }

    /// Return the actual transition so the desk can publish it.
    pub(crate) fn apply_clock_edge(
        &mut self,
        list: Uuid,
        edge: ClockEdge,
        policy: TcOnStop,
        context: &CueContext,
    ) -> Result<Option<CueState>> {
        let owner = self.clock_run.filter(|(owner, _)| *owner == list).map(|(_, run)| run);
        match (edge, policy, owner, self.state) {
            (ClockEdge::Lost(at), TcOnStop::Pause, Some(ClockRun::Controlled), CueState::Running) => {
                let at = self.started_at.map_or(at, |started_at| at.max(started_at));
                self.pause_at(at);
                self.clock_run = Some((list, ClockRun::PausedByClock { at }));
            }
            (ClockEdge::Lost(_), TcOnStop::Stop, Some(ClockRun::Controlled), CueState::Running) => {
                self.stop(context)?;
            }
            (ClockEdge::Returned(at), _, Some(ClockRun::PausedByClock { at: paused_at }), CueState::Paused) => {
                self.resume_at(at.max(paused_at));
                self.clock_run = Some((list, ClockRun::Controlled));
            }
            _ => return Ok(None),
        }
        Ok(Some(self.state()))
    }

    fn pause_at(&mut self, at: Instant) {
        if self.state == CueState::Running {
            if let Some(started_at) = self.started_at.take() {
                self.elapsed_before_pause += at.saturating_duration_since(started_at);
            }
            self.state = CueState::Paused;
        }
    }

    fn resume_at(&mut self, at: Instant) {
        if self.state == CueState::Paused {
            self.started_at = Some(at);
            self.state = CueState::Running;
        }
    }

    fn elapsed_at(&self, at: Instant) -> Duration {
        match self.state {
            CueState::Running => {
                self.elapsed_before_pause
                    + self.started_at.map(|start| at.saturating_duration_since(start)).unwrap_or(Duration::ZERO)
            }
            CueState::Paused => self.elapsed_before_pause,
            _ => Duration::ZERO,
        }
    }
}

impl Default for WaitCue {
    fn default() -> Self {
        Self::new()
    }
}

impl Cue for WaitCue {
    inherit_playback_fields!(
        state, play_generation, start_reported, clock_run, auto_continue_fired, started_at, elapsed_before_pause
    );

    fn id(&self) -> CueId { self.id }

    fn cue_type(&self) -> CueType { CueType::Wait }

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

    fn load(&mut self, _context: &CueContext) -> Result<()> {
        Ok(())
    }

    fn go(&mut self, context: &CueContext) -> Result<()> {
        self.start_with_elapsed(Duration::ZERO, context);
        Ok(())
    }

    fn tick(&mut self, context: &CueContext) -> Result<()> {
        if !self.start_reported && self.state == CueState::Running && self.is_action_started() {
            self.start_reported = context.mark_action_started(self.id);
        }
        Ok(())
    }

    fn stop(&mut self, context: &CueContext) -> Result<()> {
        self.detach_clock();
        self.elapsed_before_pause = Duration::ZERO;
        self.started_at = None;
        self.auto_continue_fired = false;
        self.state = CueState::Standby;
        context.emit(CueEvent::Stopped { cue_id: self.id });
        Ok(())
    }

    fn pause(&mut self, _context: &CueContext) -> Result<()> {
        self.detach_clock();
        self.pause_at(Instant::now());
        Ok(())
    }

    fn resume(&mut self, _context: &CueContext) -> Result<()> {
        self.detach_clock();
        self.resume_at(Instant::now());
        Ok(())
    }

    fn hard_stop(&mut self, context: &CueContext) -> Result<()> {
        self.stop(context)
    }

    fn reset(&mut self) -> Result<()> {
        self.detach_clock();
        self.elapsed_before_pause = Duration::ZERO;
        self.started_at = None;
        self.auto_continue_fired = false;
        self.state = CueState::Standby;
        Ok(())
    }

    fn play_generation(&self) -> u64 { self.play_generation }

    fn is_action_started(&self) -> bool {
        matches!(self.state, CueState::Running | CueState::Paused) && self.elapsed() >= self.pre_wait
    }

    fn pre_wait(&self) -> Duration { self.pre_wait }
    fn set_pre_wait(&mut self, d: Duration) { self.pre_wait = d; }

    fn post_wait(&self) -> Duration { self.post_wait }
    fn set_post_wait(&mut self, d: Duration) { self.post_wait = d; }

    /// The wait duration — used by the event loop for `time_done` detection.
    fn duration(&self) -> Option<Duration> {
        Some(self.wait_duration)
    }

    fn elapsed(&self) -> Duration {
        self.elapsed_at(Instant::now())
    }

    fn action_elapsed(&self) -> Duration {
        self.elapsed().saturating_sub(self.pre_wait)
    }

    fn continue_mode(&self) -> ContinueMode { self.continue_mode }
    fn set_continue_mode(&mut self, mode: ContinueMode) { self.continue_mode = mode; }

    fn is_auto_continue_fired(&self) -> bool { self.auto_continue_fired }
    fn mark_auto_continue_fired(&mut self) { self.auto_continue_fired = true; }
    fn clear_auto_continue_fired(&mut self) { self.auto_continue_fired = false; }

    fn serialize(&self) -> Value {
        json!({
            "type": "wait",
            "cue_type": "wait",
            "id": self.id,
            "number": self.number,
            "name": self.name,
            "notes": self.notes,
            "color": self.color,
            "pre_wait_ms": self.pre_wait.as_millis() as u64,
            "post_wait_ms": self.post_wait.as_millis() as u64,
            "continue_mode": self.continue_mode,
            "wait_duration_ms": self.wait_duration.as_millis() as u64,
            "is_disabled": self.is_disabled,
        })
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Factory for [`WaitCue`].
pub struct WaitCueFactory;

impl CueFactory for WaitCueFactory {
    fn create(&self) -> Box<dyn Cue> {
        Box::new(WaitCue::new())
    }

    fn from_json(&self, value: Value) -> anyhow::Result<Box<dyn Cue>> {
        let mut cue = WaitCue::new();

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
        if let Some(ms) = value.get("wait_duration_ms").and_then(|v| v.as_u64()) {
            cue.wait_duration = Duration::from_millis(ms);
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
        if let Some(b) = value.get("is_disabled").and_then(|v| v.as_bool()) {
            cue.is_disabled = b;
        }

        Ok(Box::new(cue))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_ownership_follows_the_live_wait_through_edits_and_is_cleared_by_reset() {
        let mut wait = WaitCue::new();
        let list = Uuid::new_v4();
        wait.play_generation = 3;
        wait.follow_clock(list, 3);
        assert!(wait.clock_run.is_none());

        wait.state = CueState::Running;
        wait.follow_clock(list, 2);
        assert!(wait.clock_run.is_none());
        wait.follow_clock(list, 3);
        assert!(matches!(wait.clock_run, Some((id, ClockRun::Controlled)) if id == list));

        let mut edited = WaitCue::new();
        edited.inherit_playback(&mut wait);
        assert!(matches!(edited.clock_run, Some((id, ClockRun::Controlled)) if id == list));
        assert_eq!(edited.play_generation(), 3);

        edited.reset().unwrap();
        assert!(edited.clock_run.is_none());
        assert_eq!(edited.state(), CueState::Standby);
    }

    #[test]
    fn wait_excludes_gap_between_loss_and_return_in_one_tick() {
        let (context, _, _) = crate::test_support::recording_context();
        let began = Instant::now() - Duration::from_secs(5);
        let list = Uuid::new_v4();
        let mut wait = WaitCue::new();
        wait.go(&context).unwrap();
        wait.started_at = Some(began);
        wait.follow_clock(list, wait.play_generation());
        let lost = began + Duration::from_millis(500);
        let returned = began + Duration::from_millis(2500);
        wait.apply_clock_edge(list, ClockEdge::Lost(lost), TcOnStop::Pause, &context).unwrap();
        wait.apply_clock_edge(list, ClockEdge::Returned(returned), TcOnStop::Pause, &context).unwrap();
        assert_eq!(wait.elapsed_at(began + Duration::from_secs(4)), Duration::from_secs(2));
    }
}
