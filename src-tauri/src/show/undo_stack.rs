//! Undo / redo history for the cue list.
//!
//! Structural changes capture the active cue list as a [`Snapshot`].
//! Flag, note and Timeline entrance edits restore one field in place.
//! A snapshot saves each cue's JSON and clones its reference-counted decoded
//! audio handle. Restoration rebuilds cues via the
//! `CueRegistry` and injects the saved audio Arc back, so there is never a
//! re-decode round-trip on undo/redo.
//!
//! [`UndoHistory`] keeps one [`UndoStack`] per cue list (keyed by its id)
//! instead of a single shared stack: without that, switching the active cue
//! list tab and pressing Ctrl+Z would pop a snapshot recorded for a
//! *different* list and splice its cues into the one on screen. Loading a
//! different workspace gives its cue lists fresh ids, so this also keeps one
//! workspace's history from bleeding into the next.

use std::{collections::HashMap, time::Duration};

use crate::cue::{decoded_audio::DecodedAudioMap, types::PlaylistSecondTrigger};

/// Maximum number of undo levels kept in memory.
const MAX_UNDO: usize = 50;

/// Snapshot of a single cue: its serialised state plus the decoded audio it
/// (and, for a Group, each of its children) already had in memory.
pub struct CueSnapshot {
    /// Full serialised form produced by [`crate::cue::traits::Cue::serialize`].
    pub json: serde_json::Value,
    /// Decoded audio of the cue and its descendants, keyed by cue id.
    pub decoded: DecodedAudioMap,
}

/// Complete snapshot of the active cue list at a single point in time.
pub struct Snapshot {
    pub cart_layout: Option<super::cart_layout::CartLayout>,
    pub hotkeys: std::collections::BTreeMap<uuid::Uuid, String>,
    pub flagged_cues: std::collections::BTreeSet<uuid::Uuid>,
    pub second_colors: std::collections::BTreeMap<uuid::Uuid, crate::cue::types::CueColor>,
    pub wall_clock_triggers: std::collections::BTreeMap<uuid::Uuid, super::wall_clock::WallClockTrigger>,
    /// Ordered cue snapshots, matching the cue list order.
    pub cues: Vec<CueSnapshot>,
    /// Playhead position at the time the snapshot was taken.
    pub playhead_id: Option<uuid::Uuid>,
    /// Next direct child of the Start First And Enter Group on standby.
    pub playhead_child_id: Option<uuid::Uuid>,
}

/// The list state or cue property to restore on the next step.
pub enum HistoryEntry {
    /// Restore the complete cue list snapshot.
    Snapshot(Snapshot),
    /// Restore this cue's flag without rebuilding the list.
    CueFlag {
        /// Cue ID scoped to the owning list.
        cue_id: uuid::Uuid,
        /// Value to restore when this entry is applied.
        flagged: bool,
    },
    /// Restore this cue's note without rebuilding the list.
    CueNote {
        cue_id: uuid::Uuid,
        note: String,
    },
    /// Restore a Timeline entrance without rebuilding the cue or its group.
    CuePreWait {
        cue_id: uuid::Uuid,
        pre_wait: Duration,
    },
    CueHotkey {
        cue_id: uuid::Uuid,
        key: Option<String>,
    },
    PlaylistTrigger {
        group_id: uuid::Uuid,
        trigger: PlaylistSecondTrigger,
    },
    PlaylistShuffle {
        group_id: uuid::Uuid,
        enabled: bool,
    },
}

/// Undo / redo history.
pub struct UndoStack {
    /// Past states — oldest at index 0, most-recent at the back.
    past: Vec<HistoryEntry>,
    /// Future states available after one or more undos — most-recent at back.
    future: Vec<HistoryEntry>,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self::new()
    }
}

impl UndoStack {
    /// Create an empty history.
    pub fn new() -> Self {
        Self {
            past: Vec::new(),
            future: Vec::new(),
        }
    }

    /// Record the state *before* a mutating action.
    ///
    /// Clears the redo stack — once the user performs a new action the redo
    /// history is no longer meaningful.  Trims the past stack to `MAX_UNDO`.
    pub fn push_action(&mut self, entry: HistoryEntry) {
        self.future.clear();
        self.past.push(entry);
        if self.past.len() > MAX_UNDO {
            self.past.remove(0);
        }
    }

    /// Whether there is at least one state to undo.
    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    /// Whether there is at least one state to redo.
    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }
}

