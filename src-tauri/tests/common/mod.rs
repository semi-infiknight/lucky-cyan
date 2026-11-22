//! Shared helpers for Inkue integration tests.
//!
//! This module is dev-only test infrastructure. It touches no production code
//! and pulls in no dependency beyond what `inkue_lib` already links.
//!
//! Provided helpers:
//! - dependency-free WAV fixture generation (PCM16 + IEEE float32),
//! - calibrated signal generators (sine, silence),
//! - a fully-populated [`CueRegistry`] mirroring the app's startup registration,
//! - temp-dir management and a decode-with-timeout guard.

#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver};
use ringbuf::traits::Split;
use ringbuf::HeapRb;
use uuid::Uuid;

use inkue_lib::cue::context::{CueContext, CueEvent};
use inkue_lib::cue::registry::CueRegistry;
use inkue_lib::cue::types::CueType;
use inkue_lib::engine::dmx_engine::{ChannelWidth, DmxChannelLevel};
use inkue_lib::engine::engine_traits::{AudioEngineApi, DmxEngineApi, OutputEngineApi};
use inkue_lib::engine::output_engine::{
    ContentRequest, CrossfadePhase, CrossfadeRequest, SilentSpeedOrigin, VideoGeometry,
    VideoGeometryUpdate,
};
use inkue_lib::engine::ring_command::{FadeCurve, VoiceId};
use inkue_lib::engine::voice::Voice;

// ---------------------------------------------------------------------------
// Temp directories
// ---------------------------------------------------------------------------

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a fresh, unique temp directory for generated fixtures.
/// Names are unique across threads and runs (nanos + atomic counter).
pub fn temp_dir(tag: &str) -> PathBuf {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("inkue_test_{tag}_{nanos}_{n}"));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Directory where the user drops real compressed audio fixtures
/// (`.flac` / `.mp3` / `.ogg` / `.m4a`). Committed to the repo with a README.
pub fn fixtures_audio_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("audio")
}

