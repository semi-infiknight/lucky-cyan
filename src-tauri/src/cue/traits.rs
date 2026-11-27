//! The [`Cue`] trait — the universal contract for every cue type in Inkue.
//!
//! All cue types (Audio, Memo, Wait, Group, …) implement this trait so that the
//! Show Engine can drive them uniformly through `dyn Cue`.  The trait is
//! **object-safe**: every method either takes `&self`/`&mut self` or returns
//! a type with a known size.

use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use super::{
    context::CueContext,
    types::{ContinueMode, CueColor, CueId, CueState, CueType, FadeAction, FadeCrossfade, GroupMode},
};

/// Live audio parameters to re-apply to a cue's currently-playing voice after an
/// inspector edit, so volume/pan changes take effect without restarting
/// playback.  Returned by [`Cue::live_audio_params`].
#[derive(Debug, Clone)]
pub struct LiveAudioParams {
    /// The engine voice id to update.
    pub voice_id: CueId,
    /// Linear gain to apply (already converted from dB).
    pub gain: f32,
    /// Stereo pan (-1.0 .. 1.0).
    pub pan: f32,
    /// Crosspoint levels in dB, `[input][patch channel]`, when the cue has a
    /// level matrix.  `None` clears any matrix on the voice, putting it back
    /// on pan routing.  `update_cue` resolves the columns against the cue's
    /// Output Patch, exactly as the GO path does.
    pub level_matrix: Option<Vec<Vec<f64>>>,
    pub matrix_mirror_mono: bool,
}

// ---------------------------------------------------------------------------
// CueFactory trait
// ---------------------------------------------------------------------------

/// Factory for a specific cue type.  Each cue type registers one factory in
/// the [`super::registry::CueRegistry`].
pub trait CueFactory: Send + Sync {
    /// Create a new, empty cue of this type with a fresh UUID.
    fn create(&self) -> Box<dyn Cue>;

    /// Deserialise a cue from its JSON representation.
    #[allow(clippy::wrong_self_convention)]
    fn from_json(&self, value: Value) -> Result<Box<dyn Cue>>;
}

// ---------------------------------------------------------------------------
// Cue trait
// ---------------------------------------------------------------------------

/// The universal cue contract.  Implementors must be `Send` so they can be
/// moved across thread boundaries (e.g., when loading a workspace on a worker
/// thread).
pub trait Cue: Send + std::any::Any {
    // -----------------------------------------------------------------------
    // Identity
    // -----------------------------------------------------------------------

    /// Unique identifier for this cue instance.
    fn id(&self) -> CueId;

    /// The discriminant type of this cue (Audio, Memo, …).
    fn cue_type(&self) -> CueType;

    /// Human-readable name of the cue (editable by the operator).
    fn name(&self) -> &str;

    /// Update the cue's name.
    fn set_name(&mut self, name: String);

    /// Optional alphanumeric cue number (e.g. "1", "1.5", "Intro").
    /// This is a *string*, not a numeric index.
    fn number(&self) -> Option<&str>;

    /// Update the cue number.  Pass `None` to clear it.
    fn set_number(&mut self, number: Option<String>);

    /// Free-form notes visible in the inspector.
    fn notes(&self) -> &str;

    /// Update the notes field.
    fn set_notes(&mut self, notes: String);

    /// Colour label shown on the cue row in the Cue List.
    fn color(&self) -> CueColor;

    /// Update the colour label.
    fn set_color(&mut self, color: CueColor);

    /// Whether this cue is disabled.  Disabled cues are skipped by the
    /// transport — the Playhead advances past them automatically.
    fn is_disabled(&self) -> bool {
        false
    }

    /// Enable or disable this cue.
    fn set_disabled(&mut self, _disabled: bool) {}

    /// Optional timecode trigger: the SMPTE position at which this cue fires
    /// when the CueList's TC sync is enabled.  `None` = not TC-triggered.
    fn tc_trigger(&self) -> Option<&crate::engine::timecode_types::TcTrigger> {
        None
    }

    /// Set or clear the TC trigger for this cue.
    fn set_tc_trigger(&mut self, _trigger: Option<crate::engine::timecode_types::TcTrigger>) {}

