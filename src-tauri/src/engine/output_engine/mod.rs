//! [`OutputEngine`] — unified output for both video and image cues.
//!
//! The engine drives one or more **outputs** (see `output.rs`): each is a native
//! window hosting libmpv via the OpenGL Render API (`vo=libmpv`, see
//! `render.rs`), with its own layer stack, overlay and dip-to-black curtain.
//! The **main output** (`MAIN_OUTPUT`, configured by Preferences → Display) always
//! exists; the workspace may declare extra ones (a façade projector, a return
//! monitor…), opened in the background as soon as the show declares them and
//! destroyed when it drops them (`lifecycle.rs`).
//!
//! The render loop and fade are identical on every OS — only native window
//! creation differs (winit on Windows/Linux, AppKit/objc2 on macOS, see
//! `window.rs` / `macos_window.rs`).
//!
//! On every OS the floating cue timer is a Tauri WebView window (`float-timer`),
//! and the on-output timer is mpv's OSD (`osd-msg1`) on the output the show
//! picks for it (the main one by default).

mod blend;
pub(crate) mod confidence;
mod crossfade;
mod lifecycle;
mod mask_image;
mod mpv_events;
mod output;
mod overlay;
mod regions;
mod render;
mod screens;
mod slot;
mod source_placement;
mod still_jpeg;
mod warp;
mod window;
/// macOS-only: AppKit NSWindow creation + control for the GL output path.
#[cfg(target_os = "macos")]
mod macos_window;
mod types;

pub use blend::BlendMode;
pub use mask_image::MaskImage;
pub use regions::{OutputRegions, OutputSplitDirection};
pub(crate) use source_placement::supports_still_rotation_path;
pub use types::{
    ContentRequest, CrossfadePhase, CrossfadeRequest, FitMode, LayerStyle, OutputConfig, OutputId,
    OutputStatus, OutputTransform, PictureColorParameter, ScreenInfo, SilentSpeedOrigin, TestPattern, VideoGeometry,
    VideoGeometryUpdate, VideoTranslation, VoiceId, MAIN_OUTPUT,
};
use types::compose_display_props;

use std::collections::{HashMap, HashSet};
use std::ffi::{c_void, CString};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossbeam_channel::{Receiver, Sender};
use uuid::Uuid;

use crate::cue::types::{db_to_linear, FadeSpec};
use crate::engine::AudioEngine;

use output::{register, Output, OverlayMpv};
use screens::resolve as resolve_output_screen;
use super::mpv_sys::{MpvLib, MPV_FORMAT_INT64};

// ---------------------------------------------------------------------------
// Process-wide state
// ---------------------------------------------------------------------------

/// Where slot event threads report completion / duration / errors.  One channel
/// for every output: the show event loop does not care which window a voice
/// plays on.
pub(super) static OUTPUT_STATUS_TX: OnceLock<Sender<OutputStatus>> = OnceLock::new();
/// When `Some`, the timer refresh loop shows this text instead of live cue time.
pub(crate) static TIMER_PREVIEW: OnceLock<Mutex<Option<String>>> = OnceLock::new();
/// Maximum lifetime of a single operator saturation sample.
pub(crate) const PICTURE_SATURATION_PREVIEW_MS: u64 = 2_000;
/// Deduplication cache for the floating timer text (avoids redundant Tauri events).
pub(super) static FLOAT_TIMER_TEXT: OnceLock<Mutex<String>> = OnceLock::new();
/// Font family mirrored from OSD settings → emitted to the float-timer window.
pub(super) static FLOAT_TIMER_FONT: OnceLock<Mutex<String>> = OnceLock::new();

/// Initialise every global that does not depend on libmpv.
///
/// Shared by both constructors so a headless engine behaves exactly like a
/// live one minus the video output — the timer keeps working (and keeps
/// serialising) with no output window attached.
fn init_shared_globals(status_tx: &Sender<OutputStatus>) {
    OUTPUT_STATUS_TX.get_or_init(|| status_tx.clone());
    TIMER_PREVIEW.get_or_init(|| Mutex::new(None));
    FLOAT_TIMER_TEXT.get_or_init(|| Mutex::new(String::new()));
    // Empty sentinel (never a real font name) so the first set_timer_style()
    // call always emits float-timer-font, regardless of what the persisted
    // preference happens to be — the float-timer window's own React state
    // has no other way to learn the current font.
    FLOAT_TIMER_FONT.get_or_init(|| Mutex::new(String::new()));
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// What the show says about the outputs: the main output's screen + alignment
/// (from Preferences → Display) and the workspace's extra outputs.  Pushed to
/// the engine by the event loop every tick — a no-op unless it changed, which
/// covers open / new / crash-recovery without hooking every load path.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OutputsConfig {
    pub main_screen: Option<u32>,
    pub main_transform: OutputTransform,
    pub mask_images: HashMap<OutputId, MaskImage>,
    pub extras: Vec<OutputConfig>,
    /// The output that shows the on-output timer (`None` = the main one).
    pub timer_output: Option<OutputId>,
}

impl OutputsConfig {
    /// Whether the show declares the extra output `output`.
    fn has_extra(&self, output: OutputId) -> bool {
        self.extras.iter().any(|c| c.id == output)
    }

    /// The output the on-output timer belongs on.
    fn timer_output_id(&self) -> OutputId {
        self.timer_output.unwrap_or(MAIN_OUTPUT)
    }

    /// Screen assigned to `output` (`None` = floating).
    fn screen_of(&self, output: OutputId) -> Option<u32> {
        if output == MAIN_OUTPUT {
            self.main_screen
        } else {
            self.extras.iter().find(|c| c.id == output).and_then(|c| c.screen)
        }
    }

    /// Alignment transform of `output`.
    fn transform_of(&self, output: OutputId) -> OutputTransform {
        let mut transform = self.stored_transform_of(output);
        if let Some(peer) = transform.regions.seam_partner() {
            let screen = self.screen_of(output);
            let same_screen = screen.is_some() && screen == self.screen_of(peer);
            if same_screen || !transform.regions.matches_seam(output, &self.stored_transform_of(peer).regions) {
                transform.regions.clear_seam();
            }
        }
        transform
    }

    fn stored_transform_of(&self, output: OutputId) -> OutputTransform {
        if output == MAIN_OUTPUT {
            self.main_transform
        } else {
            self.extras
                .iter()
                .find(|c| c.id == output)
                .map(|c| c.transform)
                .unwrap_or_default()
        }
    }

    /// Operator-facing name of `output`.
    fn name_of(&self, output: OutputId) -> String {
        if output == MAIN_OUTPUT {
            "Main".to_owned()
        } else {
            self.extras
                .iter()
                .find(|c| c.id == output)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| "Output".to_owned())
        }
    }
}

/// Health-banner id for an output's "screen not connected" warning.
fn screen_alert_id(output: OutputId) -> String {
    if output == MAIN_OUTPUT {
        "output-screen".to_owned()
    } else {
        format!("output-screen-{output}")
    }
}

// ---------------------------------------------------------------------------
// OutputEngine
// ---------------------------------------------------------------------------

/// Error surfaced by every visual operation attempted in headless mode.
pub const NO_VIDEO_OUTPUT: &str =
    "Video output unavailable — libmpv is not loaded (see the startup log)";

/// Manages the output windows + libmpv contexts for all video and image output.
pub struct OutputEngine {
    /// `None` in **headless mode** — see [`OutputEngine::new_headless`].
    lib: Option<Arc<MpvLib>>,
    #[allow(dead_code)]
    status_tx: Sender<OutputStatus>,
    status_rx: Mutex<Receiver<OutputStatus>>,
    audio_engine: Arc<AudioEngine>,
    /// Tauri app handle — used to show/hide and emit events to the float-timer window.
    app_handle: tauri::AppHandle,
    /// Live outputs by id.  The main output is created at startup; extra ones
    /// as soon as the show declares them.
    outputs: Mutex<HashMap<OutputId, Arc<Output>>>,
    /// Extra outputs being opened in the background.
    creating: Mutex<HashSet<OutputId>>,
    config: Mutex<OutputsConfig>,
    /// The timer OSD style, applied to every output (whichever shows it).
    timer_style: Mutex<Option<TimerOsdStyle>>,
    /// The engine itself, for work it hands to its own threads.
    me: Weak<OutputEngine>,
}

/// Font, size, alignment and margin of the on-output timer.
#[derive(Debug, Clone)]
struct TimerOsdStyle {
    font: String,
    size: u32,
    align: (&'static str, &'static str),
    margin: String,
}

impl OutputEngine {
    /// Construct the engine.
    ///
    /// Creates the native GL window of the main output (hidden) and blocks
    /// until mpv's render context is live.
    pub fn new(audio_engine: Arc<AudioEngine>, app_handle: tauri::AppHandle) -> Result<Arc<Self>> {
        let lib = Arc::new(MpvLib::load()?);

        // mpv requires LC_NUMERIC=C; set it before mpv_create() on non-Windows.
        #[cfg(not(target_os = "windows"))]
        unsafe {
            libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr());
        }

        let (status_tx, status_rx) = crossbeam_channel::unbounded();
        init_shared_globals(&status_tx);
        screens::watch(&app_handle);

        let engine = Arc::new_cyclic(|me| Self {
            lib: Some(lib),
            status_tx,
            status_rx: Mutex::new(status_rx),
            audio_engine,
            app_handle,
            outputs: Mutex::new(HashMap::new()),
            creating: Mutex::new(HashSet::new()),
            config: Mutex::new(OutputsConfig::default()),
            timer_style: Mutex::new(None),
            me: me.clone(),
        });

