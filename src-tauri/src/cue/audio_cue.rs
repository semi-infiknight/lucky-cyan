//! [`AudioCue`] — plays an audio file through the audio engine.
//!
//! This is the primary cue type for Inkue.  It decodes an audio file using
//! symphonia and submits a [`Voice`](crate::engine::voice::Voice) to the
//! [`AudioEngine`](crate::engine::AudioEngine) when triggered.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::engine::{
    cue_effects::{validate_effects, CueEffect},
    ring_command::{FadeCurve as EngineFadeCurve, VoiceId},
    voice::{FadeDirection, FadeState, LevelMatrix, Voice, MATRIX_INPUTS, MATRIX_OUTPUTS},
};
use crate::show::timecode_loss::{ClockMediaCue, ClockRun};

use super::{
    context::{CueContext, CueEvent},
    traits::{Cue, CueFactory},
    types::{
        ContinueMode, CueColor, CueId, CueState, CueType, FadeCurve,
        FadeSpec,
    },
};

// ---------------------------------------------------------------------------
// AudioCue
// ---------------------------------------------------------------------------

/// A cue that plays an audio file through the audio engine.
pub struct AudioCue {
    // --- Identity ---
    id: CueId,
    name: String,
    number: Option<String>,
    notes: String,
    color: CueColor,

    // --- State ---
    state: CueState,

    // --- Timing ---
    pre_wait: Duration,
    post_wait: Duration,
    started_at: Option<Instant>,
    action_started_at: Option<Instant>,

    // --- Continue ---
    continue_mode: ContinueMode,

    // --- Audio-specific ---
    /// Path to the audio file, relative to the workspace directory.
    pub file_path: Option<PathBuf>,
    /// Volume in dB (-60 to +12).
    pub volume_db: f64,
    /// Manual post-fader compensation for this file Audio cue.
    trim_db: f64,
    pub(crate) output_trim: Option<super::output_trim::OutputTrim>,
    /// Stereo pan (-1.0 to +1.0).
    pub pan: f32,
    /// Optional fade in specification.
    pub fade_in: Option<FadeSpec>,
    /// Optional fade out specification (also used on soft stop).
    pub fade_out: Option<FadeSpec>,
    /// Start playback at this offset into the file.
    pub start_time: Option<Duration>,
    /// Stop playback at this offset into the file.
    pub end_time: Option<Duration>,
    /// Number of extra loop repetitions (0 = play once, u32::MAX = infinite).
    pub loop_count: u32,
    /// Output patch to route through.
    pub output_patch_id: Option<uuid::Uuid>,
    /// Playback rate multiplier (1.0 = normal speed).
    pub rate: f64,
    effects: Vec<CueEffect>,
    /// QLab-style slices (markers + per-segment play counts).  Empty = plain
    /// playback.  When present, `loop_count` is ignored (the vamp segments
    /// own the looping).
    pub slices: crate::cue::types::SliceList,
    /// QLab-style crosspoint levels in dB, `[input channel][patch channel]`.
    ///
    /// `None` — the default — keeps the cue on the plain pan + Output Patch
    /// routing. When set it **replaces** pan: a crosspoint says how much of
    /// input channel *i* reaches the patch's *j*-th channel, which is what a
    /// theatre mix actually needs (and what QLab's `AudioLevelMatrix` carries).
    /// The cue's `volume_db` still applies on top, so a Fade Cue keeps working.
    /// Columns address the **Output Patch's** channels, not raw device
    /// channels, so a matrix survives repatching.
    pub level_matrix: Option<Vec<Vec<f64>>>,
    /// Operator labels indexed by source channel, before patch routing.
    pub source_channel_names: Vec<String>,
    pub matrix_mirror_mono: bool,
    pub point_mix: Option<super::point_mix::PointMix>,
    /// Live overrides from OSC `/live` writes — runtime only, never saved and
    /// cleared on reset (QLab "temporary properties").
    live_rate: Option<f64>,
    live_levels: Vec<(u32, u32, f64)>,

    is_disabled: bool,

    // --- Runtime ---
    /// Pre-decoded samples, loaded by `load()`.
    decoded_samples: Option<Arc<Vec<f32>>>,
    decoded_channels: u16,
    decoded_sample_rate: u32,
    /// The voice ID currently in use, if any.
    active_voice_id: Option<VoiceId>,
    start_reported: bool,
    prepared: Option<PreparedAudio>,
    load_to_time: Option<LoadedAudioTime>,
    /// Cached duration computed from the decoded samples.
    cached_duration: Option<Duration>,
    /// `true` between `go()` and the moment the audio action actually starts
    /// (i.e. while waiting for `pre_wait` to expire).
    in_pre_wait: bool,
    named_prewait: Option<crate::engine::device_manager::NamedOutput>,
    named_start_held: bool,
    /// Incremented on every `go()` call.  Kept for diagnostics / future use.
    play_generation: u64,
    clock_run: Option<(Uuid, ClockRun)>,
    /// Set to `true` by [`Transport::go`] immediately after firing the
    /// Auto-Continue chain, so the event loop never double-fires it.
    auto_continue_fired: bool,
    /// Elapsed time accumulated before the most recent pause (mirrors WaitCue pattern).
    elapsed_before_pause: Duration,
    /// Action-elapsed time accumulated before the most recent pause.
    action_elapsed_before_pause: Duration,
}

struct PreparedAudio {
    programming: Value,
    samples: Arc<Vec<f32>>,
    resumed: bool,
}

struct LoadedAudioTime {
    samples: Weak<Vec<f32>>,
    sample_rate: u32,
    clip_start: u64,
    clip_end: u64,
    frame: u64,
    elapsed: Duration,
}

impl AudioCue {
    pub(crate) fn effects(&self) -> &[CueEffect] {
        &self.effects
    }