    /// For media cues (Audio, Video, Image): the file path as stored in the
    /// cue (may be relative to the workspace directory).  Used by the command
    /// layer to detect broken cues without calling `serialize()`.
    fn media_file_path(&self) -> Option<&std::path::Path> {
        None
    }

    /// Memo Cue only: the note it carries.  A Memo has no file, so the cue
    /// list shows this in the Target column instead — which is the whole point
    /// of the cue type, and what makes an imported `[Unconverted …]`
    /// placeholder readable in the stack.
    fn memo_text(&self) -> Option<&str> {
        None
    }

    // -----------------------------------------------------------------------
    // State
    // -----------------------------------------------------------------------

    /// Current lifecycle state.
    fn state(&self) -> CueState;

    /// `true` if the cue is currently in [`CueState::Running`].
    fn is_running(&self) -> bool {
        self.state() == CueState::Running
    }

    /// `true` if the cue is currently in [`CueState::Paused`].
    fn is_paused(&self) -> bool {
        self.state() == CueState::Paused
    }

    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    /// Pre-load any resources needed for fast execution (e.g., decode audio
    /// into memory).  Called when the workspace is opened or when the
    /// operator manually loads a cue.
    fn load(&mut self, context: &CueContext) -> Result<()>;

    /// Trigger the cue at the Playhead.  Starts the pre-wait timer.
    fn go(&mut self, context: &CueContext) -> Result<()>;

    /// Stop the cue with a short fade-out (default 0.5 s).  Resets to Standby.
    fn stop(&mut self, context: &CueContext) -> Result<()>;

    /// Suspend execution mid-action.
    fn pause(&mut self, context: &CueContext) -> Result<()>;

    /// Resume a paused cue.
    fn resume(&mut self, context: &CueContext) -> Result<()>;

    /// Immediately cut playback without any fade.  Used on double-Escape.
    fn hard_stop(&mut self, context: &CueContext) -> Result<()>;

    /// Reset the cue to its initial Standby state (clears elapsed time etc.).
    fn reset(&mut self) -> Result<()>;

    /// Called by the event loop at ~30 fps for every Running cue.
    ///
    /// The default implementation is a no-op.  Audio cues override this to
    /// handle the pre-wait phase: once `pre_wait` has elapsed the audio
    /// action starts without `go()` having to block on a timer.
    fn tick(&mut self, _context: &CueContext) -> Result<()> {
        Ok(())
    }

    /// Returns `false` while the cue is in its Pre-Wait phase (i.e. `go()`
    /// has been called but the action has not yet started).
    ///
    /// The event loop uses this to avoid firing Auto-Continue before the
    /// action — and therefore Post-Wait — has actually begun.
    /// Default: `true` (most cue types start their action synchronously).
    fn is_action_started(&self) -> bool {
        true
    }

    /// Opt in to action-relative continuation for a running timed action.
    /// Its Auto-Continue uses Post-Wait from action start even if the action
    /// has already finished sending output, and Auto-Follow waits for completion.
    fn uses_action_clock_for_continue(&self) -> bool {
        false
    }

    // -----------------------------------------------------------------------
    // Timing
    // -----------------------------------------------------------------------

    /// Delay inserted before the cue's action begins.
    fn pre_wait(&self) -> Duration;

    /// Update the Pre-Wait duration.
    fn set_pre_wait(&mut self, d: Duration);

    /// Delay after the action *starts* (not ends) before continue mode fires.
    fn post_wait(&self) -> Duration;

    /// Update the Post-Wait duration.
    fn set_post_wait(&mut self, d: Duration);

    /// Whether the wall clock has finished this run.
    fn is_time_complete(&self) -> bool {
        self.is_action_started() && !self.awaits_voice_completion()
            && self.duration().is_some_and(|duration| self.action_elapsed() >= duration)
    }

    /// Whether the engine's completion signal is authoritative for this run.
    fn awaits_voice_completion(&self) -> bool {
        false
    }

    fn supports_silent_speed(&self) -> bool { false }

    fn uses_silent_speed_clock(&self) -> bool { false }

