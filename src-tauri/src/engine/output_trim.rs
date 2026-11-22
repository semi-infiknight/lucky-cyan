use std::cell::UnsafeCell;

use super::{
    rate_fader::{ScalarFader, ScalarSlew},
    voice::MATRIX_OUTPUTS,
};

pub(super) type OutputGains = [f64; MATRIX_OUTPUTS];

struct ChannelTrim {
    output: usize,
    fader: ScalarFader,
    slew: UnsafeCell<ScalarSlew>,
}

/// Control targets are independent of the crosspoint matrix and main gain.
/// Only the owning PCM callback accesses the individual slew states.
pub(super) struct ChannelTrims(Box<[ChannelTrim]>);

impl ChannelTrims {
    pub(super) fn new(channels: &[u16], gains: &[f32]) -> Self {
        Self(
            channels
                .iter()
                .enumerate()
                .map(|(index, &channel)| {
                    let fader = ScalarFader::default();
                    fader.initialize(gains.get(index).copied().unwrap_or(1.0));
                    ChannelTrim {
                        output: usize::from(channel),
                        fader,
                        slew: UnsafeCell::new(ScalarSlew::default()),
                    }
                })
                .collect(),
        )
    }

    /// An empty gain slice restores unity to every output.
    pub(super) fn set(&self, gains: &[f32]) -> bool {
        if !gains.is_empty() && gains.len() != self.0.len() {
            return false;
        }
        for (index, channel) in self.0.iter().enumerate() {
            channel
                .fader
                .manual(gains.get(index).copied().unwrap_or(1.0));
        }
        true
    }

    /// The owning callback prepares a block. Returns whether any gain is moving.
    pub(super) unsafe fn prepare(&self, gains: &mut OutputGains, rate: u32, paused: bool) -> bool {
        let mut moving = false;
        for channel in &self.0 {
            let slew = unsafe { &mut *channel.slew.get() };
            gains[channel.output] = slew.begin_buffer(&channel.fader, rate, paused);
            moving |= slew.remaining() > 0;
        }
        moving
    }

    pub(super) unsafe fn advance(&self, gains: &mut OutputGains) -> bool {
        let mut moving = false;
        for channel in &self.0 {
            let slew = unsafe { &mut *channel.slew.get() };
            slew.advance(&mut gains[channel.output]);
            moving |= slew.remaining() > 0;
        }
        moving
    }

    pub(super) unsafe fn finish(&self, gains: &OutputGains) {
        for channel in &self.0 {
            let slew = unsafe { &mut *channel.slew.get() };
            slew.finish_buffer(&channel.fader, gains[channel.output]);
        }
    }
}