    /// Create a new, empty Audio Cue with a fresh UUID.
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::from("Audio Cue"),
            number: None,
            notes: String::new(),
            color: CueColor::Blue,
            state: CueState::Standby,
            pre_wait: Duration::ZERO,
            post_wait: Duration::ZERO,
            started_at: None,
            action_started_at: None,
            continue_mode: ContinueMode::DoNotContinue,
            file_path: None,
            volume_db: 0.0,
            trim_db: 0.0,
            output_trim: None,
            pan: 0.0,
            fade_in: None,
            fade_out: None,
            start_time: None,
            end_time: None,
            loop_count: 0,
            output_patch_id: None,
            rate: 1.0,
            effects: Vec::new(),
            slices: crate::cue::types::SliceList::default(),
            level_matrix: None,
            source_channel_names: Vec::new(),
            matrix_mirror_mono: true,
            point_mix: None,
            live_rate: None,
            live_levels: Vec::new(),
            is_disabled: false,
            decoded_samples: None,
            decoded_channels: 2,
            decoded_sample_rate: 44100,
            active_voice_id: None,
            start_reported: false,
            prepared: None,
            load_to_time: None,
            cached_duration: None,
            in_pre_wait: false,
            named_prewait: None,
            named_start_held: false,
            play_generation: 0,
            clock_run: None,
            auto_continue_fired: false,
            elapsed_before_pause: Duration::ZERO,
            action_elapsed_before_pause: Duration::ZERO,
        }
    }

    /// Return the active voice ID if the cue is currently playing.
    pub fn voice_id(&self) -> Option<VoiceId> {
        self.active_voice_id.filter(|_| self.prepared.is_none())
    }

    pub(crate) fn prepared_voice_id(&self) -> Option<VoiceId> {
        self.active_voice_id.filter(|_| self.prepared.is_some())
    }

    /// Build the frame-resolved slice program for the current clip window,
    /// or `None` when the cue has no slice markers inside it.
    fn build_slice_program(&self) -> Option<crate::engine::voice::SliceProgram> {
        if self.slices.is_empty() {
            return None;
        }
        let sr = self.decoded_sample_rate as u64;
        let file_ms = self
            .cached_duration
            .map(|d| d.as_millis() as u64)
            .unwrap_or(u64::MAX);
        let clip_start = self.start_time.map(|d| d.as_millis() as u64).unwrap_or(0);
        let clip_end = self
            .end_time
            .map(|d| d.as_millis() as u64)
            .unwrap_or(file_ms)
            .min(file_ms);
        let segments: Vec<crate::engine::voice::SliceSegment> = self
            .slices
            .segments(clip_start, clip_end)
            .into_iter()
            .map(|(s, e, count)| crate::engine::voice::SliceSegment {
                start_frame: s * sr / 1000,
                end_frame: e * sr / 1000,
                play_count: count,
            })
            .collect();
        crate::engine::voice::SliceProgram::new(segments)
    }

    /// Convert a [`FadeCurve`] from the cue layer to the engine layer.
    fn engine_curve(c: FadeCurve) -> EngineFadeCurve {
        match c {
            FadeCurve::Linear => EngineFadeCurve::Linear,
            FadeCurve::SCurve => EngineFadeCurve::SCurve,
            FadeCurve::Exponential => EngineFadeCurve::Exponential,
        }
    }

    /// Start the audio action (submit a voice to the engine).
    ///
    /// Called either directly from `go()` when `pre_wait` is zero, or from
    /// `tick()` once the pre-wait timer has expired.
    fn start_audio_action(&mut self, context: &CueContext, loaded_time: Option<(u64, Duration)>) -> Result<()> {
        if let Some((_, elapsed)) = loaded_time {
            anyhow::ensure!(Instant::now().checked_sub(elapsed).is_some(), "Loaded time is too far back for the playback clock.");
        }
        let resumed = self.prepared.as_ref().is_some_and(|prepared| prepared.resumed);
        let voice_id = match self.release_prepared_voice(context)? {
            Some(voice) => voice,
            None => {
                let (voice, device) = self.prepare_voice(context, loaded_time.map(|(frame, _)| frame))?;
                context.audio_engine.play_voice_routed(voice, device.as_deref())?
            }
        };
        self.active_voice_id = Some(voice_id);
        let now = Instant::now();
        let origin = loaded_time.map_or(now, |(_, elapsed)| now - elapsed);
        self.action_started_at = Some(origin);
        if loaded_time.is_some() {
            self.started_at = Some(origin);
            self.clear_loaded_time(context);
        }
        self.in_pre_wait = false;

        if context.playlist_cut.is_none() {
            self.report_file_start(context, resumed);
        }
        Ok(())
    }

    pub(crate) fn report_file_start(&mut self, context: &CueContext, resumed: bool) {
        if resumed {
            crate::media_usage::resume(context.media_show.as_ref(), self);
        } else {
            crate::media_usage::start(context.media_show.as_ref(), self);
        }
        self.start_reported = context.announce_action_started(self.id);
    }

    pub(crate) fn supports_playlist_cut(&self, ctx: &CueContext) -> bool {
        self.output_patch_id.is_none()
            && ctx.default_patch_id.is_none()
            && self.output_trim.is_none()
            && self.pre_wait.is_zero()
            && self.loop_count == 0
            && self.slices.is_empty()
            && self.start_time.unwrap_or_default().is_zero()
            && self.end_time.is_none()
            && self.rate == 1.0
            && self.fade_in.is_none()
            && self.fade_out.is_none()
            && self.effects.is_empty()
            && self.level_matrix.is_none()
            && self.point_mix.is_none()
            && self.prepared.is_none()
            && self.load_to_time.is_none()
            && matches!(self.decoded_channels, 1 | 2)
            && self.decoded_sample_rate > 0
            && self
                .decoded_samples
                .as_ref()
                .is_some_and(|samples| samples.len() >= usize::from(self.decoded_channels))
    }

    fn release_prepared_voice(&mut self, context: &CueContext) -> Result<Option<VoiceId>> {
        let Some(prepared) = self.prepared.take() else { return Ok(None) };
        context.request_cue_refresh();
        let Some(voice) = self.active_voice_id.take() else { return Ok(None) };
        let same_program = self.serialize() == prepared.programming
            && self.decoded_samples.as_ref().is_some_and(|samples| Arc::ptr_eq(samples, &prepared.samples));
        let direct_output = context.resolve_patch(self.output_patch_id).is_none();
        // An immediate simultaneous Group must admit its children together on
        // the callback. A voice prepared separately cannot join that batch.
        let start = if same_program && direct_output && context.audio_start_batch.is_none() {
            context.audio_engine.start_preloaded_file_voice(voice)
        } else {
            Ok(false)
        };
        match start {
            Ok(true) => Ok(Some(voice)),
            other => {
                let discarded = context.audio_engine.stop_voice(voice, 0, EngineFadeCurve::Linear);
                other?;
                discarded?;
                Ok(None)
            }
        }
    }

    fn source_frame(&self, time: Duration) -> u64 {
        (time.as_secs_f64() * f64::from(self.decoded_sample_rate)) as u64
    }

    fn load_time_window(&self) -> Result<(&Arc<Vec<f32>>, u64, u64)> {
        anyhow::ensure!(self.loop_count == 0 && self.slices.is_empty() && self.rate == 1.0,
            "Load to Time requires File Audio played once at normal speed without slices.");
        anyhow::ensure!(self.pre_wait.is_zero() && self.continue_mode == ContinueMode::DoNotContinue,
            "Load to Time requires zero Pre-Wait and Do Not Continue.");
        let samples = self.decoded_samples.as_ref().filter(|_| self.decoded_channels != 0 && self.decoded_sample_rate != 0)
            .ok_or_else(|| anyhow!("Wait for the Audio cue to finish loading its file."))?;
        let total = (samples.len() / usize::from(self.decoded_channels)) as u64;
        let start = self.source_frame(self.start_time.unwrap_or_default());
        let end = self.end_time.map_or(total, |time| self.source_frame(time).min(total));
        anyhow::ensure!(start < end, "Set a playable clip range before using Load to Time.");
        Ok((samples, start, end))
    }

    /// Stage a clip-relative offset for the next admitted GO. Negative values
    /// count back from the exclusive end of the current saved clip window.
    pub fn load_to_time(&mut self, offset_ms: i64) -> Result<()> {
        self.stage_clip_offset(Duration::from_millis(offset_ms.unsigned_abs()), offset_ms < 0)
    }

    pub(crate) fn stage_timecode_offset(&mut self, elapsed: Duration) -> Result<()> {
        self.stage_clip_offset(elapsed, false)
    }

    fn stage_clip_offset(&mut self, elapsed: Duration, from_end: bool) -> Result<()> {
        anyhow::ensure!(self.state == CueState::Standby && !self.is_disabled,
            "Stop and arm the Audio cue before using Load to Time.");
        let (samples, clip_start, clip_end) = self.load_time_window()?;
        let distance = self.source_frame(elapsed);
        let frame = if from_end {
            clip_end.checked_sub(distance)
        } else {
            clip_start.checked_add(distance)
        };
        let frame = frame.filter(|&frame| (clip_start..clip_end).contains(&frame))
            .ok_or_else(|| anyhow!("Enter a time inside the saved clip range."))?;
        let elapsed = Duration::from_secs_f64((frame - clip_start) as f64 / f64::from(self.decoded_sample_rate));
        self.load_to_time = Some(LoadedAudioTime {
            samples: Arc::downgrade(samples), sample_rate: self.decoded_sample_rate,
            clip_start, clip_end, frame, elapsed,
        });
        Ok(())
    }

    pub(crate) fn clear_loaded_time(&mut self, context: &CueContext) {
        if self.cancel_loaded_time() {
            context.request_cue_refresh();
        }
    }

    fn checked_loaded_time(&self) -> Result<Option<(u64, Duration)>> {
        let Some(loaded) = &self.load_to_time else { return Ok(None) };
        let (samples, start, end) = self.load_time_window()?;
        anyhow::ensure!(loaded.sample_rate == self.decoded_sample_rate && (start, end) == (loaded.clip_start, loaded.clip_end)
            && loaded.samples.ptr_eq(&Arc::downgrade(samples)),
            "The Audio cue changed. Reload its time or clear Load to Time before starting it.");
        Ok(Some((loaded.frame, loaded.elapsed)))
    }

    fn prepare_voice(&self, context: &CueContext, loaded_frame: Option<u64>) -> Result<(Voice, Option<String>)> {
        let samples = match &self.decoded_samples {
            Some(s) => Arc::clone(s),
            None => return Err(anyhow!(
                "AudioCue '{}': audio not loaded — assign a file and try again",
                self.name
            )),
        };

        let gain = crate::cue::types::db_to_linear(self.volume_db) as f32;
        let mut voice = Voice::new(
            samples,
            self.decoded_channels,
            self.decoded_sample_rate,
            gain,
            self.pan,
        ).with_cue_effects(&self.effects)
            .with_cue_trim(crate::cue::types::db_to_linear(self.trim_db) as f32);

        voice.inner.loops_remaining.store(self.loop_count, std::sync::atomic::Ordering::Relaxed);
        voice.inner.set_rate(self.rate as f32);
        if let Some((duration, curve)) = self.natural_fade_out() {
            voice.inner.set_natural_fade_out(duration, curve);
        }

        // Apply start/end time markers (written before play_voice; no RT thread yet).
        if let Some(end) = self.end_time {
            let end_frame = self.source_frame(end);
            // SAFETY: written once before play_voice(); RT thread has not started yet.
            unsafe { *voice.inner.end_frame.get() = Some(end_frame); }
        }
        if let Some(start) = self.start_time {
            let start_frame = self.source_frame(start);
            voice.frame_pos.store(start_frame, std::sync::atomic::Ordering::Relaxed);
            voice.loop_start_frame = start_frame;
        }
        if let Some(frame) = loaded_frame {
            voice.frame_pos.store(frame, std::sync::atomic::Ordering::Relaxed);
        }

        // Slice program (QLab slices): resolved against the clip window in
        // frames.  When active it owns all boundaries — loop_count/end_frame
        // are ignored by the callback.
        if let Some(program) = self.build_slice_program() {
            voice.frame_pos.store(
                program.segments[0].start_frame,
                std::sync::atomic::Ordering::Relaxed,
            );
            // SAFETY: written once before play_voice(); RT thread not started.
            unsafe { *voice.inner.slices.get() = Some(program) };
        } else if self.slices.is_empty() && self.loop_count > 0 {
            voice.whole_loop_release = Some(std::sync::atomic::AtomicBool::new(false));
        }

        // Apply fade-in if configured (written before play_voice; RT thread not running yet).
        if let Some(ref fi) = self.fade_in {
            let total = (fi.duration_ms * self.decoded_sample_rate as u64) / 1000;
            let elapsed = loaded_frame.map_or(0, |frame| frame - voice.loop_start_frame);
            if loaded_frame.is_none() || elapsed < total {
                // SAFETY: same as above — single writer before submission.
                unsafe {
                    *voice.inner.fade.get() = Some(FadeState {
                        direction: FadeDirection::In,
                        total_samples: total,
                        elapsed_samples: elapsed,
                        curve: Self::engine_curve(fi.curve),
                    });
                }
            }
        }

        // Apply Output Patch routing.  Look up the cue's assigned patch
        // (falling back to the workspace default); if found, map its first two
        // channel indices to the voice's L/R output slots and target its
        // device (the engine opens an aux stream for non-main devices).
        let mut patch_device: Option<String> = None;
        let patch = context.resolve_patch(self.output_patch_id);
        if let Some(patch) = patch {
            voice.output_patch_id = Some(patch.id);
            if let Some(&ch_l) = patch.channels.first() {
                voice.out_l = ch_l as usize;
            }
            if let Some(&ch_r) = patch.channels.get(1) {
                voice.out_r = ch_r as usize;
            } else if let Some(&ch_l) = patch.channels.first() {
                // Mono patch — route both L and R to the same channel.
                voice.out_r = ch_l as usize;
            }
            voice.patched = true;
            voice.patch_fader = Some(context.patch_fader(patch));
            voice.patch_slot = context
                .output_patches
                .iter()
                .position(|p| p.id == patch.id)
                .map(|i| i as u8);
            patch_device = Some(patch.device_id.clone());
        }
        voice.named_output = self.named_output(context)?;
        if let Some(planned) = self.named_prewait.as_ref() {
            anyhow::ensure!(
                voice.named_output.as_ref() == Some(planned),
                "The required output changed during Pre-Wait. Check the Output Patch and retry GO."
            );
        }
        let mix_channels = patch.map(|patch| patch.cue_mixer_channels()).unwrap_or_default();
        if let Some(patch) = patch {
            if voice.named_output.is_some() && super::output_trim::OutputTrim::valid_channels(&patch.channels) {
                let gains = self.output_trim.as_ref().map_or_else(Vec::new, |trim| trim.linear_levels());
                voice = voice.with_output_trims(&mix_channels, &gains);
            }
        }

        // Crosspoint levels, resolved against the patch that was just applied
        // so a matrix column addresses the patch's channel, not a raw device
        // one.  Written before submission — the RT callback only reads it.
        if let Some(spec) = &self.level_matrix {
            if let Some(matrix) = build_level_matrix(spec, &mix_channels, self.matrix_mirror_mono) {
                // SAFETY: written once, before play_voice_routed hands the
                // voice to the RT thread.
                voice.inner.level_matrix.update(|_| Some(matrix));
            }
        }

        voice.start_batch = context.audio_start_batch.clone();
        voice.playlist_cut = context.playlist_cut.clone();
        Ok((voice, patch_device))
    }

    pub(crate) fn requires_named_output(&self, context: &CueContext) -> bool {
        self.output_trim.is_some() || context.resolve_patch(self.output_patch_id)
            .is_some_and(|patch| patch.require_named_device || patch.has_audio_effects())
    }

    fn named_output(&self, context: &CueContext) -> Result<Option<crate::engine::device_manager::NamedOutput>> {
        let patch = context.resolve_patch(self.output_patch_id);
        if let Some(trim) = &self.output_trim {
            return trim.route(patch).map(Some).map_err(anyhow::Error::msg);
        }
        match patch {
            Some(patch) => patch.named_output().map_err(anyhow::Error::msg),
            None => Ok(None),
        }
    }

    fn hold_for_named_output(&mut self, error: &anyhow::Error) {
        self.named_start_held = true;
        crate::health::set(crate::health::HealthAlert::new(
            "output-patch-device",
            crate::health::HealthLevel::Error,
            format!("Audio GO held for “{}”. {error}", self.name),
        ));
    }

    /// Decode an audio file to interleaved f32 samples.
    ///
    /// This is a pure function (no `self` mutation) and must be called on a
    /// non-RT thread without holding any workspace locks.  The result is
    /// pushed back into the cue via [`accept_preloaded_audio`].  Delegates to
    /// the shared [`media_decode::decode_audio_track`] so audio files and the
    /// audio track of video containers decode through one code path.
    pub fn decode_file(path: &Path) -> Result<(Vec<f32>, u16, u32)> {
        crate::cue::media_decode::decode_audio_track(path)?
            .ok_or_else(|| anyhow!("No audio track in file: {}", path.display()))
    }
}

