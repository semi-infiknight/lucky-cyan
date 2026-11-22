//! [`CueList`] — an ordered sequence of cues with a Playhead.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::cart_layout::{CartEdit, CartLayout};
use super::wall_clock::{Observation, WallClockSession, WallClockTrigger};
use crate::cue::{control_cue::ControlAction, registry::CueRegistry, traits::Cue, types::{ContinueMode, CueColor, CueId, CueType, GroupMode}};
use crate::cue::group_cue::GroupCue;
use super::timecode_loss::ClockCue;
use crate::engine::midi_trigger::MidiTrigger;
use crate::engine::timecode_types::{CueListTcConfig, TcOnStart, TcPosition, TcTrigger, TcTriggerWindow};

// ---------------------------------------------------------------------------
// CueListMode
// ---------------------------------------------------------------------------

/// Playback mode for a cue list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CueListMode {
    /// Sequential: GO fires the cue at the Playhead then advances it.
    #[default]
    Sequential,
    /// Cart: each cue is triggered independently by clicking its tile.
    Cart,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CueNumberChange {
    pub cue_id: CueId,
    pub previous: Option<String>,
    pub number: String,
}

// ---------------------------------------------------------------------------
// Recursive helpers (free functions to avoid borrow-checker conflicts)
// ---------------------------------------------------------------------------

/// Recursively assign cue numbers.
///
/// `prefix` is the parent's number (e.g. `"3"` for top-level cue 3).
/// Children of that cue get `"3.1"`, `"3.2"`, etc.
/// An empty prefix means we are at the top level: numbers are `"1"`, `"2"`, …
fn renumber_recursive(cues: &mut Vec<Box<dyn Cue>>, prefix: &str) -> bool {
    let mut changed = false;
    for (i, cue) in cues.iter_mut().enumerate() {
        let number = if prefix.is_empty() {
            (i + 1).to_string()
        } else {
            format!("{}.{}", prefix, i + 1)
        };
        if cue.number() != Some(number.as_str()) {
            cue.set_number(Some(number.clone()));
            changed = true;
        }
        if let Some(children) = cue.child_cues_mut() {
            changed |= renumber_recursive(children, &number);
        }
    }
    changed
}

/// Whether any cue in the hierarchy already carries `number`.
fn number_in_use(cues: &[Box<dyn Cue>], number: &str) -> bool {
    cues.iter().any(|cue| {
        cue.number() == Some(number)
            || cue.child_cues().is_some_and(|children| number_in_use(children, number))
    })
}

/// Format a cue number so whole steps read as integers ("1", "2") while
/// fractional ones keep only the digits they need ("1.5", "0.25") — a cue
/// number is a display string, and "1.0" or "1.500" would be noise.
fn format_cue_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value.trunc() as i64);
    }
    let mut s = format!("{value:.6}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" { "0".into() } else { s }
}

/// Collect selected cues in display order and reserve each untouched number.
fn collect_numbering_cues<'a>(
    cues: &'a [Box<dyn Cue>],
    selected: &HashSet<CueId>,
    targets: &mut Vec<&'a dyn Cue>,
    occupied: &mut HashSet<String>,
) {
    for cue in cues {
        if selected.contains(&cue.id()) {
            targets.push(cue.as_ref());
        } else if let Some(number) = cue.number() {
            occupied.insert(number.to_owned());
        }
        if let Some(children) = cue.child_cues() {
            collect_numbering_cues(children, selected, targets, occupied);
        }
    }
}

fn apply_cue_numbers(cues: &mut [Box<dyn Cue>], numbers: &HashMap<CueId, &str>) {
    for cue in cues {
        if let Some(&number) = numbers.get(&cue.id()) {
            cue.set_number(Some(number.to_owned()));
        }
        if let Some(children) = cue.child_cues_mut() {
            apply_cue_numbers(children, numbers);
        }
    }
}

/// Clear every cue number in the hierarchy.
fn clear_numbers_recursive(cues: &mut Vec<Box<dyn Cue>>) -> bool {
    let mut changed = false;
    for cue in cues.iter_mut() {
        if cue.number().is_some() {
            cue.set_number(None);
            changed = true;
        }
        if let Some(children) = cue.child_cues_mut() {
            changed |= clear_numbers_recursive(children);
        }
    }
    changed
}

/// Extract a cue from anywhere in the hierarchy (top-level or inside any group).
fn extract_cue_anywhere(cues: &mut Vec<Box<dyn Cue>>, id: &CueId) -> Option<Box<dyn Cue>> {
    if let Some(idx) = cues.iter().position(|c| c.id() == *id) {
        return Some(cues.remove(idx));
    }
    for cue in cues.iter_mut() {
        if let Some(children) = cue.child_cues_mut() {
            if let Some(extracted) = extract_cue_anywhere(children, id) {
                return Some(extracted);
            }
        }
    }
    None
}

fn contains_cue(cue: &dyn Cue, id: &CueId) -> bool {
    cue.id() == *id || cue.child_cues().is_some_and(|children| {
        children.iter().any(|child| contains_cue(child.as_ref(), id))
    })
}

/// Add `child` to the group identified by `group_id`, searching recursively.
/// Returns `Ok(None)` when placed successfully, `Ok(Some(child))` when the
/// group was not found (caller must handle the returned child to avoid losing it).
fn add_child_to_group_anywhere(
    cues: &mut Vec<Box<dyn Cue>>,
    group_id: &CueId,
    child: Box<dyn Cue>,
    position: i32,
) -> Result<Option<Box<dyn Cue>>> {
    // First pass: check if any entry IS the target group (avoids mid-loop borrow).
    for cue in cues.iter_mut() {
        if cue.id() == *group_id {
            cue.add_child(child, position)?;
            return Ok(None);
        }
    }
    // Second pass: recurse into children.
    let mut child_opt = Some(child);
    for cue in cues.iter_mut() {
        if let Some(children) = cue.child_cues_mut() {
            let c = child_opt.take().expect("invariant: always Some here");
            match add_child_to_group_anywhere(children, group_id, c, position)? {
                None => return Ok(None),
                Some(returned) => child_opt = Some(returned),
            }
        }
    }
    Ok(child_opt)
}

fn valid_hotkey(key: &str) -> bool {
    if matches!(key.as_bytes(), [b'0'..=b'9']) {
        return true;
    }
    let unshifted = key.strip_prefix("Shift+").unwrap_or(key);
    matches!(
        unshifted.as_bytes(),
        [b'A'..=b'F' | b'H'..=b'O' | b'Q'..=b'R' | b'T'..=b'Z']
    ) || matches!(
        unshifted,
        "F2" | "F3" | "F4" | "F6" | "F7" | "F8" | "F13" | "F14" | "F15" | "F16"
            | "F17" | "F18" | "F19" | "F20" | "F21" | "F22" | "F23" | "F24"
    )
}

/// An ordered list of cues with a Playhead indicating the next cue to GO.
pub struct CueList {
    pub id: Uuid,
    pub name: String,
    /// Sequential (playhead-driven) or Cart (each tile fires independently).
    pub mode: CueListMode,
    /// Older Inkue carts ran list GO and followed their cue chains.
    pub chain_cart_cues: bool,
    pub cart_layout: Option<CartLayout>,
    /// Cues in their display/execution order.
    pub cues: Vec<Box<dyn Cue>>,
    /// ID of the cue at the Playhead (next to be triggered by GO).
    /// `None` means the playhead is past the last cue.
    pub playhead_cue_id: Option<CueId>,
    enter_follow: Option<EnterFollow>,
    /// Timecode synchronisation settings for this list.
    pub tc_config: CueListTcConfig,
    /// Per-cue TC triggers: cue_id → TcTrigger.  Stored on the list so
    /// every cue type gains TC triggering without a per-type code change.
    pub tc_triggers: HashMap<CueId, TcTrigger>,
    /// Per-cue MIDI triggers: cue_id → MidiTrigger.  On the list for the same
    /// reason as [`Self::tc_triggers`] — every cue type gains MIDI triggering
    /// without a per-type code change, and a trigger survives the cue rebuild
    /// that every inspector edit performs.
    pub midi_triggers: HashMap<CueId, MidiTrigger>,
    pub hotkeys: BTreeMap<CueId, String>,
    pub flagged_cues: BTreeSet<CueId>,
    pub second_colors: BTreeMap<CueId, CueColor>,
    pub wall_clock_triggers: BTreeMap<CueId, WallClockTrigger>,
    pub(crate) wall_clock_session: WallClockSession,
    /// Playback launched without a list advance. A live edit cannot enable a follow on this run.
    pub(crate) independent_runs: BTreeSet<CueId>,
    /// Runtime mirror of `GeneralPreferences::auto_renumber_on_reorder`
    /// (not persisted). When `false` (default), structural mutations leave cue
    /// numbers untouched; when `true`, they resequence via [`renumber_all`].
    /// Kept in sync by the workspace whenever preferences load or change.
    pub auto_renumber: bool,
}

/// One Enter group's authority to continue to an exact outer standby.
#[derive(Clone, Copy)]
struct EnterFollow {
    group_id: CueId,
    wait_id: CueId,
    successor: CueId,
}

impl EnterFollow {
    fn arming(self, list: &CueList) -> [Option<bool>; 3] {
        let group = list.get(&self.group_id);
        let wait = group.and_then(|cue| cue.child_cues()).and_then(|children| {
            children.iter().find(|child| child.id() == self.wait_id).map(|child| child.as_ref())
        });
        let successor = list.get(&self.successor);
        [group, wait, successor].map(|cue| cue.map(|cue| !cue.is_disabled()))
    }
}

