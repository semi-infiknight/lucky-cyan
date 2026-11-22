//! Zone (🔴 core) — Crossfade: a Fade Cue that dissolves its targets into
//! another visual cue.
//!
//! Drives `Transport::go` plus the event loop's per-tick crossfade step
//! (`show::crossfade::advance_pending`) against the recording engine doubles.
//! The output double's `CrossfadeSim` stands in for the picture: a linked
//! dissolve either starts at once (content already on screen) or waits for
//! `reveal_all()` (a video decoding its first frame).

mod common;

use std::time::{Duration, Instant};

use common::{
    full_registry, recording_context, recording_context_with_crossfades,
    recording_context_with_video_audio, CallLog, CrossfadeSim, EngineCall,
};
use inkue_lib::cue::context::CueContext;
use inkue_lib::cue::fade_cue::{FadeCue, RelativeGeometry};
use inkue_lib::cue::group_cue::GroupCue;
use inkue_lib::cue::registry::CueRegistry;
use inkue_lib::cue::traits::Cue;
use inkue_lib::cue::types::{ContinueMode, CueType, GroupMode};
use inkue_lib::engine::output_engine::{LayerStyle, SilentSpeedOrigin, VideoGeometry, VideoGeometryUpdate, VideoTranslation};
use inkue_lib::show::crossfade;
use inkue_lib::show::cue_list::CueList;
use inkue_lib::show::transport::{GoResult, Transport};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn visual(reg: &CueRegistry, cue_type: CueType, file: &str) -> Box<dyn Cue> {
    let mut json = reg.create(&cue_type).unwrap().serialize();
    json["file_path"] = serde_json::json!(file);
    reg.from_json(json).unwrap()
}

fn video(reg: &CueRegistry, file: &str) -> Box<dyn Cue> {
    visual(reg, CueType::Video, file)
}

/// A crossfading Fade with `stop_at_end` **off**: whatever it dissolves away
/// must still go, because a crossfade replaces its targets.
fn crossfade_fade(targets: Vec<Uuid>, into: Uuid, ms: u64) -> FadeCue {
    let mut fade = FadeCue::new();
    fade.target_cue_ids = targets;
    fade.crossfade_into = Some(into);
    fade.fade_duration_ms = ms;
    fade.stop_at_end = false;
    fade.set_continue_mode(ContinueMode::DoNotContinue);
    fade
}

fn list_of(cues: Vec<Box<dyn Cue>>) -> CueList {
    let mut list = CueList::new("T");
    for c in cues {
        list.push(c);
    }
    list
}

fn go(transport: &mut Transport, list: &mut CueList, id: Uuid) -> GoResult {
    list.playhead_cue_id = Some(id);
    transport.go(list).unwrap()
}

fn position_fade(target: Uuid, duration_ms: u64) -> FadeCue {
    let mut fade = FadeCue::new();
    fade.target_cue_ids = vec![target];
    fade.target_translation = Some(VideoTranslation { x: Some(0.37), y: None });
    fade.fade_duration_ms = duration_ms;
    fade.fade_volume = false;
    fade.fade_brightness = false;
    fade.set_continue_mode(ContinueMode::DoNotContinue);
    fade
}

#[test]
fn saturation_preview_resolves_only_the_selected_live_playback_and_saved_destination() {
    let registry = full_registry();
    let image = visual(&registry, CueType::Image, "rehearsal.png");
    let image_id = image.id();
    let mut fade = FadeCue::new();
    let fade_id = fade.id();
    fade.target_cue_ids = vec![image_id];
    fade.target_saturation = Some(0.0);
    let mut list = list_of(vec![image, Box::new(fade)]);
    let (context, _, calls) = recording_context();
    let mut transport = Transport::new(context.clone());

    let image = list.get_mut(&image_id).unwrap();
    image.preload(&context).unwrap();
    let staged = image.playing_voice_id().unwrap();
    assert!(crossfade::resolve_picture_saturation_preview(&list, fade_id, image_id, staged, 0.0).is_err());

    go(&mut transport, &mut list, image_id);
    let voice = list.get(&image_id).unwrap().playing_voice_id().unwrap();
    calls.lock().unwrap().clear();
    assert_eq!(crossfade::resolve_picture_saturation_preview(&list, fade_id, image_id, voice, 0.0).unwrap(), 0.0);
    assert!(crossfade::resolve_picture_saturation_preview(&list, fade_id, image_id, voice, 0.5).is_err());
    assert!(crossfade::resolve_picture_saturation_preview(&list, fade_id, Uuid::new_v4(), voice, 0.0).is_err());
    list.get_mut(&image_id).unwrap().set_disabled(true);
    assert!(crossfade::resolve_picture_saturation_preview(&list, fade_id, image_id, voice, 0.0).is_err());
    assert!(calls.lock().unwrap().is_empty());

    let image = list.get_mut(&image_id).unwrap();
    image.set_disabled(false);
    image.hard_stop(&context).unwrap();
    go(&mut transport, &mut list, image_id);
    assert_ne!(list.get(&image_id).unwrap().playing_voice_id(), Some(voice));
    assert!(crossfade::resolve_picture_saturation_preview(&list, fade_id, image_id, voice, 0.0).is_err());
}

fn translations(log: &CallLog) -> Vec<(Uuid, VideoTranslation)> {
    log.lock().unwrap().iter().filter_map(|call| match call {
        EngineCall::OutputGeometryFade { voice, update } => update.translation.map(|position| (*voice, position)),
        _ => None,
    }).collect()
}

fn opacity_writes(log: &CallLog) -> Vec<(Uuid, f32)> {
    log.lock().unwrap().iter().filter_map(|call| match call {
        EngineCall::OutputSetOpacity { voice, opacity } => Some((*voice, *opacity)),
        _ => None,
    }).collect()
}

fn rotations(log: &CallLog) -> Vec<(Uuid, f64)> {
    log.lock().unwrap().iter().filter_map(|call| match call {
        EngineCall::OutputGeometryFade { voice, update } => update.rotation_degrees.map(|angle| (*voice, angle)),
        _ => None,
    }).collect()
}

fn rotatable_image(reg: &CueRegistry, file: &str) -> Box<dyn Cue> {
    let mut json = visual(reg, CueType::Image, file).serialize();
    json["geometry"]["source_rotation_degrees"] = serde_json::json!(350.5);
    reg.from_json(json).unwrap()
}

fn rotation_fade(target: Uuid, duration_ms: u64) -> FadeCue {
    let mut fade = position_fade(target, duration_ms);
    fade.target_translation = None;
    fade.target_rotation_degrees = Some(10.5);
    fade
}