    /// Automatic fade-out for a decoded Audio voice.
    fn natural_fade_out(&self) -> Option<(u32, crate::engine::ring_command::FadeCurve)> {
        None
    }

    /// Total duration of the cue's action, if known in advance.
    /// Returns `None` for open-ended cues (e.g., looping audio).
    fn duration(&self) -> Option<Duration>;

    /// Total time elapsed since `go()` was called (including pre-wait).
    fn elapsed(&self) -> Duration;

    /// Time elapsed since the action started (i.e., after pre-wait).
    fn action_elapsed(&self) -> Duration;

    // -----------------------------------------------------------------------
    // Continue mode
    // -----------------------------------------------------------------------

    /// What happens after this cue's Post-Wait expires.
    fn continue_mode(&self) -> ContinueMode;

    /// Update the continue mode.
    fn set_continue_mode(&mut self, mode: ContinueMode);

    // -----------------------------------------------------------------------
    // Runtime helpers
    // -----------------------------------------------------------------------

    /// Seek to `position_ms` from the start of the cue's action.
    ///
    /// For audio cues this repositions the audio voice.  For video cues it
    /// issues an mpv seek and re-anchors the paired audio voice.  Non-seekable
    /// cue types (Memo, Stop, …) use the default no-op.
    ///
    /// The caller is responsible for updating any transport-level timing only
    /// when the cue is actually running or paused; calling seek on a standby
    /// cue has no effect.
    fn seek(&mut self, _position_ms: u64, _ctx: &CueContext) {}

    /// Clip-relative one-shot start prepared by the local rehearsal tool.
    fn loaded_time_ms(&self) -> Option<u64> { None }

    fn cancel_loaded_time(&mut self) -> bool { false }

    /// An ordinary Load cue created a held engine voice that awaits a Start.
    fn has_prepared_media(&self) -> bool { false }

    /// `true` when [`seek`](Cue::seek) moves this cue's playback — the UI
    /// offers scrubbing only then.  Types that implement `seek` override it.
    fn supports_seek(&self) -> bool {
        false
    }

    /// Inject pre-decoded audio samples that were decoded *outside* the
    /// workspace mutex.  The caller decodes on a background thread, then
    /// briefly re-acquires the mutex to call this method.  Non-audio cues
    /// ignore the call (default no-op).
    fn accept_preloaded_audio(
        &mut self,
        _samples: std::sync::Arc<Vec<f32>>,
        _channels: u16,
        _sample_rate: u32,
        _duration: std::time::Duration,
    ) {
    }

    /// Returns the active audio voice ID if this cue is currently playing
    /// through the audio engine.  Non-audio cues return `None` (default).
    /// Used by the event loop to correlate [`crate::engine::ring_command::AudioStatus::Completed`]
    /// events back to the owning cue.
    fn playing_voice_id(&self) -> Option<CueId> {
        None
    }

    /// Every **visual** voice (output layer) this cue currently shows,
    /// recursively — the picture counterpart of [`all_voice_ids`](Self::all_voice_ids).
    /// A visual leaf cue reports its own layer; a Group flattens its children,
    /// so a Group holding a video is a real visual target for a Fade.
    fn visual_voice_ids(&self) -> Vec<CueId> {
        if self.is_visual() {
            self.playing_voice_id().into_iter().collect()
        } else {
            Vec::new()
        }
    }

    /// Every audio voice this cue currently controls, **recursively**.
    ///
    /// For a leaf cue this is just its own [`playing_voice_id`](Self::playing_voice_id)
    /// (the default). A [`GroupCue`](crate::cue::group_cue::GroupCue) overrides this
    /// to flatten the voices of all its children (at any depth), so a single Group
    /// can be a first-class target for volume/pan Fades and Stops — the transport
    /// asks "what voices does this target own?" instead of assuming one voice on a
    /// top-level cue.
    fn all_voice_ids(&self) -> Vec<CueId> {
        self.playing_voice_id().into_iter().collect()
    }

    /// Hold automatic continuation after a direct child loses a required output.
    fn child_output_withdrawn(&mut self, _child_id: CueId) {}

    fn has_withdrawn_output(&self) -> bool { false }

