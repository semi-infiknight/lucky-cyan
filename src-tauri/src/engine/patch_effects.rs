//! Prepared Output Patch buses for the exact main hardware stream.

use std::cell::UnsafeCell;

use anyhow::{anyhow, Result};

use super::{cue_effects::CueEffects, device_manager::NamedOutput};

pub(crate) const MAX_PATCH_EFFECT_CHANNELS: usize = 32;
pub(crate) const MAX_PATCH_EFFECT_FRAMES: usize = 4096;
pub(super) const MAX_PROCESSED_PATCHES: usize = 8;

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
        if !route.has_audio_effects() {
            return Err(anyhow!("The patch has no active effects."));
        }
        let processor = CueEffects::for_static_chain(&route.audio_effects, channels, sample_rate);
        let cue_outputs = if route.has_cue_output_effects() {
            check_capacity(route.channels.len(), 0)?;
            Some(CueOutputBuses {
                routes: route.channels.clone(),
                processors: route.channels.iter().enumerate().map(|(index, _)| {
                    route.cue_output_effects.iter().find(|entry| entry.output == index)
                        .and_then(|entry| CueEffects::for_static_chain(&entry.effects, 1, sample_rate))
                }).collect(),
                device_samples: vec![0.0; MAX_PATCH_EFFECT_FRAMES * channels].into_boxed_slice(),
            })
        } else {
            None
        };
        let mix_channels = cue_outputs.as_ref().map_or(channels, |outputs| outputs.routes.len());
        Ok(Self {
            route,
            state: UnsafeCell::new(PatchBusState {
                processor,
                cue_outputs,
                mix_channels,
                samples: vec![0.0; MAX_PATCH_EFFECT_FRAMES * mix_channels].into_boxed_slice(),
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
    processor: Option<CueEffects>,
    cue_outputs: Option<CueOutputBuses>,
    mix_channels: usize,
    samples: Box<[f32]>,
}

struct CueOutputBuses {
    routes: Vec<u16>,
    processors: Vec<Option<CueEffects>>,
    device_samples: Box<[f32]>,
}

impl PatchBusState {
    pub(super) fn begin(&mut self, frames: usize) -> bool {
        if let Some(output) = self.samples.get_mut(..frames * self.mix_channels) {
            output.fill(0.0);
            true
        } else {
            self.reset();
            false
        }
    }

    pub(super) fn channels(&self) -> usize {
        self.mix_channels
    }

    pub(super) fn uses_cue_outputs(&self) -> bool {
        self.cue_outputs.is_some()
    }

    pub(super) fn samples(&mut self, frames: usize) -> &mut [f32] {
        &mut self.samples[..frames * self.mix_channels]
    }

    pub(super) fn finish(&mut self, output: &mut [f32], channels: usize, sample_rate: u32) {
        let frames = output.len() / channels;
        let block = &mut self.samples[..frames * self.mix_channels];
        let hardware = if let Some(cue) = &mut self.cue_outputs {
            let hardware = &mut cue.device_samples[..frames * channels];
            hardware.fill(0.0);
            for processor in cue.processors.iter_mut().flatten() {
                processor.prepare(sample_rate);
            }
            for (frame, device) in block.chunks_exact_mut(self.mix_channels).zip(hardware.chunks_exact_mut(channels)) {
                for (index, sample) in frame.iter_mut().enumerate() {
                    if let Some(processor) = &mut cue.processors[index] {
                        processor.process(std::slice::from_mut(sample));
                    }
                    if let Some(out) = device.get_mut(usize::from(cue.routes[index])) {
                        *out += *sample;
                    }
                }
            }
            for processor in cue.processors.iter_mut().flatten() {
                processor.finish_buffer();
            }
            hardware
        } else {
            block
        };
        if let Some(processor) = &mut self.processor {
            processor.prepare(sample_rate);
            for frame in hardware.chunks_exact_mut(channels) {
                processor.process(frame);
            }
            processor.finish_buffer();
        }
        for (dry, wet) in output.iter_mut().zip(hardware) {
            *dry += *wet;
        }
    }

    pub(super) fn reset(&mut self) {
        if let Some(processor) = &mut self.processor { processor.reset(); }
        if let Some(cue) = &mut self.cue_outputs {
            for processor in cue.processors.iter_mut().flatten() {
                processor.reset();
            }
        }
    }
}