fn silent_movie(registry: &CueRegistry) -> Box<dyn Cue> {
    let mut json = video(registry, "silent.mp4").serialize();
    json["allow_silent_speed"] = serde_json::json!(true);
    json["cached_duration_ms"] = serde_json::json!(1);
    registry.from_json(json).unwrap()
}

fn movie_speed_fade(target: Uuid, duration_ms: u64) -> FadeCue {
    let mut fade = position_fade(target, duration_ms);
    fade.target_translation = None;
    fade.target_silent_speed = Some(2.0);
    fade
}

#[test]
fn rotation_waits_through_pause_and_fresh_png_playback_starts_at_the_saved_angle() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = rotatable_image(&registry, "centre.PNG");
    let mut fade = rotation_fade(target.id(), 0);
    fade.set_pre_wait(Duration::from_millis(15));
    let (target_id, fade_id) = (target.id(), fade.id());
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());

    go(&mut transport, &mut list, target_id);
    let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    go(&mut transport, &mut list, fade_id);
    transport.pause_cue(&mut list, &fade_id).unwrap();
    run_for(&mut list, &ctx, Duration::from_millis(30));
    assert!(rotations(&log).is_empty());
    transport.resume_cue(&mut list, &fade_id).unwrap();
    run_for(&mut list, &ctx, Duration::from_millis(50));
    assert_eq!(rotations(&log).last(), Some(&(voice, 10.5)));
    assert_eq!(list.get(&target_id).unwrap().visual_geometry().unwrap().source_rotation_degrees, Some(350.5));

    transport.hard_stop_cue(&mut list, &target_id).unwrap();
    go(&mut transport, &mut list, target_id);
    let fresh = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    assert_ne!(voice, fresh);
    let geometry = ctx.output_engine.begin_voice_rotation_fade(fresh, Uuid::new_v4()).unwrap();
    assert_eq!(geometry.source_rotation_degrees, Some(350.5));
}

#[test]
fn manual_geometry_takes_over_rotation_and_ineligible_targets_receive_no_fade_writes() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = rotatable_image(&registry, "centre.png");
    let target_id = target.id();
    let mut fade = rotation_fade(target_id, 10);
    fade.stop_at_end = true;
    let fade_id = fade.id();
    let invalid = rotatable_image(&registry, "legacy.bmp");
    let invalid_id = invalid.id();
    let mut invalid_fade = rotation_fade(invalid_id, 0);
    invalid_fade.fade_brightness = true;
    let invalid_fade_id = invalid_fade.id();
    let mut list = list_of(vec![target, Box::new(fade), invalid, Box::new(invalid_fade)]);
    let mut transport = Transport::new(ctx.clone());

    go(&mut transport, &mut list, target_id);
    let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    go(&mut transport, &mut list, fade_id);
    ctx.output_engine.apply_geometry(voice, &VideoGeometry {
        source_rotation_degrees: Some(22.5), ..Default::default()
    });
    run_for(&mut list, &ctx, Duration::from_millis(40));
    assert!(rotations(&log).is_empty());
    assert!(list.get_mut(&fade_id).unwrap().take_fade_stop_targets().is_empty());

    // A Fade already bound to the old voice must not write into a new GO.
    go(&mut transport, &mut list, fade_id);
    transport.hard_stop_cue(&mut list, &target_id).unwrap();
    go(&mut transport, &mut list, target_id);
    run_for(&mut list, &ctx, Duration::from_millis(30));
    assert!(rotations(&log).is_empty());
    assert!(list.get_mut(&fade_id).unwrap().take_fade_stop_targets().is_empty());

    go(&mut transport, &mut list, invalid_id);
    go(&mut transport, &mut list, invalid_fade_id);
    run_for(&mut list, &ctx, Duration::from_millis(20));
    assert!(rotations(&log).is_empty());
    assert!(opacity_writes(&log).is_empty());
}

#[test]
fn baked_jpeg_accepts_a_rotation_fade_on_its_active_voice() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = rotatable_image(&registry, "centre.JPEG");
    let target_id = target.id();
    let fade = rotation_fade(target_id, 0);
    let fade_id = fade.id();
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());

    go(&mut transport, &mut list, target_id);
    let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    go(&mut transport, &mut list, fade_id);
    run_for(&mut list, &ctx, Duration::from_millis(20));
    assert_eq!(rotations(&log).last(), Some(&(voice, 10.5)));
}

#[test]
fn silent_speed_fade_lands_after_paused_prewait_and_fresh_go_resets_rate() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = silent_movie(&registry);
    let target_id = target.id();
    let mut fade = movie_speed_fade(target_id, 0);
    fade.set_pre_wait(Duration::from_millis(10));
    let fade_id = fade.id();
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());
    go(&mut transport, &mut list, target_id);
    let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    go(&mut transport, &mut list, fade_id);
    let live_fade = list.get_mut(&fade_id).unwrap();
    live_fade.set_continue_mode(ContinueMode::AutoFollow);
    assert_eq!(live_fade.continue_mode(), ContinueMode::DoNotContinue);
    list.get_mut(&fade_id).unwrap().pause(&ctx).unwrap();
    run_for(&mut list, &ctx, Duration::from_millis(25));
    assert_eq!(count(&log, |call| matches!(call, EngineCall::OutputSilentSpeed { .. })), 0);
    // Media EOF is authoritative even though the saved duration has elapsed.
    assert!(!list.get(&target_id).unwrap().is_time_complete());

    list.get_mut(&fade_id).unwrap().resume(&ctx).unwrap();
    run_for(&mut list, &ctx, Duration::from_millis(40));
    assert!(log.lock().unwrap().iter().any(|call|
        matches!(call, EngineCall::OutputSilentSpeed { voice: id, rate } if *id == voice && *rate == 2.0)));
    assert!(list.get(&fade_id).unwrap().is_time_complete());
    assert_eq!(list.get(&fade_id).unwrap().continue_mode(), ContinueMode::DoNotContinue);

    list.get_mut(&target_id).unwrap().stop(&ctx).unwrap();
    go(&mut transport, &mut list, target_id);
    let fresh = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    assert_ne!(fresh, voice);
    assert_eq!(
        ctx.output_engine.claim_silent_speed(fresh, Uuid::new_v4()),
        Some(SilentSpeedOrigin::Ready(1.0)),
    );
}