#[derive(Clone, Copy)]
pub enum HistoryDirection {
    Undo,
    Redo,
}

/// Undo/redo histories for every cue list in the current workspace, keyed by
/// cue list id so each list's history is independent of the others.
#[derive(Default)]
pub struct UndoHistory {
    stacks: HashMap<uuid::Uuid, UndoStack>,
}

impl UndoHistory {
    /// Create an empty history.
    pub fn new() -> Self {
        Self::default()
    }

    fn stack_mut(&mut self, cue_list_id: uuid::Uuid) -> &mut UndoStack {
        self.stacks.entry(cue_list_id).or_default()
    }

    /// Record the state *before* a mutating action on the given cue list.
    pub fn push_action(&mut self, cue_list_id: uuid::Uuid, snapshot: Snapshot) {
        self.stack_mut(cue_list_id)
            .push_action(HistoryEntry::Snapshot(snapshot));
    }

    /// Record only the previous flag for an edit that changes that cue's flag.
    pub fn push_flag_edit(&mut self, cue_list_id: uuid::Uuid, cue_id: uuid::Uuid, previous: bool) {
        self.stack_mut(cue_list_id).push_action(HistoryEntry::CueFlag {
            cue_id,
            flagged: previous,
        });
    }

    /// Record only the previous note for an edit that changes that cue's note.
    pub fn push_note_edit(&mut self, cue_list_id: uuid::Uuid, cue_id: uuid::Uuid, note: String) {
        self.stack_mut(cue_list_id)
            .push_action(HistoryEntry::CueNote { cue_id, note });
    }

    pub fn push_pre_wait_edit(&mut self, cue_list_id: uuid::Uuid, cue_id: uuid::Uuid, pre_wait: Duration) {
        self.stack_mut(cue_list_id)
            .push_action(HistoryEntry::CuePreWait { cue_id, pre_wait });
    }

    /// Record only the previous keyboard assignment, without a list snapshot.
    pub fn push_hotkey_edit(&mut self, cue_list_id: uuid::Uuid, cue_id: uuid::Uuid, key: Option<String>) {
        self.stack_mut(cue_list_id).push_action(HistoryEntry::CueHotkey { cue_id, key });
    }

    pub fn push_playlist_trigger_edit(
        &mut self, cue_list_id: uuid::Uuid, group_id: uuid::Uuid, trigger: PlaylistSecondTrigger,
    ) {
        self.stack_mut(cue_list_id).push_action(HistoryEntry::PlaylistTrigger { group_id, trigger });
    }

    pub fn push_playlist_shuffle_edit(&mut self, cue_list_id: uuid::Uuid, group_id: uuid::Uuid, enabled: bool) {
        self.stack_mut(cue_list_id).push_action(HistoryEntry::PlaylistShuffle { group_id, enabled });
    }

    /// Whether the given cue list has at least one state to undo.
    pub fn can_undo(&self, cue_list_id: uuid::Uuid) -> bool {
        self.stacks.get(&cue_list_id).is_some_and(UndoStack::can_undo)
    }

    /// Whether the given cue list has at least one state to redo.
    pub fn can_redo(&self, cue_list_id: uuid::Uuid) -> bool {
        self.stacks.get(&cue_list_id).is_some_and(UndoStack::can_redo)
    }

    /// Inspect the candidate before changing either history stack.
    pub fn candidate(&self, list: uuid::Uuid, direction: HistoryDirection) -> Option<&HistoryEntry> {
        let stack = self.stacks.get(&list)?;
        match direction {
            HistoryDirection::Undo => stack.past.last(),
            HistoryDirection::Redo => stack.future.last(),
        }
    }

    /// Consume the candidate after restoration and record its inverse.
    pub fn advance(&mut self, list: uuid::Uuid, direction: HistoryDirection, inverse: HistoryEntry) {
        let stack = self.stack_mut(list);
        let (from, to) = match direction {
            HistoryDirection::Undo => (&mut stack.past, &mut stack.future),
            HistoryDirection::Redo => (&mut stack.future, &mut stack.past),
        };
        if from.pop().is_some() {
            to.push(inverse);
        }
    }

    /// Drop every cue list's history. Called when a new, loaded, or imported
    /// workspace replaces the cue lists wholesale — stale histories keyed by
    /// ids that no longer exist in the workspace would just be dead weight.
    pub fn clear(&mut self) {
        self.stacks.clear();
    }
}
