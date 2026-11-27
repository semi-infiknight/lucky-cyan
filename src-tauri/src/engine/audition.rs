//! Private stereo clip monitor. It owns its player and never submits a voice to show transport.
use std::io::{self, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use super::cue_effects::{CueEffect, CueEffects};

pub struct AuditionClip {
    pub samples: Arc<Vec<f32>>,
    pub channels: usize,
    pub sample_rate: u32,
    pub start_ms: u64,
    pub end_ms: Option<u64>,
    pub gain: f32,
    pub pan: f32,
}

fn named_node(id: &str) -> Option<&str> {
    let node = id.strip_prefix("pw:")?;
    if node.is_empty()
        || node.chars().any(char::is_control)
        || node.trim() != node
        || node.chars().all(|ch| ch.is_ascii_digit())
        || matches!(
            node,
            "auto"
                | "default"
                | "output_default"
                | "sink_default"
                | "input_default"
                | "@DEFAULT_AUDIO_SINK@"
                | "@DEFAULT_SINK@"
                | "-1"
        )
    {
        None
    } else {
        Some(node)
    }
}

pub fn dedicated_node<'a, 'b>(
    device: &'a str,
    main: Option<&str>,
    patches: impl IntoIterator<Item = &'b str>,
) -> Result<&'a str, String> {
    let target = named_node(device).ok_or("Choose a named PipeWire headphone output.")?;
    let main = main
        .and_then(named_node)
        .ok_or("Select a named PipeWire show output in Audio Preferences before auditioning.")?;
    if main == target {
        return Err(
            "This output carries the live show. Choose a dedicated headphone output.".into(),
        );
    }
    for id in patches {
        // An empty patch id explicitly uses the engine’s main stream.
        if id.is_empty() {
            continue;
        }
        let node = named_node(id).ok_or("A show patch uses an unidentified output. Assign it a named PipeWire output before auditioning.")?;
        if node == target {
            return Err(
                "A show output patch uses this device. Choose a dedicated headphone output.".into(),
            );
        }
    }
    Ok(target)
}

impl AuditionClip {
    fn frame_range(&self) -> Result<std::ops::Range<usize>, String> {
        if self.channels == 0
            || self.sample_rate == 0
            || !self.gain.is_finite()
            || !self.pan.is_finite()
        {
            return Err("Invalid decoded audio or cue level.".into());
        }
        let total = self.samples.len() / self.channels;
        let frame = |ms: u64| {
            ((u128::from(ms) * u128::from(self.sample_rate) / 1000).min(total as u128)) as usize
        };
        let start = frame(self.start_ms);
        let end = self.end_ms.map(frame).unwrap_or(total);
        if start >= end {
            return Err("The audition trim contains no audio.".into());
        }
        Ok(start..end)
    }

    fn write_stereo(
        &self,
        frames: std::ops::Range<usize>,
        effects: &[CueEffect],
        mut writer: impl Write,
    ) -> io::Result<()> {
        let pan = self.pan.clamp(-1.0, 1.0);
        let left = ((1.0 - pan) * 0.5).sqrt() * self.gain;
        let right = ((1.0 + pan) * 0.5).sqrt() * self.gain;
        let monitor_channels = self.channels.min(2);
        let right_channel = monitor_channels - 1;
        let mut effects = CueEffects::for_static_chain(effects, monitor_channels, self.sample_rate);
        let mut buffer = Vec::with_capacity(8192);
        for frame in self.samples[frames.start * self.channels..frames.end * self.channels]
            .chunks_exact(self.channels)
        {
            let mut monitored = [frame[0], frame[right_channel]];
            if let Some(effects) = &mut effects {
                effects.process(&mut monitored[..monitor_channels]);
            }
            buffer.extend_from_slice(&(monitored[0] * left).clamp(-1.0, 1.0).to_ne_bytes());
            buffer.extend_from_slice(&(monitored[right_channel] * right).clamp(-1.0, 1.0).to_ne_bytes());
            if buffer.len() == 8192 {
                writer.write_all(&buffer)?;
                buffer.clear();
            }
        }
        writer.write_all(&buffer)
    }
}

pub struct Audition {
    child: Child,
}

