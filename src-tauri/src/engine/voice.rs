//! [`Voice`] — a single active audio stream in the voice pool.
//!
//! Mutable fields that are modified inside the audio callback use
//! [`std::cell::UnsafeCell`] (for fade state and loop counter, which are only
//! ever mutated from the callback), or [`std::sync::atomic`] types (for fields
//! that may also be written via ring-buffer commands processed inside the same
//! callback).

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use super::output_trim::{ChannelTrims, OutputGains};
use super::rate_fader::{RateFader, RateSlew, ScalarFader, ScalarSlew};
use super::ring_command::{FadeCurve, VoiceId};

// ---------------------------------------------------------------------------
// Voice state (atomic, shared with the audio callback)
// ---------------------------------------------------------------------------

/// Playback state, stored as u8 for atomic access.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    Idle = 0,
    Playing = 1,
    Paused = 2,
    FadingOut = 3,
    Stopped = 4,
}

impl From<u8> for VoiceState {
    fn from(v: u8) -> Self {
        match v {
            1 => VoiceState::Playing,
            2 => VoiceState::Paused,
            3 => VoiceState::FadingOut,
            4 => VoiceState::Stopped,
            _ => VoiceState::Idle,
        }
    }
}

// ---------------------------------------------------------------------------
// Fade state (used inside the audio callback — no allocations)
// ---------------------------------------------------------------------------

/// Direction of an active fade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FadeDirection {
    In,
    Out,
}

/// Fade progress tracked entirely with primitives so the audio callback can
/// update it without locks.
pub struct FadeState {
    pub direction: FadeDirection,
    /// Total fade duration in samples.
    pub total_samples: u64,
    /// Samples elapsed so far.
    pub elapsed_samples: u64,
    /// Shape of the fade curve.
    pub curve: FadeCurve,
}

impl FadeState {
    /// Compute the current gain multiplier [0.0, 1.0] from elapsed progress
    /// using the configured curve.
    pub fn gain(&self) -> f32 {
        if self.total_samples == 0 {
            return match self.direction {
                FadeDirection::In => 1.0,
                FadeDirection::Out => 0.0,
            };
        }
        let t = (self.elapsed_samples as f64 / self.total_samples as f64).clamp(0.0, 1.0);
        let s = self.curve.apply(t);
        match self.direction {
            FadeDirection::In => s as f32,
            FadeDirection::Out => (1.0 - s) as f32,
        }
    }

    /// Advance by `n` samples.  Returns `true` when the fade is complete.
    pub fn advance(&mut self, n: u64) -> bool {
        self.elapsed_samples = (self.elapsed_samples + n).min(self.total_samples);
        self.elapsed_samples >= self.total_samples
    }
}

// ---------------------------------------------------------------------------
// Slice program (QLab-style slices — used inside the audio callback)
// ---------------------------------------------------------------------------

/// Devamp request values for [`VoiceInner::devamp_request`].
pub const DEVAMP_NONE: u8 = 0;
/// Release the current slice's loop: finish the pass, then continue.
pub const DEVAMP_CONTINUE: u8 = 1;
/// Release the loop and stop the voice at the end of the current slice.
pub const DEVAMP_STOP: u8 = 2;

/// One slice segment, resolved to frames at GO time.
#[derive(Debug, Clone, Copy)]
pub struct SliceSegment {
    pub start_frame: u64,
    pub end_frame: u64,
    /// Times to play this segment; `u32::MAX` = vamp (infinite).
    pub play_count: u32,
}

/// Playback program for a sliced voice. Built once before `play_voice()`;
/// `current`/`remaining` are mutated **only** inside the audio callback.
/// The `segments` Vec is never resized after submission (no RT allocation).
#[derive(Debug, Clone)]
pub struct SliceProgram {
    pub segments: Vec<SliceSegment>,
    /// Index of the segment being played.
    pub current: usize,
    /// Remaining *additional* passes of the current segment
    /// (`u32::MAX` = vamp).
    pub remaining: u32,
}

