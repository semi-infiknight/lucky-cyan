//! [`DevampCue`] — releases a vamping (infinitely-looping) slice on its
//! target cues, QLab-style.
//!
//! A media cue whose current slice has an infinite play count (*vamp*) loops
//! until told otherwise.  GO on a Devamp Cue lets the pass in progress finish,
//! then the target continues into its next slice — the musical way out of a
//! loop.  With **Stop at end of current slice** the target stops at the slice
//! boundary instead of continuing.
//!
//! The cue completes when the transport requests the release. Slice release
//! can target video and its soundtrack. Whole-cue release is available for
//! one running, unsliced File Audio voice or one looping Video cue — a movie's
//! soundtrack is released with its picture.

use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    context::CueContext,
    traits::{Cue, CueFactory},
    types::{ContinueMode, CueColor, CueId, CueState, CueType},
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevampScope {
    #[default]
    CurrentSlice,
    WholeCue,
}

/// A cue that releases a slice or a whole Audio loop at the current pass end.
pub struct DevampCue {
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

    /// UUIDs of the cues to devamp.
    pub target_cue_ids: Vec<CueId>,
    temporary_target: Option<CueId>,
    /// Cue numbers kept in sync for display / legacy-file resolution.
    pub target_cue_numbers: Vec<String>,
    pub scope: DevampScope,
    /// `true` = the target stops at the end of its current slice instead of
    /// continuing into the next one.
    pub stop_at_end: bool,
    /// `true` = Whole-cue release waits for the target's current pass to end,
    /// then fires the next cue (QLab "start next cue when the loop ends").
    pub start_next_at_boundary: bool,
    is_disabled: bool,
    /// Runtime: the released voice whose pass end completes this Devamp.
    waiting_for_voice: Option<Uuid>,
}

impl DevampCue {
    /// Create a new Devamp cue with a fresh UUID.
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::from("Devamp Cue"),
            number: None,
            notes: String::new(),
            color: CueColor::Yellow,
            state: CueState::Standby,
            continue_mode: ContinueMode::DoNotContinue,
            pre_wait: Duration::ZERO,
            post_wait: Duration::ZERO,
            started_at: None,
            target_cue_ids: Vec::new(),
            temporary_target: None,
            target_cue_numbers: Vec::new(),
            scope: DevampScope::CurrentSlice,
            stop_at_end: false,
            start_next_at_boundary: false,
            is_disabled: false,
            waiting_for_voice: None,
        }
    }

    pub(crate) fn whole_release_target(&self) -> Result<CueId> {
        anyhow::ensure!(self.pre_wait.is_zero(), "Whole cue release requires zero Pre-Wait.");
        anyhow::ensure!(
            self.continue_mode == ContinueMode::DoNotContinue
                || (self.scope == DevampScope::WholeCue && self.start_next_at_boundary),
            "Whole cue release requires Do Not Continue, or Auto-Follow with Start next cue at the boundary."
        );
        if let Some(id) = self.temporary_target {
            return Ok(id);
        }
        let [target] = self.target_cue_ids.as_slice() else {
            anyhow::bail!("Select one File Audio or Video cue for Whole cue release.");
        };
        Ok(*target)
    }

    pub(crate) fn complete(&mut self, context: &CueContext) {
        self.state = CueState::Completed;
        self.started_at = Some(Instant::now());
        context.mark_action_started(self.id);
    }

    /// Stay Running until `voice` finishes its released pass; the event loop
    /// completes this cue when that voice reports completion, which fires the
    /// Auto-Follow successor exactly at the boundary.
    pub(crate) fn begin_boundary_wait(&mut self, voice: Uuid) {
        self.state = CueState::Running;
        self.started_at = Some(Instant::now());
        self.waiting_for_voice = Some(voice);
    }
}

impl Default for DevampCue {
    fn default() -> Self {
        Self::new()
    }
}

impl Cue for DevampCue {
    inherit_playback_fields!(state, started_at, temporary_target, waiting_for_voice);

