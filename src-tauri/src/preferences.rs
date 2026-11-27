//! Application-wide preferences, persisted inside the `.inkue` file under
//! the `"preferences"` key.
//!
//! Each top-level category (audio, general, network, display) is its own
//! struct so future categories can be added without touching existing ones.

use serde::{Deserialize, Serialize};

use crate::cue::types::FadeCurve;

// ---------------------------------------------------------------------------
// Audio backend choice
// ---------------------------------------------------------------------------

/// Audio output backend.
///
/// `WasapiShared` / `WasapiExclusive` / `Asio` are Windows-specific.
/// `SystemDefault` is used on Mac / Linux where cpal picks CoreAudio or ALSA.
/// The `Default` impl is therefore per-OS, and every load goes through
/// [`AudioBackend::for_this_platform`] — see it for why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioBackend {
    #[cfg_attr(target_os = "windows", default)]
    WasapiShared,
    WasapiExclusive,
    Asio,
    #[cfg_attr(not(target_os = "windows"), default)]
    SystemDefault,
}

impl AudioBackend {
    /// Whether this backend can exist on the OS Inkue is running on.
    ///
    /// Driver *presence* is deliberately not considered: ASIO stays selectable
    /// on a Windows build with the feature compiled in even when no driver is
    /// installed, because `open_stream_inner` already falls back to the default
    /// host with a warning and a developer's saved config must survive.
    pub fn is_available_here(self) -> bool {
        #[cfg(target_os = "windows")]
        {
            #[cfg(feature = "asio-support")]
            {
                !matches!(self, AudioBackend::SystemDefault)
            }
            #[cfg(not(feature = "asio-support"))]
            {
                matches!(self, AudioBackend::WasapiShared | AudioBackend::WasapiExclusive)
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            matches!(self, AudioBackend::SystemDefault)
        }
    }

    /// This backend, or this platform's default when it cannot exist here.
    ///
    /// A machine config travels: copied between machines, restored from a
    /// backup, or simply written by a build whose default was Windows-only.
    /// Without this, Linux ran on `WasapiShared` — harmless (both resolve to
    /// `cpal::default_host()`) but it logged `backend=WasapiShared` on ALSA and
    /// left Preferences displaying a backend it could not offer.
    #[must_use]
    pub fn for_this_platform(self) -> Self {
        if self.is_available_here() {
            self
        } else {
            Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Audio preferences
// ---------------------------------------------------------------------------

/// Hardware-specific audio settings — device, backend, buffer size.
///
/// Stored in `%APPDATA%\Inkue\audio.json`, **not** in the workspace file,
/// because they describe the physical machine rather than the show.
/// Moving a `.inkue` file to another machine keeps its show defaults intact
/// while this config adapts to the local hardware.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineAudioConfig {
    /// WASAPI/ASIO backend to use.
    #[serde(default)]
    pub backend: AudioBackend,

    /// Identifier of the selected output device.  `None` = system default.
    #[serde(default)]
    pub device_id: Option<String>,

    /// Human-readable name of the selected output device, captured at selection
    /// time so a banner can show "Focusrite Scarlett…" instead of the raw
    /// WASAPI endpoint id even when the device is currently absent.  `None` =
    /// system default (or selected before this field existed).
    #[serde(default)]
    pub device_name: Option<String>,

    /// Identifier of the selected audio **input** device for Mic Cues / live
    /// capture.  `None` = system default input.  Machine-specific, like
    /// `device_id`.
    #[serde(default)]
    pub input_device_id: Option<String>,

    /// Dedicated monitor ("audition") output used by the cue-monitor preview
    /// and OSC `/auditionGo`.  `None` = not configured.  The system default is
    /// deliberately NOT used: the audition node must be a named output that is
    /// distinct from the show output and every output patch.
    #[serde(default)]
    pub audition_device_id: Option<String>,

    /// Human-readable name of the audition output, captured at selection time.
    #[serde(default)]
    pub audition_device_name: Option<String>,

    /// Output buffer size in samples.
    /// Only applied for WASAPI Exclusive; ignored in Shared mode (Windows
    /// controls the period) and ASIO mode (driver controls its own buffer).
    #[serde(default = "MachineAudioConfig::default_buffer_size")]
    pub buffer_size: u32,

    /// ASIO output pair index (0 = Out 1-2, 1 = Out 3-4, …).
    /// Ignored when backend is not ASIO.
    #[serde(default)]
    pub asio_out_pair: u32,
}

impl MachineAudioConfig {
    fn default_buffer_size() -> u32 { 256 }
}

impl Default for MachineAudioConfig {
    fn default() -> Self {
        Self {
            backend: AudioBackend::default(),
            device_id: None,
            device_name: None,
            input_device_id: None,
            audition_device_id: None,
            audition_device_name: None,
            buffer_size: Self::default_buffer_size(),
            asio_out_pair: 0,
        }
    }
}

/// Show-specific audio defaults — stored in the workspace file.
///
/// These travel with the `.inkue` file because they describe the show's
/// intent (how loud new cues are, what fade curve to use) rather than the
/// hardware it runs on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPreferences {
    /// Default volume (dB) applied to newly created cues.
    #[serde(default)]
    pub default_volume_db: f32,

    /// Duration (ms) of the soft fade-out applied on Stop.
    #[serde(default = "AudioPreferences::default_fade_out_ms")]
    pub default_fade_out_ms: u32,

    /// Default fade curve for newly created cues.
    #[serde(default = "AudioPreferences::default_fade_curve")]
    pub default_fade_curve: FadeCurve,

    /// Runtime-only: the machine's configured audio buffer size, injected at
    /// startup from `MachineAudioConfig` so `CueContext` can pass it to
    /// `ensure_input_feed`.  Never serialised into the workspace file.
    #[serde(skip)]
    pub audio_buffer_size: u32,
}

impl AudioPreferences {
    fn default_fade_out_ms() -> u32 { 500 }
    fn default_fade_curve() -> FadeCurve { FadeCurve::Linear }
}

impl Default for AudioPreferences {
    fn default() -> Self {
        Self {
            default_volume_db: 0.0,
            default_fade_out_ms: Self::default_fade_out_ms(),
            default_fade_curve: Self::default_fade_curve(),
            audio_buffer_size: 256,
        }
    }
}

// ---------------------------------------------------------------------------
// Reserved category structs (empty, ready for future content)
// ---------------------------------------------------------------------------

/// Row height for the Cue List table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CueRowHeight {
    Compact,
    #[default]
    Normal,
    Tall,
}

/// General app-behaviour preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralPreferences {
    #[serde(default)]
    pub timed_revisions: RevisionInterval,