impl SliceProgram {
    /// Build a program from `(start_frame, end_frame, play_count)` triples.
    /// Returns `None` for an empty list (plain linear playback).
    pub fn new(segments: Vec<SliceSegment>) -> Option<Self> {
        let first = segments.first()?;
        let remaining = first.play_count.saturating_sub(1);
        let remaining = if first.play_count == u32::MAX { u32::MAX } else { remaining };
        Some(Self { segments, current: 0, remaining })
    }
}

// ---------------------------------------------------------------------------
// Level matrix
// ---------------------------------------------------------------------------

pub const MATRIX_INPUTS: usize = 128;
pub const MATRIX_OUTPUTS: usize = 128;

/// Sparse physical-output routes. Only connected crosspoints enter the sample loop.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelMatrix {
    /// Publish source interpretation in the same snapshot as the crosspoints.
    pub(crate) mirror_mono: bool,
    outputs: Vec<Vec<(u8, f32)>>,
}

impl Default for LevelMatrix {
    fn default() -> Self { Self { outputs: Vec::new(), mirror_mono: true } }
}

impl LevelMatrix {
    pub fn set(&mut self, input: usize, output: usize, gain: f32) {
        if input >= MATRIX_INPUTS || output >= MATRIX_OUTPUTS { return; }
        if output >= self.outputs.len() {
            if gain == 0.0 { return; }
            self.outputs.resize_with(output + 1, Vec::new);
        }
        let routes = &mut self.outputs[output];
        let key = input as u8;
        match routes.binary_search_by_key(&key, |&(channel, _)| channel) {
            Ok(at) if gain == 0.0 => { routes.remove(at); }
            Ok(at) => routes[at].1 = gain,
            Err(at) if gain != 0.0 => routes.insert(at, (key, gain)),
            _ => {}
        }
    }

    /// Multiple logical patch channels may feed the same physical output.
    pub(crate) fn add(&mut self, input: usize, output: usize, gain: f32) {
        if gain != 0.0 { self.set(input, output, self.gain(input as u8, output) + gain); }
    }

    #[cfg(test)]
    pub(crate) fn width(&self) -> usize { self.outputs.len() }

    pub(crate) fn gain(&self, input: u8, output: usize) -> f32 {
        self.outputs.get(output).and_then(|routes| routes.iter().find(|p| p.0 == input))
            .map_or(0.0, |p| p.1)
    }

    pub(super) fn mix(&self, samples: &[f32], level: f32, trim: Option<&OutputGains>, output: &mut [f32], peaks: &mut [f32; 2]) {
        for (channel, (destination, routes)) in output.iter_mut().zip(&self.outputs).enumerate() {
            let mut sum = 0.0;
            for &(input, gain) in routes {
                if let Some(sample) = samples.get(usize::from(input)) { sum += sample * gain; }
            }
            let mixed = sum * level * trim.map_or(1.0, |gains| gains[channel] as f32);
            *destination += mixed;
            peaks[channel % 2] = peaks[channel % 2].max(mixed.abs());
        }
    }
}

/// Writers publish one complete routing snapshot. Retain snapshots still held by a
/// callback so the callback never has to deallocate an edited matrix.
#[derive(Default)]
pub struct MatrixRouting {
    current: arc_swap::ArcSwapOption<LevelMatrix>,
    writes: std::sync::Mutex<MatrixWrites>,
}

#[derive(Default)]
struct MatrixWrites {
    owner: Option<Uuid>,
    retired: Vec<Arc<LevelMatrix>>,
}

impl MatrixRouting {
    pub fn snapshot(&self) -> Option<std::sync::Arc<LevelMatrix>> { self.current.load_full() }

