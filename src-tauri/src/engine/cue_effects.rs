//! Portable inserts on file Audio cues. A voice owns its filter history.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::rate_fader::{ScalarFader, ScalarSlew};

const MAX_EFFECTS: usize = 8;
pub(crate) const MIN_FREQUENCY: f32 = 20.0;
pub(crate) const MAX_FREQUENCY: f32 = 20_000.0;
pub(crate) const EQ_GAIN_RANGE: std::ops::RangeInclusive<f32> = -12.0..=12.0;
const COEFFICIENT_SEGMENT_FRAMES: u32 = 16;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CueEffect {
    HighPass {
        #[serde(default = "Uuid::new_v4")]
        id: Uuid,
        enabled: bool,
        frequency_hz: f32,
    },
    LowPass {
        #[serde(default = "Uuid::new_v4")]
        id: Uuid,
        enabled: bool,
        frequency_hz: f32,
    },
    Peak {
        #[serde(default = "Uuid::new_v4")]
        id: Uuid,
        enabled: bool,
        frequency_hz: f32,
        gain_db: f32,
        q: f32,
    },
}

impl CueEffect {
    pub(crate) fn id(self) -> Uuid {
        match self {
            Self::HighPass { id, .. } | Self::LowPass { id, .. } | Self::Peak { id, .. } => id,
        }
    }

    fn enabled(self) -> bool {
        match self {
            Self::HighPass { enabled, .. }
            | Self::LowPass { enabled, .. }
            | Self::Peak { enabled, .. } => enabled,
        }
    }

    pub(crate) fn processes_audio(self) -> bool {
        self.enabled() && !matches!(self, Self::Peak { gain_db, .. } if gain_db == 0.0)
    }

    fn frequency(self) -> f32 {
        match self {
            Self::HighPass { frequency_hz, .. }
            | Self::LowPass { frequency_hz, .. }
            | Self::Peak { frequency_hz, .. } => frequency_hz,
        }
    }

    pub(crate) fn enabled_cutoff(self) -> Option<f32> {
        match self {
            Self::HighPass { enabled: true, frequency_hz, .. }
            | Self::LowPass { enabled: true, frequency_hz, .. } => Some(frequency_hz),
            _ => None,
        }
    }

    pub(crate) fn enabled_eq_gain(self) -> Option<f32> {
        match self {
            Self::Peak { enabled: true, gain_db, .. } => Some(gain_db),
            _ => None,
        }
    }

    fn parameter(self) -> f32 {
        match self {
            Self::Peak { gain_db, .. } => gain_db,
            filter => filter.frequency(),
        }
    }
}