        // On failure everything the attempt created is torn down, so the
        // caller can fall back to a headless engine without leaving an orphan
        // mpv instance behind.
        let main = engine.spawn_output(MAIN_OUTPUT, "Main")?;
        engine.adopt_output(main)?;
        Ok(engine)
    }

    /// Construct a **headless** engine: no libmpv, no output window.
    ///
    /// The fallback when [`Self::new`] fails — libmpv absent (the common case
    /// on a fresh Linux install: the AppImage does not bundle it), or no
    /// display server / GL context available.  The show still runs for audio,
    /// MIDI, OSC, timecode and lighting; every visual operation is a no-op or
    /// returns [`NO_VIDEO_OUTPUT`], and the operator sees a health banner
    /// instead of the process disappearing at startup.
    pub fn new_headless(audio_engine: Arc<AudioEngine>, app_handle: tauri::AppHandle) -> Arc<Self> {
        let (status_tx, status_rx) = crossbeam_channel::unbounded();
        init_shared_globals(&status_tx);
        screens::watch(&app_handle);

        Arc::new_cyclic(|me| Self {
            lib: None,
            status_tx,
            status_rx: Mutex::new(status_rx),
            audio_engine,
            app_handle,
            outputs: Mutex::new(HashMap::new()),
            creating: Mutex::new(HashSet::new()),
            config: Mutex::new(OutputsConfig::default()),
            timer_style: Mutex::new(None),
            me: me.clone(),
        })
    }

    /// `false` when the engine is headless: no video or image cue can play.
    pub fn is_available(&self) -> bool {
        self.lib.is_some()
    }

    /// The loaded `MpvLib` for probing, or `None` in headless mode.
    pub fn try_mpv_lib(&self) -> Option<&MpvLib> {
        self.lib.as_deref()
    }

    /// Owned handle to the loaded `MpvLib` for background work
    /// (e.g. thumbnail generation on a blocking task).
    pub fn try_mpv_lib_arc(&self) -> Option<Arc<MpvLib>> {
        self.lib.clone()
    }

    /// Probe the duration of a video file without displaying it.
    pub fn probe_duration(lib: &MpvLib, path: &Path) -> Option<Duration> {
        unsafe {
            let ctx = (lib.mpv_create)();
            if ctx.is_null() {
                return None;
            }

            opt_str(lib, ctx, "vo", "null");
            opt_str(lib, ctx, "ao", "null");
            opt_str(lib, ctx, "pause", "yes");
            opt_str(lib, ctx, "hwdec", "no");

            if (lib.mpv_initialize)(ctx) < 0 {
                (lib.mpv_terminate_destroy)(ctx);
                return None;
            }

            let path_str = path.to_string_lossy().replace('\\', "/");
            let path_cstr = match CString::new(path_str.as_str()) {
                Ok(c) => c,
                Err(_) => {
                    (lib.mpv_terminate_destroy)(ctx);
                    return None;
                }
            };
            let cmd_cstr     = cs("loadfile");
            let replace_cstr = cs("replace");
            let args: [*const std::ffi::c_char; 4] = [
                cmd_cstr.as_ptr(),
                path_cstr.as_ptr(),
                replace_cstr.as_ptr(),
                std::ptr::null(),
            ];
            (lib.mpv_command)(ctx, args.as_ptr());

            use super::mpv_sys::{MPV_EVENT_FILE_LOADED, MPV_EVENT_SHUTDOWN, MPV_FORMAT_DOUBLE};
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut duration_secs: Option<f64> = None;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let timeout = remaining.as_secs_f64().max(0.01);
                let event = (lib.mpv_wait_event)(ctx, timeout);
                if event.is_null() { break; }
                let event_id = (*event).event_id;
                if event_id == MPV_EVENT_FILE_LOADED {
                    let mut val: f64 = 0.0;
                    let name = cs("duration");
                    let ret = (lib.mpv_get_property)(
                        ctx, name.as_ptr(), MPV_FORMAT_DOUBLE,
                        &mut val as *mut f64 as *mut c_void,
                    );
                    if ret == 0 && val > 0.0 {
                        duration_secs = Some(val);
                    }
                    break;
                }
                if event_id == MPV_EVENT_SHUTDOWN { break; }
                if Instant::now() >= deadline { break; }
            }

            (lib.mpv_terminate_destroy)(ctx);
            duration_secs.map(|s| Duration::from_millis((s * 1000.0) as u64))
        }
    }

    /// Enumerate all connected monitors.  Index 0 is always the primary.
    pub fn list_screens(&self) -> Vec<ScreenInfo> {
        screens::list(&self.app_handle)
    }

    // ── Outputs ──────────────────────────────────────────────────────────────

    /// Create an output: native window, GL render thread, overlay mpv context.
    /// Not yet live — [`Self::adopt_output`] makes it so.
    fn spawn_output(&self, id: OutputId, name: &str) -> Result<Arc<Output>> {
        let lib = self.lib.as_ref().ok_or_else(|| anyhow!("{NO_VIDEO_OUTPUT}"))?;
        let output = Output::new(id, name);
        let ctx = overlay::create_overlay_context(lib)?;
        let _ = output.overlay.set(OverlayMpv::new(Arc::clone(lib), Arc::clone(&ctx)));

        // Create the window and block until mpv's render context is live, so no
        // `loadfile` can race ahead of it.
        if let Err(e) = render::init(&output, &self.app_handle, Arc::clone(lib), Arc::clone(&ctx)) {
            if let Some(overlay) = output.overlay.get() {
                overlay.destroy_on_drop.store(true, std::sync::atomic::Ordering::Release);
            }
            return Err(e);
        }

        let (lib2, ctx2, label) = (Arc::clone(lib), Arc::clone(&ctx), name.to_owned());
        let stop = Arc::clone(&output.events_stop);
        let events = std::thread::Builder::new()
            .name("inkue-output-mpv-events".into())
            .spawn(move || mpv_events::overlay_event_loop(lib2, ctx2, label, stop))
            .map_err(|e| anyhow!("Failed to spawn mpv event thread: {e}"))?;
        output.adopt_thread(events);

        output.window.set_title(&window::window_title(name));
        if let Some(style) = self.timer_style.lock().ok().and_then(|s| s.clone()) {
            output.set_timer_osd_style(&style.font, style.size, style.align, &style.margin);
        }
        log::info!("[output] '{name}' created");
        Ok(output)
    }

    /// Make a freshly built output live — unless another thread built the same
    /// one meanwhile (theirs stays) or the show dropped it in the meantime
    /// (this one is destroyed).
    fn adopt_output(&self, output: Arc<Output>) -> Result<Arc<Output>> {
        let config = self.config.lock().unwrap();
        let wanted = output.id == MAIN_OUTPUT || config.has_extra(output.id);
        let existing = {
            let mut map = self.outputs.lock().unwrap();
            match map.get(&output.id) {
                Some(existing) => Some(Arc::clone(existing)),
                None if wanted => {
                    // Publish the new window with the current alignment. An
                    // output created in the background can outlive a paired edit.
                    apply_transform(&output, &config.transform_of(output.id));
                    output.set_mask_image(config.mask_images.get(&output.id).cloned());
                    map.insert(output.id, Arc::clone(&output));
                    None
                }
                None => {
                    drop(map);
                    lifecycle::destroy_in_background(output);
                    return Err(anyhow!(
                        "This cue's video output no longer exists — pick another output in the cue's inspector"
                    ));
                }
            }
        };
        drop(config);
        match existing {
            Some(existing) => {
                lifecycle::destroy_in_background(output);
                Ok(existing)
            }
            None => {
                register(&output);
                Ok(output)
            }
        }
    }

    /// Build the extra output `id` (blocking: a window and a GL context).
    fn build_extra(&self, id: OutputId) -> Result<Arc<Output>> {
        let name = self
            .config
            .lock()
            .ok()
            .and_then(|c| c.extras.iter().find(|e| e.id == id).map(|e| e.name.clone()))
            .ok_or_else(|| {
                anyhow!("This cue's video output no longer exists — pick another output in the cue's inspector")
            })?;
        let output = self.spawn_output(id, &name)?;
        self.adopt_output(output)
    }

    /// Open the extra output `id` on a thread of its own, then light its
    /// screen the way a workspace load does.  One build per output at a time.
    fn create_in_background(&self, id: OutputId) {
        let Some(engine) = self.me.upgrade() else { return };
        if self.lib.is_none() || !self.creating.lock().unwrap().insert(id) {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("inkue-output-create".into())
            .spawn(move || {
                let built = engine.build_extra(id);
                engine.creating.lock().unwrap().remove(&id);
                match built {
                    Ok(output) => engine.apply_screen_on_load(&output, engine.configured_screen(id)),
                    Err(e) => log::warn!("[output] could not open output {id}: {e}"),
                }
            });
        if let Err(e) = spawned {
            self.creating.lock().unwrap().remove(&id);
            log::error!("[output] could not start the output thread: {e}");
        }
    }

    /// The output `id` (`None` = main), opened now if the show declares it but
    /// it is not open yet.
    ///
    /// On macOS a window can only be built on the main thread, and a GO fired
    /// from another one (the event loop) holds the workspace lock the main
    /// thread may be waiting for: there the output opens in the background and
    /// this GO reports it, instead of risking a deadlock.  Outputs normally
    /// open as soon as the show declares them, long before their first cue.
    fn output_for(&self, id: Option<OutputId>) -> Result<Arc<Output>> {
        let id = id.unwrap_or(MAIN_OUTPUT);
        if let Some(live) = self.live_output(id) {
            return Ok(live);
        }
        if id == MAIN_OUTPUT {
            return Err(anyhow!("{NO_VIDEO_OUTPUT}"));
        }
        ensure_output_exists(&self.config.lock().unwrap(), id)?;
        if !window::can_create_window_here() {
            self.create_in_background(id);
            return Err(anyhow!(
                "Output '{}' is still opening — GO again in a moment",
                self.output_name(Some(id)),
            ));
        }
        self.build_extra(id)
    }

    /// The live output `id`, if it is open.
    fn live_output(&self, id: OutputId) -> Option<Arc<Output>> {
        self.outputs.lock().unwrap().get(&id).cloned()
    }

    /// Ask an already visible projector window for one finished frame.
    pub(crate) fn request_stage_capture(&self, id: OutputId) -> Result<confidence::CaptureReceiver, String> {
        let output = self.live_output(id)
            .filter(|out| out.is_visible())
            .ok_or_else(|| "Show this video output before capturing a frame".to_owned())?;
        let receiver = output.captures.request(Instant::now())?;
        output.wake();
        Ok(receiver)
    }

    pub(crate) fn start_stage_preview(&self, id: OutputId, owner: confidence::PreviewOwner) -> Result<(), String> {
        let output = self.live_output(id).filter(|out| out.is_visible())
            .ok_or("Show this video output before starting preview")?;
        output.preview.start(owner, Instant::now())?;
        output.wake();
        Ok(())
    }

    pub(crate) fn stop_stage_preview(&self, id: OutputId, owner: confidence::PreviewOwner) {
        if let Some(output) = self.live_output(id) {
            output.preview.stop(owner);
        }
    }

    pub(crate) fn read_stage_preview(&self, id: OutputId, owner: confidence::PreviewOwner, after: u32)
        -> Result<Option<(u32, Arc<confidence::RawStageFrame>)>, String>
    {
        let output = self.live_output(id).filter(|out| out.is_visible())
            .ok_or("The video output is hidden or closed")?;
        output.preview.latest(owner, after, Instant::now())
    }

    /// Snapshot of every live output.
    fn live_outputs(&self) -> Vec<Arc<Output>> {
        self.outputs.lock().unwrap().values().cloned().collect()
    }

    /// Screen the show assigns to `output`.
    fn configured_screen(&self, output: OutputId) -> Option<u32> {
        self.config.lock().map(|c| c.screen_of(output)).unwrap_or(None)
    }

    /// Record what the show says about the outputs and apply it: outputs it
    /// dropped are destroyed, new ones open in the background, live ones follow
    /// their name, alignment and screen, and the timer moves with its output.
    /// Idempotent — cheap when nothing changed (the event loop calls it every
    /// tick, which covers open / new / crash recovery without hooking each).
    pub fn sync_outputs_config(&self, config: &OutputsConfig) {
        let previous = {
            let mut current = self.config.lock().unwrap();
            if *current == *config {
                return;
            }
            std::mem::replace(&mut *current, config.clone())
        };
        if self.lib.is_none() {
            return;
        }

        let dropped: Vec<Arc<Output>> = {
            let mut map = self.outputs.lock().unwrap();
            let gone: Vec<OutputId> = map
                .keys()
                .copied()
                .filter(|&id| id != MAIN_OUTPUT && !config.has_extra(id))
                .collect();
            gone.iter().filter_map(|id| map.remove(id)).collect()
        };
        for output in dropped {
            crate::health::clear(&screen_alert_id(output.id));
            log::info!("[output] '{}' left the show — closing it", output.name());
            lifecycle::destroy_in_background(output);
        }

        for output in self.live_outputs() {
            output.set_name(&config.name_of(output.id));
            apply_transform(&output, &config.transform_of(output.id));
            output.set_mask_image(config.mask_images.get(&output.id).cloned());
            let moved = output.id != MAIN_OUTPUT
                && previous.has_extra(output.id)
                && previous.screen_of(output.id) != config.screen_of(output.id);
            if moved {
                self.apply_screen_on_load(&output, config.screen_of(output.id));
            }
        }

        if previous.timer_output_id() != config.timer_output_id() {
            if let Some(old) = self.live_output(previous.timer_output_id()) {
                old.set_timer_text("");
            }
        }

        for extra in &config.extras {
            if self.live_output(extra.id).is_none() {
                self.create_in_background(extra.id);
            }
        }
    }

    // ── Unified content display ──────────────────────────────────────────────

    /// Display content (video, image or live feed) on an output.
    ///
    /// The content gets its own video slot and is **composited** with
    /// whatever else is on that output (layer / opacity / blend from
    /// `req.layer_style`) — nothing is replaced; stopping other cues is the
    /// transport's policy, not the engine's.
    pub fn show_content(&self, req: ContentRequest<'_>) -> Result<VoiceId> {
        let Some(lib) = self.lib.as_ref() else {
            return Err(anyhow!("{NO_VIDEO_OUTPUT}"));
        };
        anyhow::ensure!(!req.require_soundtrack || req.audio_voice_id.is_some(),
            "A required soundtrack must be prepared before the Video cue can start.");
        anyhow::ensure!(!req.require_positioned_frame
            || (req.start_ms.is_some() && !req.is_image && !req.live_source && !req.preload),
            "A positioned video start requires a file and a source position.");
        let still_raster = if req.is_image && req.geometry.source_rotation_degrees.is_some() {
            Some(source_placement::StillRaster::open(req.file_path)?)
        } else {
            None
        };
        if req.allow_silent_speed {
            anyhow::ensure!(
                !req.is_image && !req.live_source && req.audio_voice_id.is_none()
                    && req.loop_count == 0 && req.slices.is_empty() && req.start_ms.is_none()
                    && req.end_ms.is_none() && !req.hold_last_frame && req.file_path.is_file()
                    && SilentMovieSource::from_path(req.file_path).is_some(),
                "Silent speed control requires a local silent MP4, MOV or linear MKV without trimming, loops, slices or a held frame."
            );
        }
        let output = if req.preload {
            self.live_output(req.output.unwrap_or(MAIN_OUTPUT))
                .ok_or_else(|| anyhow!("Video output is unavailable for Load"))?
        } else {
            self.output_for(req.output)?
        };
        if !req.preload {
            self.place_output(&output)?;
        }
        // The master fade quad is only a blackout curtain now (startup idle,
        // panic, cleared test pattern) — any content GO lifts it; per-slot
        // opacity handles the actual reveal fade.
        let voice_id = Uuid::new_v4();
        let slot = slot::acquire_slot(&output, lib, &self.audio_engine, req.preload)?;
        let jpeg_checks_before_reveal = still_raster.is_some()
            && source_placement::is_jpeg_still(req.file_path);
        if !req.preload && !jpeg_checks_before_reveal && !req.require_soundtrack {
            output.set_overlay_alpha(0);
        }
        slot::load_into_slot(&slot, slot::SlotLoad {
            voice_id,
            audio_voice_id: req.audio_voice_id,
            require_soundtrack: req.require_soundtrack,
            url: req.file_path.to_string_lossy().replace('\\', "/"),
            is_image: req.is_image,
            allow_silent_speed: req.allow_silent_speed,
            allow_whole_loop_release: req.allow_whole_loop_release
                && !req.is_image && !req.live_source && req.audio_voice_id.is_none()
                && req.loop_count == u32::MAX && req.slices.is_empty()
                && req.start_ms.is_none() && req.end_ms.is_none() && !req.hold_last_frame,
            fade_in_ms: req.fade_in_ms,
            loop_count: req.loop_count,
            start_ms: req.start_ms,
            require_positioned_frame: req.require_positioned_frame,
            end_ms: req.end_ms,
            display_duration_ms: req.display_duration_ms,
            hold_last_frame: req.hold_last_frame,
            live_source: req.live_source,
            geometry: req.geometry,
            still_raster,
            layer_style: req.layer_style,
            slices: req.slices,
            preload: req.preload,
        })?;

        Ok(voice_id)
    }

    /// Devamp: release the visual voice's current slice loop (see
    /// [`slot::devamp_slot`]) — the pass in progress finishes, then playback
    /// continues into the next slice, or stops at the boundary when
    /// `stop_at_end` is set.  No-op for unsliced content.
    pub fn devamp_voice(&self, voice_id: VoiceId, stop_at_end: bool) {
        if let Some(slot) = output::slot_for_voice(voice_id) {
            slot::devamp_slot(&slot, stop_at_end);
        }
    }

    pub fn release_whole_video_loop(&self, voice_id: VoiceId) -> bool {
        output::slot_for_voice(voice_id)
            .is_some_and(|slot| slot::release_whole_loop(&slot, voice_id))
    }

    /// Current playback position of a visual voice in **file time** (ms) —
    /// mpv's `time-pos`, which reflects ab-loop jumps.  `None` when the voice
    /// is not on a slot (or mpv has no position yet).
    pub fn voice_position_ms(&self, voice_id: VoiceId) -> Option<u64> {
        let slot = output::slot_for_voice(voice_id)?;
        slot::position_ms(&slot)
    }

    /// Stop the content identified by `voice_id`: fade its layer's opacity to
    /// zero over `visual_fade_ms` (then unload the slot) and fade its audio
    /// voice out over `audio_fade_ms`.  Other layers are untouched.
    pub fn stop_content(&self, voice_id: VoiceId, visual_fade_ms: u32, audio_fade_ms: u32) {
        let Some(slot) = output::slot_for_voice(voice_id) else { return };

        // Take the audio voice out of the slot (the engine owns its fade-out;
        // the slot must not hard-cut it again at unload).
        let audio_id = slot.state.lock().ok().and_then(|st| st.audio_voice_id);
        slot::begin_stop(&slot, visual_fade_ms);
        if let Some(aid) = audio_id {
            let _ = self.audio_engine.stop_voice(
                aid,
                audio_fade_ms,
                crate::engine::ring_command::FadeCurve::SCurve,
            );
        }
    }

    /// Hard-stop all content immediately (no fade), on every output.
    pub fn hard_stop_current(&self) {
        slot::panic_all();
    }

    /// Panic: unconditionally cut whatever the outputs are doing.
    ///
    /// Unlike [`Self::stop_content`] / [`Self::hard_stop_current`], this also
    /// stops every overlay and paints the black curtain, so it silences every
    /// surface even when a cue lost track of its voice (double-Escape backstop).
    pub fn panic_stop(&self) {
        slot::panic_all();
        for output in self.live_outputs() {
            output.panic_overlay();
        }
    }

    /// `true` when `voice_id` is content currently on an output window.
    pub fn is_current_voice(&self, voice_id: VoiceId) -> bool {
        output::slot_for_voice(voice_id).is_some()
    }

    /// Apply per-cue visual geometry to a voice's content, live.
    ///
    /// Called from `update_cue` when the operator edits the Geometry tab of a
    /// cue that is on screen; the load path applies geometry itself.
    pub fn apply_geometry(&self, voice_id: VoiceId, geometry: &VideoGeometry) {
        let Some(slot) = output::slot_for_voice(voice_id) else { return };
        let source_placed = {
            let Ok(mut state) = slot.state.lock() else { return };
            if state.voice_id != Some(voice_id) { return; }
            state.geometry_owner = None;
            if state.still_raster.is_some() {
                // Choose the renderer at GO. Turning its saved option off
                // keeps the current angle until the operator restarts the cue.
                let angle = geometry.source_rotation_degrees.or(state.geometry.source_rotation_degrees);
                state.geometry = VideoGeometry { source_rotation_degrees: angle, ..*geometry };
                true
            } else {
                false
            }
        };
        if source_placed {
            slot.wake();
            return;
        }
        apply_scalar_geometry(&slot.lib, slot.mpv_ctx.0, geometry);
        let applied = try_apply_crop(&slot.lib, slot.mpv_ctx.0, geometry);
        if let Ok(mut st) = slot.state.lock() {
            if st.voice_id != Some(voice_id) { return; }
            st.geometry = *geometry;
            st.crop_applied = applied || !geometry.has_crop();
        }
        slot.wake();
    }

    /// Claim the current geometry on a specific playback, including a held Fade.
    pub fn begin_voice_geometry_fade(&self, voice_id: VoiceId, owner: Uuid) -> Option<VideoGeometry> {
        let slot = output::slot_for_voice(voice_id)?;
        let mut state = slot.state.lock().ok()?;
        state.claim_geometry(voice_id, owner, false)
    }

    /// Claim geometry only after an opted-in PNG frame has arrived.
    pub fn begin_voice_rotation_fade(&self, voice_id: VoiceId, owner: Uuid) -> Option<VideoGeometry> {
        let slot = output::slot_for_voice(voice_id)?;
        let mut state = slot.state.lock().ok()?;
        state.claim_geometry(voice_id, owner, true)
    }

    /// Update only selected values on the playback that still owns this Fade.
    pub fn set_voice_geometry_fade(
        &self, voice_id: VoiceId, owner: Uuid, update: VideoGeometryUpdate,
    ) -> bool {
        let (x, y) = update.translation.map_or((None, None), |position| (position.x, position.y));
        if [x, y].into_iter().flatten().any(|value| !value.is_finite())
            || update.scale.is_some_and(|value| !value.is_finite() || value <= 0.0)
            || !update.rotation_degrees.is_none_or(VideoGeometryUpdate::valid_rotation)
        {
            return false;
        }
        let Some(slot) = output::slot_for_voice(voice_id) else { return false };
        let zoom = {
            let Ok(mut state) = slot.state.lock() else { return false };
            if state.voice_id != Some(voice_id) || state.geometry_owner != Some(owner) || state.pending_unload {
                return false;
            }
            if let Some(angle) = update.rotation_degrees {
                if !state.accepts_rotation_fade() {
                    return false;
                }
                state.geometry.source_rotation_degrees = Some(angle);
            }
            if let Some(x) = x { state.geometry.pan_x = x; }
            if let Some(y) = y { state.geometry.pan_y = y; }
            if state.still_raster.is_some() {
                if let Some(scale) = update.scale { state.geometry.scale = scale; }
                drop(state);
                slot.wake();
                return true;
            }
            update.scale.map(|scale| {
                state.geometry.scale = scale;
                state.geometry.mpv_video_zoom()
            })
        };
        // The show transport serializes cue loads and edits. Keep the state lock
        // out of libmpv calls so its event and render threads remain independent.
        unsafe {
            if let Some(x) = x { prop_str(&slot.lib, slot.mpv_ctx.0, "video-pan-x", &x.to_string()); }
            if let Some(y) = y { prop_str(&slot.lib, slot.mpv_ctx.0, "video-pan-y", &y.to_string()); }
            if let Some(zoom) = zoom { prop_str(&slot.lib, slot.mpv_ctx.0, "video-zoom", &zoom.to_string()); }
        }
        slot.wake();
        true
    }

    /// Claim selected parameters and snapshot their live compositor values.
    pub fn begin_voice_color_fade(&self, voice_id: VoiceId, owner: Uuid, fields: [bool; 4]) -> [Option<f32>; 4] {
        let Some(slot) = output::slot_for_voice(voice_id) else { return [None; 4] };
        let origins = slot.state.lock().map_or([None; 4], |mut state| state.claim_color(voice_id, owner, fields));
        if origins.iter().any(Option::is_some) {
            slot.wake();
        }
        origins
    }

    pub fn set_voice_color_fade(&self, voice_id: VoiceId, owner: Uuid, values: [Option<f32>; 4]) -> [bool; 4] {
        let Some(slot) = output::slot_for_voice(voice_id) else { return [false; 4] };
        let written = slot.state.lock().map_or([false; 4], |mut state| state.write_color(voice_id, owner, values));
        if written.contains(&true) {
            slot.wake();
        }
        written
    }

    /// Display a short saturation sample on an already visible playback.
    pub(crate) fn preview_voice_saturation(&self, voice: VoiceId, saturation: f32) -> Option<Uuid> {
        let slot = output::slot_for_voice(voice)?;
        let output = self.live_output(slot.output_id)?;
        let token = Uuid::new_v4();
        let expires_at = Instant::now() + Duration::from_millis(PICTURE_SATURATION_PREVIEW_MS);
        let mut state = slot.state.lock().ok()?;
        if !output.is_visible() || !state.preview_saturation(voice, token, saturation, expires_at) {
            return None;
        }
        drop(state);
        slot.wake();
        Some(token)
    }

    /// Cancel only the exact preview requested by the operator.
    pub(crate) fn cancel_voice_saturation_preview(&self, voice: VoiceId, token: Uuid) {
        if let Some(slot) = output::slot_for_voice(voice) {
            let removed = slot.state.lock().is_ok_and(|mut state| state.cancel_saturation_preview(voice, token));
            if removed {
                slot.wake();
            }
        }
    }

    /// Live-apply compositing properties, taking over each explicitly edited color parameter.
    pub fn set_layer_props(&self, voice_id: VoiceId, style: &LayerStyle, edit_color: [bool; 4]) {
        if let Some(slot) = output::slot_for_voice(voice_id) {
            slot::set_layer_style(&slot, voice_id, style, edit_color);
        }
    }

    /// Current animated opacity (0.0–1.0) of a voice's layer.
    pub fn get_voice_opacity(&self, voice_id: VoiceId) -> f32 {
        output::slot_for_voice(voice_id).map(|s| slot::opacity_of(&s)).unwrap_or(0.0)
    }

    /// Directly drive a voice's layer opacity — Fade Cue tick at ~30 fps.
    pub fn set_voice_opacity(&self, voice_id: VoiceId, opacity: f32) {
        if let Some(slot) = output::slot_for_voice(voice_id) {
            slot::set_opacity_direct(&slot, opacity);
        }
    }

    /// Begin the visual fade-out that lands exactly on a cue's natural end
    /// (EOF), so the content fades out instead of hard-cutting.
    ///
    /// Called from `VideoCue::tick` / `ImageCue::tick` once the remaining
    /// action time drops inside the configured fade-out window.  Returns
    /// `false` (and does nothing) when `voice_id` is no longer on screen.
    pub fn begin_eof_fade_out(&self, voice_id: VoiceId, fade_ms: u32) -> bool {
        let Some(slot) = output::slot_for_voice(voice_id) else { return false };
        slot::animate_opacity(&slot, 0.0, fade_ms);
        true
    }

    /// Start a crossfade (Fade Cue): the request's incoming content dissolves
    /// in while its outgoing layers leave — mixed exactly by the compositor on
    /// the incoming output, faded out on any other output — on a clock that
    /// starts with the incoming content's first frame (see `crossfade.rs`).
    /// The outgoing content stays loaded; the Fade Cue stops it at its end.
    ///
    /// Returns `false` when the incoming content is not on an output.
    pub fn link_crossfade(&self, request: &CrossfadeRequest) -> bool {
        output::slot_for_voice(request.incoming)
            .is_some_and(|incoming| slot::link_crossfade(&incoming, request))
    }

    /// Where the crossfade into `incoming` stands.
    pub fn crossfade_phase(&self, incoming: VoiceId) -> CrossfadePhase {
        output::slot_for_voice(incoming)
            .map(|slot| slot::crossfade_phase(&slot))
            .unwrap_or(CrossfadePhase::None)
    }

    /// Pause (`true`) or resume the crossfade into `incoming` together with
    /// the Fade Cue that drives it.
    pub fn hold_crossfade(&self, incoming: VoiceId, paused: bool) {
        if let Some(slot) = output::slot_for_voice(incoming) {
            slot::hold_crossfade(&slot, paused);
        }
    }

    /// The Fade Cue driving the crossfade into `incoming` was stopped: cancel
    /// it if it has not started, freeze it where it is otherwise.
    pub fn release_crossfade(&self, incoming: VoiceId) {
        if let Some(slot) = output::slot_for_voice(incoming) {
            slot::release_crossfade(&slot);
        }
    }

    /// Start content that was preloaded (Load Cue): reveal it and unpause.
    ///
    /// A removed output or stolen slot can be retried by the owning cue.
    pub fn start_preloaded(&self, voice_id: VoiceId) -> Result<bool> {
        let Some(slot) = output::slot_for_voice(voice_id) else { return Ok(false) };
        let Some(output) = self.live_output(slot.output_id) else { return Ok(false) };
        self.place_output(&output)?;
        let lifts_at_reveal = slot.state.lock().is_ok_and(|state| {
            state.jpeg_lifts_blackout_on_reveal() || state.require_soundtrack
        });
        let started = slot::start_preloaded(&slot);
        if started && !lifts_at_reveal {
            output.set_overlay_alpha(0);
        }
        Ok(started)
    }

    /// Return the main output's master fade alpha (0 = transparent, 255 = black).
    pub fn get_overlay_alpha(&self) -> u8 {
        self.outputs
            .lock()
            .unwrap()
            .get(&MAIN_OUTPUT)
            .map(|o| o.overlay_alpha())
            .unwrap_or(0)
    }

    /// Directly set the main output's master fade alpha.
    pub fn set_overlay_alpha_direct(&self, alpha: u8) {
        if let Some(main) = self.outputs.lock().unwrap().get(&MAIN_OUTPUT) {
            main.set_overlay_alpha(alpha);
        }
    }

    /// Return the AudioEngine voice carrying a video voice's audio track.
    pub fn video_audio_voice(&self, voice_id: VoiceId) -> Option<VoiceId> {
        let slot = output::slot_for_voice(voice_id)?;
        let audio = slot.state.lock().ok().and_then(|st| st.audio_voice_id);
        audio
    }

    /// Current playback position of a voice's video (mpv `time-pos`), in ms.
    pub fn current_video_position_ms(&self, voice_id: VoiceId) -> Option<u64> {
        let slot = output::slot_for_voice(voice_id)?;
        slot::position_ms(&slot)
    }

    pub(crate) fn positioned_video_location_ms(&self, voice_id: VoiceId) -> Option<u64> {
        let slot = output::slot_for_voice(voice_id)?;
        let admitted = slot.state.lock().is_ok_and(|state| state.voice_id == Some(voice_id)
            && state.first_frame_ready && !state.positioned_reveal_pending(voice_id) && !state.pending_unload);
        admitted.then(|| slot::position_ms(&slot)).flatten()
    }

    /// Claim a prepared silent movie's speed without affecting its visual fades.
    pub fn claim_silent_speed(&self, voice: VoiceId, owner: Uuid) -> Option<SilentSpeedOrigin> {
        let slot = output::slot_for_voice(voice)?;
        let mut state = slot.state.lock().ok()?;
        if state.voice_id != Some(voice) || state.preloaded || state.pending_unload
            || state.audio_voice_id.is_some()
        { return None; }
        state.silent_speed.as_mut()?.claim(owner)
    }

    /// Read the applied origin after a preceding owner's native write settles.
    pub fn silent_speed_origin(&self, voice: VoiceId, owner: Uuid) -> Option<SilentSpeedOrigin> {
        let slot = output::slot_for_voice(voice)?;
        let state = slot.state.lock().ok()?;
        if state.voice_id != Some(voice) || state.pending_unload { return None; }
        state.silent_speed.as_ref()?.origin(owner)
    }

    pub fn set_silent_speed(&self, voice: VoiceId, owner: Uuid, rate: f64) -> bool {
        if !(0.25..=4.0).contains(&rate) { return false; }
        let Some(slot) = output::slot_for_voice(voice) else { return false };
        let Ok(mut state) = slot.state.lock() else { return false };
        if state.voice_id != Some(voice) || state.pending_unload { return false; }
        let Some(speed) = state.silent_speed.as_mut() else { return false };
        if speed.eligible != Some(true) || speed.owner != Some(owner) { return false; }
        speed.requested = Some(rate);
        drop(state);
        slot.wake_event_worker();
        true
    }

    /// Drop an unapplied update when this Fade is stopped or paused.
    pub fn hold_silent_speed(&self, voice: VoiceId, owner: Uuid) {
        let Some(slot) = output::slot_for_voice(voice) else { return };
        let Ok(mut state) = slot.state.lock() else { return };
        if state.voice_id != Some(voice) { return; }
        if let Some(speed) = state.silent_speed.as_mut().filter(|speed| speed.owner == Some(owner)) {
            speed.requested = None;
        }
    }

    /// File time from the slot's existing time-pos observation. The show tick
    /// never waits on a movie decoder for timing information.
    pub fn silent_speed_position_ms(&self, voice: VoiceId) -> Option<u64> {
        let slot = output::slot_for_voice(voice)?;
        let state = slot.state.lock().ok()?;
        if state.voice_id != Some(voice) || state.pending_unload { return None; }
        state.silent_speed.as_ref()?.position_ms
    }

    /// Re-anchor the paired audio voice to the video's **actual** position
    /// (mpv `time-pos`), without moving mpv.  Corrects the A/V drift that builds
    /// up when the picture keeps advancing while the audio voice is frozen
    /// during an output-device outage.
    pub fn resync_audio_to_video(&self, voice_id: VoiceId) {
        if output::slot_for_voice(voice_id).is_some_and(|slot| {
            slot.state.lock().is_ok_and(|state| state.positioned_reveal_pending(voice_id))
        }) {
            return;
        }
        let Some(audio) = self.video_audio_voice(voice_id) else { return };
        if let Some(ms) = self.current_video_position_ms(voice_id) {
            let _ = self.audio_engine.seek_voice_ms(audio, ms);
        }
    }

    // ── Legacy API kept for VideoCue ─────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    pub fn play_voice(
        &self,
        file_path: &Path,
        output: Option<OutputId>,
        _volume_db: f64,
        loop_count: u32,
        start_ms: Option<u64>,
        end_ms: Option<u64>,
        _fade_in: Option<&FadeSpec>,
    ) -> Result<VoiceId> {
        self.show_content(ContentRequest {
            preload: false,
            file_path,
            is_image: false,
            allow_silent_speed: false,
            allow_whole_loop_release: false,
            fade_in_ms: 0,
            loop_count,
            start_ms,
            require_positioned_frame: false,
            end_ms,
            output,
            audio_voice_id: None,
            require_soundtrack: false,
            display_duration_ms: None,
            hold_last_frame: false,
            geometry: VideoGeometry::default(),
            live_source: false,
            layer_style: LayerStyle::default(),
            slices: Vec::new(),
        })
    }

    pub fn stop_voice(&self, voice_id: VoiceId, fade_ms: u32) -> Result<()> {
        self.stop_content(voice_id, fade_ms, fade_ms);
        Ok(())
    }

    pub fn stop_current_voice(&self, _fade_ms: u32) {
        self.hard_stop_current();
    }

    /// The mpv context owning `voice_id` (the voice's slot).
    fn voice_mpv_ctx(&self, voice_id: VoiceId) -> Option<*mut c_void> {
        output::slot_for_voice(voice_id).map(|s| s.mpv_ctx.0)
    }

    pub fn pause_voice(&self, voice_id: VoiceId) -> Result<()> {
        if output::slot_for_voice(voice_id).is_some_and(|slot| slot::hold_pending_reveal(&slot, voice_id, true)) {
            return Ok(());
        }
        if self.queue_silent_pause(voice_id, true) { return Ok(()); }
        if let (Some(lib), Some(ctx)) = (self.try_mpv_lib(), self.voice_mpv_ctx(voice_id)) {
            unsafe {
                (lib.mpv_set_property_string)(ctx, cs("pause").as_ptr(), cs("yes").as_ptr());
            }
        }
        if let Some(aid) = self.video_audio_voice(voice_id) {
            let _ = self.audio_engine.pause_voice(aid);
        }
        Ok(())
    }

    pub fn resume_voice(&self, voice_id: VoiceId) -> Result<()> {
        let slot = output::slot_for_voice(voice_id);
        if slot.as_ref().is_some_and(|slot| slot::hold_pending_reveal(slot, voice_id, false)) {
            return Ok(());
        }
        if self.queue_silent_pause(voice_id, false) { return Ok(()); }
        let soundtrack = slot.as_ref().and_then(|slot| {
            let state = slot.state.lock().ok()?;
            if state.voice_id != Some(voice_id) { return None; }
            state.audio_voice_id.map(|audio| (audio, state.require_soundtrack))
        });
        if let (Some(slot), Some((audio, true))) = (&slot, soundtrack) {
            if let Err(error) = self.audio_engine.resume_required_soundtrack(audio) {
                slot::withdraw_required_soundtrack(slot, audio);
                return Err(error);
            }
        }
        if let (Some(lib), Some(ctx)) = (self.try_mpv_lib(), self.voice_mpv_ctx(voice_id)) {
            unsafe {
                (lib.mpv_set_property_string)(ctx, cs("pause").as_ptr(), cs("no").as_ptr());
            }
        }
        if let Some((aid, false)) = soundtrack {
            let _ = self.audio_engine.resume_voice(aid);
        }
        Ok(())
    }

    pub(crate) fn withdraw_required_soundtrack(&self, voice: VoiceId) {
        for slot in output::all_slots() {
            if slot::withdraw_required_soundtrack(&slot, voice) { break; }
        }
    }

    pub(crate) fn finish_required_soundtrack(&self, voice: VoiceId) {
        for slot in output::all_slots() {
            if let Ok(mut state) = slot.state.lock() {
                if state.require_soundtrack && state.audio_voice_id == Some(voice) {
                    // The soundtrack can end before the movie's last frame.
                    state.audio_voice_id = None;
                    state.require_soundtrack = false;
                    break;
                }
            }
        }
    }

    pub fn soundtrack_revealed(&self, voice: VoiceId) -> bool {
        output::slot_for_voice(voice).is_some_and(|slot| slot.state.lock().is_ok_and(|state| {
            state.voice_id == Some(voice) && state.soundtrack_revealed && !state.pending_unload
        }))
    }

    fn queue_silent_pause(&self, voice: VoiceId, paused: bool) -> bool {
        let Some(slot) = output::slot_for_voice(voice) else { return false };
        let Ok(mut state) = slot.state.lock() else { return false };
        if state.voice_id != Some(voice) { return false; }
        let Some(speed) = state.silent_speed.as_mut() else { return false };
        speed.request_pause(paused);
        drop(state);
        slot.wake_event_worker();
        true
    }

    pub fn set_voice_volume(&self, voice_id: VoiceId, volume_db: f64) -> Result<()> {
        if let Some(aid) = self.video_audio_voice(voice_id) {
            let _ = self.audio_engine.set_voice_gain(aid, db_to_linear(volume_db) as f32);
        }
        Ok(())
    }

    /// Seek a voice's video (and re-anchor its paired audio voice).
    pub fn seek_voice_ms(&self, voice_id: VoiceId, position_ms: u64) {
        let (Some(lib), Some(ctx)) = (self.try_mpv_lib(), self.voice_mpv_ctx(voice_id)) else {
            return;
        };
        let pos_str = format!("{:.3}", position_ms as f64 / 1000.0);
        let cmd_cstr = cs("seek");
        let pos_cstr = cs(&pos_str);
        let mode_cstr = cs("absolute");
        unsafe {
            let args = [
                cmd_cstr.as_ptr(),
                pos_cstr.as_ptr(),
                mode_cstr.as_ptr(),
                std::ptr::null(),
            ];
            (lib.mpv_command)(ctx, args.as_ptr());
        }
        if let Some(aid) = self.video_audio_voice(voice_id) {
            let _ = self.audio_engine.seek_voice_ms(aid, position_ms);
        }
    }

    // ── Window visibility ─────────────────────────────────────────────────────

    /// Toggle the visibility of the output windows (F9 / View menu): hide them
    /// all when any is visible, otherwise show them all.
    pub fn toggle_visibility(&self) {
        if self.is_visible() {
            self.hide_output();
        } else {
            self.show_output();
        }
    }

    /// Make every output window visible.
    pub fn show_output(&self) {
        for output in self.live_outputs() {
            output.show();
        }
        self.emit_visibility(true);
    }

    /// Hide every output window.
    pub fn hide_output(&self) {
        for output in self.live_outputs() {
            output.hide();
        }
        self.emit_visibility(false);
    }

    /// Whether any output window is currently visible.
    pub fn is_visible(&self) -> bool {
        self.live_outputs().iter().any(|o| o.is_visible())
    }

    /// Whether any output window is currently user-visible.
    pub fn is_output_visible(&self) -> bool {
        self.is_visible()
    }

    fn emit_visibility(&self, visible: bool) {
        use tauri::Emitter;
        let _ = self.app_handle.emit("output-window-visible", visible);
    }

    // ── OSD / timer ──────────────────────────────────────────────────────────

    /// The main output, when it exists (not headless).
    fn main_output(&self) -> Option<Arc<Output>> {
        self.live_output(MAIN_OUTPUT)
    }

    /// Update the countdown text of the on-output timer (mpv OSD), on the
    /// output the show picked for it.  An output that is gone shows it nowhere
    /// — never on another screen, which may be the audience's.
    ///
    /// Pass `None` (or an empty string) to hide the timer.
    pub fn set_output_timer(&self, text: Option<&str>) {
        let id = self.config.lock().map(|c| c.timer_output_id()).unwrap_or(MAIN_OUTPUT);
        if let Some(output) = self.live_output(id) {
            output.set_timer_text(text.unwrap_or(""));
        }
    }

    /// Apply font, size, position and margin settings for the OSD timer overlay.
    pub fn set_timer_style(
        &self,
        font: &str,
        font_size: u32,
        position: crate::preferences::TimerPosition,
        margin: u32,
    ) {
        use crate::preferences::TimerPosition;
        let font_changed = FLOAT_TIMER_FONT.get().and_then(|m| m.lock().ok()).map(|mut g| {
            if *g != font { *g = font.to_owned(); true } else { false }
        }).unwrap_or(false);
        if font_changed {
            use tauri::Emitter;
            let _ = self.app_handle.emit("float-timer-font", font);
        }
        let align = match position {
            TimerPosition::Center      => ("center", "center"),
            TimerPosition::TopLeft     => ("left",   "top"),
            TimerPosition::TopRight    => ("right",  "top"),
            TimerPosition::BottomLeft  => ("left",   "bottom"),
            TimerPosition::BottomRight => ("right",  "bottom"),
        };
        let margin = match position {
            TimerPosition::Center => "0".to_string(),
            _                    => margin.to_string(),
        };
        // Every output carries the style, so the timer looks the same
        // whichever one it moves to; outputs opened later get it at creation.
        for output in self.live_outputs() {
            output.set_timer_osd_style(font, font_size, align, &margin);
        }
        if let Ok(mut style) = self.timer_style.lock() {
            *style = Some(TimerOsdStyle { font: font.to_owned(), size: font_size, align, margin });
        }
    }

    // ── Floating timer (Tauri WebView window) ─────────────────────────────────

    /// Show or hide the standalone floating timer window (Tauri WebView).
    ///
    /// GTK (Linux) and AppKit (macOS) require window show/hide on the main
    /// thread, but Tauri command handlers run on a worker thread.  Marshalling
    /// onto the main thread makes this safe on all three OS — the same
    /// cross-platform discipline the output windows follow.
    pub fn set_floating_timer_visible(&self, visible: bool) {
        let app = self.app_handle.clone();
        let _ = self.app_handle.run_on_main_thread(move || {
            use tauri::Manager;
            if let Some(win) = app.get_webview_window("float-timer") {
                let _ = if visible { win.show() } else { win.hide() };
            }
        });
    }

    /// Write the current timer text to the floating window.
    /// Only emits a Tauri event when the text actually changed.
    pub fn update_floating_timer(&self, text: Option<&str>) {
        let new_text = text.unwrap_or("");
        let changed = FLOAT_TIMER_TEXT.get().and_then(|m| m.lock().ok()).map(|mut g| {
            if *g != new_text { *g = new_text.to_owned(); true } else { false }
        }).unwrap_or(false);
        if changed {
            use tauri::Emitter;
            let _ = self.app_handle.emit("float-timer-text", new_text);
        }
    }

    /// Set or clear the preview text shown on the OSD timer.
    pub fn set_timer_preview(&self, text: Option<String>) {
        if let Some(m) = TIMER_PREVIEW.get() {
            if let Ok(mut g) = m.lock() {
                *g = text;
            }
        }
    }

    /// Return the current preview text, if any.
    pub fn get_timer_preview(&self) -> Option<String> {
        TIMER_PREVIEW.get()?.lock().ok()?.clone()
    }

    // ── Text overlay (sub-text / ASS) ────────────────────────────────────────

    /// Display an ASS-tagged text string on an output (`None` = main).
    ///
    /// When nothing is playing, a transparent lavfi source is loaded so the OSD
    /// has a surface to composite onto and the output shows black rather than
    /// the desktop.
    pub fn show_text_overlay(&self, ass_text: &str, output: Option<OutputId>) {
        let output = match self.output_for(output).and_then(|o| self.place_output(&o).map(|_| o)) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("[output] text overlay not shown: {e}");
                return;
            }
        };
        output.show_text(ass_text);
    }

    /// Clear the text set via [`show_text_overlay`] on an output (`None` = main).
    pub fn clear_text_overlay(&self, output: Option<OutputId>) {
        let id = output.unwrap_or(MAIN_OUTPUT);
        if let Some(live) = self.outputs.lock().unwrap().get(&id).cloned() {
            live.clear_text();
        }
    }

    // ── Test patterns (projector calibration) ────────────────────────────────

    /// Show a calibration pattern (grid, colour bars, custom image, …) on an
    /// output (`None` = main), replacing whatever that output is playing.
    ///
    /// The output's content is hard-stopped first (its owning cue resets
    /// through `OutputStatus::Withdrawn`), the window is
    /// positioned like a GO would (fallback + banner included), and the pattern
    /// is shown with **neutral cue geometry** — only the output's own
    /// transform applies, which is exactly what alignment and colorimetry need.
    pub fn show_test_pattern(&self, pattern: &TestPattern, output: Option<OutputId>) {
        let output = match self.output_for(output) {
            Ok(o) => o,
            Err(e) => {
                log::warn!("[output] test pattern ignored: {e}");
                return;
            }
        };
        for slot in output.slots_snapshot() {
            slot::hard_unload(&slot, slot::UnloadReport::Withdrawn);
        }
        if let Err(e) = self.place_output(&output) {
            log::warn!("[output] test pattern ignored: {e}");
            return;
        }

        // Pattern resolution: match the target screen so the grid is 1:1.
        let (w, h) = resolve_output_screen(&self.list_screens(), self.configured_screen(output.id))
            .0
            .map(|s| (s.width, s.height))
            .unwrap_or((1920, 1080));
        output.show_pattern(pattern, w, h);
    }

    /// Clear the test pattern of an output (`None` = every output): stop
    /// playback and return to opaque black.
    pub fn clear_test_pattern(&self, output: Option<OutputId>) {
        for live in self.live_outputs() {
            if output.is_none_or(|id| id == live.id) {
                live.clear_pattern();
            }
        }
    }

    // ── Screen identification ────────────────────────────────────────────────

    /// Flash `ass` on `screen` using an output's window, so the operator can
    /// verify before a show that "Screen 2" really is the projector.  The
    /// window is moved there for the occasion only; [`Self::end_identify`]
    /// puts everything back.
    pub fn identify_screen(
        &self,
        output: Option<OutputId>,
        screen: Option<u32>,
        ass: &str,
    ) -> Result<()> {
        let output = self.output_for(output)?;
        self.place_output_at(&output, screen)?;
        output.show_text(ass);
        Ok(())
    }

    /// End an identification: remove the label and return the window to where
    /// the show wants it (or hide it again when it was hidden before).
    pub fn end_identify(&self, output: Option<OutputId>, was_visible: bool) {
        self.clear_text_overlay(output);
        let Ok(live) = self.output_for(output) else { return };
        if was_visible {
            let _ = self.place_output(&live);
        } else {
            live.hide();
            self.emit_visibility(self.is_visible());
        }
    }

    /// Operator-facing name of an output (`None` = main).
    pub fn output_name(&self, output: Option<OutputId>) -> String {
        self.config
            .lock()
            .map(|c| c.name_of(output.unwrap_or(MAIN_OUTPUT)))
            .unwrap_or_default()
    }

    // ── Placement ─────────────────────────────────────────────────────────────

    /// Immediately apply the main output's screen preference to its live window.
    ///
    /// `Some(idx)` shows the window fullscreen on that screen right away (same
    /// missing-screen fallback + health banner as a GO), so the operator sees
    /// the effect of the Preferences selection without waiting for the next
    /// visual cue.  `None` (floating) restores the windowed floating rect.
    pub fn apply_output_screen(&self, screen_index: Option<u32>) {
        self.apply_output_screen_of(None, screen_index);
    }

    /// [`Self::apply_output_screen`] for any output (`None` = main).  An extra
    /// output that has not been created yet is created when it gets a screen,
    /// so the operator sees the projector light up as soon as it is assigned.
    pub fn apply_output_screen_of(&self, output: Option<OutputId>, screen_index: Option<u32>) {
        let id = output.unwrap_or(MAIN_OUTPUT);
        let seam_config = self.config.lock().ok().and_then(|mut config| {
            let peer = config.stored_transform_of(id).regions.seam_partner();
            if id == MAIN_OUTPUT {
                config.main_screen = screen_index;
            } else if let Some(extra) = config.extras.iter_mut().find(|c| c.id == id) {
                extra.screen = screen_index;
            }
            peer.map(|peer| (peer, config.transform_of(id), config.transform_of(peer)))
        });
        if let Some((peer, own_transform, peer_transform)) = seam_config {
            for (id, transform) in [(id, own_transform), (peer, peer_transform)] {
                if let Some(live) = self.live_output(id) { apply_transform(&live, &transform); }
            }
        }
        match screen_index {
            Some(_) => {
                if let Ok(live) = self.output_for(Some(id)) {
                    let _ = self.place_output(&live);
                }
            }
            None => {
                crate::health::clear(&screen_alert_id(id));
                if let Some(live) = self.outputs.lock().unwrap().get(&id) {
                    live.window.set_windowed_floating();
                }
            }
        }
    }

    /// Apply the output-screen preference when a workspace is (re)loaded, so a
    /// configured screen goes live as a black fullscreen surface immediately —
    /// not only on the first visual GO.  The extra outputs get the same
    /// treatment as they open (see [`Self::sync_outputs_config`]).
    pub fn apply_output_screen_on_load(&self, screen_index: Option<u32>) {
        if let Ok(mut config) = self.config.lock() {
            config.main_screen = screen_index;
        }
        if let Some(main) = self.main_output() {
            self.apply_screen_on_load(&main, screen_index);
        }
    }

    /// Light the screens of the extra outputs already open when a workspace
    /// is (re)loaded; those that open later are lit as they open.
    pub fn apply_extra_screens_on_load(&self) {
        for output in self.live_outputs().into_iter().filter(|o| o.id != MAIN_OUTPUT) {
            self.apply_screen_on_load(&output, self.configured_screen(output.id));
        }
    }

    /// Light an output's screen the way a workspace load does.  A connected
    /// screen goes live as a black fullscreen surface.  A missing one raises
    /// the banner and keeps the window hidden: falling back to the operator's
    /// own display here would black it out the moment a show opens without
    /// its projector.  A floating output stays hidden until a cue plays on it.
    fn apply_screen_on_load(&self, output: &Arc<Output>, screen_index: Option<u32>) {
        match screen_index {
            None => {
                crate::health::clear(&screen_alert_id(output.id));
                output.window.set_windowed_floating();
            }
            Some(idx) if self.list_screens().iter().any(|s| s.index == idx) => {
                let _ = self.place_output(output);
            }
            Some(idx) => crate::health::set(crate::health::HealthAlert::new(
                screen_alert_id(output.id),
                crate::health::HealthLevel::Warning,
                missing_screen_on_load(output, idx),
            )),
        }
    }

    /// Toggle the main output window between windowed and true fullscreen.
    pub fn toggle_fullscreen(&self) {
        if let Some(main) = self.main_output() {
            main.window.toggle_fullscreen();
        }
    }

    // ── Status / GC ──────────────────────────────────────────────────────────

    pub fn push_status(&self, _status: OutputStatus) {}

    /// Drain all pending status events.  Called by the 30 fps event loop.
    pub fn drain_status(&self) -> Vec<OutputStatus> {
        let rx = self.status_rx.lock().unwrap();
        let mut out = Vec::new();
        while let Ok(s) = rx.try_recv() {
            out.push(s);
        }
        out
    }

    /// A completed voice needs no cleanup: its slot released itself.
    pub fn gc_voice(&self, _voice_id: VoiceId) {}

    // ── Internal helpers ─────────────────────────────────────────────────────

    /// Put `output` on its configured screen and make it visible.
    ///
    /// The **main** output falls back to the primary display when its screen is
    /// missing (never a silent no-op) and raises a health banner.  An **extra**
    /// output refuses instead — showing a façade projector's picture on the
    /// operator's own screen would be worse than showing nothing.
    fn place_output(&self, output: &Arc<Output>) -> Result<()> {
        self.place_output_at(output, self.configured_screen(output.id))
    }

    /// [`Self::place_output`] on an explicit screen instead of the configured
    /// one (screen identification).
    fn place_output_at(&self, output: &Arc<Output>, configured: Option<u32>) -> Result<()> {
        let (target_screen, screen_missing) =
            resolve_output_screen(&self.list_screens(), configured);
        let alert_id = screen_alert_id(output.id);

        if screen_missing {
            let shown = configured.map(|i| i + 1).unwrap_or(0);
            if output.id != MAIN_OUTPUT {
                crate::health::set(crate::health::HealthAlert::new(
                    &alert_id,
                    crate::health::HealthLevel::Warning,
                    format!(
                        "Output '{}' is on screen {shown}, which is not connected. \
                         Check Preferences → Outputs.",
                        output.name(),
                    ),
                ));
                return Err(anyhow!(
                    "Output '{}' is not connected (screen {shown})",
                    output.name(),
                ));
            }
            crate::health::set(crate::health::HealthAlert::new(
                &alert_id,
                crate::health::HealthLevel::Warning,
                format!(
                    "Output screen {shown} is not connected — using the primary display instead. \
                     Check Preferences → Display.",
                ),
            ));
        } else {
            crate::health::clear(&alert_id);
        }

        if let Some(s) = &target_screen {
            // Windows/Linux: borderless-fullscreen the winit window on the
            // monitor matching the physical rect from list_screens(). macOS:
            // place the NSWindow onto NSScreen[idx] directly (AppKit's own
            // coordinate space — no rect conversion needed; our sorted list and
            // NSScreen both put the primary at index 0, so the fallback index
            // maps correctly too).
            #[cfg(not(target_os = "macos"))]
            output.window.place_on_rect(s.x, s.y, s.width, s.height);
            #[cfg(target_os = "macos")]
            output.window.place_on_screen(s.index);
        }
        output.show();
        self.emit_visibility(true);
        Ok(())
    }
}