    pub fn update(&self, edit: impl FnOnce(Option<&LevelMatrix>) -> Option<LevelMatrix>) {
        let mut writes = self.writes.lock().unwrap_or_else(|poison| poison.into_inner());
        writes.owner = None;
        let replacement = edit(self.current.load().as_deref());
        self.replace(&mut writes.retired, replacement);
    }

    pub(crate) fn claim(&self, owner: Uuid, expected: &LevelMatrix) -> bool {
        let mut writes = self.writes.lock().unwrap_or_else(|poison| poison.into_inner());
        let matches = self.current.load().as_deref() == Some(expected);
        if matches { writes.owner = Some(owner); }
        matches
    }

    pub(crate) fn update_owned(&self, owner: Uuid, matrix: LevelMatrix) -> bool {
        let mut writes = self.writes.lock().unwrap_or_else(|poison| poison.into_inner());
        if writes.owner != Some(owner) { return false; }
        self.replace(&mut writes.retired, Some(matrix));
        true
    }

    fn revoke_owner(&self) {
        self.writes.lock().unwrap_or_else(|poison| poison.into_inner()).owner = None;
    }

    fn replace(&self, retired: &mut Vec<Arc<LevelMatrix>>, matrix: Option<LevelMatrix>) {
        retired.retain(|matrix| std::sync::Arc::strong_count(matrix) > 1);
        let replacement = matrix.map(Arc::new);
        if let Some(old) = self.current.swap(replacement) {
            if std::sync::Arc::strong_count(&old) > 1 { retired.push(old); }
        }
    }
}

#[cfg(test)]
#[test]
fn map_motion_cannot_overwrite_a_manual_matrix_edit_or_reclaim_the_wrong_start() {
    let router = MatrixRouting::default();
    let mut start = LevelMatrix::default();
    start.set(0, 0, 1.0);
    let mut end = LevelMatrix::default();
    end.set(0, 1, 1.0);
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    router.update(|_| Some(start.clone()));
    assert!(!router.claim(first, &end));
    assert!(router.claim(first, &start));
    assert!(router.update_owned(first, end.clone()));
    assert!(router.claim(second, &end));
    assert!(!router.update_owned(first, start.clone()));
    router.update(|_| Some(start.clone()));
    assert!(!router.update_owned(second, end.clone()));
    assert!(router.claim(first, &start));
    router.revoke_owner();
    assert!(!router.update_owned(first, end));
    assert_eq!(router.snapshot().as_deref(), Some(&start));
}

// ---------------------------------------------------------------------------
// VoiceInner — wraps all RT-mutable fields in UnsafeCell / atomics
// ---------------------------------------------------------------------------

