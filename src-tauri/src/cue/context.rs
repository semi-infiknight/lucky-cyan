//! [`CueContext`] is passed to cue lifecycle methods so they can interact with
//! the audio engine, the output engine, and emit show-level events without
//! direct coupling.

use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;

use crate::engine::{
    audio_input::InputPatch, device_manager::OutputPatch, fixture::{FixtureGroup, PatchedFixture},
    osc_patch::OscPatch, AudioEngineApi, DmxEngineApi, OutputEngineApi,
};

/// Events emitted by cues to the Show Engine during execution.
#[derive(Debug, Clone)]
pub enum CueEvent {
    /// The cue's pre-wait has elapsed; the action is about to start.
    ActionStarted { cue_id: uuid::Uuid },
    /// The cue's action has completed naturally (e.g., audio file finished).
    ActionCompleted { cue_id: uuid::Uuid },
    /// The cue has been stopped externally.
    Stopped { cue_id: uuid::Uuid },
    /// The cue's post-wait has elapsed; continue mode should be evaluated.
    PostWaitElapsed { cue_id: uuid::Uuid },
}

/// Shared context passed to every cue lifecycle call (`go`, `stop`, `pause`, …).
///
/// Provides access to the audio engine, the output engine, and a channel for
/// emitting show events.  The struct is cheap to clone (all fields are `Arc`
/// or `Sender`).
#[derive(Clone)]
pub struct CueContext {
    pub(crate) playlist_cut: Option<Arc<crate::engine::voice::PlaylistCut>>,
    pub(crate) audio_start_batch: Option<Arc<crate::engine::voice::AudioStartBatch>>,
    pub(crate) media_show: Option<Arc<crate::media_usage::Show>>,
    pub(crate) cue_starts: Option<Arc<Mutex<crate::show::cue_colors::CueStarts>>>,
    pub(crate) map_revisions: Arc<Vec<(uuid::Uuid, uuid::Uuid)>>,
    /// The audio engine, used by [`AudioCue`](crate::cue::audio_cue::AudioCue).
    /// A trait object so tests can inject a double (see `engine::engine_traits`).
    pub audio_engine: Arc<dyn AudioEngineApi>,
    /// The unified output engine, used by video and image cues.
    pub output_engine: Arc<dyn OutputEngineApi>,
    /// Channel for signalling events back to the Show Engine / transport layer.
    pub event_sender: Sender<CueEvent>,
    /// Duration (ms) of the soft fade-out applied on Stop when the cue has no
    /// explicit `fade_out` spec set.  Comes from `AudioPreferences::default_fade_out_ms`.
    pub stop_fade_ms: u32,
    /// Snapshot of the workspace's Output Patch table.  Cues look up their
    /// patch here to resolve device names and channel indices at GO time.
    pub output_patches: Arc<Vec<OutputPatch>>,
    /// The workspace's default Output Patch ID.
    pub default_patch_id: Option<uuid::Uuid>,
    /// Snapshot of the workspace's OSC Patch table.
    pub osc_patches: Arc<Vec<OscPatch>>,
    /// The DMX lighting engine, used by [`LightCue`](crate::cue::light_cue::LightCue).
    pub dmx_engine: Arc<dyn DmxEngineApi>,
    /// Snapshot of the workspace's fixture patch.  Light Cues resolve their
    /// targets' `(universe, channel, width)` here at GO time.
    pub fixtures: Arc<Vec<PatchedFixture>>,
    /// Snapshot of the workspace's fixture groups.  Light Cue group targets
    /// resolve to their member fixtures here at GO time.
    pub fixture_groups: Arc<Vec<FixtureGroup>>,
    /// Snapshot of the workspace's Input Patch table.  Mic Cues resolve their
    /// capture device + channels here at GO time.
    pub input_patches: Arc<Vec<InputPatch>>,
    /// Output buffer size from machine config — passed to `ensure_input_feed`
    /// so the input stream uses the same period as the output stream.
    pub audio_buffer_size: u32,
    /// The live workspace, when the context was built from one.  Lets OSC cues
    /// resolve QLab query tokens (`#/cue/...#`) in their messages at GO time.
    pub(crate) workspace_ref: Option<Arc<Mutex<crate::show::Workspace>>>,
}

