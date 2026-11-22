//! Each resolved patch shares one gain across all of its voices, including future starts.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Default)]
pub struct PatchFaders(Mutex<HashMap<Uuid, Arc<PatchFader>>>);

impl PatchFaders {
    pub fn resolve(&self, id: Uuid, saved_gain: f32) -> Arc<PatchFader> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id)
            .or_insert_with(|| {
                Arc::new(PatchFader {
                    value: AtomicU32::new(saved_gain.to_bits()),
                    retired: AtomicBool::new(false),
                    owner: Mutex::new(None),
                })
            })
            .clone()
    }

    pub fn remove(&self, id: Uuid) {
        if let Some(fader) = self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
            fader.release();
        }
    }

    /// Existing tails keep their last level. The next start resolves the saved mixer baseline.
    pub fn clear(&self) {
        for (_, fader) in self.0.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            fader.release();
        }
    }
}

#[derive(Debug)]
pub struct PatchFader {
    value: AtomicU32,
    retired: AtomicBool,
    owner: Mutex<Option<Uuid>>,
}

impl PatchFader {
    /// The only operation used by the real-time audio callback.
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.value.load(Ordering::Relaxed))
    }

    fn release(&self) {
        let mut owner = self.owner.lock().unwrap_or_else(|e| e.into_inner());
        self.retired.store(true, Ordering::Relaxed);
        *owner = None;
    }

    pub fn manual(&self, gain: f32) {
        let mut owner = self.owner.lock().unwrap_or_else(|e| e.into_inner());
        *owner = None;
        self.value.store(gain.to_bits(), Ordering::Relaxed);
    }

    pub fn begin(self: &Arc<Self>, target: f32) -> Option<PatchFade> {
        let token = Uuid::new_v4();
        let mut owner = self.owner.lock().unwrap_or_else(|e| e.into_inner());
        if self.retired.load(Ordering::Relaxed) {
            return None;
        }
        let start = self.gain();
        *owner = Some(token);
        Some(PatchFade {
            fader: Arc::clone(self),
            token,
            start,
            target,
        })
    }
}

#[derive(Clone, Debug)]
pub struct PatchFade {
    fader: Arc<PatchFader>,
    token: Uuid,
    start: f32,
    target: f32,
}

impl PatchFade {
    pub fn update(&self, rising: f32, falling: f32) {
        let owner = self.fader.owner.lock().unwrap_or_else(|e| e.into_inner());
        if *owner == Some(self.token) {
            let t = if self.target >= self.start {
                rising
            } else {
                falling
            };
            let gain = self.start + (self.target - self.start) * t;
            self.fader.value.store(gain.to_bits(), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_action_owns_the_patch_and_retiring_preserves_old_audio_levels() {
        let faders = PatchFaders::default();
        let id = Uuid::new_v4();
        let earlier_voice = faders.resolve(id, 1.0);
        let down = earlier_voice.begin(0.0).unwrap();
        down.update(0.25, 0.5);
        let later_voice = faders.resolve(id, 1.0);
        assert_eq!(later_voice.gain(), 0.5);
        let up = later_voice.begin(1.0).unwrap();
        down.update(1.0, 1.0);
        up.update(0.5, 0.25);
        assert_eq!(earlier_voice.gain(), 0.75);
        earlier_voice.manual(0.125);
        up.update(1.0, 1.0);
        assert_eq!(later_voice.gain(), 0.125);
        faders.clear();
        assert!(earlier_voice.begin(1.0).is_none());
        assert_eq!(faders.resolve(id, 1.0).gain(), 1.0);
        assert_eq!(earlier_voice.gain(), 0.125);
    }
}