impl CueList {
    /// Create a new, empty cue list.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            mode: CueListMode::default(),
            chain_cart_cues: false,
            cart_layout: None,
            cues: Vec::new(),
            playhead_cue_id: None,
            enter_follow: None,
            tc_config: CueListTcConfig::default(),
            tc_triggers: HashMap::new(),
            midi_triggers: HashMap::new(),
            hotkeys: BTreeMap::new(),
            flagged_cues: BTreeSet::new(),
            second_colors: BTreeMap::new(),
            wall_clock_triggers: BTreeMap::new(),
            wall_clock_session: WallClockSession::default(),
            independent_runs: BTreeSet::new(),
            auto_renumber: false,
        }
    }

    /// Addressed GO can aim at these cues without choosing a sibling.
    pub(crate) fn hotkey_eligible(&self, id: CueId) -> bool {
        self.get(&id).is_some() || self.cues.iter().any(|parent|
            matches!(parent.group_mode(), Some(GroupMode::Sequential | GroupMode::Playlist | GroupMode::StartFirstAndEnter))
            && parent.child_cues().is_some_and(|children| children.iter().any(|child| child.id() == id)))
    }

    pub(crate) fn check_hotkey(&self, id: CueId, key: Option<&str>) -> Result<()> {
        anyhow::ensure!(self.get_recursive(&id).is_some(), "Cue no longer exists");
        if let Some(key) = key {
            anyhow::ensure!(self.hotkey_eligible(id), "Assign the hotkey to the group or move the cue to the top level");
            anyhow::ensure!(valid_hotkey(key), "Use 0–9 or a letter other than G, P or S, with optional Shift on letters");
            if let Some(cue) = self.hotkeys.iter().filter(|(other, value)| **other != id && value.as_str() == key)
                .find_map(|(other, _)| self.get_recursive(other)) {
                anyhow::bail!("{} is already assigned to cue {} {}", key, cue.number().unwrap_or(""), cue.name());
            }
        }
        Ok(())
    }

    /// Update one validated assignment and return its previous value.
    pub(crate) fn replace_hotkey(&mut self, id: CueId, key: Option<String>) -> Result<Option<String>> {
        self.check_hotkey(id, key.as_deref())?;
        Ok(match key {
            Some(value) => self.hotkeys.insert(id, value),
            None => self.hotkeys.remove(&id),
        })
    }

    /// Independently addressed starts preserve the manual playhead. A cue
    /// whose action depends on that playhead cannot be scheduled here.
    pub(crate) fn can_start_independently(&self, id: CueId) -> bool {
        self.get(&id).is_some_and(|cue| {
            (self.independent_pads() || cue.continue_mode() == ContinueMode::DoNotContinue)
                && !matches!(cue.group_mode(), Some(GroupMode::Sequential | GroupMode::Playlist | GroupMode::StartFirstAndEnter))
                && !cue.control_specification().is_some_and(|(action, _)| {
                    matches!(action, ControlAction::Goto | ControlAction::Start)
                })
                && !cue.fade_specification().is_some_and(|fade| fade.crossfade_into.is_some())
        })
    }

    pub(crate) fn collect_wall_clock(&mut self, now: Observation) -> Vec<CueId> {
        let due = self.wall_clock_session.collect(&self.wall_clock_triggers, now);
        if due.is_empty() {
            return due;
        }
        // When the time is shared, use the cue order instead of random UUID order.
        self.cues.iter().map(|cue| cue.id()).filter(|id| due.contains(id)).collect()
    }

    pub fn edited_cart_layout(&self, edit: CartEdit) -> Result<Option<CartLayout>> {
        anyhow::ensure!(self.mode == CueListMode::Cart, "Switch this cue list to cart mode first");
        let mut layout = self.cart_layout.clone();
        match edit {
            CartEdit::Compact => return Ok(None),
            CartEdit::Resize { rows, columns } => {
                if let Some(grid) = &mut layout {
                    grid.resize(rows, columns)?;
                } else {
                    layout = Some(CartLayout::new(rows, columns)?);
                }
            }
            CartEdit::Place { cue_id, cell } => {
                let grid = layout.as_mut().ok_or_else(|| anyhow!("Choose a fixed grid first"))?;
                grid.place(cue_id, cell)?;
            }
        }
        if let Some(grid) = &mut layout {
            grid.sync(self.cues.iter().map(|cue| cue.id()));
        }
        Ok(layout)
    }

    pub fn independent_pads(&self) -> bool {
        self.mode == CueListMode::Cart && !self.chain_cart_cues
    }

    pub fn set_mode(&mut self, mode: CueListMode, chain_cart_cues: Option<bool>) {
        self.cancel_enter_follow();
        let previously_independent = self.independent_pads();
        self.mode = mode;
        if let Some(chain) = chain_cart_cues {
            self.chain_cart_cues = chain;
        }
        if self.independent_pads() {
            self.playhead_cue_id = None;
        } else if previously_independent {
            self.playhead_cue_id = self.cues.iter().find(|cue| !cue.is_disabled()).map(|cue| cue.id());
        }
    }

    /// Cue ids whose MIDI trigger matches `message`.
    ///
    /// More than one cue may be bound to the same message; all of them fire,
    /// which is how an operator builds a one-button "everything goes" pad.
    pub fn midi_triggers_matching(&self, message: &[u8]) -> Vec<CueId> {
        self.midi_triggers
            .iter()
            .filter(|(_, trigger)| trigger.matches(message))
            .map(|(&id, _)| id)
            .collect()
    }

    /// Collect the cue IDs in a confirmed forward clock interval.
    pub(crate) fn tc_triggers_crossed(&self, window: TcTriggerWindow) -> Vec<CueId> {
        if !self.tc_config.enabled || !self.tc_config.rate.same_address_format(window.rate) {
            return Vec::new();
        }

        let mut crossed: Vec<_> = self.tc_triggers
            .iter()
            .filter_map(|(&id, trigger)| window.offset(trigger.position).map(|offset| (offset, id)))
            .collect();
        crossed.sort_unstable();
        crossed.into_iter().map(|(_, id)| id).collect()
    }

    pub(crate) fn tc_joins_at(&self, position: TcPosition) -> Vec<(CueId, std::time::Duration)> {
        let rate = self.tc_config.rate;
        if !self.tc_config.enabled || self.tc_config.on_start != TcOnStart::JoinActive
            || !rate.same_address_format(position.rate) || !position.is_valid() {
            return Vec::new();
        }
        let frame = position.to_frame_number();
        let mut candidates: Vec<_> = self.cues.iter().filter_map(|cue| {
            if cue.is_disabled() || cue.continue_mode() != ContinueMode::DoNotContinue
                || !matches!(cue.cue_type(), CueType::Wait | CueType::Audio | CueType::Video) {
                return None;
            }
            let mark = self.tc_triggers.get(&cue.id())?.position;
            if !mark.is_valid() || !rate.same_address_format(mark.rate) {
                return None;
            }
            let frames = frame.checked_sub(mark.to_frame_number()).filter(|&frames| frames > 0)?;
            Some((cue.id(), std::time::Duration::from_micros(frames * rate.frame_us())))
        }).collect();
        // Join earlier marks first, with cue-list order breaking ties. The
        // initial position has no day history to resolve marks across midnight.
        candidates.sort_by_key(|&(_, elapsed)| std::cmp::Reverse(elapsed));
        candidates
    }

    pub(crate) fn detach_timecode_runs(&mut self) {
        for cue in self.cues.iter_mut().filter_map(|cue| ClockCue::from_cue_mut(cue.as_mut())) {
            cue.detach();
        }
    }

    // -----------------------------------------------------------------------
    // Cue access
    // -----------------------------------------------------------------------

    /// Resolve one exact top-level cue number. Refuse ambiguous show data.
    pub(crate) fn unique_numbered_cue(&self, number: &str) -> Option<&dyn Cue> {
        let mut matches = self.cues.iter().filter(|cue| cue.number() == Some(number));
        let first = matches.next()?;
        matches.next().is_none().then(|| first.as_ref())
    }

    /// Resolve an operator's standby request without guessing among duplicates.
    pub(crate) fn goto_number_target(&self, number: &str) -> Result<CueId> {
        anyhow::ensure!(!self.independent_pads(), "Choose a cue list to move standby by number.");
        let mut matches = self.cues.iter().flat_map(|cue| {
            let direct_children = (cue.group_mode() == Some(GroupMode::StartFirstAndEnter))
                .then(|| cue.child_cues()).flatten().unwrap_or_default();
            std::iter::once(cue).chain(direct_children)
        }).filter(|cue| cue.number() == Some(number));
        let cue = matches.next().ok_or_else(|| anyhow!("No cue numbered '{}' is available in this list.", number))?;
        anyhow::ensure!(matches.next().is_none(), "More than one cue is numbered '{}'.", number);
        if let Some((_, armed)) = self.cue_with_arming(&cue.id()) {
            anyhow::ensure!(armed, "Cue '{}' is disarmed.", number);
        }
        Ok(cue.id())
    }

    /// Find a cue by ID (immutable).
    pub fn get(&self, id: &CueId) -> Option<&dyn Cue> {
        self.cues.iter().find(|c| c.id() == *id).map(|c| c.as_ref())
    }

    /// Find a cue by ID (mutable), searching only top-level cues.
    pub fn get_mut(&mut self, id: &CueId) -> Option<&mut dyn Cue> {
        self.cues
            .iter_mut()
            .find(|c| c.id() == *id)
            .map(|c| c.as_mut() as &mut dyn Cue)
    }

    /// Find a cue by ID (mutable), searching recursively through group children.
    ///
    /// Used when applying preloaded audio to cues that may live inside groups.
    pub fn get_mut_recursive(&mut self, id: &CueId) -> Option<&mut dyn Cue> {
        fn search(cues: &mut Vec<Box<dyn crate::cue::traits::Cue>>, id: &CueId) -> Option<*mut dyn crate::cue::traits::Cue> {
            for cue in cues.iter_mut() {
                if cue.id() == *id {
                    return Some(cue.as_mut() as *mut dyn crate::cue::traits::Cue);
                }
                if let Some(children) = cue.child_cues_mut() {
                    if let Some(ptr) = search(children, id) {
                        return Some(ptr);
                    }
                }
            }
            None
        }
        // SAFETY: the pointer is valid as long as `self` is borrowed mutably,
        // which is guaranteed by the lifetime of the returned reference.
        search(&mut self.cues, id).map(|ptr| unsafe { &mut *ptr })
    }

    /// Find a cue by ID (immutable), searching recursively through group
    /// children.  Used by the inspector / per-cue commands so cues nested in a
    /// group are reachable, not just top-level ones.
    pub fn get_recursive(&self, id: &CueId) -> Option<&dyn Cue> {
        self.cue_with_arming(id).map(|(cue, _)| cue)
    }

    /// An explicit trigger can start a child only when the whole group path is armed.
    pub(crate) fn cue_with_arming(&self, id: &CueId) -> Option<(&dyn Cue, bool)> {
        fn search<'a>(cues: &'a [Box<dyn Cue>], id: &CueId, parent_armed: bool) -> Option<(&'a dyn Cue, bool)> {
            for cue in cues {
                let armed = parent_armed && !cue.is_disabled();
                if cue.id() == *id {
                    return Some((cue.as_ref(), armed));
                }
                if let Some(children) = cue.child_cues() {
                    if let Some(found) = search(children, id, armed) {
                        return Some(found);
                    }
                }
            }
            None
        }
        search(&self.cues, id, true)
    }

    /// Verify a temporary target assignment before it can change any playback state.
    pub fn target_destination(&self, cue: &dyn Cue) -> anyhow::Result<Option<CueId>> {
        use crate::cue::{control_cue::ControlAction, types::CueType};
        if cue.cue_type() != CueType::Target { return Ok(None); }
        let destination = cue.replacement_target().and_then(|id| self.get_recursive(&id))
            .filter(|c| c.number().is_some_and(|n| !n.is_empty()))
            .ok_or_else(|| anyhow::anyhow!("Choose a numbered replacement cue in this list"))?;
        let subject = cue.control_specification().and_then(|(_, ids)| ids.first().copied())
            .and_then(|id| self.get_recursive(&id))
            .ok_or_else(|| anyhow::anyhow!("Choose a cue to retarget"))?;
        let kind = subject.cue_type();
        anyhow::ensure!(subject.id() != cue.id() && subject.id() != destination.id()
            && (matches!(kind, CueType::Devamp | CueType::Stop) || ControlAction::from_cue_type(&kind).is_some()),
            "Choose a Stop, Devamp or Command cue and a separate destination");
        anyhow::ensure!(kind != CueType::Goto || self.index_of(&destination.id()).is_some(),
            "Goto requires a top-level destination in its list");
        Ok(Some(destination.id()))
    }

    /// Replace a cue by ID in-place, searching recursively through group children.
    ///
    /// Returns `true` if the cue was found and replaced.
    pub fn replace_cue_recursive(&mut self, id: &CueId, new_cue: Box<dyn crate::cue::traits::Cue>) -> bool {
        fn replace(
            cues: &mut Vec<Box<dyn crate::cue::traits::Cue>>,
            id: &CueId,
            slot: &mut Option<Box<dyn crate::cue::traits::Cue>>,
        ) -> bool {
            for cue in cues.iter_mut() {
                if cue.id() == *id {
                    if let Some(c) = slot.take() {
                        *cue = c;
                    }
                    return true;
                }
            }
            for cue in cues.iter_mut() {
                if let Some(children) = cue.child_cues_mut() {
                    if replace(children, id, slot) {
                        return true;
                    }
                }
            }
            false
        }
        let lease = self.enter_follow.map(|owner| (owner, owner.arming(self)));
        let mut slot = Some(new_cue);
        let replaced = replace(&mut self.cues, id, &mut slot);
        // Inspector and batch edits can rebuild a Group with its children.
        // Revoke only when the committed replacement changes a lease cue's arming.
        if replaced && lease.is_some_and(|(owner, before)| owner.arming(self) != before) {
            self.cancel_enter_follow();
        }
        replaced
    }

    /// Index of a cue within the list, if found.
    pub fn index_of(&self, id: &CueId) -> Option<usize> {
        self.cues.iter().position(|c| c.id() == *id)
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Resequence **every** cue number to its position, recursively:
    /// top-level cues get "1", "2", "3"…; children of group "3" get "3.1",
    /// "3.2"…; deeper nesting continues ("3.2.1"…).
    ///
    /// Unconditional — the explicit "Renumber All Cues" action calls this.
    /// Structural mutations use [`structure_changed`](Self::structure_changed) instead.
    /// Returns whether the assigned numbers changed.
    pub fn renumber_all(&mut self) -> bool {
        renumber_recursive(&mut self.cues, "")
    }

    /// Plan the selection in display order, skipping occupied workspace numbers.
    pub(crate) fn plan_renumber_selected<'a>(
        &'a self,
        ids: &[CueId],
        start: f64,
        increment: f64,
        other_lists: impl IntoIterator<Item = &'a Self>,
    ) -> Result<Vec<CueNumberChange>> {
        anyhow::ensure!(start.is_finite() && increment.is_finite() && increment != 0.0,
            "Enter a finite starting number and a nonzero increment.");
        let selected: HashSet<_> = ids.iter().copied().collect();
        anyhow::ensure!(!selected.is_empty(), "Select cues to renumber.");
        let mut targets = Vec::with_capacity(selected.len());
        let mut occupied = HashSet::new();
        collect_numbering_cues(&self.cues, &selected, &mut targets, &mut occupied);
        anyhow::ensure!(
            targets.len() == selected.len() && targets.iter().map(|cue| cue.id()).collect::<HashSet<_>>() == selected,
            "The cue selection changed or contains conflicting IDs."
        );
        let no_selection = HashSet::new();
        for list in other_lists {
            collect_numbering_cues(&list.cues, &no_selection, &mut targets, &mut occupied);
        }

        let mut candidate = start;
        let mut previous_candidate = String::new();
        let mut changes = Vec::with_capacity(targets.len());
        for cue in targets {
            loop {
                anyhow::ensure!(candidate.is_finite(), "The proposed cue numbering exceeds the numeric range.");
                let number = format_cue_number(candidate);
                anyhow::ensure!(number != previous_candidate,
                    "Use a larger increment to get distinct cue numbers.");
                candidate += increment;
                previous_candidate.clone_from(&number);
                if occupied.insert(number.clone()) {
                    changes.push(CueNumberChange {
                        cue_id: cue.id(),
                        previous: cue.number().map(str::to_owned),
                        number,
                    });
                    break;
                }
            }
        }
        Ok(changes)
    }

    /// Apply a plan verified against this list under the workspace lock.
    pub(crate) fn apply_renumber_plan(&mut self, changes: &[CueNumberChange]) {
        let numbers = changes.iter().map(|change| (change.cue_id, change.number.as_str())).collect();
        apply_cue_numbers(&mut self.cues, &numbers);
    }

    /// Clear every cue number, recursively — the blank slate before a manual
    /// numbering pass or after an import that brought its own scheme.
    /// Returns whether any numbers were cleared.
    pub fn clear_all_numbers(&mut self) -> bool {
        clear_numbers_recursive(&mut self.cues)
    }

    /// Maintain ID-based cart positions and the optional numbering policy.
    fn structure_changed(&mut self) {
        self.cancel_enter_follow();
        if !self.flagged_cues.is_empty() || !self.second_colors.is_empty() {
            let live = self.all_cue_ids();
            self.flagged_cues.retain(|id| live.contains(id));
            self.second_colors.retain(|id, _| live.contains(id));
        }
        if let Some(grid) = &mut self.cart_layout {
            grid.sync(self.cues.iter().map(|cue| cue.id()));
        }
        if self.auto_renumber {
            renumber_recursive(&mut self.cues, "");
        }
    }

    /// Next free top-level integer cue number as a string (max existing +1).
    /// Used to number a freshly-created cue without disturbing the others.
    pub fn next_available_number(&self) -> String {
        let max = self
            .cues
            .iter()
            .filter_map(|c| c.number().and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        (max + 1).to_string()
    }

    /// Make `cue`'s number unique in this list before it is inserted — QLab
    /// numbers are stable, but a pasted/duplicated cue must not shadow the
    /// number of its source.  A colliding cue takes the next free number and,
    /// for a Group, its children are re-prefixed to match (`3.1`, `3.2`, …).
    pub fn ensure_unique_number(&self, cue: &mut dyn Cue) {
        let Some(number) = cue.number() else { return };
        if !number_in_use(&self.cues, number) {
            return;
        }
        let fresh = self.next_available_number();
        cue.set_number(Some(fresh.clone()));
        if let Some(children) = cue.child_cues_mut() {
            renumber_recursive(children, &fresh);
        }
    }

    /// Append a cue to the end of the list.
    pub fn push(&mut self, cue: Box<dyn Cue>) {
        if self.cues.is_empty() && !self.independent_pads() {
            // Auto-advance playhead to first cue.
            self.playhead_cue_id = Some(cue.id());
        }
        self.cues.push(cue);
        self.structure_changed();
    }

    /// Insert a cue at the given index (0-based).
    pub fn insert(&mut self, index: usize, cue: Box<dyn Cue>) {
        let id = cue.id();
        let idx = index.min(self.cues.len());
        self.cues.insert(idx, cue);
        if self.playhead_cue_id.is_none() && !self.independent_pads() {
            self.playhead_cue_id = Some(id);
        }
        self.structure_changed();
    }

    /// Remove the cue with the given ID.  If the removed cue was at the
    /// Playhead, advance the Playhead to the next cue.
    pub fn remove(&mut self, id: &CueId) -> Result<Box<dyn Cue>> {
        let idx = self
            .index_of(id)
            .ok_or_else(|| anyhow!("Cue {:?} not found", id))?;

        let removed = self.cues.remove(idx);

        if self.playhead_cue_id == Some(*id) {
            // Move playhead to the cue now at this position, or the last cue.
            self.playhead_cue_id = self
                .cues
                .get(idx)
                .or_else(|| self.cues.last())
                .map(|c| c.id());
        }

        self.structure_changed();
        Ok(removed)
    }

    /// Move a cue from `from_index` to `to_index`.
    pub fn move_cue(&mut self, id: &CueId, to_index: usize) -> Result<()> {
        let from = self.index_of(id).ok_or_else(|| anyhow!("Cue not found"))?;
        let cue = self.cues.remove(from);
        let to = to_index.min(self.cues.len());
        self.cues.insert(to, cue);
        self.structure_changed();
        Ok(())
    }

    /// Remove multiple cues at once.  If the Playhead is among the removed
    /// cues it advances to the first remaining cue at or after the lowest
    /// removed position, falling back to the last remaining cue.
    /// Return the detached cues so the caller can stop their engine outputs.
    pub fn remove_many(&mut self, ids: &[CueId]) -> Result<Vec<Box<dyn Cue>>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids_set: HashSet<CueId> = ids.iter().cloned().collect();
        for id in ids {
            if self.index_of(id).is_none() {
                return Err(anyhow!("Cue {:?} not found", id));
            }
        }

        let new_playhead = if self.playhead_cue_id.is_some_and(|ph| ids_set.contains(&ph)) {
            let ph_idx = self
                .playhead_cue_id
                .and_then(|id| self.index_of(&id))
                .unwrap_or(0);
            // First non-removed cue at or after ph_idx.
            self.cues
                .iter()
                .skip(ph_idx)
                .find(|c| !ids_set.contains(&c.id()))
                .or_else(|| self.cues.iter().rev().find(|c| !ids_set.contains(&c.id())))
                .map(|c| c.id())
        } else {
            self.playhead_cue_id
        };

        let (removed, remaining) = std::mem::take(&mut self.cues)
            .into_iter()
            .partition(|c| ids_set.contains(&c.id()));
        self.cues = remaining;
        self.playhead_cue_id = new_playhead;
        self.structure_changed();
        Ok(removed)
    }

    /// Child membership and order are part of these live Group runs.
    /// Deleting a whole group still uses the ordinary stopped removal path.
    fn check_group_structure_change(&self, ids: &[CueId], include_group_row: bool) -> Result<()> {
        let touches_active_group = self.cues.iter().any(|group| {
            group.group_mode() == Some(GroupMode::StartFirstAndEnter)
                && (group.is_running() || group.is_paused())
                && (include_group_row || !ids.contains(&group.id()))
                && ids.iter().any(|id| contains_cue(group.as_ref(), id))
        });
        anyhow::ensure!(!touches_active_group,
            "Stop the Start First And Enter group before editing its children.");

        fn touches_live_shuffle(cue: &dyn Cue, ids: &[CueId], include_group_row: bool) -> bool {
            let live_shuffle = (cue.is_running() || cue.is_paused())
                && (cue as &dyn std::any::Any).downcast_ref::<GroupCue>()
                    .is_some_and(GroupCue::has_shuffled_playlist_run);
            if live_shuffle && (include_group_row || !ids.contains(&cue.id()))
                && ids.iter().any(|id| contains_cue(cue, id))
            {
                return true;
            }
            cue.child_cues().is_some_and(|children| children.iter().any(|child| {
                touches_live_shuffle(child.as_ref(), ids, include_group_row)
            }))
        }

        anyhow::ensure!(
            !self.cues.iter().any(|cue| touches_live_shuffle(cue.as_ref(), ids, include_group_row)),
            "Stop the shuffled Playlist before changing its child order or membership."
        );
        Ok(())
    }

    fn capture_enter_standby(&self) -> Option<(CueId, CueId)> {
        self.playhead_cue_id.zip(self.enter_standby_child_id())
    }

    /// Preserve the visible standby through idle edits, including deletion of
    /// the position's internal predecessor. Leave an intact cursor untouched.
    fn preserve_enter_standby(&mut self, before: Option<(CueId, CueId)>) {
        let Some((group_id, child_id)) = before else { return };
        if self.playhead_cue_id != Some(group_id) {
            return;
        }
        let Some(group) = self.get_mut(&group_id) else { return };
        let current = group.active_child_id();
        if current != Some(child_id) && Self::valid_enter_standby_child(group, &child_id) {
            group.set_active_child(&child_id);
        }
    }

    /// Remove a cue by ID from anywhere in the hierarchy (top-level or nested in
    /// a group).  Top-level removals adjust the Playhead exactly like [`remove`];
    /// nested removals leave a surviving Enter standby child selected.
    pub fn remove_anywhere(&mut self, id: &CueId) -> Result<Box<dyn Cue>> {
        if self.index_of(id).is_some() {
            return self.remove(id);
        }
        self.check_group_structure_change(std::slice::from_ref(id), false)?;
        let standby = self.capture_enter_standby();
        let removed = extract_cue_anywhere(&mut self.cues, id)
            .ok_or_else(|| anyhow!("Cue {:?} not found", id))?;
        self.structure_changed();
        self.preserve_enter_standby(standby);
        Ok(removed)
    }

    /// Remove multiple cues from anywhere in the hierarchy in one operation.
    /// Nested cues are removed first (no Playhead impact); the remaining
    /// top-level cues go through the Playhead-aware [`remove_many`].
    /// The caller retains ownership of the detached cue instances.
    pub fn remove_many_anywhere(&mut self, ids: &[CueId]) -> Result<Vec<Box<dyn Cue>>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        for id in ids {
            if self.get_recursive(id).is_none() {
                return Err(anyhow!("Cue {:?} not found", id));
            }
        }
        self.check_group_structure_change(ids, false)?;
        let standby = self.capture_enter_standby();
        let nested: Vec<CueId> =
            ids.iter().copied().filter(|id| self.index_of(id).is_none()).collect();
        let top_level: Vec<CueId> =
            ids.iter().copied().filter(|id| self.index_of(id).is_some()).collect();
        // A selected nested group may already carry another selected cue.
        let mut removed: Vec<_> = nested.iter()
            .filter_map(|id| extract_cue_anywhere(&mut self.cues, id))
            .collect();
        if top_level.is_empty() {
            self.structure_changed();
        } else {
            removed.extend(self.remove_many(&top_level)?);
        }
        self.preserve_enter_standby(standby);
        Ok(removed)
    }

    /// Insert `cue` immediately after `anchor_id`, wherever the anchor lives
    /// (top-level or inside a group).  Used to place a duplicate right after its
    /// source so duplicating a group child keeps it in the same group.
    pub fn insert_after_anywhere(&mut self, anchor_id: &CueId, cue: Box<dyn Cue>) -> Result<()> {
        fn insert(
            cues: &mut Vec<Box<dyn Cue>>,
            anchor: &CueId,
            cue: Box<dyn Cue>,
        ) -> std::result::Result<(), Box<dyn Cue>> {
            if let Some(idx) = cues.iter().position(|c| c.id() == *anchor) {
                cues.insert(idx + 1, cue);
                return Ok(());
            }
            let mut carry = cue;
            for c in cues.iter_mut() {
                if let Some(children) = c.child_cues_mut() {
                    match insert(children, anchor, carry) {
                        Ok(()) => return Ok(()),
                        Err(returned) => carry = returned,
                    }
                }
            }
            Err(carry)
        }
        self.check_group_structure_change(std::slice::from_ref(anchor_id), false)?;
        let standby = self.capture_enter_standby();
        insert(&mut self.cues, anchor_id, cue)
            .map_err(|_| anyhow!("Anchor cue {:?} not found", anchor_id))?;
        self.structure_changed();
        self.preserve_enter_standby(standby);
        Ok(())
    }

    /// Move `ids` (preserving their relative order) so they appear immediately
    /// before `before_id` in the list, or at the end if `before_id` is `None`.
    /// If `before_id` is one of the moved cues the call is a no-op.
    pub fn move_before(&mut self, ids: &[CueId], before_id: Option<CueId>) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let ids_set: HashSet<CueId> = ids.iter().cloned().collect();
        for id in ids {
            if self.index_of(id).is_none() {
                return Err(anyhow!("Cue {:?} not found", id));
            }
        }
        if before_id.is_some_and(|bid| ids_set.contains(&bid)) {
            return Ok(());
        }

        let all = std::mem::take(&mut self.cues);
        let mut staying: Vec<Box<dyn Cue>> = Vec::with_capacity(all.len());
        let mut moving_map: HashMap<CueId, Box<dyn Cue>> = HashMap::new();
        for cue in all {
            if ids_set.contains(&cue.id()) {
                moving_map.insert(cue.id(), cue);
            } else {
                staying.push(cue);
            }
        }
        let moving: Vec<Box<dyn Cue>> = ids.iter().filter_map(|id| moving_map.remove(id)).collect();

        let insert_at = before_id
            .and_then(|id| staying.iter().position(|c| c.id() == id))
            .unwrap_or(staying.len());

        let mut result = Vec::with_capacity(staying.len() + moving.len());
        result.extend(staying.drain(..insert_at));
        result.extend(moving);
        result.extend(staying);
        self.cues = result;
        self.structure_changed();
        Ok(())
    }

    /// Wrap the given cues in a new [`GroupCue`] inserted at the first selected
    /// position.  Returns the new Group's ID.
    pub fn group_cues(&mut self, ids: &[CueId]) -> Result<CueId> {
        if ids.is_empty() {
            return Err(anyhow!("No cues to group"));
        }
        self.check_group_structure_change(ids, true)?;
        let ids_set: HashSet<CueId> = ids.iter().cloned().collect();

        // Record original order for computing insertion position.
        let original: Vec<CueId> = self.cues.iter().map(|c| c.id()).collect();
        let min_pos = ids
            .iter()
            .filter_map(|id| original.iter().position(|o| o == id))
            .min()
            .ok_or_else(|| anyhow!("Cue not found"))?;

        // Count non-selected cues that appear before min_pos — that is the
        // insertion index in the `staying` list.
        let insert_at = original[..min_pos]
            .iter()
            .filter(|id| !ids_set.contains(*id))
            .count();

        // Partition into staying / children; preserve ids order for children.
        let all = std::mem::take(&mut self.cues);
        let mut staying: Vec<Box<dyn Cue>> = Vec::with_capacity(all.len());
        let mut children_map: BTreeMap<usize, Box<dyn Cue>> = BTreeMap::new();
        for mut cue in all {
            if let Some(order_pos) = ids.iter().position(|id| *id == cue.id()) {
                cue.cancel_loaded_time();
                children_map.insert(order_pos, cue);
            } else {
                staying.push(cue);
            }
        }
        let children: Vec<Box<dyn Cue>> = children_map.into_values().collect();

        let mut group = crate::cue::group_cue::GroupCue::new();
        group.children = children;
        let group_id = group.id;

        staying.insert(insert_at.min(staying.len()), Box::new(group));
        self.cues = staying;
        self.structure_changed();
        Ok(group_id)
    }

    /// Dissolve a Group: insert its children at the Group's position and remove
    /// the Group itself.
    pub fn ungroup(&mut self, group_id: &CueId) -> Result<()> {
        let idx = self
            .index_of(group_id)
            .ok_or_else(|| anyhow!("Group {:?} not found", group_id))?;

        self.check_group_structure_change(std::slice::from_ref(group_id), true)?;
        let standby = self.enter_standby_child_id();
        let children = self.cues[idx].take_children()
            .ok_or_else(|| anyhow!("Cue is not a Group"))?;
        self.cues.remove(idx);

        // Adjust playhead if it pointed to the group.
        if self.playhead_cue_id == Some(*group_id) {
            self.playhead_cue_id = standby.or_else(|| children.first().map(|c| c.id()));
        }

        for (i, child) in children.into_iter().enumerate() {
            self.cues.insert(idx + i, child);
        }

        self.structure_changed();
        Ok(())
    }

    /// Move a cue (from anywhere in the hierarchy) into a group's children at
    /// the given position (−1 = append).  Both source and target are searched
    /// recursively so nested cues and nested groups are handled correctly.
    pub fn add_to_group(
        &mut self,
        cue_id: &CueId,
        group_id: &CueId,
        position: i32,
    ) -> Result<()> {
        let source = self.get_recursive(cue_id).ok_or_else(|| anyhow!("Cue {:?} not found", cue_id))?;
        anyhow::ensure!(!contains_cue(source, group_id), "A cue cannot move into itself or its descendants");
        anyhow::ensure!(self.get_recursive(group_id).and_then(|group| group.child_cues()).is_some(),
            "Group {:?} not found", group_id);
        self.check_group_structure_change(&[*cue_id, *group_id], true)?;
        let standby = self.capture_enter_standby();
        let mut cue = extract_cue_anywhere(&mut self.cues, cue_id)
            .ok_or_else(|| anyhow!("Cue {:?} not found", cue_id))?;
        cue.cancel_loaded_time();

        if self.playhead_cue_id == Some(*cue_id) {
            self.playhead_cue_id = self.cues.first().map(|c| c.id());
        }

        match add_child_to_group_anywhere(&mut self.cues, group_id, cue, position)? {
            None => {
                self.structure_changed();
                self.preserve_enter_standby(standby);
                Ok(())
            }
            Some(_) => Err(anyhow!("Group {:?} not found", group_id)),
        }
    }

    /// Remove a child from a group and reinsert it into the main list
    /// immediately after the group.
    pub fn remove_from_group(&mut self, group_id: &CueId, cue_id: &CueId) -> Result<()> {
        self.remove_many_from_group(group_id, std::slice::from_ref(cue_id))
    }

    /// Move direct children of a top-level Group after it in cue order.
    pub(crate) fn remove_many_from_group(&mut self, group_id: &CueId, ids: &[CueId]) -> Result<()> {
        let group_idx = self
            .index_of(group_id)
            .ok_or_else(|| anyhow!("Group {:?} not found", group_id))?;
        self.check_group_structure_change(ids, false)?;
        let standby = self.capture_enter_standby();
        let children = self.cues[group_idx].child_cues_mut()
            .ok_or_else(|| anyhow!("The selected parent is not a Group"))?;
        let selected: HashSet<_> = ids.iter().copied().collect();
        anyhow::ensure!(
            ids.iter().all(|id| children.iter().any(|child| child.id() == *id)),
            "A selected cue is no longer a direct child of this Group"
        );

        let (removed, remaining): (Vec<_>, _) = std::mem::take(children)
            .into_iter()
            .partition(|child| selected.contains(&child.id()));
        *children = remaining;
        drop(self.cues.splice(group_idx + 1..group_idx + 1, removed));
        self.structure_changed();
        self.preserve_enter_standby(standby);
        Ok(())
    }

    /// Move a cue from anywhere in the hierarchy to the top-level list,
    /// immediately before `before_id` (or at the end if `None`).
    pub fn move_to_top_level_before(
        &mut self,
        cue_id: &CueId,
        before_id: Option<&CueId>,
    ) -> Result<()> {
        self.check_group_structure_change(std::slice::from_ref(cue_id), false)?;
        let standby = self.capture_enter_standby();
        let cue = extract_cue_anywhere(&mut self.cues, cue_id)
            .ok_or_else(|| anyhow!("Cue {:?} not found", cue_id))?;

        if self.playhead_cue_id == Some(*cue_id) {
            self.playhead_cue_id = self.cues.first().map(|c| c.id());
        }

        let insert_at = before_id
            .and_then(|id| self.index_of(id))
            .unwrap_or(self.cues.len());

        self.cues.insert(insert_at, cue);
        self.structure_changed();
        self.preserve_enter_standby(standby);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Playhead
    // -----------------------------------------------------------------------

    /// The cue currently at the Playhead, if any.
    pub fn playhead_cue(&self) -> Option<&dyn Cue> {
        self.playhead_cue_id
            .as_ref()
            .and_then(|id| self.get(id))
    }

    /// Persist only the next direct child of the Group currently on standby.
    /// Playback clocks stay in the running Group.
    pub(crate) fn enter_standby_child_id(&self) -> Option<CueId> {
        if self.independent_pads() {
            return None;
        }
        let group = self.playhead_cue()?;
        group.active_child_id().filter(|child| Self::valid_enter_standby_child(group, child))
    }

    /// Restore a saved inner position after the outer playhead and cues have
    /// been restored. An invalid hint leaves the Group's default standby.
    pub(crate) fn restore_enter_standby_child(&mut self, child_id: Option<CueId>) {
        let Some(child) = child_id else { return };
        if self.independent_pads() {
            return;
        }
        let Some(group_id) = self.playhead_cue_id else { return };
        let Some(group) = self.get_mut(&group_id) else { return };
        if Self::valid_enter_standby_child(group, &child) {
            group.set_active_child(&child);
        }
    }

    fn valid_enter_standby_child(group: &dyn Cue, child_id: &CueId) -> bool {
        group.group_mode() == Some(GroupMode::StartFirstAndEnter) && !group.is_disabled()
            && group.child_cues().is_some_and(|children| {
                children.iter().any(|child| child.id() == *child_id && !child.is_disabled())
            })
    }

    fn next_armed_after_index(&self, index: usize) -> Option<CueId> {
        self.cues[index + 1..].iter().find(|cue| !cue.is_disabled()).map(|cue| cue.id())
    }

    pub(super) fn next_armed_after(&self, id: CueId) -> Option<CueId> {
        self.next_armed_after_index(self.index_of(&id)?)
    }

    /// Advance the Playhead to the next non-disabled cue after the current one.
    /// Returns the new Playhead cue ID, or `None` if we're past the end.
    pub fn advance_playhead(&mut self) -> Option<CueId> {
        let current = self.index_of(&self.playhead_cue_id?)?;
        let next = self.next_armed_after_index(current);
        self.playhead_cue_id = next;
        next
    }

    pub(super) fn arm_enter_follow(&mut self, group_id: CueId, wait_id: Option<CueId>, successor: Option<CueId>) {
        self.enter_follow = wait_id.zip(successor).map(|(wait_id, successor)| EnterFollow {
            group_id, wait_id, successor,
        });
    }

    pub(crate) fn cancel_enter_follow(&mut self) {
        self.enter_follow = None;
    }

    pub(super) fn cancel_enter_follow_for(&mut self, cue_id: CueId) {
        if self.enter_follow.is_some_and(|owner| {
            [owner.group_id, owner.wait_id, owner.successor].contains(&cue_id)
        }) {
            self.cancel_enter_follow();
        }
    }

    /// Keep the captured successor through the final Wait and its authored
    /// Post-Wait. Only that exact standby can consume the pending handoff.
    pub(super) fn take_enter_follow(&mut self, group_id: CueId) -> Option<CueId> {
        let owner = self.enter_follow?;
        if owner.group_id != group_id { return None; }
        let ready = self.get(&group_id)
            .filter(|cue| !cue.is_disabled() && self.mode == CueListMode::Sequential
                && self.playhead_cue_id == Some(owner.successor)
                && self.next_armed_after(group_id) == Some(owner.successor))
            .and_then(|cue| (cue as &dyn std::any::Any).downcast_ref::<GroupCue>())
            .and_then(|group| group.enter_wait_ready(owner.wait_id));
        if ready != Some(false) { self.cancel_enter_follow(); }
        (ready == Some(true)).then_some(owner.successor)
    }

    /// Keep the group available to Pause, Resume and Stop after its Wait
    /// finishes, while its exact outer cue is still waiting for Post-Wait.
    pub(super) fn released_enter_group(&self) -> Option<CueId> {
        self.enter_follow.filter(|owner| self.playhead_cue_id == Some(owner.successor))
            .map(|owner| owner.group_id)
    }

    pub(super) fn take_released_enter_follow(&mut self) -> Option<CueId> {
        let owner = self.enter_follow?;
        if self.playhead_cue_id == Some(owner.group_id) { return None; }
        self.take_enter_follow(owner.group_id)
    }

    /// Move the Playhead to a specific cue.
    ///
    /// A top-level cue ID parks the Playhead directly on it.  A cue nested in a
    /// group parks the outer Playhead on that cue's top-level ancestor. An
    /// ordered Group can also put a direct child on standby for the next GO.
    pub fn set_playhead(&mut self, id: Option<CueId>) -> Result<()> {
        let Some(cid) = id else {
            self.cancel_enter_follow();
            self.playhead_cue_id = None;
            return Ok(());
        };
        if !self.independent_pads() {
            let outer = self.prepare_trigger(cid)?;
            self.cancel_enter_follow();
            self.playhead_cue_id = Some(outer);
        }
        Ok(())
    }

    /// Addressing a group child aims its group at that child before firing.
    pub(super) fn prepare_trigger(&mut self, cid: CueId) -> Result<CueId> {
        if self.index_of(&cid).is_some() {
            return Ok(cid);
        }
        let ancestor = self
            .top_level_ancestor_of(&cid)
            .ok_or_else(|| anyhow!("Cue {:?} not found", cid))?;
        if let Some(group) = self.get_mut(&ancestor) {
            if group.playlist_advance_playhead() == Some(true) {
                anyhow::ensure!(!group.is_running() && !group.is_paused(),
                    "Stop the Playlist before parking standby on a child");
                anyhow::ensure!(group.set_active_child(&cid),
                    "This Playlist can address a direct child only");
                return Ok(ancestor);
            }
            if group.group_mode() == Some(GroupMode::StartFirstAndEnter) {
                anyhow::ensure!(!group.pending_start_admission(),
                    "Wait for the current child's output before moving standby inside the group");
                anyhow::ensure!(group.set_active_child(&cid),
                    "Start First And Enter can address a direct child only");
            } else {
                group.set_active_child(&cid);
            }
        }
        Ok(ancestor)
    }

    /// The ID of the top-level cue whose subtree contains `id` (the cue itself
    /// when it is already top-level).
    fn top_level_ancestor_of(&self, id: &CueId) -> Option<CueId> {
        self.cues.iter().find(|c| contains_cue(c.as_ref(), id)).map(|c| c.id())
    }

    // -----------------------------------------------------------------------
    // Serialisation
    // -----------------------------------------------------------------------

    /// Every cue id in this list, descending into groups.
    pub fn all_cue_ids(&self) -> HashSet<CueId> {
        fn walk(cues: &[Box<dyn Cue>], out: &mut HashSet<CueId>) {
            for cue in cues {
                out.insert(cue.id());
                if let Some(children) = cue.child_cues() {
                    walk(children, out);
                }
            }
        }
        let mut ids = HashSet::new();
        walk(&self.cues, &mut ids);
        ids
    }

    /// Serialise this cue list to a JSON [`serde_json::Value`].
    ///
    /// Triggers whose cue no longer exists are dropped here rather than on
    /// removal: several "remove" paths are really the first half of a move
    /// (ungroup, move-to-top-level), so purging there would lose a trigger the
    /// operator still wants. Filtering at save time is unambiguous, and stops
    /// deleted cues' triggers accumulating in the file forever.
    pub fn to_json(&self) -> serde_json::Value {
        let live_ids = self.all_cue_ids();
        let cues_json: Vec<serde_json::Value> = self.cues.iter().map(|c| c.serialize()).collect();
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "mode": self.mode,
            "chain_cart_cues": self.chain_cart_cues,
            "cart_layout": self.cart_layout,
            "playhead_cue_id": self.playhead_cue_id,
            "playhead_child_id": self.enter_standby_child_id(),
            "tc_config": self.tc_config,
            "hotkeys": self.hotkeys.iter().filter(|(id, _)| live_ids.contains(id)).collect::<BTreeMap<_, _>>(),
            "flagged_cues": self.flagged_cues.iter().filter(|id| live_ids.contains(id)).collect::<Vec<_>>(),
            "second_colors": self.second_colors.iter().filter(|(id, _)| live_ids.contains(id)).collect::<BTreeMap<_, _>>(),
            "wall_clock_triggers": self.wall_clock_triggers.iter()
                .filter(|(id, _)| live_ids.contains(id)).collect::<BTreeMap<_, _>>(),
            "tc_triggers": self.tc_triggers.iter()
                .filter(|(id, _)| live_ids.contains(id))
                .map(|(id, t)| serde_json::json!({ "cue_id": id, "trigger": t }))
                .collect::<Vec<_>>(),
            "midi_triggers": self.midi_triggers.iter()
                .filter(|(id, _)| live_ids.contains(id))
                .map(|(id, t)| serde_json::json!({ "cue_id": id, "trigger": t }))
                .collect::<Vec<_>>(),
            "cues": cues_json,
        })
    }

    /// Deserialise a cue list from JSON using the given registry.
    pub fn from_json(value: serde_json::Value, registry: &CueRegistry) -> anyhow::Result<Self> {
        let id: Uuid = value
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(Uuid::new_v4);

        let name = value
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Cue List")
            .to_string();

        let mode: CueListMode = value
            .get("mode")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let chain_cart_cues = value.get("chain_cart_cues").and_then(|v| v.as_bool())
            .unwrap_or(mode == CueListMode::Cart);
        let playhead_cue_id: Option<CueId> = value
            .get("playhead_cue_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok());
        let playhead_child_id = value["playhead_child_id"].as_str().and_then(|s| s.parse().ok());

        let mut cues: Vec<Box<dyn Cue>> = Vec::new();
        if let Some(arr) = value.get("cues").and_then(|v| v.as_array()) {
            for cue_val in arr {
                match registry.from_json(cue_val.clone()) {
                    Ok(cue) => cues.push(cue),
                    Err(e) => log::warn!("Skipping unrecognised cue: {e}"),
                }
            }
        }

        // TODO(compat): Remove once all workspace files have been re-saved with
        // `target_cue_id`.  This pass converts the legacy `target_cue_number`
        // string into a stable UUID for the current session.  It is a no-op for
        // files that already carry `target_cue_id`.  When removed, also delete:
        //   - `StopCue.target_cue_number` field and its serialize/from_json code
        //   - `Cue::resolve_stop_target` trait method and its impl in StopCue
        //   - The `target_cue_number` field in `StopCueData` (types.ts)
        fn collect_numbers(cues: &[Box<dyn Cue>], numbers: &mut HashMap<String, CueId>) {
            for cue in cues {
                if let Some(number) = cue.number() { numbers.insert(number.to_owned(), cue.id()); }
                if let Some(children) = cue.child_cues() { collect_numbers(children, numbers); }
            }
        }
        fn resolve_targets(cues: &mut [Box<dyn Cue>], numbers: &HashMap<String, CueId>) {
            for cue in cues {
                cue.resolve_stop_target(numbers);
                cue.resolve_fade_targets(numbers);
                if let Some(children) = cue.child_cues_mut() { resolve_targets(children, numbers); }
            }
        }
        let mut numbers = HashMap::new();
        collect_numbers(&cues, &mut numbers);
        resolve_targets(&mut cues, &numbers);

        let tc_config: CueListTcConfig = value
            .get("tc_config")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        let tc_triggers: HashMap<CueId, TcTrigger> = value
            .get("tc_triggers")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().filter_map(|item| {
                    let id: CueId = item.get("cue_id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok())?;
                    let trigger: TcTrigger = serde_json::from_value(
                        item.get("trigger")?.clone()).ok()?;
                    Some((id, trigger))
                }).collect()
            })
            .unwrap_or_default();

        let midi_triggers: HashMap<CueId, MidiTrigger> = value
            .get("midi_triggers")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().filter_map(|item| {
                    let id: CueId = item.get("cue_id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok())?;
                    let trigger: MidiTrigger = serde_json::from_value(
                        item.get("trigger")?.clone()).ok()?;
                    Some((id, trigger))
                }).collect()
            })
            .unwrap_or_default();

        let hotkeys: BTreeMap<CueId, String> = match value.get("hotkeys") {
            Some(saved) => serde_json::from_value(saved.clone())?,
            None => BTreeMap::new(),
        };
        let wall_clock_triggers: BTreeMap<CueId, WallClockTrigger> = match value.get("wall_clock_triggers") {
            Some(saved) => serde_json::from_value(saved.clone())?,
            None => BTreeMap::new(),
        };
        for trigger in wall_clock_triggers.values() {
            trigger.validate()?;
        }
        let mut used = HashSet::new();
        for key in hotkeys.values() {
            anyhow::ensure!(valid_hotkey(key) && used.insert(key), "Invalid or repeated cue hotkey: {key}");
        }
        let mut cart_layout: Option<CartLayout> = serde_json::from_value(value["cart_layout"].clone())?;
        if let Some(grid) = &mut cart_layout {
            grid.validate()?;
            grid.sync(cues.iter().map(|cue| cue.id()));
        }
        let mut list = Self {
            id,
            name,
            mode,
            cart_layout,
            chain_cart_cues,
            cues,
            playhead_cue_id: playhead_cue_id.filter(|_| mode != CueListMode::Cart || chain_cart_cues),
            enter_follow: None,
            tc_config,
            tc_triggers,
            midi_triggers,
            hotkeys,
            flagged_cues: BTreeSet::new(),
            second_colors: BTreeMap::new(),
            wall_clock_triggers,
            wall_clock_session: WallClockSession::default(),
            independent_runs: BTreeSet::new(),
            auto_renumber: false,
        };
        list.restore_enter_standby_child(playhead_child_id);
        let live = list.all_cue_ids();
        if let Some(saved) = value["flagged_cues"].as_array() {
            list.flagged_cues = saved.iter().filter_map(|id| id.as_str()?.parse().ok())
                .filter(|id| live.contains(id)).collect();
        }
        if let Some(saved) = value["second_colors"].as_object() {
            list.second_colors = saved.iter().filter_map(|(id, color)| {
                Some((id.parse().ok()?, serde_json::from_value(color.clone()).ok()?))
            }).filter(|(id, _)| live.contains(id)).collect();
        }
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{memo_cue::MemoCue, types::CueType};

    #[test]
    fn flags_follow_group_children_and_drop_deleted_ids() {
        use crate::cue::{group_cue::GroupCueFactory, memo_cue::MemoCueFactory};
        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        let mut list = CueList::new("Review");
        let child = memo();
        let child_id = child.id();
        list.push(child);
        list.flagged_cues.insert(child_id);
        let group = list.group_cues(&[child_id]).unwrap();
        list.flagged_cues.insert(group);
        let mut saved = list.to_json();
        saved["flagged_cues"].as_array_mut().unwrap()
            .extend([serde_json::json!(Uuid::new_v4()), serde_json::json!(true)]);
        let mut loaded = CueList::from_json(saved.clone(), &registry).unwrap();
        assert_eq!(loaded.flagged_cues, BTreeSet::from([group, child_id]));
        loaded.ungroup(&group).unwrap();
        assert_eq!(loaded.flagged_cues, BTreeSet::from([child_id]));
        loaded.remove(&child_id).unwrap();
        assert!(loaded.flagged_cues.is_empty());
        saved.as_object_mut().unwrap().remove("flagged_cues");
        assert!(CueList::from_json(saved, &registry).unwrap().flagged_cues.is_empty());
    }

    #[test]
    fn invalid_group_edits_keep_the_cue_tree_flags_and_playhead() {
        let mut list = CueList::new("Review");
        let child = memo();
        let child_id = child.id();
        list.push(child);
        let group = list.group_cues(&[child_id]).unwrap();
        let leaf = memo();
        let leaf_id = leaf.id();
        list.push(leaf);
        list.playhead_cue_id = Some(group);
        list.flagged_cues.extend([group, child_id, leaf_id]);
        let before = list.to_json();
        assert!(list.ungroup(&leaf_id).is_err());
        for invalid in [Uuid::new_v4(), leaf_id, child_id, group] {
            assert!(list.add_to_group(&group, &invalid, -1).is_err());
        }
        assert_eq!(list.to_json(), before);
    }

    #[test]
    fn cart_layout_is_opt_in_and_survives_native_workspace_json() {
        let registry = CueRegistry::new();
        let legacy = serde_json::json!({"mode": "cart", "cues": []});
        let mut list = CueList::from_json(legacy, &registry).unwrap();
        assert!(list.cart_layout.is_none());
        list.cart_layout = list.edited_cart_layout(CartEdit::Resize { rows: 3, columns: 5 }).unwrap();
        let restored = CueList::from_json(list.to_json(), &registry).unwrap();
        assert_eq!(restored.cart_layout, list.cart_layout);
        assert!(restored.chain_cart_cues);
    }

    fn memo() -> Box<dyn Cue> {
        Box::new(MemoCue::new())
    }

    fn numbered_memo(n: Option<&str>) -> Box<dyn Cue> {
        let mut m = MemoCue::new();
        m.set_number(n.map(|s| s.to_string()));
        Box::new(m)
    }

    fn numbers(list: &CueList) -> Vec<Option<String>> {
        list.cues.iter().map(|c| c.number().map(|s| s.to_string())).collect()
    }

    #[test]
    fn reorder_preserves_numbers_when_auto_off() {
        // Default (auto off, QLab-style): moving cues must NOT touch any number —
        // a blank memo stays blank, a custom "Intro" stays "Intro".
        let mut list = CueList::new("Test");
        let intro = numbered_memo(Some("Intro"));
        let blank = numbered_memo(None);
        let one = numbered_memo(Some("1"));
        let (intro_id, blank_id) = (intro.id(), blank.id());
        list.push(intro);
        list.push(blank);
        list.push(one);

        // Move the blank memo to the front.
        list.move_before(&[blank_id], Some(intro_id)).unwrap();

        assert_eq!(
            numbers(&list),
            vec![None, Some("Intro".into()), Some("1".into())],
            "reorder with auto-renumber off must leave every number untouched",
        );
    }

    #[test]
    fn reorder_renumbers_when_auto_on() {
        let mut list = CueList::new("Test");
        list.auto_renumber = true;
        list.push(numbered_memo(Some("Intro")));
        list.push(numbered_memo(None));
        let last = numbered_memo(Some("x"));
        let last_id = last.id();
        list.push(last);

        // Move the last cue to the front; auto-renumber rewrites all to position.
        let first_id = list.cues[0].id();
        list.move_before(&[last_id], Some(first_id)).unwrap();

        assert_eq!(
            numbers(&list),
            vec![Some("1".into()), Some("2".into()), Some("3".into())],
            "with auto-renumber on, reorder resequences every number by position",
        );
    }

    #[test]
    fn renumber_all_is_explicit_and_unconditional() {
        // Even with auto off, the explicit action resequences everything.
        let mut list = CueList::new("Test");
        list.push(numbered_memo(Some("Intro")));
        list.push(numbered_memo(None));
        list.push(numbered_memo(Some("99")));

        list.renumber_all();

        assert_eq!(numbers(&list), vec![Some("1".into()), Some("2".into()), Some("3".into())]);
    }

    #[test]
    fn renumber_selected_leaves_the_surrounding_cues_alone() {
        let mut list = CueList::new("Test");
        list.push(numbered_memo(Some("Intro")));
        let a = numbered_memo(Some("99"));
        let a_id = a.id();
        list.push(a);
        let b = numbered_memo(None);
        let b_id = b.id();
        list.push(b);
        list.push(numbered_memo(Some("Outro")));

        let changes = list.plan_renumber_selected(&[a_id, b_id], 10.0, 10.0, []).unwrap();
        list.apply_renumber_plan(&changes);

        assert_eq!(
            numbers(&list),
            vec![Some("Intro".into()), Some("10".into()), Some("20".into()), Some("Outro".into())],
            "only the selection is resequenced — its neighbours keep their numbers",
        );
    }

    #[test]
    fn renumber_selected_follows_display_order_not_selection_order() {
        let mut list = CueList::new("Test");
        let first = numbered_memo(None);
        let first_id = first.id();
        list.push(first);
        let second = numbered_memo(None);
        let second_id = second.id();
        list.push(second);

        // Ids handed over "backwards" — the list order is what decides.
        let changes = list.plan_renumber_selected(&[second_id, first_id], 1.0, 1.0, []).unwrap();
        list.apply_renumber_plan(&changes);

        assert_eq!(numbers(&list), vec![Some("1".into()), Some("2".into())]);
    }

    #[test]
    fn renumber_selected_writes_fractional_steps_without_trailing_zeros() {
        let mut list = CueList::new("Test");
        let ids: Vec<_> = (0..3)
            .map(|_| {
                let c = numbered_memo(None);
                let id = c.id();
                list.push(c);
                id
            })
            .collect();

        let changes = list.plan_renumber_selected(&ids, 1.0, 0.5, []).unwrap();
        list.apply_renumber_plan(&changes);

        assert_eq!(
            numbers(&list),
            vec![Some("1".into()), Some("1.5".into()), Some("2".into())],
            "a whole number must read as \"2\", never \"2.0\"",
        );
    }

    #[test]
    fn clear_all_numbers_empties_every_number() {
        let mut list = CueList::new("Test");
        list.push(numbered_memo(Some("Intro")));
        list.push(numbered_memo(Some("7")));

        list.clear_all_numbers();

        assert_eq!(numbers(&list), vec![None, None]);
    }

    #[test]
    fn next_available_number_is_max_plus_one() {
        let mut list = CueList::new("Test");
        assert_eq!(list.next_available_number(), "1"); // empty
        list.push(numbered_memo(Some("1")));
        list.push(numbered_memo(Some("4")));
        list.push(numbered_memo(None)); // non-numeric ignored
        assert_eq!(list.next_available_number(), "5");
    }

    #[test]
    fn push_sets_playhead_on_first_cue() {
        let mut list = CueList::new("Test");
        let cue = memo();
        let id = cue.id();
        list.push(cue);
        assert_eq!(list.playhead_cue_id, Some(id));
    }

    #[test]
    fn advance_playhead() {
        let mut list = CueList::new("Test");
        let c1 = memo();
        let c2 = memo();
        let id2 = c2.id();
        list.push(c1);
        list.push(c2);
        list.advance_playhead();
        assert_eq!(list.playhead_cue_id, Some(id2));
    }

    #[test]
    fn advance_past_end_returns_none() {
        let mut list = CueList::new("Test");
        list.push(memo());
        let result = list.advance_playhead();
        assert!(result.is_none());
        assert!(list.playhead_cue_id.is_none());
    }

    #[test]
    fn remove_advances_playhead() {
        let mut list = CueList::new("Test");
        let c1 = memo();
        let c2 = memo();
        let id1 = c1.id();
        let id2 = c2.id();
        list.push(c1);
        list.push(c2);
        assert_eq!(list.playhead_cue_id, Some(id1));
        list.remove(&id1).unwrap();
        assert_eq!(list.playhead_cue_id, Some(id2));
    }

    #[test]
    fn move_cue_reorders() {
        let mut list = CueList::new("Test");
        let c1 = memo();
        let c2 = memo();
        let c3 = memo();
        let id1 = c1.id();
        let id3 = c3.id();
        list.push(c1);
        list.push(c2);
        list.push(c3);
        // Move c3 to position 0.
        list.move_cue(&id3, 0).unwrap();
        assert_eq!(list.cues[0].id(), id3);
        assert_eq!(list.cues[1].id(), id1);
    }

    #[test]
    fn remove_many_basic() {
        let mut list = CueList::new("Test");
        let c1 = memo(); let id1 = c1.id();
        let c2 = memo(); let id2 = c2.id();
        let c3 = memo(); let id3 = c3.id();
        list.push(c1); list.push(c2); list.push(c3);
        list.remove_many(&[id1, id3]).unwrap();
        assert_eq!(list.cues.len(), 1);
        assert_eq!(list.cues[0].id(), id2);
    }

    #[test]
    fn remove_many_advances_playhead() {
        let mut list = CueList::new("Test");
        let c1 = memo(); let id1 = c1.id();
        let c2 = memo(); let id2 = c2.id();
        let c3 = memo(); let id3 = c3.id();
        list.push(c1); list.push(c2); list.push(c3);
        // Playhead starts at c1.
        list.remove_many(&[id1, id2]).unwrap();
        // Only c3 remains; playhead should advance to it.
        assert_eq!(list.playhead_cue_id, Some(id3));
    }

    #[test]
    fn move_before_reorders_group() {
        let mut list = CueList::new("Test");
        let c1 = memo(); let id1 = c1.id();
        let c2 = memo(); let id2 = c2.id();
        let c3 = memo(); let id3 = c3.id();
        list.push(c1); list.push(c2); list.push(c3);
        // Move c2 and c3 before c1 → [c2, c3, c1]
        list.move_before(&[id2, id3], Some(id1)).unwrap();
        assert_eq!(list.cues[0].id(), id2);
        assert_eq!(list.cues[1].id(), id3);
        assert_eq!(list.cues[2].id(), id1);
    }

    #[test]
    fn move_before_none_appends_to_end() {
        let mut list = CueList::new("Test");
        let c1 = memo(); let id1 = c1.id();
        let c2 = memo();
        let c3 = memo(); let id3 = c3.id();
        list.push(c1); list.push(c2); list.push(c3);
        list.move_before(&[id1], None).unwrap();
        assert_eq!(list.cues.last().unwrap().id(), id1);
        assert_eq!(list.cues[1].id(), id3);
    }

    #[test]
    fn serialise_roundtrip() {
        use crate::cue::{memo_cue::MemoCueFactory, registry::CueRegistry};
        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));

        let mut list = CueList::new("Round-trip");
        let mut cue = MemoCue::new();
        cue.set_name("A Memo".to_string());
        list.push(Box::new(cue));

        let json = list.to_json();
        let restored = CueList::from_json(json, &registry).unwrap();
        assert_eq!(restored.name, "Round-trip");
        assert_eq!(restored.cues.len(), 1);
        assert_eq!(restored.cues[0].name(), "A Memo");
    }

    /// Build a list holding one group whose only child is a memo.
    /// Returns (list, group_id, child_id).
    fn list_with_group_child() -> (CueList, CueId, CueId) {
        let mut list = CueList::new("Test");
        let mut group = crate::cue::group_cue::GroupCue::new();
        let child = memo();
        let child_id = child.id();
        group.children.push(child);
        let group_id = group.id;
        list.push(Box::new(group));
        (list, group_id, child_id)
    }

    #[test]
    fn get_recursive_finds_nested_child() {
        let (list, _group_id, child_id) = list_with_group_child();
        // Top-level get cannot see a nested cue …
        assert!(list.get(&child_id).is_none());
        // … but the recursive variant can.
        assert!(list.get_recursive(&child_id).is_some());
    }

    #[test]
    fn remove_anywhere_removes_nested_child() {
        let (mut list, group_id, child_id) = list_with_group_child();
        list.remove_anywhere(&child_id).unwrap();
        assert!(list.get_recursive(&child_id).is_none());
        // The group is still present, now empty.
        let group = list.get(&group_id).expect("group still present");
        assert_eq!(group.child_cues().unwrap().len(), 0);
    }

    #[test]
    fn remove_many_anywhere_mixes_nested_and_top_level() {
        let (mut list, _group_id, child_id) = list_with_group_child();
        let top = memo();
        let top_id = top.id();
        list.push(top);
        list.remove_many_anywhere(&[child_id, top_id]).unwrap();
        assert!(list.get_recursive(&child_id).is_none());
        assert!(list.get(&top_id).is_none());
    }

    #[test]
    fn insert_after_anywhere_keeps_copy_in_group() {
        let (mut list, group_id, child_id) = list_with_group_child();
        let sibling = memo();
        let sibling_id = sibling.id();
        list.insert_after_anywhere(&child_id, sibling).unwrap();
        let group = list.get(&group_id).unwrap();
        let children = group.child_cues().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].id(), child_id);
        assert_eq!(children[1].id(), sibling_id);
    }

    #[test]
    fn set_playhead_on_sequential_child_parks_on_group() {
        use crate::cue::types::GroupMode;
        let mut list = CueList::new("Test");
        let mut group = crate::cue::group_cue::GroupCue::new();
        group.mode = GroupMode::Sequential;
        let c0 = memo();
        let c1 = memo();
        let c1_id = c1.id();
        group.children.push(c0);
        group.children.push(c1);
        let group_id = group.id;
        list.push(Box::new(group));

        list.set_playhead(Some(c1_id)).unwrap();
        // Outer playhead parks on the group …
        assert_eq!(list.playhead_cue_id, Some(group_id));
        // … and the group's next-to-fire child is the one we picked.
        assert_eq!(list.get(&group_id).unwrap().active_child_id(), Some(c1_id));
    }

    #[test]
    fn goto_number_parks_an_armed_enter_child_and_refuses_ambiguity() {
        let mut list = CueList::new("Standby numbers");
        let mut group = crate::cue::group_cue::GroupCue::new();
        group.mode = GroupMode::StartFirstAndEnter;
        let mut target = memo();
        target.set_number(Some("3.5".into()));
        let id = target.id();
        group.children = vec![memo(), target];
        let outer = group.id;
        list.push(Box::new(group));

        assert_eq!(list.goto_number_target("3.5").unwrap(), id);
        list.set_playhead(Some(id)).unwrap();
        assert_eq!(list.playhead_cue_id, Some(outer));
        assert_eq!(list.enter_standby_child_id(), Some(id));
        assert!(list.goto_number_target("3.50").is_err());

        list.get_mut_recursive(&id).unwrap().set_disabled(true);
        assert!(list.goto_number_target("3.5").unwrap_err().to_string().contains("disarmed"));
        let mut duplicate = memo();
        duplicate.set_number(Some("3.5".into()));
        list.push(duplicate);
        assert!(list.goto_number_target("3.5").unwrap_err().to_string().contains("More than one"));
        assert_eq!(list.playhead_cue_id, Some(outer));
    }

    #[test]
    fn enter_standby_ignores_invalid_or_legacy_child_hints() {
        use crate::cue::{group_cue::{GroupCue, GroupCueFactory}, memo_cue::MemoCueFactory};
        use serde_json::json;

        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        let first = memo();
        let first_id = first.id();
        let second = memo();
        let second_id = second.id();
        let mut disarmed = memo();
        disarmed.set_disabled(true);
        let disarmed_id = disarmed.id();
        let grandchild = memo();
        let grandchild_id = grandchild.id();
        let mut nested = GroupCue::new();
        nested.children.push(grandchild);
        let mut group = GroupCue::new();
        group.mode = GroupMode::StartFirstAndEnter;
        group.children = vec![first, second, disarmed, Box::new(nested)];
        let group_id = group.id();
        let mut other = GroupCue::new();
        other.mode = GroupMode::StartFirstAndEnter;
        let foreign = memo();
        let foreign_id = foreign.id();
        other.children.push(foreign);
        let other_id = other.id();

        let mut list = CueList::new("Inner standby");
        list.push(Box::new(group));
        list.push(Box::new(other));
        list.set_playhead(Some(second_id)).unwrap();
        let saved = list.to_json();
        assert_eq!(saved["playhead_child_id"], json!(second_id));

        for hint in [json!("invalid"), json!(false), json!(foreign_id), json!(grandchild_id), json!(disarmed_id)] {
            let mut input = saved.clone();
            input["playhead_child_id"] = hint;
            let loaded = CueList::from_json(input, &registry).unwrap();
            assert_eq!(loaded.playhead_cue_id, Some(group_id));
            assert_eq!(loaded.enter_standby_child_id(), Some(first_id));
        }

        let mut legacy = saved.clone();
        legacy.as_object_mut().unwrap().remove("playhead_child_id");
        let loaded = CueList::from_json(legacy, &registry).unwrap();
        assert_eq!(loaded.enter_standby_child_id(), Some(first_id));

        let mut wrong_mode = saved.clone();
        wrong_mode["cues"][0]["group_mode"] = json!("sequential");
        let loaded = CueList::from_json(wrong_mode, &registry).unwrap();
        assert_eq!(loaded.playhead_cue().unwrap().active_child_id(), Some(first_id));
        assert_eq!(loaded.enter_standby_child_id(), None);

        let mut wrong_playhead = saved.clone();
        wrong_playhead["playhead_cue_id"] = json!(other_id);
        let loaded = CueList::from_json(wrong_playhead, &registry).unwrap();
        assert_eq!(loaded.playhead_cue_id, Some(other_id));
        assert_eq!(loaded.enter_standby_child_id(), Some(foreign_id));

        let mut disarmed_group = saved.clone();
        disarmed_group["cues"][0]["is_disabled"] = json!(true);
        let loaded = CueList::from_json(disarmed_group, &registry).unwrap();
        assert_eq!(loaded.playhead_cue().unwrap().active_child_id(), Some(first_id));
        assert_eq!(loaded.enter_standby_child_id(), None);

        let mut independent_cart = saved;
        independent_cart["mode"] = json!("cart");
        let loaded = CueList::from_json(independent_cart, &registry).unwrap();
        assert_eq!(loaded.playhead_cue_id, None);
        assert_eq!(loaded.enter_standby_child_id(), None);
    }

    #[test]
    fn idle_enter_standby_survives_predecessor_edits_after_reopen() {
        use crate::{
            cue::{
                group_cue::{GroupCue, GroupCueFactory},
                memo_cue::MemoCueFactory,
                registry::CueRegistry,
                types::{ContinueMode, CueType},
                wait_cue::{WaitCue, WaitCueFactory},
            },
            show::transport::Transport,
        };

        let first = memo();
        let first_id = first.id();
        let mut second = WaitCue::new();
        second.set_continue_mode(ContinueMode::DoNotContinue);
        let second_id = second.id();
        let mut group = GroupCue::new();
        group.mode = GroupMode::StartFirstAndEnter;
        group.children = vec![first, Box::new(second)];
        let group_id = group.id();
        let mut list = CueList::new("Reopened standby");
        list.push(Box::new(group));
        list.push(memo());
        list.set_playhead(Some(second_id)).unwrap();
        let saved = list.to_json();

        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        registry.register(CueType::Wait, Box::new(WaitCueFactory));
        let (context, _, _) = crate::test_support::recording_context();
        let mut transport = Transport::new(context);

        for edit in ["delete", "remove from group", "drag out", "reorder"] {
            let mut reopened = CueList::from_json(saved.clone(), &registry).unwrap();
            assert_eq!(reopened.enter_standby_child_id(), Some(second_id));
            match edit {
                "delete" => { reopened.remove_many_anywhere(&[first_id]).unwrap(); }
                "remove from group" => reopened.remove_from_group(&group_id, &first_id).unwrap(),
                "drag out" => reopened.move_to_top_level_before(&first_id, None).unwrap(),
                "reorder" => reopened.add_to_group(&first_id, &group_id, -1).unwrap(),
                _ => unreachable!(),
            }
            assert_eq!(reopened.playhead_cue_id, Some(group_id), "{edit}");
            assert_eq!(reopened.enter_standby_child_id(), Some(second_id), "{edit}");
            transport.go(&mut reopened).unwrap();
            assert!(reopened.get_recursive(&second_id).unwrap().is_running());
        }
    }

    #[test]
    fn active_enter_membership_edits_reject_before_removing_or_reordering_children() {
        use crate::{
            cue::{group_cue::GroupCue, types::ContinueMode},
            show::{event_loop::release_admitted_playhead, transport::Transport},
        };

        let mut first = memo();
        first.set_continue_mode(ContinueMode::DoNotContinue);
        let first_id = first.id();
        let second = memo();
        let second_id = second.id();
        let mut group = GroupCue::new();
        group.mode = GroupMode::StartFirstAndEnter;
        group.children = vec![first, second];
        let group_id = group.id();
        let outside = memo();
        let outside_id = outside.id();
        let mut list = CueList::new("Active standby");
        list.push(Box::new(group));
        list.push(outside);
        let (context, _, _) = crate::test_support::recording_context();
        Transport::new(context).go(&mut list).unwrap();
        assert_eq!(list.enter_standby_child_id(), Some(second_id));

        assert!(list.remove_many_anywhere(&[first_id, outside_id]).is_err());
        assert!(list.remove_from_group(&group_id, &first_id).is_err());
        assert!(list.move_to_top_level_before(&first_id, None).is_err());
        assert!(list.add_to_group(&first_id, &group_id, -1).is_err());
        assert!(list.insert_after_anywhere(&first_id, memo()).is_err());
        assert!(list.ungroup(&group_id).is_err());
        assert!(list.get_recursive(&first_id).is_some());
        assert!(list.get(&outside_id).is_some());
        assert_eq!(list.enter_standby_child_id(), Some(second_id));
        assert!(!release_admitted_playhead(&mut list));

        let removed = list.remove_many_anywhere(&[group_id]).unwrap();
        assert_eq!(removed[0].id(), group_id);
        assert_eq!(list.playhead_cue_id, Some(outside_id));
    }

    #[test]
    fn active_shuffled_playlist_rejects_partial_child_removal() {
        use crate::cue::wait_cue::WaitCue;
        use crate::show::transport::Transport;

        let mut group = GroupCue::new();
        group.mode = GroupMode::Playlist;
        group.set_playlist_auto_shuffle(true);
        let child = WaitCue::new();
        let child_id = child.id();
        group.children.push(Box::new(child));
        let group_id = group.id();
        let mut list = CueList::new("Shuffle rehearsal");
        let outside = memo();
        let outside_id = outside.id();
        list.push(Box::new(group));
        list.push(outside);
        let (context, _, _) = crate::test_support::recording_context();
        Transport::new(context.clone()).go(&mut list).unwrap();

        assert!(list.get_recursive(&child_id).unwrap().is_running());
        assert!(list.remove_many_anywhere(&[child_id, outside_id]).is_err());
        assert!(list.get_recursive(&child_id).unwrap().is_running());
        assert!(list.get(&outside_id).is_some());
        assert_eq!(list.playhead_cue_id, Some(outside_id));

        list.get_mut(&group_id).unwrap().stop(&context).unwrap();
        assert_eq!(list.remove_many_anywhere(&[child_id, outside_id]).unwrap().len(), 2);
    }

    fn add_tc_mark(list: &mut CueList, position: crate::engine::timecode_types::TcPosition) -> CueId {
        let cue = memo();
        let id = cue.id();
        list.push(cue);
        list.tc_triggers.insert(id, TcTrigger { position, real_time: false });
        id
    }

    #[test]
    fn tc_marks_fire_in_order_across_continuous_mtc_frames_and_rearm_after_locate() {
        use crate::engine::timecode_types::{TcEvent, TcPosition, TcRate};

        let mut list = CueList::new("Test");
        list.tc_config.enabled = true;
        list.tc_config.rate = TcRate::Fps25;
        let pos = |frame| TcPosition::from_frame_number(frame, TcRate::Fps25);
        let later = add_tc_mark(&mut list, pos(102));
        let earlier = add_tc_mark(&mut list, pos(101));
        let skipped = add_tc_mark(&mut list, pos(105));
        let landed = add_tc_mark(&mut list, pos(110));
        let mut previous = None;
        let mut go = |event| {
            TcTriggerWindow::advance(&mut previous, event)
                .map(|window| list.tc_triggers_crossed(window))
                .unwrap_or_default()
        };
        assert!(go(TcEvent::Position(pos(100), 2)).is_empty());
        assert_eq!(go(TcEvent::Position(pos(102), 2)), [earlier, later]);
        assert!(go(TcEvent::Position(pos(102), 2)).is_empty());
        assert!(go(TcEvent::Located(pos(105))).is_empty());
        assert_eq!(go(TcEvent::Position(pos(110), 2)), [landed]);
        assert!(go(TcEvent::Located(pos(100))).is_empty());
        assert!(go(TcEvent::Position(pos(100), 2)).is_empty());
        assert_eq!(go(TcEvent::Position(pos(102), 2)), [earlier, later]);
        assert!(go(TcEvent::Position(pos(104), 2)).is_empty());
        assert_eq!(go(TcEvent::Position(pos(106), 2)), [skipped]);
    }

    #[test]
    fn tc_ltc_jump_drop_and_reverse_do_not_cross_stale_marks() {
        use crate::engine::timecode_types::{TcEvent, TcPosition, TcRate};

        let mut list = CueList::new("Test");
        list.tc_config.enabled = true;
        list.tc_config.rate = TcRate::Fps30;
        let pos = |frame| TcPosition::from_frame_number(frame, TcRate::Fps30);
        add_tc_mark(&mut list, pos(102));
        let landed = add_tc_mark(&mut list, pos(200));
        let mut previous = None;
        let mut go = |event| {
            TcTriggerWindow::advance(&mut previous, event)
                .map(|window| list.tc_triggers_crossed(window))
                .unwrap_or_default()
        };
        assert!(go(TcEvent::Position(pos(100), 1)).is_empty());
        assert_eq!(go(TcEvent::Position(pos(200), 1)), [landed]);
        assert!(go(TcEvent::Position(pos(102), 1)).is_empty());
        assert!(go(TcEvent::Stopped).is_empty());
        assert!(go(TcEvent::Position(pos(199), 1)).is_empty());
        assert_eq!(go(TcEvent::Position(pos(200), 1)), [landed]);
    }

    #[test]
    fn tc_marks_require_an_enabled_list_matching_the_incoming_rate() {
        use crate::engine::timecode_types::{TcEvent, TcPosition, TcRate};

        let mut list = CueList::new("Test");
        let zero = TcPosition::from_frame_number(0, TcRate::Fps25);
        let cue = add_tc_mark(&mut list, zero);
        let window = TcTriggerWindow::advance(&mut None, TcEvent::Position(zero, 2)).unwrap();
        assert!(list.tc_triggers_crossed(window).is_empty());
        list.tc_config.enabled = true;
        list.tc_config.rate = TcRate::Fps30;
        assert!(list.tc_triggers_crossed(window).is_empty());
        list.tc_config.rate = TcRate::Fps25;
        assert_eq!(list.tc_triggers_crossed(window), [cue]);
    }

    // -----------------------------------------------------------------------
    // MIDI triggers
    // -----------------------------------------------------------------------

    fn list_with_midi_trigger(trigger: MidiTrigger) -> (CueList, CueId) {
        let mut list = CueList::new("Test");
        let cue = memo();
        let id = cue.id();
        list.push(cue);
        list.midi_triggers.insert(id, trigger);
        (list, id)
    }

    #[test]
    fn a_midi_trigger_fires_its_cue() {
        let (list, id) = list_with_midi_trigger(MidiTrigger::default());
        assert_eq!(list.midi_triggers_matching(&[0x90, 60, 100]), vec![id]);
    }

    #[test]
    fn an_unrelated_message_fires_nothing() {
        let (list, _) = list_with_midi_trigger(MidiTrigger::default());
        assert!(list.midi_triggers_matching(&[0x90, 61, 100]).is_empty());
    }

    #[test]
    fn one_message_can_fire_several_cues() {
        // A single pad wired to a whole set of cues is a real operator layout.
        let (mut list, first) = list_with_midi_trigger(MidiTrigger::default());
        let second = memo();
        let second_id = second.id();
        list.push(second);
        list.midi_triggers.insert(second_id, MidiTrigger::default());

        let mut fired = list.midi_triggers_matching(&[0x90, 60, 64]);
        fired.sort();
        let mut expected = vec![first, second_id];
        expected.sort();
        assert_eq!(fired, expected);
    }

    #[test]
    fn the_same_message_fires_again_every_time() {
        // Unlike a TC trigger there is no monotone guard: the message *is* the
        // event, so pressing the key twice must fire the cue twice.
        let (list, id) = list_with_midi_trigger(MidiTrigger::default());
        assert_eq!(list.midi_triggers_matching(&[0x90, 60, 100]), vec![id]);
        assert_eq!(list.midi_triggers_matching(&[0x90, 60, 100]), vec![id]);
    }

    #[test]
    fn hotkeys_validate_assignments_and_survive_save_reload_and_cue_removal() {
        use crate::cue::memo_cue::MemoCueFactory;
        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        let mut list = CueList::new("Relief desk");
        let first = memo();
        let second = memo();
        let (a, b) = (first.id(), second.id());
        list.push(first);
        list.push(second);
        for invalid in ["Junk", "Space", "Escape", "Ctrl+J", "Shift+S", "shift+J", "P", "G", "ß",
            "00", "Shift+4", "Numpad4", " 4", "４", "F1", "Shift+F5", "F9", "F11", "F25", "F02"] {
            assert!(list.check_hotkey(a, Some(invalid)).is_err());
        }
        list.check_hotkey(a, Some("Shift+J")).unwrap();
        list.hotkeys.insert(a, "Shift+J".into());
        assert!(list.check_hotkey(b, Some("Shift+J")).is_err());
        list.check_hotkey(b, Some("F2")).unwrap();
        list.replace_hotkey(b, Some("Shift+F13".into())).unwrap();
        let saved = list.to_json();
        let mut loaded = CueList::from_json(saved.clone(), &registry).unwrap();
        assert_eq!(loaded.hotkeys.get(&a).map(String::as_str), Some("Shift+J"));
        assert_eq!(loaded.hotkeys.get(&b).map(String::as_str), Some("Shift+F13"));
        loaded.remove(&a).unwrap();
        loaded.check_hotkey(b, Some("Shift+J")).unwrap();
        assert_eq!(loaded.to_json()["hotkeys"], serde_json::json!({ (b.to_string()): "Shift+F13" }));
        let mut legacy = saved.clone();
        legacy.as_object_mut().unwrap().remove("hotkeys");
        assert!(CueList::from_json(legacy, &registry).unwrap().hotkeys.is_empty());
        let mut conflicting = saved;
        conflicting["hotkeys"][b.to_string()] = serde_json::json!("Shift+J");
        assert!(CueList::from_json(conflicting, &registry).is_err());

        for key in ["0", "4", "9"] {
            assert!(list.check_hotkey(b, Some(key)).is_ok());
        }
        list.replace_hotkey(a, Some("4".into())).unwrap();
        assert!(list.check_hotkey(b, Some("4")).is_err());
        let numeric = CueList::from_json(list.to_json(), &registry).unwrap();
        assert_eq!(numeric.hotkeys.get(&a).map(String::as_str), Some("4"));
    }

    #[test]
    fn wall_clock_programming_survives_reload_without_session_arming() {
        use crate::cue::memo_cue::MemoCueFactory;

        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        let mut list = CueList::new("Clock cues");
        let cue = memo();
        let id = cue.id();
        list.push(cue);
        let trigger = WallClockTrigger { time: "09:30:00".parse().unwrap(), weekdays: 31 };
        list.wall_clock_triggers.insert(id, trigger);
        list.wall_clock_session.set_armed(id, true, Observation::now());
        let mut document = list.to_json();
        let reopened = CueList::from_json(document.clone(), &registry).unwrap();
        assert_eq!(reopened.wall_clock_triggers.get(&id), Some(&trigger));
        assert!(!reopened.wall_clock_session.is_armed(id));
        document["wall_clock_triggers"][id.to_string()]["weekdays"] = serde_json::json!(128);
        assert!(CueList::from_json(document, &registry).is_err());
    }

    #[test]
    fn hotkey_assignment_requires_an_unambiguous_addressed_go() {
        use crate::cue::{group_cue::GroupCue, types::GroupMode};
        for (mode, accepts_child) in [(GroupMode::Sequential, true), (GroupMode::Playlist, true),
            (GroupMode::Simultaneous, false), (GroupMode::StartRandom, false)] {
            let mut list = CueList::new("Groups");
            let mut group = GroupCue::new();
            group.mode = mode;
            let mut inner = GroupCue::new();
            let leaf = memo();
            let (outer_id, inner_id, leaf_id) = (group.id, inner.id, leaf.id());
            inner.children.push(leaf);
            group.children.push(Box::new(inner));
            list.push(Box::new(group));
            assert!(list.hotkey_eligible(outer_id));
            assert_eq!(list.hotkey_eligible(inner_id), accepts_child);
            assert!(!list.hotkey_eligible(leaf_id));
        }
    }

    #[test]
    fn midi_triggers_survive_a_save_and_reload() {
        let (list, id) = list_with_midi_trigger(MidiTrigger {
            message_type: crate::engine::midi_trigger::MidiTriggerType::ControlChange,
            channel: 0,
            data1: 7,
            data2: Some(127),
        });
        use crate::cue::{memo_cue::MemoCueFactory, registry::CueRegistry};
        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));

        let json = list.to_json();
        let reloaded = CueList::from_json(json, &registry).unwrap();
        let trigger = reloaded.midi_triggers.get(&id).expect("trigger persisted");
        assert_eq!(trigger.data1, 7);
        assert_eq!(trigger.data2, Some(127));
        assert_eq!(trigger.channel, 0, "omni survives the roundtrip");
    }

    #[test]
    fn saving_drops_triggers_whose_cue_is_gone() {
        // Both trigger maps outlive their cue on removal (several remove paths
        // are really the first half of a move), so the file must not keep them.
        let (mut list, id) = list_with_midi_trigger(MidiTrigger::default());
        list.tc_triggers.insert(id, TcTrigger {
            position: crate::engine::timecode_types::TcPosition::new(
                1, 0, 0, 0, crate::engine::timecode_types::TcRate::Fps25),
            real_time: false,
        });
        list.remove(&id).unwrap();

        let json = list.to_json();
        assert_eq!(json["midi_triggers"].as_array().unwrap().len(), 0);
        assert_eq!(json["tc_triggers"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn unique_number_keeps_a_free_number() {
        let mut list = CueList::new("L");
        list.push(numbered_memo(Some("1")));
        let mut incoming = numbered_memo(Some("7"));
        list.ensure_unique_number(incoming.as_mut());
        assert_eq!(incoming.number(), Some("7"));
    }

    #[test]
    fn unique_number_moves_a_colliding_cue_to_the_next_free_number() {
        let mut list = CueList::new("L");
        list.push(numbered_memo(Some("1")));
        list.push(numbered_memo(Some("2")));
        let mut incoming = numbered_memo(Some("1"));
        list.ensure_unique_number(incoming.as_mut());
        assert_eq!(incoming.number(), Some("3"));
    }

    #[test]
    fn unique_number_leaves_an_unnumbered_cue_alone() {
        let mut list = CueList::new("L");
        list.push(numbered_memo(Some("1")));
        let mut incoming = numbered_memo(None);
        list.ensure_unique_number(incoming.as_mut());
        assert_eq!(incoming.number(), None);
    }
}