impl CueContext {
    /// Create a new context.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        audio_engine: Arc<dyn AudioEngineApi>,
        output_engine: Arc<dyn OutputEngineApi>,
        event_sender: Sender<CueEvent>,
        stop_fade_ms: u32,
        output_patches: Vec<OutputPatch>,
        default_patch_id: Option<uuid::Uuid>,
        osc_patches: Vec<OscPatch>,
        dmx_engine: Arc<dyn DmxEngineApi>,
        fixtures: Vec<PatchedFixture>,
        fixture_groups: Vec<FixtureGroup>,
        input_patches: Vec<InputPatch>,
        audio_buffer_size: u32,
    ) -> Self {
        Self {
            audio_start_batch: None,
            playlist_cut: None,
            media_show: None,
            cue_starts: None,
            map_revisions: Arc::new(Vec::new()),
            audio_engine,
            output_engine,
            event_sender,
            stop_fade_ms,
            output_patches: Arc::new(output_patches),
            default_patch_id,
            osc_patches: Arc::new(osc_patches),
            dmx_engine,
            fixtures: Arc::new(fixtures),
            fixture_groups: Arc::new(fixture_groups),
            input_patches: Arc::new(input_patches),
            audio_buffer_size,
            workspace_ref: None,
        }
    }

    /// Give an immediate simultaneous group one callback boundary for its
    /// accepted Audio cues, including nested immediate simultaneous groups.
    pub(crate) fn start_simultaneous_audio(&self, start: impl FnOnce(&Self)) {
        if self.audio_start_batch.is_some() {
            start(self);
            return;
        }
        let batch = Arc::new(crate::engine::voice::AudioStartBatch::new());
        let mut context = self.clone();
        context.audio_start_batch = Some(Arc::clone(&batch));
        start(&context);
        batch.release();
    }

    /// Resolve an Output Patch by ID, falling back to the workspace default,
    /// then to `None` if neither is available.
    pub fn resolve_patch(&self, patch_id: Option<uuid::Uuid>) -> Option<&OutputPatch> {
        let id = patch_id.or(self.default_patch_id)?;
        self.output_patches.iter().find(|p| p.id == id)
    }

    pub fn patch_fader(&self, patch: &OutputPatch) -> Arc<crate::engine::patch_fader::PatchFader> {
        self.audio_engine.patch_faders().resolve(patch.id, super::types::db_to_linear(patch.gain_db as f64) as f32)
    }

    /// Resolve an OSC Patch by ID.  Returns `None` if the patch is not in the
    /// workspace's OSC patch table.
    pub fn resolve_osc_patch(&self, patch_id: uuid::Uuid) -> Option<&OscPatch> {
        self.osc_patches.iter().find(|p| p.id == patch_id)
    }

    /// Resolve a patched fixture by ID.  Returns `None` if the fixture is not
    /// in the workspace's patch.
    pub fn resolve_fixture(&self, fixture_id: uuid::Uuid) -> Option<&PatchedFixture> {
        self.fixtures.iter().find(|f| f.id == fixture_id)
    }

    /// Resolve a fixture group by ID.  Returns `None` if the group is not in the
    /// workspace.
    pub fn resolve_group(&self, group_id: uuid::Uuid) -> Option<&FixtureGroup> {
        self.fixture_groups.iter().find(|g| g.id == group_id)
    }

    /// Resolve an Input Patch by ID.  Returns `None` if the patch is not in the
    /// workspace's Input Patch table.
    pub fn resolve_input_patch(&self, patch_id: Option<uuid::Uuid>) -> Option<&InputPatch> {
        let id = patch_id?;
        self.input_patches.iter().find(|p| p.id == id)
    }

    /// Attach workspace-scoped media reporting and calibration versions.
    pub fn set_workspace(&mut self, workspace: &crate::show::Workspace) {
        self.media_show = crate::media_usage::show(workspace);
        self.cue_starts = Some(Arc::clone(&workspace.cue_starts));
        self.map_revisions = Arc::new(workspace.audio_maps.iter().map(|map| (map.id, map.revision)).collect());
    }

    /// Record admission once, separately from transport state and legacy events.
    pub(crate) fn mark_action_started(&self, cue_id: uuid::Uuid) -> bool {
        if let Some(mut starts) = self.cue_starts.as_ref().and_then(|starts| starts.lock().ok()) {
            starts.mark(cue_id);
            true
        } else {
            false
        }
    }

    pub(crate) fn announce_action_started(&self, cue_id: uuid::Uuid) -> bool {
        let recorded = self.mark_action_started(cue_id);
        self.emit(CueEvent::ActionStarted { cue_id });
        recorded
    }

    /// Queue a fresh cue summary for a change the state event cannot describe.
    pub(crate) fn request_cue_refresh(&self) {
        if let Some(mut starts) = self.cue_starts.as_ref().and_then(|starts| starts.lock().ok()) {
            starts.request_refresh();
        }
    }

    pub(crate) fn emit_for_action(&self, event: CueEvent) {
        if let CueEvent::ActionStarted { cue_id } = &event {
            self.mark_action_started(*cue_id);
        }
        self.emit(event);
    }

    pub(crate) fn reset_action_colors(&self, cue: &dyn super::traits::Cue) {
        if let Some(mut starts) = self.cue_starts.as_ref().and_then(|starts| starts.lock().ok()) {
            starts.clear_tree(cue);
        }
    }

    /// Convenience: emit an event through the context without unwrapping.
    /// Silently drops the event if the receiver has been dropped.
    pub fn emit(&self, event: CueEvent) {
        let _ = self.event_sender.send(event);
    }
}