    /// Minimum time (ms) between two GO triggers.  A second GO fired within
    /// this window is silently ignored to prevent accidental double-presses
    /// during live shows.  Set to 0 to disable.
    #[serde(default = "GeneralPreferences::default_double_go_protection_ms")]
    pub double_go_protection_ms: u32,

    /// Expose a separate local stop-and-blackout action for this show.
    #[serde(default)]
    pub show_hard_stop_blackout: bool,

    /// When true, deleting a cue via the keyboard shows a confirmation dialog.
    #[serde(default)]
    pub confirm_before_delete: bool,

    /// When true, the cue list automatically scrolls to keep the Playhead
    /// visible after each GO.
    #[serde(default = "GeneralPreferences::default_auto_scroll_to_playhead")]
    pub auto_scroll_to_playhead: bool,

    /// Height of each row in the cue list table.
    #[serde(default)]
    pub cue_row_height: CueRowHeight,

    /// When true, reordering/adding/removing cues rewrites every cue number to
    /// match its position (1, 2, 3…). When false (default, QLab-style), cue
    /// numbers are stable: reordering never touches them, so manually-set or
    /// imported numbers — and blank memo numbers — are preserved. Resequencing
    /// is then an explicit action (Action → Renumber All Cues).
    #[serde(default)]
    pub auto_renumber_on_reorder: bool,
}

