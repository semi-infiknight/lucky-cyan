//! Global application state shared across Tauri command handlers.
//!
//! All mutable state is wrapped in `Arc<Mutex<...>>` so it can be safely
//! accessed from multiple Tauri command handler threads.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use uuid::Uuid;

use super::LoadingCues;

use crate::{
    cue::{
        audio_cue::AudioCueFactory,
        decoded_audio::DecodedAudioMap,
        fade_cue::FadeCueFactory,
        group_cue::GroupCueFactory,
        light_cue::LightCueFactory,
        midi_cue::MidiCueFactory,
        image_cue::ImageCueFactory,
        memo_cue::MemoCueFactory,
        osc_cue::OscCueFactory,
        registry::CueRegistry,
        stop_cue::StopCueFactory,
        types::CueType,
        video_cue::VideoCueFactory,
        wait_cue::WaitCueFactory,
    },
    engine::{
        AudioEngine, DmxEngine, OscServer, OutputEngine,
        timecode_receiver::TimecodeReceiver,
    },
    show::{undo_stack::UndoHistory, Workspace},
};

/// A cue copied to the in-app clipboard.
pub struct ClipboardCue {
    pub revision: Uuid,
    /// Exact source assignment at Copy for an optional crosspoint paste.
    pub matrix_origin: Option<ClipboardMatrixOrigin>,
    /// Serialised cue (a Group carries its children).
    pub json: serde_json::Value,
    pub flagged_cues: BTreeSet<Uuid>,
    pub second_colors: BTreeMap<Uuid, crate::cue::types::CueColor>,
    /// Decoded audio of the cue and its descendants at copy time.
    pub audio: DecodedAudioMap,
}

#[derive(PartialEq, Eq)]
pub struct ClipboardMatrixOrigin {
    pub(crate) workspace_id: Uuid,
    pub(crate) patch: crate::cue::point_mix::AudioMapPatch,
    pub(crate) require_named_device: bool,
}

/// The Tauri managed state object.
pub struct AppState {
    pub(crate) media_presence: super::media_presence::MediaPresence,
    pub agent: Arc<Mutex<crate::commands::agent_cmds::AgentDesk>>,
    /// The current workspace (project file).
    pub workspace: Arc<Mutex<Workspace>>,
    /// The audio engine (shared; owns its own real-time thread internally).
    pub audio_engine: Arc<AudioEngine>,
    /// The unified output engine (video + image via libmpv Win32 window).
    pub output_engine: Arc<OutputEngine>,
    /// OSC receive server (background UDP listener thread).
    pub osc_server: Arc<OscServer>,
    /// DMX-over-IP lighting engine (owns its own ~40Hz output thread).
    pub dmx_engine: Arc<DmxEngine>,
    /// The cue type registry used for workspace de/serialisation.
    pub registry: Arc<Mutex<CueRegistry>>,
    /// Background media work tracked for cue loading indicators.
    pub loading_cues: Arc<Mutex<LoadingCues>>,
    /// Undo / redo history, one stack per cue list (see [`UndoHistory`]).
    pub undo_stack: Arc<Mutex<UndoHistory>>,
    /// In-app clipboard: the last cue copied via Ctrl+C, with the decoded audio it
    /// carried so a paste into another list is playable immediately.
    pub clipboard: Arc<Mutex<Option<ClipboardCue>>>,
    /// Timestamp of the last GO trigger in ms since Unix epoch.
    /// Used to enforce `double_go_protection_ms` — any GO within that window
    /// is silently dropped.  Lock-free so it adds zero latency to the hot path.
    pub last_go_at: Arc<AtomicU64>,
    /// Timecode receiver (MTC / LTC) — `None` until the first `set_tc_config`.
    pub tc_receiver: Arc<Mutex<Option<Arc<TimecodeReceiver>>>>,
    /// Session override and clock history shared with the transport tick.
    pub(crate) timecode_dispatch: Arc<Mutex<crate::show::event_loop::TimecodeDispatch>>,
    /// MIDI input listener driving per-cue MIDI triggers — `None` while
    /// triggers are disabled for this machine.
    pub midi_listener: Arc<Mutex<Option<Arc<crate::engine::midi_trigger::MidiTriggerListener>>>>,
    /// Session-only MIDI trigger override, shared by replacement listeners.
    pub(crate) midi_trigger_gate: Arc<crate::engine::midi_trigger::MidiTriggerGate>,
}

impl AppState {
    /// Build the initial application state from already-constructed engines.
    pub fn new(
        audio_engine: Arc<AudioEngine>,
        output_engine: Arc<OutputEngine>,
        osc_server: Arc<OscServer>,
        dmx_engine: Arc<DmxEngine>,
        tc_receiver: Option<Arc<TimecodeReceiver>>,
    ) -> Self {
        let workspace = Workspace::new("Untitled");

        let mut registry = CueRegistry::new();
        registry.register(CueType::Audio, Box::new(AudioCueFactory));
        registry.register(CueType::Fade,  Box::new(FadeCueFactory));
        registry.register(CueType::Midi,  Box::new(MidiCueFactory));
        registry.register(CueType::MidiFile, Box::new(crate::cue::midi_file_cue::MidiFileCueFactory));
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        registry.register(CueType::Light, Box::new(LightCueFactory));
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        registry.register(CueType::Osc,   Box::new(OscCueFactory));
        registry.register(CueType::Stop, Box::new(StopCueFactory));
        registry.register(CueType::Devamp, Box::new(crate::cue::devamp_cue::DevampCueFactory));
        registry.register(CueType::Video, Box::new(VideoCueFactory));
        registry.register(CueType::Image, Box::new(ImageCueFactory));
        registry.register(CueType::Mic,      Box::new(crate::cue::mic_cue::MicCueFactory));
        registry.register(CueType::Timecode, Box::new(crate::cue::timecode_cue::TimecodeCueFactory));
        registry.register(CueType::Text,     Box::new(crate::cue::text_cue::TextCueFactory));
        registry.register(CueType::Camera,   Box::new(crate::cue::camera_cue::CameraCueFactory));
        registry.register(CueType::Wait, Box::new(WaitCueFactory));
        registry.register(CueType::Script, Box::new(crate::cue::script_cue::ScriptCueFactory));
        // Command cues: eight distinct types over one shared implementation.
        for action in crate::cue::control_cue::ALL_CONTROL_ACTIONS {
            registry.register(
                action.cue_type(),
                Box::new(crate::cue::control_cue::ControlCueFactory(action)),
            );
        }

        Self {
            agent: Default::default(),
            workspace: Arc::new(Mutex::new(workspace)),
            audio_engine,
            output_engine,
            osc_server,
            dmx_engine,
            registry: Arc::new(Mutex::new(registry)),
            media_presence: Default::default(),
            loading_cues: Default::default(),
            undo_stack: Arc::new(Mutex::new(UndoHistory::new())),
            clipboard: Arc::new(Mutex::new(None)),
            last_go_at: Arc::new(AtomicU64::new(0)),
            tc_receiver: Arc::new(Mutex::new(tc_receiver)),
            timecode_dispatch: Arc::default(),
            midi_listener: Arc::new(Mutex::new(None)),
            midi_trigger_gate: Arc::default(),
        }
    }
}
