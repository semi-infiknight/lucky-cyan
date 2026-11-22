//! Engine interface traits held by [`CueContext`](crate::cue::context::CueContext).
//!
//! The cue and transport layers drive playback through these traits rather than
//! the concrete engines. In production the real [`AudioEngine`], [`OutputEngine`]
//! and [`DmxEngine`] implement them (thin forwarding to the existing inherent
//! methods, so behaviour is unchanged). In tests, lightweight doubles implement
//! them, which is what lets `CueContext` — and therefore `Transport::go` — be
//! constructed and exercised without an audio device, a GL window, or libmpv.
//!
//! The trait surface is exactly the set of methods reached via
//! `context.{audio,output,dmx}_engine.*` from the cue and transport layers; the
//! event loop keeps its own concrete engine handles and is unaffected.

use std::time::Duration;

use anyhow::Result;
use uuid::Uuid;

use super::audio_engine::{AudioEngine, LiveVoiceRequest};
use super::dmx_engine::{DmxChannelLevel, DmxEngine, PatchedChannel};
use super::output_engine::{
    ContentRequest, CrossfadePhase, CrossfadeRequest, OutputEngine, OutputId, SilentSpeedOrigin,
};
use super::ring_command::{FadeCurve, VoiceId};
use super::voice::Voice;

// ---------------------------------------------------------------------------
// Audio engine
// ---------------------------------------------------------------------------

/// The audio-engine operations the cue and transport layers depend on.
pub trait AudioEngineApi: Send + Sync {
    fn patch_faders(&self) -> &super::patch_fader::PatchFaders;
    fn check_named_output_ready(&self, route: &super::device_manager::NamedOutput) -> Result<()>;
    fn play_voice_routed(&self, voice: Voice, device_id: Option<&str>) -> Result<VoiceId>;
    fn play_voice_paused_routed(&self, voice: Voice, device_id: Option<&str>) -> Result<VoiceId>;
    /// Start a prepared File Audio voice staged on the main output.
    /// Returns false if that exact voice is no longer available.
    fn start_preloaded_file_voice(&self, _voice_id: VoiceId) -> Result<bool> { Ok(false) }
    fn stop_voice(&self, voice_id: VoiceId, fade_ms: u32, fade_curve: FadeCurve) -> Result<()>;
    fn pause_voice(&self, voice_id: VoiceId) -> Result<()>;
    fn resume_voice(&self, voice_id: VoiceId) -> Result<()>;
    fn seek_voice(&self, voice_id: VoiceId, frame_pos: u64) -> Result<()>;
    /// Release the voice's current slice loop (Devamp Cue).
    fn devamp_voice(&self, voice_id: VoiceId, stop_at_end: bool) -> Result<()>;
    /// Finish the current pass of a running, unsliced File Audio voice.
    fn release_whole_audio_loop(&self, _voice_id: VoiceId) -> bool { false }
    fn set_voice_gain(&self, voice_id: VoiceId, gain: f32) -> Result<()>;
    fn get_voice_gain(&self, voice_id: VoiceId) -> f32;
    /// Manual post-fader compensation on file Audio, separate from cue gain.
    fn set_voice_trim(&self, _voice: VoiceId, _gain: f32) {}
    fn set_voice_output_trims(&self, _voice: VoiceId, _route: &super::device_manager::NamedOutput, _gains: &[f32]) -> bool { false }
    /// Return the exact physical route captured when a strict File Audio voice was admitted.
    fn named_voice_output(&self, _voice: VoiceId) -> Option<super::device_manager::NamedOutput> { None }
    fn set_voice_pan(&self, voice_id: VoiceId, pan: f32) -> Result<()>;
    /// Live crosspoint routing; `None` returns the voice to pan routing.
    fn set_voice_level_matrix(
        &self,
        voice_id: VoiceId,
        matrix: Option<&super::voice::LevelMatrix>,
    ) -> Result<()>;
    fn claim_map_motion(&self, _voice: VoiceId, _device: &str, _owner: Uuid, _start: &super::voice::LevelMatrix) -> bool { false }
    fn update_map_motion(&self, _voice: VoiceId, _device: &str, _owner: Uuid, _matrix: super::voice::LevelMatrix) -> bool { false }
    fn get_voice_pan(&self, voice_id: VoiceId) -> f32;
    /// Obtain live rate control for a decoded PCM voice.
    fn voice_rate_fader(&self, _voice_id: VoiceId) -> Option<std::sync::Arc<super::rate_fader::RateFader>> {
        None
    }
    fn voice_cutoff_fader(&self, _voice_id: VoiceId, _filter_id: uuid::Uuid) -> Option<std::sync::Arc<super::rate_fader::ScalarFader>> {
        None
    }
    fn voice_eq_gain_fader(&self, _voice_id: VoiceId, _filter_id: uuid::Uuid) -> Option<std::sync::Arc<super::rate_fader::ScalarFader>> {
        None
    }
    /// Edit a file Audio voice's automatic end fade.
    fn set_voice_natural_fade_out(&self, _voice: VoiceId, _duration_ms: u32, _curve: FadeCurve) {}
    fn sample_rate(&self) -> u32;
    fn ensure_input_feed(&self, device_id: Option<&str>, buffer_size: u32) -> Result<Uuid>;
    fn register_synthetic_feed(
        &self,
        channels: usize,
        sample_rate: u32,
    ) -> Result<(Uuid, ringbuf::HeapProd<f32>)>;
    fn play_mic_voice(&self, request: LiveVoiceRequest) -> Result<VoiceId>;
    fn panic_stop_all(&self) -> Result<()>;
}