    /// This run still needs to admit its output or effect before continuation.
    fn pending_start_admission(&self) -> bool { false }

    /// True for an armed Audio FX Fade whose action must bind before continuing.
    fn requires_eq_gain_admission(&self) -> bool { false }

    /// Reports a rejected required-output start once, for operator feedback.
    fn take_output_start_hold(&mut self) -> bool { false }

    /// Monotonically increasing counter incremented on every `go()` call.
    /// Reserved for diagnostics / future use.  Default: 0.
    fn play_generation(&self) -> u64 {
        0
    }

    /// Returns `true` if Auto-Continue has already been fired for the
    /// **current** play of this cue.  The transport sets this flag
    /// synchronously inside `go()` before chaining, so the event loop
    /// never sees the cue as needing a second chain.
    fn is_auto_continue_fired(&self) -> bool {
        false
    }

    /// Mark Auto-Continue as fired for the current play.
    /// Called by [`Transport::go`] immediately after chaining.
    fn mark_auto_continue_fired(&mut self) {}

    /// Reset the Auto-Continue fired flag.  Called by `go()` (new play) and
    /// `reset()` / `stop()` (cue stopped or completed).
    fn clear_auto_continue_fired(&mut self) {}

    /// Full source duration without start/end markers, or the period of an
    /// authored loop. By default this is the cue's action duration.
    fn file_duration(&self) -> Option<Duration> {
        self.duration()
    }

    /// Return a cheap clone of the pre-decoded audio data already in memory,
    /// or `None` if the cue has not been decoded yet.
    ///
    /// Used by [`update_cue`](crate::commands::cue_cmds::update_cue) to
    /// preserve decoded samples across cue rebuilds (name/colour/timing
    /// changes must not force a re-decode).  Non-audio cues return `None`.
    fn extract_decoded_audio(
        &self,
    ) -> Option<(std::sync::Arc<Vec<f32>>, u16, u32, Duration)> {
        None
    }

    /// Downsample the decoded audio into `bins` peak values (0.0 – 1.0) for
    /// waveform display.  Returns `None` if no audio data is loaded yet.
    /// Non-audio cues always return `None` (default).
    fn waveform_peaks(&self, _bins: usize) -> Option<Vec<f32>> {
        None
    }

    /// Called when the underlying media's total duration becomes known at
    /// runtime (e.g., after a video file's metadata loads in the surface
    /// window).  Non-video cues can ignore this call (default no-op).
    fn set_runtime_duration(&mut self, _duration: std::time::Duration) {}

    /// Logical source names are display metadata. Mutating them in place keeps the
    /// active voice, fades, decoded media and video surface untouched.
    fn source_channel_names_mut(&mut self) -> Option<&mut Vec<String>> {
        None
    }

    /// The Output Patch this cue routes audio through, when it has one
    /// assigned.  `None` = no patch (audio-producing cues then use the
    /// workspace default patch).  Used by the cue list's Output column.
    fn output_patch_id(&self) -> Option<uuid::Uuid> {
        None
    }

    /// If `true`, [`Transport::go`] will automatically stop this cue when the
    /// next GO fires.  Default: `false`.  Only the Text Cue opts in — visual
    /// cues (Video/Image/Camera) are layers and never auto-stop each other.
    fn stop_on_next_go(&self) -> bool {
        false
    }

    /// `true` for cue types that occupy the visual output surface (Video,
    /// Image, Camera).  Used by the transport's stop-on-next-GO filter and by
    /// Fade target resolution.  Override instead of adding the type to a
    /// match in `show/transport.rs`.
    fn is_visual(&self) -> bool {
        false
    }

    /// Fade Cue only: returns the fade parameters so the transport can resolve
    /// target voices and call [`set_fade_voices`] before the first tick.
    fn fade_specification(&self) -> Option<FadeAction> {
        None
    }

    /// Enabled high-pass and low-pass filters keyed by their saved identities.
    fn audio_filter_cutoffs(&self) -> Vec<(CueId, f32)> {
        Vec::new()
    }

    fn audio_eq_gains(&self) -> Vec<(CueId, f32)> { Vec::new() }

