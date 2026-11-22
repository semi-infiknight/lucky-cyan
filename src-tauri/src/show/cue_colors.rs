use std::collections::HashSet;

use crate::cue::{traits::Cue, types::CueId};

/// Session state for cues whose actions started at least once in this workspace.
#[derive(Default)]
pub(crate) struct CueStarts {
    started: HashSet<CueId>,
    changed: bool,
}

impl CueStarts {
    pub fn contains(&self, id: &CueId) -> bool {
        self.started.contains(id)
    }

    pub fn mark(&mut self, id: CueId) {
        self.changed |= self.started.insert(id);
    }

    pub fn clear_tree(&mut self, cue: &dyn Cue) {
        self.changed |= self.started.remove(&cue.id());
        if let Some(children) = cue.child_cues() {
            for child in children {
                self.clear_tree(child.as_ref());
            }
        }
    }

    pub fn clear(&mut self) {
        self.changed |= !self.started.is_empty();
        self.started.clear();
    }

    pub fn request_refresh(&mut self) {
        self.changed = true;
    }

    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}