#[test]
fn manual_rate_owner_supersedes_fade_and_unconfirmed_soundtrack_fails_closed() {
    let registry = full_registry();
    for (ctx, _rx, log, silent) in [
        { let (ctx, rx, log) = recording_context(); (ctx, rx, log, true) },
        { let (ctx, rx, log) = recording_context_with_video_audio(); (ctx, rx, log, false) },
    ] {
        let target = silent_movie(&registry);
        let target_id = target.id();
        let fade = movie_speed_fade(target_id, 0);
        let fade_id = fade.id();
        let mut list = list_of(vec![target, Box::new(fade)]);
        let mut transport = Transport::new(ctx.clone());
        go(&mut transport, &mut list, target_id);
        let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
        go(&mut transport, &mut list, fade_id);
        if silent {
            let owner = Uuid::new_v4();
            assert_eq!(
                ctx.output_engine.claim_silent_speed(voice, owner),
                Some(SilentSpeedOrigin::Ready(1.0)),
            );
            assert!(ctx.output_engine.set_silent_speed(voice, owner, 1.0));
        }
        log.lock().unwrap().clear();
        run_for(&mut list, &ctx, Duration::from_millis(20));
        assert_eq!(count(&log, |call| matches!(call, EngineCall::OutputSilentSpeed { .. })), 0);
        assert!(list.get(&fade_id).unwrap().is_time_complete());
    }
}

#[test]
fn a_zero_duration_position_fade_finishes_after_its_paused_prewait_and_keeps_the_other_axis() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = video(&registry, "moving.mp4");
    let target_id = target.id();
    let mut fade = position_fade(target_id, 0);
    fade.set_pre_wait(Duration::from_millis(15));
    let fade_id = fade.id();
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());
    go(&mut transport, &mut list, target_id);
    let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    go(&mut transport, &mut list, fade_id);
    list.get_mut(&fade_id).unwrap().pause(&ctx).unwrap();
    run_for(&mut list, &ctx, Duration::from_millis(30));
    assert!(translations(&log).is_empty());
    ctx.output_engine.apply_geometry(voice, &VideoGeometry { pan_y: -0.42, ..VideoGeometry::default() });
    list.get_mut(&fade_id).unwrap().resume(&ctx).unwrap();
    run_for(&mut list, &ctx, Duration::from_millis(60));
    let writes = translations(&log);
    assert_eq!(writes.last(), Some(&(voice, VideoTranslation { x: Some(0.37), y: None })));
    assert!(list.get(&fade_id).unwrap().is_time_complete());
    let live = ctx.output_engine.begin_voice_geometry_fade(voice, Uuid::new_v4()).unwrap();
    assert_eq!([live.pan_x, live.pan_y], [0.37, -0.42]);
    assert_eq!(list.get(&target_id).unwrap().visual_geometry().unwrap().pan_x, 0.0);
}

#[test]
fn a_manual_geometry_edit_preempts_the_motion_and_leaves_the_target_playing() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = video(&registry, "moving.mp4");
    let target_id = target.id();
    let mut fade = position_fade(target_id, 30);
    fade.stop_at_end = true;
    let fade_id = fade.id();
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());
    go(&mut transport, &mut list, target_id);
    let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    go(&mut transport, &mut list, fade_id);
    ctx.output_engine.apply_geometry(voice, &VideoGeometry { pan_x: -0.5, ..VideoGeometry::default() });
    run_for(&mut list, &ctx, Duration::from_millis(70));
    assert!(translations(&log).is_empty());
    assert!(list.get_mut(&fade_id).unwrap().take_fade_stop_targets().is_empty());
    let live = ctx.output_engine.begin_voice_geometry_fade(voice, Uuid::new_v4()).unwrap();
    assert_eq!([live.pan_x, live.pan_y], [-0.5, 0.0]);
}

#[test]
fn loop_path_keeps_the_voice_through_repeats_and_pause_until_stopped_or_replaced() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context_with_video_audio();
    let target = video(&registry, "loop-path.mp4");
    let target_id = target.id();
    let mut fade = position_fade(target_id, 20);
    fade.target_translation = Some(VideoTranslation { x: Some(0.5), y: Some(0.3) });
    fade.translation_waypoints = vec![[0.0, -0.5]];
    fade.smooth_translation_path = true;
    fade.loop_translation_path = true;
    let fade_id = fade.id();
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());
    go(&mut transport, &mut list, target_id);
    let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    go(&mut transport, &mut list, fade_id);
    run_for(&mut list, &ctx, Duration::from_millis(60));

    let fade = list.get_mut(&fade_id).unwrap();
    assert!(fade.action_elapsed() >= Duration::from_millis(40));
    assert!(!fade.is_time_complete());
    assert!(fade.take_fade_stop_targets().is_empty());
    assert_eq!(fade.duration(), None);
    assert_eq!(fade.file_duration(), Some(Duration::from_millis(20)));
    let writes = translations(&log);
    assert!(!writes.is_empty());
    assert!(writes.iter().all(|(owner, _)| *owner == voice));
    assert!(opacity_writes(&log).is_empty());
    assert!(!log.lock().unwrap().iter().any(|call| matches!(call, EngineCall::AudioSetGain { .. })));

    transport.pause_cue(&mut list, &fade_id).unwrap();
    let frozen = list.get(&fade_id).unwrap().action_elapsed();
    run_for(&mut list, &ctx, Duration::from_millis(25));
    assert_eq!(list.get(&fade_id).unwrap().action_elapsed(), frozen);
    assert_eq!(translations(&log), writes);
    transport.resume_cue(&mut list, &fade_id).unwrap();
    run_for(&mut list, &ctx, Duration::from_millis(5));
    assert!(translations(&log).len() > writes.len());

    transport.stop_cue(&mut list, &fade_id).unwrap();
    let stopped = translations(&log);
    run_for(&mut list, &ctx, Duration::from_millis(25));
    assert_eq!(translations(&log), stopped);
    assert_eq!(list.get(&target_id).unwrap().playing_voice_id(), Some(voice));
    assert!(crossfade::revert_picture_fade(&mut list, ctx.output_engine.as_ref(), fade_id).is_err());

    go(&mut transport, &mut list, fade_id);
    transport.hard_stop_cue(&mut list, &target_id).unwrap();
    go(&mut transport, &mut list, target_id);
    let fresh = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    assert_ne!(voice, fresh);
    run_for(&mut list, &ctx, Duration::from_millis(10));
    let fade = list.get_mut(&fade_id).unwrap();
    assert!(fade.is_time_complete());
    assert_eq!(fade.continue_mode(), ContinueMode::DoNotContinue);
    assert!(fade.take_fade_stop_targets().is_empty());
    assert_eq!(translations(&log), stopped);
}