    fn accepts_effect_destination(&self, destination: super::types::EffectDestination) -> bool {
        use super::types::EffectDestination;
        let (values, effect_id) = match destination {
            EffectDestination::Cutoff(filter) => (self.audio_filter_cutoffs(), filter.filter_id),
            EffectDestination::EqGain(eq) => (self.audio_eq_gains(), eq.filter_id),
        };
        values.iter().any(|(id, _)| *id == effect_id)
    }

    /// Inject resolved audio voice IDs (and their current gains) into this cue,
    /// plus per-layer visual fade targets for any Video/Image/Camera targets.
    ///
    /// - `voices`: `(audio_voice_id, start_gain, start_pan)` for each audio target.
    /// - `visual_targets`: `(output_voice_id, start_opacity)` for each visual
    ///   target's layer.
    /// - `visual_target_opacity`: layer opacity when the fade completes
    ///   (0.0 = black, 1.0 = fully visible).
    ///
    /// Called by [`crate::show::transport::Transport::go`] after `go()` so that
    /// `tick()` knows which voices/layers to update.
    fn set_fade_voices(
        &mut self,
        _voices: Vec<(CueId, f32, f32)>,
        _visual_targets: Vec<(CueId, f32)>,
        _visual_target_opacity: f32,
    ) {}

    /// Stop Cue only: describes what to stop after `go()` completes.
    ///
    /// Returns `Some((hard_stop, target_cue_ids))` where:
    /// - `hard_stop` — `true` = immediate cut, `false` = soft fade.
    /// - `target_cue_ids` — empty = stop all, non-empty = stop those UUIDs only.
    ///
    /// Transport reads this and executes the stop **before** evaluating
    /// Auto-Follow chains, preventing the chained cue from being killed.
    fn stop_specification(&self) -> Option<(bool, Vec<CueId>)> {
        None
    }

    /// `true` while this cue plays with an active slice program — the UI's
    /// time display then follows the engine-reported **media position**
    /// instead of wall-clock elapsed (a vamp holds position while the clock
    /// runs on).  Default: `false`.
    fn uses_sliced_playback(&self) -> bool {
        false
    }

    /// Devamp Cue only: describes which cues to devamp after `go()`.
    ///
    /// Returns `Some((stop_at_end, target_cue_ids))`:
    /// - `stop_at_end` — `true` = the target stops at the end of its current
    ///   slice, `false` = it continues into the next slice.
    ///
    /// Transport resolves each target's voices (audio + visual + a video's
    /// paired audio voice) and forwards the devamp to the engines.
    fn devamp_specification(&self) -> Option<(bool, Vec<CueId>)> {
        None
    }

    /// Devamp Cue only: a Whole-cue release waiting for this voice to reach its
    /// natural pass end before the cue completes and fires its successor
    /// (QLab "start next cue when the loop ends").
    fn devamp_boundary_voice(&self) -> Option<uuid::Uuid> {
        None
    }

    /// Prepare for a later Start without executing any part of the cue action.
    /// Cue types opt in when they can warm resources without producing output.
    fn preload(&mut self, _context: &CueContext) -> Result<()> {
        Ok(())
    }

    /// Command cues only (Start, Pause, Resume, Load, Reset, Goto, Arm,
    /// Disarm): describes what to do to which cues after `go()`.
    ///
    /// Returns `Some((action, target_cue_ids))`. Unlike a Stop Cue, an empty
    /// target list yields `None` rather than "every cue" — resetting or
    /// starting a whole show by omission is not a useful default.
    ///
    /// Transport executes this alongside the stop specification, before
    /// Auto-Follow is evaluated, and resolves targets recursively so a cue
    /// nested in a Group can be addressed.
    fn control_specification(
        &self,
    ) -> Option<(crate::cue::control_cue::ControlAction, Vec<CueId>)> {
        None
    }

    /// The replacement destination configured on a Target cue.
    fn replacement_target(&self) -> Option<CueId> { None }

    /// Temporary target set by a Target cue. Never part of the saved document.
    fn set_temporary_target(&mut self, _id: Option<CueId>) {}
    fn temporary_target(&self) -> Option<CueId> { None }