/// All fields of a `Voice` that may be mutated from the audio callback.
///
/// # Safety contract
/// `VoiceInner` is shared between the non-RT thread (via `Arc`) and the RT
/// audio callback.  The contract is:
/// - `gain` and `pan` are `AtomicU32` (f32-bits) — safe for concurrent
///   read/write from both the RT and non-RT sides.
/// - `loops_remaining` is `AtomicU32` — only *decremented* inside the callback,
///   but can be *read* by the non-RT side.
/// - `fade` and `end_frame` are `UnsafeCell` — **only ever written from the
///   audio callback after the voice has been submitted to the pool**.  The
///   non-RT side writes these fields once, *before* calling `play_voice()`, so
///   there is no data race.
pub struct VoiceInner {
    /// Linear gain (f32 bits in an AtomicU32).  Written by `SetGain` commands
    /// processed inside the callback, and by the non-RT `play_voice` path.
    pub gain_bits: AtomicU32,
    /// Stereo pan (f32 bits).
    pub pan_bits: AtomicU32,
    /// Remaining loop repetitions.  `0` = play once.  `u32::MAX` = infinite.
    pub loops_remaining: AtomicU32,
    /// Playback-rate control shared with the non-RT Fade owner.
    pub rate_fader: Arc<RateFader>,
    pub(super) rate_slew: UnsafeCell<RateSlew>,
    pub(super) cue_trim: Option<CueTrim>,
    pub(super) output_trim: Option<ChannelTrims>,
    /// Processing history is exclusively owned by the mix callback after GO.
    pub(super) effects: UnsafeCell<Option<super::cue_effects::CueEffects>>,
    pub(super) cutoff_controls: Box<[(Uuid, Arc<super::rate_fader::ScalarFader>)]>,
    pub(super) eq_gain_controls: Box<[(Uuid, Arc<super::rate_fader::ScalarFader>)]>,
    /// Active fade, if any.  Mutated exclusively from inside the callback.
    /// SAFETY: only ever accessed from the single RT callback thread.
    pub fade: UnsafeCell<Option<FadeState>>,
    /// Encoded natural fade-out duration and curve. Updated independently of
    /// a manual Stop fade so adapting to rate changes never stops PCM early.
    natural_fade_out: AtomicU64,
    /// Optional end-frame marker.  Written once before voice is submitted.
    pub end_frame: UnsafeCell<Option<u64>>,
    /// Optional slice program (QLab slices).  Written once before the voice is
    /// submitted; `current`/`remaining` mutated only inside the callback.
    /// When present, `loops_remaining` and `end_frame` are ignored — the
    /// program owns the playback boundaries.
    pub slices: UnsafeCell<Option<SliceProgram>>,
    /// Pending devamp request ([`DEVAMP_NONE`] / [`DEVAMP_CONTINUE`] /
    /// [`DEVAMP_STOP`]).  Written from the non-RT side, consumed by the
    /// callback at the next slice boundary.
    pub devamp_request: AtomicU8,
    /// Optional crosspoint routing ([`LevelMatrix`]).  Written once before the
    /// voice is submitted, read-only in the callback.  `None` — the default —
    /// leaves the voice on the original pan + `out_l`/`out_r` mix path.
    pub level_matrix: MatrixRouting,
}

/// Only file Audio cues initialize this control. The mixer owns the slew
/// after voice submission, while manual trim writes use the atomic target.
pub(super) struct CueTrim {
    pub(super) fader: ScalarFader,
    pub(super) slew: UnsafeCell<ScalarSlew>,
}

// SAFETY: `VoiceInner` is never accessed from two threads simultaneously
// except via the documented atomic fields.
unsafe impl Sync for VoiceInner {}
unsafe impl Send for VoiceInner {}

impl VoiceInner {
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
    }
    pub fn set_gain(&self, g: f32) {
        self.gain_bits.store(f32::to_bits(g), Ordering::Relaxed);
    }
    pub fn pan(&self) -> f32 {
        f32::from_bits(self.pan_bits.load(Ordering::Relaxed))
    }
    pub fn set_pan(&self, p: f32) {
        self.pan_bits.store(f32::to_bits(p), Ordering::Relaxed);
    }
    pub fn rate(&self) -> f32 {
        self.rate_fader.current()
    }
    pub fn set_rate(&self, r: f32) {
        self.rate_fader.initialize(r);
    }

    /// Replace the PCM natural fade-out. A zero duration clears it.
    pub fn set_natural_fade_out(&self, duration_ms: u32, curve: FadeCurve) {
        let shape = match curve {
            FadeCurve::Linear => 0,
            FadeCurve::Exponential => 1,
            _ => 2,
        };
        self.natural_fade_out.store((u64::from(duration_ms) << 2) | shape, Ordering::Relaxed);
    }

    /// The automatic fade-out applied near the remaining source's end.
    pub fn natural_fade_out(&self) -> Option<(u32, FadeCurve)> {
        let value = self.natural_fade_out.load(Ordering::Relaxed);
        let duration = (value >> 2) as u32;
        (duration > 0).then(|| (duration, match value & 3 {
            0 => FadeCurve::Linear,
            1 => FadeCurve::Exponential,
            _ => FadeCurve::SCurve,
        }))
    }

}

// ---------------------------------------------------------------------------
// LiveSource — live audio input read state (Mic Cue)
// ---------------------------------------------------------------------------