/// The banner for an output whose screen is missing when the show opens.
fn missing_screen_on_load(output: &Output, idx: u32) -> String {
    if output.id == MAIN_OUTPUT {
        format!(
            "Output screen {} is not connected — the output window stays hidden; visual cues \
             will fall back to the primary display. Check Preferences → Display.",
            idx + 1,
        )
    } else {
        format!(
            "Output '{}' is on screen {}, which is not connected — its cues will not play \
             until it is. Check Preferences → Outputs.",
            output.name(),
            idx + 1,
        )
    }
}

/// Error for an extra output that is not (or no longer) part of the show.
fn ensure_output_exists(config: &OutputsConfig, id: OutputId) -> Result<()> {
    if config.extras.iter().any(|c| c.id == id) {
        Ok(())
    } else {
        Err(anyhow!(
            "This cue's video output no longer exists — pick another output in the cue's inspector"
        ))
    }
}

/// Apply an alignment transform to an output (no-op when unchanged).
fn apply_transform(output: &Arc<Output>, transform: &OutputTransform) {
    let changed = output
        .transform
        .lock()
        .map(|mut t| {
            if *t == *transform {
                false
            } else {
                *t = *transform;
                true
            }
        })
        .unwrap_or(false);
    if changed {
        output.set_warp(warp::OutputWarp::from_transform(transform));
    }
}