impl GeneralPreferences {
    fn default_double_go_protection_ms() -> u32 { 500 }
    fn default_auto_scroll_to_playhead() -> bool { true }
}

impl Default for GeneralPreferences {
    fn default() -> Self {
        Self {
            timed_revisions: RevisionInterval::Off,
            double_go_protection_ms: Self::default_double_go_protection_ms(),
            show_hard_stop_blackout: false,
            confirm_before_delete: false,
            auto_scroll_to_playhead: Self::default_auto_scroll_to_playhead(),
            cue_row_height: CueRowHeight::default(),
            auto_renumber_on_reorder: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionInterval {
    #[default]
    Off,
    OneMinute,
    FiveMinutes,
    FifteenMinutes,
}

impl RevisionInterval {
    pub(crate) fn duration(self) -> Option<std::time::Duration> {
        use std::time::Duration;
        Some(Duration::from_secs(match self {
            Self::Off => return None,
            Self::OneMinute => 60,
            Self::FiveMinutes => 300,
            Self::FifteenMinutes => 900,
        }))
    }
}

/// OSC receive server configuration — stored machine-level in
/// `%APPDATA%\Inkue\osc.json`, not in the workspace file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OscReceiveConfig {
    /// When `false` the server is not started (or is stopped if already running).
    #[serde(default)]
    pub enabled: bool,
    /// UDP port to listen on.  Default: 53001.
    #[serde(default = "OscReceiveConfig::default_port")]
    pub port: u16,
    /// Plain text commands use a separate opt-in UDP listener.
    #[serde(default)]
    pub plain_text_enabled: bool,
    #[serde(default = "OscReceiveConfig::default_plain_text_port")]
    pub plain_text_port: u16,
    /// An empty allowlist permits binary OSC and blocks all plain text senders.
    #[serde(default)]
    pub allowed_ips: Vec<String>,

    /// When `true`, Inkue broadcasts the running cue's name and number to
    /// `feedback_host:feedback_port` whenever the active cue changes.
    #[serde(default)]
    pub feedback_enabled: bool,
    /// Destination hostname or IP for OSC feedback (e.g. `"127.0.0.1"`).
    #[serde(default = "OscReceiveConfig::default_feedback_host")]
    pub feedback_host: String,
    /// Destination UDP port for OSC feedback.  Default: 53000.
    #[serde(default = "OscReceiveConfig::default_feedback_port")]
    pub feedback_port: u16,
    /// Send rate (Hz) for the per-slot media progress messages
    /// (`/inkue/cue/{i}/progress|elapsed|remaining|duration`).
    /// `0` disables progress feedback (cue number/name still sent on change).
    #[serde(default = "OscReceiveConfig::default_feedback_progress_hz")]
    pub feedback_progress_hz: u8,
}

impl OscReceiveConfig {
    fn default_port()                 -> u16    { 53001 }
    fn default_plain_text_port()      -> u16    { 53535 }
    fn default_feedback_host()        -> String { "127.0.0.1".into() }
    fn default_feedback_port()        -> u16    { 53000 }
    fn default_feedback_progress_hz() -> u8     { 10 }
}

impl Default for OscReceiveConfig {
    fn default() -> Self {
        Self {
            enabled:               false,
            port:                  Self::default_port(),
            plain_text_enabled:    false,
            plain_text_port:       Self::default_plain_text_port(),
            allowed_ips:           Vec::new(),
            feedback_enabled:      false,
            feedback_host:         Self::default_feedback_host(),
            feedback_port:         Self::default_feedback_port(),
            feedback_progress_hz:  Self::default_feedback_progress_hz(),
        }
    }
}

/// Network preferences (OSC, MIDI, Art-Net, …).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkPreferences {}

/// MIDI preferences saved with the workspace (QLab's per-workspace MIDI
/// settings). Currently holds the MSC show-control broadcast configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiPreferences {
    /// QLab's MSC Broadcast tab checkbox: send MSC show-control broadcast
    /// messages on GO / Stop All / HardStop All / Panic All / Reset All.
    #[serde(default)]
    pub msc_broadcast_enabled: bool,
    /// MSC broadcast destinations (MIDI output port + command format +
    /// device ID), like QLab's per-workspace destination list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub msc_broadcast_destinations: Vec<crate::cue::msc::MscBroadcastDestination>,
}

/// Machine-level network interface selection — stored in `network.json`,
/// not in the workspace file, because it describes the physical machine.
///
/// `None` everywhere = Automatic: bind to all interfaces and let the OS
/// routing table pick the egress interface.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInterfaceConfig {
    /// OS name of the selected interface (e.g. `"Ethernet"`, `"en0"`, `"eth0"`).
    #[serde(default)]
    pub interface_name: Option<String>,
    /// IPv4 address of the interface captured at selection time.  Used as the
    /// bind address, and as a fallback match if the interface was renamed.
    #[serde(default)]
    pub interface_ip: Option<String>,
}