    // QLab OSC media-cue properties: saved playback rate and crosspoint level
    // matrix (`/cue/{n}/rate`, `/cue/{n}/level/{row}/{col}`), plus the `/live`
    // overrides that drive a running voice without touching saved data.
    /// Saved playback rate (1.0 = normal); `None` when the cue type has none.
    fn rate(&self) -> Option<f64> { None }
    fn set_rate(&mut self, _rate: f64) {}
    /// Saved crosspoint level in dB at `[row][col]`; `None` when unset or the
    /// cue type has no level matrix.
    fn level(&self, _row: usize, _col: usize) -> Option<f64> { None }
    fn set_level(&mut self, _row: usize, _col: usize, _db: f64) {}
    /// Live rate override from an OSC `/live` write (cleared on reset).
    fn live_rate(&self) -> Option<f64> { None }
    fn set_live_rate(&mut self, _rate: Option<f64>) {}
    /// Live level override in dB from an OSC `/live` write (cleared on reset).
    fn live_level(&self, _row: usize, _col: usize) -> Option<f64> { None }
    fn set_live_level(&mut self, _row: usize, _col: usize, _db: Option<f64>) {}
    /// Clear every live level override (on reset).
    fn clear_live_levels(&mut self) {}
    /// Running engine voice of this cue, when playing (for live writes).
    fn media_voice_id(&self) -> Option<crate::engine::ring_command::VoiceId> { None }
    /// Crosspoint mono-mirroring flag used when building the live matrix.
    fn level_mirror_mono(&self) -> bool { false }

    fn clear_temporary_targets(&mut self) {
        self.set_temporary_target(None);
        self.set_live_rate(None);
        self.clear_live_levels();
        if let Some(children) = self.child_cues_mut() {
            for child in children { child.clear_temporary_targets(); }
        }
    }

    /// Fade Cue only: drain the cue ids whose cues should be hard-stopped now
    /// that a `stop_at_end` fade has finished.  The event loop calls this every
    /// tick and stops the returned cues (the Fade cannot reach the cue list from
    /// `tick`).  Default: nothing to stop.
    fn take_fade_stop_targets(&mut self) -> Vec<CueId> {
        Vec::new()
    }

    /// Fade Cue only: the show layer has set up — or given up on — this Fade's
    /// crossfade (see [`FadeCrossfade`]).  Default: ignored.
    fn set_crossfade(&mut self, _crossfade: FadeCrossfade) {}

    /// Fade Cue only: the crossfade still waiting to be set up, as
    /// `(incoming cue, already started)`.  Default: none.
    fn pending_crossfade(&self) -> Option<(CueId, bool)> {
        None
    }

    /// Resolve stop/fade targets from cue-number strings to UUIDs.
    ///
    /// Called once per cue after the whole cue list is loaded, allowing cues
    /// saved in the old format (number only, no UUID) to be upgraded
    /// in-memory for the current session.  Default implementation is a no-op.
    fn resolve_stop_target(&mut self, _number_to_id: &std::collections::HashMap<String, CueId>) {}

    /// Fade Cue only: resolve target UUIDs from cue-number labels.
    /// Whether this Fade's action is ready to resolve its running Audio targets.
    fn rate_fade_targets_pending(&self) -> bool {
        false
    }

    /// Bind the running Audio voices that this Fade can automate.
    fn set_rate_fade_targets(&mut self, _faders: Vec<std::sync::Arc<crate::engine::rate_fader::RateFader>>) {}

    fn effect_target_pending(&self) -> Option<(CueId, super::types::EffectDestination)> { None }

    fn set_effect_fader(&mut self, _fader: Option<std::sync::Arc<crate::engine::rate_fader::ScalarFader>>, _context: &CueContext) {}

    fn silent_speed_target_pending(&self) -> Option<CueId> { None }
    fn set_silent_speed_voice(&mut self, _voice: Option<CueId>, _context: &CueContext) {}

    /// The visual target whose live geometry this Fade needs at action start.
    fn geometry_target_pending(&self) -> Option<(CueId, bool)> { None }
    fn set_geometry_voice(&mut self, _voice: Option<CueId>, _context: &CueContext) {}

