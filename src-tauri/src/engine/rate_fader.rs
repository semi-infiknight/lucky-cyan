//! Rate control for a single PCM voice. The mixer only reads and writes
//! atomics; Fade ownership is serialized on the control thread.

use std::sync::atomic::Ordering::{Relaxed, SeqCst};
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use uuid::Uuid;

/// Supported positive playback-rate multipliers.
pub const MIN_PLAYBACK_RATE: f64 = 0.03;
pub const MAX_PLAYBACK_RATE: f64 = 33.0;

const HOLD: u64 = 0;
const SHORT_SLEW: Duration = Duration::from_millis(5);

/// Keep invalid saved rates from stalling or running the source cursor backwards.
pub fn normalize_rate(rate: f64) -> f64 {
    if rate.is_finite() {
        rate.clamp(MIN_PLAYBACK_RATE, MAX_PLAYBACK_RATE)
    } else {
        1.0
    }
}

/// Control and rendered values for a scalar PCM voice parameter.
pub struct ScalarFader {
    requested: AtomicU64,
    anchored: Option<AnchoredRequests>,
    rendered: AtomicU32,
    owner: Mutex<Option<Uuid>>,
}

pub type RateFader = ScalarFader;

/// Long enough to carry both direction curves alongside the destination.
/// A callback that overlaps a write keeps its current segment for one buffer.
#[derive(Default)]
struct AnchoredRequests {
    stamp: AtomicU64,
    destination: AtomicU64,
    progress: AtomicU64,
}

impl AnchoredRequests {
    // Calls that write or issue tokens already own the control-thread mutex.
    fn next_revision(&self) -> u32 {
        (self.stamp.load(SeqCst) as u32).wrapping_add(2)
    }

    fn write(&self, destination: u64, progress: u64, anchor: u32) {
        let stamp = (u64::from(anchor) << 32) | u64::from(self.next_revision());
        self.stamp.store(stamp | 1, SeqCst);
        self.destination.store(destination, SeqCst);
        self.progress.store(progress, SeqCst);
        self.stamp.store(stamp, SeqCst);
    }

    fn read(&self) -> Option<(u64, u64, u32)> {
        let stamp = self.stamp.load(SeqCst);
        if stamp & 1 != 0 {
            return None;
        }
        let destination = self.destination.load(SeqCst);
        let progress = self.progress.load(SeqCst);
        (stamp == self.stamp.load(SeqCst)).then_some((destination, progress, (stamp >> 32) as u32))
    }
}

impl Default for ScalarFader {
    fn default() -> Self {
        Self {
            requested: AtomicU64::new(HOLD),
            anchored: None,
            rendered: AtomicU32::new(1.0_f32.to_bits()),
            owner: Mutex::new(None),
        }
    }
}

impl ScalarFader {
    pub(super) fn render_anchored() -> Self {
        Self { anchored: Some(AnchoredRequests::default()), ..Self::default() }
    }

    /// Set a newly created voice's starting rate before submission.
    pub fn initialize(&self, rate: f32) {
        self.publish(rate);
    }

    /// Last rendered multiplier.
    pub fn current(&self) -> f32 {
        f32::from_bits(self.rendered.load(Relaxed))
    }

    /// Take ownership from any earlier Fade and slew to a manually set rate.
    pub fn manual(&self, rate: f32) {
        let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
        *owner = None;
        self.request(rate, SHORT_SLEW);
    }

    /// Start from the rendered value, superseding any previous rate Fade.
    pub fn begin(self: &Arc<Self>, target: f32) -> ScalarFade {
        let mut owner = self.owner.lock().unwrap_or_else(|error| error.into_inner());
        let token = Uuid::new_v4();
        let start = self.current();
        *owner = Some(token);
        let anchor = self.anchored.as_ref().map_or(0, |requests| {
            // Zero denotes an ordinary Hold, manual change or final slew.
            requests.next_revision().max(1)
        });
        self.request_hold();
        ScalarFade {
            fader: Arc::clone(self),
            token,
            start,
            target,
            anchor,
        }
    }

    fn request(&self, rate: f32, duration: Duration) {
        self.write_request(pack_segment(rate, duration), 0, 0);
    }