impl Default for AudioCue {
    fn default() -> Self {
        Self::new()
    }
}

/// Turn a cue's dB crosspoints into the engine's linear matrix, mapping each
/// column through `patch_channels` so column *j* lands on the patch's *j*-th
/// device channel.  Falls back to column index when the cue has no patch.
///
/// Returns `None` when nothing would be routed — the voice then stays on the
/// original pan path rather than being silently muted by an empty matrix.
pub(crate) fn build_level_matrix(
    spec: &[Vec<f64>],
    patch_channels: &[u16],
    mirror_mono: bool,
) -> Option<LevelMatrix> {
    // "Configured or not" is a property of the *spec*, not of the padded array
    // below — an empty spec means the cue has no matrix at all.
    if spec.iter().all(|row| row.is_empty()) {
        return None;
    }
    let mut routed = LevelMatrix::default();
    routed.mirror_mono = mirror_mono;
    for (input, row) in spec.iter().take(MATRIX_INPUTS).enumerate() {
        for (column, &db) in row.iter().take(MATRIX_OUTPUTS).enumerate() {
            let device = if patch_channels.is_empty() {
                column
            } else if let Some(&assigned) = patch_channels.get(column) {
                usize::from(assigned)
            } else {
                continue;
            };
            if device < MATRIX_OUTPUTS {
                routed.add(input, device, crate::cue::types::db_to_linear(db) as f32);
            }
        }
    }
    Some(routed)
}