/// How a cue's colour tag is rendered in the Cue List.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CueColorStyle {
    /// A 4px tinted strip along the left edge of the row (original look).
    #[default]
    Stripe,
    /// The entire row background tinted with the cue's colour.
    FullRow,
}

/// Where on the output window the cue timer is anchored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TimerPosition {
    /// Centered horizontally and vertically — large display.
    #[default]
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Display preferences (output surface, colour theme, …).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayPreferences {
    /// Monitor index for the unified output surface.
    /// `None` = floating windowed (no fixed screen).
    /// `Some(n)` = fullscreen on monitor n (0 = primary).
    #[serde(default)]
    pub output_screen: Option<u32>,

    /// When `true`, a countdown timer is drawn on the output window showing
    /// the timing of the currently running audio cue.
    #[serde(default)]
    pub show_output_timer: bool,

    /// When `true` the timer counts down (time remaining).
    /// When `false` (default) it counts up (elapsed position in the file).
    #[serde(default)]
    pub timer_count_down: bool,

    /// Font family name for the output timer (e.g. `"Arial"`, `"Courier New"`).
    #[serde(default = "DisplayPreferences::default_timer_font")]
    pub timer_font: String,

    /// Font size for the output timer, in mpv OSD points.
    /// Default 120 is suitable for center; use 60–80 for corner positions.
    #[serde(default = "DisplayPreferences::default_timer_font_size")]
    pub timer_font_size: u32,

    /// Where on the output window the timer is drawn.
    #[serde(default)]
    pub timer_position: TimerPosition,

    /// When `true`, milliseconds are shown after the seconds (e.g. `00:00.000`).
    #[serde(default)]
    pub timer_show_ms: bool,

    /// Margin in pixels from the screen edge for corner positions.
    /// Ignored when `timer_position` is `Center`.
    #[serde(default = "DisplayPreferences::default_timer_margin")]
    pub timer_margin: u32,

    /// When `true` (and `show_output_timer` is also `true`), the timer is shown
    /// in a small always-on-top floating Win32 window instead of as an OSD
    /// overlay on the output surface.
    #[serde(default)]
    pub timer_floating: bool,

    /// UI colour theme: `"dark"`, `"light"`, `"navy"`, or `"system"`.
    #[serde(default = "DisplayPreferences::default_theme")]
    pub theme: String,

    /// How a cue's colour tag is rendered in the Cue List (stripe vs full row).
    #[serde(default)]
    pub cue_color_style: CueColorStyle,

    /// Projector-alignment transform of the main output (Preferences →
    /// Display), applied by the output engine's warp pass.
    #[serde(default)]
    pub output_transform: crate::engine::output_engine::OutputTransform,

    /// The video output that shows the on-output timer.  `None` = the main
    /// output; an extra output (a backstage return monitor) keeps the timer
    /// off the audience's screen.
    #[serde(default)]
    pub timer_output: Option<uuid::Uuid>,
}

