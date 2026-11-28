//! [`AudioEngine`] — the top-level audio subsystem.
//!
//! **Real-time safety:** the audio callback (`fill_buffer`) must never
//! allocate, block, or do I/O. Control and status use ring buffers, and meter
//! snapshots use bounded atomic storage.

use std::{collections::HashSet, sync::{Arc, Mutex}};

use anyhow::{anyhow, Result};
use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Stream, StreamConfig};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;
use uuid::Uuid;

use crate::preferences::MachineAudioConfig;

use super::{
    audio_host::{self, Resolved},
    audio_input::{open_input, InputCapture},
    cue_effects::CueEffect,
    device_manager::{DeviceManager, NamedOutput},
    output_meter::OutputMeter,
    patch_effects::{self, PatchBus, PatchBusState, MAX_PROCESSED_PATCHES},
    rate_fader::RateFader,
    ring_command::{AudioCommand, AudioStatus, FadeCurve, VoiceId},
    voice::{FadeDirection, FadeState, LiveSource, Voice, VoiceState},
};

const _MAX_VOICES: usize = 64;
const RING_CAPACITY: usize = 256;
pub const DEFAULT_FADE_OUT_MS: u32 = 500;

/// Longest the watchdog waits for PipeWire to say whether a pinned node still
/// exists, so a stalled daemon only delays (never blocks) its 2 s tick.
const NODE_PRESENCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Circular staging buffer per input feed, in frames.  Large enough to absorb
/// two full input-callback periods at maximum buffer size (2 × 2048 @ 48 kHz ≈
/// 85 ms) while staying small so the target-lag calculation stays well inside.
const STAGING_FRAMES: usize = 8192;

// ---------------------------------------------------------------------------
// Live input feed — one per captured device, drained by the output callback
// ---------------------------------------------------------------------------

/// One live input device's capture: its ring consumer plus a circular staging
/// buffer the output callback keeps current and live voices resample from.
///
/// Drained every output block (`drain`) so the input stays "warm" and bounded
/// even when no Mic Cue is playing — a GO is then instant with no cold-start.
struct InputFeed {
    /// Stable id referenced by a [`LiveSource`].
    id: Uuid,
    /// OS device id this feed captures from (one feed per device).
    device_id: String,
    /// Interleaved channel count of the staging frames.
    in_channels: usize,
    /// Input device sample rate (Hz).
    sample_rate: u32,
    /// Ring consumer fed by the cpal input callback.
    cons: ringbuf::HeapCons<f32>,
    /// Circular interleaved staging, `STAGING_FRAMES * in_channels` long.
    staging: Box<[f32]>,
    /// Monotonic count of frames written into `staging`.
    write_frame: u64,
    /// Keeps the cpal input stream alive (`None` only in unit tests).
    _capture: Option<InputCapture>,
}

impl InputFeed {
    /// Pop every available frame from the ring into the circular staging buffer.
    /// RT-safe: bounded by what the input callback produced, no allocation.
    fn drain(&mut self) {
        let ch = self.in_channels;
        while self.cons.occupied_len() >= ch {
            let slot = (self.write_frame as usize % STAGING_FRAMES) * ch;
            for c in 0..ch {
                if let Some(s) = self.cons.try_pop() {
                    self.staging[slot + c] = s;
                }
            }
            self.write_frame += 1;
        }
    }

    /// Linear-interpolated sample for input channel `ch` at fractional frame `pos`.
    fn sample(&self, ch: usize, pos: f64) -> f32 {
        let i0 = pos.floor() as u64;
        let frac = (pos - i0 as f64) as f32;
        let a = self.staging[(i0 as usize % STAGING_FRAMES) * self.in_channels + ch];
        let b = self.staging[((i0 + 1) as usize % STAGING_FRAMES) * self.in_channels + ch];
        a + (b - a) * frac
    }
}


/// A voice pool published **wait-free** to the real-time audio callback.
///
/// The RT callback reads a snapshot via [`ArcSwap::load`] — no lock, so it can
/// never be starved into emitting a block of silence (the old `try_lock`
/// failure mode).  Non-RT threads mutate `master` (a plain `Mutex`, only ever
/// contended among themselves) and then [`publish`](VoicePool::publish) a fresh
/// immutable snapshot. A writer keeps retired snapshots until their readers
/// release them, so lists, voices and sample buffers are reclaimed off the callback.
struct VoicePool {
    /// Editable master list — locked only by non-RT threads.
    master: Mutex<Vec<Arc<Voice>>>,
    /// Immutable snapshot the RT callback loads.
    rt: Arc<ArcSwap<Vec<Arc<Voice>>>>,
    /// Retired lists that a reader was still using at publication.
    retained: Mutex<Vec<Arc<Vec<Arc<Voice>>>>>,
}

impl VoicePool {
    fn new() -> Self {
        Self {
            master: Mutex::new(Vec::new()),
            rt: Arc::new(ArcSwap::from_pointee(Vec::new())),
            retained: Mutex::new(Vec::new()),
        }
    }

    /// The handle the RT callback reads from (`load()` each block).
    fn rt_handle(&self) -> Arc<ArcSwap<Vec<Arc<Voice>>>> {
        Arc::clone(&self.rt)
    }

    /// Called with the master list locked, so all publications are serialized.
    fn publish(&self, master: &[Arc<Voice>]) {
        let old = self.rt.swap(Arc::new(master.to_vec()));
        let mut retained = self.retained.lock().unwrap_or_else(|poison| poison.into_inner());
        retained.retain(|snapshot| Arc::strong_count(snapshot) > 1);
        // swap has paid outstanding ArcSwap reader debts. No new callback
        // can obtain this old list, and any callback still using it owns a count.
        if Arc::strong_count(&old) > 1 {
            retained.push(old);
        }
    }

    /// Append a voice and publish.  On a poisoned lock the voice is handed back
    /// (`Err`) so a caller can recover it — e.g. fall the cue back to the main
    /// output instead of losing it.
    fn push(&self, voice: Arc<Voice>) -> std::result::Result<(), Arc<Voice>> {
        match self.master.lock() {
            Ok(mut m) => {
                // The potential replacement must decide its cut before the
                // outgoing voice is visited in the same callback snapshot.
                if voice.playlist_cut.is_some() { m.insert(0, voice); } else { m.push(voice); }
                self.publish(&m);
                Ok(())
            }
            Err(_) => Err(voice),
        }
    }

    /// Run `f` over the current voices (non-RT read; locks `master`).
    fn with<T>(&self, f: impl FnOnce(&[Arc<Voice>]) -> T) -> Option<T> {
        self.master.lock().ok().map(|m| f(&m))
    }

    /// Retain voices matching `keep`, then publish.  Removed `Arc<Voice>`s are
    /// dropped on this (non-RT) thread.
    fn retain(&self, keep: impl Fn(&Arc<Voice>) -> bool) {
        if let Ok(mut m) = self.master.lock() {
            m.retain(|v| keep(v));
            self.publish(&m);
        }
    }

    fn withdraw_named_voices(&self, statuses: &mut Vec<AudioStatus>) {
        for voice in self.rt.load().iter() {
            if voice.named_output.is_some()
                && matches!(voice.voice_state(), VoiceState::Playing | VoiceState::Paused | VoiceState::FadingOut)
            {
                voice.set_stopped();
                statuses.push(AudioStatus::OutputWithdrawn { voice_id: voice.id });
            }
        }
    }
}

/// One additional cpal output stream opened for an Output Patch that targets
/// a different device than the main stream.
///
/// Owns its own voice pool (mixed only by its own callback — per-stream pools
/// keep RT `try_lock` contention identical to the single-stream design) and
/// its own command/status rings.  Live (Mic) voices never land here: input
/// feeds are drained exclusively by the main stream's callback, so aux
/// streams get a permanently empty feed list.
struct AuxStream {
    // Drop the stream and its callback before dropping the last writer-owned lists.
    _stream: Stream,
    /// OS device id this stream is open on.
    device_id: String,
    exact_device_id: Option<String>,
    /// Output channel count of this stream (for channel-bounds alerts).
    channels: u32,
    voices: VoicePool,
    cmd_prod: ringbuf::HeapProd<AudioCommand>,
    status_cons: ringbuf::HeapCons<AudioStatus>,
    /// Set by the cpal error callback on DeviceNotAvailable.
    failed: Arc<std::sync::atomic::AtomicBool>,
    last_callback_us: Arc<std::sync::atomic::AtomicU64>,
    period: Arc<std::sync::atomic::AtomicU32>,
    sample_rate: u32,
    opened_at_us: u64,
}

impl AuxStream {
    fn deadline(&self, now: u64) -> Option<u64> {
        use std::sync::atomic::Ordering::Relaxed;
        let last = self.last_callback_us.load(Relaxed).max(self.opened_at_us);
        let window = first_render_window_us(self.period.load(Relaxed), self.sample_rate);
        (!self.failed.load(Relaxed) && now.saturating_sub(last) <= window)
            .then_some(now.saturating_add(window))
    }

    fn named_deadline(&self, route: &NamedOutput) -> Result<u64> {
        check_named_output(route, self.exact_device_id.as_deref(), self.channels)?;
        self.deadline(audio_clock_us()).ok_or_else(|| {
            anyhow!("The patch output stopped responding. Reconnect it and try GO again.")
        })
    }
}

#[derive(Default)]
struct AuditionSlot {
    pending: Option<Uuid>,
    playing: Option<(Uuid, super::audition::Audition)>,
}

/// The audio engine.
pub struct AudioEngine {
    pub patch_faders: super::patch_fader::PatchFaders,
    // Keep the voice pool alive until the main output callback has shut down.
    _stream: Mutex<Option<MainOutput>>,
    pub device_manager: Mutex<DeviceManager>,
    cmd_prod: Mutex<ringbuf::HeapProd<AudioCommand>>,
    status_cons: Mutex<ringbuf::HeapCons<AudioStatus>>,
    voices: VoicePool,
    /// Additional per-device streams for Output Patches targeting a device
    /// other than the main one.  Opened lazily at the first GO that needs the
    /// device, kept open afterwards so later GOs start with zero device-open
    /// latency.  Never contains the main device.
    aux_streams: Mutex<Vec<AuxStream>>,
    /// Route withdrawals on the control thread when a callback cannot report.
    named_output_withdrawals: Mutex<Vec<AudioStatus>>,
    /// Live input feeds (one per captured device), shared with the output
    /// callback which drains them each block.
    input_feeds: Arc<Mutex<Vec<InputFeed>>>,
    sample_rate: std::sync::atomic::AtomicU32,
    /// Actual output callback period in frames (updated on the first callback).
    /// Used by `mix_live` to set a tight `target_lag` instead of a fixed 25 ms.
    output_period: Arc<std::sync::atomic::AtomicU32>,
    /// Monotonic microsecond timestamp of the last output callback.  Shared across restarts; the
    /// device watchdog treats a count that stops advancing for ~2 s as a dead
    /// stream (kind-agnostic device-loss detection, even if cpal reports no error).
    output_callbacks: Arc<std::sync::atomic::AtomicU64>,
    /// Total output channel count of the current stream (updated on restart).
    output_channels: std::sync::atomic::AtomicU32,
    /// Output-channel offset applied to unpatched voices at submission — the
    /// selected ASIO output pair (0 on other backends).
    default_out_offset: std::sync::atomic::AtomicU32,
    master_gain: Arc<std::sync::atomic::AtomicU32>,
    /// Failure flag of the **current** stream (replaced on every restart).  Set by
    /// the cpal error callback after repeated `DeviceNotAvailable`; read by the
    /// device watchdog to detect a lost output device mid-show.
    stream_failed: Mutex<Arc<std::sync::atomic::AtomicBool>>,
    /// The config the operator actually selected — kept across an automatic
    /// fallback so the watchdog can detect the desired device's return.
    desired_config: Mutex<MachineAudioConfig>,
    /// Device id the current stream is open on (`None` = system default).
    current_device_id: Mutex<Option<String>>,
    audition: Mutex<AuditionSlot>,
    /// `true` while running on the default device after the desired one was lost.
    in_fallback: std::sync::atomic::AtomicBool,
    /// The current stream is pinned to one PipeWire node, whose loss raises no
    /// stream error (WirePlumber re-links the stream to the default sink): the
    /// watchdog polls the node's presence instead.
    pins_node: std::sync::atomic::AtomicBool,
}

struct MainOutput {
    _stream: Stream,
    meter: Arc<OutputMeter>,
    exact_device_id: Option<String>,
}

/// Snapshot of the output device's health, read by the device watchdog.
#[derive(Debug, Clone)]
pub struct AudioHealth {
    /// The current stream is erroring (device pulled / grabbed exclusively).
    pub failed: bool,
    /// Running on the default device after an automatic fallback.
    pub in_fallback: bool,
    /// The operator-selected device's display label — friendly name when known,
    /// else its id (`None` = system default).
    pub desired_device: Option<String>,
    /// Whether the desired device is currently present among enumerated devices.
    pub desired_present: bool,
}

// SAFETY: cpal::Stream is not Send on Windows when using WASAPI.
unsafe impl Send for AudioEngine {}
unsafe impl Sync for AudioEngine {}

/// The stream-independent half of an [`AudioEngine`], built before any device
/// is opened and shared with every stream the engine goes on to open.
///
/// Exists so the normal and the silent constructor assemble exactly the same
/// engine — the only difference being whether a [`StreamResult`] is available.
fn audio_clock_us() -> u64 {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    1 + EPOCH.get_or_init(std::time::Instant::now).elapsed().as_micros() as u64
}

fn first_render_window_us(period: u32, sample_rate: u32) -> u64 {
    (4 * u64::from(period) * 1_000_000 / u64::from(sample_rate.max(1))).max(250_000)
}

const OUTPUT_START_MESSAGE: &str = "Audio start held. Wait for the output device, then GO again.";

pub(crate) fn report_held_audio() {
    log::warn!("{OUTPUT_START_MESSAGE}");
    crate::health::set(crate::health::HealthAlert::new(
        "audio-start", crate::health::HealthLevel::Error, OUTPUT_START_MESSAGE,
    ));
}

struct EngineCore {
    voices: VoicePool,
    input_feeds: Arc<Mutex<Vec<InputFeed>>>,
    master_gain: Arc<std::sync::atomic::AtomicU32>,
    output_period: Arc<std::sync::atomic::AtomicU32>,
    output_callbacks: Arc<std::sync::atomic::AtomicU64>,
}