// ---------------------------------------------------------------------------
// Private utility functions
// ---------------------------------------------------------------------------

pub(super) fn cs(s: &str) -> CString {
    CString::new(s).expect("cs(): interior NUL byte in literal")
}

/// Read an int64 mpv property, or `None` when unavailable.
pub(super) unsafe fn get_prop_i64(lib: &MpvLib, ctx: *mut c_void, name: &str) -> Option<i64> {
    let mut val: i64 = 0;
    let n = cs(name);
    let ret = (lib.mpv_get_property)(
        ctx,
        n.as_ptr(),
        MPV_FORMAT_INT64,
        &mut val as *mut i64 as *mut c_void,
    );
    (ret == 0).then_some(val)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SilentMovieSource {
    QuickTime,
    LinearMatroska,
}

impl SilentMovieSource {
    fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        if extension.eq_ignore_ascii_case("mp4") || extension.eq_ignore_ascii_case("mov") {
            Some(Self::QuickTime)
        } else if extension.eq_ignore_ascii_case("mkv") {
            Some(Self::LinearMatroska)
        } else {
            None
        }
    }

    fn matches_format(self, format: &str) -> bool {
        match self {
            Self::QuickTime => format.split(',').any(|part| part == "mov"),
            // Ordered chapters can introduce other files and tracks.
            // libmpv reports those as a timeline such as "mkv_oc/mkv".
            Self::LinearMatroska => format == "mkv",
        }
    }
}