impl ClockMediaCue for AudioCue {
    fn clock_run(&mut self) -> &mut Option<(Uuid, ClockRun)> { &mut self.clock_run }
}

impl Cue for AudioCue {
    inherit_playback_fields!(
        state, started_at, action_started_at, active_voice_id, start_reported, prepared, load_to_time, in_pre_wait,
        play_generation, clock_run, auto_continue_fired, elapsed_before_pause,
        action_elapsed_before_pause, named_prewait, named_start_held,
    );

    fn id(&self) -> CueId { self.id }
    fn cue_type(&self) -> CueType { CueType::Audio }
    fn rate(&self) -> Option<f64> { Some(self.rate) }
    fn infinite_loop(&self) -> Option<bool> { Some(self.loop_count == super::types::PLAY_COUNT_INFINITE) }
    fn set_infinite_loop(&mut self, on: bool) {
        self.loop_count = if on { super::types::PLAY_COUNT_INFINITE } else { 0 };
    }
    fn slice_list(&self) -> Option<super::types::SliceList> { Some(self.slices.clone()) }
    fn set_slice_play_count(&mut self, index: usize, play_count: u32) -> Result<(), String> {
        if self.slices.markers.is_empty() {
            return Err("The cue has no slice markers.".into());
        }
        let Some(slot) = self.slices.play_counts.get_mut(index) else {
            return Err(format!("Slice index {index} does not exist."));
        };
        *slot = play_count.max(1);
        Ok(())
    }
    fn set_rate(&mut self, rate: f64) { self.rate = rate; }
    fn level(&self, row: usize, col: usize) -> Option<f64> {
        self.level_matrix.as_ref()?.get(row)?.get(col).copied()
    }
    fn set_level(&mut self, row: usize, col: usize, db: f64) {
        let matrix = self.level_matrix.get_or_insert_with(Vec::new);
        while matrix.len() <= row { matrix.push(Vec::new()); }
        while matrix[row].len() <= col { matrix[row].push(0.0); }
        matrix[row][col] = db;
    }
    fn live_rate(&self) -> Option<f64> { self.live_rate }
    fn set_live_rate(&mut self, rate: Option<f64>) { self.live_rate = rate; }
    fn live_level(&self, row: usize, col: usize) -> Option<f64> {
        self.live_levels.iter()
            .find(|(r, c, _)| *r == row as u32 && *c == col as u32)
            .map(|(_, _, db)| *db)
    }
    fn set_live_level(&mut self, row: usize, col: usize, db: Option<f64>) {
        self.live_levels.retain(|(r, c, _)| !(*r == row as u32 && *c == col as u32));
        if let Some(db) = db {
            self.live_levels.push((row as u32, col as u32, db));
        }
    }
    fn clear_live_levels(&mut self) { self.live_levels.clear(); }
    fn media_voice_id(&self) -> Option<VoiceId> { self.voice_id() }
    fn level_mirror_mono(&self) -> bool { self.matrix_mirror_mono }
    fn has_prepared_media(&self) -> bool { self.prepared.is_some() }
    fn source_channel_names_mut(&mut self) -> Option<&mut Vec<String>> { Some(&mut self.source_channel_names) }
    fn output_patch_id(&self) -> Option<uuid::Uuid> { self.output_patch_id }
    fn name(&self) -> &str { &self.name }
    fn set_name(&mut self, name: String) { self.name = name; }
    fn number(&self) -> Option<&str> { self.number.as_deref() }
    fn set_number(&mut self, number: Option<String>) { self.number = number; }
    fn notes(&self) -> &str { &self.notes }
    fn set_notes(&mut self, notes: String) { self.notes = notes; }
    fn color(&self) -> CueColor { self.color }
    fn set_color(&mut self, color: CueColor) { self.color = color; }
    fn is_disabled(&self) -> bool { self.is_disabled }
    fn set_disabled(&mut self, d: bool) {
        self.is_disabled = d;
        if d {
            self.load_to_time = None;
        }
    }
    fn state(&self) -> CueState { self.state }

    fn load(&mut self, _context: &CueContext) -> Result<()> {
        let path = match &self.file_path {
            Some(p) => p.clone(),
            None => return Ok(()), // No file assigned; nothing to load.
        };

        let (samples, channels, sample_rate) = Self::decode_file(&path)?;
        self.cached_duration = Some(Duration::from_secs_f64(
            samples.len() as f64 / channels.max(1) as f64 / sample_rate.max(1) as f64,
        ));
        self.decoded_channels = channels;
        self.decoded_sample_rate = sample_rate;
        self.decoded_samples = Some(Arc::new(samples));
        Ok(())
    }

    fn accept_preloaded_audio(
        &mut self,
        samples: std::sync::Arc<Vec<f32>>,
        channels: u16,
        sample_rate: u32,
        duration: std::time::Duration,
    ) {
        self.decoded_samples = Some(samples);
        self.decoded_channels = channels;
        self.decoded_sample_rate = sample_rate;
        self.cached_duration = Some(duration);
    }

    fn preload(&mut self, context: &CueContext) -> Result<()> {
        if self.is_running() || self.is_paused() || self.load_to_time.is_some() {
            return Ok(());
        }
        let samples = self.decoded_samples.as_ref().cloned().ok_or_else(|| anyhow!(
            "AudioCue '{}': audio not loaded — assign a file and try again", self.name
        ))?;
        if let Some(route) = self.named_output(context)? {
            return context.audio_engine.check_named_output_ready(&route);
        }
        if context.resolve_patch(self.output_patch_id).is_some() {
            // Preserve current patch routing at GO, including device changes.
            return Ok(());
        }

        let (voice, _) = self.prepare_voice(context, None)?;
        self.active_voice_id = Some(context.audio_engine.play_voice_paused_routed(voice, None)?);
        self.prepared = Some(PreparedAudio { programming: self.serialize(), samples, resumed: false });
        self.state = CueState::Paused;
        self.started_at = None;
        self.action_started_at = None;
        self.in_pre_wait = false;
        self.elapsed_before_pause = Duration::ZERO;
        self.action_elapsed_before_pause = Duration::ZERO;
        self.start_reported = false;
        Ok(())
    }

    fn go(&mut self, context: &CueContext) -> Result<()> {
        if self.state == CueState::Running {
            return Ok(()); // Already playing; ignore duplicate GO.
        }
        let loaded_time = self.checked_loaded_time()?;
        if self.prepared.is_some() && context.resolve_patch(self.output_patch_id).is_some() {
            self.hard_stop(context)?;
        }
        self.named_start_held = false;
        self.named_prewait = None;
        let requires_named = self.requires_named_output(context);
        if requires_named && !self.pre_wait.is_zero() {
            let route = match self.named_output(context) {
                Ok(Some(route)) => route,
                Ok(None) => return Err(anyhow!("The required Output Patch is missing.")),
                Err(error) => {
                    self.hold_for_named_output(&error);
                    return Err(error);
                }
            };
            if let Err(error) = context.audio_engine.check_named_output_ready(&route) {
                self.hold_for_named_output(&error);
                return Err(error);
            }
            self.named_prewait = Some(route);
        }

        // New play: bump generation and clear the auto-continue flag so the
        // transport can fire the chain again for this play.
        self.clock_run = None;
        self.play_generation = self.play_generation.wrapping_add(1);
        self.auto_continue_fired = false;

        self.state = CueState::Running;
        self.started_at = Some(Instant::now());

        if !self.pre_wait.is_zero() {
            // Pre-wait active: record the start time and defer the action.
            // tick() will call start_audio_action() once the timer expires.
            self.in_pre_wait = true;
            return Ok(());
        }

        // No pre-wait: start the audio action immediately.
        // On failure, roll back to Standby so callers (e.g. GroupCue) don't
        // see a permanently-Running cue that will never complete.
        if let Err(e) = self.start_audio_action(context, loaded_time) {
            self.state = CueState::Standby;
            self.started_at = None;
            if requires_named {
                self.hold_for_named_output(&e);
            }
            return Err(e);
        }
        Ok(())
    }