/// First fixture file with the given extension, if the user provided one.
pub fn find_fixture(ext: &str) -> Option<PathBuf> {
    let dir = fixtures_audio_dir();
    let entries = fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case(ext)) == Some(true) {
            return Some(p);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Signal generators
// ---------------------------------------------------------------------------

/// Interleaved sine wave, amplitude 0.5, `channels` identical channels.
pub fn sine(freq: f64, secs: f64, sample_rate: u32, channels: u16) -> Vec<f32> {
    let frames = (secs * sample_rate as f64) as usize;
    let mut out = Vec::with_capacity(frames * channels as usize);
    for i in 0..frames {
        let t = i as f64 / sample_rate as f64;
        let s = (2.0 * std::f64::consts::PI * freq * t).sin() as f32 * 0.5;
        for _ in 0..channels {
            out.push(s);
        }
    }
    out
}

/// Interleaved silence of exactly `frames` frames.
pub fn silence(frames: usize, channels: u16) -> Vec<f32> {
    vec![0.0f32; frames * channels as usize]
}

// ---------------------------------------------------------------------------
// WAV writers (dependency-free)
// ---------------------------------------------------------------------------

/// Write interleaved f32 samples in [-1, 1] as a 16-bit PCM WAV file.
pub fn write_wav_pcm16(path: &Path, samples: &[f32], channels: u16, sample_rate: u32) {
    let bits: u16 = 16;
    let block_align: u16 = channels * bits / 8;
    let byte_rate: u32 = sample_rate * block_align as u32;
    let data_len: u32 = (samples.len() * 2) as u32;

    let mut f = fs::File::create(path).expect("create wav");
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
    f.write_all(b"WAVE").unwrap();
    f.write_all(b"fmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap(); // fmt chunk size
    f.write_all(&1u16.to_le_bytes()).unwrap(); // audio format: PCM
    f.write_all(&channels.to_le_bytes()).unwrap();
    f.write_all(&sample_rate.to_le_bytes()).unwrap();
    f.write_all(&byte_rate.to_le_bytes()).unwrap();
    f.write_all(&block_align.to_le_bytes()).unwrap();
    f.write_all(&bits.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data_len.to_le_bytes()).unwrap();
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        f.write_all(&v.to_le_bytes()).unwrap();
    }
}

/// Write interleaved f32 samples as an IEEE-float (format 3) WAV file.
pub fn write_wav_float32(path: &Path, samples: &[f32], channels: u16, sample_rate: u32) {
    let bits: u16 = 32;
    let block_align: u16 = channels * bits / 8;
    let byte_rate: u32 = sample_rate * block_align as u32;
    let data_len: u32 = (samples.len() * 4) as u32;

    let mut f = fs::File::create(path).expect("create wav");
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
    f.write_all(b"WAVE").unwrap();
    f.write_all(b"fmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&3u16.to_le_bytes()).unwrap(); // audio format: IEEE float
    f.write_all(&channels.to_le_bytes()).unwrap();
    f.write_all(&sample_rate.to_le_bytes()).unwrap();
    f.write_all(&byte_rate.to_le_bytes()).unwrap();
    f.write_all(&block_align.to_le_bytes()).unwrap();
    f.write_all(&bits.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data_len.to_le_bytes()).unwrap();
    for &s in samples {
        f.write_all(&s.to_le_bytes()).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// A [`CueRegistry`] with every built-in cue type registered — mirrors the
/// registration performed in `AppState::new` so tests exercise the real load
/// path. Kept in sync manually; the registry-contract test asserts all 15
/// types are present so drift is caught.
pub fn full_registry() -> CueRegistry {
    use inkue_lib::cue::audio_cue::AudioCueFactory;
    use inkue_lib::cue::devamp_cue::DevampCueFactory;
    use inkue_lib::cue::fade_cue::FadeCueFactory;
    use inkue_lib::cue::group_cue::GroupCueFactory;
    use inkue_lib::cue::image_cue::ImageCueFactory;
    use inkue_lib::cue::light_cue::LightCueFactory;
    use inkue_lib::cue::memo_cue::MemoCueFactory;
    use inkue_lib::cue::mic_cue::MicCueFactory;
    use inkue_lib::cue::midi_cue::MidiCueFactory;
    use inkue_lib::cue::osc_cue::OscCueFactory;
    use inkue_lib::cue::stop_cue::StopCueFactory;
    use inkue_lib::cue::text_cue::TextCueFactory;
    use inkue_lib::cue::timecode_cue::TimecodeCueFactory;
    use inkue_lib::cue::video_cue::VideoCueFactory;
    use inkue_lib::cue::wait_cue::WaitCueFactory;

    let mut r = CueRegistry::new();
    r.register(CueType::Audio, Box::new(AudioCueFactory));
    r.register(CueType::Devamp, Box::new(DevampCueFactory));
    r.register(CueType::Fade, Box::new(FadeCueFactory));
    r.register(CueType::Midi, Box::new(MidiCueFactory));
    r.register(CueType::Group, Box::new(GroupCueFactory));
    r.register(CueType::Light, Box::new(LightCueFactory));
    r.register(CueType::Memo, Box::new(MemoCueFactory));
    r.register(CueType::Osc, Box::new(OscCueFactory));
    r.register(CueType::Stop, Box::new(StopCueFactory));
    r.register(CueType::Video, Box::new(VideoCueFactory));
    r.register(CueType::Image, Box::new(ImageCueFactory));
    r.register(CueType::Mic, Box::new(MicCueFactory));
    r.register(CueType::Timecode, Box::new(TimecodeCueFactory));
    r.register(CueType::Text, Box::new(TextCueFactory));
    r.register(CueType::Wait, Box::new(WaitCueFactory));
    r.register(CueType::Camera, Box::new(inkue_lib::cue::camera_cue::CameraCueFactory));
    r.register(CueType::Script, Box::new(inkue_lib::cue::script_cue::ScriptCueFactory));
    r.register(CueType::MidiFile, Box::new(inkue_lib::cue::midi_file_cue::MidiFileCueFactory));
    for action in inkue_lib::cue::control_cue::ALL_CONTROL_ACTIONS {
        r.register(
            action.cue_type(),
            Box::new(inkue_lib::cue::control_cue::ControlCueFactory(action)),
        );
    }
    r
}

/// Every built-in cue type — the single source of truth for "what should the
/// registry contain".
pub const ALL_CUE_TYPES: [CueType; 27] = [
    CueType::Audio,
    CueType::Memo,
    CueType::Wait,
    CueType::Group,
    CueType::Fade,
    CueType::Stop,
    CueType::Devamp,
    CueType::Video,
    CueType::Image,
    CueType::Osc,
    CueType::Midi,
    CueType::Light,
    CueType::Mic,
    CueType::Timecode,
    CueType::Text,
    CueType::Camera,
    // Command cues — eight types over one shared implementation.
    CueType::Start,
    CueType::Pause,
    CueType::Resume,
    CueType::Load,
    CueType::Reset,
    CueType::Goto,
    CueType::Arm,
    CueType::Disarm,
    CueType::Target,
    CueType::Script,
    CueType::MidiFile,
];

// ---------------------------------------------------------------------------
// Decode-with-timeout guard
// ---------------------------------------------------------------------------

/// Run `decode` on a worker thread and fail the test if it does not finish
/// within `secs`. Guards against the libmpv fallback (`mpv_wait_event` has a
/// 300 s per-wait timeout) hanging the suite on a pathological input.
pub fn with_timeout<T: Send + 'static>(
    secs: u64,
    label: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(std::time::Duration::from_secs(secs)) {
        Ok(v) => {
            let _ = handle.join();
            v
        }
        Err(_) => panic!("`{label}` did not complete within {secs}s (possible hang)"),
    }
}

// ---------------------------------------------------------------------------
// Recording engine doubles
// ---------------------------------------------------------------------------
//
// Implement the `engine_traits` interfaces, record every call, and return inert
// defaults. This is what lets a cue's `go()`/`stop()` run with no hardware while
// the test asserts *which* engine operation it triggered (and with what key
// arguments). Shared by the transport and per-cue behavioural suites.

/// One recorded engine interaction (only the fields tests assert on).
#[derive(Debug, Clone, PartialEq)]
pub enum EngineCall {
    AudioCheckNamedOutput { device: String },
    AudioPlayRouted { device: Option<String> },
    AudioPlayPausedRouted { device: Option<String> },
    AudioStopVoice { fade_ms: u32 },
    AudioPauseVoice,
    AudioResumeVoice,
    AudioSetGain { gain: f32 },
    AudioSetTrim { voice: VoiceId, gain: f32 },
    AudioSetOutputTrims { voice: VoiceId, device: String, gains: Vec<f32> },
    AudioSetPan { pan: f32 },
    AudioSetLevelMatrix { present: bool },
    AudioMapMotionStart { voice: VoiceId, device: String },
    AudioMapMotionUpdate { voice: VoiceId },
    AudioReadFilter { voice: VoiceId, filter_id: Uuid },
    AudioReadEqGain { voice: VoiceId, filter_id: Uuid },
    AudioEnsureInputFeed,
    AudioPlayMicVoice,
    AudioPanicStopAll,
    OutputShowContent { path: String, is_image: bool, preload: bool },
    OutputStopContent,
    OutputTextOverlay { ass: String },
    OutputClearText,
    OutputPanicStop,
    OutputEofFade { fade_ms: u32 },
    OutputStartPreloaded,
    OutputLinkCrossfade { duration_ms: u32, outgoing: usize },
    OutputHoldCrossfade { paused: bool },
    OutputReleaseCrossfade,
    OutputSetOpacity { voice: VoiceId, opacity: f32 },
    OutputGeometryFade { voice: VoiceId, update: VideoGeometryUpdate },
    OutputSaturationFade { voice: VoiceId, saturation: f32 },
    OutputSilentSpeed { voice: VoiceId, rate: f64 },
    AudioDevamp { stop_at_end: bool },
    AudioWholeDevamp { voice: VoiceId },
    OutputDevamp { stop_at_end: bool },
    DmxSubmitFadeBatch { channels: Vec<DmxChannelLevel>, duration: Duration, curve: FadeCurve },
}

/// Shared, thread-safe recording of engine calls in order.
pub type CallLog = Arc<Mutex<Vec<EngineCall>>>;

fn record(log: &CallLog, call: EngineCall) {
    log.lock().unwrap().push(call);
}

struct RecAudio(
    CallLog,
    inkue_lib::engine::patch_fader::PatchFaders,
    Mutex<std::collections::HashMap<VoiceId, inkue_lib::engine::device_manager::NamedOutput>>,
    Arc<Mutex<Vec<u64>>>,
);
impl AudioEngineApi for RecAudio {
    fn set_voice_trim(&self, voice: VoiceId, gain: f32) {
        record(&self.0, EngineCall::AudioSetTrim { voice, gain });
    }
    fn set_voice_output_trims(&self, voice: VoiceId, route: &inkue_lib::engine::device_manager::NamedOutput, gains: &[f32]) -> bool {
        record(&self.0, EngineCall::AudioSetOutputTrims { voice, device: route.device_id.clone(), gains: gains.to_vec() });
        true
    }
    fn named_voice_output(&self, voice: VoiceId) -> Option<inkue_lib::engine::device_manager::NamedOutput> {
        self.2.lock().unwrap().get(&voice).cloned()
    }
    fn check_named_output_ready(&self, route: &inkue_lib::engine::device_manager::NamedOutput) -> Result<()> {
        record(&self.0, EngineCall::AudioCheckNamedOutput { device: route.device_id.clone() });
        Ok(())
    }
    fn patch_faders(&self) -> &inkue_lib::engine::patch_fader::PatchFaders { &self.1 }
    fn claim_map_motion(&self, voice: VoiceId, device: &str, _: Uuid, _: &inkue_lib::engine::voice::LevelMatrix) -> bool {
        record(&self.0, EngineCall::AudioMapMotionStart { voice, device: device.into() });
        true
    }
    fn update_map_motion(&self, voice: VoiceId, _: &str, _: Uuid, _: inkue_lib::engine::voice::LevelMatrix) -> bool {
        record(&self.0, EngineCall::AudioMapMotionUpdate { voice });
        true
    }
    fn play_voice_routed(&self, voice: Voice, device_id: Option<&str>) -> Result<VoiceId> {
        record(&self.0, EngineCall::AudioPlayRouted { device: device_id.map(str::to_string) });
        self.3.lock().unwrap().push(voice.frame_pos.load(std::sync::atomic::Ordering::Relaxed));
        if let Some(route) = voice.named_output {
            self.2.lock().unwrap().insert(voice.id, route);
        }
        Ok(voice.id)
    }
    fn play_voice_paused_routed(&self, _v: Voice, device_id: Option<&str>) -> Result<VoiceId> {
        record(&self.0, EngineCall::AudioPlayPausedRouted { device: device_id.map(str::to_string) });
        Ok(Uuid::new_v4())
    }
    fn start_preloaded_file_voice(&self, _voice: VoiceId) -> Result<bool> {
        record(&self.0, EngineCall::AudioResumeVoice);
        Ok(true)
    }
    fn stop_voice(&self, _v: VoiceId, fade_ms: u32, _c: FadeCurve) -> Result<()> {
        record(&self.0, EngineCall::AudioStopVoice { fade_ms });
        Ok(())
    }
    fn pause_voice(&self, _v: VoiceId) -> Result<()> {
        record(&self.0, EngineCall::AudioPauseVoice);
        Ok(())
    }
    fn resume_voice(&self, _v: VoiceId) -> Result<()> {
        record(&self.0, EngineCall::AudioResumeVoice);
        Ok(())
    }
    fn seek_voice(&self, _v: VoiceId, _p: u64) -> Result<()> { Ok(()) }
    fn devamp_voice(&self, _v: VoiceId, stop_at_end: bool) -> Result<()> {
        record(&self.0, EngineCall::AudioDevamp { stop_at_end });
        Ok(())
    }
    fn release_whole_audio_loop(&self, voice: VoiceId) -> bool {
        record(&self.0, EngineCall::AudioWholeDevamp { voice });
        true
    }
    fn set_voice_gain(&self, _v: VoiceId, gain: f32) -> Result<()> {
        record(&self.0, EngineCall::AudioSetGain { gain });
        Ok(())
    }
    fn get_voice_gain(&self, _v: VoiceId) -> f32 { 1.0 }
    fn set_voice_pan(&self, _v: VoiceId, pan: f32) -> Result<()> {
        record(&self.0, EngineCall::AudioSetPan { pan });
        Ok(())
    }
    fn voice_cutoff_fader(&self, voice: VoiceId, filter_id: Uuid) -> Option<Arc<inkue_lib::engine::rate_fader::ScalarFader>> {
        record(&self.0, EngineCall::AudioReadFilter { voice, filter_id });
        let fader = Arc::new(inkue_lib::engine::rate_fader::ScalarFader::default());
        fader.initialize(600.0);
        Some(fader)
    }
    fn voice_eq_gain_fader(&self, voice: VoiceId, filter_id: Uuid) -> Option<Arc<inkue_lib::engine::rate_fader::ScalarFader>> {
        record(&self.0, EngineCall::AudioReadEqGain { voice, filter_id });
        let fader = Arc::new(inkue_lib::engine::rate_fader::ScalarFader::default());
        fader.initialize(0.0);
        Some(fader)
    }
    fn get_voice_pan(&self, _v: VoiceId) -> f32 { 0.0 }
    fn set_voice_level_matrix(
        &self,
        _v: VoiceId,
        matrix: Option<&inkue_lib::engine::voice::LevelMatrix>,
    ) -> Result<()> {
        record(&self.0, EngineCall::AudioSetLevelMatrix { present: matrix.is_some() });
        Ok(())
    }
    fn sample_rate(&self) -> u32 { 48_000 }
    fn ensure_input_feed(&self, _d: Option<&str>, _b: u32) -> Result<Uuid> {
        record(&self.0, EngineCall::AudioEnsureInputFeed);
        Ok(Uuid::new_v4())
    }
    fn register_synthetic_feed(&self, ch: usize, _sr: u32) -> Result<(Uuid, ringbuf::HeapProd<f32>)> {
        let (prod, _cons) = HeapRb::<f32>::new(ch.max(1) * 64).split();
        Ok((Uuid::new_v4(), prod))
    }
    #[allow(clippy::too_many_arguments)]
    fn play_mic_voice(&self, _f: Uuid, _il: usize, _ir: usize, _ol: usize, _or: usize, _g: f32, _p: f32, _fi: u32, _c: FadeCurve, _patch: Option<Arc<inkue_lib::engine::patch_fader::PatchFader>>) -> Result<VoiceId> {
        record(&self.0, EngineCall::AudioPlayMicVoice);
        Ok(Uuid::new_v4())
    }
    fn panic_stop_all(&self) -> Result<()> {
        self.1.clear();
        record(&self.0, EngineCall::AudioPanicStopAll);
        Ok(())
    }
}

/// The crossfades the recording output double has linked, by incoming voice.
/// Tests drive their phase: the incoming picture's first frame, or the
/// incoming content leaving before the dissolve has landed.
#[derive(Clone, Default)]
pub struct CrossfadeSim(Arc<Mutex<HashMap<VoiceId, CrossfadePhase>>>);

impl CrossfadeSim {
    /// Every crossfade still waiting shows its first frame now.
    pub fn reveal_all(&self) {
        for phase in self.0.lock().unwrap().values_mut() {
            if *phase == CrossfadePhase::Waiting {
                *phase = CrossfadePhase::Started { started_at: Instant::now() };
            }
        }
    }

    /// Every incoming content leaves the output (stopped, ended, failed).
    pub fn vanish_all(&self) {
        for phase in self.0.lock().unwrap().values_mut() {
            *phase = CrossfadePhase::None;
        }
    }

    /// How many crossfades were linked.
    pub fn linked(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

/// `video_audio` is the voice a Video Cue's audio track would occupy on the
/// real engine — `None` (the default) models a silent video, `Some(id)` a
/// video whose sound the cue can fade.
///
/// `headless` models the real [`OutputEngine::new_headless`]: libmpv could not
/// be loaded, so every `show_content` is refused.
///
/// `picture_waits`: a linked crossfade waits for [`CrossfadeSim::reveal_all`]
/// (a video still decoding its first frame) instead of starting at once (an
/// image already on screen).
struct RecOutput {
    log: CallLog,
    video_audio: Option<VoiceId>,
    headless: bool,
    crossfades: CrossfadeSim,
    picture_waits: bool,
    geometry: Mutex<std::collections::HashMap<VoiceId, (VideoGeometry, Option<Uuid>)>>,
    saturation: Mutex<std::collections::HashMap<VoiceId, (f32, Option<Uuid>)>>,
    silent_speed: Mutex<std::collections::HashMap<VoiceId, (f64, Option<Uuid>)>>,
}
impl OutputEngineApi for RecOutput {
    fn show_content(&self, req: ContentRequest<'_>) -> Result<VoiceId> {
        record(&self.log, EngineCall::OutputShowContent {
            path: req.file_path.to_string_lossy().into_owned(),
            is_image: req.is_image,
            preload: req.preload,
        });
        if self.headless {
            anyhow::bail!("{}", inkue_lib::engine::output_engine::NO_VIDEO_OUTPUT);
        }
        let voice = Uuid::new_v4();
        self.geometry.lock().unwrap().insert(voice, (req.geometry, None));
        self.saturation.lock().unwrap().insert(voice, (req.layer_style.saturation as f32, None));
        if req.allow_silent_speed && req.audio_voice_id.is_none() && self.video_audio.is_none() {
            self.silent_speed.lock().unwrap().insert(voice, (1.0, None));
        }
        Ok(voice)
    }
    fn claim_silent_speed(&self, voice: VoiceId, owner: Uuid) -> Option<SilentSpeedOrigin> {
        let mut speeds = self.silent_speed.lock().unwrap();
        let (rate, current_owner) = speeds.get_mut(&voice)?;
        *current_owner = Some(owner);
        Some(SilentSpeedOrigin::Ready(*rate))
    }
    fn set_silent_speed(&self, voice: VoiceId, owner: Uuid, rate: f64) -> bool {
        let mut speeds = self.silent_speed.lock().unwrap();
        let Some((speed, current_owner)) = speeds.get_mut(&voice) else { return false };
        if *current_owner != Some(owner) { return false; }
        *speed = rate;
        record(&self.log, EngineCall::OutputSilentSpeed { voice, rate });
        true
    }
    fn stop_content(&self, voice: VoiceId, _vf: u32, _af: u32) {
        record(&self.log, EngineCall::OutputStopContent);
        self.geometry.lock().unwrap().remove(&voice);
        self.saturation.lock().unwrap().remove(&voice);
        self.silent_speed.lock().unwrap().remove(&voice);
    }
    fn hard_stop_current(&self) {}
    fn panic_stop(&self) {
        record(&self.log, EngineCall::OutputPanicStop);
    }
    fn apply_geometry(&self, voice: VoiceId, geometry: &VideoGeometry) {
        if let Some(entry) = self.geometry.lock().unwrap().get_mut(&voice) {
            *entry = (*geometry, None);
        }
    }
    fn begin_voice_geometry_fade(&self, voice: VoiceId, owner: Uuid) -> Option<VideoGeometry> {
        let mut geometry = self.geometry.lock().unwrap();
        let (current, claimed) = geometry.get_mut(&voice)?;
        *claimed = Some(owner);
        Some(*current)
    }
    fn begin_voice_rotation_fade(&self, voice: VoiceId, owner: Uuid) -> Option<VideoGeometry> {
        let mut geometry = self.geometry.lock().unwrap();
        let (current, claimed) = geometry.get_mut(&voice)?;
        current.source_rotation_degrees?;
        *claimed = Some(owner);
        Some(*current)
    }
    fn set_voice_geometry_fade(&self, voice: VoiceId, owner: Uuid, update: VideoGeometryUpdate) -> bool {
        let mut geometry = self.geometry.lock().unwrap();
        let Some((current, claimed)) = geometry.get_mut(&voice) else { return false };
        if *claimed != Some(owner) { return false; }
        if let Some(angle) = update.rotation_degrees {
            let Some(current_angle) = current.source_rotation_degrees.as_mut() else { return false };
            *current_angle = angle;
        }
        if let Some(position) = update.translation {
            if let Some(x) = position.x { current.pan_x = x; }
            if let Some(y) = position.y { current.pan_y = y; }
        }
        if let Some(scale) = update.scale { current.scale = scale; }
        record(&self.log, EngineCall::OutputGeometryFade { voice, update });
        true
    }
    fn begin_voice_saturation_fade(&self, voice: VoiceId, owner: Uuid) -> Option<f32> {
        let mut values = self.saturation.lock().unwrap();
        let (saturation, current_owner) = values.get_mut(&voice)?;
        *current_owner = Some(owner);
        Some(*saturation)
    }
    fn set_voice_saturation_fade(&self, voice: VoiceId, owner: Uuid, saturation: f32) -> bool {
        let mut values = self.saturation.lock().unwrap();
        let Some((current, current_owner)) = values.get_mut(&voice) else { return false };
        if *current_owner != Some(owner) { return false; }
        *current = saturation;
        record(&self.log, EngineCall::OutputSaturationFade { voice, saturation });
        true
    }
    fn set_layer_props(&self, voice: VoiceId, style: &inkue_lib::engine::output_engine::LayerStyle, edit_saturation: bool) {
        if edit_saturation {
            if let Some(value) = self.saturation.lock().unwrap().get_mut(&voice) {
                *value = (style.saturation as f32, None);
            }
        }
    }
    fn video_audio_voice(&self, _v: VoiceId) -> Option<VoiceId> { self.video_audio }
    fn resync_audio_to_video(&self, _v: VoiceId) {}
    fn get_voice_opacity(&self, _v: VoiceId) -> f32 { 1.0 }
    fn set_voice_opacity(&self, voice: VoiceId, opacity: f32) {
        record(&self.log, EngineCall::OutputSetOpacity { voice, opacity });
    }
    fn stop_voice(&self, voice: VoiceId, _f: u32) -> Result<()> {
        self.geometry.lock().unwrap().remove(&voice);
        self.saturation.lock().unwrap().remove(&voice);
        self.silent_speed.lock().unwrap().remove(&voice);
        Ok(())
    }
    fn pause_voice(&self, _v: VoiceId) -> Result<()> { Ok(()) }
    fn resume_voice(&self, _v: VoiceId) -> Result<()> { Ok(()) }
    fn seek_voice_ms(&self, _v: VoiceId, _p: u64) {}
    fn show_text_overlay(&self, ass_text: &str, _output: Option<uuid::Uuid>) {
        record(&self.log, EngineCall::OutputTextOverlay { ass: ass_text.to_string() });
    }
    fn clear_text_overlay(&self, _output: Option<uuid::Uuid>) {
        record(&self.log, EngineCall::OutputClearText);
    }
    fn begin_eof_fade_out(&self, _v: VoiceId, fade_ms: u32) -> bool {
        record(&self.log, EngineCall::OutputEofFade { fade_ms });
        true
    }
    fn devamp_voice(&self, _v: VoiceId, stop_at_end: bool) {
        record(&self.log, EngineCall::OutputDevamp { stop_at_end });
    }
    fn start_preloaded(&self, _v: VoiceId) -> Result<bool> {
        record(&self.log, EngineCall::OutputStartPreloaded);
        Ok(true)
    }
    fn link_crossfade(&self, request: &CrossfadeRequest) -> bool {
        record(&self.log, EngineCall::OutputLinkCrossfade {
            duration_ms: request.duration_ms,
            outgoing: request.outgoing.len(),
        });
        let phase = if self.picture_waits {
            CrossfadePhase::Waiting
        } else {
            CrossfadePhase::Started { started_at: Instant::now() }
        };
        self.crossfades.0.lock().unwrap().insert(request.incoming, phase);
        true
    }
    fn crossfade_phase(&self, incoming: VoiceId) -> CrossfadePhase {
        self.crossfades.0.lock().unwrap().get(&incoming).copied().unwrap_or(CrossfadePhase::None)
    }
    fn hold_crossfade(&self, _incoming: VoiceId, paused: bool) {
        record(&self.log, EngineCall::OutputHoldCrossfade { paused });
    }
    fn release_crossfade(&self, _incoming: VoiceId) {
        record(&self.log, EngineCall::OutputReleaseCrossfade);
    }
}

struct RecDmx(CallLog);
impl DmxEngineApi for RecDmx {
    fn submit_fade(&self, universe: u16, channel: u16, width: ChannelWidth, level: f64, duration: Duration, curve: FadeCurve) {
        self.submit_fade_batch(vec![(universe, channel, width, level)], duration, curve).unwrap();
    }

    fn submit_fade_batch(&self, channels: Vec<DmxChannelLevel>, duration: Duration, curve: FadeCurve) -> anyhow::Result<()> {
        record(&self.0, EngineCall::DmxSubmitFadeBatch { channels, duration, curve });
        Ok(())
    }
}

/// A [`CueContext`] wired to recording doubles. Returns the context, the event
/// receiver (assert emitted [`CueEvent`]s), and the shared [`CallLog`].
///
/// `fixtures` / `osc_patches` / `input_patches` are injected so cues that
/// resolve workspace tables (Light, OSC, Mic) can be exercised.
pub fn recording_context_with(
    osc_patches: Vec<inkue_lib::engine::osc_patch::OscPatch>,
    fixtures: Vec<inkue_lib::engine::fixture::PatchedFixture>,
    input_patches: Vec<inkue_lib::engine::audio_input::InputPatch>,
) -> (CueContext, Receiver<CueEvent>, CallLog) {
    let (ctx, rx, log, _) = build_recording_context(osc_patches, fixtures, input_patches, None, false, false);
    (ctx, rx, log)
}

/// [`recording_context`] whose output double reports a paired audio voice for
/// every visual voice — i.e. a Video Cue that carries a sound track.
pub fn recording_context_with_video_audio() -> (CueContext, Receiver<CueEvent>, CallLog) {
    let (ctx, rx, log, _) =
        build_recording_context(Vec::new(), Vec::new(), Vec::new(), Some(Uuid::new_v4()), false, false);
    (ctx, rx, log)
}

/// [`recording_context`] whose crossfades wait for their incoming picture
/// (driven through the returned [`CrossfadeSim`]); videos carry a sound track
/// when `video_audio` is set.
pub fn recording_context_with_crossfades(
    video_audio: bool,
) -> (CueContext, Receiver<CueEvent>, CallLog, CrossfadeSim) {
    let audio = video_audio.then(Uuid::new_v4);
    build_recording_context(Vec::new(), Vec::new(), Vec::new(), audio, false, true)
}

/// [`recording_context`] running on a headless output engine — the state the
/// app falls back to when libmpv is missing.
pub fn recording_context_headless() -> (CueContext, Receiver<CueEvent>, CallLog) {
    let (ctx, rx, log, _) = build_recording_context(Vec::new(), Vec::new(), Vec::new(), None, true, false);
    (ctx, rx, log)
}

fn build_recording_context(
    osc_patches: Vec<inkue_lib::engine::osc_patch::OscPatch>,
    fixtures: Vec<inkue_lib::engine::fixture::PatchedFixture>,
    input_patches: Vec<inkue_lib::engine::audio_input::InputPatch>,
    video_audio: Option<VoiceId>,
    headless: bool,
    picture_waits: bool,
) -> (CueContext, Receiver<CueEvent>, CallLog, CrossfadeSim) {
    let log: CallLog = Arc::new(Mutex::new(Vec::new()));
    let crossfades = CrossfadeSim::default();
    let (tx, rx) = unbounded();
    let ctx = CueContext::new(
        Arc::new(RecAudio(log.clone(), Default::default(), Default::default(), Default::default())),
        Arc::new(RecOutput {
            log: log.clone(),
            video_audio,
            headless,
            crossfades: crossfades.clone(),
            picture_waits,
            geometry: Mutex::new(std::collections::HashMap::new()),
            saturation: Mutex::new(std::collections::HashMap::new()),
            silent_speed: Mutex::new(std::collections::HashMap::new()),
        }),
        tx,
        500,
        Vec::new(),
        None,
        osc_patches,
        Arc::new(RecDmx(log.clone())),
        fixtures,
        Vec::new(),
        input_patches,
        256,
    );
    (ctx, rx, log, crossfades)
}

/// [`recording_context_with`] with empty workspace tables.
pub fn recording_context() -> (CueContext, Receiver<CueEvent>, CallLog) {
    recording_context_with(Vec::new(), Vec::new(), Vec::new())
}

pub fn record_audio_launch_positions(context: &mut CueContext, log: &CallLog) -> Arc<Mutex<Vec<u64>>> {
    let positions = Arc::new(Mutex::new(Vec::new()));
    context.audio_engine = Arc::new(RecAudio(log.clone(), Default::default(), Default::default(), positions.clone()));
    positions
}
