//! Physical output sample peaks, collected only while an editor requests them.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use super::voice::MATRIX_OUTPUTS;

const WINDOW_US: u64 = 125_000;
const LEASE_US: u64 = 500_000;

pub(super) struct OutputMeter {
    expires_at: AtomicU64,
    windows: [[AtomicU64; MATRIX_OUTPUTS]; 2],
}

impl OutputMeter {
    pub(super) fn new() -> Self {
        Self {
            expires_at: AtomicU64::new(0),
            windows: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
        }
    }

    /// Single callback writer per stream, with no allocations or locks.
    pub(super) fn observe(&self, now: u64, output: &[f32], channels: usize) {
        let deadline = self.expires_at.load(Relaxed);
        if deadline == 0 || now > deadline || channels == 0 || output.len() < channels {
            return;
        }
        let mut peaks = [0.0_f32; MATRIX_OUTPUTS];
        for frame in output.chunks_exact(channels) {
            for (peak, &sample) in peaks.iter_mut().zip(frame) {
                if sample.is_finite() {
                    *peak = peak.max(sample.abs());
                }
            }
        }
        let window = ((now / WINDOW_US) as u32).wrapping_add(1);
        let slots = &self.windows[(window % 2) as usize];
        // Each channel is an atomic pair of window and peak. Keeping two windows
        // avoids resetting a transient at the same time another editor reads it.
        for (slot, peak) in slots.iter().zip(peaks).take(channels) {
            let previous = slot.load(Relaxed);
            let peak = if (previous >> 32) as u32 == window {
                peak.max(f32::from_bits(previous as u32))
            } else {
                peak
            };
            slot.store(
                (u64::from(window) << 32) | u64::from(peak.to_bits()),
                Relaxed,
            );
        }
    }

    pub(super) fn read(&self, now: u64, channels: &[u16]) -> Option<Vec<f32>> {
        self.expires_at.store(now + LEASE_US, Relaxed);
        let window = ((now / WINDOW_US) as u32).wrapping_add(1);
        channels
            .iter()
            .map(|&channel| {
                self.windows
                    .iter()
                    .filter_map(|slots| {
                        let packed = slots[usize::from(channel)].load(Relaxed);
                        let age = window.wrapping_sub((packed >> 32) as u32);
                        (packed != 0 && age <= 1).then(|| f32::from_bits(packed as u32))
                    })
                    .reduce(f32::max)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_physical_peaks_in_patch_order_and_keeps_recent_transients() {
        let meter = OutputMeter::new();
        let at = WINDOW_US * 10;
        let frames = [0.25, -0.9, 1.2, -0.75, 0.0, 0.5];
        meter.observe(at, &frames, 3);
        assert_eq!(meter.read(at, &[2, 0, 2]), None);
        meter.observe(at, &frames, 3);
        meter.observe(at + WINDOW_US, &[0.1, 0.2, 0.3], 3);
        let peaks = Some(vec![1.2, 0.75, 1.2]);
        assert_eq!(meter.read(at + WINDOW_US, &[2, 0, 2]), peaks);
        assert_eq!(meter.read(at + WINDOW_US, &[2, 0, 2]), peaks);
        assert_eq!(
            meter.read(at + WINDOW_US * 2, &[0, 2]),
            Some(vec![0.1, 0.3])
        );
    }

    #[test]
    fn inactive_meter_expires_and_silence_is_a_valid_sample() {
        let meter = OutputMeter::new();
        let at = 1_000_000;
        assert_eq!(meter.read(at, &[0]), None);
        meter.observe(at, &[0.0], 1);
        assert_eq!(meter.read(at, &[0]), Some(vec![0.0]));
        meter.observe(at + LEASE_US + 1, &[0.5], 1);
        assert_eq!(meter.read(at + LEASE_US + 1, &[0]), None);
    }
}