    fn stop(&mut self, context: &CueContext) -> Result<()> {
        self.clock_run = None;
        crate::media_usage::end(self, "stop_requested");
        self.in_pre_wait = false; // Cancel any pending pre-wait.
        self.clear_loaded_time(context);
        if self.prepared.take().is_some() {
            context.request_cue_refresh();
        }
        self.named_prewait = None;
        self.named_start_held = false;
        if let Some(vid) = self.active_voice_id.take() {
            let (fade_ms, fade_curve) = self.fade_out
                .as_ref()
                .map(|f| (f.duration_ms as u32, Self::engine_curve(f.curve)))
                .unwrap_or((context.stop_fade_ms, EngineFadeCurve::SCurve));
            context.audio_engine.stop_voice(vid, fade_ms, fade_curve)?;
        }
        self.state = CueState::Standby;
        self.started_at = None;
        self.action_started_at = None;
        self.elapsed_before_pause = Duration::ZERO;
        self.action_elapsed_before_pause = Duration::ZERO;
        self.auto_continue_fired = false;
        context.emit(CueEvent::Stopped { cue_id: self.id });
        Ok(())
    }

    fn pause(&mut self, context: &CueContext) -> Result<()> {
        if self.state != CueState::Running {
            if self.is_paused() {
                ClockRun::leave_paused(&mut self.clock_run);
            }
            return Ok(());
        }
        if !self.in_pre_wait {
            if let Some(vid) = self.active_voice_id {
                context.audio_engine.pause_voice(vid)?;
            }
            crate::media_usage::paused(self.id, true);
        }
        ClockRun::leave_paused(&mut self.clock_run);
        // Snapshot elapsed times so they freeze while paused.
        // Use = (replace) not += (accumulate): after resume(), started_at is
        // already re-anchored to encode the full play-time, so .elapsed()
        // returns the correct total — no accumulation needed.
        if let Some(t) = self.started_at.take() {
            self.elapsed_before_pause = t.elapsed();
        }
        if let Some(t) = self.action_started_at.take() {
            self.action_elapsed_before_pause = t.elapsed();
        }
        self.state = CueState::Paused;
        Ok(())
    }

    fn resume(&mut self, context: &CueContext) -> Result<()> {
        if self.state != CueState::Paused {
            self.clock_run = None;
            return Ok(());
        }
        if self.in_pre_wait {
            // Only the cue clock resumes. Its media stays held until the entrance.
            self.clock_run = None;
            self.started_at = Some(Instant::now() - self.elapsed_before_pause);
            self.state = CueState::Running;
            return Ok(());
        }
        if let Some(prepared) = &mut self.prepared {
            prepared.resumed = true;
            return self.go(context);
        }
        if let Some(vid) = self.active_voice_id {
            context.audio_engine.resume_voice(vid)?;
            if !self.start_reported {
                self.start_reported = context.mark_action_started(self.id);
            }
        }
        // Re-anchor Instants so elapsed() resumes from the frozen position.
        self.clock_run = None;
        self.started_at = Some(Instant::now() - self.elapsed_before_pause);
        self.action_started_at = Some(Instant::now() - self.action_elapsed_before_pause);
        self.state = CueState::Running;
        if self.active_voice_id.is_some() {
            crate::media_usage::resume(context.media_show.as_ref(), self);
        }
        Ok(())
    }

    fn supports_seek(&self) -> bool {
        self.prepared.is_none()
    }

    fn loaded_time_ms(&self) -> Option<u64> {
        self.load_to_time.as_ref().map(|time| (time.elapsed.as_secs_f64() * 1000.0).round() as u64)
    }

    fn cancel_loaded_time(&mut self) -> bool {
        self.load_to_time.take().is_some()
    }

    fn seek(&mut self, position_ms: u64, ctx: &CueContext) {
        // A prepared voice follows the programmed entrance. Its playback
        // clocks begin only after Start; seeking is available from then on.
        if !self.supports_seek() || self.in_pre_wait
            || (self.action_started_at.is_none() && self.state != CueState::Paused)
        {
            return;
        }
        if let Some(vid) = self.active_voice_id {
            let start_ms = self.start_time.unwrap_or(Duration::ZERO).as_millis() as u64;
            let target_ms = start_ms + position_ms;
            let frame_pos = target_ms * self.decoded_sample_rate as u64 / 1000;
            let _ = ctx.audio_engine.seek_voice(vid, frame_pos);
        }
        if self.state == CueState::Paused {
            // Update the frozen accumulators directly — elapsed() reads these
            // when paused, so the display updates on the next event-loop tick.
            self.action_elapsed_before_pause = Duration::from_millis(position_ms);
            self.elapsed_before_pause = self.pre_wait + Duration::from_millis(position_ms);
        } else {
            self.action_started_at =
                Some(Instant::now() - Duration::from_millis(position_ms));
        }
    }

    fn hard_stop(&mut self, context: &CueContext) -> Result<()> {
        self.clock_run = None;
        crate::media_usage::end(self, "hard_stop_requested");
        self.in_pre_wait = false;
        self.clear_loaded_time(context);
        if self.prepared.take().is_some() {
            context.request_cue_refresh();
        }
        self.named_prewait = None;
        self.named_start_held = false;
        if let Some(vid) = self.active_voice_id.take() {
            context.audio_engine.stop_voice(vid, 0, EngineFadeCurve::Linear)?;
        }
        self.state = CueState::Standby;
        self.started_at = None;
        self.action_started_at = None;
        self.elapsed_before_pause = Duration::ZERO;
        self.action_elapsed_before_pause = Duration::ZERO;
        self.auto_continue_fired = false;
        context.emit(CueEvent::Stopped { cue_id: self.id });
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.clock_run = None;
        crate::media_usage::end(self, "released");
        // Note: does not stop playback — call stop() first if needed.
        self.state = CueState::Standby;
        self.active_voice_id = None;
        self.prepared = None;
        self.load_to_time = None;
        self.started_at = None;
        self.action_started_at = None;
        self.elapsed_before_pause = Duration::ZERO;
        self.action_elapsed_before_pause = Duration::ZERO;
        self.in_pre_wait = false;
        self.named_prewait = None;
        self.named_start_held = false;
        self.auto_continue_fired = false;
        Ok(())
    }

    fn tick(&mut self, context: &CueContext) -> Result<()> {
        if self.state != CueState::Running {
            return Ok(());
        }
        if self.in_pre_wait && self.elapsed() >= self.pre_wait {
            if let Err(error) = self.start_audio_action(context, None) {
                self.in_pre_wait = false;
                self.started_at = None;
                self.state = CueState::Standby;
                if self.named_prewait.is_some() || context.resolve_patch(self.output_patch_id)
                    .is_some_and(|patch| patch.require_named_device || patch.has_audio_effects())
                {
                    self.hold_for_named_output(&error);
                }
                return Err(error);
            }
        }
        Ok(())
    }

    fn is_action_started(&self) -> bool {
        !self.in_pre_wait && self.prepared.is_none() && self.load_to_time.is_none()
    }

    fn has_withdrawn_output(&self) -> bool {
        self.named_start_held
    }

    fn take_output_start_hold(&mut self) -> bool {
        std::mem::take(&mut self.named_start_held) && self.named_prewait.is_some()
    }