fn silent_local_movie(lib: &MpvLib, ctx: *mut c_void, path: &str) -> bool {
    let Some(source) = SilentMovieSource::from_path(Path::new(path)) else { return false };
    // The demuxer reports audio tracks even with mpv's audio output disabled.
    let property = |name: &str| {
        let key = cs(name);
        let mut value: *mut std::ffi::c_char = std::ptr::null_mut();
        let result = unsafe {
            (lib.mpv_get_property)(ctx, key.as_ptr(), super::mpv_sys::MPV_FORMAT_STRING,
                (&mut value as *mut *mut std::ffi::c_char).cast())
        };
        if result != 0 || value.is_null() { return None; }
        let text = unsafe { std::ffi::CStr::from_ptr(value).to_string_lossy().into_owned() };
        unsafe { (lib.mpv_free)(value.cast()); }
        Some(text)
    };
    if property("path").as_deref() != Some(path)
        || !property("file-format").is_some_and(|format| source.matches_format(&format))
        || (source == SilentMovieSource::LinearMatroska
            && property("current-demuxer").as_deref() != Some("mkv"))
    { return false; }
    let Some(count) = (unsafe { get_prop_i64(lib, ctx, "track-list/count") }) else { return false };
    if !(1..=128).contains(&count) { return false; }
    let mut video = false;
    for index in 0..count {
        let track = |field| property(&format!("track-list/{index}/{field}"));
        match track("type").as_deref() {
            Some("video") => {
                video |= source == SilentMovieSource::QuickTime
                    || (track("selected").as_deref() == Some("yes")
                        && track("image").as_deref() == Some("no")
                        && track("albumart").as_deref() != Some("yes"));
            }
            Some("sub") => {}
            _ => return false,
        }
    }
    video
}