pub(crate) fn validate_effects(effects: &[CueEffect]) -> Result<(), &'static str> {
    if effects.len() > MAX_EFFECTS {
        return Err("An effects chain supports up to eight inserts.");
    }
    for (index, effect) in effects.iter().enumerate() {
        if effects[..index].iter().any(|other| other.id() == effect.id()) {
            return Err("Effect IDs must be unique within the chain.");
        }
        if !(MIN_FREQUENCY..=MAX_FREQUENCY).contains(&effect.frequency()) {
            return Err("Effect frequency must be between 20 Hz and 20 kHz.");
        }
        if let CueEffect::Peak { gain_db, q, .. } = effect {
            if !EQ_GAIN_RANGE.contains(gain_db) || !(0.3..=6.0).contains(q) {
                return Err("Peaking EQ requires gain from −12 to +12 dB and Q from 0.3 to 6.");
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct History {
    z1: f64,
    z2: f64,
}

#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn new(effect: CueEffect, sample_rate: u32, parameter: f64) -> Self {
        // Retain the saved frequency but cap it below Nyquist on a device
        // whose negotiated rate is too low to represent it.
        let sample_rate = f64::from(sample_rate.max(1));
        let frequency_hz = match effect {
            CueEffect::Peak { frequency_hz, .. } => f64::from(frequency_hz),
            _ => parameter,
        };
        let frequency = frequency_hz.min(sample_rate * 0.49);
        let (sin, cos) = (std::f64::consts::TAU * frequency / sample_rate).sin_cos();
        let alpha = sin * std::f64::consts::FRAC_1_SQRT_2;
        let (b0, b1, b2, a0, a2) = match effect {
            CueEffect::HighPass { .. } => {
                let b = (1.0 + cos) / 2.0;
                (b, -2.0 * b, b, 1.0 + alpha, 1.0 - alpha)
            }
            CueEffect::LowPass { .. } => {
                let b = (1.0 - cos) / 2.0;
                (b, 2.0 * b, b, 1.0 + alpha, 1.0 - alpha)
            }
            CueEffect::Peak { q, .. } => {
                let alpha = sin / (2.0 * f64::from(q));
                let amplitude = 10.0_f64.powf(parameter / 40.0);
                (
                    1.0 + alpha * amplitude,
                    -2.0 * cos,
                    1.0 - alpha * amplitude,
                    1.0 + alpha / amplitude,
                    1.0 - alpha / amplitude,
                )
            }
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: -2.0 * cos / a0,
            a2: a2 / a0,
        }
    }

    fn step_to(self, next: Self, frames: u32) -> Self {
        let scale = 1.0 / f64::from(frames);
        Self {
            b0: (next.b0 - self.b0) * scale,
            b1: (next.b1 - self.b1) * scale,
            b2: (next.b2 - self.b2) * scale,
            a1: (next.a1 - self.a1) * scale,
            a2: (next.a2 - self.a2) * scale,
        }
    }

    fn step(&mut self, delta: Self) {
        self.b0 += delta.b0;
        self.b1 += delta.b1;
        self.b2 += delta.b2;
        self.a1 += delta.a1;
        self.a2 += delta.a2;
    }

    fn process(&self, input: f64, state: &mut History) -> f64 {
        let output = self.b0 * input + state.z1;
        state.z1 = self.b1 * input - self.a1 * output + state.z2;
        state.z2 = self.b2 * input - self.a2 * output;
        // Flush decaying state well below the audible range before it can
        // become subnormal on a callback thread without FTZ enabled.
        if state.z1.abs() < 1e-24 {
            state.z1 = 0.0;
        }
        if state.z2.abs() < 1e-24 {
            state.z2 = 0.0;
        }
        output
    }
}

struct EffectStage {
    effect: CueEffect,
    coefficients: Biquad,
    history: Box<[History]>,
    automation: Option<ParameterAutomation>,
}

struct ParameterAutomation {
    fader: Arc<ScalarFader>,
    slew: ScalarSlew,
    value: f64,
    segment_remaining: u32,
    delta: Biquad,
}

impl ParameterAutomation {
    fn new(value: f32, render_anchored: bool) -> Self {
        let fader = Arc::new(if render_anchored { ScalarFader::render_anchored() } else { ScalarFader::default() });
        fader.initialize(value);
        Self {
            fader,
            slew: ScalarSlew::default(),
            value: value.into(),
            segment_remaining: 0,
            delta: Biquad::default(),
        }
    }

    fn advance(&mut self, coefficients: &mut Biquad, effect: CueEffect, sample_rate: u32) {
        let remaining = self.slew.remaining();
        if remaining == 0 {
            return;
        }
        if self.segment_remaining == 0 {
            // Compute an anchor for up to sixteen frames. Linear denominator
            // interpolation between these stable biquads remains in the
            // second-order stability region.
            let span = remaining.min(COEFFICIENT_SEGMENT_FRAMES);
            let next = self.slew.projected(self.value, span);
            self.delta = coefficients.step_to(Biquad::new(effect, sample_rate, next), span);
            self.segment_remaining = span;
        }
        coefficients.step(self.delta);
        self.slew.advance(&mut self.value);
        self.segment_remaining -= 1;
    }
}

pub(super) struct CueEffects {
    stages: Box<[EffectStage]>,
    sample_rate: u32,
    automating: bool,
}

impl CueEffects {
    /// Prepare live controls before a file voice is submitted. An enabled
    /// neutral EQ can later leave unity without replacing the voice.
    pub(super) fn for_voice(
        effects: &[CueEffect],
        channels: usize,
        sample_rate: u32,
    ) -> Option<Self> {
        Self::build(effects, channels, sample_rate, true)
    }

    /// Prepare a saved chain without live parameter controls.
    pub(super) fn for_static_chain(
        effects: &[CueEffect],
        channels: usize,
        sample_rate: u32,
    ) -> Option<Self> {
        Self::build(effects, channels, sample_rate, false)
    }

    fn build(effects: &[CueEffect], channels: usize, sample_rate: u32, live: bool) -> Option<Self> {
        let stages: Box<[_]> = effects
            .iter()
            .copied()
            .filter(|effect| if live { effect.enabled() } else { effect.processes_audio() })
            .map(|effect| EffectStage {
                effect,
                coefficients: Biquad::new(effect, sample_rate, effect.parameter().into()),
                history: (0..channels).map(|_| History::default()).collect(),
                automation: live.then(|| ParameterAutomation::new(effect.parameter(), effect.enabled_eq_gain().is_some())),
            })
            .collect();
        (!stages.is_empty()).then_some(Self {
            stages,
            sample_rate,
            automating: false,
        })
    }

    pub(super) fn cutoff_controls(&self) -> impl Iterator<Item = (Uuid, Arc<ScalarFader>)> + '_ {
        self.parameter_controls(|effect| effect.enabled_cutoff().is_some())
    }

    pub(super) fn eq_gain_controls(&self) -> impl Iterator<Item = (Uuid, Arc<ScalarFader>)> + '_ {
        self.parameter_controls(|effect| effect.enabled_eq_gain().is_some())
    }

    fn parameter_controls(&self, matches: fn(CueEffect) -> bool) -> impl Iterator<Item = (Uuid, Arc<ScalarFader>)> + '_ {
        self.stages.iter().filter(move |stage| matches(stage.effect)).filter_map(|stage| {
            Some((stage.effect.id(), Arc::clone(&stage.automation.as_ref()?.fader)))
        })
    }

    /// Adapt to the actual output clock before processing. State is prepared
    /// once per rate change without allocating or dropping the history buffers.
    pub(super) fn prepare(&mut self, sample_rate: u32) {
        if self.sample_rate != sample_rate {
            self.sample_rate = sample_rate;
            for stage in &mut self.stages {
                let value = stage.automation.as_ref().map_or(
                    f64::from(stage.effect.parameter()), |automation| automation.value,
                );
                stage.coefficients = Biquad::new(stage.effect, sample_rate, value);
            }
            self.reset();
        }
        self.automating = false;
        for stage in self.stages.iter_mut().filter_map(|stage| stage.automation.as_mut()) {
            stage.value = stage.slew.begin_buffer(&stage.fader, sample_rate, false);
            stage.segment_remaining = 0;
            self.automating |= stage.slew.remaining() > 0;
        }
    }

    pub(super) fn finish_buffer(&mut self) {
        if self.automating {
            for stage in self.stages.iter_mut().filter_map(|stage| stage.automation.as_mut()) {
                stage.slew.finish_buffer(&stage.fader, stage.value);
            }
        }
    }

    pub(super) fn reset(&mut self) {
        for stage in &mut self.stages {
            for history in &mut stage.history {
                *history = History::default();
            }
        }
    }

    pub(super) fn process(&mut self, frame: &mut [f32]) {
        for (channel, sample) in frame.iter_mut().enumerate() {
            let mut value = if sample.is_finite() {
                f64::from(*sample)
            } else {
                0.0
            };
            for stage in &mut self.stages {
                value = stage
                    .coefficients
                    .process(value, &mut stage.history[channel]);
            }
            let output = value as f32;
            *sample = if output.is_finite() { output } else { 0.0 };
        }
        if self.automating {
            for stage in &mut self.stages {
                if let Some(automation) = &mut stage.automation {
                    automation.advance(&mut stage.coefficients, stage.effect, self.sample_rate);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn low_pass(frequency_hz: f32) -> CueEffect {
        CueEffect::LowPass {
            id: Uuid::new_v4(),
            enabled: true,
            frequency_hz,
        }
    }

    fn tone_rms(effect: CueEffect, frequency: f64, rate: u32) -> f64 {
        // Deliberately construct with a different sample rate. The test uses
        // the negotiated output clock, just as a resampled cue would.
        let mut chain = CueEffects::for_voice(&[effect], 1, 44_100).unwrap();
        chain.prepare(rate);
        let count = rate / 10;
        let mut sum = 0.0;
        for index in 0..2 * count {
            let theta = std::f64::consts::TAU * f64::from(index) * frequency / f64::from(rate);
            let mut frame = [theta.sin() as f32];
            chain.process(&mut frame);
            if index >= count {
                sum += f64::from(frame[0]).powi(2);
            }
        }
        (sum / f64::from(count)).sqrt()
    }

    #[test]
    fn filters_and_peaking_eq_follow_their_specified_frequencies() {
        let lpf = low_pass(1_000.0);
        let hpf = CueEffect::HighPass {
            id: Uuid::new_v4(),
            enabled: true,
            frequency_hz: 1_000.0,
        };
        let peak = CueEffect::Peak {
            id: Uuid::new_v4(),
            enabled: true,
            frequency_hz: 1_000.0,
            gain_db: -6.0,
            q: 2.0,
        };
        for sample_rate in [32_000, 48_000, 96_000] {
            assert!(tone_rms(lpf, 100.0, sample_rate) > 0.70);
            assert!(tone_rms(lpf, 8_000.0, sample_rate) < 0.014);
            assert!(tone_rms(hpf, 100.0, sample_rate) < 0.01);
            assert!(tone_rms(hpf, 8_000.0, sample_rate) > 0.70);
            assert!((tone_rms(peak, 1_000.0, sample_rate) - 0.354).abs() < 0.002);
        }
    }

    #[test]
    fn histories_are_isolated_and_can_be_reset() {
        let mut chain = CueEffects::for_voice(&[low_pass(500.0)], 3, 48_000).unwrap();
        let mut impulse = [0.0, 0.0, 1.0];
        chain.process(&mut impulse);
        assert_eq!(impulse[..2], [0.0, 0.0]);
        let mut next_frame = [0.0; 3];
        chain.process(&mut next_frame);
        assert!(next_frame[2] > 0.0);
        chain.reset();
        let mut silence = [0.0; 3];
        chain.process(&mut silence);
        assert_eq!(silence, [0.0; 3]);
    }

    #[test]
    fn active_cutoff_fades_hold_and_the_latest_command_owns_the_filter() {
        use std::time::Duration;

        let filter = low_pass(600.0);
        let mut chain = CueEffects::for_voice(&[filter], 1, 48_000).unwrap();
        let (id, control) = chain.cutoff_controls().next().unwrap();
        assert_eq!(id, filter.id());
        let render = |chain: &mut CueEffects, frames| {
            chain.prepare(48_000);
            for _ in 0..frames {
                chain.process(&mut [0.0]);
            }
            chain.finish_buffer();
        };
        let earlier = control.begin(1_000.0);
        earlier.update(1.0, 1.0, Duration::from_millis(16));
        render(&mut chain, 192);
        assert_eq!(control.current(), 700.0);

        earlier.hold();
        render(&mut chain, 192);
        assert_eq!(control.current(), 700.0);
        earlier.update(0.75, 0.0, Duration::from_millis(16));
        render(&mut chain, 768);
        assert_eq!(control.current(), 900.0);

        let newer = control.begin(100.0);
        newer.update(0.0, 1.0, Duration::from_millis(16));
        earlier.finish();
        render(&mut chain, 768);
        assert_eq!(control.current(), 100.0);

        control.manual(1_200.0);
        newer.finish();
        render(&mut chain, 300);
        assert_eq!(control.current(), 1_200.0);

        let stopped = control.begin(600.0);
        stopped.update(0.0, 1.0, Duration::from_millis(16));
        render(&mut chain, 192);
        drop(stopped);
        render(&mut chain, 768);
        assert_eq!(control.current(), 1_050.0);
    }

    #[test]
    fn a_cutoff_fade_changes_pcm_and_a_new_voice_uses_the_saved_filter() {
        let effect = CueEffect::HighPass {
            id: Uuid::new_v4(),
            enabled: true,
            frequency_hz: 600.0,
        };
        let mut chain = CueEffects::for_voice(&[effect], 2, 48_000).unwrap();
        let (_, control) = chain.cutoff_controls().next().unwrap();
        let measure = |chain: &mut CueEffects| {
            let mut sum = 0.0;
            for block in 0..32 {
                chain.prepare(48_000);
                for frame in 0..256 {
                    let time = f64::from(block * 256 + frame) / 48_000.0;
                    let mut source = [(time * 180.0 * std::f64::consts::TAU).sin() as f32, 0.0];
                    chain.process(&mut source);
                    assert_eq!(source[1], 0.0, "filter history stays isolated");
                    assert!(source[0].is_finite());
                    if block >= 16 {
                        sum += f64::from(source[0]).powi(2);
                    }
                }
                chain.finish_buffer();
            }
            (sum / 4096.0).sqrt()
        };
        let filtered = measure(&mut chain);
        assert!((0.06..0.07).contains(&filtered));
        control.begin(80.0).finish();
        assert!(measure(&mut chain) > 0.68);
        let mut new_voice = CueEffects::for_voice(&[effect], 2, 48_000).unwrap();
        assert_eq!(new_voice.cutoff_controls().next().unwrap().1.current(), 600.0);
        assert_eq!(measure(&mut new_voice), filtered);
    }

    #[test]
    fn peaking_eq_gain_fades_from_unity_and_releases_to_manual_edits() {
        use std::time::Duration;

        let effect = CueEffect::Peak {
            id: Uuid::new_v4(), enabled: true, frequency_hz: 1_000.0, gain_db: 0.0, q: 1.0,
        };
        assert!(CueEffects::for_static_chain(&[effect], 2, 48_000).is_none());
        let mut chain = CueEffects::for_voice(&[effect], 2, 48_000).unwrap();
        assert!(chain.cutoff_controls().next().is_none());
        let (id, control) = chain.eq_gain_controls().next().unwrap();
        assert_eq!((id, control.current()), (effect.id(), 0.0));

        let render = |chain: &mut CueEffects, frames: u32| {
            chain.prepare(48_000);
            let mut sum = 0.0;
            for index in 0..frames {
                let phase = f64::from(index) * std::f64::consts::TAU / 48.0;
                let mut frame = [phase.sin() as f32, 0.0];
                chain.process(&mut frame);
                assert_eq!(frame[1], 0.0);
                if index >= frames / 2 {
                    sum += f64::from(frame[0]).powi(2);
                }
            }
            chain.finish_buffer();
            (sum / f64::from(frames / 2)).sqrt()
        };
        assert!((render(&mut chain, 9600) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        let fade = control.begin(6.0);
        fade.update(1.0, 1.0, Duration::from_millis(16));
        render(&mut chain, 192);
        assert_eq!(control.current(), 1.5);
        fade.hold();
        render(&mut chain, 192);
        assert_eq!(control.current(), 1.5);
        fade.finish();
        assert!((render(&mut chain, 9600) - 1.4109).abs() < 0.002);

        let obsolete = control.begin(12.0);
        control.manual(-6.0);
        obsolete.finish();
        assert!((render(&mut chain, 9600) - 0.3544).abs() < 0.002);
        assert_eq!(control.current(), -6.0);

        let stopped = control.begin(-12.0);
        stopped.update(0.0, 1.0, Duration::from_millis(16));
        render(&mut chain, 192);
        drop(stopped);
        render(&mut chain, 9600);
        assert_eq!(control.current(), -7.5);

        let fresh = CueEffects::for_voice(&[effect], 2, 48_000).unwrap();
        assert_eq!(fresh.eq_gain_controls().next().unwrap().1.current(), 0.0);
    }

    #[test]
    fn limits_apply_to_disabled_inserts_and_bypass_needs_no_processor() {
        let bypassed = CueEffect::LowPass {
            id: Uuid::new_v4(),
            enabled: false,
            frequency_hz: 500.0,
        };
        assert!(CueEffects::for_voice(&[bypassed], 2, 48_000).is_none());
        assert!(validate_effects(&[bypassed; MAX_EFFECTS + 1]).is_err());
        assert!(validate_effects(&[low_pass(f32::NAN)]).is_err());
        assert!(validate_effects(&[low_pass(20_001.0)]).is_err());
        assert!(validate_effects(&[CueEffect::Peak {
            id: Uuid::new_v4(),
            enabled: false,
            frequency_hz: 1_000.0,
            gain_db: 12.0,
            q: 0.0,
        }])
        .is_err());
    }
}