/// Render each clip with its own trim, gain, pan and effects to stereo, resample
/// to a common rate and sum them into one monitor clip. Used to audition a
/// Group's playable children together, the way QLab previews a group.
pub(crate) fn mix_clips(clips: &[(AuditionClip, Vec<CueEffect>)]) -> Result<AuditionClip, String> {
    let Some((first, _)) = clips.first() else {
        return Err("Nothing to audition.".into());
    };
    let rate = clips.iter().map(|(clip, _)| clip.sample_rate).max().unwrap_or(0);
    if rate == 0 || first.channels == 0 {
        return Err("Invalid decoded audio.".into());
    }
    let mut mixed: Vec<f32> = Vec::new();
    for (clip, effects) in clips {
        let frames = clip.frame_range()?;
        let mut rendered = Vec::new();
        clip.write_stereo(frames, effects, &mut rendered).map_err(|err| err.to_string())?;
        let (stereo_bytes, _) = rendered.as_chunks::<4>();
        let stereo: Vec<f32> = stereo_bytes.iter().map(|bytes| f32::from_ne_bytes(*bytes)).collect();
        let resampled = resample_stereo(&stereo, clip.sample_rate, rate);
        // Keep each clip's absolute start offset so trims stay in place.
        let offset = ((u128::from(clip.start_ms) * u128::from(rate) / 1000) as usize) * 2;
        let end = offset + resampled.len();
        if mixed.len() < end {
            mixed.resize(end, 0.0);
        }
        for (dst, src) in mixed[offset..].iter_mut().zip(resampled.iter()) {
            *dst += src;
        }
    }
    Ok(AuditionClip {
        samples: Arc::new(mixed),
        channels: 2,
        sample_rate: rate,
        start_ms: 0,
        end_ms: None,
        // write_stereo applies the constant-power pan law (sqrt(0.5) per
        // channel at center); compensate so the summed mix renders at unity.
        gain: 2f32.sqrt(),
        pan: 0.0,
    })
}

/// Linear-interpolation resample of an interleaved stereo buffer. Monitor-only
/// and offline, so a simple interpolator is enough.
fn resample_stereo(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || from == 0 || to == 0 || input.len() < 4 {
        return input.to_vec();
    }
    let frames = input.len() / 2;
    let out_frames = (((frames as u64) * u64::from(to)) / u64::from(from)).max(1) as usize;
    let ratio = f64::from(from) / f64::from(to);
    let mut out = Vec::with_capacity(out_frames * 2);
    for index in 0..out_frames {
        let position = index as f64 * ratio;
        let base = (position as usize).min(frames - 1);
        let fraction = (position - base as f64) as f32;
        let next = (base + 1).min(frames - 1);
        out.push(input[base * 2] * (1.0 - fraction) + input[next * 2] * fraction);
        out.push(input[base * 2 + 1] * (1.0 - fraction) + input[next * 2 + 1] * fraction);
    }
    out
}

impl Audition {
    pub fn start(node: &str, clip: AuditionClip) -> Result<Self, String> {
        Self::start_with_effects(node, clip, Vec::new())
    }

    pub(super) fn start_with_effects(node: &str, clip: AuditionClip, effects: Vec<CueEffect>) -> Result<Self, String> {
        let frames = clip.frame_range()?;
        // WirePlumber must never select a replacement/default device on startup or unplug.
        let mut child = Command::new("pw-play")
            .args(["--target", node, "-P",
                "{ node.name=inkue-audition node.dont-fallback=true node.dont-reconnect=true node.dont-move=true }",
                "--raw", "--format", "f32", "--rate", &clip.sample_rate.to_string(),
                "--channels", "2", "--channel-map", "stereo", "-"])
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().map_err(|err| format!("Cannot launch the PipeWire player. Install pw-play and retry. {err}"))?;
        let stdin = child.stdin.take().expect("requested piped stdin");
        let playback = Self { child };
        std::thread::Builder::new()
            .name("audition-pcm".into())
            .spawn(move || {
                // Closing stdin drains the final frames. Stopping the child closes the pipe.
                let _ = clip.write_stereo(frames, &effects, stdin);
            })
            .map_err(|err| format!("Could not start the headphone feed: {err}"))?;
        Ok(playback)
    }

    pub fn running(&mut self) -> Result<bool, String> {
        match self.child.try_wait().map_err(|err| err.to_string())? {
            None => Ok(true),
            Some(status) if status.success() => Ok(false),
            Some(_) => Err(
                "The audition output closed or rejected playback. Check the headphone device."
                    .into(),
            ),
        }
    }
}

impl Drop for Audition {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_requires_separate_identifiable_outputs() {
        assert_eq!(
            dedicated_node("pw:headphones", Some("pw:house"), ["", "pw:stalls"]).unwrap(),
            "headphones"
        );
        for target in [
            "pw:house",
            "pw:stalls",
            "alsa:headphones",
            "default",
            "pw:0",
            "pw:auto",
            "pw:output_default",
            "pw:@DEFAULT_AUDIO_SINK@",
        ] {
            assert!(dedicated_node(target, Some("pw:house"), ["pw:stalls"]).is_err());
        }
        assert!(dedicated_node("pw:headphones", None, []).is_err());
        assert!(dedicated_node("pw:headphones", Some("default"), []).is_err());
        assert!(dedicated_node("pw:headphones", Some("pw:house"), ["default"]).is_err());
        assert!(dedicated_node("pw:headphones", Some("pw:house"), ["plughw:1,0"]).is_err());
    }