/// Apply the pixel `video-crop` derived from `geometry` — possible only once
/// the source dimensions (`video-params/w|h`) are known.  Returns `false`
/// when they are not yet available (caller keeps the crop pending).
pub(super) fn try_apply_crop(lib: &MpvLib, ctx: *mut c_void, geometry: &VideoGeometry) -> bool {
    unsafe {
        let w = get_prop_i64(lib, ctx, "video-params/w").unwrap_or(0);
        let h = get_prop_i64(lib, ctx, "video-params/h").unwrap_or(0);
        if w <= 0 || h <= 0 {
            return false;
        }
        match geometry.crop_rect_px(w as u32, h as u32) {
            Some((cw, ch, cx, cy)) => {
                prop_str(lib, ctx, "video-crop", &format!("{cw}x{ch}+{cx}+{cy}"));
            }
            None => prop_str(lib, ctx, "video-crop", ""),
        }
        true
    }
}

/// Push a geometry's scalar mpv properties (everything except the pixel
/// crop, which needs the source dimensions).  Per-context — used by both the
/// overlay context and each video slot.
///
/// The cue geometry is applied **pure** (the global OutputTransform lives in
/// the warp render pass).
pub(super) fn apply_scalar_geometry(lib: &MpvLib, ctx: *mut c_void, geometry: &VideoGeometry) {
    let props = compose_display_props(geometry, &OutputTransform::default());
    unsafe {
        let (keepaspect, panscan) = geometry.fit_props();
        prop_str(lib, ctx, "keepaspect", keepaspect);
        prop_str(lib, ctx, "panscan", panscan);
        prop_str(lib, ctx, "video-zoom", &format!("{:.6}", props.zoom_log2));
        prop_str(lib, ctx, "video-pan-x", &format!("{:.6}", props.pan_x));
        prop_str(lib, ctx, "video-pan-y", &format!("{:.6}", props.pan_y));
        prop_str(lib, ctx, "video-rotate", &props.rotation.to_string());
    }
}