    fn uses_action_clock_for_continue(&self) -> bool {
        self.named_prewait.is_some()
    }

    fn pending_start_admission(&self) -> bool {
        self.in_pre_wait && self.named_prewait.is_some()
    }

    fn holds_playhead(&self) -> bool {
        self.pending_start_admission()
    }

    fn released_playhead(&self) -> bool {
        self.named_prewait.is_some() && self.active_voice_id.is_some() && self.state == CueState::Running
    }

    fn play_generation(&self) -> u64 {
        self.play_generation
    }

    fn is_auto_continue_fired(&self) -> bool {
        self.auto_continue_fired
    }

    fn mark_auto_continue_fired(&mut self) {
        self.auto_continue_fired = true;
    }

    fn clear_auto_continue_fired(&mut self) {
        self.auto_continue_fired = false;
    }

    fn pre_wait(&self) -> Duration { self.pre_wait }
    fn set_pre_wait(&mut self, d: Duration) { self.pre_wait = d; }
    fn post_wait(&self) -> Duration { self.post_wait }
    fn set_post_wait(&mut self, d: Duration) { self.post_wait = d; }

    fn natural_fade_out(&self) -> Option<(u32, EngineFadeCurve)> {
        self.fade_out.as_ref().map(|fade| {
            (u32::try_from(fade.duration_ms).unwrap_or(u32::MAX), Self::engine_curve(fade.curve))
        })
    }

    fn awaits_voice_completion(&self) -> bool {
        self.voice_id().is_some()
    }

    fn duration(&self) -> Option<Duration> {
        // Infinite loop — no fixed duration; rely on voice_done for completion.
        if self.loop_count == u32::MAX {
            return None;
        }
        self.cached_duration.map(|d| {
            // Adjust for start/end markers.
            let start = self.start_time.unwrap_or(Duration::ZERO);
            let end = self.end_time.unwrap_or(d);
            let base = end.saturating_sub(start);
            // Adjust for playback rate: rate > 1.0 shortens effective duration.
            let adjusted = if self.rate > 0.0 && (self.rate - 1.0).abs() > f64::EPSILON {
                Duration::from_secs_f64(base.as_secs_f64() / self.rate)
            } else {
                base
            };
            // Multiply by total number of plays: initial play + loop_count repeats.
            adjusted * (self.loop_count + 1)
        })
    }

    fn elapsed(&self) -> Duration {
        if self.state == CueState::Paused {
            return self.elapsed_before_pause;
        }
        self.started_at.map(|t| t.elapsed()).unwrap_or(Duration::ZERO)
    }

    fn action_elapsed(&self) -> Duration {
        if self.state == CueState::Paused {
            return self.action_elapsed_before_pause;
        }
        self.action_started_at.map(|t| t.elapsed()).unwrap_or(Duration::ZERO)
    }

    fn continue_mode(&self) -> ContinueMode { self.continue_mode }
    fn set_continue_mode(&mut self, mode: ContinueMode) { self.continue_mode = mode; }

    fn validate_programming(
        &self,
        ctx: &crate::cue::validation::ValidationContext,
    ) -> Vec<crate::cue::validation::CueIssue> {
        use crate::cue::validation::CueIssue;
        // `None` routes to the default patch — only a *set* patch that no longer
        // exists is worth flagging (the cue silently falls back otherwise).
        match self.output_patch_id {
            Some(id) if !ctx.output_patch_ids.contains(&id) => {
                vec![CueIssue::warning("Output Patch not found (using default patch)")]
            }
            _ => Vec::new(),
        }
    }

    fn playing_voice_id(&self) -> Option<CueId> {
        self.voice_id()
    }

    fn uses_sliced_playback(&self) -> bool {
        !self.slices.is_empty()
    }

    fn file_duration(&self) -> Option<Duration> {
        self.cached_duration
    }

    fn media_file_path(&self) -> Option<&std::path::Path> {
        self.file_path.as_deref()
    }

    fn extract_decoded_audio(
        &self,
    ) -> Option<(std::sync::Arc<Vec<f32>>, u16, u32, Duration)> {
        let samples = self.decoded_samples.as_ref()?;
        let duration = self.cached_duration?;
        Some((Arc::clone(samples), self.decoded_channels, self.decoded_sample_rate, duration))
    }

    fn waveform_peaks(&self, bins: usize) -> Option<Vec<f32>> {
        let samples = self.decoded_samples.as_ref()?;
        let channels = self.decoded_channels as usize;
        if bins == 0 || channels == 0 { return Some(vec![]); }
        let total_frames = samples.len() / channels;
        if total_frames == 0 { return Some(vec![]); }

        let mut peaks = Vec::with_capacity(bins);
        for i in 0..bins {
            let start_frame = (i * total_frames) / bins;
            let end_frame = (((i + 1) * total_frames) / bins).max(start_frame + 1);
            let mut peak = 0.0f32;
            for frame in start_frame..end_frame.min(total_frames) {
                for ch in 0..channels {
                    let v = samples[frame * channels + ch].abs();
                    if v > peak { peak = v; }
                }
            }
            peaks.push(peak);
        }
        Some(peaks)
    }

    fn applied_point_mix(&self) -> Option<&super::point_mix::PointMix> { self.point_mix.as_ref() }

    fn live_audio_params(&self) -> Option<crate::cue::traits::LiveAudioParams> {
        let voice_id = self.voice_id()?;
        Some(crate::cue::traits::LiveAudioParams {
            voice_id,
            gain: crate::cue::types::db_to_linear(self.volume_db) as f32,
            pan: self.pan,
            matrix_mirror_mono: self.matrix_mirror_mono,
            level_matrix: self.level_matrix.clone(),
        })
    }

    fn audio_filter_cutoffs(&self) -> Vec<(CueId, f32)> {
        self.effects.iter()
            .filter_map(|effect| Some((effect.id(), effect.enabled_cutoff()?)))
            .collect()
    }

    fn audio_eq_gains(&self) -> Vec<(CueId, f32)> {
        self.effects.iter()
            .filter_map(|effect| Some((effect.id(), effect.enabled_eq_gain()?)))
            .collect()
    }