impl DisplayPreferences {
    fn default_theme()           -> String { "system".into() }
    fn default_timer_font()      -> String { crate::bundled_fonts::FONT_FAMILY.into() }
    fn default_timer_font_size() -> u32   { 120 }
    fn default_timer_margin()    -> u32   { 50 }
}

impl Default for DisplayPreferences {
    fn default() -> Self {
        Self {
            output_screen:      None,
            show_output_timer:  false,
            timer_count_down:   false,
            timer_font:         Self::default_timer_font(),
            timer_font_size:    Self::default_timer_font_size(),
            timer_position:     TimerPosition::default(),
            timer_show_ms:      false,
            timer_margin:       Self::default_timer_margin(),
            timer_floating:     false,
            theme:              Self::default_theme(),
            cue_color_style:    CueColorStyle::default(),
            output_transform:   crate::engine::output_engine::OutputTransform::default(),
            timer_output:       None,
        }
    }
}

// ---------------------------------------------------------------------------
// Root preferences struct
// ---------------------------------------------------------------------------

/// All application preferences, serialised into the workspace file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppPreferences {
    #[serde(default)]
    pub audio: AudioPreferences,
    #[serde(default)]
    pub general: GeneralPreferences,
    #[serde(default)]
    pub network: NetworkPreferences,
    #[serde(default)]
    pub midi: MidiPreferences,
    #[serde(default)]
    pub display: DisplayPreferences,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msc_broadcast_preferences_default_off_and_round_trip() {
        let legacy: AppPreferences = serde_json::from_str("{}").unwrap();
        assert!(!legacy.midi.msc_broadcast_enabled);
        assert!(legacy.midi.msc_broadcast_destinations.is_empty());
        let authored = MidiPreferences {
            msc_broadcast_enabled: true,
            msc_broadcast_destinations: vec![crate::cue::msc::MscBroadcastDestination {
                port_name: "IAC Bus 1".into(),
                command_format: 48,
                device_id: 127,
            }],
        };
        let value = serde_json::to_value(&authored).unwrap();
        let reopened: MidiPreferences = serde_json::from_value(value).unwrap();
        assert_eq!(reopened, authored);
    }

    #[test]
    fn combined_transport_action_is_opt_in_and_saved_with_the_workspace_preferences() {
        let legacy: GeneralPreferences = serde_json::from_str("{}").unwrap();
        assert!(!legacy.show_hard_stop_blackout);
        let authored = GeneralPreferences { show_hard_stop_blackout: true, ..legacy };
        let reopened: GeneralPreferences = serde_json::from_value(serde_json::to_value(authored).unwrap()).unwrap();
        assert!(reopened.show_hard_stop_blackout);
    }

    #[test]
    fn the_platform_default_backend_is_one_this_os_has() {
        // Guards the per-OS `#[default]`: a Linux/macOS build defaulting to
        // WASAPI logged "backend=WasapiShared" on ALSA and left Preferences
        // showing a backend `get_available_backends` never offers.
        assert!(AudioBackend::default().is_available_here());
    }

    #[test]
    fn a_backend_from_another_os_is_coerced_on_load() {
        let foreign = if cfg!(target_os = "windows") {
            AudioBackend::SystemDefault
        } else {
            AudioBackend::WasapiShared
        };
        assert!(!foreign.is_available_here());
        assert_eq!(foreign.for_this_platform(), AudioBackend::default());
    }

    #[test]
    fn a_backend_this_os_supports_is_left_alone() {
        let native = AudioBackend::default();
        assert_eq!(native.for_this_platform(), native);
    }

    #[test]
    fn machine_config_json_roundtrips_through_the_platform_backend() {
        let json = serde_json::to_string(&AudioBackend::default()).unwrap();
        let back: AudioBackend = serde_json::from_str(&json).unwrap();
        assert_eq!(back.for_this_platform(), AudioBackend::default());
    }
}