#[test]
fn scale_fade_lands_exactly_and_a_new_picture_load_restores_the_saved_scale() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = video(&registry, "zoom.mp4");
    let target_id = target.id();
    let mut fade = position_fade(target_id, 0);
    fade.target_translation = None;
    fade.target_scale = Some(1.37);
    let fade_id = fade.id();
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());
    go(&mut transport, &mut list, target_id);
    let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    let current = VideoGeometry { pan_x: 0.2, pan_y: -0.32, scale: 0.5, ..VideoGeometry::default() };
    ctx.output_engine.apply_geometry(voice, &current);
    go(&mut transport, &mut list, fade_id);
    run_for(&mut list, &ctx, Duration::from_millis(20));

    let live = ctx.output_engine.begin_voice_geometry_fade(voice, Uuid::new_v4()).unwrap();
    assert_eq!(live, VideoGeometry { scale: 1.37, ..current });
    assert!(translations(&log).is_empty());
    assert_eq!(list.get(&target_id).unwrap().visual_geometry().unwrap().scale, 1.0);

    list.get_mut(&target_id).unwrap().stop(&ctx).unwrap();
    go(&mut transport, &mut list, target_id);
    let fresh = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    assert_ne!(fresh, voice);
    let geometry = ctx.output_engine.begin_voice_geometry_fade(fresh, Uuid::new_v4()).unwrap();
    assert_eq!(geometry, VideoGeometry::default());
}

#[test]
fn relative_picture_fade_binds_after_prewait_and_accumulates_until_fresh_go() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = video(&registry, "move-and-shrink.mp4");
    let target_id = target.id();
    let mut fade = position_fade(target_id, 0);
    fade.target_translation = None;
    fade.target_relative_geometry = Some(RelativeGeometry {
        translation: Some(VideoTranslation { x: Some(0.25), y: None }),
        scale_factor: Some(0.5),
    });
    fade.set_pre_wait(Duration::from_millis(15));
    let fade_id = fade.id();
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());
    go(&mut transport, &mut list, target_id);
    let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    go(&mut transport, &mut list, fade_id);
    list.get_mut(&fade_id).unwrap().pause(&ctx).unwrap();
    let current = VideoGeometry { pan_x: 0.125, pan_y: -0.25, scale: 0.75, ..Default::default() };
    ctx.output_engine.apply_geometry(voice, &current);
    run_for(&mut list, &ctx, Duration::from_millis(20));
    assert!(translations(&log).is_empty());
    list.get_mut(&fade_id).unwrap().resume(&ctx).unwrap();
    run_for(&mut list, &ctx, Duration::from_millis(50));

    let live = || ctx.output_engine.begin_voice_geometry_fade(voice, Uuid::new_v4()).unwrap();
    assert_eq!(live(), VideoGeometry { pan_x: 0.375, scale: 0.375, ..current });
    list.get_mut(&fade_id).unwrap().reset().unwrap();
    go(&mut transport, &mut list, fade_id);
    run_for(&mut list, &ctx, Duration::from_millis(50));
    assert_eq!(live(), VideoGeometry { pan_x: 0.625, scale: 0.1875, ..current });
    assert_eq!(list.get(&target_id).unwrap().visual_geometry().unwrap(), VideoGeometry::default());

    list.get_mut(&target_id).unwrap().stop(&ctx).unwrap();
    go(&mut transport, &mut list, target_id);
    let fresh = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    assert_ne!(fresh, voice);
    assert_eq!(ctx.output_engine.begin_voice_geometry_fade(fresh, Uuid::new_v4()),
        Some(VideoGeometry::default()));
}

#[test]
fn saturation_only_fade_keeps_geometry_and_returns_to_saved_colour_on_fresh_go() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = video(&registry, "colour.mp4");
    let target_id = target.id();
    let mut fade = position_fade(target_id, 0);
    fade.target_translation = None;
    fade.target_saturation = Some(0.25);
    let fade_id = fade.id();
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());
    go(&mut transport, &mut list, target_id);
    let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    let owner = Uuid::new_v4();
    assert!(ctx.output_engine.begin_voice_geometry_fade(voice, owner).is_some());

    go(&mut transport, &mut list, fade_id);
    ctx.output_engine.set_layer_props(voice, &LayerStyle { opacity: 0.7, ..LayerStyle::default() }, false);
    run_for(&mut list, &ctx, Duration::from_millis(20));
    assert!(log.lock().unwrap().iter().any(|call|
        matches!(call, EngineCall::OutputSaturationFade { voice: id, saturation }
            if *id == voice && *saturation == 0.25)));
    assert!(translations(&log).is_empty());
    assert!(opacity_writes(&log).is_empty());
    assert!(ctx.output_engine.set_voice_geometry_fade(voice, owner, Default::default()));
    assert_eq!(ctx.output_engine.begin_voice_saturation_fade(voice, Uuid::new_v4()), Some(0.25));

    list.get_mut(&target_id).unwrap().stop(&ctx).unwrap();
    go(&mut transport, &mut list, target_id);
    let fresh = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    assert_ne!(fresh, voice);
    assert_eq!(ctx.output_engine.begin_voice_saturation_fade(fresh, Uuid::new_v4()), Some(1.0));
}

#[test]
fn manual_geometry_and_saturation_edits_release_only_their_own_part_of_a_combined_fade() {
    let registry = full_registry();
    for edit_colour in [false, true] {
        let (ctx, _rx, log) = recording_context();
        let target = video(&registry, "colour.mp4");
        let target_id = target.id();
        let mut fade = position_fade(target_id, 0);
        fade.target_saturation = Some(0.0);
        let fade_id = fade.id();
        let mut list = list_of(vec![target, Box::new(fade)]);
        let mut transport = Transport::new(ctx.clone());
        go(&mut transport, &mut list, target_id);
        let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
        go(&mut transport, &mut list, fade_id);
        if edit_colour {
            ctx.output_engine.set_layer_props(voice, &LayerStyle::default(), true);
        } else {
            ctx.output_engine.apply_geometry(voice, &VideoGeometry::default());
        }
        run_for(&mut list, &ctx, Duration::from_millis(20));
        assert_eq!(!translations(&log).is_empty(), edit_colour);
        let expected = if edit_colour { 1.0 } else { 0.0 };
        assert_eq!(ctx.output_engine.begin_voice_saturation_fade(voice, Uuid::new_v4()), Some(expected));
    }
}