    fn request_hold(&self) {
        self.write_request(HOLD, 0, 0);
    }

    fn write_request(&self, segment: u64, progress: u64, anchor: u32) {
        if let Some(requests) = &self.anchored {
            requests.write(segment, progress, anchor);
        } else {
            self.requested.store(segment, Relaxed);
        }
    }

    pub(super) fn publish(&self, rate: f32) {
        self.rendered.store(rate.to_bits(), Relaxed);
    }
}

/// An active Fade's exclusive ownership of this voice's rate automation.
pub struct ScalarFade {
    fader: Arc<ScalarFader>,
    token: Uuid,
    start: f32,
    target: f32,
    anchor: u32,
}

pub type RateFade = ScalarFade;

impl ScalarFade {
    /// Apply the owning Fade's sampled rising or falling envelope.
    pub fn update(&self, rising: f32, falling: f32, duration: Duration) {
        let owner = self
            .fader
            .owner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *owner == Some(self.token) {
            if self.anchor != 0 {
                let progress = (u64::from(rising.to_bits()) << 32) | u64::from(falling.to_bits());
                let segment = pack_segment(self.target, duration.max(Duration::from_micros(1)));
                self.fader.write_request(segment, progress, self.anchor);
                return;
            }
            let progress = if self.target >= self.start {
                rising
            } else {
                falling
            };
            self.fader
                .request(self.start + (self.target - self.start) * progress, duration);
        }
    }

    /// Freeze at the callback's current rate, keeping ownership for Resume.
    pub fn hold(&self) {
        let owner = self
            .fader
            .owner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *owner == Some(self.token) {
            self.fader.request_hold();
        }
    }

    /// Release ownership and leave the voice at the destination.
    pub fn finish(self) {
        let mut owner = self
            .fader
            .owner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *owner == Some(self.token) {
            self.fader.request(self.target, SHORT_SLEW);
            *owner = None;
        }
    }
}

impl Drop for ScalarFade {
    fn drop(&mut self) {
        let mut owner = self
            .fader
            .owner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *owner == Some(self.token) {
            self.fader.request_hold();
            *owner = None;
        }
    }
}

fn pack_segment(value: f32, duration: Duration) -> u64 {
    let micros = u32::try_from(duration.as_micros()).unwrap_or(u32::MAX);
    (u64::from(micros) << 32) | u64::from(value.to_bits())
}

/// Callback-only smoothing and fractional source position.
#[derive(Default)]
pub(super) struct ScalarSlew {
    previous_request: u64,
    rendered_rate: Option<f64>,
    frames_left: u32,
    step: f64,
    pub(super) source_fraction: f64,
    anchor: u32,
    anchor_value: f64,
}

pub(super) type RateSlew = ScalarSlew;

impl ScalarSlew {
    /// Replace the short ramp whenever the control thread sends a new value.
    pub(super) fn begin_buffer(
        &mut self,
        fader: &ScalarFader,
        output_rate: u32,
        paused: bool,
    ) -> f64 {
        let current = self
            .rendered_rate
            .unwrap_or_else(|| f64::from(fader.current()));
        let request = if let Some(requests) = &fader.anchored {
            let Some((destination, progress, anchor)) = requests.read() else {
                return current;
            };
            if anchor == 0 {
                destination
            } else {
                self.anchored_segment(destination, progress, anchor, current)
            }
        } else {
            fader.requested.load(Relaxed)
        };
        if request == HOLD {
            self.frames_left = 0;
            self.previous_request = request;
        } else if paused {
            self.frames_left = 0;
            self.previous_request = request;
            return f64::from(f32::from_bits(request as u32));
        } else if request != self.previous_request {
            self.previous_request = request;
            let micros = request >> 32;
            self.frames_left = (u64::from(output_rate) * micros)
                .div_ceil(1_000_000)
                .clamp(1, u64::from(u32::MAX)) as u32;
            self.step =
                (f64::from(f32::from_bits(request as u32)) - current) / f64::from(self.frames_left);
        }
        current
    }