impl AudioEngineApi for AudioEngine {
    fn patch_faders(&self) -> &super::patch_fader::PatchFaders { &self.patch_faders }
    fn set_voice_trim(&self, voice: VoiceId, gain: f32) {
        AudioEngine::set_voice_trim(self, voice, gain);
    }
    fn set_voice_output_trims(&self, voice: VoiceId, route: &super::device_manager::NamedOutput, gains: &[f32]) -> bool {
        AudioEngine::set_voice_output_trims(self, voice, route, gains)
    }
    fn named_voice_output(&self, voice: VoiceId) -> Option<super::device_manager::NamedOutput> {
        AudioEngine::named_voice_output(self, voice)
    }
    fn claim_map_motion(&self, voice: VoiceId, device: &str, owner: Uuid, start: &super::voice::LevelMatrix) -> bool {
        AudioEngine::claim_map_motion(self, voice, device, owner, start)
    }
    fn update_map_motion(&self, voice: VoiceId, device: &str, owner: Uuid, matrix: super::voice::LevelMatrix) -> bool {
        AudioEngine::update_map_motion(self, voice, device, owner, matrix)
    }
    fn play_voice_routed(&self, voice: Voice, device_id: Option<&str>) -> Result<VoiceId> {
        AudioEngine::play_voice_routed(self, voice, device_id)
    }
    fn check_named_output_ready(&self, route: &super::device_manager::NamedOutput) -> Result<()> {
        AudioEngine::check_named_output_ready(self, route)
    }
    fn play_voice_paused_routed(&self, voice: Voice, device_id: Option<&str>) -> Result<VoiceId> {
        AudioEngine::play_voice_paused_routed(self, voice, device_id)
    }
    fn start_preloaded_file_voice(&self, voice_id: VoiceId) -> Result<bool> {
        AudioEngine::start_preloaded_file_voice(self, voice_id)
    }
    fn stop_voice(&self, voice_id: VoiceId, fade_ms: u32, fade_curve: FadeCurve) -> Result<()> {
        AudioEngine::stop_voice(self, voice_id, fade_ms, fade_curve)
    }
    fn pause_voice(&self, voice_id: VoiceId) -> Result<()> {
        AudioEngine::pause_voice(self, voice_id)
    }
    fn resume_voice(&self, voice_id: VoiceId) -> Result<()> {
        AudioEngine::resume_voice(self, voice_id)
    }
    fn seek_voice(&self, voice_id: VoiceId, frame_pos: u64) -> Result<()> {
        AudioEngine::seek_voice(self, voice_id, frame_pos)
    }
    fn devamp_voice(&self, voice_id: VoiceId, stop_at_end: bool) -> Result<()> {
        AudioEngine::devamp_voice(self, voice_id, stop_at_end)
    }
    fn release_whole_audio_loop(&self, voice_id: VoiceId) -> bool {
        AudioEngine::release_whole_audio_loop(self, voice_id)
    }
    fn set_voice_gain(&self, voice_id: VoiceId, gain: f32) -> Result<()> {
        AudioEngine::set_voice_gain(self, voice_id, gain)
    }
    fn get_voice_gain(&self, voice_id: VoiceId) -> f32 {
        AudioEngine::get_voice_gain(self, voice_id)
    }
    fn set_voice_pan(&self, voice_id: VoiceId, pan: f32) -> Result<()> {
        AudioEngine::set_voice_pan(self, voice_id, pan)
    }
    fn set_voice_level_matrix(
        &self,
        voice_id: VoiceId,
        matrix: Option<&super::voice::LevelMatrix>,
    ) -> Result<()> {
        AudioEngine::set_voice_level_matrix(self, voice_id, matrix)
    }
    fn get_voice_pan(&self, voice_id: VoiceId) -> f32 {
        AudioEngine::get_voice_pan(self, voice_id)
    }
    fn voice_rate_fader(&self, voice_id: VoiceId) -> Option<std::sync::Arc<super::rate_fader::RateFader>> {
        AudioEngine::voice_rate_fader(self, voice_id)
    }
    fn voice_cutoff_fader(&self, voice_id: VoiceId, filter_id: uuid::Uuid) -> Option<std::sync::Arc<super::rate_fader::ScalarFader>> {
        AudioEngine::voice_cutoff_fader(self, voice_id, filter_id)
    }
    fn voice_eq_gain_fader(&self, voice_id: VoiceId, filter_id: uuid::Uuid) -> Option<std::sync::Arc<super::rate_fader::ScalarFader>> {
        AudioEngine::voice_eq_gain_fader(self, voice_id, filter_id)
    }
    fn set_voice_natural_fade_out(&self, voice: VoiceId, duration_ms: u32, curve: FadeCurve) {
        AudioEngine::set_voice_natural_fade_out(self, voice, duration_ms, curve);
    }
    fn sample_rate(&self) -> u32 {
        AudioEngine::sample_rate(self)
    }
    fn ensure_input_feed(&self, device_id: Option<&str>, buffer_size: u32) -> Result<Uuid> {
        AudioEngine::ensure_input_feed(self, device_id, buffer_size)
    }
    fn register_synthetic_feed(
        &self,
        channels: usize,
        sample_rate: u32,
    ) -> Result<(Uuid, ringbuf::HeapProd<f32>)> {
        AudioEngine::register_synthetic_feed(self, channels, sample_rate)
    }
    fn play_mic_voice(&self, request: LiveVoiceRequest) -> Result<VoiceId> {
        AudioEngine::play_mic_voice(self, request)
    }
    fn panic_stop_all(&self) -> Result<()> {
        AudioEngine::panic_stop_all(self)
    }
}