    fn id(&self) -> CueId { self.id }
    fn cue_type(&self) -> CueType { CueType::Devamp }
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
        anyhow::ensure!(self.scope == DevampScope::CurrentSlice,
            "Use GO on a top-level Devamp cue to release a whole Audio or silent Video loop.");
        self.complete(context);
        Ok(())
    }

    fn stop(&mut self, _context: &CueContext) -> Result<()> {
        self.state = CueState::Standby;
        self.started_at = None;
        self.waiting_for_voice = None;
        Ok(())
    }

    fn pause(&mut self, _context: &CueContext) -> Result<()> { Ok(()) }
    fn resume(&mut self, _context: &CueContext) -> Result<()> { Ok(()) }

    fn hard_stop(&mut self, context: &CueContext) -> Result<()> {
        self.stop(context)
    }

    fn reset(&mut self) -> Result<()> {
        self.state = CueState::Standby;
        self.started_at = None;
        self.waiting_for_voice = None;
        Ok(())
    }

    fn pre_wait(&self) -> Duration { self.pre_wait }
    fn set_pre_wait(&mut self, d: Duration) { self.pre_wait = d; }
    fn post_wait(&self) -> Duration { self.post_wait }
    fn set_post_wait(&mut self, d: Duration) { self.post_wait = d; }

    fn duration(&self) -> Option<Duration> { None }

    fn elapsed(&self) -> Duration {
        self.started_at.map(|t| t.elapsed()).unwrap_or(Duration::ZERO)
    }

    fn action_elapsed(&self) -> Duration { self.elapsed() }

    fn continue_mode(&self) -> ContinueMode { self.continue_mode }
    fn set_continue_mode(&mut self, mode: ContinueMode) { self.continue_mode = mode; }

    fn set_temporary_target(&mut self, id: Option<CueId>) { self.temporary_target = id; }
    fn temporary_target(&self) -> Option<CueId> { self.temporary_target }

    fn devamp_specification(&self) -> Option<(bool, Vec<CueId>)> {
        (self.scope == DevampScope::CurrentSlice).then(|| {
            (self.stop_at_end, self.temporary_target.map(|id| vec![id]).unwrap_or_else(|| self.target_cue_ids.clone()))
        })
    }

    fn devamp_boundary_voice(&self) -> Option<uuid::Uuid> {
        self.waiting_for_voice
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
            .map(|_| CueIssue::warning("Devamp target not found (cue deleted)"))
            .collect();
        if self.target_cue_ids.is_empty() {
            issues.push(CueIssue::warning("No target selected — this Devamp does nothing"));
        }
        if self.scope == DevampScope::WholeCue {
            if let Err(error) = self.whole_release_target() {
                issues.push(CueIssue::error(error.to_string()));
            }
        }
        issues
    }

    fn resolve_stop_target(&mut self, number_to_id: &std::collections::HashMap<String, CueId>) {
        if self.target_cue_ids.is_empty() {
            for num in &self.target_cue_numbers {
                if let Some(&id) = number_to_id.get(num) {
                    if !self.target_cue_ids.contains(&id) {
                        self.target_cue_ids.push(id);
                    }
                }
            }
        }
    }

    fn serialize(&self) -> Value {
        json!({
            "type": "devamp",
            "cue_type": "devamp",
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
            "scope": self.scope,
            "stop_at_end": self.stop_at_end,
            "start_next_at_boundary": self.start_next_at_boundary,
            "is_disabled": self.is_disabled,
        })
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Factory for [`DevampCue`].
pub struct DevampCueFactory;

impl CueFactory for DevampCueFactory {
    fn create(&self) -> Box<dyn Cue> {
        Box::new(DevampCue::new())
    }

    fn from_json(&self, value: Value) -> Result<Box<dyn Cue>> {
        let mut cue = DevampCue::new();

        if let Some(id_str) = value.get("id").and_then(|v| v.as_str()) {
            cue.id = id_str.parse().unwrap_or_else(|_| Uuid::new_v4());
        }
        if let Some(name) = value.get("name").and_then(|v| v.as_str()) {
            cue.name = name.to_string();
        }
        if let Some(num) = value.get("number").and_then(|v| v.as_str()) {
            cue.number = Some(num.to_string());
        }
        if let Some(notes) = value.get("notes").and_then(|v| v.as_str()) {
            cue.notes = notes.to_string();
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
        if let Some(ids) = value.get("target_cue_ids").and_then(|v| v.as_array()) {
            cue.target_cue_ids = ids
                .iter()
                .filter_map(|v| v.as_str().and_then(|s| s.parse().ok()))
                .collect();
        }
        if let Some(nums) = value.get("target_cue_numbers").and_then(|v| v.as_array()) {
            cue.target_cue_numbers = nums
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        }
        if let Some(b) = value.get("stop_at_end").and_then(|v| v.as_bool()) {
            cue.stop_at_end = b;
        }
        if let Some(scope) = value.get("scope") {
            cue.scope = serde_json::from_value(scope.clone())?;
        }
        if let Some(b) = value.get("start_next_at_boundary").and_then(|v| v.as_bool()) {
            cue.start_next_at_boundary = b;
        }
        if let Some(b) = value.get("is_disabled").and_then(|v| v.as_bool()) {
            cue.is_disabled = b;
        }

        Ok(Box::new(cue))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_audio_devamp_round_trips_with_a_current_slice_legacy_default() {
        let mut cue = DevampCue::new();
        cue.scope = DevampScope::WholeCue;
        let mut saved = cue.serialize();
        assert_eq!(DevampCueFactory.from_json(saved.clone()).unwrap().serialize()["scope"], "whole_cue");
        saved.as_object_mut().unwrap().remove("scope");
        let old = DevampCueFactory.from_json(saved).unwrap();
        assert_eq!(old.serialize()["scope"], "current_slice");
        assert!(old.devamp_specification().is_some());
    }

    #[test]
    fn serialize_roundtrip_preserves_targets_and_mode() {
        let mut cue = DevampCue::new();
        cue.set_name("Release vamp".to_string());
        cue.set_number(Some("10".to_string()));
        let target = Uuid::new_v4();
        cue.target_cue_ids = vec![target];
        cue.target_cue_numbers = vec!["3".into()];
        cue.stop_at_end = true;

        let json = cue.serialize();
        let restored = DevampCueFactory.from_json(json).expect("deserialize");
        assert_eq!(restored.name(), "Release vamp");
        assert_eq!(restored.number(), Some("10"));
        assert_eq!(restored.devamp_specification(), Some((true, vec![target])));
    }

    #[test]
    fn go_completes_synchronously() {
        let cue = DevampCue::new();
        assert_eq!(cue.state(), CueState::Standby);
        // go() needs no engine access — the transport does the voice work.
        let spec = cue.devamp_specification();
        assert_eq!(spec, Some((false, vec![])));
    }
}