#[test]
fn picture_fade_revert_uses_the_original_live_voice_and_respects_later_owners() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = video(&registry, "colour.mp4");
    let target_id = target.id();
    let mut fade = position_fade(target_id, 0);
    fade.target_scale = Some(1.5);
    fade.target_saturation = Some(0.25);
    let fade_id = fade.id();
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());
    go(&mut transport, &mut list, target_id);
    let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    let original = VideoGeometry { pan_x: -0.25, pan_y: -0.5, scale: 0.75, ..Default::default() };
    ctx.output_engine.apply_geometry(voice, &original);
    ctx.output_engine.set_layer_props(voice, &LayerStyle { saturation: 0.75, ..LayerStyle::default() }, true);

    go(&mut transport, &mut list, fade_id);
    run_for(&mut list, &ctx, Duration::from_millis(20));
    log.lock().unwrap().clear();
    assert!(crossfade::revert_picture_fade(&mut list, ctx.output_engine.as_ref(), fade_id).is_err());
    assert!(log.lock().unwrap().is_empty());
    list.get_mut(&fade_id).unwrap().reset().unwrap();
    let result = crossfade::revert_picture_fade(&mut list, ctx.output_engine.as_ref(), fade_id).unwrap();
    assert!(result.geometry && result.saturation);
    assert_eq!(*log.lock().unwrap(), vec![
        EngineCall::OutputGeometryFade { voice, update: VideoGeometryUpdate {
            translation: Some(VideoTranslation { x: Some(-0.25), y: None }),
            scale: Some(0.75),
            rotation_degrees: None,
        }},
        EngineCall::OutputSaturationFade { voice, saturation: 0.75 },
    ]);
    assert!(crossfade::revert_picture_fade(&mut list, ctx.output_engine.as_ref(), fade_id).is_err());
    assert_eq!(list.get(&target_id).unwrap().visual_geometry(), Some(VideoGeometry::default()));

    go(&mut transport, &mut list, fade_id);
    run_for(&mut list, &ctx, Duration::from_millis(20));
    list.get_mut(&fade_id).unwrap().reset().unwrap();
    let edited = VideoGeometry { pan_y: 0.5, ..original };
    ctx.output_engine.apply_geometry(voice, &edited);
    log.lock().unwrap().clear();
    let result = crossfade::revert_picture_fade(&mut list, ctx.output_engine.as_ref(), fade_id).unwrap();
    assert!(!result.geometry && result.saturation);
    assert_eq!(*log.lock().unwrap(), vec![EngineCall::OutputSaturationFade { voice, saturation: 0.75 }]);

    go(&mut transport, &mut list, fade_id);
    run_for(&mut list, &ctx, Duration::from_millis(20));
    list.get_mut(&fade_id).unwrap().reset().unwrap();
    let later_owner = Uuid::new_v4();
    let saturation = ctx.output_engine.begin_voice_saturation_fade(voice, later_owner).unwrap();
    assert_eq!(saturation, 0.25);
    assert!(ctx.output_engine.set_voice_saturation_fade(voice, later_owner, 1.25));
    log.lock().unwrap().clear();
    let result = crossfade::revert_picture_fade(&mut list, ctx.output_engine.as_ref(), fade_id).unwrap();
    assert!(result.geometry && !result.saturation);
    assert_eq!(log.lock().unwrap().len(), 1);
    assert_eq!(ctx.output_engine.begin_voice_saturation_fade(voice, Uuid::new_v4()), Some(1.25));

    go(&mut transport, &mut list, fade_id);
    run_for(&mut list, &ctx, Duration::from_millis(20));
    list.get_mut(&fade_id).unwrap().reset().unwrap();
    list.get_mut(&target_id).unwrap().stop(&ctx).unwrap();
    go(&mut transport, &mut list, target_id);
    assert_ne!(list.get(&target_id).unwrap().playing_voice_id(), Some(voice));
    log.lock().unwrap().clear();
    assert!(crossfade::revert_picture_fade(&mut list, ctx.output_engine.as_ref(), fade_id).is_err());
    assert!(log.lock().unwrap().is_empty());
}

#[test]
fn a_position_fade_leaves_a_disarmed_visual_target_alone() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = video(&registry, "moving.mp4");
    let target_id = target.id();
    let mut fade = position_fade(target_id, 0);
    fade.fade_brightness = true;
    fade.stop_at_end = true;
    let fade_id = fade.id();
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());
    go(&mut transport, &mut list, target_id);
    list.get_mut(&target_id).unwrap().set_disabled(true);
    go(&mut transport, &mut list, fade_id);
    run_for(&mut list, &ctx, Duration::from_millis(15));
    assert!(translations(&log).is_empty());
    assert!(!log.lock().unwrap().iter().any(|call| matches!(call, EngineCall::OutputSetOpacity { .. })));
    assert!(list.get_mut(&fade_id).unwrap().take_fade_stop_targets().is_empty());
}

#[test]
fn combined_fade_binds_geometry_and_brightness_to_the_playback_after_prewait() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = video(&registry, "moving.mp4");
    let target_id = target.id();
    let mut fade = position_fade(target_id, 0);
    fade.set_pre_wait(Duration::from_millis(15));
    fade.fade_brightness = true;
    fade.target_brightness_pct = 0.0;
    let fade_id = fade.id();
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());
    go(&mut transport, &mut list, target_id);
    let first = list.get(&target_id).unwrap().playing_voice_id().unwrap();

    go(&mut transport, &mut list, fade_id);
    list.get_mut(&fade_id).unwrap().pause(&ctx).unwrap();
    list.get_mut(&target_id).unwrap().stop(&ctx).unwrap();
    go(&mut transport, &mut list, target_id);
    let second = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    assert_ne!(first, second);
    assert!(translations(&log).is_empty());
    assert!(opacity_writes(&log).is_empty());
    list.get_mut(&fade_id).unwrap().resume(&ctx).unwrap();
    run_for(&mut list, &ctx, Duration::from_millis(50));

    assert_eq!(translations(&log).last().map(|(voice, _)| *voice), Some(second));
    let opacity = opacity_writes(&log);
    assert_eq!(opacity.last(), Some(&(second, 0.0)));
    assert!(opacity.iter().all(|(voice, _)| *voice == second));
}

#[test]
fn combined_fade_leaves_every_parameter_alone_if_disarmed_during_prewait() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context_with_video_audio();
    let target = video(&registry, "moving.mp4");
    let target_id = target.id();
    let mut fade = position_fade(target_id, 0);
    fade.set_pre_wait(Duration::from_millis(15));
    fade.fade_brightness = true;
    fade.fade_volume = true;
    fade.target_pan = Some(-0.7);
    fade.stop_at_end = true;
    let fade_id = fade.id();
    let mut list = list_of(vec![target, Box::new(fade)]);
    let mut transport = Transport::new(ctx.clone());
    go(&mut transport, &mut list, target_id);
    go(&mut transport, &mut list, fade_id);
    list.get_mut(&target_id).unwrap().set_disabled(true);
    log.lock().unwrap().clear();
    run_for(&mut list, &ctx, Duration::from_millis(50));

    assert!(list.get(&fade_id).unwrap().is_time_complete());
    assert!(list.get_mut(&fade_id).unwrap().take_fade_stop_targets().is_empty());
    assert!(translations(&log).is_empty());
    assert!(opacity_writes(&log).is_empty());
    assert!(gains(&log).is_empty());
    assert_eq!(count(&log, |call| matches!(call, EngineCall::AudioSetPan { .. })), 0);
}

