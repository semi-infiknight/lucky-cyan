use std::collections::HashMap;

use uuid::Uuid;

/// Cue loading status owned by the most recent media request for each cue.
#[derive(Default)]
pub struct LoadingCues {
    requests: HashMap<Uuid, Uuid>,
}

impl LoadingCues {
    pub(crate) fn contains(&self, cue_id: &Uuid) -> bool {
        self.requests.contains_key(cue_id)
    }

    pub(crate) fn clear(&mut self) {
        self.requests.clear();
    }

    pub(crate) fn begin(&mut self, cue_id: Uuid, request_id: Uuid) {
        self.requests.insert(cue_id, request_id);
    }

    /// Retire this request only while it owns the displayed loading state.
    pub(crate) fn finish(&mut self, cue_id: Uuid, request_id: Uuid) -> bool {
        if self.requests.get(&cue_id) != Some(&request_id) {
            return false;
        }
        self.requests.remove(&cue_id);
        true
    }
}
