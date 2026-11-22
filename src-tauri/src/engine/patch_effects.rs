//! A single prepared Output Patch bus for the exact main hardware stream.

use std::cell::UnsafeCell;

use anyhow::{anyhow, Result};

use super::{cue_effects::CueEffects, device_manager::NamedOutput};

pub(crate) const MAX_PATCH_EFFECT_CHANNELS: usize = 32;
pub(crate) const MAX_PATCH_EFFECT_FRAMES: usize = 4096;

pub(super) struct PatchBus {
    pub(super) route: NamedOutput,
    pub(super) state: UnsafeCell<PatchBusState>,
}

// SAFETY: the control thread constructs the state before publishing the first
// voice and never accesses it after that. Every voice sharing this bus is
// published to the same exact main stream. Its serial callback owns state
// until that stream is fully dropped. Strict voices cannot transfer streams.
unsafe impl Sync for PatchBus {}

impl PatchBus {
    pub(super) fn new(route: NamedOutput, channels: usize, sample_rate: u32) -> Result<Self> {
        check_capacity(channels, 0)?;
        let processor = CueEffects::for_static_chain(&route.audio_effects, channels, sample_rate)
            .ok_or_else(|| anyhow!("The patch has no active effects."))?;
        Ok(Self {
            route,
            state: UnsafeCell::new(PatchBusState {
                processor,
                samples: vec![0.0; MAX_PATCH_EFFECT_FRAMES * channels].into_boxed_slice(),
            }),
        })
    }
}

pub(super) fn check_capacity(channels: usize, frames: usize) -> Result<()> {
    anyhow::ensure!(
        (1..=MAX_PATCH_EFFECT_CHANNELS).contains(&channels) && frames <= MAX_PATCH_EFFECT_FRAMES,
        "Output Patch effects require at most {MAX_PATCH_EFFECT_CHANNELS} device channels and {MAX_PATCH_EFFECT_FRAMES} frames per audio block."
    );
    Ok(())
}

pub(super) struct PatchBusState {
    processor: CueEffects,
    samples: Box<[f32]>,
}

impl PatchBusState {
    pub(super) fn begin(&mut self, samples: usize) -> bool {
        if let Some(output) = self.samples.get_mut(..samples) {
            output.fill(0.0);
            true
        } else {
            self.reset();
            false
        }
    }

    pub(super) fn samples(&mut self, count: usize) -> &mut [f32] {
        &mut self.samples[..count]
    }

    pub(super) fn finish(&mut self, output: &mut [f32], channels: usize, sample_rate: u32) {
        self.processor.prepare(sample_rate);
        let block = &mut self.samples[..output.len()];
        for frame in block.chunks_exact_mut(channels) {
            self.processor.process(frame);
        }
        self.processor.finish_buffer();
        for (dry, wet) in output.iter_mut().zip(block) {
            *dry += *wet;
        }
    }

    pub(super) fn reset(&mut self) {
        self.processor.reset();
    }
}