#[test]
fn combined_zero_prewait_fade_binds_companion_values_inside_a_group() {
    let registry = full_registry();
    let (ctx, _rx, log) = recording_context();
    let target = video(&registry, "moving.mp4");
    let target_id = target.id();
    let mut fade = position_fade(target_id, 0);
    fade.fade_brightness = true;
    fade.target_brightness_pct = 0.0;
    let mut group = GroupCue::new();
    group.mode = GroupMode::Simultaneous;
    group.children.push(Box::new(fade));
    let group_id = group.id();
    let mut list = list_of(vec![target, Box::new(group)]);
    let mut transport = Transport::new(ctx.clone());
    go(&mut transport, &mut list, target_id);
    let voice = list.get(&target_id).unwrap().playing_voice_id().unwrap();
    go(&mut transport, &mut list, group_id);
    run_for(&mut list, &ctx, Duration::from_millis(25));

    assert_eq!(translations(&log).last().map(|(id, _)| *id), Some(voice));
    assert_eq!(opacity_writes(&log).last(), Some(&(voice, 0.0)));
}

/// What the event loop does every tick for these cues: tick every running
/// cue, then push the crossfades that had to wait.
fn run_for(list: &mut CueList, ctx: &CueContext, duration: Duration) -> Vec<Uuid> {
    let mut started = Vec::new();
    let begin = Instant::now();
    loop {
        for cue in list.cues.iter_mut() {
            if cue.is_running() {
                let _ = cue.tick(ctx);
            }
        }
        started.extend(crossfade::advance_pending(list, ctx));
        let remaining = duration.saturating_sub(begin.elapsed());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(3)));
    }
    started
}

fn links(log: &CallLog) -> Vec<(u32, usize)> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|c| match c {
            EngineCall::OutputLinkCrossfade { duration_ms, outgoing } => Some((*duration_ms, *outgoing)),
            _ => None,
        })
        .collect()
}

fn gains(log: &CallLog) -> Vec<f32> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|c| match c {
            EngineCall::AudioSetGain { gain } => Some(*gain),
            _ => None,
        })
        .collect()
}

fn count(log: &CallLog, wanted: fn(&EngineCall) -> bool) -> usize {
    log.lock().unwrap().iter().filter(|c| wanted(c)).count()
}

fn fade_stops(list: &mut CueList, fade_id: Uuid) -> Vec<Uuid> {
    list.get_mut(&fade_id).unwrap().take_fade_stop_targets()
}

/// A, B and a Fade dissolving A into B; A is on screen.
struct Scene {
    list: CueList,
    transport: Transport,
    ctx: CueContext,
    log: CallLog,
    sim: CrossfadeSim,
    /// The incoming picture takes a while to appear (`sim.reveal_all()`).
    picture_waits: bool,
    a: Uuid,
    b: Uuid,
    fade: Uuid,
}

fn scene(fade_ms: u64, picture_waits: bool, video_audio: bool) -> Scene {
    let reg = full_registry();
    let (ctx, _rx, log, sim) = recording_context_with_crossfades(video_audio);
    let a = video(&reg, "video/a.mp4");
    let b = video(&reg, "video/b.mp4");
    let (a_id, b_id) = (a.id(), b.id());
    let fade = crossfade_fade(vec![a_id], b_id, fade_ms);
    let fade_id = fade.id();
    let mut transport = Transport::new(ctx.clone());
    let mut list = list_of(vec![a, b, Box::new(fade)]);
    go(&mut transport, &mut list, a_id);
    Scene { list, transport, ctx, log, sim, picture_waits, a: a_id, b: b_id, fade: fade_id }
}

impl Scene {
    fn go_fade(&mut self) -> GoResult {
        let result = go(&mut self.transport, &mut self.list, self.fade);
        if !self.picture_waits {
            self.sim.reveal_all();
        }
        result
    }

    fn action_elapsed(&self) -> Duration {
        self.list.get(&self.fade).unwrap().action_elapsed()
    }
}

// ---------------------------------------------------------------------------
// Starting and linking
// ---------------------------------------------------------------------------

#[test]
fn crossfade_starts_the_incoming_cue_and_reports_it_as_triggered() {
    let mut s = scene(1500, false, false);
    let result = s.go_fade();
    assert!(s.list.get(&s.b).unwrap().is_running(), "the Fade starts the cue it dissolves into");
    assert!(
        result.triggered.contains(&s.b),
        "the UI only refreshes a cue it is told about: {:?}",
        result.triggered,
    );
    assert_eq!(links(&s.log), vec![(1500, 1)], "one dissolve, with the fade's duration and one picture away");
}

#[test]
fn every_outgoing_picture_is_dissolved_away() {
    let reg = full_registry();
    let (ctx, _rx, log, _sim) = recording_context_with_crossfades(false);
    let a1 = video(&reg, "video/a1.mp4");
    let a2 = visual(&reg, CueType::Image, "img/a2.png");
    let b = video(&reg, "video/b.mp4");
    let (a1_id, a2_id, b_id) = (a1.id(), a2.id(), b.id());
    let fade = crossfade_fade(vec![a1_id, a2_id], b_id, 500);
    let fade_id = fade.id();
    let mut transport = Transport::new(ctx);
    let mut list = list_of(vec![a1, a2, b, Box::new(fade)]);
    go(&mut transport, &mut list, a1_id);
    go(&mut transport, &mut list, a2_id);
    go(&mut transport, &mut list, fade_id);

    assert_eq!(links(&log), vec![(500, 2)], "both targets leave in the same dissolve");
}

#[test]
fn a_crossfade_leaves_a_disarmed_incoming_visual_idle() {
    let reg = full_registry();
    let (ctx, _rx, log, _sim) = recording_context_with_crossfades(false);
    let mut desk = Transport::new(ctx);
    let outgoing = video(&reg, "out.mp4");
    let mut incoming = video(&reg, "in.mp4");
    incoming.set_disabled(true);
    let (out_id, in_id) = (outgoing.id(), incoming.id());
    let fade = crossfade_fade(vec![out_id], in_id, 30);
    let fade_id = fade.id();
    let mut list = list_of(vec![outgoing, incoming, Box::new(fade)]);
    go(&mut desk, &mut list, out_id);
    assert_eq!(go(&mut desk, &mut list, fade_id).triggered, [fade_id]);
    assert!(!list.get(&in_id).unwrap().is_running());
    assert!(links(&log).is_empty());
}