    fn serialize(&self) -> Value {
        json!({
            "type": "audio",
            "cue_type": "audio",
            "id": self.id,
            "number": self.number,
            "name": self.name,
            "notes": self.notes,
            "color": self.color,
            "pre_wait_ms": self.pre_wait.as_millis() as u64,
            "post_wait_ms": self.post_wait.as_millis() as u64,
            "continue_mode": self.continue_mode,
            "file_path": self.file_path.as_ref().map(|p| p.to_string_lossy().to_string()),
            "volume_db": self.volume_db,
            "trim_db": self.trim_db,
            "output_trim": self.output_trim,
            "pan": self.pan,
            "fade_in_ms": self.fade_in.as_ref().map(|f| f.duration_ms),
            "fade_in_curve": self.fade_in.as_ref().map(|f| f.curve),
            "fade_out_ms": self.fade_out.as_ref().map(|f| f.duration_ms),
            "fade_out_curve": self.fade_out.as_ref().map(|f| f.curve),
            "start_time_ms": self.start_time.map(|d| d.as_millis() as u64),
            "end_time_ms": self.end_time.map(|d| d.as_millis() as u64),
            "loop_count": self.loop_count,
            "output_patch_id": self.output_patch_id,
            "rate": self.rate,
            "audio_effects": self.effects,
            "slices": self.slices,
            "level_matrix": self.level_matrix,
            "source_channel_names": self.source_channel_names,
            "matrix_mirror_mono": self.matrix_mirror_mono,
            "point_mix": self.point_mix,
            "is_disabled": self.is_disabled,
        })
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Factory for [`AudioCue`].
pub struct AudioCueFactory;

impl CueFactory for AudioCueFactory {
    fn create(&self) -> Box<dyn Cue> {
        Box::new(AudioCue::new())
    }

    fn from_json(&self, value: Value) -> Result<Box<dyn Cue>> {
        let mut cue = AudioCue::new();

        if let Some(id_str) = value.get("id").and_then(|v| v.as_str()) {
            cue.id = id_str.parse().unwrap_or_else(|_| Uuid::new_v4());
        }
        if let Some(name) = value.get("name").and_then(|v| v.as_str()) {
            cue.name = name.to_string();
        }
        if let Some(num) = value.get("number").and_then(|v| v.as_str()) {
            cue.number = Some(num.to_string());
        }
        if let Some(notes) = value.get("notes").and_then(|v| v.as_str()) {
            cue.notes = notes.to_string();
        }
        if let Some(ms) = value.get("pre_wait_ms").and_then(|v| v.as_u64()) {
            cue.pre_wait = Duration::from_millis(ms);
        }
        if let Some(ms) = value.get("post_wait_ms").and_then(|v| v.as_u64()) {
            cue.post_wait = Duration::from_millis(ms);
        }
        if let Some(cm) = value.get("continue_mode") {
            if let Ok(mode) = serde_json::from_value(cm.clone()) {
                cue.continue_mode = mode;
            }
        }
        if let Some(col) = value.get("color") {
            if let Ok(color) = serde_json::from_value(col.clone()) {
                cue.color = color;
            }
        }
        if let Some(path) = value.get("file_path").and_then(|v| v.as_str()) {
            cue.file_path = Some(PathBuf::from(path));
        }
        if let Some(db) = value.get("volume_db").and_then(|v| v.as_f64()) {
            cue.volume_db = db;
        }
        if let Some(trim) = value.get("trim_db") {
            cue.trim_db = trim.as_f64().filter(|db| (-60.0..=12.0).contains(db))
                .ok_or_else(|| anyhow!("Audio cue trim must be between −60 and +12 dB."))?;
        }
        if let Some(value) = value.get("output_trim").filter(|value| !value.is_null()) {
            let trim: super::output_trim::OutputTrim = serde_json::from_value(value.clone())?;
            trim.validate().map_err(anyhow::Error::msg)?;
            cue.output_trim = Some(trim);
        }
        if let Some(pan) = value.get("pan").and_then(|v| v.as_f64()) {
            cue.pan = pan as f32;
        }
        if let Some(ms) = value.get("fade_in_ms").and_then(|v| v.as_u64()) {
            let curve = value.get("fade_in_curve")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(FadeCurve::SCurve);
            cue.fade_in = Some(FadeSpec { duration_ms: ms, curve });
        }
        if let Some(ms) = value.get("fade_out_ms").and_then(|v| v.as_u64()) {
            let curve = value.get("fade_out_curve")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(FadeCurve::SCurve);
            cue.fade_out = Some(FadeSpec { duration_ms: ms, curve });
        }
        if let Some(ms) = value.get("start_time_ms").and_then(|v| v.as_u64()) {
            cue.start_time = Some(Duration::from_millis(ms));
        }
        if let Some(ms) = value.get("end_time_ms").and_then(|v| v.as_u64()) {
            cue.end_time = Some(Duration::from_millis(ms));
        }
        if let Some(lc) = value.get("loop_count").and_then(|v| v.as_u64()) {
            cue.loop_count = lc as u32;
        }
        if let Some(patch_str) = value.get("output_patch_id").and_then(|v| v.as_str()) {
            cue.output_patch_id = patch_str.parse().ok();
        }
        if let Some(rate) = value.get("rate").and_then(|v| v.as_f64()) {
            cue.rate = crate::engine::rate_fader::normalize_rate(rate);
        }
        if let Some(effects) = value.get("audio_effects") {
            cue.effects = serde_json::from_value(effects.clone()).context("Invalid Audio cue effects")?;
            validate_effects(&cue.effects).map_err(|message| anyhow!(message))?;
        }
        if let Some(s) = value.get("slices") {
            if let Ok(mut slices) = serde_json::from_value::<crate::cue::types::SliceList>(s.clone()) {
                slices.normalize();
                cue.slices = slices;
            }
        }
        // Absent (every workspace written before matrices existed) stays None,
        // which is the untouched pan path.
        // The baked matrix remains playable if a newer desk adds point metadata
        // this version cannot edit.
        cue.point_mix = value.get("point_mix").and_then(|v| serde_json::from_value(v.clone()).ok());
        cue.matrix_mirror_mono = value.get("matrix_mirror_mono").and_then(Value::as_bool).unwrap_or(true);
        cue.source_channel_names = serde_json::from_value(value["source_channel_names"].clone()).unwrap_or_default();
        if let Some(m) = value.get("level_matrix") {
            if let Ok(rows) = serde_json::from_value::<Option<Vec<Vec<f64>>>>(m.clone()) {
                cue.level_matrix = rows.filter(|r| !r.is_empty());
            }
        }
        if let Some(b) = value.get("is_disabled").and_then(|v| v.as_bool()) {
            cue.is_disabled = b;
        }

        Ok(Box::new(cue))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod effects_tests {
    use super::*;

    #[test]
    fn cue_effects_round_trip_in_order_and_legacy_defaults_to_dry() {
        let chain = json!([
            {"kind":"low_pass", "id":Uuid::new_v4(), "enabled":false, "frequency_hz":8000.0},
            {"kind":"peak", "id":Uuid::new_v4(), "enabled":true, "frequency_hz":1200.0, "gain_db":-6.0, "q":2.0},
            {"kind":"high_pass", "id":Uuid::new_v4(), "enabled":true, "frequency_hz":100.0}
        ]);
        let mut cue = AudioCue::new().serialize();
        cue.as_object_mut().unwrap().remove("audio_effects");
        let old = AudioCueFactory.from_json(cue.clone()).unwrap();
        assert_eq!(old.serialize()["audio_effects"], json!([]));
        cue["audio_effects"] = chain;
        let restored = AudioCueFactory.from_json(cue.clone()).unwrap();
        assert_eq!(restored.serialize()["audio_effects"], cue["audio_effects"]);
        cue["audio_effects"][0]["frequency_hz"] = json!(1);
        assert!(AudioCueFactory.from_json(cue).is_err());
    }

    #[test]
    fn legacy_effects_gain_saved_ids_and_move_without_changing_identity() {
        let mut cue = AudioCue::new().serialize();
        cue["audio_effects"] = json!([
            {"kind":"low_pass", "enabled":true, "frequency_hz":8000.0},
            {"kind":"high_pass", "enabled":true, "frequency_hz":100.0},
        ]);
        let initial = AudioCueFactory.from_json(cue).unwrap();
        let ids = initial.audio_filter_cutoffs();
        assert_ne!(ids[0].0, ids[1].0);
        let mut reordered = initial.serialize();
        reordered["audio_effects"].as_array_mut().unwrap().reverse();
        let restored = AudioCueFactory.from_json(reordered.clone()).unwrap();
        assert_eq!(restored.audio_filter_cutoffs(), vec![ids[1], ids[0]]);
        reordered["audio_effects"][0]["id"] = json!(ids[0].0);
        assert!(AudioCueFactory.from_json(reordered).is_err());
    }
}

#[cfg(test)]
mod level_matrix_tests {
    use super::*;

    #[test]
    fn a_cue_has_no_matrix_by_default() {
        // The guarantee this whole feature rests on: existing cues keep the
        // original pan + patch routing, untouched.
        assert!(AudioCue::new().level_matrix.is_none());
    }

    #[test]
    fn columns_map_through_the_output_patch_channels() {
        // Patch feeds device channels 4 and 5; column 0 must land on 4.
        let spec = vec![vec![0.0, -60.0], vec![-60.0, 0.0]];
        let matrix = build_level_matrix(&spec, &[4, 5], true).expect("routed");

        assert!((matrix.gain(0, 4) - 1.0).abs() < 1e-6, "input L → patch channel 0 (device 4)");
        assert_eq!(matrix.gain(0, 5), 0.0, "and nothing to the other one");
        assert!((matrix.gain(1, 5) - 1.0).abs() < 1e-6, "input R → patch channel 1 (device 5)");
        assert_eq!(matrix.width(), 6, "width covers up to the highest used device channel");
    }

    #[test]
    fn without_a_patch_columns_are_device_channels() {
        let spec = vec![vec![0.0], vec![0.0]];
        let matrix = build_level_matrix(&spec, &[], true).expect("routed");
        assert!((matrix.gain(0, 0) - 1.0).abs() < 1e-6);
        assert_eq!(matrix.width(), 1);
    }

    #[test]
    fn an_empty_spec_yields_no_matrix() {
        // Nothing configured — the voice stays on the ordinary pan path.
        assert!(build_level_matrix(&[], &[], true).is_none());
        assert!(build_level_matrix(&[vec![], vec![]], &[], true).is_none());
    }

    #[test]
    fn an_all_silent_matrix_is_kept_and_mutes_the_cue() {
        // The operator pulled every crosspoint down; honouring that means
        // silence. Falling back to pan routing here would make the cue audible
        // against an explicit instruction.
        let matrix = build_level_matrix(&[vec![-60.0, -60.0]], &[], true).expect("kept");
        assert_eq!(matrix.gain(0, 0), 0.0);
    }

    #[test]
    fn patch_aliases_sum_and_unassigned_columns_cannot_leak_to_hardware() {
        let matrix = build_level_matrix(&[vec![0.0, -6.0, 0.0, -60.0]], &[6, 6], true).unwrap();
        assert!((matrix.gain(0, 6) - 1.5011872).abs() < 0.00001);
        assert_eq!(matrix.gain(0, 2), 0.0);
    }

    #[test]
    fn live_matrix_edits_reach_the_engine_one_crosspoint_at_a_time() {
        // Independent inputs can contribute to the same physical output.
        let matrix = build_level_matrix(&[vec![0.0], vec![0.0]], &[], true).expect("routed");
        assert_eq!(matrix.gain(0, 0), 1.0);
        assert_eq!(matrix.gain(1, 0), 1.0);
    }

    #[test]
    fn crosspoints_past_the_fixed_capacity_are_dropped_not_wrapped() {
        let wide: Vec<f64> = (0..MATRIX_OUTPUTS + 4).map(|_| 0.0).collect();
        let matrix = build_level_matrix(&[wide], &[], true).expect("routed");
        assert_eq!(matrix.gain(0, MATRIX_OUTPUTS - 1), 1.0);
        assert_eq!(matrix.gain(0, MATRIX_OUTPUTS), 0.0);
    }

    #[test]
    fn serialize_roundtrip_preserves_the_matrix() {
        let factory = AudioCueFactory;
        let mut cue = AudioCue::new();
        cue.level_matrix = Some(vec![vec![0.0, -6.0], vec![-6.0, 0.0]]);

        let rebuilt = factory.from_json(cue.serialize()).unwrap();
        let json = rebuilt.serialize();

        assert_eq!(json["level_matrix"][0][1], -6.0);
        assert_eq!(json["level_matrix"][1][0], -6.0);
    }

    #[test]
    fn a_workspace_written_before_matrices_existed_loads_without_one() {
        let factory = AudioCueFactory;
        let mut json = AudioCue::new().serialize();
        json.as_object_mut().unwrap().remove("level_matrix");

        let rebuilt = factory.from_json(json).unwrap();

        assert_eq!(rebuilt.serialize()["level_matrix"], serde_json::Value::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_output_withdrawal_releases_paused_descendants_and_keeps_siblings() {
        use crate::cue::group_cue::GroupCue;
        use std::collections::HashSet;

        let mut paused = AudioCue::new();
        let withdrawn_voice = Uuid::new_v4();
        paused.active_voice_id = Some(withdrawn_voice);
        paused.state = CueState::Paused;
        let paused_id = paused.id();

        let mut sibling = AudioCue::new();
        let sibling_voice = Uuid::new_v4();
        sibling.active_voice_id = Some(sibling_voice);
        sibling.state = CueState::Running;

        let mut inner = GroupCue::new();
        inner.children.push(Box::new(paused));
        inner.children.push(Box::new(sibling));
        let mut outer = GroupCue::new();
        outer.children.push(Box::new(inner));

        let mut changes = Vec::new();
        let withdrawn = HashSet::from([withdrawn_voice]);
        assert!(crate::show::event_loop::reset_withdrawn_output(&mut outer, &withdrawn, &mut changes));
        assert_eq!(changes, [(paused_id, CueState::Paused)]);
        assert!(outer.has_withdrawn_output());
        assert!(outer.is_auto_continue_fired());
        let inner = &outer.children[0];
        assert!(inner.has_withdrawn_output());
        let children = inner.child_cues().unwrap();
        assert_eq!(children[0].state(), CueState::Standby);
        assert_eq!(children[0].playing_voice_id(), None);
        assert_eq!(children[1].state(), CueState::Running);
        assert_eq!(children[1].playing_voice_id(), Some(sibling_voice));
        assert!(!crate::show::event_loop::reset_withdrawn_output(&mut outer, &withdrawn, &mut changes));
        assert_eq!(changes.len(), 1);
    }

    fn make_cue() -> AudioCue {
        let mut c = AudioCue::new();
        c.set_name("Test Audio".to_string());
        c.set_number(Some("1".to_string()));
        c.volume_db = -6.0;
        c.pan = 0.5;
        c.fade_in = Some(FadeSpec::new(500));
        c.pre_wait = Duration::from_millis(1000);
        c.post_wait = Duration::from_millis(200);
        c.continue_mode = ContinueMode::AutoContinue;
        c
    }

    #[test]
    fn serialize_roundtrip() {
        let cue = make_cue();
        let json = cue.serialize();

        let factory = AudioCueFactory;
        let restored = factory.from_json(json).expect("should deserialize");

        assert_eq!(restored.name(), "Test Audio");
        assert_eq!(restored.number(), Some("1"));
        assert_eq!(restored.continue_mode(), ContinueMode::AutoContinue);
    }

    #[test]
    fn serialize_roundtrip_preserves_slices() {
        use crate::cue::types::{SliceList, PLAY_COUNT_INFINITE};
        let mut cue = make_cue();
        cue.slices = SliceList {
            markers: vec![2000, 8000],
            play_counts: vec![1, PLAY_COUNT_INFINITE, 2],
        };
        let json = cue.serialize();
        let restored = AudioCueFactory.from_json(json).expect("deserialize");
        let restored_json = restored.serialize();
        let slices: SliceList =
            serde_json::from_value(restored_json.get("slices").unwrap().clone()).unwrap();
        assert_eq!(slices.markers, vec![2000, 8000]);
        assert_eq!(slices.play_counts, vec![1, PLAY_COUNT_INFINITE, 2]);
    }

    #[test]
    fn legacy_json_without_slices_loads_empty() {
        let cue = make_cue();
        let mut json = cue.serialize();
        json.as_object_mut().unwrap().remove("slices");
        let restored = AudioCueFactory.from_json(json).expect("deserialize");
        let restored_json = restored.serialize();
        let slices: crate::cue::types::SliceList =
            serde_json::from_value(restored_json.get("slices").unwrap().clone()).unwrap();
        assert!(slices.is_empty());
    }

    #[test]
    fn initial_state_is_standby() {
        let cue = AudioCue::new();
        assert_eq!(cue.state(), CueState::Standby);
        assert!(!cue.is_running());
        assert!(!cue.is_paused());
    }

    #[test]
    fn elapsed_zero_before_go() {
        let cue = AudioCue::new();
        assert_eq!(cue.elapsed(), Duration::ZERO);
        assert_eq!(cue.action_elapsed(), Duration::ZERO);
    }

    #[test]
    fn cue_number_is_string() {
        let mut cue = AudioCue::new();
        cue.set_number(Some("1.5.1".to_string()));
        assert_eq!(cue.number(), Some("1.5.1"));
        cue.set_number(Some("Intro".to_string()));
        assert_eq!(cue.number(), Some("Intro"));
        cue.set_number(None);
        assert_eq!(cue.number(), None);
    }
}