/// Read state for a voice fed by a live audio **input** instead of a fixed
/// sample buffer.
///
/// A live voice has no `samples`; it reads from the shared input feed identified
/// by `feed_id` (owned by the [`AudioEngine`](super::audio_engine::AudioEngine)),
/// resampling the input device clock to the output clock with adaptive drift
/// compensation.  The read cursor is mutated **only inside the audio callback**.
pub struct LiveSource {
    /// The input feed this voice reads from.
    pub feed_id: Uuid,
    /// Device input channel feeding the Left output (0-based).
    pub in_l: usize,
    /// Device input channel feeding the Right output (`== in_l` for a mono source).
    pub in_r: usize,
    /// Input device sample rate (Hz) — numerator of the base resample ratio.
    pub src_rate: u32,
    /// Absolute fractional read cursor, in input frames.  Callback-only.
    read_frame: UnsafeCell<f64>,
    /// `false` until the first callback initialises the read cursor.
    started: AtomicBool,
}

// SAFETY: `read_frame` is only ever written from the single RT callback thread.
unsafe impl Sync for LiveSource {}
unsafe impl Send for LiveSource {}

impl LiveSource {
    /// Create a live source bound to `feed_id`, taking input channels `in_l` /
    /// `in_r` (equal for mono) from a device running at `src_rate`.
    pub fn new(feed_id: Uuid, in_l: usize, in_r: usize, src_rate: u32) -> Self {
        Self {
            feed_id,
            in_l,
            in_r,
            src_rate,
            read_frame: UnsafeCell::new(0.0),
            started: AtomicBool::new(false),
        }
    }
    /// Current read cursor. SAFETY: only called from the audio callback.
    pub fn read_frame(&self) -> f64 {
        unsafe { *self.read_frame.get() }
    }
    /// Set the read cursor. SAFETY: only called from the audio callback.
    pub fn set_read_frame(&self, v: f64) {
        unsafe {
            *self.read_frame.get() = v;
        }
    }
    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::Relaxed)
    }
    pub fn mark_started(&self) {
        self.started.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Voice
// ---------------------------------------------------------------------------

static AUDIO_START_EPOCH: AtomicU64 = AtomicU64::new(0);
static AUDIO_START_PUBLISHER: Mutex<()> = Mutex::new(());

/// Holds a set of submitted file voices until the next mix block. The producer
/// publishes each voice before opening the gate.
pub(crate) struct AudioStartBatch(AtomicU64);

impl AudioStartBatch {
    pub(crate) fn new() -> Self {
        Self(AtomicU64::new(u64::MAX))
    }

    /// Release once all voices have been admitted. Writers serialize the gate
    /// store and epoch advance. Audio readers observe a single epoch per block
    /// and cannot observe the gate opening halfway through that block.
    pub(crate) fn release(&self) {
        let _writer = AUDIO_START_PUBLISHER.lock().unwrap_or_else(|e| e.into_inner());
        let epoch = AUDIO_START_EPOCH.load(Ordering::Relaxed) + 1;
        self.0.store(epoch, Ordering::Release);
        AUDIO_START_EPOCH.store(epoch, Ordering::Release);
    }

    pub(crate) fn current_epoch() -> u64 {
        AUDIO_START_EPOCH.load(Ordering::Acquire)
    }

    pub(crate) fn released_at(&self, epoch: u64) -> bool {
        self.0.load(Ordering::Acquire) <= epoch
    }
}

/// One Playlist entrance on the main mixer. Only its first admitted block can
/// cut the exact outgoing voice. A cancelled request cannot cut anything.
pub(crate) struct PlaylistCut {
    outgoing: Option<VoiceId>,
    state: AtomicU8,
}

impl PlaylistCut {
    const PENDING: u8 = 0;
    const STARTED: u8 = 1;
    const CANCELLED: u8 = 2;

    pub(crate) fn new(outgoing: Option<VoiceId>) -> Self {
        Self { outgoing, state: AtomicU8::new(Self::PENDING) }
    }

    pub(crate) fn pending(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::PENDING
    }
    pub(crate) fn started(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::STARTED
    }

    pub(crate) fn cancel_pending(&self) {
        let _ =
            self.state.compare_exchange(Self::PENDING, Self::CANCELLED, Ordering::AcqRel, Ordering::Acquire);
    }

    pub(crate) fn abandon(&self) {
        self.state.store(Self::CANCELLED, Ordering::Release);
    }

    pub(crate) fn admit(&self, pool: &[Arc<Voice>], outputs: (usize, usize)) -> bool {
        if !self.pending() {
            return self.started();
        }
        let outgoing = self.outgoing.and_then(|id| {
            pool.iter().find(|v| {
                v.id == id
                    && v.voice_state() == VoiceState::Playing
                    && !v.stop_requested.load(Ordering::Acquire)
                    && (v.out_l, v.out_r) == outputs
                    && v.start_deadline_us.load(Ordering::Acquire) == 0
                    && !v.patched
                    && v.live.is_none()
            })
        });
        if self.outgoing.is_some() && outgoing.is_none() {
            self.cancel_pending();
            return false;
        }
        if self
            .state
            .compare_exchange(Self::PENDING, Self::STARTED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        if let Some(old) = outgoing {
            old.set_stopped();
        }
        true
    }
}

/// Shared, thread-safe audio voice.
pub struct Voice {
    pub id: VoiceId,

    /// Fully decoded PCM samples, interleaved (L, R, L, R, …).
    pub samples: Arc<Vec<f32>>,

    /// Number of channels in `samples` (1 = mono, 2 = stereo).
    pub channels: u16,

    /// Sample rate of the decoded audio (e.g., 44100, 48000).
    pub sample_rate: u32,

    /// Start of the trimmed loop, immutable after submission.
    pub(crate) loop_start_frame: u64,

    /// Current read position in frames (atomic).
    pub frame_pos: AtomicU64,

    /// [`VoiceState`] encoded as u8.
    pub state: AtomicU8,

    /// Latest acceptable first render of a new GO, in engine clock microseconds.
    pub(crate) start_deadline_us: AtomicU64,
    /// Optional shared gate for audio voices launched by one simultaneous Group.
    pub(crate) start_batch: Option<Arc<AudioStartBatch>>,
    pub(crate) playlist_cut: Option<Arc<PlaylistCut>>,
    /// A stop requested before a stream swap must survive the discarded ring.
    pub(crate) stop_requested: AtomicBool,
    /// Present only on an unsliced, looping File Audio voice (a movie's
    /// soundtrack included). Its owner can request a stop at the next
    /// whole-clip boundary without changing its loop count.
    pub(crate) whole_loop_release: Option<AtomicBool>,

    /// Interior-mutable fields modified from the RT callback.
    pub inner: Arc<VoiceInner>,

    /// Zero-based index of the output channel that receives the Left mix.
    /// Defaults to 0.  Set from the cue's Output Patch before submitting.
    pub out_l: usize,
    /// Zero-based index of the output channel that receives the Right mix.
    /// Defaults to 1.  Set from the cue's Output Patch before submitting.
    pub out_r: usize,

    /// When `Some`, this is a live (Mic Cue) voice that reads from an input feed
    /// instead of `samples`.  `None` for ordinary file/decoded voices.
    pub live: Option<Arc<LiveSource>>,

    /// `true` when an Output Patch set `out_l`/`out_r` explicitly.  Unpatched
    /// voices are shifted to the engine's default output offset (the selected
    /// ASIO pair) at submission.  Written once before `play_voice()`.
    pub patched: bool,
    /// Exact output requested by a File Audio cue, captured at GO.
    pub named_output: Option<super::device_manager::NamedOutput>,
    /// Stable output patch identity for file Audio route edits.
    pub(crate) output_patch_id: Option<Uuid>,
    /// Set by the exact main stream before publishing this file Audio voice.
    pub(super) patch_effects: Option<Arc<super::patch_effects::PatchBus>>,
    /// Shared patch master, read once per mix block.
    pub patch_fader: Option<Arc<super::patch_fader::PatchFader>>,
    /// Index of the patch in the workspace table at GO time — the per-patch
    /// VU accumulator slot (`PATCH_VU_SLOTS`-bounded).  `None` = unmetered.
    pub patch_slot: Option<u8>,
}

impl Voice {
    pub fn patch_gain(&self) -> f32 { self.patch_fader.as_ref().map_or(1.0, |f| f.gain()) }

    /// Create a new idle voice from pre-decoded samples.
    pub fn new(
        samples: Arc<Vec<f32>>,
        channels: u16,
        sample_rate: u32,
        gain: f32,
        pan: f32,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            samples,
            channels,
            sample_rate,
            frame_pos: AtomicU64::new(0),
            loop_start_frame: 0,
            start_deadline_us: AtomicU64::new(0),
            start_batch: None,
            playlist_cut: None,
            stop_requested: AtomicBool::new(false),
            whole_loop_release: None,
            state: AtomicU8::new(VoiceState::Idle as u8),
            inner: Arc::new(VoiceInner {
                gain_bits: AtomicU32::new(f32::to_bits(gain)),
                pan_bits: AtomicU32::new(f32::to_bits(pan)),
                loops_remaining: AtomicU32::new(0),
                rate_fader: Arc::new(RateFader::default()),
                rate_slew: UnsafeCell::new(RateSlew::default()),
                cue_trim: None,
                output_trim: None,
                effects: UnsafeCell::new(None),
                cutoff_controls: Box::new([]),
                eq_gain_controls: Box::new([]),
                natural_fade_out: AtomicU64::new(0),
                fade: UnsafeCell::new(None),
                end_frame: UnsafeCell::new(None),
                slices: UnsafeCell::new(None),
                devamp_request: AtomicU8::new(DEVAMP_NONE),
                level_matrix: MatrixRouting::default(),
            }),
            out_l: 0,
            out_r: 1,
            live: None,
            patched: false,
            named_output: None,
            output_patch_id: None,
            patch_effects: None,
            patch_fader: None,
            patch_slot: None,
        }
    }

    /// Create a live (Mic Cue) voice that reads from an input feed instead of a
    /// fixed sample buffer.  `sample_rate` is the **output** rate so soft-fade
    /// durations (computed in [`crate::engine::audio_engine`]) land in
    /// wall-clock milliseconds.
    pub fn new_live(live: LiveSource, sample_rate: u32, gain: f32, pan: f32) -> Self {
        Self {
            id: Uuid::new_v4(),
            samples: Arc::new(Vec::new()),
            channels: 2,
            sample_rate,
            frame_pos: AtomicU64::new(0),
            loop_start_frame: 0,
            start_deadline_us: AtomicU64::new(0),
            start_batch: None,
            playlist_cut: None,
            stop_requested: AtomicBool::new(false),
            whole_loop_release: None,
            state: AtomicU8::new(VoiceState::Idle as u8),
            inner: Arc::new(VoiceInner {
                gain_bits: AtomicU32::new(f32::to_bits(gain)),
                pan_bits: AtomicU32::new(f32::to_bits(pan)),
                loops_remaining: AtomicU32::new(0),
                rate_fader: Arc::new(RateFader::default()),
                rate_slew: UnsafeCell::new(RateSlew::default()),
                cue_trim: None,
                output_trim: None,
                effects: UnsafeCell::new(None),
                cutoff_controls: Box::new([]),
                eq_gain_controls: Box::new([]),
                natural_fade_out: AtomicU64::new(0),
                fade: UnsafeCell::new(None),
                end_frame: UnsafeCell::new(None),
                slices: UnsafeCell::new(None),
                devamp_request: AtomicU8::new(DEVAMP_NONE),
                level_matrix: MatrixRouting::default(),
            }),
            out_l: 0,
            out_r: 1,
            live: Some(Arc::new(live)),
            patched: false,
            named_output: None,
            output_patch_id: None,
            patch_effects: None,
            patch_fader: None,
            patch_slot: None,
        }
    }

    /// Install cue effects before submitting the new voice.
    pub(crate) fn with_cue_effects(mut self, effects: &[super::cue_effects::CueEffect]) -> Self {
        let inner = Arc::get_mut(&mut self.inner).expect("configure effects on a new voice");
        let processor = super::cue_effects::CueEffects::for_voice(
            effects,
            usize::from(self.channels).min(MATRIX_INPUTS),
            self.sample_rate,
        );
        inner.cutoff_controls = processor.iter().flat_map(|effects| effects.cutoff_controls()).collect();
        inner.eq_gain_controls = processor.iter().flat_map(|effects| effects.eq_gain_controls()).collect();
        *inner.effects.get_mut() = processor;
        self
    }

    /// Set the saved compensation before this Audio cue voice starts.
    pub(crate) fn with_cue_trim(mut self, gain: f32) -> Self {
        let inner = Arc::get_mut(&mut self.inner).expect("voice is not yet submitted");
        let fader = ScalarFader::default();
        fader.initialize(gain);
        inner.cue_trim = Some(CueTrim { fader, slew: UnsafeCell::new(ScalarSlew::default()) });
        self
    }

    pub(crate) fn with_output_trims(mut self, channels: &[u16], gains: &[f32]) -> Self {
        let inner = Arc::get_mut(&mut self.inner).expect("voice is not yet submitted");
        inner.output_trim = Some(ChannelTrims::new(channels, gains));
        self
    }

    /// Return the total number of frames in the voice.
    pub fn total_frames(&self) -> u64 {
        if self.channels == 0 { 0 } else { self.samples.len() as u64 / self.channels as u64 }
    }

    /// End of the trimmed file, clamped to its decoded frames.
    pub(super) fn end_frame(&self) -> u64 {
        // SAFETY: end_frame is written once before submitting the voice.
        let end = unsafe { *self.inner.end_frame.get() };
        end.unwrap_or(u64::MAX).min(self.total_frames())
    }

    /// Current frame position.
    pub fn current_frame(&self) -> u64 {
        self.frame_pos.load(Ordering::Relaxed)
    }

    pub fn set_playing(&self) {
        self.state.store(VoiceState::Playing as u8, Ordering::Release);
    }
    pub fn set_paused(&self) {
        self.state.store(VoiceState::Paused as u8, Ordering::Release);
    }
    pub fn set_stopped(&self) {
        self.state.store(VoiceState::Stopped as u8, Ordering::Release);
    }

    pub(crate) fn prepare_for_output_restart(&self) {
        self.inner.level_matrix.revoke_owner();
        if self.stop_requested.load(Ordering::Acquire) {
            self.set_stopped();
        } else if self.start_deadline_us.load(Ordering::Acquire) != 0 {
            // Expire an unrendered GO. The replacement stream reports it as
            // withdrawn on its first callback without firing an auto-follow.
            self.start_deadline_us.store(1, Ordering::Release);
        }
    }

    pub fn voice_state(&self) -> VoiceState {
        VoiceState::from(self.state.load(Ordering::Acquire))
    }

    /// Compute pan gains `(left, right)`.
    pub fn pan_gains(&self) -> (f32, f32) {
        let pan = self.inner.pan();
        let gain = self.inner.gain();
        let left = ((1.0 - pan) * 0.5).clamp(0.0, 1.0).sqrt();
        let right = ((1.0 + pan) * 0.5).clamp(0.0, 1.0).sqrt();
        (left * gain, right * gain)
    }
}