#[test]
fn a_crossfade_with_nothing_on_screen_still_brings_the_cue_in() {
    // The target is not playing: there is nothing to dissolve away, but the
    // incoming picture must still fade in over the fade time, not cut in.
    let reg = full_registry();
    let (ctx, _rx, log, _sim) = recording_context_with_crossfades(false);
    let a = video(&reg, "video/a.mp4");
    let b = video(&reg, "video/b.mp4");
    let fade = crossfade_fade(vec![a.id()], b.id(), 800);
    let fade_id = fade.id();
    let mut transport = Transport::new(ctx);
    let mut list = list_of(vec![a, b, Box::new(fade)]);
    go(&mut transport, &mut list, fade_id);

    assert_eq!(links(&log), vec![(800, 0)]);
}

#[test]
fn a_fade_without_crossfade_links_nothing() {
    let reg = full_registry();
    let (ctx, _rx, log) = recording_context();
    let a = video(&reg, "video/a.mp4");
    let a_id = a.id();
    let mut fade = FadeCue::new();
    fade.target_cue_ids = vec![a_id];
    let fade_id = fade.id();
    let mut transport = Transport::new(ctx);
    let mut list = list_of(vec![a, Box::new(fade)]);
    go(&mut transport, &mut list, a_id);
    go(&mut transport, &mut list, fade_id);

    assert!(links(&log).is_empty());
}

// ---------------------------------------------------------------------------
// The clock follows the picture
// ---------------------------------------------------------------------------

#[test]
fn the_fade_waits_for_the_incoming_picture() {
    let mut s = scene(40, true, false);
    s.go_fade();
    let ctx = s.ctx.clone();
    run_for(&mut s.list, &ctx, Duration::from_millis(60));
    assert_eq!(s.action_elapsed(), Duration::ZERO, "no picture yet: the dissolve has not started");
    assert!(fade_stops(&mut s.list, s.fade).is_empty(), "nor ended");

    s.sim.reveal_all();
    run_for(&mut s.list, &ctx, Duration::from_millis(80));
    assert!(s.action_elapsed() >= Duration::from_millis(40), "first frame: the fade runs");
    assert_eq!(fade_stops(&mut s.list, s.fade), vec![s.a], "and removes what it dissolved away");
}

#[test]
fn the_outgoing_picture_is_left_to_the_output_engine() {
    // Two clocks animating one layer would fight: the engine owns the picture.
    let mut s = scene(20, false, false);
    s.go_fade();
    let ctx = s.ctx.clone();
    run_for(&mut s.list, &ctx, Duration::from_millis(60));
    assert_eq!(count(&s.log, |c| matches!(c, EngineCall::OutputSetOpacity { .. })), 0);
}

#[test]
fn a_crossfade_removes_what_it_dissolved_away_even_without_stop_at_end() {
    let mut s = scene(20, false, false);
    s.go_fade();
    let ctx = s.ctx.clone();
    run_for(&mut s.list, &ctx, Duration::from_millis(60));
    let stops = fade_stops(&mut s.list, s.fade);
    assert_eq!(stops, vec![s.a], "a crossfade replaces its targets");
    assert!(!stops.contains(&s.b), "and never stops the cue it brought in");
}

#[test]
fn the_incoming_sound_rises_from_silence_and_the_outgoing_sound_falls_to_it() {
    let mut s = scene(20, false, true);
    s.go_fade();
    assert!(gains(&s.log).contains(&0.0), "the incoming sound starts silent, under the still-dark picture");
    let ctx = s.ctx.clone();
    run_for(&mut s.list, &ctx, Duration::from_millis(80));
    let all = gains(&s.log);
    assert!(all.iter().any(|g| (*g - 1.0).abs() < 1e-3), "the incoming sound reaches its level: {all:?}");
    let last_two: Vec<f32> = all.iter().rev().take(2).copied().collect();
    assert!(
        last_two.iter().any(|g| g.abs() < 1e-3),
        "and the outgoing sound lands on silence: {last_two:?}",
    );
}

// ---------------------------------------------------------------------------
// Falling back to a plain fade
// ---------------------------------------------------------------------------

#[test]
fn crossfading_into_a_cue_already_on_screen_falls_back_to_a_plain_fade() {
    // There is no entrance left to dissolve; restarting its fade from black
    // would flash it off and on.
    let mut s = scene(20, false, false);
    let b = s.b;
    go(&mut s.transport, &mut s.list, b);
    s.go_fade();
    assert!(links(&s.log).is_empty());
    let ctx = s.ctx.clone();
    run_for(&mut s.list, &ctx, Duration::from_millis(60));
    assert!(
        count(&s.log, |c| matches!(c, EngineCall::OutputSetOpacity { .. })) > 0,
        "the target is faded like an ordinary Fade",
    );
}

#[test]
fn crossfading_into_a_cue_that_is_not_visual_falls_back_to_a_plain_fade() {
    let reg = full_registry();
    let (ctx, _rx, log, _sim) = recording_context_with_crossfades(false);
    let a = video(&reg, "video/a.mp4");
    let memo = reg.create(&CueType::Memo).unwrap();
    let (a_id, memo_id) = (a.id(), memo.id());
    let fade = crossfade_fade(vec![a_id], memo_id, 30);
    let fade_id = fade.id();
    let mut transport = Transport::new(ctx.clone());
    let mut list = list_of(vec![a, memo, Box::new(fade)]);
    go(&mut transport, &mut list, a_id);
    let result = go(&mut transport, &mut list, fade_id);

    assert!(links(&log).is_empty());
    assert!(!result.triggered.contains(&memo_id), "and the Fade does not start it");
    run_for(&mut list, &ctx, Duration::from_millis(60));
    assert!(count(&log, |c| matches!(c, EngineCall::OutputSetOpacity { .. })) > 0);
}

#[test]
fn an_abandoned_crossfade_no_longer_stops_its_targets() {
    // The incoming cue left mid-way (stopped, failed): the outgoing picture is
    // back on screen, so the Fade must not cut it at its end.
    let mut s = scene(60, false, false);
    s.go_fade();
    let ctx = s.ctx.clone();
    run_for(&mut s.list, &ctx, Duration::from_millis(15));
    s.sim.vanish_all();
    run_for(&mut s.list, &ctx, Duration::from_millis(100));
    assert!(fade_stops(&mut s.list, s.fade).is_empty());
}

// ---------------------------------------------------------------------------
// Pre-waits
// ---------------------------------------------------------------------------