impl EngineCore {
    fn new() -> Self {
        audio_clock_us(); // Initialize the process clock off the realtime thread.
        Self {
            voices: VoicePool::new(),
            input_feeds: Arc::new(Mutex::new(Vec::new())),
            master_gain: Arc::new(std::sync::atomic::AtomicU32::new(f32::to_bits(1.0_f32))),
            output_period: Arc::new(std::sync::atomic::AtomicU32::new(256)),
            output_callbacks: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    fn open(
        &self,
        config: &MachineAudioConfig,
        stream_failed: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<StreamResult> {
        open_stream_inner(
            config,
            self.voices.rt_handle(),
            Arc::clone(&self.input_feeds),
            Arc::clone(&self.master_gain),
            Arc::clone(&self.output_period),
            stream_failed,
            Arc::clone(&self.output_callbacks),
            false,
        )
    }

    /// Build the engine around an open stream, or around none (silent mode).
    fn assemble(
        self,
        config: &MachineAudioConfig,
        stream: Option<StreamResult>,
        stream_failed: Arc<std::sync::atomic::AtomicBool>,
        started_in_fallback: bool,
    ) -> Arc<AudioEngine> {
        // In silent mode the ring buffers have no consumer: `send_command`
        // fills the queue and then reports "ring buffer full", which callers
        // already treat as a non-fatal playback error.  A later `restart()`
        // replaces both halves with the new stream's.
        let (cmd_prod, status_cons, stream, sample_rate, channels, out_offset, pins_node) = match stream {
            Some(sr) => (
                sr.cmd_prod, sr.status_cons, Some(MainOutput {
                    _stream: sr.stream, meter: sr.meter, exact_device_id: sr.exact_device_id,
                }),
                sr.sample_rate, sr.channels, sr.default_out_offset, sr.pins_node,
            ),
            None => {
                let (cmd_prod, _) = HeapRb::<AudioCommand>::new(RING_CAPACITY).split();
                let (_, status_cons) = HeapRb::<AudioStatus>::new(RING_CAPACITY).split();
                (cmd_prod, status_cons, None, 48_000, 2, 0, false)
            }
        };

        let engine = Arc::new(AudioEngine {
            device_manager: Mutex::new(DeviceManager::new()),
            cmd_prod: Mutex::new(cmd_prod),
            status_cons: Mutex::new(status_cons),
            voices: self.voices,
            aux_streams: Mutex::new(Vec::new()),
            named_output_withdrawals: Mutex::default(),
            patch_faders: Default::default(),
            input_feeds: self.input_feeds,
            _stream: Mutex::new(stream),
            sample_rate: std::sync::atomic::AtomicU32::new(sample_rate),
            output_channels: std::sync::atomic::AtomicU32::new(channels),
            default_out_offset: std::sync::atomic::AtomicU32::new(out_offset),
            master_gain: self.master_gain,
            output_period: self.output_period,
            output_callbacks: self.output_callbacks,
            stream_failed: Mutex::new(stream_failed),
            // `desired_config` keeps the operator's choice even when we started on
            // the fallback, so the watchdog can offer a restore when it returns.
            desired_config: Mutex::new(config.clone()),
            audition: Mutex::default(),
            current_device_id: Mutex::new(
                if started_in_fallback { None } else { config.device_id.clone() },
            ),
            in_fallback: std::sync::atomic::AtomicBool::new(started_in_fallback),
            pins_node: std::sync::atomic::AtomicBool::new(pins_node),
        });

        // A broken configured device (HDMI with no display, unplugged interface)
        // is handled continuously by the device watchdog (lib.rs), which falls
        // back to the default device and surfaces a banner — no one-shot startup
        // watchdog needed.

        // Warm the device cache off the main thread.  `DeviceManager::new()` no
        // longer enumerates (that blocked app startup on a slow/hung Windows
        // audio driver, cpal #867); a one-shot bounded refresh fills it here
        // without stalling `.setup()`.
        {
            let engine_bg = Arc::clone(&engine);
            let _ = std::thread::Builder::new()
                .name("inkue-device-warmup".to_string())
                .spawn(move || {
                    let devices = crate::engine::device_manager::run_bounded(
                        crate::engine::device_manager::ENUM_TIMEOUT,
                        crate::engine::device_manager::enumerate_output_devices,
                    )
                    .unwrap_or_default();
                    if let Ok(mut mgr) = engine_bg.device_manager.lock() {
                        mgr.replace_cache(devices);
                    }
                });
        }

        engine
    }
}

impl AudioEngine {
    /// Open an output device according to `config` and start the audio callback.
    pub fn new(config: &MachineAudioConfig) -> Result<Arc<Self>> {
        let core = EngineCore::new();
        let stream_failed = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Resilient startup: if the configured device is absent (unplugged since
        // it was saved), fall back to the system default rather than crashing.
        // The device watchdog then raises the banner and offers a restore when it
        // returns.  `desired_config` keeps the operator's original choice.
        let (sr, started_in_fallback) = match core.open(config, Arc::clone(&stream_failed)) {
            Ok(sr) => (sr, false),
            Err(e) if config.device_id.is_some() => {
                log::warn!(
                    "Configured audio device unavailable at startup ({e}); \
                     falling back to the system default"
                );
                let fallback = MachineAudioConfig { device_id: None, ..config.clone() };
                (core.open(&fallback, Arc::clone(&stream_failed))?, true)
            }
            Err(e) => return Err(e),
        };

        Ok(core.assemble(config, Some(sr), stream_failed, started_in_fallback))
    }

    /// Construct a **silent** engine: no device opened, no output callback.
    ///
    /// The fallback when no audio device can be opened at all — no sound card,
    /// or no ALSA/PipeWire server running.  The workspace still loads and
    /// video, MIDI, OSC, timecode and lighting cues still run, so the operator
    /// gets a working app plus a health banner instead of a process that
    /// vanishes at startup.  The device watchdog keeps retrying and brings
    /// audio up as soon as a device appears.
    pub fn new_silent(config: &MachineAudioConfig) -> Arc<Self> {
        // Pre-failed so `audio_health()` reports the fault immediately instead
        // of waiting for the watchdog's callback-stall heuristic.
        let stream_failed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        EngineCore::new().assemble(config, None, stream_failed, false)
    }

    // ── Device resilience ─────────────────────────────────────────────────────

    /// Monotonic output-callback timestamp.  The watchdog flags a dead stream when
    /// this stops advancing — a device-loss signal that does not depend on cpal
    /// surfacing an error of a particular kind.
    pub fn last_callback_us(&self) -> u64 {
        self.output_callbacks.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Snapshot of the output device's current health for the watchdog.
    pub fn audio_health(&self) -> AudioHealth {
        let in_fallback = self.in_fallback.load(std::sync::atomic::Ordering::Relaxed);
        let failed = self
            .stream_failed
            .lock()
            .map(|f| f.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(false)
            || (!in_fallback && self.pinned_node_vanished());
        let (desired_id, desired_label) = self
            .desired_config
            .lock()
            .ok()
            .map(|c| {
                // Presence is checked by id; the banner shows the friendly name
                // (falling back to the id for devices saved before names existed).
                (c.device_id.clone(), c.device_name.clone().or_else(|| c.device_id.clone()))
            })
            .unwrap_or((None, None));
        // Only enumerate devices while in fallback — that is the sole moment we
        // need to detect the desired device's return.  In the healthy steady
        // state the watchdog stays free (just an atomic read of `failed`).
        let desired_present = match (&in_fallback, &desired_id) {
            (true, Some(id)) => {
                // Enumerate off the manager lock (bounded) so a slow WASAPI query
                // never stalls a concurrent main-thread reader of the cache.
                let devices = crate::engine::device_manager::run_bounded(
                    crate::engine::device_manager::ENUM_TIMEOUT,
                    crate::engine::device_manager::enumerate_output_devices,
                )
                .unwrap_or_default();
                let present = devices.iter().any(|d| &d.id == id);
                if let Ok(mut mgr) = self.device_manager.lock() {
                    mgr.replace_cache(devices);
                }
                present
            }
            _ => true,
        };
        AudioHealth { failed, in_fallback, desired_device: desired_label, desired_present }
    }

    /// `true` when the PipeWire node the current stream is pinned to has
    /// disappeared.  An unanswered query counts as present: a slow daemon
    /// must not trigger a fallback.
    fn pinned_node_vanished(&self) -> bool {
        if !self.pins_node.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let Some(id) = self.current_device_id.lock().ok().and_then(|d| d.clone()) else {
            return false;
        };
        crate::engine::device_manager::run_bounded(NODE_PRESENCE_TIMEOUT, move || {
            audio_host::pipewire_output_present(&id)
        })
        .flatten()
            == Some(false)
    }

    /// Open `config` as the operator's chosen device (an explicit settings change).
    /// Records it as the desired device and clears any fallback state.
    pub fn apply_user_config(&self, config: &MachineAudioConfig) -> Result<()> {
        if let Ok(mut d) = self.desired_config.lock() {
            *d = config.clone();
        }
        self.in_fallback.store(false, std::sync::atomic::Ordering::Relaxed);
        self.restart(config)
    }

    /// Automatically fall back to the system default after the desired device was
    /// lost, keeping the show audible.
    ///
    /// Returns the lost device id when the default device **actually took
    /// over**, `None` when there was nothing to fall back to either — the
    /// engine then stays silent and `in_fallback` stays false, so the watchdog
    /// keeps retrying instead of reporting a fallback that never happened.
    pub fn fall_back_to_default(&self) -> Option<String> {
        let desired = self.desired_config.lock().ok().map(|c| c.clone())?;
        let lost = desired.device_id.clone();
        let fallback = MachineAudioConfig { device_id: None, ..desired };
        match self.restart(&fallback) {
            Ok(()) => {
                self.in_fallback.store(true, std::sync::atomic::Ordering::Relaxed);
                lost
            }
            Err(e) => {
                log::error!("Audio fallback restart failed: {e}");
                None
            }
        }
    }

    /// Retry the configured device after a *total* failure — no device could be
    /// opened at startup, or the machine has none at all.
    ///
    /// Unlike [`Self::fall_back_to_default`] there is nothing to fall back to,
    /// so this is a plain retry: `true` means audio is live again and the
    /// watchdog clears the banner on its next tick.  A failed attempt is silent
    /// (it runs on a timer and must not spam the log).
    pub fn retry_output_stream(&self) -> bool {
        let Ok(desired) = self.desired_config.lock().map(|c| c.clone()) else {
            return false;
        };
        match self.restart(&desired) {
            Ok(()) => {
                log::info!("Audio output device opened on retry");
                true
            }
            Err(_) => false,
        }
    }

    /// Re-open the operator's chosen device after it returned (manual restore).
    pub fn restore_desired(&self) -> Result<()> {
        let desired = self
            .desired_config
            .lock()
            .map(|c| c.clone())
            .map_err(|_| anyhow!("desired_config poisoned"))?;
        self.in_fallback.store(false, std::sync::atomic::Ordering::Relaxed);
        self.restart(&desired)
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Total output channel count of the currently open stream.
    pub fn output_channels(&self) -> u32 {
        self.output_channels.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// A manual mixer move takes control from the patch's active Fade.
    pub fn set_patch_gain(&self, patch_id: Uuid, gain: f32) -> Result<()> {
        self.patch_faders.resolve(patch_id, gain).manual(gain);
        Ok(())
    }

    /// Set the master output gain (real-time safe via atomic).
    pub fn set_master_gain(&self, gain: f32) {
        self.master_gain.store(f32::to_bits(gain), std::sync::atomic::Ordering::Relaxed);
    }

    /// Shift an unpatched voice's L/R outputs to the default output offset
    /// (the selected ASIO pair).  Patched voices route exactly where their
    /// Output Patch says.
    fn apply_default_offset(&self, voice: &mut Voice) {
        if voice.patched {
            return;
        }
        let offset = self.default_out_offset.load(std::sync::atomic::Ordering::Relaxed) as usize;
        voice.out_l += offset;
        voice.out_r += offset;
    }

    /// Hold the stream stable through publication. During loss or reopening,
    /// reject the GO before it can leave a voice in the shared pool.
    fn ready_output(&self) -> Result<(std::sync::MutexGuard<'_, Option<MainOutput>>, u64)> {
        use std::sync::atomic::Ordering::Relaxed;
        let now = audio_clock_us();
        let last = self.last_callback_us();
        let tolerance = first_render_window_us(self.output_period.load(Relaxed), self.sample_rate());
        if let Ok(stream) = self._stream.try_lock() {
            if stream.is_some() && last > 0 && now.saturating_sub(last) <= tolerance
                && self.stream_failed.lock().is_ok_and(|flag| !flag.load(Relaxed))
            {
                return Ok((stream, now.saturating_add(tolerance)));
            }
        }
        report_held_audio();
        anyhow::bail!(OUTPUT_START_MESSAGE)
    }

    pub(crate) fn named_main_device(&self) -> Option<String> {
        let stream = self._stream.try_lock().ok()?;
        let failed = self.stream_failed.lock().ok()?.load(std::sync::atomic::Ordering::Relaxed);
        if failed {
            return None;
        }
        stream.as_ref()?.exact_device_id.clone()
    }

    fn withdraw_named_voices(&self, pool: &VoicePool) {
        let mut statuses = self.named_output_withdrawals.lock().unwrap_or_else(|e| e.into_inner());
        pool.withdraw_named_voices(&mut statuses);
    }

    /// Start on the next callback. A deadline also covers loss immediately
    /// after the producer's health check, before any samples were rendered.
    pub fn play_voice(&self, mut voice: Voice) -> Result<VoiceId> {
        let (stream, deadline) = self.ready_output()?;
        self.prepare_main_voice(&mut voice, stream.as_ref())?;
        let id = voice.id;
        voice.start_deadline_us.store(deadline, std::sync::atomic::Ordering::Relaxed);
        voice.set_playing();
        self.voices.push(Arc::new(voice)).map_err(|_| anyhow!("voices mutex poisoned"))?;
        crate::health::clear("audio-start");
        Ok(id)
    }

    fn prepare_main_voice(&self, voice: &mut Voice, stream: Option<&MainOutput>) -> Result<()> {
        if let Some(route) = &voice.named_output {
            check_named_output(route, stream.and_then(|s| s.exact_device_id.as_deref()), self.output_channels())?;
            if route.has_audio_effects() {
                voice.patch_effects = Some(match self.available_patch_bus(route)? {
                    Some(bus) => bus,
                    None => Arc::new(PatchBus::new(route.clone(), self.output_channels() as usize, self.sample_rate())?),
                });
            }
        }
        self.apply_default_offset(voice);
        self.check_channel_bounds(voice, self.output_channels());
        Ok(())
    }

    fn available_patch_bus(&self, route: &NamedOutput) -> Result<Option<Arc<PatchBus>>> {
        use std::sync::atomic::Ordering::Relaxed;
        patch_effects::check_capacity(self.output_channels() as usize, self.output_period.load(Relaxed) as usize)?;
        self.voices.with(|voices| {
            let mut active_patches = HashSet::new();
            for bus in voices.iter().filter(|voice| active_voice(voice))
                .filter_map(|voice| voice.patch_effects.as_ref())
            {
                if bus.route.patch_id == route.patch_id {
                    anyhow::ensure!(bus.route == *route, "Stop the audio on this Output Patch before using its changed effects or routing.");
                    return Ok(Some(bus.clone()));
                }
                active_patches.insert(bus.route.patch_id);
            }
            anyhow::ensure!(active_patches.len() < MAX_PROCESSED_PATCHES,
                "Stop one of the {MAX_PROCESSED_PATCHES} processed Output Patches before starting another.");
            Ok(None)
        }).ok_or_else(|| anyhow!("Cannot verify the active Output Patch effects."))?
    }

    pub(crate) fn patch_has_active_audio(&self, patch_id: Uuid) -> Result<bool, String> {
        let contains = |voices: &[Arc<Voice>]| {
            voices.iter().any(|voice| voice.output_patch_id == Some(patch_id) && active_voice(voice))
        };
        let busy = self.voices.with(contains).ok_or("Cannot verify the active patch audio.")?;
        if busy {
            return Ok(true);
        }
        let aux = self.aux_streams.lock().map_err(|_| "Cannot verify the active patch audio.")?;
        for stream in aux.iter() {
            if stream.voices.with(contains).ok_or("Cannot verify the active patch audio.")? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Pair a paused output voice with the first video frame. Resume checks
    /// device health again because the output may disappear while waiting.
    pub fn play_voice_paused(&self, mut voice: Voice) -> Result<VoiceId> {
        let (stream, _) = self.ready_output()?;
        self.prepare_main_voice(&mut voice, stream.as_ref())?;
        let id = voice.id;
        voice.set_paused();
        self.voices.push(Arc::new(voice)).map_err(|_| anyhow!("voices mutex poisoned"))?;
        Ok(id)
    }

    /// Release a File Audio voice staged on the main output without starting
    /// a replacement or treating a voice discarded by the engine as playing.
    pub(crate) fn start_preloaded_file_voice(&self, voice_id: VoiceId) -> Result<bool> {
        use std::sync::atomic::Ordering::{Relaxed, Release};
        let (stream, deadline) = self.ready_output()?;
        let offset = self.default_out_offset.load(Relaxed) as usize;
        let resumed = self.voices.with(|pool| {
            let Some(voice) = pool.iter().find(|voice| voice.id == voice_id) else { return false };
            if voice.patched || (voice.out_l, voice.out_r) != (offset, offset + 1) {
                return false;
            }
            voice.start_deadline_us.store(deadline, Relaxed);
            voice.state.compare_exchange(VoiceState::Paused as u8, VoiceState::Playing as u8, Release, Relaxed).is_ok()
        }).ok_or_else(|| anyhow!("Could not release the prepared audio voice."))?;
        drop(stream);
        if resumed {
            crate::health::clear("audio-start");
        }
        Ok(resumed)
    }

    // ── Output Patch device routing ───────────────────────────────────────────

    pub fn reserve_audition(&self, request: Uuid) -> Result<(), String> {
        self.audition.lock().map_err(|e| e.to_string())?.pending = Some(request);
        Ok(())
    }

    pub fn start_audition<'a>(
        &self,
        request: Uuid,
        device: &str,
        patches: impl IntoIterator<Item = &'a str>,
        clip: super::audition::AuditionClip,
    ) -> Result<Uuid, String> {
        self.start_audition_with_effects(request, device, patches, clip, Vec::new())
    }

    pub(crate) fn start_audition_with_effects<'a>(
        &self,
        request: Uuid,
        device: &str,
        patches: impl IntoIterator<Item = &'a str>,
        clip: super::audition::AuditionClip,
        effects: Vec<CueEffect>,
    ) -> Result<Uuid, String> {
        let mut slot = self.audition.lock().map_err(|e| e.to_string())?;
        if slot.pending != Some(request) {
            return Err("Audition canceled.".into());
        }
        slot.pending = None;
        let main = self.current_device_id.lock().map_err(|e| e.to_string())?;
        let pinned = self.pins_node.load(std::sync::atomic::Ordering::Relaxed)
            && !self.in_fallback.load(std::sync::atomic::Ordering::Relaxed);
        let node = super::audition::dedicated_node(device, main.as_deref().filter(|_| pinned), patches)?;
        drop(slot.playing.take());
        slot.playing = Some((request, super::audition::Audition::start_with_effects(node, clip, effects)?));
        Ok(request)
    }

    pub fn stop_audition(&self, id: Option<Uuid>) {
        if let Ok(mut slot) = self.audition.lock() {
            if id.is_none() || slot.pending == id {
                slot.pending = None;
            }
            if id.is_none() || slot.playing.as_ref().map(|(key, _)| *key) == id {
                drop(slot.playing.take());
            }
        }
    }

    pub fn audition_running(&self, id: Uuid) -> Result<bool, String> {
        let mut slot = self.audition.lock().map_err(|e| e.to_string())?;
        let Some((_, player)) = slot.playing.as_mut().filter(|(key, _)| *key == id) else {
            return Ok(false);
        };
        let running = player.running();
        if !matches!(running, Ok(true)) {
            drop(slot.playing.take());
        }
        running
    }

    /// `true` when `device_id` should play on the main stream: no device
    /// requested, or it is the device the main stream is already open on
    /// (including when "main" is the system default and the patch names that
    /// same device explicitly — no duplicate stream on one device).
    fn routes_to_main(&self, device_id: Option<&str>) -> bool {
        let Some(dev) = device_id.filter(|d| !d.is_empty()) else { return true };
        let main = self.current_device_id.lock().ok().and_then(|c| c.clone());
        let default_dev = match main {
            Some(_) => None,
            None => self
                .device_manager
                .lock()
                .ok()
                .and_then(|m| m.default_device().map(|d| d.id.clone())),
        };
        resolves_to_main_device(dev, main.as_deref(), default_dev.as_deref())
    }

    /// Play `voice` on the device an Output Patch routes to.
    ///
    /// `None` / empty / the main device → the normal main-stream path.  Any
    /// other device → a dedicated aux stream (opened on first use).  If the
    /// device cannot be opened the voice falls back to the main stream — a
    /// mis-patched cue must stay audible in a show — and a health alert tells
    /// the operator.
    pub fn play_voice_routed(&self, voice: Voice, device_id: Option<&str>) -> Result<VoiceId> {
        if voice.named_output.is_some() {
            let result = self.play_voice_named(voice, device_id);
            match &result {
                Ok(_) => crate::health::clear("output-patch-device"),
                Err(e) => crate::health::set(crate::health::HealthAlert::new(
                    "output-patch-device",
                    crate::health::HealthLevel::Error,
                    format!("Audio GO held. {e}"),
                )),
            }
            return result;
        }
        if self.routes_to_main(device_id) {
            // Routing is healthy again — retire any stale device alert so the
            // banner always matches the latest GO.
            crate::health::clear("output-patch-device");
            return self.play_voice(voice);
        }
        // routes_to_main returned false, so device_id is Some(non-empty).
        let dev = device_id.unwrap_or_default();
        match self.submit_to_aux(voice, dev, true) {
            Ok(id) => Ok(id),
            Err((voice, e)) => {
                log::warn!("[audio] patch device '{dev}' unavailable ({e}) — playing on main output");
                crate::health::set(crate::health::HealthAlert::new(
                    "output-patch-device",
                    crate::health::HealthLevel::Warning,
                    format!("Patch output device unavailable — audio routed to the main output ({e})"),
                ));
                self.play_voice(voice)
            }
        }
    }

    fn play_voice_named(&self, voice: Voice, device_id: Option<&str>) -> Result<VoiceId> {
        let requested = voice.named_output.as_ref().map(|route| route.device_id.as_str());
        let device = device_id
            .filter(|id| Some(*id) == requested)
            .ok_or_else(|| anyhow!("No matching device selected for the Output Patch."))?;
        if self.main_has_named_output(device)? {
            // Recheck the actual destination under the guard used to publish
            // the voice. The stream could be replaced after the selection above.
            self.play_voice(voice)
        } else {
            anyhow::ensure!(
                !voice.named_output.as_ref().is_some_and(NamedOutput::has_audio_effects),
                "Select this patch’s device as the named main output in Audio Preferences before using Output Patch effects."
            );
            self.submit_to_aux(voice, device, true).map_err(|(_, e)| e)
        }
    }

    fn main_has_named_output(&self, device: &str) -> Result<bool> {
        anyhow::ensure!(
            audio_host::is_named_output_id(device),
            "This backend cannot hold the patch to ‘{device}’. Choose a direct hardware output and try GO again."
        );
        let stream = self._stream.try_lock().map_err(|_| anyhow!("The audio output is changing. Try GO again."))?;
        Ok(stream.as_ref().and_then(|s| s.exact_device_id.as_deref()) == Some(device))
    }

    /// Open/check the same route that an upcoming named-output GO will use.
    pub(crate) fn check_named_output_ready(&self, route: &NamedOutput) -> Result<()> {
        if self.main_has_named_output(&route.device_id)? {
            let (stream, _) = self.ready_output()?;
            check_named_output(route, stream.as_ref().and_then(|s| s.exact_device_id.as_deref()), self.output_channels())?;
            if route.has_audio_effects() {
                self.available_patch_bus(route)?;
            }
            return Ok(());
        }
        anyhow::ensure!(
            !route.has_audio_effects(),
            "Select this patch’s device as the named main output in Audio Preferences before using Output Patch effects."
        );
        let mut aux = self.aux_streams.lock().map_err(|_| anyhow!("aux_streams mutex poisoned"))?;
        let stream = self.aux_for_route(&mut aux, &route.device_id, true)?;
        stream.named_deadline(route).map(|_| ())
    }

    /// [`Self::play_voice_paused`] with Output Patch device routing.
    /// Used for a video's audio track when its patch targets another device.
    pub fn play_voice_paused_routed(&self, voice: Voice, device_id: Option<&str>) -> Result<VoiceId> {
        if let Some(route) = &voice.named_output {
            anyhow::ensure!(device_id == Some(route.device_id.as_str()),
                "No matching device selected for the Video soundtrack’s Output Patch.");
            anyhow::ensure!(self.main_has_named_output(&route.device_id)?,
                "Select this patch’s device as the named main output before using effects on a Video soundtrack.");
            let voice = self.play_voice_paused(voice)?;
            crate::health::clear("output-patch-device");
            return Ok(voice);
        }
        if self.routes_to_main(device_id) {
            crate::health::clear("output-patch-device");
            return self.play_voice_paused(voice);
        }
        let dev = device_id.unwrap_or_default();
        match self.submit_to_aux(voice, dev, false) {
            Ok(id) => Ok(id),
            Err((voice, e)) => {
                log::warn!("[audio] patch device '{dev}' unavailable ({e}) — playing on main output");
                crate::health::set(crate::health::HealthAlert::new(
                    "output-patch-device",
                    crate::health::HealthLevel::Warning,
                    format!("Patch output device unavailable — audio routed to the main output ({e})"),
                ));
                self.play_voice_paused(voice)
            }
        }
    }

    /// Push `voice` into the aux stream for `device_id` (opening it if
    /// needed).  `start` issues the Play command; `false` submits paused.
    /// On failure the voice is handed back so the caller can fall back to
    /// the main stream.
    fn submit_to_aux(
        &self,
        voice: Voice,
        device_id: &str,
        start: bool,
    ) -> std::result::Result<VoiceId, (Voice, anyhow::Error)> {
        let mut aux = match self.aux_streams.lock() {
            Ok(g) => g,
            Err(_) => return Err((voice, anyhow!("aux_streams mutex poisoned"))),
        };

        let stream = match self.aux_for_route(&mut aux, device_id, voice.named_output.is_some()) {
            Ok(stream) => stream,
            Err(error) => return Err((voice, error)),
        };
        if let Some(route) = &voice.named_output {
            use std::sync::atomic::Ordering::Relaxed;
            let deadline = match stream.named_deadline(route) {
                Ok(deadline) => deadline,
                Err(error) => return Err((voice, error)),
            };
            voice.start_deadline_us.store(deadline, Relaxed);
        }
        self.check_channel_bounds(&voice, stream.channels);
        let id = voice.id;
        let arc = Arc::new(voice);
        if start { arc.set_playing(); } else { arc.set_paused(); }
        if let Err(arc) = stream.voices.push(arc) {
            // Poisoned aux pool — recover the (still sole-owned) Voice so the
            // caller can fall the cue back to the main output.
            let voice = Arc::try_unwrap(arc)
                .unwrap_or_else(|_| unreachable!("push failure keeps the only ref"));
            return Err((voice, anyhow!("aux voice pool poisoned")));
        }
        Ok(id)
    }

    fn aux_for_route<'a>(
        &self,
        aux: &'a mut Vec<AuxStream>,
        device_id: &str,
        exact: bool,
    ) -> Result<&'a mut AuxStream> {
        aux.retain(|s| {
            let retire = s.device_id == device_id
                && (s.failed.load(std::sync::atomic::Ordering::Relaxed)
                    || (exact && s.deadline(audio_clock_us()).is_none()));
            if retire {
                self.withdraw_named_voices(&s.voices);
            }
            !retire
        });

        if !aux.iter().any(|s| s.device_id == device_id) {
            let buffer_size = self
                .desired_config
                .lock()
                .map(|c| c.buffer_size)
                .unwrap_or(256);
            let stream = open_aux_stream(device_id, buffer_size, Arc::clone(&self.master_gain), exact)?;
            log::info!("[audio] aux output stream opened on '{device_id}'");
            crate::health::clear("output-patch-device");
            aux.push(stream);
        }
        aux.iter_mut().find(|s| s.device_id == device_id).ok_or_else(|| anyhow!("aux stream vanished"))
    }

    /// Send `cmd` to the main stream and every aux stream.  Streams that do
    /// not own the referenced voice ignore the command, so broadcasting keeps
    /// the public API device-agnostic.
    fn broadcast_command(&self, cmd: AudioCommand) -> Result<()> {
        let main = self.send_command(cmd.clone());
        if let Ok(mut aux) = self.aux_streams.lock() {
            for s in aux.iter_mut() {
                let _ = s.cmd_prod.try_push(cmd.clone());
            }
        }
        main
    }

    /// Ensure an input capture exists for `device_id` (or the default input when
    /// `None`/empty), returning the feed id to bind a Mic Cue to.
    ///
    /// Idempotent: a device is captured once and shared by all Mic Cues using it.
    /// The feed is released by [`gc_voices`](Self::gc_voices) once no live voice
    /// references it any more (so the OS mic indicator turns off after stop).
    /// Ensure a capture feed exists for `device_id`.
    ///
    /// `buffer_size` is passed to the cpal input stream (0 = OS default).
    /// Pass the same value as the output stream's configured buffer so both
    /// device clocks fire at the same period, minimising input ↔ output drift.
    pub fn ensure_input_feed(&self, device_id: Option<&str>, buffer_size: u32) -> Result<Uuid> {
        let key = device_id.unwrap_or_default().to_string();
        {
            let feeds = self.input_feeds.lock().map_err(|_| anyhow!("input_feeds poisoned"))?;
            if let Some(f) = feeds.iter().find(|f| f.device_id == key) {
                return Ok(f.id);
            }
        }
        log::info!("ensure_input_feed: opening device={device_id:?} buf={buffer_size}");
        let (capture, cons) = open_input(device_id, buffer_size).map_err(|e| {
            log::error!("ensure_input_feed failed for {device_id:?}: {e}");
            e
        })?;
        let in_channels = capture.channels.max(1) as usize;
        let id = Uuid::new_v4();
        let feed = InputFeed {
            id,
            device_id: key,
            in_channels,
            sample_rate: capture.sample_rate,
            cons,
            staging: vec![0.0_f32; STAGING_FRAMES * in_channels].into_boxed_slice(),
            write_frame: 0,
            _capture: Some(capture),
        };
        self.input_feeds
            .lock()
            .map_err(|_| anyhow!("input_feeds poisoned"))?
            .push(feed);
        Ok(id)
    }

    /// Register a synthetic input feed not backed by any capture device, and
    /// return its id plus the ring producer to push samples into.
    ///
    /// Used by the Timecode Cue's LTC generator: a background thread fills the
    /// producer with encoded LTC audio, and a live voice reading this feed routes
    /// it to an Output Patch through the normal [`Self::play_mic_voice`] path.
    /// The feed is released by [`Self::gc_voices`] once its live voice stops.
    pub fn register_synthetic_feed(
        &self,
        channels: usize,
        sample_rate: u32,
    ) -> Result<(Uuid, ringbuf::HeapProd<f32>)> {
        let channels = channels.max(1);
        let (prod, cons) = HeapRb::<f32>::new(STAGING_FRAMES * channels).split();
        let id = Uuid::new_v4();
        let feed = InputFeed {
            id,
            device_id: format!("synthetic:{id}"),
            in_channels: channels,
            sample_rate,
            cons,
            staging: vec![0.0_f32; STAGING_FRAMES * channels].into_boxed_slice(),
            write_frame: 0,
            _capture: None,
        };
        self.input_feeds
            .lock()
            .map_err(|_| anyhow!("input_feeds poisoned"))?
            .push(feed);
        Ok((id, prod))
    }

    /// Channel count and sample rate of an existing input feed.
    fn feed_info(&self, feed_id: Uuid) -> Option<(usize, u32)> {
        self.input_feeds
            .lock()
            .ok()
            .and_then(|f| f.iter().find(|f| f.id == feed_id).map(|f| (f.in_channels, f.sample_rate)))
    }

    /// Submit a live voice reading a captured or synthetic input feed.
    pub fn play_mic_voice(&self, request: LiveVoiceRequest) -> Result<VoiceId> {
        let LiveVoiceRequest {
            feed_id, inputs: [in_l, in_r], outputs: [out_l, out_r], gain, pan,
            fade_in: (fade_in_ms, fade_curve), patch_fader, patch_id, patch_slot,
            named_output, level_matrix,
        } = request;
        let (in_ch, src_rate) = self
            .feed_info(feed_id)
            .ok_or_else(|| anyhow!("input feed {feed_id:?} not found"))?;
        if named_output.is_some() {
            anyhow::ensure!(in_l < in_ch && in_r < in_ch,
                "The Mic cue’s selected input channels are unavailable.");
        }
        // Clamp requested channels to what the device offers.
        let in_l = in_l.min(in_ch.saturating_sub(1));
        let in_r = in_r.min(in_ch.saturating_sub(1));

        let live = LiveSource::new(feed_id, in_l, in_r, src_rate);
        let mut voice = Voice::new_live(live, self.sample_rate(), gain, pan);
        voice.patch_fader = patch_fader;
        voice.output_patch_id = patch_id;
        voice.patch_slot = patch_slot;
        voice.named_output = named_output;
        if let Some(matrix) = level_matrix {
            voice.inner.level_matrix.update(|_| Some(matrix));
        }
        voice.out_l = out_l;
        voice.out_r = out_r;
        // Mic routing is always explicit (Input Patch → Output Patch channels);
        // never shift it by the default ASIO pair offset.
        voice.patched = true;
        if fade_in_ms > 0 {
            let total = fade_in_ms as u64 * self.sample_rate() as u64 / 1000;
            // SAFETY: written once before the voice is shared with the callback.
            unsafe {
                *voice.inner.fade.get() = Some(FadeState {
                    direction: FadeDirection::In,
                    total_samples: total,
                    elapsed_samples: 0,
                    curve: fade_curve,
                });
            }
        }

        self.play_voice(voice)
    }

    /// Live playback-rate change on a playing voice (QLab `/live` OSC writes).
    pub fn set_voice_rate(&self, voice_id: VoiceId, rate: f32) -> Result<()> {
        self.with_voice(voice_id, |voice| voice.inner.set_rate(rate))
            .ok_or_else(|| anyhow::anyhow!("voice {voice_id:?} is not running"))
    }

    /// Current live rate of a playing voice, when it is running.
    pub fn voice_rate(&self, voice_id: VoiceId) -> Option<f32> {
        self.with_voice(voice_id, |voice| voice.inner.rate())
    }

    /// Set a single crosspoint on a playing voice (live matrix editing).
    pub fn set_voice_crosspoint(
        &self,
        voice_id: VoiceId,
        input: u8,
        output: u8,
        gain: f32,
        mirror_mono: bool,
    ) -> Result<()> {
        anyhow::ensure!(usize::from(input) < super::voice::MATRIX_INPUTS && usize::from(output) < super::voice::MATRIX_OUTPUTS, "Crosspoint exceeds 128 channels");
        self.with_voice(voice_id, |voice| voice.inner.level_matrix.update(|old| {
            let mut matrix = old.cloned().unwrap_or_default();
            matrix.mirror_mono = mirror_mono;
            matrix.set(usize::from(input), usize::from(output), gain);
            Some(matrix)
        }));
        Ok(())
    }

    /// Publish all crosspoints together at a callback boundary.
    pub fn set_voice_level_matrix(
        &self,
        voice_id: VoiceId,
        matrix: Option<&crate::engine::voice::LevelMatrix>,
    ) -> Result<()> {
        self.with_voice(voice_id, |voice| voice.inner.level_matrix.update(|_| matrix.cloned()));
        Ok(())
    }

    pub(crate) fn claim_map_motion(&self, voice: VoiceId, device: &str, owner: Uuid, start: &super::voice::LevelMatrix) -> bool {
        self.with_map_motion_voice(voice, device, |voice| voice.inner.level_matrix.claim(owner, start))
    }

    pub(crate) fn update_map_motion(&self, voice: VoiceId, device: &str, owner: Uuid, matrix: super::voice::LevelMatrix) -> bool {
        self.with_map_motion_voice(voice, device, |voice| voice.inner.level_matrix.update_owned(owner, matrix))
    }

    /// Only animate voices on the exact named main stream. The generic
    /// routing fallback and auxiliary streams cannot authorize a motion pass.
    fn with_map_motion_voice(&self, id: VoiceId, device: &str, apply: impl FnOnce(&Voice) -> bool) -> bool {
        use std::sync::atomic::Ordering::Acquire;
        let Ok(stream) = self._stream.try_lock() else { return false };
        if stream.is_none() || !self.named_output_matches(device) {
            return false;
        }
        self.voices.with(|pool| {
            let Some(voice) = pool.iter().find(|voice| voice.id == id) else { return false };
            voice.live.is_none() && voice.patched && !voice.stop_requested.load(Acquire)
                && matches!(voice.voice_state(), VoiceState::Playing | VoiceState::Paused) && apply(voice)
        }).unwrap_or(false)
    }

    /// Called with `_stream` held so the stream and route metadata agree.
    fn named_output_matches(&self, device: &str) -> bool {
        use std::sync::atomic::Ordering::Acquire;
        !self.in_fallback.load(Acquire) && !matches!(device, "" | "default" | "pipewire" | "pw:")
            && (!device.starts_with("pw:") || self.pins_node.load(Acquire))
            && self.current_device_id.lock().is_ok_and(|current| current.as_deref() == Some(device))
    }

    pub(crate) fn map_output_peaks(&self, device: &str, channels: &[u16]) -> Result<Option<Vec<f32>>, String> {
        use std::sync::atomic::Ordering::Relaxed;
        let stream = self._stream.try_lock().map_err(|_| "Audio output is reopening.")?;
        let output = stream.as_ref().filter(|_| self.named_output_matches(device))
            .ok_or("Choose the patch's named device as the main audio output to see live meters.")?;
        if !self.stream_failed.lock().is_ok_and(|flag| !flag.load(Relaxed)) {
            return Err("The main audio output is unavailable.".into());
        }
        let width = (self.output_channels() as usize).min(super::voice::MATRIX_OUTPUTS);
        if channels.is_empty() || channels.iter().any(|&channel| usize::from(channel) >= width) {
            return Err("This patch assigns channels outside the live device's output range.".into());
        }
        Ok(output.meter.read(audio_clock_us(), channels))
    }

    pub fn stop_voice(&self, voice_id: VoiceId, fade_ms: u32, fade_curve: FadeCurve) -> Result<()> {
        let stream = self._stream.try_lock().ok();
        let changing_output = stream.as_ref().is_none_or(|current| current.is_none());
        self.with_voice(voice_id, |voice| {
            voice.stop_requested.store(true, std::sync::atomic::Ordering::Release);
            if fade_ms == 0 || changing_output {
                voice.set_stopped();
            }
        });
        self.broadcast_command(AudioCommand::Stop { voice_id, fade_ms, fade_curve })
    }

    /// Panic: silence every voice immediately, bypassing per-voice IDs.
    ///
    /// One ring-buffer command per stream stops its whole pool inside a
    /// single RT callback, so it works even for voices whose owning cue lost
    /// track of them (desynced bookkeeping) and cannot overflow the command
    /// ring the way N individual `Stop`s could.  Broadcast to every aux
    /// stream so patch-routed voices are silenced too.
    pub fn panic_stop_all(&self) -> Result<()> {
        // A dead callback cannot consume StopAll. Mark the pool immediately
        // so a replacement stream cannot revive voices silenced during loss.
        self.voices.with(|pool| pool.iter().for_each(|voice| voice.set_stopped()));
        if let Ok(aux) = self.aux_streams.lock() {
            for stream in aux.iter() {
                stream.voices.with(|pool| pool.iter().for_each(|voice| voice.set_stopped()));
            }
        }
        let result = self.broadcast_command(AudioCommand::StopAll);
        self.patch_faders.clear();
        self.stop_audition(None);
        result
    }

    pub fn pause_voice(&self, voice_id: VoiceId) -> Result<()> {
        self.broadcast_command(AudioCommand::Pause { voice_id })
    }

    pub fn resume_voice(&self, voice_id: VoiceId) -> Result<()> {
        let main = self.voices.with(|pool| pool.iter().find(|voice| voice.id == voice_id).cloned()).flatten();
        if let Some(voice) = main {
            let (_stream, deadline) = self.ready_output()?;
            let previous = voice.start_deadline_us.load(std::sync::atomic::Ordering::Acquire);
            if previous != 0 && previous < audio_clock_us() {
                report_held_audio();
                anyhow::bail!(OUTPUT_START_MESSAGE);
            }
            voice.start_deadline_us.store(deadline, std::sync::atomic::Ordering::Relaxed);
            let _ = voice.state.compare_exchange(
                VoiceState::Paused as u8, VoiceState::Playing as u8,
                std::sync::atomic::Ordering::Release, std::sync::atomic::Ordering::Relaxed,
            );
            Ok(())
        } else {
            self.broadcast_command(AudioCommand::Resume { voice_id })
        }
    }

    /// Admit a held soundtrack immediately before revealing its picture.
    /// A paused load has no deadline until the picture requests playback.
    pub(crate) fn resume_required_soundtrack(&self, voice_id: VoiceId) -> Result<()> {
        use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
        let (stream, deadline) = self.ready_output()?;
        let voice = self.voices.with(|pool| pool.iter().find(|voice| voice.id == voice_id).cloned())
            .flatten().ok_or_else(|| anyhow!("The required Video soundtrack is no longer prepared."))?;
        let route = voice.named_output.as_ref().ok_or_else(|| anyhow!("The Video soundtrack has no required output."))?;
        check_named_output(route, stream.as_ref().and_then(|stream| stream.exact_device_id.as_deref()), self.output_channels())?;
        let previous = voice.start_deadline_us.load(Acquire);
        anyhow::ensure!(previous == 0 || previous >= audio_clock_us(), "{OUTPUT_START_MESSAGE}");
        if voice.voice_state() == VoiceState::Playing { return Ok(()); }
        voice.start_deadline_us.store(deadline, Relaxed);
        voice.state.compare_exchange(VoiceState::Paused as u8, VoiceState::Playing as u8, Release, Relaxed)
            .map_err(|_| anyhow!("The required Video soundtrack was withdrawn."))?;
        Ok(())
    }

    pub fn set_voice_gain(&self, voice_id: VoiceId, gain: f32) -> Result<()> {
        self.broadcast_command(AudioCommand::SetGain { voice_id, gain })
    }

    pub(crate) fn set_voice_trim(&self, voice_id: VoiceId, gain: f32) {
        let _ = self.with_voice(voice_id, |voice| {
            if let Some(trim) = &voice.inner.cue_trim {
                trim.fader.manual(gain);
            }
        });
    }

    pub(crate) fn set_voice_output_trims(&self, voice_id: VoiceId, route: &NamedOutput, gains: &[f32]) -> bool {
        self.with_voice(voice_id, |voice| {
            voice.named_output.as_ref() == Some(route)
                && matches!(voice.voice_state(), VoiceState::Playing | VoiceState::Paused | VoiceState::FadingOut)
                && voice.inner.output_trim.as_ref().is_some_and(|trim| trim.set(gains))
        }).unwrap_or(false)
    }

    pub(crate) fn named_voice_output(&self, voice: VoiceId) -> Option<NamedOutput> {
        self.with_voice(voice, |voice| voice.named_output.clone()).flatten()
    }

    pub fn set_voice_pan(&self, voice_id: VoiceId, pan: f32) -> Result<()> {
        self.broadcast_command(AudioCommand::SetPan { voice_id, pan })
    }

    /// Find an existing PCM voice's rate control on the main or an auxiliary output.
    pub fn voice_rate_fader(&self, voice_id: VoiceId) -> Option<Arc<RateFader>> {
        self.with_voice(voice_id, |voice| {
            (voice.live.is_none() && voice.voice_state() != VoiceState::Stopped)
                .then(|| Arc::clone(&voice.inner.rate_fader))
        }).flatten()
    }

    pub fn voice_cutoff_fader(&self, voice_id: VoiceId, filter_id: Uuid) -> Option<Arc<super::rate_fader::ScalarFader>> {
        self.voice_effect_fader(voice_id, filter_id, |voice| &voice.inner.cutoff_controls)
    }

    pub fn voice_eq_gain_fader(&self, voice_id: VoiceId, filter_id: Uuid) -> Option<Arc<super::rate_fader::ScalarFader>> {
        self.voice_effect_fader(voice_id, filter_id, |voice| &voice.inner.eq_gain_controls)
    }

    fn voice_effect_fader(
        &self, voice_id: VoiceId, filter_id: Uuid,
        controls: fn(&Voice) -> &[(Uuid, Arc<super::rate_fader::ScalarFader>)],
    ) -> Option<Arc<super::rate_fader::ScalarFader>> {
        self.with_voice(voice_id, |voice| {
            if !matches!(voice.voice_state(), VoiceState::Playing | VoiceState::Paused | VoiceState::FadingOut) {
                return None;
            }
            controls(voice).iter().find(|(id, _)| *id == filter_id)
                .map(|(_, fader)| Arc::clone(fader))
        }).flatten()
    }

    /// Edit the natural fade-out of a currently loaded PCM voice.
    pub fn set_voice_natural_fade_out(&self, voice_id: VoiceId, duration_ms: u32, curve: FadeCurve) {
        let _ = self.with_voice(voice_id, |voice| voice.inner.set_natural_fade_out(duration_ms, curve));
    }

    /// Estimated remaining wall-clock milliseconds if the current rate is held.
    /// Used for ordinary Audio clips, where all future loop bounds are known.
    pub fn voice_remaining_ms(&self, voice_id: VoiceId) -> Option<u64> {
        self.with_voice(voice_id, |voice| {
            use std::sync::atomic::Ordering::{Acquire, Relaxed};
            let loops = if voice.whole_loop_release.as_ref().is_some_and(|request| request.load(Acquire)) {
                0
            } else {
                voice.inner.loops_remaining.load(Relaxed)
            };
            if loops == u32::MAX {
                return None;
            }
            let end = voice.end_frame();
            let frames = end.saturating_sub(voice.current_frame()) as f64
                + end.saturating_sub(voice.loop_start_frame) as f64 * f64::from(loops);
            let frames_per_second = f64::from(voice.sample_rate) * f64::from(voice.inner.rate());
            Some((frames * 1000.0 / frames_per_second) as u64)
        }).flatten()
    }

    /// Seek a voice to the given decoded-audio frame position.
    pub fn seek_voice(&self, voice_id: VoiceId, frame_pos: u64) -> Result<()> {
        self.broadcast_command(AudioCommand::Seek { voice_id, frame_pos })
    }



    /// Devamp: release the voice's current slice loop.  The pass in progress
    /// finishes, then playback continues into the next slice — or stops at the
    /// slice boundary when `stop_at_end` is set.  No-op on unsliced voices.
    pub fn devamp_voice(&self, voice_id: VoiceId, stop_at_end: bool) -> Result<()> {
        self.broadcast_command(AudioCommand::Devamp { voice_id, stop_at_end })
    }

    /// Arm a release on one exact, already-rendered File Audio playback.
    /// The mixer reads this lease at its whole-clip boundary on the owning stream.
    pub(crate) fn release_whole_audio_loop(&self, voice_id: VoiceId) -> bool {
        use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
        self.with_voice(voice_id, |voice| {
            let Some(request) = &voice.whole_loop_release else { return false; };
            if voice.voice_state() != VoiceState::Playing
                || voice.stop_requested.load(Acquire)
                || voice.start_deadline_us.load(Acquire) != 0
                || voice.inner.loops_remaining.load(Relaxed) == 0
            {
                return false;
            }
            request.store(true, Release);
            true
        }).unwrap_or(false)
    }

    /// Latest live output levels of a voice as linear amplitudes:
    /// `([peak_l, peak_r], [rms_l, rms_r])`, updated by the RT callback each
    /// block.  `None` when the voice is unknown; zeros when silent.
    pub fn voice_levels(&self, voice_id: VoiceId) -> Option<([f32; 2], [f32; 2])> {
        use std::sync::atomic::Ordering::Relaxed;
        self.with_voice(voice_id, |voice| {
            let peak = [
                f32::from_bits(voice.inner.live_peak[0].load(Relaxed)),
                f32::from_bits(voice.inner.live_peak[1].load(Relaxed)),
            ];
            let rms = [
                f32::from_bits(voice.inner.live_rms[0].load(Relaxed)),
                f32::from_bits(voice.inner.live_rms[1].load(Relaxed)),
            ];
            (peak, rms)
        })
    }

    /// Current playback position of a voice in **file time** (ms).
    ///
    /// Reads the RT frame cursor, so it reflects loops and slice jumps —
    /// unlike a cue's wall-clock elapsed.  Used for UI time displays.
    pub fn voice_position_ms(&self, voice_id: VoiceId) -> Option<u64> {
        self.with_voice(voice_id, |v| {
            v.current_frame() * 1000 / v.sample_rate.max(1) as u64
        })
    }

    /// Close every aux stream and clear routing alerts.
    ///
    /// Called on any event that changes the routing universe — main device /
    /// backend change, Output Patch edits — so no stream keeps playing on an
    /// output the operator just re-configured away from, and no stale banner
    /// contradicts the new reality.  The next GO reopens what it needs.
    pub fn close_all_aux(&self) {
        if let Ok(mut aux) = self.aux_streams.lock() {
            if !aux.is_empty() {
                log::info!("[audio] closing {} aux output stream(s) (routing changed)", aux.len());
            }
            for stream in aux.iter() {
                self.withdraw_named_voices(&stream.voices);
            }
            aux.clear();
        }
        crate::health::clear("output-patch-device");
        crate::health::clear("output-patch-channels");
    }

    /// Raise or clear the "patch routes to a channel the device does not
    /// have" alert for one submitted voice.  Reflects the **latest** GO, so
    /// the banner always matches what the operator just heard (or didn't).
    fn check_channel_bounds(&self, voice: &Voice, device_channels: u32) {
        let highest = voice.out_l.max(voice.out_r);
        if highest >= device_channels as usize {
            crate::health::set(crate::health::HealthAlert::new(
                "output-patch-channels",
                crate::health::HealthLevel::Warning,
                format!(
                    "Cue routed to output channel {} but the device has only {} — audio is dropped. Fix the Output Patch channels.",
                    highest + 1,
                    device_channels
                ),
            ));
        } else {
            crate::health::clear("output-patch-channels");
        }
    }

    /// Run `f` on the voice with `voice_id`, searching the main pool then
    /// every aux pool.  Returns `None` when the voice is not found.
    fn with_voice<T>(&self, voice_id: VoiceId, f: impl Fn(&Arc<Voice>) -> T) -> Option<T> {
        let find = |pool: &[Arc<Voice>]| pool.iter().find(|v| v.id == voice_id).map(&f);
        if let Some(Some(t)) = self.voices.with(find) {
            return Some(t);
        }
        let aux = self.aux_streams.lock().ok()?;
        for s in aux.iter() {
            if let Some(Some(t)) = s.voices.with(find) {
                return Some(t);
            }
        }
        None
    }

    /// Read the current linear gain of a voice.
    /// Returns 1.0 if the voice is not found.
    pub fn get_voice_gain(&self, voice_id: VoiceId) -> f32 {
        self.with_voice(voice_id, |v| v.inner.gain()).unwrap_or(1.0)
    }

    /// Read the current stereo pan of a voice (-1 = left, 0 = center, +1 = right).
    /// Returns 0.0 (center) if the voice is not found.
    pub fn get_voice_pan(&self, voice_id: VoiceId) -> f32 {
        self.with_voice(voice_id, |v| v.inner.pan()).unwrap_or(0.0)
    }

    /// Seek a voice to the given position in milliseconds.
    ///
    /// Looks up the voice's decoded sample rate to convert `position_ms` into a
    /// frame position, then sends a [`AudioCommand::Seek`] to the RT callback.
    /// Used by [`OutputEngine::seek`] which only knows wall-clock position.
    pub fn seek_voice_ms(&self, voice_id: VoiceId, position_ms: u64) -> Result<()> {
        let frame_pos = self.with_voice(voice_id, |v| position_ms * v.sample_rate as u64 / 1000);
        if let Some(fp) = frame_pos {
            self.broadcast_command(AudioCommand::Seek { voice_id, frame_pos: fp })?;
        }
        Ok(())
    }

    /// Drain the status ring buffers (main + aux) and return all pending
    /// status messages.
    pub fn drain_status(&self) -> Vec<AudioStatus> {
        let mut out = self.named_output_withdrawals.lock()
            .map(|mut statuses| std::mem::take(&mut *statuses))
            .unwrap_or_default();
        if let Ok(mut cons) = self.status_cons.lock() {
            while let Some(s) = cons.try_pop() {
                out.push(s);
            }
        }
        if let Ok(mut aux) = self.aux_streams.lock() {
            for s in aux.iter_mut() {
                while let Some(msg) = s.status_cons.try_pop() {
                    // Aux master levels pass through tagged with their device,
                    // so the mixer can meter each auxiliary output separately.
                    match msg {
                        AudioStatus::MasterLevels { peak_l, peak_r } => out.push(AudioStatus::AuxMasterLevels {
                            device_id: s.device_id.clone(),
                            peak_l,
                            peak_r,
                        }),
                        other => out.push(other),
                    }
                }
                if s.deadline(audio_clock_us()).is_none() {
                    s.voices.withdraw_named_voices(&mut out);
                }
            }
        }
        out
    }

    /// Remove fully-stopped voices from the pool, and close any input capture
    /// device that no live voice still references (so the OS releases the mic
    /// once its Mic Cue stops — the device indicator turns off).
    ///
    /// Locks `voices` then `input_feeds`, matching the RT callback's order so the
    /// two never deadlock.
    pub fn gc_voices(&self) {
        self.voices
            .retain(|v| !matches!(v.voice_state(), VoiceState::Stopped | VoiceState::Idle));

        // Feed ids still referenced by a surviving live voice.
        let in_use: std::collections::HashSet<Uuid> = self
            .voices
            .with(|pool| {
                pool.iter().filter_map(|v| v.live.as_ref().map(|l| l.feed_id)).collect()
            })
            .unwrap_or_default();

        // Drop unreferenced feeds — dropping an InputFeed drops its cpal input
        // stream, releasing the device.
        if let Ok(mut feeds) = self.input_feeds.lock() {
            feeds.retain(|f| in_use.contains(&f.id));
        }

        // Aux pools: same sweep.  A healthy aux stream stays open even when
        // idle (zero device-open latency on the next GO); a failed one is
        // dropped once empty so the next GO retries a fresh open.
        if let Ok(mut aux) = self.aux_streams.lock() {
            for s in aux.iter() {
                s.voices
                    .retain(|v| !matches!(v.voice_state(), VoiceState::Stopped | VoiceState::Idle));
            }
            aux.retain(|s| {
                let empty = s.voices.with(|p| p.is_empty()).unwrap_or(true);
                let failed = s.failed.load(std::sync::atomic::Ordering::Relaxed);
                !(failed && empty)
            });
        }
    }

    /// Stop the current stream and re-open according to `config`.
    ///
    /// Playing voices are **preserved**: the voice pool is shared with the new
    /// stream's callback, so each voice resumes from its current `frame_pos` on
    /// the new device (the source-frame cursor is output-rate-independent, and
    /// channel routing is bounds-checked in `fill_buffer`).  A device switch
    /// therefore only drops audio for the brief teardown/re-open gap instead of
    /// killing the running cue — for both a planned change and an auto-fallback.
    pub fn restart(&self, config: &MachineAudioConfig) -> Result<()> {
        // Serialize the whole route switch with audition starts. A failed reopen
        // leaves the live device unknown until a complete restart succeeds.
        let mut audition = self.audition.lock().map_err(|_| anyhow!("audition mutex poisoned"))?;
        *audition = AuditionSlot::default();
        self.pins_node.store(false, std::sync::atomic::Ordering::Relaxed);
        // Drop the old stream before opening the new one (exclusive backends
        // require the device to be released first).  Voices are intentionally
        // left intact so the new stream continues mixing them where they froze.
        {
            let mut sg = self._stream.lock().map_err(|_| anyhow!("stream mutex poisoned"))?;
            *sg = None;
            self.output_callbacks.store(0, std::sync::atomic::Ordering::Relaxed);
            self.withdraw_named_voices(&self.voices);
            // Never transfer an unrendered GO to a replacement stream. Its
            // first callback will withdraw the cue without advancing a follow.
            self.voices.with(|pool| {
                for voice in pool {
                    if voice.named_output.is_none() {
                        voice.prepare_for_output_restart();
                    }
                }
            });
        }

        // Close ALL aux streams: they belong to the previous device universe
        // (a WASAPI aux on the interface ASIO is about to grab exclusively
        // would make this restart fail), and stale ones would keep playing on
        // outputs the operator just re-configured away from.  The next GO
        // through a patch reopens exactly what the new universe needs.
        self.close_all_aux();

        let new_failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sr = open_stream_inner(
            config,
            self.voices.rt_handle(),
            Arc::clone(&self.input_feeds),
            Arc::clone(&self.master_gain),
            Arc::clone(&self.output_period),
            Arc::clone(&new_failed),
            Arc::clone(&self.output_callbacks),
            false,
        )?;

        *self.cmd_prod.lock().map_err(|_| anyhow!("cmd_prod poisoned"))? = sr.cmd_prod;
        *self.status_cons.lock().map_err(|_| anyhow!("status_cons poisoned"))? = sr.status_cons;
        let mut stream = self._stream.lock().map_err(|_| anyhow!("stream poisoned"))?;
        self.sample_rate.store(sr.sample_rate, std::sync::atomic::Ordering::Relaxed);
        self.output_channels.store(sr.channels, std::sync::atomic::Ordering::Relaxed);
        self.default_out_offset.store(sr.default_out_offset, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut f) = self.stream_failed.lock() { *f = new_failed; }
        if let Ok(mut d) = self.current_device_id.lock() { *d = config.device_id.clone(); }
        self.pins_node.store(sr.pins_node, std::sync::atomic::Ordering::Relaxed);
        // Route-bound features must see a stream and its own metadata together.
        *stream = Some(MainOutput { _stream: sr.stream, meter: sr.meter, exact_device_id: sr.exact_device_id });

        // The device set does not change on a restart (same hardware, different
        // selection), so we do not re-enumerate here — that would add a slow,
        // possibly hanging WASAPI query to this main-thread path.  The cache is
        // kept fresh by the startup warm-up and every Preferences enumeration.

        Ok(())
    }

    fn send_command(&self, cmd: AudioCommand) -> Result<()> {
        self.cmd_prod
            .lock()
            .map_err(|_| anyhow!("cmd_prod mutex poisoned"))?
            .try_push(cmd)
            .map_err(|_| anyhow!("Audio command ring buffer full"))
    }
}

pub struct LiveVoiceRequest {
    pub feed_id: Uuid,
    pub inputs: [usize; 2],
    pub outputs: [usize; 2],
    pub gain: f32,
    pub pan: f32,
    pub fade_in: (u32, FadeCurve),
    pub patch_fader: Option<Arc<super::patch_fader::PatchFader>>,
    pub patch_id: Option<Uuid>,
    pub patch_slot: Option<u8>,
    pub named_output: Option<NamedOutput>,
    pub level_matrix: Option<super::voice::LevelMatrix>,
}

// ---------------------------------------------------------------------------
// Stream builder — shared between new() and restart()
// ---------------------------------------------------------------------------

struct StreamResult {
    stream: Stream,
    meter: Arc<OutputMeter>,
    sample_rate: u32,
    channels: u32,
    /// Default output-channel offset for unpatched voices (the selected ASIO
    /// pair × 2; always 0 on non-ASIO backends).
    default_out_offset: u32,
    cmd_prod: ringbuf::HeapProd<AudioCommand>,
    status_cons: ringbuf::HeapCons<AudioStatus>,
    /// Opened on one PipeWire node (see [`AudioEngine::pinned_node_vanished`]).
    pins_node: bool,
    exact_device_id: Option<String>,
}

fn check_named_output(route: &NamedOutput, opened: Option<&str>, channels: u32) -> Result<()> {
    anyhow::ensure!(opened == Some(route.device_id.as_str()),
        "Patch output ‘{}’ is unavailable. Reconnect it or select it in Audio Preferences, then try GO again.", route.device_id);
    let required_channels = route.required_channel_count();
    anyhow::ensure!(required_channels <= channels,
        "Patch output ‘{}’ needs channel {}, but the device opened with {}. Check the Output Patch channels.",
        route.device_id, required_channels, channels);
    Ok(())
}

fn active_voice(voice: &Voice) -> bool {
    matches!(voice.voice_state(), VoiceState::Playing | VoiceState::Paused | VoiceState::FadingOut)
}

/// Select device, configure buffer, build and start the cpal stream.
///
/// Creates fresh ring buffers and returns them alongside the running stream so
/// the caller can wire them into the engine (either for initial construction or
/// after a restart).
fn open_stream_inner(
    config: &MachineAudioConfig,
    cb_voices: Arc<ArcSwap<Vec<Arc<Voice>>>>,
    cb_feeds: Arc<Mutex<Vec<InputFeed>>>,
    cb_mg: Arc<std::sync::atomic::AtomicU32>,
    cb_period: Arc<std::sync::atomic::AtomicU32>,
    stream_failed: Arc<std::sync::atomic::AtomicBool>,
    cb_callbacks: Arc<std::sync::atomic::AtomicU64>,
    require_named_device: bool,
) -> Result<StreamResult> {
    use crate::preferences::AudioBackend;

    let device_name = config.device_id.as_deref().filter(|s| !s.is_empty());

    // Linux: the native PipeWire host when its daemon answers, else ALSA
    // (`audio_host`).  `_host` stays alive until the stream is built.
    let resolved = if matches!(config.backend, AudioBackend::Asio) {
        resolve_asio_device(device_name)?
    } else {
        audio_host::output_device(device_name)?
    };
    let exact_device_id = resolved.resolved_named_id(device_name);
    if require_named_device {
        anyhow::ensure!(exact_device_id.is_some(),
            "The patch device ‘{}’ could not be opened exactly. Select a named device and try GO again.",
            device_name.unwrap_or_default());
    }
    let Resolved { host: _host, device, kind: host_kind } = resolved;

    let default_config = device.default_output_config()
        .map_err(|e| anyhow!("Device config error: {e}"))?;

    // The ALSA name the stream opens (`None` = ALSA `default`), for the
    // width and quantum decisions below.
    #[cfg(target_os = "linux")]
    let alsa_name = match audio_host::route(device_name, host_kind) {
        (_, audio_host::DeviceTarget::Id(name)) => Some(name),
        (_, audio_host::DeviceTarget::Default) => None,
    };

    // ALSA has no native default format and cpal's fallback ranks stereo
    // first, so a PipeWire node or interface with more outs would open as
    // two channels and every Output Patch past output 2 would be dropped.
    // Open at the target's real width instead.  (The native PipeWire host
    // already reports a node's own channel count.)
    #[cfg(target_os = "linux")]
    let default_config = if host_kind == audio_host::HostKind::Alsa {
        use crate::engine::device_manager::{pipewire_node_channels, pipewire_node_of};
        let width = match device_name.and_then(pipewire_node_of) {
            Some(node) => pipewire_node_channels(node).map(ChannelWidth::Exactly),
            None if alsa_name.is_some_and(|n| n.starts_with("hw:")) => Some(ChannelWidth::Widest),
            None => None,
        };
        width
            .and_then(|w| {
                let ranges = device.supported_output_configs().ok()?;
                wider_output_config(&default_config, ranges, w)
            })
            .unwrap_or(default_config)
    } else {
        default_config
    };

    let sample_rate = default_config.sample_rate();
    let channels = default_config.channels();
    let total_ch = channels as usize;

    // Buffer size: apply Fixed on all backends except ASIO (which uses its own
    // control panel) and WASAPI Shared (where Windows owns the engine period and
    // ignores the hint).  This makes the user-configured buffer_size effective on
    // macOS (CoreAudio) and Linux (ALSA/PipeWire) — previously they always got
    // the OS default (typically 256-1024 samples), causing high Mic Cue latency
    // even when the operator had set 64 samples in Preferences.
    //
    // On the native PipeWire host the period is only a latency *request*
    // (`node.latency`): the graph runs at its own quantum (forced or not) and
    // the callback simply receives a quantum's worth of frames.
    let period = config.buffer_size;
    // Through PipeWire's ALSA plugin the period must not be shorter than a
    // graph cycle: cpal keeps only two periods buffered, so with a forced
    // quantum above the configured buffer the plugin underruns on every cycle.
    #[cfg(target_os = "linux")]
    let period = match crate::engine::device_manager::pipewire_quantum_floor() {
        Some(floor) if host_kind == audio_host::HostKind::Alsa && routes_via_pipewire(alsa_name) && floor > period => {
            log::info!("PipeWire quantum floor {floor} > buffer {period}: raising the period");
            floor
        }
        _ => period,
    };
    let period = clamp_period(period, default_config.buffer_size());
    let buf_size = match config.backend {
        AudioBackend::Asio | AudioBackend::WasapiShared => cpal::BufferSize::Default,
        _ => cpal::BufferSize::Fixed(period),
    };

    let stream_cfg = StreamConfig {
        channels,
        sample_rate,
        buffer_size: buf_size,
    };

    // ASIO: default output pair for voices that have no Output Patch.
    // Applied at voice submission (play_voice), not in the callback.
    let pair_offset = if matches!(config.backend, AudioBackend::Asio) {
        (config.asio_out_pair as usize * 2).min(total_ch.saturating_sub(2))
    } else {
        0
    };

    let (cmd_prod, mut cmd_cons) = HeapRb::<AudioCommand>::new(RING_CAPACITY).split();
    let (mut status_prod, status_cons) = HeapRb::<AudioStatus>::new(RING_CAPACITY).split();

    // Throttled error callback: log at most once per second, signal stream_failed
    // on the first DeviceNotAvailable so the watchdog can switch devices quickly.
    let make_err_fn = {
        let last_log  = Arc::new(std::sync::atomic::AtomicU64::new(0));
        move || {
            let last_log2  = Arc::clone(&last_log);
            let failed2    = Arc::clone(&stream_failed);
            move |err: cpal::Error| {
                if stream_error_is_fatal(err.kind()) {
                    failed2.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let prev = last_log2.swap(now, std::sync::atomic::Ordering::Relaxed);
                if now > prev {
                    log::error!("cpal stream error ({:?}): {err}", err.kind());
                }
            }
        }
    };

    // Both formats mix **full-width** (all device channels) so Output Patch
    // channel routing reaches every physical output — essential on ASIO where
    // one driver exposes all of the interface's outs.  Unpatched voices are
    // shifted to `pair_offset` at submission (see `play_voice`), not here.
    let meter = Arc::new(OutputMeter::new());
    let cb_meter = Arc::clone(&meter);
    let stream = match default_config.sample_format() {
        cpal::SampleFormat::F32 => device.build_output_stream(
            stream_cfg,
            move |data: &mut [f32], _| {
                let callback_at = audio_clock_us();
                cb_callbacks.store(callback_at, std::sync::atomic::Ordering::Relaxed);
                cb_period.store((data.len() / total_ch) as u32, std::sync::atomic::Ordering::Relaxed);
                fill_buffer(callback_at, data, total_ch, sample_rate, &cb_voices, &cb_feeds, &mut cmd_cons, &mut status_prod, &cb_mg, &cb_period);
                cb_meter.observe(callback_at, data, total_ch);
            },
            make_err_fn(),
            None,
        )?,
        cpal::SampleFormat::I32 => {
            // Pre-allocate a full-width f32 scratch — no alloc inside the RT
            // callback.  A period larger than the scratch is mixed in
            // scratch-sized chunks rather than left partly unwritten.
            let scratch_len = (period as usize).max(4096) * total_ch;
            let mut scratch = vec![0.0f32; scratch_len].into_boxed_slice();
            device.build_output_stream(
                stream_cfg,
                move |data: &mut [i32], _| {
                    let callback_at = audio_clock_us();
                    cb_callbacks.store(callback_at, std::sync::atomic::Ordering::Relaxed);
                    let frames = data.len() / total_ch;
                    cb_period.store(frames as u32, std::sync::atomic::Ordering::Relaxed);
                    for chunk in data.chunks_mut(scratch.len()) {
                        let n = chunk.len();
                        fill_buffer(callback_at, &mut scratch[..n], total_ch, sample_rate, &cb_voices, &cb_feeds, &mut cmd_cons, &mut status_prod, &cb_mg, &cb_period);
                        cb_meter.observe(callback_at, &scratch[..n], total_ch);
                        for (dst, src) in chunk.iter_mut().zip(scratch[..n].iter()) {
                            *dst = (src.clamp(-1.0, 1.0) * i32::MAX as f32) as i32;
                        }
                    }
                },
                make_err_fn(),
                None,
            )?
        }
        fmt => return Err(anyhow!("Unsupported sample format: {fmt:?}")),
    };

    stream.play()?;
    log::info!(
        "Audio stream opened — backend={:?} device={:?} rate={}Hz channels={} buf={:?}",
        config.backend,
        config.device_id,
        sample_rate,
        channels,
        buf_size,
    );

    Ok(StreamResult {
        stream,
        meter,
        sample_rate,
        channels: channels as u32,
        default_out_offset: pair_offset as u32,
        cmd_prod,
        status_cons,
        pins_node: audio_host::pins_pipewire_node(host_kind, device_name),
        exact_device_id,
    })
}

/// Resolve the ASIO driver `device_id` (or the first one).  Without ASIO
/// compiled in (e.g. `pnpm tauri dev` without `--features asio-support`),
/// fall back to the system host so developers can iterate while their saved
/// machine config keeps pointing at ASIO for production builds.
fn resolve_asio_device(device_id: Option<&str>) -> Result<Resolved> {
    let host = match open_asio_host() {
        Ok(h) => h,
        Err(e) => {
            log::warn!("ASIO unavailable ({e}), falling back to default host");
            return audio_host::output_device(device_id).or_else(|_| audio_host::output_device(None));
        }
    };
    let found = device_id.and_then(|name| {
        host.output_devices()
            .ok()
            .and_then(|mut it| it.find(|d| d.id().ok().is_some_and(|id| id.id() == name)))
    });
    let device = found
        .or_else(|| host.default_output_device())
        .ok_or_else(|| anyhow!(
            "No ASIO device found. Make sure the driver is not already in use by another application."
        ))?;
    Ok(Resolved { host, device, kind: audio_host::HostKind::System })
}

/// Does a stream error leave the stream dead, so the watchdog must reopen it?
///
/// DeviceNotAvailable is (device pulled or exclusively grabbed).  Other kinds
/// cover recoverable ALSA errors (e.g. Xrun, POLLERR) that should not trigger
/// the watchdog restart — except, on Linux, StreamInvalidated: the native
/// PipeWire host reports a stream that entered its error state or lost its
/// negotiated format with it, and stops that stream for good.  (cpal's ALSA
/// host never emits it.)
fn stream_error_is_fatal(kind: cpal::ErrorKind) -> bool {
    kind == cpal::ErrorKind::DeviceNotAvailable
        || (cfg!(target_os = "linux") && kind == cpal::ErrorKind::StreamInvalidated)
}

/// Clamp a requested period into the device's supported range.  A range
/// with `max == 0` is the PipeWire host before the server clock settings
/// arrived: no bounds are known, so the request stands.
fn clamp_period(period: u32, supported: &cpal::SupportedBufferSize) -> u32 {
    match supported {
        cpal::SupportedBufferSize::Range { min, max } if *max > 0 => period.clamp(*min, (*max).max(*min)),
        _ => period,
    }
}

/// How wide to open an output stream, when the device default is too narrow.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelWidth {
    /// The target's known channel count (a PipeWire node's `audio.channels`).
    Exactly(u16),
    /// The most channels the device offers (a raw `hw:` ALSA device).
    Widest,
}

/// A config wider than `default` among `ranges`, at the same sample rate and
/// in a sample format the callback mixes (F32 / I32, the default's first).
///
/// `None` when nothing wider fits, so the caller keeps the default.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn wider_output_config(
    default: &cpal::SupportedStreamConfig,
    ranges: impl IntoIterator<Item = cpal::SupportedStreamConfigRange>,
    width: ChannelWidth,
) -> Option<cpal::SupportedStreamConfig> {
    let rate = default.sample_rate();
    let format_rank = |f: cpal::SampleFormat| {
        if f == default.sample_format() {
            2
        } else if matches!(f, cpal::SampleFormat::F32 | cpal::SampleFormat::I32) {
            1
        } else {
            0
        }
    };
    ranges
        .into_iter()
        .filter(|r| r.channels() > default.channels() && r.contains_rate(rate))
        .filter(|r| format_rank(r.sample_format()) > 0)
        .filter(|r| match width {
            ChannelWidth::Exactly(n) => r.channels() == n,
            ChannelWidth::Widest => true,
        })
        .max_by_key(|r| (r.channels(), format_rank(r.sample_format())))
        .map(|r| r.with_sample_rate(rate))
}

/// `true` when the ALSA PCM `name` is served by PipeWire's ALSA plugin: the
/// `pipewire` PCM itself (used for `pw:` nodes) or the system default, which
/// pipewire-alsa takes over when PipeWire runs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn routes_via_pipewire(name: Option<&str>) -> bool {
    matches!(name, None | Some("pipewire") | Some("default"))
}

/// Pure routing decision: does an explicitly-requested patch device match the
/// device the main stream is open on?  `main` is the main stream's device id
/// (`None` = system default, compared via `default_dev`).
fn resolves_to_main_device(requested: &str, main: Option<&str>, default_dev: Option<&str>) -> bool {
    match main {
        Some(cur) => cur == requested,
        None => default_dev == Some(requested),
    }
}

/// Open an additional output stream on `device_id` for Output Patch routing.
///
/// Reuses [`open_stream_inner`] with a synthetic config: generic default host
/// (WASAPI shared on Windows, CoreAudio on macOS, ALSA/PipeWire on Linux — no
/// ASIO, which is exclusive and single-device by design), fresh voice pool,
/// permanently empty input-feed list (live voices stay on the main stream),
/// and the shared master gain so the master fader applies everywhere.
fn open_aux_stream(
    device_id: &str,
    buffer_size: u32,
    master_gain: Arc<std::sync::atomic::AtomicU32>,
    require_named_device: bool,
) -> Result<AuxStream> {
    use crate::preferences::AudioBackend;

    // WasapiShared on Windows selects BufferSize::Default (WASAPI shared mode
    // owns its engine period); SystemDefault elsewhere applies the configured
    // buffer like the main stream does on CoreAudio/ALSA.
    #[cfg(windows)]
    let backend = AudioBackend::WasapiShared;
    #[cfg(not(windows))]
    let backend = AudioBackend::SystemDefault;

    let config = MachineAudioConfig {
        backend,
        device_id: Some(device_id.to_string()),
        device_name: None,
        input_device_id: None,
        audition_device_id: None,
        audition_device_name: None,
        buffer_size,
        asio_out_pair: 0,
    };

    let voices = VoicePool::new();
    let feeds: Arc<Mutex<Vec<InputFeed>>> = Arc::new(Mutex::new(Vec::new()));
    let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let last_callback_us = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let period = Arc::new(std::sync::atomic::AtomicU32::new(buffer_size));

    let sr = open_stream_inner(
        &config,
        voices.rt_handle(),
        feeds,
        master_gain,
        Arc::clone(&period),
        Arc::clone(&failed),
        Arc::clone(&last_callback_us),
        require_named_device,
    )?;

    Ok(AuxStream {
        device_id: device_id.to_string(),
        exact_device_id: sr.exact_device_id,
        channels: sr.channels,
        voices,
        cmd_prod: sr.cmd_prod,
        status_cons: sr.status_cons,
        failed,
        last_callback_us,
        period,
        sample_rate: sr.sample_rate,
        opened_at_us: audio_clock_us(),
        _stream: sr.stream,
    })
}

// ---------------------------------------------------------------------------
// Audio callback — real-time safe
// ---------------------------------------------------------------------------

struct ActivePatchBlock<'a> {
    bus: &'a PatchBus,
    block: &'a mut PatchBusState,
    fits: bool,
    rendered: bool,
}

#[allow(clippy::too_many_arguments)]
fn fill_buffer(
    block_started_at: u64,
    output: &mut [f32],
    channels: usize,
    output_sample_rate: u32,
    voices: &ArcSwap<Vec<Arc<Voice>>>,
    input_feeds: &Arc<Mutex<Vec<InputFeed>>>,
    cmd_cons: &mut ringbuf::HeapCons<AudioCommand>,
    status_prod: &mut ringbuf::HeapProd<AudioStatus>,
    master_gain: &Arc<std::sync::atomic::AtomicU32>,
    output_period: &Arc<std::sync::atomic::AtomicU32>,
) {
    let start_epoch = super::voice::AudioStartBatch::current_epoch();
    output.fill(0.0);

    // Wait-free snapshot of the voice list — no lock, so the callback can never
    // be starved into emitting a block of silence (the old `try_lock` failure
    // mode). The producer keeps retired snapshots that still have readers so
    // dropping this guard never runs a destructor on the RT thread.
    let voices_guard = voices.load();

    // Process incoming commands first.
    while let Some(cmd) = cmd_cons.try_pop() {
        apply_command(&voices_guard, cmd, status_prod);
    }

    // Keep live input feeds current: drain each device's ring into its staging
    // buffer (cheap, bounded) so live voices have fresh audio and the input
    // stays "warm" even with no Mic Cue playing.  Held for the voice loop.
    let mut feeds_guard = input_feeds.try_lock().ok();
    if let Some(feeds) = feeds_guard.as_deref_mut() {
        for feed in feeds.iter_mut() {
            feed.drain();
        }
    }

    let master = f32::from_bits(
        master_gain.load(std::sync::atomic::Ordering::Relaxed),
    );

    let frames = output.len() / channels;
    let mut peak_l = 0.0_f32;
    let mut peak_r = 0.0_f32;
    // Per-Output-Patch peaks, indexed by Voice::patch_slot (fixed-size — no
    // allocation in the RT callback).
    let mut patch_peaks = [0.0_f32; PATCH_VU_SLOTS * 2];
    let mut patch_blocks: [Option<ActivePatchBlock<'_>>; MAX_PROCESSED_PATCHES] = [const { None }; MAX_PROCESSED_PATCHES];

    for voice in voices_guard.iter() {
        let deadline = voice.start_deadline_us.load(std::sync::atomic::Ordering::Acquire);
        if deadline != 0 && block_started_at > deadline {
            if let Some(cut) = &voice.playlist_cut { cut.cancel_pending(); }
            voice.start_deadline_us.store(0, std::sync::atomic::Ordering::Relaxed);
            voice.set_stopped();
            let status = if voice.named_output.is_some() {
                AudioStatus::OutputWithdrawn { voice_id: voice.id }
            } else {
                AudioStatus::StartExpired { voice_id: voice.id }
            };
            let _ = status_prod.try_push(status);
            continue;
        }
        let state = voice.voice_state();
        if let Some(cut) = &voice.playlist_cut {
            if cut.pending() && (state != VoiceState::Playing || voice.stop_requested.load(std::sync::atomic::Ordering::Acquire)
                || voice.out_l.max(voice.out_r) >= channels || voice.current_frame() >= voice.total_frames())
            {
                cut.cancel_pending();
                voice.set_stopped();
                continue;
            }
        }
        let patch_block = match state {
            VoiceState::Playing | VoiceState::Paused | VoiceState::FadingOut => voice.patch_effects.as_deref()
                .and_then(|bus| {
                    patch_blocks.iter_mut()
                        .find(|slot| slot.as_ref().is_none_or(|active| std::ptr::eq(active.bus, bus)))
                        .map(|slot| slot.get_or_insert_with(|| {
                            // SAFETY: this callback owns the bus state. Blocks occupy
                            // a dense prefix, so each bus is borrowed at most once.
                            let block = unsafe { &mut *bus.state.get() };
                            let fits = output_period.load(std::sync::atomic::Ordering::Relaxed) as usize
                                <= patch_effects::MAX_PATCH_EFFECT_FRAMES && block.begin(frames);
                            ActivePatchBlock { bus, block, fits, rendered: false }
                        }))
                }),
            _ => None,
        };
        if state == VoiceState::Paused && voice.live.is_none() {
            let fader = &voice.inner.rate_fader;
            // SAFETY: the mixer is the only reader and writer of RateSlew.
            let slew = unsafe { &mut *voice.inner.rate_slew.get() };
            let rate = slew.begin_buffer(fader, output_sample_rate, true);
            slew.finish_buffer(fader, rate);
            if let Some(trim) = &voice.inner.cue_trim {
                // SAFETY: only this stream's mix callback writes the trim slew.
                let slew = unsafe { &mut *trim.slew.get() };
                let gain = slew.begin_buffer(&trim.fader, output_sample_rate, true);
                slew.finish_buffer(&trim.fader, gain);
            }
            if let Some(trim) = &voice.inner.output_trim {
                let mut gains = [1.0; super::voice::MATRIX_OUTPUTS];
                // SAFETY: only the owning PCM callback advances trim slews.
                unsafe {
                    trim.prepare(&mut gains, output_sample_rate, true);
                    trim.finish(&gains);
                }
            }
        }
        if state != VoiceState::Playing && state != VoiceState::FadingOut {
            continue;
        }
        if voice.start_batch.as_ref().is_some_and(|batch| !batch.released_at(start_epoch)) {
            // Child loading can take longer than a first-render deadline.
            // A healthy callback keeps the unrendered start valid. A stream
            // loss still expires it, and a restart replaces the deadline.
            if deadline != 0 {
                use std::sync::atomic::Ordering::Relaxed;
                let window = first_render_window_us(output_period.load(Relaxed), output_sample_rate);
                let _ = voice.start_deadline_us.compare_exchange(
                    deadline, block_started_at.saturating_add(window), Relaxed, Relaxed,
                );
            }
            continue;
        }
        if let Some(cut) = &voice.playlist_cut {
            if frames == 0 { continue; }
            if !cut.admit(&voices_guard, (voice.out_l, voice.out_r)) {
                voice.set_stopped();
                continue;
            }
        }
        if deadline != 0 {
            voice.start_deadline_us.store(0, std::sync::atomic::Ordering::Relaxed);
        }

        let mut voice_peak_l = 0.0_f32;
        let mut voice_peak_r = 0.0_f32;
        let mut voice_sum = [0.0_f64; 2];

        let (output, channels, left, right) = match (voice.patch_effects.as_deref(), patch_block) {
            (None, _) => (&mut *output, channels, voice.out_l, voice.out_r),
            (Some(_), Some(active)) if active.fits => {
                active.rendered = true;
                let mix_channels = active.block.channels();
                let (left, right) = if active.block.uses_cue_outputs() {
                    (0, usize::from(mix_channels > 1))
                } else {
                    (voice.out_l, voice.out_r)
                };
                (active.block.samples(frames), mix_channels, left, right)
            }
            (Some(_), _) => {
                voice.set_stopped();
                let _ = status_prod.try_push(AudioStatus::OutputWithdrawn { voice_id: voice.id });
                continue;
            }
        };
        // Capture and decoded-file voices can contribute to the same patch bus.
        if voice.live.is_some() {
            if let Some(feeds) = feeds_guard.as_deref() {
                let mut peaks = [0.0; 2];
                mix_live(output, channels, output_sample_rate, voice, feeds, status_prod, [left, right], &mut peaks, &mut voice_sum, output_period);
                [voice_peak_l, voice_peak_r] = peaks;
            }
            let rms = rms_levels(voice_sum, frames);
            accumulate_peaks(voice, voice_peak_l, voice_peak_r, rms, &mut peak_l, &mut peak_r, &mut patch_peaks);
            continue;
        }
        let patch_gain = voice.patch_gain();
        let (gain_l, gain_r) = voice.pan_gains();
        let (gain_l, gain_r) = (gain_l * patch_gain, gain_r * patch_gain);
        // A level matrix carries its own routing, so it multiplies the voice
        // gain directly instead of the panned pair.
        let matrix_gain = voice.inner.gain() * patch_gain;
        // SAFETY: written once before the voice was submitted; read-only here.
        let matrix = voice.inner.level_matrix.snapshot();
        let mut input_frame = [0.0_f32; super::voice::MATRIX_INPUTS];
        let voice_channels = voice.channels as usize;
        // SAFETY: after submission this stream's mix callback owns the
        // filters and their histories, including state reset on seek.
        let effects = unsafe { &mut *voice.inner.effects.get() };
        if let Some(effects) = effects {
            effects.prepare(output_sample_rate);
        }
        let source_inputs = voice_channels.min(if matrix.is_some() {
            input_frame.len()
        } else {
            2
        });
        let total_frames = voice.total_frames();
        let sample_rate_ratio = f64::from(voice.sample_rate) / f64::from(output_sample_rate);
        let rate_fader = &voice.inner.rate_fader;
        // SAFETY: the mixer is the only reader and writer of RateSlew.
        let slew = unsafe { &mut *voice.inner.rate_slew.get() };
        let mut rate = slew.begin_buffer(rate_fader, output_sample_rate, false);
        let mut trim = voice.inner.cue_trim.as_ref().map(|trim| {
            // SAFETY: only this stream's mix callback writes the trim slew.
            (&trim.fader, unsafe { &mut *trim.slew.get() })
        });
        let mut trim_gain = trim.as_mut().map_or(1.0, |(fader, slew)| {
            slew.begin_buffer(fader, output_sample_rate, false)
        });
        let mut output_trims = voice.inner.output_trim.as_ref().map(|trim| {
            let mut gains = [1.0; super::voice::MATRIX_OUTPUTS];
            // SAFETY: only the owning PCM callback advances trim slews.
            let moving = unsafe { trim.prepare(&mut gains, output_sample_rate, false) };
            (trim, gains, moving)
        });
        let mut frame_pos_f = voice.current_frame() as f64 + slew.source_fraction;

        // SAFETY: `fade` is only mutated from this callback; VoiceInner docs
        // establish the single-writer invariant.
        let fade_ptr = voice.inner.fade.get();

        let end = voice.end_frame();
        let loop_start = voice.loop_start_frame;
        let loop_length = end.saturating_sub(loop_start);
        let mut loops = voice.inner.loops_remaining.load(std::sync::atomic::Ordering::Relaxed);

        // SAFETY: `slices` is written once before submission; `current` /
        // `remaining` are mutated only from this callback (single-writer).
        let slices_ptr = voice.inner.slices.get();

        let end_fade = if loops == u32::MAX || unsafe { (*slices_ptr).is_some() } {
            None
        } else {
            voice.inner.natural_fade_out().map(|(ms, curve)| {
                (f64::from(ms) * f64::from(voice.sample_rate) / 1000.0, curve)
            })
        };

        let mut voice_stopped = false;

        for frame in 0..frames {
            // --- Per-frame fade gain -------------------------------------------
            let fade_gain: f32 = if let Some(fade) = unsafe { &mut *fade_ptr } {
                let g = fade.gain();
                let done = fade.advance(1);
                if done {
                    if fade.direction == FadeDirection::Out {
                        voice.set_stopped();
                        let _ = status_prod.try_push(AudioStatus::Completed { voice_id: voice.id });
                        unsafe { *fade_ptr = None };
                        voice_stopped = true;
                    } else {
                        // Fade-in complete — clear state, continue playing.
                        unsafe { *fade_ptr = None };
                    }
                }
                g
            } else {
                1.0_f32
            };

            if voice_stopped {
                break;
            }

            // --- Boundary / loop check ----------------------------------------
            let int_pos = frame_pos_f as u64;
            let segment_end;
            if let Some(prog) = unsafe { &mut *slices_ptr } {
                // A fast playback step can cross several short slices at once.
                // Carry the overshoot through the loop boundaries and count
                // each crossed pass, including a pending Devamp at the first.
                segment_end = loop {
                    let seg = prog.segments[prog.current];
                    let end = seg.end_frame.min(total_frames);
                    if frame_pos_f < end as f64 {
                        break end;
                    }
                    let req = voice.inner.devamp_request.swap(
                        crate::engine::voice::DEVAMP_NONE,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    if req != crate::engine::voice::DEVAMP_NONE {
                        // The devamped slice ended — notify even when the
                        // voice continues into the next slice, so a boundary
                        // successor can fire at the slice end.
                        let _ = status_prod
                            .try_push(AudioStatus::SliceBoundary { voice_id: voice.id });
                    }
                    if req == crate::engine::voice::DEVAMP_STOP || end <= seg.start_frame {
                        voice_stopped = true;
                        break end;
                    }
                    if req == crate::engine::voice::DEVAMP_CONTINUE {
                        prog.remaining = 0;
                    }
                    let overshoot = frame_pos_f - end as f64;
                    let width = (end - seg.start_frame) as f64;
                    let repeat_frames = f64::from(prog.remaining) * width;
                    if prog.remaining == u32::MAX || overshoot < repeat_frames {
                        if prog.remaining != u32::MAX {
                            prog.remaining -= 1 + (overshoot / width) as u32;
                        }
                        frame_pos_f = seg.start_frame as f64 + overshoot.rem_euclid(width);
                        break end;
                    }
                    if prog.current + 1 == prog.segments.len() {
                        voice_stopped = true;
                        break end;
                    }
                    prog.current += 1;
                    let next = prog.segments[prog.current];
                    prog.remaining = if next.play_count == u32::MAX {
                        u32::MAX
                    } else {
                        next.play_count.saturating_sub(1)
                    };
                    frame_pos_f = next.start_frame as f64 + (overshoot - repeat_frames);
                };
            } else {
                segment_end = end;
                if int_pos >= end {
                    let release_whole = voice.whole_loop_release.as_ref()
                        .is_some_and(|request| request.load(std::sync::atomic::Ordering::Acquire));
                    let overshoot = frame_pos_f - end as f64;
                    let passes = ((overshoot / loop_length.max(1) as f64) as u64).saturating_add(1);
                    if release_whole || loop_length == 0 || (loops != u32::MAX && passes > u64::from(loops)) {
                        voice_stopped = true;
                    } else {
                        if loops != u32::MAX {
                            loops -= passes as u32;
                            voice.inner.loops_remaining.store(loops, std::sync::atomic::Ordering::Relaxed);
                        }
                        frame_pos_f = loop_start as f64 + overshoot.rem_euclid(loop_length as f64);
                    }
                }
            }
            if voice_stopped {
                voice.set_stopped();
                let _ = status_prod.try_push(AudioStatus::Completed { voice_id: voice.id });
                break;
            }

            let fade_gain = if let Some((distance_per_unit_rate, curve)) = end_fade {
                let remaining = end as f64 - frame_pos_f + loop_length as f64 * f64::from(loops);
                let fraction = remaining / (distance_per_unit_rate * rate);
                fade_gain * (1.0 - curve.apply(1.0 - fraction)) as f32
            } else {
                fade_gain
            };
            let fade_gain = fade_gain * trim_gain as f32;

            // --- Sample with linear interpolation (handles rate != 1.0) --------
            let int_pos = frame_pos_f as u64;
            let frac = (frame_pos_f - int_pos as f64) as f32;
            let base = int_pos as usize * voice_channels;
            // Clamp interpolation at the active source boundary.
            let next = (int_pos + 1).min(segment_end.saturating_sub(1)) as usize * voice_channels;

            let samples = &mut input_frame[..source_inputs];
            for (channel, dest) in samples.iter_mut().enumerate() {
                let current = voice.samples[base + channel];
                *dest = current + (voice.samples[next + channel] - current) * frac;
            }
            if let Some(effects) = effects {
                effects.process(samples);
            }

            let out_base = frame * channels;
            let output_gains = output_trims.as_ref().map(|(_, gains, _)| gains);

            if let Some(matrix) = matrix.as_deref() {
                let mut inputs = source_inputs;
                if inputs == 1 && matrix.mirror_mono {
                    input_frame[1] = input_frame[0];
                    inputs = 2;
                }
                let mut peaks = [voice_peak_l, voice_peak_r];
                matrix.mix(&input_frame[..inputs], matrix_gain * fade_gain, output_gains,
                    &mut output[out_base..out_base + channels], &mut peaks, &mut voice_sum);
                [voice_peak_l, voice_peak_r] = peaks;
            } else {
                let sample_l = input_frame[0];
                let sample_r = if source_inputs > 1 { input_frame[1] } else { sample_l };
                let trim_at = |channel| output_gains.and_then(|gains| gains.get(channel)).copied().unwrap_or(1.0) as f32;
                let out_l = sample_l * gain_l * fade_gain * trim_at(left);
                let out_r = sample_r * gain_r * fade_gain * trim_at(right);

                // Route to the per-voice output channels (from Output Patch).
                // Bounds-check at the sample level keeps the RT callback safe even
                // if the patch references a channel the device does not have.
                if left < channels { output[out_base + left] += out_l; }
                if right < channels { output[out_base + right] += out_r; }

                voice_peak_l = voice_peak_l.max(out_l.abs());
                voice_peak_r = voice_peak_r.max(out_r.abs());
                voice_sum[0] += f64::from(out_l) * f64::from(out_l);
                voice_sum[1] += f64::from(out_r) * f64::from(out_r);
            }

            frame_pos_f += rate * sample_rate_ratio;
            slew.advance(&mut rate);
            if let Some((_, trim_slew)) = &mut trim {
                trim_slew.advance(&mut trim_gain);
            }
            if let Some((bank, gains, moving)) = &mut output_trims {
                if *moving {
                    // SAFETY: only the owning PCM callback advances trim slews.
                    *moving = unsafe { bank.advance(gains) };
                }
            }
        }

        // Keep the fractional position across callbacks, even below one frame
        // per block. The integer snapshot is all non-RT time displays need.
        slew.source_fraction = frame_pos_f.fract();
        voice.frame_pos.store(frame_pos_f as u64, std::sync::atomic::Ordering::Relaxed);
        slew.finish_buffer(rate_fader, rate);
        if let Some((fader, trim_slew)) = trim {
            trim_slew.finish_buffer(fader, trim_gain);
        }
        if let Some((bank, gains, _)) = output_trims {
            // SAFETY: only the owning PCM callback advances trim slews.
            unsafe { bank.finish(&gains) };
        }
        if let Some(effects) = effects {
            effects.finish_buffer();
        }

        let rms = rms_levels(voice_sum, frames);
        accumulate_peaks(voice, voice_peak_l, voice_peak_r, rms, &mut peak_l, &mut peak_r, &mut patch_peaks);
    }

    for active in patch_blocks.into_iter().flatten() {
        if active.rendered {
            active.block.finish(output, channels, output_sample_rate);
        } else {
            active.block.reset();
        }
    }
    // Apply master gain.
    for s in output.iter_mut() { *s *= master; }

    let _ = status_prod.try_push(AudioStatus::MasterLevels {
        peak_l: peak_l * master,
        peak_r: peak_r * master,
    });
    for slot in 0..PATCH_VU_SLOTS {
        let (l, r) = (patch_peaks[slot * 2], patch_peaks[slot * 2 + 1]);
        if l > 0.0 || r > 0.0 {
            let _ = status_prod.try_push(AudioStatus::PatchLevels {
                slot: slot as u8,
                peak_l: l * master,
                peak_r: r * master,
            });
        }
    }
}

/// Number of Output Patches metered by the per-patch VU (fixed so the RT
/// callback can accumulate into a stack array — patches beyond this many are
/// simply unmetered in the mixer).
pub const PATCH_VU_SLOTS: usize = 16;

/// Fold one voice's block peaks into the master meters and, when the voice is
/// routed through a metered Output Patch, into that patch's VU slot.
fn rms_levels(sum_sq: [f64; 2], frames: usize) -> [f32; 2] {
    let frames = frames.max(1) as f64;
    [(sum_sq[0] / frames).sqrt() as f32, (sum_sq[1] / frames).sqrt() as f32]
}

fn accumulate_peaks(
    voice: &Arc<Voice>,
    voice_peak_l: f32,
    voice_peak_r: f32,
    voice_rms: [f32; 2],
    peak_l: &mut f32,
    peak_r: &mut f32,
    patch_peaks: &mut [f32; PATCH_VU_SLOTS * 2],
) {
    use std::sync::atomic::Ordering::Relaxed;
    voice.inner.live_peak[0].store(voice_peak_l.to_bits(), Relaxed);
    voice.inner.live_peak[1].store(voice_peak_r.to_bits(), Relaxed);
    voice.inner.live_rms[0].store(voice_rms[0].to_bits(), Relaxed);
    voice.inner.live_rms[1].store(voice_rms[1].to_bits(), Relaxed);
    *peak_l = (*peak_l).max(voice_peak_l);
    *peak_r = (*peak_r).max(voice_peak_r);
    if let Some(slot) = voice.patch_slot {
        let slot = slot as usize;
        if slot < PATCH_VU_SLOTS {
            patch_peaks[slot * 2] = patch_peaks[slot * 2].max(voice_peak_l);
            patch_peaks[slot * 2 + 1] = patch_peaks[slot * 2 + 1].max(voice_peak_r);
        }
    }
}

/// Mix one live (Mic Cue) voice from its input feed into `output`.
///
/// Resamples the input device clock to the output clock with adaptive drift
/// compensation: the read cursor is kept ~25 ms behind the feed's write head,
/// and the resample ratio is nudged ±2 % to hold that lag, so slow clock drift
/// between the input and output devices never under/overruns.  Applies the
/// voice's pan/gain, soft fade, and Output-Patch channel routing exactly like
/// the file path.
#[allow(clippy::too_many_arguments)]
fn mix_live(
    output: &mut [f32],
    channels: usize,
    output_sample_rate: u32,
    voice: &Arc<Voice>,
    feeds: &[InputFeed],
    status_prod: &mut ringbuf::HeapProd<AudioStatus>,
    [left, right]: [usize; 2],
    peaks: &mut [f32; 2],
    sum_sq: &mut [f64; 2],
    output_period: &Arc<std::sync::atomic::AtomicU32>,
) {
    let Some(live) = voice.live.as_ref() else { return };
    let Some(feed) = feeds.iter().find(|f| f.id == live.feed_id) else { return };
    // Need at least a couple of frames captured before we can interpolate.
    if feed.write_frame < 2 {
        return;
    }

    let frames = output.len() / channels;
    // Keep the read cursor 3 output periods behind the write head.  This gives
    // enough headroom to absorb one missed input callback without underrunning,
    // while minimising the imposed latency.  With 64-sample buffers at 48kHz
    // that is 3 × 64 / 48000 ≈ 4 ms — vs. the old fixed 1200 samples (25 ms).
    let period = output_period.load(std::sync::atomic::Ordering::Relaxed).max(64) as f64;
    let target_lag = (period * 3.0).max(192.0);

    if !live.is_started() {
        live.set_read_frame((feed.write_frame as f64 - target_lag).max(0.0));
        live.mark_started();
    }

    let base_ratio = live.src_rate as f64 / output_sample_rate as f64;
    let mut read = live.read_frame();
    // Resync on a gross lag (resume after pause, glitch, or runaway drift):
    // jump the cursor back to `target_lag` behind the write head.
    if !(0.0..=STAGING_FRAMES as f64).contains(&(feed.write_frame as f64 - read)) {
        read = (feed.write_frame as f64 - target_lag).max(0.0);
    }
    // Adaptive: nudge the ratio to hold the read cursor near `target_lag`.
    let lag = feed.write_frame as f64 - read;
    let correction = ((lag - target_lag) / target_lag).clamp(-0.02, 0.02);
    let ratio = base_ratio * (1.0 + correction);

    let patch_gain = voice.patch_gain();
    let (gain_l, gain_r) = voice.pan_gains();
    let (gain_l, gain_r) = (gain_l * patch_gain, gain_r * patch_gain);
    let matrix = voice.inner.level_matrix.snapshot();
    let matrix_gain = voice.inner.gain() * patch_gain;
    let inputs = if live.in_r == live.in_l { 1 } else { 2 };
    // SAFETY: `fade` is only mutated from this callback thread.
    let fade_ptr = voice.inner.fade.get();
    // Oldest frame still resident in the circular staging buffer.
    let oldest = feed.write_frame as f64 - (STAGING_FRAMES as f64 - 2.0);

    for frame in 0..frames {
        let fade_gain: f32 = if let Some(fade) = unsafe { &mut *fade_ptr } {
            let g = fade.gain();
            let done = fade.advance(1);
            if done {
                if fade.direction == FadeDirection::Out {
                    voice.set_stopped();
                    let _ = status_prod.try_push(AudioStatus::Completed { voice_id: voice.id });
                    unsafe { *fade_ptr = None };
                    break;
                } else {
                    unsafe { *fade_ptr = None };
                }
            }
            g
        } else {
            1.0
        };

        if read < oldest {
            read = oldest.max(0.0);
        }
        // Underrun: not enough fresh audio yet — stop here, resume next block.
        if read + 1.0 >= feed.write_frame as f64 {
            break;
        }

        let s_l = feed.sample(live.in_l, read);
        let s_r = if live.in_r == live.in_l { s_l } else { feed.sample(live.in_r, read) };
        let base = frame * channels;
        if let Some(matrix) = matrix.as_deref() {
            let source = [s_l, s_r];
            matrix.mix(&source[..inputs], matrix_gain * fade_gain, None,
                &mut output[base..base + channels], peaks, sum_sq);
        } else {
            let out_l = s_l * gain_l * fade_gain;
            let out_r = s_r * gain_r * fade_gain;
            if left < channels { output[base + left] += out_l; }
            if right < channels { output[base + right] += out_r; }
            peaks[0] = peaks[0].max(out_l.abs());
            peaks[1] = peaks[1].max(out_r.abs());
            sum_sq[0] += f64::from(out_l) * f64::from(out_l);
            sum_sq[1] += f64::from(out_r) * f64::from(out_r);
        }

        read += ratio;
    }

    live.set_read_frame(read);
}

fn apply_command(
    voices: &[Arc<Voice>],
    cmd: AudioCommand,
    _status_prod: &mut ringbuf::HeapProd<AudioStatus>,
) {
    match cmd {
        AudioCommand::Stop { voice_id, fade_ms, fade_curve } => {
            if let Some(v) = voices.iter().find(|v| v.id == voice_id) {
                // A paused voice (e.g. a video's audio that was never resumed
                // because the video was replaced before its first frame) must
                // hard-stop: fading it would set it Playing and make it audible.
                if fade_ms == 0 || matches!(v.voice_state(), VoiceState::Paused | VoiceState::Stopped) {
                    v.set_stopped();
                } else {
                    let total = (fade_ms as u64 * v.sample_rate as u64) / 1000;
                    // SAFETY: Only written from this callback.
                    unsafe {
                        *v.inner.fade.get() = Some(FadeState {
                            direction: FadeDirection::Out,
                            total_samples: total,
                            elapsed_samples: 0,
                            curve: fade_curve,
                        });
                    }
                    let _ = v.state.fetch_update(
                        std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Relaxed,
                        |state| (state != VoiceState::Stopped as u8).then_some(VoiceState::FadingOut as u8),
                    );
                }
            }
        }
        AudioCommand::Pause { voice_id } => {
            if let Some(v) = voices.iter().find(|v| v.id == voice_id && v.voice_state() != VoiceState::Stopped) {
                v.set_paused();
            }
        }
        AudioCommand::Resume { voice_id } => {
            if let Some(v) = voices.iter().find(|v| v.id == voice_id && v.voice_state() != VoiceState::Stopped) {
                v.set_playing();
            }
        }
        AudioCommand::SetGain { voice_id, gain } => {
            if let Some(v) = voices.iter().find(|v| v.id == voice_id) {
                v.inner.set_gain(gain);
            }
        }
        AudioCommand::SetPan { voice_id, pan } => {
            if let Some(v) = voices.iter().find(|v| v.id == voice_id) {
                v.inner.set_pan(pan);
            }
        }
        AudioCommand::SetMasterGain { .. } => {}

        AudioCommand::StopAll => {
            for v in voices {
                v.set_stopped();
            }
        }
        AudioCommand::Seek { voice_id, frame_pos } => {
            if let Some(v) = voices.iter().find(|v| v.id == voice_id) {
                v.frame_pos.store(frame_pos, std::sync::atomic::Ordering::Relaxed);
                // SAFETY: consumed on the same callback that owns both states.
                unsafe {
                    (*v.inner.rate_slew.get()).source_fraction = 0.0;
                    if let Some(effects) = &mut *v.inner.effects.get() {
                        effects.reset();
                    }
                }
            }
        }
        AudioCommand::Devamp { voice_id, stop_at_end } => {
            if let Some(v) = voices.iter().find(|v| v.id == voice_id) {
                let req = if stop_at_end { crate::engine::voice::DEVAMP_STOP } else { crate::engine::voice::DEVAMP_CONTINUE };
                v.inner.devamp_request.store(req, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}


/// Open and return the ASIO cpal host.
///
/// Requires the `asio-support` Cargo feature. Returns an error when the
/// feature is absent or no ASIO host is detected at runtime.
fn open_asio_host() -> Result<cpal::Host> {
    #[cfg(all(windows, feature = "asio-support"))]
    {
        let asio = cpal::available_hosts()
            .into_iter()
            .filter(|id| *id != cpal::default_host().id())
            .find_map(|id| cpal::host_from_id(id).ok());
        asio.ok_or_else(|| anyhow!("No ASIO host found. Ensure your ASIO drivers are installed."))
    }
    #[cfg(not(all(windows, feature = "asio-support")))]
    {
        Err(anyhow!(
            "ASIO support is not compiled in. \
             Install the Steinberg ASIO SDK, set CPAL_ASIO_DIR, \
             then build with: pnpm tauri build -- --features asio-support"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named_stage_output(channels: Vec<u16>) -> NamedOutput {
        let mut patch = super::super::device_manager::OutputPatch::new("Stage", "hw:CARD=Stage", channels);
        patch.require_named_device = true;
        patch.named_output().unwrap().unwrap()
    }

    fn filtered_stage_voice(level: f32, bus: Arc<PatchBus>, frames: usize) -> Arc<Voice> {
        let mut voice = Voice::new(Arc::new(vec![level; frames]), 1, 48_000, 1.0, -1.0);
        voice.named_output = Some(bus.route.clone());
        voice.patch_effects = Some(bus);
        voice.set_playing();
        Arc::new(voice)
    }

    fn high_pass_stage_bus() -> Arc<PatchBus> {
        use super::super::cue_effects::CueEffect;
        let mut route = named_stage_output(vec![0, 1]);
        route.audio_effects.push(CueEffect::HighPass {
            id: Uuid::new_v4(), enabled: true, frequency_hz: 600.0,
        });
        Arc::new(PatchBus::new(route, 2, 48_000).unwrap())
    }

    #[test]
    fn patch_effects_filter_the_summed_file_audio_and_keep_the_live_signal_dry() {
        use std::sync::atomic::AtomicU32;
        let bus = high_pass_stage_bus();
        let voices = [
            filtered_stage_voice(0.12, bus.clone(), 2048),
            filtered_stage_voice(0.30, bus, 2048),
        ];
        let (feed, mut feed_prod) = make_feed(1);
        for _ in 0..4096 {
            feed_prod.try_push(0.20).unwrap();
        }
        let live = LiveSource::new(feed.id, 0, 0, 48_000);
        let ltc = Arc::new(Voice::new_live(live, 48_000, 1.0, -1.0));
        ltc.set_playing();
        let pool = ArcSwap::from_pointee(vec![voices[0].clone(), ltc, voices[1].clone()]);
        let feeds = Arc::new(Mutex::new(vec![feed]));
        let (_, mut commands) = HeapRb::new(16).split();
        let (mut status, _) = HeapRb::new(16).split();
        let master = Arc::new(AtomicU32::new(1.0_f32.to_bits()));
        let period = Arc::new(AtomicU32::new(2048));
        let mut output = [0.0; 4096];
        fill_buffer(audio_clock_us(), &mut output, 2, 48_000, &pool, &feeds, &mut commands, &mut status, &master, &period);
        assert!(output[0] > 0.58);
        assert!((output[4094] - 0.20).abs() < 1e-6);
        assert!(output.iter().skip(1).step_by(2).all(|&sample| sample == 0.0));
    }

    #[test]
    fn patch_effects_keep_two_chains_independent_on_the_same_output() {
        use std::sync::atomic::AtomicU32;
        let high_pass = high_pass_stage_bus();
        let mut low_pass = named_stage_output(vec![0, 1]);
        low_pass.audio_effects.push(CueEffect::LowPass {
            id: Uuid::new_v4(), enabled: true, frequency_hz: 600.0,
        });
        let low_pass = Arc::new(PatchBus::new(low_pass, 2, 48_000).unwrap());
        let first = filtered_stage_voice(0.35, high_pass, 2048);
        let second = filtered_stage_voice(0.20, low_pass, 2048);
        let pool = ArcSwap::from_pointee(vec![first.clone(), second.clone()]);
        let feeds = Arc::new(Mutex::new(Vec::new()));
        let (_, mut commands) = HeapRb::new(16).split();
        let (mut status, _) = HeapRb::new(16).split();
        let master = Arc::new(AtomicU32::new(1.0_f32.to_bits()));
        let period = Arc::new(AtomicU32::new(1024));
        let mut output = [0.0; 2048];
        fill_buffer(audio_clock_us(), &mut output, 2, 48_000, &pool, &feeds, &mut commands, &mut status, &master, &period);
        assert_eq!(first.current_frame(), 1024);
        assert_eq!(second.current_frame(), 1024);
        assert!(output[0] > 0.30);
        assert!((output[2046] - 0.20).abs() < 1e-5);
        assert!(output.iter().skip(1).step_by(2).all(|&sample| sample == 0.0));
    }

    #[test]
    fn cue_output_effects_run_before_two_patch_outputs_sum_on_one_hardware_channel() {
        use super::super::{device_manager::{CueOutputEffects, OutputPatch}, voice::LevelMatrix};
        use std::sync::atomic::AtomicU32;

        let mut patch = OutputPatch::new("Stage", "hw:CARD=Stage", vec![1, 1]);
        patch.require_named_device = true;
        patch.cue_output_effects.push(CueOutputEffects { output: 0, effects: vec![
            CueEffect::HighPass { id: Uuid::new_v4(), enabled: true, frequency_hz: 600.0 },
        ] });
        assert_eq!(patch.cue_mixer_channels().as_ref(), &[0, 1]);
        let saved = serde_json::to_value(&patch).unwrap();
        let reopened: OutputPatch = serde_json::from_value(saved).unwrap();
        let route = reopened.named_output().unwrap().unwrap();
        let bus = Arc::new(PatchBus::new(route, 2, 48_000).unwrap());
        let wet = filtered_stage_voice(0.35, bus.clone(), 2048);
        let dry = filtered_stage_voice(0.20, bus, 2048);
        let mut matrix = LevelMatrix::default();
        matrix.set(0, 1, 1.0);
        dry.inner.level_matrix.update(|_| Some(matrix));

        let pool = ArcSwap::from_pointee(vec![wet, dry]);
        let feeds = Arc::new(Mutex::new(Vec::new()));
        let (_, mut commands) = HeapRb::new(16).split();
        let (mut status, _) = HeapRb::new(16).split();
        let master = Arc::new(AtomicU32::new(1.0_f32.to_bits()));
        let period = Arc::new(AtomicU32::new(1024));
        let mut output = [0.0; 2048];
        fill_buffer(audio_clock_us(), &mut output, 2, 48_000, &pool, &feeds, &mut commands, &mut status, &master, &period);
        assert!(output[1] > 0.50, "both cue outputs enter the device output: {}", output[1]);
        assert!((output[2047] - 0.20).abs() < 1e-5, "the unaffected cue output stays dry");
        assert!(output.iter().step_by(2).all(|&sample| sample == 0.0));
    }

    #[test]
    fn processed_mic_matrix_filters_a_live_send_before_it_joins_a_dry_send() {
        use super::super::{device_manager::{CueOutputEffects, OutputPatch}, voice::LevelMatrix};
        use std::sync::atomic::AtomicU32;

        let mut patch = OutputPatch::new("Stage", "hw:CARD=Stage", vec![1, 1]);
        patch.require_named_device = true;
        patch.cue_output_effects.push(CueOutputEffects { output: 1, effects: vec![
            CueEffect::HighPass { id: Uuid::new_v4(), enabled: true, frequency_hz: 600.0 },
        ] });
        let route = patch.named_output().unwrap().unwrap();
        let bus = Arc::new(PatchBus::new(route.clone(), 2, 48_000).unwrap());
        let (feed, mut producer) = make_feed(1);
        for _ in 0..4096 { producer.try_push(0.3).unwrap(); }
        let mut mic = Voice::new_live(LiveSource::new(feed.id, 0, 0, 48_000), 48_000, 1.0, 0.0);
        let mut matrix = LevelMatrix::default();
        matrix.set(0, 1, 1.0);
        mic.inner.level_matrix.update(|_| Some(matrix));
        mic.named_output = Some(route);
        mic.patch_effects = Some(bus.clone());
        mic.set_playing();

        let pool = ArcSwap::from_pointee(vec![Arc::new(mic), filtered_stage_voice(0.20, bus, 2048)]);
        let feeds = Arc::new(Mutex::new(vec![feed]));
        let (_, mut commands) = HeapRb::new(16).split();
        let (mut status, _) = HeapRb::new(16).split();
        let master = Arc::new(AtomicU32::new(1.0_f32.to_bits()));
        let period = Arc::new(AtomicU32::new(1024));
        let mut output = [0.0; 2048];
        fill_buffer(audio_clock_us(), &mut output, 2, 48_000, &pool, &feeds, &mut commands, &mut status, &master, &period);
        assert!(output[1] > 0.47, "the mic and the dry file use the same speaker");
        assert!((output[2047] - 0.20).abs() < 1e-5, "only the mic is filtered");
        assert!(output.iter().step_by(2).all(|&sample| sample == 0.0));
    }

    #[test]
    fn oversized_patch_block_withdraws_wet_voice_and_keeps_dry_playback() {
        use std::sync::atomic::AtomicU32;
        let frames = patch_effects::MAX_PATCH_EFFECT_FRAMES + 1;
        let wet = filtered_stage_voice(0.40, high_pass_stage_bus(), frames);
        let dry = Voice::new(Arc::new(vec![0.20; frames]), 1, 48_000, 1.0, -1.0);
        dry.set_playing();
        let pool = ArcSwap::from_pointee(vec![wet.clone(), Arc::new(dry)]);
        let feeds = Arc::new(Mutex::new(Vec::new()));
        let (_, mut commands) = HeapRb::new(16).split();
        let (mut status, mut received) = HeapRb::new(16).split();
        let master = Arc::new(AtomicU32::new(1.0_f32.to_bits()));
        let period = Arc::new(AtomicU32::new(frames as u32));
        let mut output = vec![0.0; frames * 2];
        fill_buffer(audio_clock_us(), &mut output, 2, 48_000, &pool, &feeds, &mut commands, &mut status, &master, &period);
        assert!(matches!(received.try_pop(), Some(AudioStatus::OutputWithdrawn { voice_id }) if voice_id == wet.id));
        assert_eq!(wet.voice_state(), VoiceState::Stopped);
        assert!(output.chunks_exact(2).all(|frame| frame == [0.20, 0.0]));
    }

    #[test]
    fn processed_patch_cannot_start_on_an_aux_or_fallback_output() {
        let engine = AudioEngine::new_silent(&MachineAudioConfig::default());
        let route = high_pass_stage_bus().route.clone();
        assert!(engine.check_named_output_ready(&route).unwrap_err().to_string().contains("named main output"));
        let mut voice = Voice::new(Arc::new(vec![0.2; 16]), 1, 48_000, 1.0, -1.0);
        voice.named_output = Some(route.clone());
        assert!(engine.play_voice_routed(voice, Some(&route.device_id)).unwrap_err().to_string().contains("named main output"));
        assert!(engine.aux_streams.lock().unwrap().is_empty());
        assert!(engine.voices.rt.load().is_empty());
    }

    #[test]
    fn only_the_active_processed_patch_can_share_its_filter_history() {
        use super::super::cue_effects::CueEffect;
        let engine = AudioEngine::new_silent(&MachineAudioConfig::default());
        let first = high_pass_stage_bus();
        // Same patch, changed effects: a different route for one patch id.
        let mut other_route = first.route.clone();
        other_route.audio_effects.push(CueEffect::HighPass {
            id: Uuid::new_v4(), enabled: true, frequency_hz: 900.0,
        });
        let voice = filtered_stage_voice(0.4, first.clone(), 32);
        assert!(engine.voices.push(voice.clone()).is_ok());
        let shared = engine.available_patch_bus(&first.route).unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &shared));
        assert!(
            engine.available_patch_bus(&other_route).is_err(),
            "a changed route on the active patch must be refused until its audio stops"
        );
        voice.set_stopped();
        assert!(engine.available_patch_bus(&other_route).unwrap().is_none());
    }

    #[test]
    fn exact_output_requires_opened_identity_and_every_patch_channel() {
        let route = named_stage_output(vec![0, 7, 2]);
        assert!(check_named_output(&route, None, 8).is_err());
        assert!(check_named_output(&route, Some("hw:CARD=House"), 8).is_err());
        assert!(check_named_output(&route, Some("hw:CARD=Stage"), 7).is_err());
        assert!(check_named_output(&route, Some("hw:CARD=Stage"), 8).is_ok());
    }

    #[test]
    fn replacement_stream_withdraws_exact_playing_and_paused_voices_only() {
        let pool = VoicePool::new();
        let mut withdrawn = Vec::new();
        let mut submitted = Vec::new();
        for (named, state) in [(true, VoiceState::Playing), (true, VoiceState::Paused), (false, VoiceState::Playing)] {
            let mut voice = Voice::new(Arc::new(vec![0.5; 16]), 1, 48_000, 1.0, 0.0);
            voice.named_output = named.then(|| named_stage_output(vec![0]));
            voice.state.store(state as u8, std::sync::atomic::Ordering::Relaxed);
            let voice = Arc::new(voice);
            assert!(pool.push(voice.clone()).is_ok());
            submitted.push(voice);
        }
        pool.withdraw_named_voices(&mut withdrawn);
        assert_eq!(submitted[0].voice_state(), VoiceState::Stopped);
        assert_eq!(submitted[1].voice_state(), VoiceState::Stopped);
        assert_eq!(submitted[2].voice_state(), VoiceState::Playing);
        assert_eq!(withdrawn.len(), 2);
        for (status, voice) in withdrawn.iter().zip(&submitted) {
            assert!(matches!(status, AudioStatus::OutputWithdrawn { voice_id } if *voice_id == voice.id));
        }
        pool.withdraw_named_voices(&mut withdrawn);
        assert_eq!(withdrawn.len(), 2);
    }
    use ringbuf::HeapRb;
    use ringbuf::traits::{Producer, Split};

    /// Build a minimal Voice with `n_frames` of silence at the given sample rate.
    fn make_voice(n_frames: usize, channels: u16, sample_rate: u32, rate: f32) -> Arc<Voice> {
        let samples = Arc::new(vec![0.0f32; n_frames * channels as usize]);
        let v = Voice::new(samples, channels, sample_rate, 1.0, 0.0);
        v.inner.set_rate(rate);
        v.set_playing();
        Arc::new(v)
    }

    fn range(channels: u16, format: cpal::SampleFormat) -> cpal::SupportedStreamConfigRange {
        let buf = cpal::SupportedBufferSize::Range { min: 64, max: 8192 };
        cpal::SupportedStreamConfigRange::new(channels, 8_000, 192_000, buf, format)
    }

    /// What PipeWire's ALSA plugin advertises: any width 1..=64 in every format.
    fn pipewire_like_ranges() -> Vec<cpal::SupportedStreamConfigRange> {
        let mut v = Vec::new();
        for fmt in [cpal::SampleFormat::I16, cpal::SampleFormat::I32, cpal::SampleFormat::F32] {
            for ch in 1..=64 {
                v.push(range(ch, fmt));
            }
        }
        v
    }

    fn stereo_f32_default() -> cpal::SupportedStreamConfig {
        let buf = cpal::SupportedBufferSize::Range { min: 64, max: 8192 };
        cpal::SupportedStreamConfig::new(2, 48_000, buf, cpal::SampleFormat::F32)
    }

    #[test]
    fn wider_config_opens_a_pipewire_node_at_its_real_width() {
        // #10: a 4-out node opened stereo silently drops a patch on output 3.
        let cfg = wider_output_config(&stereo_f32_default(), pipewire_like_ranges(), ChannelWidth::Exactly(4))
            .expect("a 4-channel config is on offer");
        assert_eq!(cfg.channels(), 4);
        assert_eq!(cfg.sample_rate(), 48_000, "the default rate is kept");
        assert_eq!(cfg.sample_format(), cpal::SampleFormat::F32, "the default format is kept");
    }

    #[test]
    fn wider_config_widest_takes_every_hardware_channel() {
        let ranges = vec![range(2, cpal::SampleFormat::I32), range(10, cpal::SampleFormat::I32)];
        let cfg = wider_output_config(&stereo_f32_default(), ranges, ChannelWidth::Widest).unwrap();
        assert_eq!(cfg.channels(), 10);
        assert_eq!(cfg.sample_format(), cpal::SampleFormat::I32);
    }

    #[test]
    fn wider_config_keeps_the_default_when_nothing_wider_fits() {
        let d = stereo_f32_default();
        // A stereo node: nothing to widen.
        assert!(wider_output_config(&d, pipewire_like_ranges(), ChannelWidth::Exactly(2)).is_none());
        // Wider, but only in a format the callback cannot mix.
        assert!(wider_output_config(&d, vec![range(8, cpal::SampleFormat::I16)], ChannelWidth::Widest).is_none());
        // Wider, but not at the stream's sample rate.
        let off_rate = cpal::SupportedStreamConfigRange::new(
            8, 96_000, 96_000, cpal::SupportedBufferSize::Unknown, cpal::SampleFormat::F32,
        );
        assert!(wider_output_config(&d, vec![off_rate], ChannelWidth::Widest).is_none());
    }

    #[test]
    fn period_is_clamped_into_the_quantum_range() {
        let range = cpal::SupportedBufferSize::Range { min: 32, max: 2048 };
        assert_eq!(clamp_period(16, &range), 32);
        assert_eq!(clamp_period(256, &range), 256);
        assert_eq!(clamp_period(8192, &range), 2048);
        assert_eq!(clamp_period(256, &cpal::SupportedBufferSize::Unknown), 256);
    }

    #[test]
    fn period_stands_before_pipewire_reports_its_quantum_range() {
        // cpal's PipeWire host reports 0..0 until the server clock settings
        // arrive; clamping into it would request a zero-frame period.
        let unknown = cpal::SupportedBufferSize::Range { min: 0, max: 0 };
        assert_eq!(clamp_period(256, &unknown), 256);
    }

    #[test]
    fn only_dead_streams_trip_the_watchdog() {
        assert!(stream_error_is_fatal(cpal::ErrorKind::DeviceNotAvailable));
        assert!(!stream_error_is_fatal(cpal::ErrorKind::Xrun));
        assert!(!stream_error_is_fatal(cpal::ErrorKind::DeviceChanged));
        // A PipeWire stream in its error state never comes back on its own.
        assert_eq!(
            stream_error_is_fatal(cpal::ErrorKind::StreamInvalidated),
            cfg!(target_os = "linux"),
        );
    }

    #[test]
    fn pipewire_routing_covers_node_and_default_pcms_only() {
        assert!(routes_via_pipewire(None));
        assert!(routes_via_pipewire(Some("pipewire")));
        assert!(routes_via_pipewire(Some("default")));
        assert!(!routes_via_pipewire(Some("hw:CARD=UMC404HD,DEV=0")));
    }

    /// An RT voice-list handle (the ArcSwap the callback reads) holding `voices`.
    fn rt_pool(voices: Vec<Arc<Voice>>) -> Arc<ArcSwap<Vec<Arc<Voice>>>> {
        Arc::new(ArcSwap::from_pointee(voices))
    }

    fn capture_rate_test_samples(voice: &Arc<Voice>, frames: usize, rate: u32) -> Vec<f32> {
        let (_, mut commands) = HeapRb::<AudioCommand>::new(8).split();
        let (mut status, _) = HeapRb::<AudioStatus>::new(16).split();
        let mut output = vec![0.0; frames * 2];
        fill_buffer(audio_clock_us(), &mut output, 2, rate, &rt_pool(vec![Arc::clone(voice)]),
            &Arc::new(Mutex::new(Vec::new())), &mut commands, &mut status,
            &Arc::new(1.0_f32.to_bits().into()), &Arc::new(1u32.into()));
        output.into_iter().step_by(2).collect()
    }

    #[test]
    fn whole_audio_devamp_finishes_the_trimmed_pass_without_touching_a_fresh_voice() {
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

        let engine = AudioEngine::new_silent(&MachineAudioConfig::default());
        let make_loop = |loops| {
            let mut voice = Voice::new(Arc::new(vec![0.5; 12]), 1, 1000, 1.0, 0.0);
            voice.loop_start_frame = 2;
            voice.frame_pos.store(4, Relaxed);
            voice.inner.loops_remaining.store(loops, Relaxed);
            voice.whole_loop_release = Some(AtomicBool::new(false));
            // SAFETY: the end marker is installed before the test voice is shared.
            unsafe { *voice.inner.end_frame.get() = Some(7) };
            voice.set_playing();
            let voice = Arc::new(voice);
            assert!(engine.voices.push(Arc::clone(&voice)).is_ok());
            voice
        };
        for loops in [4, u32::MAX] {
            let voice = make_loop(loops);
            voice.set_paused();
            assert!(!engine.release_whole_audio_loop(voice.id));
            voice.set_playing();
            voice.start_deadline_us.store(1, Relaxed);
            assert!(!engine.release_whole_audio_loop(voice.id));
            voice.start_deadline_us.store(0, Relaxed);

            assert!(engine.release_whole_audio_loop(voice.id));
            assert_eq!(engine.voice_remaining_ms(voice.id), Some(3));
            let pcm = capture_rate_test_samples(&voice, 8, 1000);
            assert!(pcm[..3].iter().all(|&sample| sample > 0.0));
            assert_eq!(&pcm[3..], &[0.0; 5]);
            assert_eq!(voice.voice_state(), VoiceState::Stopped);
            assert_eq!(voice.inner.loops_remaining.load(Relaxed), loops);

            let fresh = make_loop(loops);
            assert!(!engine.release_whole_audio_loop(voice.id));
            assert!(capture_rate_test_samples(&fresh, 8, 1000).iter().all(|&sample| sample > 0.0));
            assert_eq!(fresh.voice_state(), VoiceState::Playing);
        }
        let ordinary_pcm = make_voice(12, 1, 1000, 1.0);
        ordinary_pcm.inner.loops_remaining.store(u32::MAX, Relaxed);
        assert!(engine.voices.push(Arc::clone(&ordinary_pcm)).is_ok());
        assert!(!engine.release_whole_audio_loop(ordinary_pcm.id));
    }

    #[test]
    fn output_trims_follow_physical_channels_and_leave_the_matrix_owner_intact() {
        use super::super::voice::LevelMatrix;

        for use_matrix in [false, true] {
            let mut voice = Voice::new(Arc::new(vec![0.5; 128]), 2, 1000, 1.0, 0.0)
                .with_output_trims(&[2, 0], &[0.5, 1.0]);
            voice.out_l = 2;
            voice.out_r = 0;
            let mut matrix = LevelMatrix::default();
            matrix.set(0, 2, 1.0);
            matrix.set(1, 0, 1.0);
            let owner = Uuid::new_v4();
            if use_matrix {
                voice.inner.level_matrix.update(|_| Some(matrix.clone()));
                assert!(voice.inner.level_matrix.claim(owner, &matrix));
            }
            voice.set_playing();
            let voice = Arc::new(voice);
            let render = |gain| {
                let (mut producer, mut commands) = HeapRb::<AudioCommand>::new(8).split();
                producer.try_push(AudioCommand::SetGain { voice_id: voice.id, gain }).unwrap();
                let (mut status, _) = HeapRb::<AudioStatus>::new(16).split();
                let mut output = [0.0; 24];
                fill_buffer(audio_clock_us(), &mut output, 3, 1000, &rt_pool(vec![Arc::clone(&voice)]),
                    &Arc::new(Mutex::new(Vec::new())), &mut commands, &mut status,
                    &Arc::new(1.0_f32.to_bits().into()), &Arc::new(8_u32.into()));
                output
            };
            let before = render(1.0);
            assert_eq!(before[0] / 2.0, before[2]);
            assert_eq!(before[1], 0.0);
            assert!(voice.inner.output_trim.as_ref().unwrap().set(&[1.0, 0.25]));
            let during = render(0.25);
            assert_eq!(during[0], before[0] / 4.0, "live trim slews from the saved level");
            assert_eq!(during[21], before[0] / 16.0);
            assert_eq!(during[23], before[0] / 4.0);
            assert_eq!(voice.inner.gain(), 0.25);
            if use_matrix {
                assert!(voice.inner.level_matrix.update_owned(owner, matrix));
            }
        }
    }

    #[test]
    fn file_audio_trim_slews_independently_of_master_fade_commands_in_both_routes() {
        use super::super::voice::LevelMatrix;

        for matrix in [None, Some({
            let mut matrix = LevelMatrix::default();
            matrix.set(0, 0, 1.0);
            matrix
        })] {
            let voice = Arc::new(Voice::new(Arc::new(vec![0.5; 64]), 1, 1000, 0.5, -1.0)
                .with_cue_trim(0.5));
            voice.inner.level_matrix.update(|_| matrix);
            voice.set_playing();
            assert_eq!(capture_rate_test_samples(&voice, 4, 1000), [0.125; 4]);

            voice.inner.cue_trim.as_ref().unwrap().fader.manual(0.25);
            let (mut producer, mut commands) = HeapRb::<AudioCommand>::new(8).split();
            producer.try_push(AudioCommand::SetGain { voice_id: voice.id, gain: 0.25 }).unwrap();
            let (mut status, _) = HeapRb::<AudioStatus>::new(16).split();
            let mut output = [0.0; 16];
            fill_buffer(audio_clock_us(), &mut output, 2, 1000, &rt_pool(vec![Arc::clone(&voice)]),
                &Arc::new(Mutex::new(Vec::new())), &mut commands, &mut status,
                &Arc::new(1.0_f32.to_bits().into()), &Arc::new(8_u32.into()));
            assert_eq!(output[0], 0.0625, "trim starts the live change at its previous value");
            assert_eq!(output[10], 0.03125, "trim reaches the new value after five frames");
            assert_eq!(output[14], 0.03125);
            assert_eq!(voice.inner.gain(), 0.25, "the separate master gain still belongs to the Fade");
        }
    }

    #[test]
    fn calibration_readback_uses_summed_physical_samples_after_master_gain() {
        use super::super::voice::LevelMatrix;

        let voice = |sample, channel| {
            let voice = Voice::new(Arc::new(vec![sample; 8]), 1, 48_000, 1.0, 0.0);
            let mut matrix = LevelMatrix::default();
            matrix.set(0, channel, 1.0);
            voice.inner.level_matrix.update(|_| Some(matrix));
            voice.set_playing();
            Arc::new(voice)
        };
        let voices = rt_pool(vec![voice(0.5, 2), voice(-0.5, 2), voice(0.5, 1)]);
        let (_, mut commands) = HeapRb::<AudioCommand>::new(8).split();
        let (mut status, _) = HeapRb::<AudioStatus>::new(16).split();
        let at = 1_000_000;
        let meter = OutputMeter::new();
        assert!(meter.read(at, &[2, 1, 0]).is_none());
        let mut output = [0.0; 12];
        fill_buffer(at, &mut output, 3, 48_000, &voices, &Arc::new(Mutex::new(Vec::new())),
            &mut commands, &mut status, &Arc::new(0.5_f32.to_bits().into()), &Arc::new(4u32.into()));
        meter.observe(at, &output, 3);
        assert_eq!(meter.read(at, &[2, 1, 0]), Some(vec![0.0, 0.25, 0.0]));
    }

    #[test]
    fn rate_fade_slews_the_actual_samples_across_callback_boundaries() {
        let samples = (0..128).map(|frame| frame as f32 / 100.0).collect();
        let voice = Arc::new(Voice::new(Arc::new(samples), 1, 1000, 1.0, -1.0));
        voice.set_playing();
        let fade = voice.inner.rate_fader.begin(3.0);
        let span = std::time::Duration::from_millis(5);
        fade.update(0.5, 0.25, span);
        let first = capture_rate_test_samples(&voice, 2, 1000);
        let second = capture_rate_test_samples(&voice, 5, 1000);
        for (actual, expected) in first.into_iter().chain(second).zip([0.0, 0.01, 0.022, 0.036, 0.052, 0.07, 0.09]) {
            assert!((actual - expected).abs() < 1e-6, "sample {actual} expected {expected}");
        }
        assert_eq!(voice.inner.rate(), 2.0);
        assert!((voice.current_frame() as f64 - 11.0).abs() <= 1.0);

        fade.update(0.9, 0.9, span);
        fade.hold();
        capture_rate_test_samples(&voice, 10, 1000);
        assert_eq!(voice.inner.rate(), 2.0, "pausing the Fade holds the current rate as audio continues");

        let newer = voice.inner.rate_fader.begin(0.5);
        newer.update(0.1, 0.5, span);
        fade.update(1.0, 1.0, span);
        drop(fade);
        capture_rate_test_samples(&voice, 10, 1000);
        assert_eq!(voice.inner.rate(), 1.25, "the falling curve belongs to the newer Fade");
        newer.finish();
        capture_rate_test_samples(&voice, 10, 1000);
        assert_eq!(voice.inner.rate(), 0.5);

        let stale = voice.inner.rate_fader.begin(2.0);
        voice.inner.rate_fader.manual(1.0);
        stale.update(1.0, 1.0, span);
        stale.finish();
        capture_rate_test_samples(&voice, 10, 1000);
        assert_eq!(voice.inner.rate(), 1.0, "a manual Audio rate edit revokes the earlier Fade");
    }

    #[test]
    fn very_slow_rate_advances_through_single_frame_callbacks() {
        let samples = (0..64).map(|frame| frame as f32 / 64.0).collect();
        let voice = Arc::new(Voice::new(Arc::new(samples), 1, 1000, 1.0, 0.0));
        voice.inner.set_rate(0.03);
        voice.set_playing();
        for _ in 0..101 {
            capture_rate_test_samples(&voice, 1, 1000);
        }
        assert_eq!(voice.current_frame(), 3);
    }

    #[test]
    fn natural_end_fade_uses_the_rendered_rate_and_finishes_at_the_source_end() {
        let voice = make_tone_voice(6, 1000);
        voice.inner.set_rate(2.0);
        voice.inner.set_natural_fade_out(2, FadeCurve::Linear);
        let centered = std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(capture_rate_test_samples(&voice, 5, 1000), vec![0.5 * centered, 0.5 * centered, 0.25 * centered, 0.0, 0.0]);
        assert_eq!(voice.voice_state(), VoiceState::Stopped);
    }

    #[test]
    fn patch_fade_changes_real_output_for_playing_and_later_voices_only_on_its_patch() {
        use crate::engine::{patch_fader::PatchFaders, voice::LevelMatrix};
        let faders = PatchFaders::default();
        let house = Uuid::new_v4();
        let monitor = Uuid::new_v4();
        let start = |id, output| {
            let mut voice = Voice::new(Arc::new(vec![0.25; 128]), 1, 48000, 1.0, 0.0);
            voice.patch_fader = Some(faders.resolve(id, 1.0));
            let mut matrix = LevelMatrix::default();
            matrix.set(0, output, 1.0);
            voice.inner.level_matrix.update(|_| Some(matrix));
            voice.set_playing();
            Arc::new(voice)
        };
        let capture = |voices| {
            let (_, mut commands) = HeapRb::<AudioCommand>::new(16).split();
            let (mut status, _) = HeapRb::<AudioStatus>::new(16).split();
            let mut output = [0.0; 2];
            fill_buffer(audio_clock_us(), &mut output, 2, 48000, &rt_pool(voices), &Arc::new(Mutex::new(Vec::new())),
                &mut commands, &mut status, &Arc::new(1.0_f32.to_bits().into()), &Arc::new(1u32.into()));
            output
        };
        let first = start(house, 0);
        let unaffected = start(monitor, 1);
        assert_eq!(capture(vec![first.clone(), unaffected.clone()]), [0.25, 0.25]);
        let fade = faders.resolve(house, 1.0).begin(0.0).unwrap();
        fade.update(0.5, 0.5);
        assert_eq!(capture(vec![first.clone(), unaffected.clone()]), [0.125, 0.25]);
        first.inner.set_gain(0.5);
        let next = start(house, 0);
        assert_eq!(capture(vec![first.clone(), next.clone(), unaffected.clone()]), [0.1875, 0.25]);
        fade.update(1.0, 1.0);
        assert_eq!(capture(vec![first.clone(), next.clone(), unaffected]), [0.0, 0.25]);
        faders.clear();
        // A retired voice/tail stays silent even if the saved scene is louder.
        assert_eq!(capture(vec![first, next, start(house, 0)]), [0.25, 0.0]);
    }

    #[test]
    fn discrete_channels_mix_at_the_last_supported_crosspoint() {
        use crate::engine::voice::LevelMatrix;
        let mut samples = vec![0.0; 128 * 4];
        for frame in samples.chunks_mut(128) { frame[127] = 0.5; frame[0] = 1.0; }
        let voice = Arc::new(Voice::new(Arc::new(samples), 128, 48000, 1.0, 0.0));
        voice.set_playing();
        let mut matrix = LevelMatrix::default();
        matrix.set(127, 127, 0.5);
        voice.inner.level_matrix.update(|_| Some(matrix));
        let pool = rt_pool(vec![voice]);
        let (_, mut commands) = HeapRb::<AudioCommand>::new(16).split();
        let (mut status, _) = HeapRb::<AudioStatus>::new(16).split();
        let mut output = [0.0; 256];
        fill_buffer(audio_clock_us(), &mut output, 128, 48000, &pool, &Arc::new(Mutex::new(Vec::new())),
            &mut commands, &mut status, &Arc::new(1.0_f32.to_bits().into()), &Arc::new(2u32.into()));
        for frame in output.chunks_exact(128) {
            assert_eq!(frame[127], 0.25);
            assert!(frame[..127].iter().all(|sample| *sample == 0.0));
        }
    }

    #[test]
    fn wide_pcm_wav_keeps_each_input_through_decode_patch_and_mix() {
        use crate::cue::audio_cue::{build_level_matrix, AudioCue};
        let file = tempfile::NamedTempFile::new().unwrap();
        let spec = hound::WavSpec {
            channels: 128, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(file.path(), spec).unwrap();
        for frame in 1..=8 {
            for channel in 1..=128 { writer.write_sample((frame * channel) as i16).unwrap(); }
        }
        writer.finalize().unwrap();
        let (samples, width, rate) = AudioCue::decode_file(file.path()).unwrap();
        assert_eq!((width, rate, samples.len()), (128, 48_000, 8 * 128));
        let routes: Vec<_> = (0..128).map(|input| {
            let mut row = vec![-60.0; input + 1];
            row[input] = 0.0;
            row
        }).collect();
        let reversed_patch: Vec<_> = (0..128).rev().collect();
        let matrix = build_level_matrix(&routes, &reversed_patch, false).unwrap();
        let voice = Arc::new(Voice::new(Arc::new(samples), width, rate, 1.0, 0.0));
        voice.inner.level_matrix.update(|_| Some(matrix));
        voice.set_playing();
        let (_, mut commands) = HeapRb::<AudioCommand>::new(16).split();
        let (mut status, _) = HeapRb::<AudioStatus>::new(16).split();
        let mut output = vec![0.0; 4 * 128];
        fill_buffer(audio_clock_us(), &mut output, 128, rate, &rt_pool(vec![voice]), &Arc::new(Mutex::new(Vec::new())),
            &mut commands, &mut status, &Arc::new(1.0_f32.to_bits().into()), &Arc::new(4u32.into()));
        for (frame, samples) in output.chunks_exact(128).enumerate() {
            for (channel, &measured) in samples.iter().enumerate() {
                let expected = ((frame + 1) * (128 - channel)) as f32 / i16::MAX as f32;
                assert_eq!(measured, expected, "frame {frame}, output {channel}");
            }
        }
    }

    #[test]
    fn a_saved_point_mix_drives_the_reopened_cue_and_real_output_patch() {
        use crate::cue::audio_cue::{AudioCue, AudioCueFactory, build_level_matrix};
        use crate::cue::point_mix::PointMix;
        use crate::cue::traits::{Cue, CueFactory};
        use serde_json::json;
        let point: PointMix = serde_json::from_value(json!({"input": 2, "source":{"x": 50,"y": 50},
            "speakers":[{"output": 1,"x": 0,"y": 50},{"output": 2,"x": 100,"y": 50}]})).unwrap();
        let mut cue = AudioCue::new();
        cue.level_matrix = Some(vec![vec![0.0, -60.0]]);
        let patch_id = uuid::Uuid::new_v4();
        cue.output_patch_id = Some(patch_id);
        let mut document = cue.serialize();
        let change = json!({"point_mix": point});
        crate::commands::cue_cmds::merge_properties(&mut document, &change, crate::commands::cue_cmds::MergePolicy::AllKeys);
        crate::commands::cue_cmds::apply_point_routing(&mut document, &change, None, &[(patch_id, vec![1, 0])]).unwrap();
        let reopened = AudioCueFactory.from_json(serde_json::from_slice(
            &serde_json::to_vec(&document).unwrap()).unwrap()).unwrap().serialize();
        let stored: PointMix = serde_json::from_value(reopened["point_mix"].clone()).unwrap();
        let routes: Vec<Vec<f64>> = serde_json::from_value(reopened["level_matrix"].clone()).unwrap();
        assert_eq!(routes[1], stored.levels(2).unwrap());
        assert_eq!(routes[0], [0.0, -60.0]);
        let voice = Arc::new(Voice::new(Arc::new(vec![0.125, 0.25].repeat(4)), 2, 48000, 1.0, 0.0));
        voice.inner.level_matrix.update(|_| build_level_matrix(&routes, &[1,0], true));
        voice.set_playing();
        let (_, mut commands) = HeapRb::<AudioCommand>::new(16).split();
        let (mut status, _) = HeapRb::<AudioStatus>::new(16).split();
        let mut output = [0.0;4];
        fill_buffer(audio_clock_us(), &mut output, 2, 48000, &rt_pool(vec![voice]), &Arc::new(Mutex::new(Vec::new())),
            &mut commands, &mut status, &Arc::new(1.0_f32.to_bits().into()), &Arc::new(2u32.into()));
        for frame in output.chunks_exact(2) {
            assert!((frame[0] - 0.25 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
            assert!((frame[1] - (frame[0]+0.125)).abs() < 1e-6);
        }
    }

    #[test]
    fn live_point_mix_and_retrigger_use_the_same_discrete_mono_input() {
        use crate::cue::audio_cue::{build_level_matrix, AudioCue, AudioCueFactory};
        use crate::cue::traits::{Cue, CueFactory};
        let engine = AudioEngine::new_silent(&MachineAudioConfig::default());
        let playing = Arc::new(Voice::new(Arc::new(vec![0.5; 16]), 1, 48000, 1.0, 0.0));
        playing.set_playing();
        assert!(engine.voices.push(Arc::clone(&playing)).is_ok());
        let render = |voice: Arc<Voice>| {
            let (_, mut cmd) = HeapRb::<AudioCommand>::new(8).split();
            let (mut status, _) = HeapRb::<AudioStatus>::new(8).split();
            let mut output = [0.0; 2];
            fill_buffer(audio_clock_us(), &mut output, 2, 48000, &rt_pool(vec![voice]), &Arc::new(Mutex::new(Vec::new())),
                &mut cmd, &mut status, &Arc::new(1.0_f32.to_bits().into()), &Arc::new(1u32.into()));
            output
        };
        // Legacy matrix edits mirror a mono file into input 2.
        engine.set_voice_crosspoint(playing.id, 1, 0, 1.0, true).unwrap();
        let old_snapshot = playing.inner.level_matrix.snapshot().unwrap();
        assert_eq!(render(Arc::clone(&playing)), [0.5, 0.0]);

        let mut json = AudioCue::new().serialize();
        let edit = serde_json::json!({"point_mix":{"input":2, "source":{"x":0,"y":50},
            "speakers":[{"output":1,"x":0,"y":50},{"output":2,"x":100,"y":50}]}});
        crate::commands::cue_cmds::merge_properties(&mut json, &edit, crate::commands::cue_cmds::MergePolicy::AllKeys);
        crate::commands::cue_cmds::apply_point_routing(&mut json, &edit, None, &[]).unwrap();
        let reopened = AudioCueFactory.from_json(json).unwrap();
        let audio = (reopened.as_ref() as &dyn std::any::Any).downcast_ref::<AudioCue>().unwrap();
        let rows = audio.level_matrix.as_ref().unwrap();
        assert!(rows[0].is_empty());
        assert_eq!(rows[1][0], 0.0);
        let matrix = build_level_matrix(rows, &[], audio.matrix_mirror_mono).unwrap();
        engine.set_voice_level_matrix(playing.id, Some(&matrix)).unwrap();
        assert!(old_snapshot.mirror_mono);
        assert!(!playing.inner.level_matrix.snapshot().unwrap().mirror_mono);
        let retriggered = Arc::new(Voice::new(Arc::new(vec![0.5; 16]), 1, 48000, 1.0, 0.0));
        retriggered.inner.level_matrix.update(|_| Some(matrix));
        retriggered.set_playing();
        assert_eq!(render(playing), [0.0; 2]);
        assert_eq!(render(retriggered), [0.0; 2]);
    }

    #[test]
    fn cue_effects_filter_audio_before_either_routing_path() {
        use super::super::{cue_effects::CueEffect, voice::LevelMatrix};

        let render = |matrix: Option<LevelMatrix>, enabled| {
            let channel_count = if matrix.is_some() { 3 } else { 1 };
            let mut pcm = vec![0.0; usize::from(channel_count) * 4096];
            for frame in pcm.chunks_exact_mut(usize::from(channel_count)) {
                frame[usize::from(channel_count) - 1] = 0.5;
            }
            let voice = Voice::new(Arc::new(pcm), channel_count, 48_000, 1.0, -1.0)
                .with_cue_effects(&[CueEffect::HighPass { id: Uuid::new_v4(), enabled, frequency_hz: 1_000.0 }]);
            voice.inner.level_matrix.update(|_| matrix);
            voice.set_playing();
            let pool = rt_pool(vec![Arc::new(voice)]);
            let feeds = Arc::new(Mutex::new(Vec::new()));
            let (_, mut commands) = HeapRb::<AudioCommand>::new(16).split();
            let (mut status, _) = HeapRb::<AudioStatus>::new(16).split();
            let master = Arc::new(1.0_f32.to_bits().into());
            let period = Arc::new(2048_u32.into());
            let mut output = vec![0.0; 4096];
            fill_buffer(audio_clock_us(), &mut output, 2, 48_000, &pool, &feeds, &mut commands, &mut status, &master, &period);
            output
        };
        let mut matrix = LevelMatrix::default();
        matrix.set(2, 1, 1.0);
        for (routing, output_channel) in [(None, 0), (Some(matrix), 1)] {
            let dry = render(routing.clone(), false);
            let wet = render(routing, true);
            assert_eq!(dry[4094 + output_channel], 0.5);
            assert!(wet[output_channel] > 0.4);
            assert!(wet[4094 + output_channel].abs() < 1e-6);
            assert!(wet.iter().skip(1 - output_channel).step_by(2).all(|sample| *sample == 0.0));
        }
    }

    /// Call fill_buffer for `output_frames` output frames and return the
    /// resulting frame_pos stored in the voice.
    fn run_fill(voice: Arc<Voice>, output_frames: usize, output_sr: u32) -> u64 {
        let pool = rt_pool(vec![Arc::clone(&voice)]);
        let feeds: Arc<Mutex<Vec<InputFeed>>> = Arc::new(Mutex::new(Vec::new()));
        let (_, mut cmd_cons) = HeapRb::<AudioCommand>::new(16).split();
        let (mut status_prod, _) = HeapRb::<AudioStatus>::new(16).split();
        let master  = Arc::new(std::sync::atomic::AtomicU32::new(f32::to_bits(1.0)));
        let period  = Arc::new(std::sync::atomic::AtomicU32::new(output_frames as u32));
        let mut output = vec![0.0f32; output_frames * 2];
        fill_buffer(audio_clock_us(), &mut output, 2, output_sr, &pool, &feeds, &mut cmd_cons, &mut status_prod, &master, &period);
        voice.frame_pos.load(std::sync::atomic::Ordering::Relaxed)
    }

    // The SR ratio (e.g. 44100/48000) is not exactly representable in f64, so
    // frame_pos after N output frames may be off by ±1 source frame.  All
    // assertions below allow a tolerance of 1 frame.
    fn assert_frame_pos(actual: u64, expected: u64, msg: &str) {
        let diff = (actual as i64 - expected as i64).unsigned_abs();
        assert!(diff <= 1, "{msg}: expected {expected} ± 1, got {actual}");
    }

    #[test]
    fn routing_no_device_goes_to_main() {
        // Decision covered by routes_to_main's early return; the pure helper
        // only sees explicitly-requested devices.
        assert!(resolves_to_main_device("dev-a", Some("dev-a"), None));
    }

    #[test]
    fn routing_matching_main_device() {
        assert!(resolves_to_main_device("focusrite", Some("focusrite"), None));
        assert!(!resolves_to_main_device("hdmi-out", Some("focusrite"), None));
    }

    #[test]
    fn routing_matching_system_default() {
        // Main stream on the system default: a patch naming that same device
        // explicitly must not open a duplicate aux stream.
        assert!(resolves_to_main_device("speakers", None, Some("speakers")));
        assert!(!resolves_to_main_device("hdmi-out", None, Some("speakers")));
        // Default device unknown — be conservative and open the aux stream.
        assert!(!resolves_to_main_device("hdmi-out", None, None));
    }

    #[test]
    fn stop_all_command_silences_every_voice() {
        let v1 = make_voice(1000, 2, 48000, 1.0);
        let v2 = make_voice(1000, 2, 48000, 1.0);
        let pool = rt_pool(vec![Arc::clone(&v1), Arc::clone(&v2)]);
        let feeds: Arc<Mutex<Vec<InputFeed>>> = Arc::new(Mutex::new(Vec::new()));
        let (mut cmd_prod, mut cmd_cons) = HeapRb::<AudioCommand>::new(16).split();
        let (mut status_prod, _) = HeapRb::<AudioStatus>::new(16).split();
        let master = Arc::new(std::sync::atomic::AtomicU32::new(f32::to_bits(1.0)));
        let period = Arc::new(std::sync::atomic::AtomicU32::new(64));
        cmd_prod.try_push(AudioCommand::StopAll).unwrap();

        let mut output = vec![0.0f32; 64 * 2];
        fill_buffer(audio_clock_us(), &mut output, 2, 48000, &pool, &feeds, &mut cmd_cons, &mut status_prod, &master, &period);

        for v in [&v1, &v2] {
            assert_eq!(v.voice_state(), VoiceState::Stopped);
        }
    }

    #[test]
    fn sr_ratio_44100_on_48000() {
        // 1 s of 48 kHz output = 48 000 frames → should consume 44 100 source frames.
        let voice = make_voice(220_500, 2, 44_100, 1.0);
        let pos = run_fill(voice, 48_000, 48_000);
        assert_frame_pos(pos, 44_100, "44.1 kHz file on 48 kHz output");
    }

    #[test]
    fn sr_ratio_48000_on_48000() {
        // Same SR: 1 output frame = 1 source frame exactly.
        let voice = make_voice(96_000, 2, 48_000, 1.0);
        let pos = run_fill(voice, 48_000, 48_000);
        assert_frame_pos(pos, 48_000, "48 kHz file on 48 kHz output");
    }

    #[test]
    fn sr_ratio_48000_on_44100() {
        // 1 s of 44.1 kHz output = 44 100 frames → should consume 48 000 source frames.
        let voice = make_voice(96_000, 2, 48_000, 1.0);
        let pos = run_fill(voice, 44_100, 44_100);
        assert_frame_pos(pos, 48_000, "48 kHz file on 44.1 kHz output");
    }

    #[test]
    fn user_rate_2x_on_matching_sr() {
        // rate=2.0 on matching SR: 2 source frames per output frame.
        let voice = make_voice(96_000, 2, 48_000, 2.0);
        let pos = run_fill(voice, 48_000, 48_000);
        assert_frame_pos(pos, 96_000, "rate=2.0 should consume 96 000 frames in 1 s");
    }

    #[test]
    fn sr_ratio_96000_on_48000() {
        // 96 kHz file on 48 kHz output: rate step = 2.0, correct duration.
        // Note: no anti-aliasing filter — content above 24 kHz may alias, but
        // in practice 96 kHz files are already band-limited below 20 kHz.
        let voice = make_voice(192_000, 2, 96_000, 1.0);
        let pos = run_fill(voice, 48_000, 48_000);
        assert_frame_pos(pos, 96_000, "96 kHz file on 48 kHz output: 1 s = 96 000 source frames");
    }

    // ── Live input feed / resampler ────────────────────────────────────────

    /// Build a test feed (no real device) plus its ring producer.
    fn make_feed(in_channels: usize) -> (InputFeed, ringbuf::HeapProd<f32>) {
        let (prod, cons) = HeapRb::<f32>::new(STAGING_FRAMES * in_channels).split();
        let feed = InputFeed {
            id: Uuid::new_v4(),
            device_id: "test".into(),
            in_channels,
            sample_rate: 48_000,
            cons,
            staging: vec![0.0_f32; STAGING_FRAMES * in_channels].into_boxed_slice(),
            write_frame: 0,
            _capture: None,
        };
        (feed, prod)
    }

    #[test]
    fn feed_drain_advances_write_and_interpolates() {
        let (mut feed, mut prod) = make_feed(1);
        for i in 0..4 {
            let _ = prod.try_push(i as f32); // ramp 0,1,2,3
        }
        feed.drain();
        assert_eq!(feed.write_frame, 4);
        assert_eq!(feed.sample(0, 0.0), 0.0);
        assert_eq!(feed.sample(0, 2.0), 2.0);
        assert!((feed.sample(0, 1.5) - 1.5).abs() < 1e-6, "linear interp between frames");
    }

    #[test]
    fn mix_live_unity_ratio_routes_input() {
        let (mut feed, mut prod) = make_feed(2);
        for _ in 0..2000 {
            let _ = prod.try_push(0.5); // L
            let _ = prod.try_push(0.25); // R
        }
        feed.drain();
        let feeds = vec![feed];

        let live = LiveSource::new(feeds[0].id, 0, 1, 48_000);
        let voice = Arc::new(Voice::new_live(live, 48_000, 1.0, 0.0)); // unity gain, center pan
        let (mut status_prod, _) = HeapRb::<AudioStatus>::new(64).split();
        let mut out = vec![0.0_f32; 256 * 2];
        let mut peaks = [0.0; 2];
        let period = Arc::new(std::sync::atomic::AtomicU32::new(256));

        mix_live(&mut out, 2, 48_000, &voice, &feeds, &mut status_prod, [0, 1], &mut peaks, &mut [0.0; 2], &period);

        // Center pan → both gains = sqrt(0.5) ≈ 0.707; L = 0.5·0.707 ≈ 0.354.
        assert!(out[0] > 0.34 && out[0] < 0.37, "L sample was {}", out[0]);
        assert!(out[1] > 0.16 && out[1] < 0.19, "R sample was {}", out[1]);

        // in_sr == out_sr, target_lag = 3 × period = 768, read advances 256.
        let read = voice.live.as_ref().unwrap().read_frame();
        let target_lag = 3.0 * 256.0_f64;
        let expected = (feeds[0].write_frame as f64 - target_lag) + 256.0;
        assert!((read - expected).abs() < 5.0, "read {read} vs expected {expected}");
    }

    // ── End-of-file / loop / simultaneous / seek (RT completion logic) ──────

    /// A voice whose PCM is a steady 0.5 tone (non-silent) for contrast tests.
    fn make_tone_voice(n_frames: usize, sr: u32) -> Arc<Voice> {
        let samples = Arc::new(vec![0.5f32; n_frames * 2]);
        let v = Voice::new(samples, 2, sr, 1.0, 0.0);
        v.set_playing();
        Arc::new(v)
    }

    /// Run one 256-frame callback over `pool`; return the (drained) statuses.
    fn run_block(pool: &Arc<ArcSwap<Vec<Arc<Voice>>>>, cmd: Option<AudioCommand>) -> Vec<AudioStatus> {
        use ringbuf::traits::Consumer;
        let feeds: Arc<Mutex<Vec<InputFeed>>> = Arc::new(Mutex::new(Vec::new()));
        let (mut cmd_prod, mut cmd_cons) = HeapRb::<AudioCommand>::new(16).split();
        if let Some(c) = cmd {
            cmd_prod.try_push(c).unwrap();
        }
        let (mut status_prod, mut status_cons) = HeapRb::<AudioStatus>::new(64).split();
        let master = Arc::new(std::sync::atomic::AtomicU32::new(f32::to_bits(1.0)));
        let period = Arc::new(std::sync::atomic::AtomicU32::new(256));
        let mut out = vec![0.0f32; 256 * 2];
        fill_buffer(audio_clock_us(), &mut out, 2, 48_000, pool, &feeds, &mut cmd_cons, &mut status_prod, &master, &period);
        let mut statuses = Vec::new();
        while let Some(s) = status_cons.try_pop() {
            statuses.push(s);
        }
        statuses
    }

    #[test]
    fn canceled_audition_cannot_launch_late_or_stop_a_show_voice() {
        let engine = AudioEngine::new_silent(&MachineAudioConfig::default());
        let live = make_tone_voice(1024, 48_000);
        assert!(engine.voices.push(Arc::clone(&live)).is_ok());
        let first = Uuid::new_v4();
        let next = Uuid::new_v4();
        engine.reserve_audition(first).unwrap();
        engine.reserve_audition(next).unwrap();
        engine.stop_audition(Some(first));
        engine.stop_audition(Some(live.id));
        assert_eq!(live.voice_state(), VoiceState::Playing);
        assert_eq!(engine.audition.lock().unwrap().pending, Some(next));
        engine.panic_stop_all().unwrap();
        let clip = super::super::audition::AuditionClip {
            samples: Arc::new(vec![0.5, 0.5]), channels: 2, sample_rate: 48_000,
            start_ms: 0, end_ms: None, gain: 1.0, pan: 0.0,
        };
        assert_eq!(engine.start_audition(next, "pw:phones", [], clip), Err("Audition canceled.".into()));
        assert_eq!(live.voice_state(), VoiceState::Stopped);
    }

    #[test]
    fn output_loss_rejects_new_starts_and_panic_clears_existing_voices() {
        let engine = AudioEngine::new_silent(&MachineAudioConfig::default());
        let make = || Voice::new(Arc::new(vec![0.5; 1024]), 2, 48000, 1.0, 0.0);
        assert!(engine.play_voice(make()).is_err());
        assert!(engine.play_voice_paused(make()).is_err());
        assert_eq!(engine.voices.with(|p| p.len()), Some(0));
        let old = make_tone_voice(1024, 48000);
        assert!(engine.voices.push(Arc::clone(&old)).is_ok());
        engine.stop_voice(old.id, 500, FadeCurve::SCurve).unwrap();
        old.prepare_for_output_restart();
        assert_eq!(old.voice_state(), VoiceState::Stopped);
        let active = make_tone_voice(1024, 48000);
        assert!(engine.voices.push(Arc::clone(&active)).is_ok());
        engine.panic_stop_all().unwrap();
        assert_eq!(active.voice_state(), VoiceState::Stopped);
    }

    #[test]
    fn output_loss_before_first_render_retires_voice_without_follow() {
        let voice = make_tone_voice(4096, 48000);
        let now = audio_clock_us();
        voice.start_deadline_us.store(now, std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(1));
        let statuses = run_block(&rt_pool(vec![Arc::clone(&voice)]), None);
        assert_eq!(voice.voice_state(), VoiceState::Stopped);
        assert_eq!(voice.current_frame(), 0);
        assert!(statuses.iter().any(|s| matches!(s, AudioStatus::StartExpired { voice_id } if *voice_id == voice.id)));
        assert!(!statuses.iter().any(|s| matches!(s, AudioStatus::Completed { .. })));
    }

    #[test]
    fn eof_stops_voice_and_emits_completed() {
        // 100 source frames, 256-frame block at matched SR → runs past the end.
        let voice = make_voice(100, 2, 48_000, 1.0);
        let id = voice.id;
        let pool = rt_pool(vec![Arc::clone(&voice)]);
        let statuses = run_block(&pool, None);

        assert_eq!(voice.voice_state(), VoiceState::Stopped, "voice must stop at EOF");
        assert!(
            statuses.iter().any(|s| matches!(s, AudioStatus::Completed { voice_id } if *voice_id == id)),
            "EOF must emit exactly one Completed for the voice"
        );
    }

    /// Install a slice program on a not-yet-running voice (test-side stand-in
    /// for the AudioCue GO path).
    fn set_slices(voice: &Arc<Voice>, segs: &[(u64, u64, u32)]) {
        use crate::engine::voice::{SliceProgram, SliceSegment};
        let program = SliceProgram::new(
            segs.iter()
                .map(|&(s, e, c)| SliceSegment { start_frame: s, end_frame: e, play_count: c })
                .collect(),
        )
        .expect("non-empty slice program");
        // SAFETY: the voice has not been submitted to the RT pool yet.
        unsafe { *voice.inner.slices.get() = Some(program) };
        voice.frame_pos.store(segs[0].0, std::sync::atomic::Ordering::Relaxed);
    }

    #[test]
    fn sliced_voice_vamps_on_infinite_segment() {
        // 300 frames, slices: [0,100)×1 → [100,200)×∞ → [200,300)×1.
        let voice = make_voice(300, 2, 48_000, 1.0);
        set_slices(&voice, &[(0, 100, 1), (100, 200, u32::MAX), (200, 300, 1)]);
        let pool = rt_pool(vec![Arc::clone(&voice)]);
        let statuses = run_block(&pool, None);

        assert_eq!(voice.voice_state(), VoiceState::Playing, "vamp must keep playing");
        assert!(
            !statuses.iter().any(|s| matches!(s, AudioStatus::Completed { .. })),
            "vamping must not complete"
        );
        let pos = voice.frame_pos.load(std::sync::atomic::Ordering::Relaxed);
        assert!((100..200).contains(&pos), "position must stay inside the vamp, got {pos}");
    }

    #[test]
    fn devamp_continue_releases_the_vamp_and_plays_through() {
        let voice = make_voice(300, 2, 48_000, 1.0);
        set_slices(&voice, &[(100, 200, u32::MAX), (200, 300, 1)]);
        let id = voice.id;
        let pool = rt_pool(vec![Arc::clone(&voice)]);

        // Block 1: establish the vamp.
        run_block(&pool, None);
        assert_eq!(voice.voice_state(), VoiceState::Playing);

        // Block 2: devamp → finish the pass, play the last slice, complete.
        let statuses = run_block(&pool, Some(AudioCommand::Devamp { voice_id: id, stop_at_end: false }));
        assert_eq!(voice.voice_state(), VoiceState::Stopped, "must play through to the end");
        assert!(
            statuses.iter().any(|s| matches!(s, AudioStatus::Completed { voice_id } if *voice_id == id)),
            "devamped voice must complete at file end"
        );
    }

    #[test]
    fn devamp_stop_ends_at_the_slice_boundary() {
        let voice = make_voice(300, 2, 48_000, 1.0);
        set_slices(&voice, &[(100, 200, u32::MAX), (200, 300, 1)]);
        let id = voice.id;
        let pool = rt_pool(vec![Arc::clone(&voice)]);
        run_block(&pool, None);

        let statuses = run_block(&pool, Some(AudioCommand::Devamp { voice_id: id, stop_at_end: true }));
        assert_eq!(voice.voice_state(), VoiceState::Stopped, "stop-at-end must stop the voice");
        assert!(
            statuses.iter().any(|s| matches!(s, AudioStatus::Completed { voice_id } if *voice_id == id)),
            "stop-at-end must emit Completed"
        );
        let pos = voice.frame_pos.load(std::sync::atomic::Ordering::Relaxed);
        assert!(pos <= 200, "must not play past the slice boundary, got {pos}");
    }

    #[test]
    fn devamp_continue_emits_slice_boundary_while_the_voice_keeps_playing() {
        // Infinite vamp slice followed by a long tail slice: a continue-Devamp
        // must emit SliceBoundary at the vamp's end while the voice plays on
        // into the next slice instead of stopping.
        let voice = make_voice(1000, 2, 48_000, 1.0);
        set_slices(&voice, &[(0, 100, u32::MAX), (100, 1000, u32::MAX)]);
        let id = voice.id;
        let pool = rt_pool(vec![Arc::clone(&voice)]);
        run_block(&pool, None); // establish the vamp in slice 0

        let statuses = run_block(&pool, Some(AudioCommand::Devamp { voice_id: id, stop_at_end: false }));
        assert!(
            statuses.iter().any(|s| matches!(s, AudioStatus::SliceBoundary { voice_id } if *voice_id == id)),
            "continue-Devamp must emit SliceBoundary at the slice end"
        );
        assert_eq!(voice.voice_state(), VoiceState::Playing, "voice must continue into the next slice");
        assert!(
            !statuses.iter().any(|s| matches!(s, AudioStatus::Completed { voice_id } if *voice_id == id)),
            "continue must not complete the voice"
        );
        let pos = voice.frame_pos.load(std::sync::atomic::Ordering::Relaxed);
        assert!(pos >= 100, "must have advanced past the boundary, got {pos}");

        // The boundary fires once per Devamp, not on every later pass.
        let statuses = run_block(&pool, None);
        assert!(
            !statuses.iter().any(|s| matches!(s, AudioStatus::SliceBoundary { .. })),
            "no repeat SliceBoundary without a new Devamp"
        );
    }

    #[test]
    fn voice_levels_report_block_peak_and_rms() {
        // Constant 0.5 signal, unity gain: peak and RMS must both land at 0.5.
        let samples = Arc::new(vec![0.5f32; 2000]);
        let voice = Voice::new(samples, 2, 48_000, 1.0, 0.0);
        voice.set_playing();
        let voice = Arc::new(voice);
        let pool = rt_pool(vec![Arc::clone(&voice)]);
        let _ = run_block(&pool, None);
        use std::sync::atomic::Ordering::Relaxed;
        let peak_l = f32::from_bits(voice.inner.live_peak[0].load(Relaxed));
        let rms_l = f32::from_bits(voice.inner.live_rms[0].load(Relaxed));
        // Center pan applies the constant-power gain (cos(pi/4) = 0.7071).
        let expected = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        assert!((peak_l - expected).abs() < 0.01, "peak mismatch: {peak_l}");
        assert!((rms_l - expected).abs() < 0.01, "rms mismatch: {rms_l}");
    }

    #[test]
    fn sliced_finite_counts_replay_then_advance() {
        // [0,100)×2 → [100,200)×1: one 256-frame block plays 100+100 frames of
        // segment 0, then enters segment 1 (~56 frames in).
        let voice = make_voice(300, 2, 48_000, 1.0);
        set_slices(&voice, &[(0, 100, 2), (100, 200, 1)]);
        let pool = rt_pool(vec![Arc::clone(&voice)]);
        let statuses = run_block(&pool, None);

        assert_eq!(voice.voice_state(), VoiceState::Playing);
        assert!(!statuses.iter().any(|s| matches!(s, AudioStatus::Completed { .. })));
        let pos = voice.frame_pos.load(std::sync::atomic::Ordering::Relaxed);
        assert!((100..200).contains(&pos), "expected ~156 inside segment 1, got {pos}");
    }

    #[test]
    fn finite_loop_wraps_and_decrements_without_stopping() {
        // 200 frames, loop once (loops_remaining=1); 256-frame block wraps once
        // (dec to 0) then keeps playing from the top — must NOT stop yet.
        let voice = make_voice(200, 2, 48_000, 1.0);
        voice.inner.loops_remaining.store(1, std::sync::atomic::Ordering::Relaxed);
        let pool = rt_pool(vec![Arc::clone(&voice)]);
        let statuses = run_block(&pool, None);

        assert_eq!(
            voice.inner.loops_remaining.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "one wrap must decrement the finite loop counter"
        );
        assert_eq!(voice.voice_state(), VoiceState::Playing, "still playing mid-loop");
        assert!(
            !statuses.iter().any(|s| matches!(s, AudioStatus::Completed { .. })),
            "a wrap must not emit Completed"
        );
        let pos = voice.frame_pos.load(std::sync::atomic::Ordering::Relaxed);
        assert!(pos < 100, "after wrapping 200-frame file within 256 frames, pos≈56, got {pos}");
    }

    #[test]
    fn infinite_loop_never_completes() {
        // loops_remaining = u32::MAX; play 4000 frames (20× past a 200-frame file).
        let voice = make_voice(200, 2, 48_000, 1.0);
        voice.inner.loops_remaining.store(u32::MAX, std::sync::atomic::Ordering::Relaxed);
        let pool = rt_pool(vec![Arc::clone(&voice)]);
        for _ in 0..16 {
            let statuses = run_block(&pool, None);
            assert!(
                !statuses.iter().any(|s| matches!(s, AudioStatus::Completed { .. })),
                "infinite loop must never emit Completed"
            );
        }
        assert_eq!(
            voice.inner.loops_remaining.load(std::sync::atomic::Ordering::Relaxed),
            u32::MAX,
            "infinite loop counter must not decrement"
        );
        assert_eq!(voice.voice_state(), VoiceState::Playing);
    }

    #[test]
    fn playlist_audio_step_replaces_the_exact_voice_only_on_an_admitted_block() {
        use crate::engine::voice::PlaylistCut;
        use std::sync::atomic::{AtomicU32, Ordering};

        for timely in [false, true] {
            let pool = VoicePool::new();
            let old = Arc::new(Voice::new(Arc::new(vec![0.25; 8192]), 1, 48_000, 1.0, -1.0));
            old.set_playing();
            assert!(pool.push(old.clone()).is_ok());
            let cut = Arc::new(PlaylistCut::new(Some(old.id)));
            let mut new = Voice::new(Arc::new(vec![0.5; 8192]), 1, 48_000, 1.0, -1.0);
            new.playlist_cut = Some(cut.clone());
            let deadline = first_render_window_us(2048, 48_000);
            new.start_deadline_us.store(deadline, Ordering::Release);
            new.set_playing();
            assert!(pool.push(Arc::new(new)).is_ok());
            let (_, mut commands) = HeapRb::new(16).split();
            let (mut status, _) = HeapRb::new(16).split();
            let mut output = [0.0; 4096];
            let timestamp = if timely { 42_667 } else { deadline + 1 };
            fill_buffer(timestamp, &mut output, 2, 48_000, &pool.rt, &Arc::new(Mutex::new(Vec::new())),
                &mut commands, &mut status, &Arc::new(AtomicU32::new(1.0_f32.to_bits())),
                &Arc::new(AtomicU32::new(2048)));
            assert!(!cut.pending());
            assert_eq!(cut.started(), timely);
            assert_eq!(old.voice_state() == VoiceState::Stopped, timely);
            let level = if timely { 0.5 } else { 0.25 };
            assert!(output.chunks_exact(2).all(|frame| frame == [level, 0.0]));
        }
    }

    #[test]
    fn staged_stems_start_together_even_when_a_callback_runs_between_submissions() {
        use crate::engine::voice::{AudioStartBatch, LevelMatrix};
        use std::sync::atomic::Ordering;

        let batch = Arc::new(AudioStartBatch::new());
        let make_stem = |output, amplitude, deadline| {
            let mut stem = Voice::new(Arc::new(vec![amplitude; 1024]), 1, 48_000, 1.0, 0.0);
            let mut matrix = LevelMatrix::default();
            matrix.set(0, output, 1.0);
            stem.inner.level_matrix.update(|_| Some(matrix));
            stem.start_batch = Some(Arc::clone(&batch));
            stem.start_deadline_us.store(deadline, Ordering::Relaxed);
            stem.set_playing();
            Arc::new(stem)
        };
        let left = make_stem(0, 0.25, 251_000);
        let right = make_stem(1, 0.5, 561_000);
        let interrupted = make_stem(0, 0.1, 1);
        let before_release = AudioStartBatch::current_epoch();
        let pool = rt_pool(vec![Arc::clone(&left)]);

        let render = |timestamp| {
            let feeds = Arc::new(Mutex::new(Vec::new()));
            let (_, mut commands) = HeapRb::<AudioCommand>::new(16).split();
            let (mut status, mut statuses) = HeapRb::<AudioStatus>::new(64).split();
            let master = Arc::new(std::sync::atomic::AtomicU32::new(1.0f32.to_bits()));
            let period = Arc::new(std::sync::atomic::AtomicU32::new(256));
            let mut output = [0.0; 8];
            fill_buffer(timestamp, &mut output, 2, 48_000, &pool, &feeds, &mut commands, &mut status, &master, &period);
            let mut expired = Vec::new();
            while let Some(event) = statuses.try_pop() {
                if let AudioStatus::StartExpired { voice_id } = event {
                    expired.push(voice_id);
                }
            }
            (output, expired)
        };

        for timestamp in [101_000, 201_000, 301_000] {
            assert_eq!(render(timestamp), ([0.0; 8], Vec::new()));
        }
        assert_eq!(left.frame_pos.load(Ordering::Relaxed), 0);
        pool.store(Arc::new(vec![Arc::clone(&left), Arc::clone(&right), Arc::clone(&interrupted)]));
        assert_eq!(render(311_000), ([0.0; 8], vec![interrupted.id]));

        batch.release();
        assert!(!batch.released_at(before_release));
        assert_eq!(render(321_000), ([0.25, 0.5, 0.25, 0.5, 0.25, 0.5, 0.25, 0.5], Vec::new()));
        assert_eq!(left.frame_pos.load(Ordering::Relaxed), 4);
        assert_eq!(right.frame_pos.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn simultaneous_voices_all_advance() {
        let v1 = make_voice(100_000, 2, 48_000, 1.0);
        let v2 = make_voice(100_000, 2, 48_000, 1.0);
        let pool = rt_pool(vec![Arc::clone(&v1), Arc::clone(&v2)]);
        run_block(&pool, None);
        for v in [&v1, &v2] {
            let pos = v.frame_pos.load(std::sync::atomic::Ordering::Relaxed);
            assert!((pos as i64 - 256).abs() <= 1, "each simultaneous voice advances ~256, got {pos}");
        }
    }

    #[test]
    fn seek_command_repositions_before_mixing() {
        // Seek is applied before the voice loop, so pos = seek_target + block.
        let voice = make_voice(100_000, 2, 48_000, 1.0);
        let id = voice.id;
        let pool = rt_pool(vec![Arc::clone(&voice)]);
        run_block(&pool, Some(AudioCommand::Seek { voice_id: id, frame_pos: 5_000 }));
        let pos = voice.frame_pos.load(std::sync::atomic::Ordering::Relaxed);
        assert!((pos as i64 - 5_256).abs() <= 1, "seek(5000)+256 frames → ~5256, got {pos}");
    }

    #[test]
    fn voice_pool_reclaims_a_pinned_callback_snapshot_on_the_writer() {
        let pool = VoicePool::new();
        let original = make_tone_voice(1000, 48_000);
        let lifetime = Arc::downgrade(&original);
        assert!(pool.push(original).is_ok());
        let callback = pool.rt.load();
        assert_eq!(callback.len(), 1);
        pool.retain(|_| false);
        assert!(pool.rt.load().is_empty());

        // The callback is still mixing its old list while other voices arrive.
        for _ in 0..10 {
            assert!(pool.push(make_tone_voice(1000, 48_000)).is_ok());
        }
        assert_eq!(pool.retained.lock().unwrap().len(), 1);
        drop(callback);
        assert!(lifetime.upgrade().is_some(), "callback did not destroy the removed voice");
        pool.retain(|_| false);
        assert!(lifetime.upgrade().is_none(), "writer reclaimed the voice");
        assert!(pool.retained.lock().unwrap().is_empty());
    }

    #[test]
    fn callback_mixes_the_published_snapshot_with_no_lock() {
        // Replaces the old A1 proof: there is no longer a lock the callback can be
        // starved on, so a freshly published tone always reaches the output —
        // the "block of silence on contention" failure mode is gone.
        let pool = VoicePool::new();
        assert!(pool.push(make_tone_voice(100_000, 48_000)).is_ok(), "push");

        let feeds: Arc<Mutex<Vec<InputFeed>>> = Arc::new(Mutex::new(Vec::new()));
        let (_p, mut cmd_cons) = HeapRb::<AudioCommand>::new(16).split();
        let (mut status_prod, _sc) = HeapRb::<AudioStatus>::new(16).split();
        let master = Arc::new(std::sync::atomic::AtomicU32::new(f32::to_bits(1.0)));
        let period = Arc::new(std::sync::atomic::AtomicU32::new(256));
        let mut out = vec![0.0f32; 256 * 2];
        fill_buffer(audio_clock_us(), &mut out, 2, 48_000, &pool.rt, &feeds, &mut cmd_cons, &mut status_prod, &master, &period);
        assert!(out.iter().any(|s| s.abs() > 0.1), "the published tone must reach the output");
    }

    // ── Silent mode (no audio device at all) ─────────────────────────────────

    #[test]
    fn silent_engine_reports_a_failed_stream() {
        let engine = AudioEngine::new_silent(&MachineAudioConfig::default());

        let health = engine.audio_health();
        assert!(health.failed, "silent mode must report the fault immediately");
        assert!(!health.in_fallback, "there was no device to fall back from");
        assert_eq!(engine.last_callback_us(), 0, "no callback can fire without a stream");
    }

    #[test]
    fn silent_engine_keeps_a_usable_format() {
        // The rest of the app (decode, resampling, patch routing) reads these;
        // they must be sane defaults rather than zeroes.
        let engine = AudioEngine::new_silent(&MachineAudioConfig::default());
        assert_eq!(engine.sample_rate(), 48_000);
        assert_eq!(engine.output_channels(), 2);
    }

    #[test]
    fn silent_engine_preserves_the_operator_device_choice() {
        // `desired_config` drives the watchdog banner and the manual restore —
        // starting silent must not silently rewrite the operator's selection.
        let config = MachineAudioConfig {
            device_id: Some("Focusrite Scarlett".into()),
            ..MachineAudioConfig::default()
        };
        let engine = AudioEngine::new_silent(&config);
        assert_eq!(
            engine.audio_health().desired_device.as_deref(),
            Some("Focusrite Scarlett"),
        );
    }
}