    fn applied_point_mix(&self) -> Option<&super::point_mix::PointMix> { None }

    fn map_motion_pending(&self) -> Option<(CueId, super::point_mix::MapMotion)> { None }

    fn set_map_motion_run(&mut self, _run: Option<super::point_mix::MapMotionRun>) {}

    fn resolve_fade_targets(&mut self, _number_to_id: &std::collections::HashMap<String, CueId>) {}

    /// Move the current run from the old instance after a successful property edit.
    /// Cue implementations retain the player handles, deadlines and one-shot flags
    /// of this performance independently of the newly validated programming.
    fn inherit_playback(&mut self, previous: &mut dyn Cue);

    /// Live audio parameters to push to the cue's currently-playing voice after
    /// an inspector edit (volume / pan), so changes apply without restarting.
    /// Returns `None` when the cue has no live voice.  Default is `None`.
    fn live_audio_params(&self) -> Option<LiveAudioParams> {
        None
    }

    /// Per-cue visual geometry (Video / Image cues only).  Used by `update_cue`
    /// to live-apply Geometry-tab edits to the content currently on the output
    /// window.  Default is `None` (no visual output).
    fn visual_geometry(&self) -> Option<crate::engine::output_engine::VideoGeometry> {
        None
    }

    /// Whether the saved cue opts into centre rotation of a still PNG or JPEG.
    fn is_still_rotation_target(&self) -> bool {
        self.cue_type() == CueType::Image
            && self.media_file_path().is_some_and(|path|
                crate::engine::output_engine::supports_still_rotation_path(path))
            && self.visual_geometry().and_then(|geometry| geometry.source_rotation_degrees)
                .is_some_and(f64::is_finite)
    }

    /// Compositing properties (layer / opacity / blend mode) for visual cues.
    /// Used by `update_cue` to live-apply Compositing edits to the cue's slot.
    /// Default is `None` (no visual output).
    fn layer_style(&self) -> Option<crate::engine::output_engine::LayerStyle> {
        None
    }

    // -----------------------------------------------------------------------
    // Group support
    // -----------------------------------------------------------------------

    /// Returns `true` once the cue has naturally finished all of its work and
    /// is ready to be reset.  The default (`false`) means the event loop uses
    /// voice-completion and time-based detection instead.
    ///
    /// [`GroupCue`](crate::cue::group_cue::GroupCue) overrides this: it
    /// becomes `true` when every child has completed.
    fn is_complete(&self) -> bool {
        false
    }

    /// Read-only view of direct child cues.  Returns `None` for non-Group cues.
    fn child_cues(&self) -> Option<&[Box<dyn Cue>]> {
        None
    }

    /// Mutable view of direct child cues.  Returns `None` for non-Group cues.
    fn child_cues_mut(&mut self) -> Option<&mut Vec<Box<dyn Cue>>> {
        None
    }

    /// Consume and return all children (for `ungroup`).  Returns `None` for
    /// non-Group cues.
    fn take_children(&mut self) -> Option<Vec<Box<dyn Cue>>> {
        None
    }