pub(super) unsafe fn opt_str(lib: &MpvLib, ctx: *mut c_void, name: &str, value: &str) {
    let n = cs(name);
    let v = cs(value);
    (lib.mpv_set_option_string)(ctx, n.as_ptr(), v.as_ptr());
}

/// Set an mpv *property* (after `mpv_initialize`).
pub(super) unsafe fn prop_str(lib: &MpvLib, ctx: *mut c_void, name: &str, value: &str) {
    let n = cs(name);
    let v = cs(value);
    (lib.mpv_set_property_string)(ctx, n.as_ptr(), v.as_ptr());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_movie_accepts_matching_linear_file_containers_only() {
        let mov = SilentMovieSource::from_path(Path::new("rehearsal.MOV")).unwrap();
        let mp4 = SilentMovieSource::from_path(Path::new("rehearsal.mp4")).unwrap();
        let mkv = SilentMovieSource::from_path(Path::new("rehearsal.MkV")).unwrap();

        assert_eq!(mov, mp4);
        assert!(mov.matches_format("mov,mp4,m4a,3gp,3g2,mj2"));
        assert!(!mov.matches_format("mkv"));
        assert!(mkv.matches_format("mkv"));
        assert!(!mkv.matches_format("mkv_oc/mkv"));
        assert!(!mkv.matches_format("mov,mp4,m4a,3gp,3g2,mj2"));
        assert!(SilentMovieSource::from_path(Path::new("rehearsal.webm")).is_none());
    }

    #[test]
    fn headless_error_names_the_missing_piece() {
        // The operator reads this on a cue that will not fire; it has to say
        // what is wrong, not just that something is.
        assert!(NO_VIDEO_OUTPUT.contains("libmpv"));
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    fn config_with_extra() -> (OutputsConfig, OutputConfig) {
        let mut extra = OutputConfig::new("Façade");
        extra.screen = Some(2);
        extra.transform.pan_x = 0.1;
        let config = OutputsConfig {
            main_screen: Some(1),
            mask_images: HashMap::new(),
            main_transform: OutputTransform { rotation: 0.5, ..OutputTransform::default() },
            extras: vec![extra.clone()],
            timer_output: None,
        };
        (config, extra)
    }

    #[test]
    fn the_timer_belongs_on_the_main_output_unless_the_show_moves_it() {
        let (mut config, extra) = config_with_extra();
        assert_eq!(config.timer_output_id(), MAIN_OUTPUT);
        config.timer_output = Some(extra.id);
        assert_eq!(config.timer_output_id(), extra.id);
    }

    #[test]
    fn only_declared_outputs_are_extras_of_the_show() {
        let (config, extra) = config_with_extra();
        assert!(config.has_extra(extra.id));
        assert!(!config.has_extra(MAIN_OUTPUT), "the main output is not an extra");
        assert!(!config.has_extra(Uuid::new_v4()));
    }

    #[test]
    fn a_missing_screen_banner_says_which_preference_to_check() {
        let main = Output::new(MAIN_OUTPUT, "Main");
        assert!(missing_screen_on_load(&main, 1).contains("Preferences → Display"));
        let extra = Output::new(Uuid::new_v4(), "Façade");
        let text = missing_screen_on_load(&extra, 2);
        assert!(text.contains("'Façade'") && text.contains("screen 3") && text.contains("Preferences → Outputs"));
    }

    #[test]
    fn each_output_has_its_own_screen() {
        let (config, extra) = config_with_extra();
        assert_eq!(config.screen_of(MAIN_OUTPUT), Some(1));
        assert_eq!(config.screen_of(extra.id), Some(2));
        assert_eq!(config.screen_of(Uuid::new_v4()), None, "an unknown output is floating");
    }

    #[test]
    fn each_output_has_its_own_alignment() {
        let (config, extra) = config_with_extra();
        assert_eq!(config.transform_of(MAIN_OUTPUT).rotation, 0.5);
        assert_eq!(config.transform_of(MAIN_OUTPUT).pan_x, 0.0);
        assert_eq!(config.transform_of(extra.id).pan_x, 0.1);
        assert!(config.transform_of(Uuid::new_v4()).is_identity());
    }

    #[test]
    fn seam_uses_two_reciprocal_layouts_on_distinct_screens() {
        let (mut config, extra) = config_with_extra();
        let direction = OutputSplitDirection::Horizontal;
        let [left, right] = direction.feathered_regions(1_000, 220, [MAIN_OUTPUT, extra.id]).unwrap();
        config.main_transform.regions = left;
        config.extras[0].transform.regions = right;
        config.main_screen = Some(0);
        config.extras[0].screen = Some(1);
        assert_eq!(config.transform_of(MAIN_OUTPUT).regions, left);
        assert_eq!(config.transform_of(extra.id).regions, right);
        config.extras[0].screen = Some(0);
        assert!(config.transform_of(MAIN_OUTPUT).regions.seam_partner().is_none());
        assert!(config.transform_of(extra.id).regions.seam_partner().is_none());
        config.extras[0].screen = Some(1);
        config.extras[0].transform.regions = direction.regions(1_000).unwrap()[1];
        assert!(config.transform_of(MAIN_OUTPUT).regions.seam_partner().is_none());
        assert_eq!(config.stored_transform_of(MAIN_OUTPUT).regions, left);
        config.extras.clear();
        assert!(config.transform_of(MAIN_OUTPUT).regions.seam_partner().is_none());
    }

    #[test]
    fn names_come_from_the_show() {
        let (config, extra) = config_with_extra();
        assert_eq!(config.name_of(MAIN_OUTPUT), "Main");
        assert_eq!(config.name_of(extra.id), "Façade");
    }

    #[test]
    fn health_banners_are_per_output() {
        let extra = Uuid::new_v4();
        assert_eq!(screen_alert_id(MAIN_OUTPUT), "output-screen", "the main banner keeps its id");
        assert_ne!(screen_alert_id(extra), screen_alert_id(MAIN_OUTPUT));
        assert_ne!(screen_alert_id(extra), screen_alert_id(Uuid::new_v4()));
    }

    #[test]
    fn a_deleted_output_is_refused_with_an_actionable_message() {
        let (config, extra) = config_with_extra();
        assert!(ensure_output_exists(&config, extra.id).is_ok());
        let err = ensure_output_exists(&config, Uuid::new_v4()).unwrap_err().to_string();
        assert!(err.contains("no longer exists"), "got: {err}");
    }

    #[test]
    fn an_unchanged_config_compares_equal() {
        let (config, _) = config_with_extra();
        assert_eq!(config, config.clone());
        assert_ne!(config, OutputsConfig::default());
    }
}