// ---------------------------------------------------------------------------
// Output engine
// ---------------------------------------------------------------------------

/// The output-engine operations the cue and transport layers depend on.
pub trait OutputEngineApi: Send + Sync {
    fn show_content(&self, req: ContentRequest<'_>) -> Result<VoiceId>;
    fn claim_silent_speed(&self, _voice: VoiceId, _owner: Uuid) -> Option<SilentSpeedOrigin> { None }
    fn silent_speed_origin(&self, _voice: VoiceId, _owner: Uuid) -> Option<SilentSpeedOrigin> { None }
    fn set_silent_speed(&self, _voice: VoiceId, _owner: Uuid, _rate: f64) -> bool { false }
    fn hold_silent_speed(&self, _voice: VoiceId, _owner: Uuid) {}
    fn stop_content(&self, voice_id: VoiceId, visual_fade_ms: u32, audio_fade_ms: u32);
    fn hard_stop_current(&self);
    fn panic_stop(&self);
    fn apply_geometry(&self, voice_id: VoiceId, geometry: &super::output_engine::VideoGeometry);
    fn begin_voice_geometry_fade(&self, _voice_id: VoiceId, _owner: Uuid) -> Option<super::output_engine::VideoGeometry> { None }
    fn begin_voice_rotation_fade(&self, _voice_id: VoiceId, _owner: Uuid) -> Option<super::output_engine::VideoGeometry> { None }
    fn set_voice_geometry_fade(&self, _voice_id: VoiceId, _owner: Uuid, _update: super::output_engine::VideoGeometryUpdate) -> bool { false }
    fn begin_voice_color_fade(&self, _voice_id: VoiceId, _owner: Uuid, _fields: [bool; 4]) -> [Option<f32>; 4] { [None; 4] }
    fn set_voice_color_fade(&self, _voice_id: VoiceId, _owner: Uuid, _values: [Option<f32>; 4]) -> [bool; 4] { [false; 4] }
    fn set_layer_props(&self, voice_id: VoiceId, style: &super::output_engine::LayerStyle, edit_color: [bool; 4]);
    /// The AudioEngine voice carrying a video voice's audio track, if any.
    fn video_audio_voice(&self, voice_id: VoiceId) -> Option<VoiceId>;
    /// Re-anchor a video's paired audio voice to its actual picture position.
    fn resync_audio_to_video(&self, voice_id: VoiceId);
    /// Source position of a video that has cleared its positioned reveal.
    fn positioned_video_location_ms(&self, _voice_id: VoiceId) -> Option<u64> { None }
    /// Current animated opacity (0.0–1.0) of a voice's layer.
    fn get_voice_opacity(&self, voice_id: VoiceId) -> f32;
    /// Directly drive a voice's layer opacity (Fade Cue tick, ~30 fps).
    fn set_voice_opacity(&self, voice_id: VoiceId, opacity: f32);
    fn stop_voice(&self, voice_id: VoiceId, fade_ms: u32) -> Result<()>;
    fn pause_voice(&self, voice_id: VoiceId) -> Result<()>;
    fn resume_voice(&self, voice_id: VoiceId) -> Result<()>;
    fn seek_voice_ms(&self, voice_id: VoiceId, position_ms: u64);
    /// Show ASS text on a video output (`None` = the main output).
    fn show_text_overlay(&self, ass_text: &str, output: Option<OutputId>);
    fn clear_text_overlay(&self, output: Option<OutputId>);
    /// Start the visual fade that lands exactly on the content's natural end.
    /// Returns `false` when `voice_id` is no longer on the output window.
    fn begin_eof_fade_out(&self, voice_id: VoiceId, fade_ms: u32) -> bool;
    /// Release the visual voice's current slice loop (Devamp Cue).
    fn devamp_voice(&self, voice_id: VoiceId, stop_at_end: bool);
    /// Arm one exact, verified silent movie voice to finish at natural EOF.
    fn release_whole_video_loop(&self, _voice_id: VoiceId) -> bool { false }
    /// Reveal + unpause content that was preloaded by a Load Cue.
    /// `false` = this voice was not preloaded.
    fn start_preloaded(&self, voice_id: VoiceId) -> Result<bool>;
    fn soundtrack_revealed(&self, _voice_id: VoiceId) -> bool { false }
    /// Start a crossfade: the request's incoming content dissolves in while its
    /// outgoing layers leave, from the incoming content's first frame.
    /// `false` = the incoming content is not on an output.
    fn link_crossfade(&self, request: &CrossfadeRequest) -> bool;
    /// Where the crossfade into `incoming` stands.
    fn crossfade_phase(&self, incoming: VoiceId) -> CrossfadePhase;
    /// Pause (`true`) or resume the crossfade into `incoming`.
    fn hold_crossfade(&self, incoming: VoiceId, paused: bool);
    /// The driving Fade Cue stopped: cancel a crossfade that has not started,
    /// freeze one in progress.
    fn release_crossfade(&self, incoming: VoiceId);
}

impl OutputEngineApi for OutputEngine {
    fn claim_silent_speed(&self, voice: VoiceId, owner: Uuid) -> Option<SilentSpeedOrigin> {
        OutputEngine::claim_silent_speed(self, voice, owner)
    }
    fn silent_speed_origin(&self, voice: VoiceId, owner: Uuid) -> Option<SilentSpeedOrigin> {
        OutputEngine::silent_speed_origin(self, voice, owner)
    }
    fn set_silent_speed(&self, voice: VoiceId, owner: Uuid, rate: f64) -> bool {
        OutputEngine::set_silent_speed(self, voice, owner, rate)
    }
    fn hold_silent_speed(&self, voice: VoiceId, owner: Uuid) {
        OutputEngine::hold_silent_speed(self, voice, owner);
    }
    fn show_content(&self, req: ContentRequest<'_>) -> Result<VoiceId> {
        OutputEngine::show_content(self, req)
    }
    fn stop_content(&self, voice_id: VoiceId, visual_fade_ms: u32, audio_fade_ms: u32) {
        OutputEngine::stop_content(self, voice_id, visual_fade_ms, audio_fade_ms)
    }
    fn hard_stop_current(&self) {
        OutputEngine::hard_stop_current(self)
    }
    fn panic_stop(&self) {
        OutputEngine::panic_stop(self)
    }
    fn apply_geometry(&self, voice_id: VoiceId, geometry: &super::output_engine::VideoGeometry) {
        OutputEngine::apply_geometry(self, voice_id, geometry);
    }
    fn begin_voice_geometry_fade(&self, voice_id: VoiceId, owner: Uuid) -> Option<super::output_engine::VideoGeometry> {
        OutputEngine::begin_voice_geometry_fade(self, voice_id, owner)
    }
    fn begin_voice_rotation_fade(&self, voice_id: VoiceId, owner: Uuid) -> Option<super::output_engine::VideoGeometry> {
        OutputEngine::begin_voice_rotation_fade(self, voice_id, owner)
    }
    fn set_voice_geometry_fade(&self, voice_id: VoiceId, owner: Uuid, update: super::output_engine::VideoGeometryUpdate) -> bool {
        OutputEngine::set_voice_geometry_fade(self, voice_id, owner, update)
    }
    fn begin_voice_color_fade(&self, voice_id: VoiceId, owner: Uuid, fields: [bool; 4]) -> [Option<f32>; 4] {
        OutputEngine::begin_voice_color_fade(self, voice_id, owner, fields)
    }
    fn set_voice_color_fade(&self, voice_id: VoiceId, owner: Uuid, values: [Option<f32>; 4]) -> [bool; 4] {
        OutputEngine::set_voice_color_fade(self, voice_id, owner, values)
    }
    fn set_layer_props(&self, voice_id: VoiceId, style: &super::output_engine::LayerStyle, edit_color: [bool; 4]) {
        OutputEngine::set_layer_props(self, voice_id, style, edit_color);
    }
    fn video_audio_voice(&self, voice_id: VoiceId) -> Option<VoiceId> {
        OutputEngine::video_audio_voice(self, voice_id)
    }
    fn resync_audio_to_video(&self, voice_id: VoiceId) {
        OutputEngine::resync_audio_to_video(self, voice_id)
    }
    fn positioned_video_location_ms(&self, voice_id: VoiceId) -> Option<u64> {
        OutputEngine::positioned_video_location_ms(self, voice_id)
    }
    fn get_voice_opacity(&self, voice_id: VoiceId) -> f32 {
        OutputEngine::get_voice_opacity(self, voice_id)
    }
    fn set_voice_opacity(&self, voice_id: VoiceId, opacity: f32) {
        OutputEngine::set_voice_opacity(self, voice_id, opacity)
    }
    fn stop_voice(&self, voice_id: VoiceId, fade_ms: u32) -> Result<()> {
        OutputEngine::stop_voice(self, voice_id, fade_ms)
    }
    fn pause_voice(&self, voice_id: VoiceId) -> Result<()> {
        OutputEngine::pause_voice(self, voice_id)
    }
    fn resume_voice(&self, voice_id: VoiceId) -> Result<()> {
        OutputEngine::resume_voice(self, voice_id)
    }
    fn seek_voice_ms(&self, voice_id: VoiceId, position_ms: u64) {
        OutputEngine::seek_voice_ms(self, voice_id, position_ms)
    }
    fn show_text_overlay(&self, ass_text: &str, output: Option<OutputId>) {
        OutputEngine::show_text_overlay(self, ass_text, output)
    }
    fn clear_text_overlay(&self, output: Option<OutputId>) {
        OutputEngine::clear_text_overlay(self, output)
    }
    fn begin_eof_fade_out(&self, voice_id: VoiceId, fade_ms: u32) -> bool {
        OutputEngine::begin_eof_fade_out(self, voice_id, fade_ms)
    }
    fn devamp_voice(&self, voice_id: VoiceId, stop_at_end: bool) {
        OutputEngine::devamp_voice(self, voice_id, stop_at_end)
    }
    fn release_whole_video_loop(&self, voice_id: VoiceId) -> bool {
        OutputEngine::release_whole_video_loop(self, voice_id)
    }
    fn start_preloaded(&self, voice_id: VoiceId) -> Result<bool> {
        OutputEngine::start_preloaded(self, voice_id)
    }
    fn soundtrack_revealed(&self, voice_id: VoiceId) -> bool {
        OutputEngine::soundtrack_revealed(self, voice_id)
    }
    fn link_crossfade(&self, request: &CrossfadeRequest) -> bool {
        OutputEngine::link_crossfade(self, request)
    }
    fn crossfade_phase(&self, incoming: VoiceId) -> CrossfadePhase {
        OutputEngine::crossfade_phase(self, incoming)
    }
    fn hold_crossfade(&self, incoming: VoiceId, paused: bool) {
        OutputEngine::hold_crossfade(self, incoming, paused)
    }
    fn release_crossfade(&self, incoming: VoiceId) {
        OutputEngine::release_crossfade(self, incoming)
    }
}

// ---------------------------------------------------------------------------
// DMX engine
// ---------------------------------------------------------------------------

/// The DMX-engine operations the cue layer depends on (Light Cue fades).
pub trait DmxEngineApi: Send + Sync {
    fn submit_fade(
        &self,
        address: PatchedChannel,
        target_norm: f64,
        dur: Duration,
        curve: FadeCurve,
    );

    fn submit_fade_batch(&self, channels: Vec<DmxChannelLevel>, dur: Duration, curve: FadeCurve) -> Result<()> {
        for (address, level) in channels {
            self.submit_fade(address, level, dur, curve);
        }
        Ok(())
    }
}

impl DmxEngineApi for DmxEngine {
    fn submit_fade(
        &self,
        address: PatchedChannel,
        target_norm: f64,
        dur: Duration,
        curve: FadeCurve,
    ) {
        self.submit_patched_fade(address, target_norm, dur, curve)
    }

    fn submit_fade_batch(&self, channels: Vec<DmxChannelLevel>, dur: Duration, curve: FadeCurve) -> Result<()> {
        DmxEngine::submit_fade_batch(self, channels, dur, curve).map_err(anyhow::Error::msg)
    }
}