    /// Add a child cue at `position` (−1 = append).  Returns `Err` for
    /// non-Group cues.
    fn add_child(&mut self, _child: Box<dyn Cue>, _position: i32) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("Not a Group cue"))
    }

    /// Remove and return the child with the given ID.  Returns `Err` for
    /// non-Group cues or if the child is not found.
    fn remove_child(&mut self, _id: &CueId) -> anyhow::Result<Box<dyn Cue>> {
        Err(anyhow::anyhow!("Not a Group cue"))
    }

    /// The mode of this Group cue (`None` for non-Group cues).
    fn group_mode(&self) -> Option<GroupMode> {
        None
    }

    /// Update the Group mode.  No-op for non-Group cues.
    fn set_group_mode(&mut self, _mode: GroupMode) {}

    /// Whether this Playlist Group loops (wraps last child → first).  `None` for
    /// non-Group cues and irrelevant for other modes.
    fn playlist_loop(&self) -> Option<bool> {
        None
    }

    /// Enable/disable Playlist looping.  No-op for non-Group cues.
    fn set_playlist_loop(&mut self, _on: bool) {}

    fn playlist_advance_playhead(&self) -> Option<bool> { None }

    fn set_playlist_advance_playhead(&mut self, _advance: bool) {}

    /// Returns `true` if this cue wants to consume the next outer GO press
    /// without the transport advancing the Playhead.
    ///
    /// Only [`GroupCue`](crate::cue::group_cue::GroupCue) in Sequential mode
    /// overrides this: when the internal sequence has paused at a
    /// `DoNotContinue` child and more children remain, it absorbs GO to fire
    /// the next child internally instead of advancing the outer Playhead.
    fn absorbs_go(&self) -> bool {
        false
    }

    /// Returns `true` if this cue retains the outer Playhead on itself while
    /// it is running, so that subsequent GO presses are routed into its own
    /// internal sequence rather than advancing the outer Playhead.
    ///
    fn holds_playhead(&self) -> bool {
        false
    }

    /// Track an actual delayed-admission hold imposed by the outer transport.
    fn mark_playhead_held(&mut self) {}

    /// Consume a one-shot admission release, even if standby has moved since GO.
    fn take_playhead_release(&mut self) -> bool { false }

    /// Returns `true` once a cue that held the outer Playhead has fired
    /// everything it will fire and the Playhead should now move on to the next
    /// outer cue — even though this cue may still be running (e.g. overlapping
    /// audio children still playing out).
    ///
    /// Only Sequential [`GroupCue`] overrides this: it becomes `true` the moment
    /// its **last** child is fired, so the next GO continues the outer list
    /// instead of being absorbed.  The transport (on GO) and the event loop (on
    /// auto-advance) both consult it to release the Playhead.
    fn released_playhead(&self) -> bool {
        false
    }

    /// For a running Sequential [`GroupCue`]: the ID of the child that is
    /// currently active — either running right now, or the next one to fire on
    /// GO (when the sequence is paused at a `DoNotContinue` child).
    ///
    /// Returns `None` for non-Group cues and for Simultaneous groups (the
    /// frontend derives activity from each child's own `state()` instead).
    fn active_child_id(&self) -> Option<CueId> {
        None
    }

    /// Point a Sequential [`GroupCue`]'s internal playhead at `child_id` so the
    /// next GO fires that child (and the sequence continues from there).
    ///
    /// Returns `true` if `child_id` is a direct child of a Sequential group.
    /// Default: `false` (non-Group cues and Simultaneous groups ignore it).
    fn set_active_child(&mut self, child_id: &CueId) -> bool {
        let _ = child_id;
        false
    }

    // -----------------------------------------------------------------------
    // Preflight validation
    // -----------------------------------------------------------------------

    /// Report problems with this cue's external dependencies (dangling
    /// Stop/Fade target, unpatched fixture, absent MIDI port, …) for the
    /// "Check Workspace" preflight. Call outside the workspace lock. Cue types
    /// declare in-memory checks and optional file-dependent checks below.
    ///
    /// Media-file existence is **not** checked here — the command layer covers
    /// that centrally via [`media_file_path`](Self::media_file_path) so it can
    /// also drive relink.
    fn validate(&self, ctx: &super::validation::ValidationContext) -> Vec<super::validation::CueIssue> {
        let mut issues = self.validate_programming(ctx);
        issues.extend(self.validation_files().into_iter().filter_map(|check| check.resolve(std::path::Path::exists)));
        issues
    }

    /// In-memory checks only. Called while snapshotting the live workspace.
    fn validate_programming(&self, _ctx: &super::validation::ValidationContext) -> Vec<super::validation::CueIssue> {
        Vec::new()
    }

    /// Additional external dependencies to check after releasing the workspace.
    fn validation_files(&self) -> Vec<super::validation::ValidationFile> {
        Vec::new()
    }

    // -----------------------------------------------------------------------
    // Serialisation
    // -----------------------------------------------------------------------

    /// Serialise this cue to a JSON [`Value`] for `.inkue` file persistence.
    /// The returned object must include a `"type"` field matching
    /// [`CueType`]'s serialised form.
    fn serialize(&self) -> Value;
}