#[test]
fn an_incoming_cue_with_a_pre_wait_is_linked_once_it_has_a_picture() {
    let reg = full_registry();
    let (ctx, _rx, log, sim) = recording_context_with_crossfades(false);
    let a = video(&reg, "video/a.mp4");
    let mut b = video(&reg, "video/b.mp4");
    b.set_pre_wait(Duration::from_millis(40));
    let (a_id, b_id) = (a.id(), b.id());
    let fade = crossfade_fade(vec![a_id], b_id, 30);
    let fade_id = fade.id();
    let mut transport = Transport::new(ctx.clone());
    let mut list = list_of(vec![a, b, Box::new(fade)]);
    go(&mut transport, &mut list, a_id);
    let result = go(&mut transport, &mut list, fade_id);

    assert!(result.triggered.contains(&b_id), "the incoming cue starts on the Fade's GO");
    assert!(links(&log).is_empty(), "but has no picture during its pre-wait");
    run_for(&mut list, &ctx, Duration::from_millis(80));
    assert_eq!(links(&log), vec![(30, 1)], "linked as soon as its picture exists");
    sim.reveal_all();
    run_for(&mut list, &ctx, Duration::from_millis(80));
    assert_eq!(fade_stops(&mut list, fade_id), vec![a_id]);
}

#[test]
fn a_fade_with_a_pre_wait_starts_its_crossfade_after_it() {
    let reg = full_registry();
    let (ctx, _rx, log, sim) = recording_context_with_crossfades(false);
    let a = video(&reg, "video/a.mp4");
    let b = video(&reg, "video/b.mp4");
    let (a_id, b_id) = (a.id(), b.id());
    let mut fade = crossfade_fade(vec![a_id], b_id, 30);
    fade.set_pre_wait(Duration::from_millis(40));
    let fade_id = fade.id();
    let mut transport = Transport::new(ctx.clone());
    let mut list = list_of(vec![a, b, Box::new(fade)]);
    go(&mut transport, &mut list, a_id);
    let result = go(&mut transport, &mut list, fade_id);

    assert!(!list.get(&b_id).unwrap().is_running(), "nothing happens during the Fade's pre-wait");
    assert!(!result.triggered.contains(&b_id));
    let started = run_for(&mut list, &ctx, Duration::from_millis(80));
    assert_eq!(started, vec![b_id], "the event loop starts it when the pre-wait ends");
    assert_eq!(links(&log).len(), 1);
    sim.reveal_all();
    run_for(&mut list, &ctx, Duration::from_millis(80));
    assert_eq!(fade_stops(&mut list, fade_id), vec![a_id]);
}

// ---------------------------------------------------------------------------
// Pause, stop
// ---------------------------------------------------------------------------

#[test]
fn pausing_the_fade_holds_the_dissolve_and_resuming_releases_it() {
    let mut s = scene(500, false, false);
    s.go_fade();
    let ctx = s.ctx.clone();
    let fade = s.list.get_mut(&s.fade).unwrap();
    fade.pause(&ctx).unwrap();
    fade.resume(&ctx).unwrap();
    let holds: Vec<bool> = s
        .log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|c| match c {
            EngineCall::OutputHoldCrossfade { paused } => Some(*paused),
            _ => None,
        })
        .collect();
    assert_eq!(holds, vec![true, false], "picture and sound pause together");
}

#[test]
fn stopping_the_fade_releases_the_dissolve() {
    let mut s = scene(500, false, false);
    s.go_fade();
    let ctx = s.ctx.clone();
    s.list.get_mut(&s.fade).unwrap().stop(&ctx).unwrap();
    assert_eq!(count(&s.log, |c| matches!(c, EngineCall::OutputReleaseCrossfade)), 1);
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

fn group_of(children: Vec<Box<dyn Cue>>) -> GroupCue {
    let mut group = GroupCue::new();
    group.mode = GroupMode::Simultaneous;
    group.children = children;
    group
}

#[test]
fn a_group_holding_a_video_is_dissolved_as_a_whole() {
    let reg = full_registry();
    let (ctx, _rx, log, sim) = recording_context_with_crossfades(false);
    let group = group_of(vec![video(&reg, "video/a.mp4"), visual(&reg, CueType::Image, "img/logo.png")]);
    let b = video(&reg, "video/b.mp4");
    let (group_id, b_id) = (group.id(), b.id());
    let fade = crossfade_fade(vec![group_id], b_id, 20);
    let fade_id = fade.id();
    let mut transport = Transport::new(ctx.clone());
    let mut list = list_of(vec![Box::new(group), b, Box::new(fade)]);
    go(&mut transport, &mut list, group_id);
    go(&mut transport, &mut list, fade_id);

    assert_eq!(links(&log), vec![(20, 2)], "both pictures of the group dissolve away");
    sim.reveal_all();
    run_for(&mut list, &ctx, Duration::from_millis(60));
    assert_eq!(fade_stops(&mut list, fade_id), vec![group_id], "and the group goes as one");
}

#[test]
fn a_plain_fade_on_a_group_fades_its_pictures() {
    // Regression: a group's pictures were never faded — the transport only
    // looked for pictures on visual leaf cues.
    let reg = full_registry();
    let (ctx, _rx, log) = recording_context();
    let group = group_of(vec![video(&reg, "video/a.mp4")]);
    let group_id = group.id();
    let mut fade = FadeCue::new();
    fade.target_cue_ids = vec![group_id];
    fade.fade_duration_ms = 20;
    let fade_id = fade.id();
    let mut transport = Transport::new(ctx.clone());
    let mut list = list_of(vec![Box::new(group), Box::new(fade)]);
    go(&mut transport, &mut list, group_id);
    go(&mut transport, &mut list, fade_id);
    run_for(&mut list, &ctx, Duration::from_millis(60));

    assert!(count(&log, |c| matches!(c, EngineCall::OutputSetOpacity { .. })) > 0);
}

#[test]
fn the_incoming_cue_is_never_stopped_even_when_listed_as_a_target() {
    let mut s = scene(20, false, false);
    let (a, b, fade) = (s.a, s.b, s.fade);
    if let Some(cue) = s.list.get_mut(&fade) {
        let mut json = cue.serialize();
        json["target_cue_ids"] = serde_json::json!([a.to_string(), b.to_string()]);
        json["stop_at_end"] = serde_json::json!(true);
        let rebuilt = full_registry().from_json(json).unwrap();
        let index = s.list.index_of(&fade).unwrap();
        s.list.cues[index] = rebuilt;
    }
    s.go_fade();
    let ctx = s.ctx.clone();
    run_for(&mut s.list, &ctx, Duration::from_millis(60));
    assert_eq!(fade_stops(&mut s.list, s.fade), vec![a]);
}