    fn clip() -> AuditionClip {
        AuditionClip {
            samples: Arc::new(vec![0.1, 0.2, 0.9, 0.3, 0.4, 0.8, 0.5, 0.6, 0.7]),
            channels: 3,
            sample_rate: 1000,
            start_ms: 1,
            end_ms: Some(2),
            gain: 0.5,
            pan: -1.0,
        }
    }

    fn high_pass(enabled: bool, frequency_hz: f32) -> CueEffect {
        CueEffect::HighPass { id: uuid::Uuid::new_v4(), enabled, frequency_hz }
    }

    fn rendered(clip: &AuditionClip, effects: &[CueEffect]) -> Vec<f32> {
        let mut output = Vec::new();
        clip.write_stereo(clip.frame_range().unwrap(), effects, &mut output)
            .unwrap();
        output
            .chunks_exact(4)
            .map(|b| f32::from_ne_bytes(b.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn audition_uses_enabled_fx_on_monitored_channels_before_stereo_pan() {
        let sample_rate = 48_000;
        let tone = |frequency: f32, index: u32| {
            0.25 * (std::f32::consts::TAU * frequency * index as f32 / sample_rate as f32).sin()
        };
        let samples = (0..sample_rate).flat_map(|index| {
            [tone(150.0, index), tone(4_000.0, index), 1.0]
        }).collect();
        let clip = AuditionClip {
            samples: Arc::new(samples), channels: 3, sample_rate,
            start_ms: 100, end_ms: Some(700), gain: 0.5, pan: 0.0,
        };
        let dry = rendered(&clip, &[]);
        assert_eq!(rendered(&clip, &[high_pass(false, 1_000.0)]), dry);
        let filter = high_pass(true, 1_000.0);
        let wet = rendered(&clip, &[filter]);
        assert_eq!(wet.len(), 2 * 28_800);
        let rms = |samples: &[f32], channel: usize| {
            let selected = samples[4800 + channel..].iter().step_by(2);
            (selected.clone().map(|sample| sample.powi(2)).sum::<f32>() / selected.count() as f32).sqrt()
        };
        assert!(rms(&wet, 0) < 0.04 * rms(&dry, 0));
        assert!((rms(&wet, 1) / rms(&dry, 1) - 1.0).abs() < 0.01);
        assert_eq!(rendered(&clip, &[filter]), wet, "each pass starts with fresh filter history");
    }

    #[test]
    fn trims_source_frames_and_applies_monitor_gain_and_pan() {
        assert_eq!(rendered(&clip(), &[]), vec![0.15, 0.0]);
        let mut c = clip();
        c.pan = 1.0;
        assert_eq!(rendered(&c, &[]), vec![0.0, 0.2]);
        c.channels = 1;
        assert_eq!(rendered(&c, &[]), vec![0.0, 0.1]);
    }

    #[test]
    fn clamps_end_and_rejects_empty_trim_or_invalid_format() {
        let mut c = clip();
        c.end_ms = Some(u64::MAX);
        assert_eq!(c.frame_range().unwrap(), 1..3);
        c.start_ms = 3;
        assert!(c.frame_range().is_err());
        c.start_ms = 0;
        c.channels = 0;
        assert!(c.frame_range().is_err());
    }

    fn mono_clip(samples: Vec<f32>, sample_rate: u32, gain: f32) -> AuditionClip {
        AuditionClip {
            samples: Arc::new(samples), channels: 1, sample_rate,
            start_ms: 0, end_ms: None, gain, pan: 0.0,
        }
    }

    fn rendered_mix(clips: &[(AuditionClip, Vec<CueEffect>)]) -> Vec<f32> {
        let mixed = mix_clips(clips).unwrap();
        assert_eq!(mixed.channels, 2);
        assert!((mixed.gain - 2f32.sqrt()).abs() < 1e-6);
        // The mix is already interleaved stereo at unity; read it directly so
        // comparisons against per-clip write_stereo output stay consistent.
        (*mixed.samples).clone()
    }

    #[test]
    fn mix_clips_sums_stereo_clips_at_a_common_rate() {
        // Two 1 kHz mono clips sum sample-for-sample (each keeps its own pan law).
        let a = mono_clip(vec![0.5; 10], 1_000, 1.0);
        let b = mono_clip(vec![0.25; 10], 1_000, 1.0);
        let ra = rendered(&a, &[]);
        let rb = rendered(&b, &[]);
        let mixed = rendered_mix(&[(a, vec![]), (b, vec![])]);
        assert_eq!(mixed.len(), ra.len());
        for (m, (x, y)) in mixed.iter().zip(ra.iter().zip(&rb)) {
            assert!((m - (x + y)).abs() < 1e-6, "{m} != {x} + {y}");
        }

        // A 1 kHz clip is resampled up to the common 2 kHz rate before summing.
        let fast = mono_clip(vec![0.5; 20], 2_000, 1.0);
        let slow = mono_clip(vec![0.25; 10], 1_000, 1.0);
        let rf = resample_stereo(&rendered(&fast, &[]), 2_000, 2_000);
        let rs = resample_stereo(&rendered(&slow, &[]), 1_000, 2_000);
        let mixed = rendered_mix(&[(fast, vec![]), (slow, vec![])]);
        assert_eq!(mixed.len(), rf.len().max(rs.len()));
        for (m, (x, y)) in mixed.iter().zip(rf.iter().zip(rs.iter())) {
            assert!((m - (x + y)).abs() < 1e-6);
        }

        // Per-clip gain applies before the sum; an empty mix is an error.
        let quiet = mono_clip(vec![0.5; 10], 1_000, 0.5);
        let loud = mono_clip(vec![0.25; 10], 1_000, 1.0);
        let rq = rendered(&quiet, &[]);
        let rl = rendered(&loud, &[]);
        let mixed = rendered_mix(&[(quiet, vec![]), (loud, vec![])]);
        for (m, (x, y)) in mixed.iter().zip(rq.iter().zip(&rl)) {
            assert!((m - (x + y)).abs() < 1e-6);
        }
        assert!(mix_clips(&[]).is_err());
    }

    #[test]
    fn mix_clips_applies_each_clips_trim_and_effects() {
        let sample_rate = 48_000;
        let tone = |frequency: f32, frames: usize| {
            (0..frames).map(|n| 0.25 * (std::f32::consts::TAU * frequency * n as f32 / sample_rate as f32).sin()).collect::<Vec<_>>()
        };
        // A trimmed clip contributes only its 75–100 ms window on top of a full clip.
        let full = mono_clip(tone(150.0, 4_800), sample_rate, 1.0);
        let mut trimmed = mono_clip(tone(150.0, 4_800), sample_rate, 1.0);
        trimmed.start_ms = 75;
        trimmed.end_ms = Some(100);
        let rf = rendered(&full, &[]);
        let rt = rendered(&trimmed, &[]);
        let mixed = rendered_mix(&[(full, vec![]), (trimmed, vec![])]);
        assert_eq!(mixed.len(), 2 * 4_800);
        for (m, x) in mixed[..2 * 3_600].iter().zip(&rf) {
            assert!((m - x).abs() < 1e-6, "before the trim window only the full clip plays");
        }
        for (m, (x, y)) in mixed[2 * 3_600..].iter().zip(rf[2 * 3_600..].iter().zip(rt.iter())) {
            assert!((m - (x + y)).abs() < 1e-6, "inside the window both clips play");
        }

        // Each clip keeps its own effect chain: a high-passed 150 Hz tone is gone.
        let dry = rendered_mix(&[(mono_clip(tone(150.0, 4_800), sample_rate, 1.0), vec![])]);
        let wet = rendered_mix(&[(mono_clip(tone(150.0, 4_800), sample_rate, 1.0), vec![high_pass(true, 1_000.0)])]);
        let rms = |samples: &[f32]| {
            (samples[2 * 4_000..].iter().map(|s| s * s).sum::<f32>() / (samples.len() - 2 * 4_000) as f32).sqrt()
        };
        assert!(rms(&wet) < 0.05 * rms(&dry));
    }

    #[test]
    #[ignore = "run scripts/rehearsal/check-audition-routing.py with PipeWire and WirePlumber installed"]
    fn isolated_graph_tone() {
        assert_eq!(
            std::env::var("INKUE_AUDITION_GRAPH_TEST").as_deref(),
            Ok("1")
        );
        let sample_rate = 44_100;
        let samples = (0..sample_rate * 3)
            .map(|n| {
                let phase = std::f32::consts::TAU * n as f32 / sample_rate as f32;
                ((440.0 * phase).sin() + (3_000.0 * phase).sin()) * 0.1
            })
            .collect();
        let effects = if std::env::var("INKUE_AUDITION_GRAPH_SIGNAL").as_deref() == Ok("effects") {
            vec![high_pass(true, 1_600.0)]
        } else {
            Vec::new()
        };
        let clip = AuditionClip {
            samples: Arc::new(samples),
            channels: 1,
            sample_rate,
            start_ms: 0,
            end_ms: None,
            gain: 1.0,
            pan: 0.0,
        };
        let mut playing = Audition::start_with_effects("inkue-test-headphones", clip, effects).unwrap();
        let start = std::time::Instant::now();
        while playing.running().unwrap_or(false) {
            assert!(
                start.elapsed().as_secs() < 8,
                "audition child did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