    fn anchored_segment(&mut self, destination: u64, progress: u64, anchor: u32, current: f64) -> u64 {
        if self.anchor != anchor {
            // An earlier segment may have rendered after the new Fade was
            // called. Base its curve on the first callback's realized value.
            self.anchor = anchor;
            self.anchor_value = current;
            self.previous_request = HOLD;
        }
        let target = f64::from(f32::from_bits(destination as u32));
        let progress = if target >= self.anchor_value { progress >> 32 } else { progress };
        let progress = f64::from(f32::from_bits(progress as u32));
        let value = (self.anchor_value + (target - self.anchor_value) * progress) as f32;
        (destination & (!0_u64 << 32)) | u64::from(value.to_bits())
    }

    pub(super) fn finish_buffer(&mut self, fader: &ScalarFader, rendered_rate: f64) {
        self.rendered_rate = Some(rendered_rate);
        fader.publish(rendered_rate as f32);
    }

    /// Advance one output frame through the current rate segment.
    pub(super) fn advance(&mut self, current: &mut f64) {
        if self.frames_left > 0 {
            self.frames_left -= 1;
            if self.frames_left == 0 {
                *current = f64::from(f32::from_bits(self.previous_request as u32));
            } else {
                *current += self.step;
            }
        }
    }

    pub(super) fn remaining(&self) -> u32 {
        self.frames_left
    }

    /// Predict for a small DSP coefficient segment without moving the clock.
    pub(super) fn projected(&self, current: f64, frames: u32) -> f64 {
        if self.frames_left == 0 {
            current
        } else if frames >= self.frames_left {
            f64::from(f32::from_bits(self.previous_request as u32))
        } else {
            current + self.step * f64::from(frames)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_successor_uses_the_value_and_curve_direction_at_audio_handoff() {
        let fader = Arc::new(ScalarFader::render_anchored());
        fader.initialize(3.0);
        let mut slew = ScalarSlew::default();
        let preceding = fader.begin(6.0);
        preceding.finish();
        let mut value = slew.begin_buffer(&fader, 1_000, false);
        assert_eq!(value, 3.0);

        // GO can happen while the callback is still rendering the preceding
        // endpoint, before it has published the new rendered value.
        let successor = fader.begin(4.0);
        successor.update(0.25, 0.5, Duration::from_millis(10));
        for _ in 0..5 {
            slew.advance(&mut value);
        }
        slew.finish_buffer(&fader, value);
        assert_eq!(fader.current(), 6.0);

        value = slew.begin_buffer(&fader, 1_000, false);
        for _ in 0..10 {
            let before = value;
            slew.advance(&mut value);
            assert!(value <= before && value >= 5.0);
        }
        assert_eq!(value, 5.0);
        slew.finish_buffer(&fader, value);

        successor.hold();
        assert_eq!(slew.begin_buffer(&fader, 1_000, false), 5.0);
        assert_eq!(slew.remaining(), 0);
        successor.update(0.1, 0.75, Duration::from_millis(2));
        value = slew.begin_buffer(&fader, 1_000, false);
        for _ in 0..2 {
            slew.advance(&mut value);
        }
        assert_eq!(value, 4.5);
    }

    #[test]
    fn an_incomplete_eq_request_defers_without_changing_the_callback_segment() {
        let fader = Arc::new(ScalarFader::render_anchored());
        fader.initialize(2.0);
        let fade = fader.begin(6.0);
        fade.update(1.0, 1.0, Duration::from_millis(4));
        let mut slew = ScalarSlew::default();
        let mut value = slew.begin_buffer(&fader, 1_000, false);
        slew.advance(&mut value);
        assert_eq!(value, 3.0);
        slew.finish_buffer(&fader, value);

        let requests = fader.anchored.as_ref().unwrap();
        requests.stamp.fetch_or(1, SeqCst);
        requests.destination.store(HOLD, SeqCst);
        assert_eq!(slew.begin_buffer(&fader, 1_000, false), 3.0);
        assert_eq!(slew.remaining(), 3);
        slew.advance(&mut value);
        assert_eq!(value, 4.0);
        requests.stamp.fetch_and(!1, SeqCst);
    }
}
