//! Zone (🔴 core) — Transport GO orchestration.
//!
//! Exercises `Transport::go` end-to-end: playhead advance, Auto-Follow /
//! Auto-Continue chaining, Stop-Cue action ordering, and Sequential-Group
//! hold/absorb. This is the show's trigger logic — previously untestable
//! because `CueContext` required concrete engines (an audio device + a GL/mpv
//! window). The `engine_traits` seam lets us inject inert engine doubles here,
//! so the logic runs with no hardware.
//!
//! MemoCue completes instantly in `go()` (Running → Completed); WaitCue stays
//! Running — that difference is what these scenarios lean on.

mod common;

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Receiver;

use common::{recording_context, full_registry, CallLog, EngineCall};
use inkue_lib::cue::context::{CueContext, CueEvent};
use inkue_lib::cue::devamp_cue::DevampCue;
use inkue_lib::cue::fade_cue::FadeCue;
use inkue_lib::cue::group_cue::GroupCue;
use inkue_lib::cue::registry::CueRegistry;
use inkue_lib::cue::traits::Cue;
use inkue_lib::cue::types::{ContinueMode, CueState, CueType, GroupMode};
use inkue_lib::show::cue_list::{CueList, CueListMode};
use inkue_lib::show::transport::Transport;

fn test_context() -> (CueContext, Receiver<CueEvent>) {
    let (ctx, rx, _log) = recording_context();
    (ctx, rx)
}

#[test]
fn wait_keeps_its_full_countdown_after_pre_wait() {
    let registry = full_registry();
    let mut cue = registry.create(&CueType::Wait).unwrap();
    cue.set_pre_wait(Duration::from_secs(30));
    let (context, _) = test_context();
    cue.go(&context).unwrap();
    cue.tick(&context).unwrap();
    assert!(!cue.is_action_started());
    assert_eq!(cue.action_elapsed(), Duration::ZERO);
    assert!(!cue.is_time_complete());
    cue.stop(&context).unwrap();
    assert!(!cue.is_action_started());
}

#[cfg(target_os = "linux")]
#[test]
fn camera_rechecks_the_local_input_before_leaving_pre_wait() {
    use inkue_lib::cue::{camera_cue::{CameraCue, CameraSource}, types::CueState};

    let (ctx, events, calls) = recording_context();
    let mut cue = CameraCue::new();
    cue.source = CameraSource::Url { url: "rtsp://rehearsal.invalid/live".into() };
    cue.set_pre_wait(Duration::from_secs(5));
    cue.go(&ctx).unwrap();
    assert_eq!(cue.state(), CueState::Running);
    assert!(!cue.is_action_started());

    let directory = tempfile::tempdir().unwrap();
    let unavailable = directory.path().join("camera");
    cue.source = CameraSource::Device { id: unavailable.to_string_lossy().into_owned(), name: "Camera".into() };
    cue.set_pre_wait(Duration::ZERO);
    cue.tick(&ctx).unwrap();
    assert_eq!(cue.state(), CueState::Standby);
    assert!(events.is_empty());
    assert!(calls.lock().unwrap().is_empty());

    assert!(cue.go(&ctx).unwrap_err().to_string().contains("is missing"));
    cue.set_pre_wait(Duration::from_secs(5));
    assert!(cue.go(&ctx).unwrap_err().to_string().contains("is missing"));
    assert_eq!(cue.state(), CueState::Standby);
    assert!(calls.lock().unwrap().is_empty());
}

fn memo(reg: &CueRegistry, name: &str, mode: ContinueMode, post_wait: Duration) -> Box<dyn Cue> {
    let mut c = reg.create(&CueType::Memo).unwrap();
    c.set_name(name.to_string());
    c.set_continue_mode(mode);
    c.set_post_wait(post_wait);
    c
}

fn list_of(cues: Vec<Box<dyn Cue>>) -> CueList {
    let mut list = CueList::new("T");
    for c in cues {
        list.push(c);
    }
    list
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Command cues — the transport performs their action on other cues
// ---------------------------------------------------------------------------

/// A Command cue of `cue_type` aimed at `targets`.
fn command(reg: &CueRegistry, cue_type: CueType, targets: Vec<uuid::Uuid>) -> Box<dyn Cue> {
    let mut json = reg.create(&cue_type).unwrap().serialize();
    json["target_cue_ids"] = serde_json::json!(
        targets.iter().map(|id| id.to_string()).collect::<Vec<_>>()
    );
    reg.from_json(json).unwrap()
}

/// Fire the Command cue sitting at the playhead of a freshly-built list.
fn go_command(list: &mut CueList) -> (Transport, ()) {
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);
    list.playhead_cue_id = list.cues.last().map(|c| c.id());
    transport.go(list).unwrap();
    (transport, ())
}

#[test]
fn a_cutoff_fade_go_uses_only_the_selected_filter() {
    check_effect_fade_go(false);
}

#[test]
fn an_eq_gain_fade_go_uses_only_the_selected_peaking_eq() {
    check_effect_fade_go(true);
}

fn check_effect_fade_go(eq_gain: bool) {
    let registry = full_registry();
    let filter_id = uuid::Uuid::new_v4();
    let mut data = registry.create(&CueType::Audio).unwrap().serialize();
    let effect = if eq_gain {
        serde_json::json!({"id":filter_id, "kind":"peak", "enabled":true, "frequency_hz":1000, "gain_db":0, "q":2})
    } else {
        serde_json::json!({"id":filter_id, "kind":"high_pass", "enabled":true, "frequency_hz":600})
    };
    data["audio_effects"] = serde_json::json!([effect]);
    let mut audio = registry.from_json(data).unwrap();
    audio.accept_preloaded_audio(Arc::new(vec![0.1; 9600]), 2, 48_000, Duration::from_millis(100));
    let audio_id = audio.id();
    let mut data = registry.create(&CueType::Fade).unwrap().serialize();
    data["target_cue_ids"] = serde_json::json!([audio_id]);
    if eq_gain {
        data["target_eq_gain"] = serde_json::json!({"filter_id":filter_id, "gain_db":6});
    } else {
        data["target_cutoff"] = serde_json::json!({"filter_id":filter_id, "frequency_hz":80});
    }
    data["fade_duration_ms"] = serde_json::json!(0);
    data["fade_volume"] = serde_json::json!(true);
    data["stop_at_end"] = serde_json::json!(true);
    let fade = registry.from_json(data).unwrap();
    let fade_id = fade.id();
    let mut list = list_of(vec![audio, fade]);
    list.playhead_cue_id = Some(audio_id);
    let (ctx, _, calls) = recording_context();
    let mut transport = Transport::new(ctx.clone());
    transport.go(&mut list).unwrap();
    let voice = list.get(&audio_id).unwrap().playing_voice_id().unwrap();
    calls.lock().unwrap().clear();
    transport.go(&mut list).unwrap();
    let fade = list.get_mut(&fade_id).unwrap();
    fade.tick(&ctx).unwrap();
    assert!(fade.is_time_complete());
    assert!(fade.take_fade_stop_targets().is_empty());
    let expected = if eq_gain {
        EngineCall::AudioReadEqGain { voice, filter_id }
    } else {
        EngineCall::AudioReadFilter { voice, filter_id }
    };
    assert_eq!(*calls.lock().unwrap(), vec![expected]);

    list.get_mut(&audio_id).unwrap().stop(&ctx).unwrap();
    calls.lock().unwrap().clear();
    list.playhead_cue_id = Some(fade_id);
    transport.go(&mut list).unwrap();
    let fade = list.get_mut(&fade_id).unwrap();
    fade.tick(&ctx).unwrap();
    assert_eq!(fade.is_time_complete(), !eq_gain);
    assert_eq!(fade.pending_start_admission(), eq_gain);
    assert!(fade.take_fade_stop_targets().is_empty());
    assert!(calls.lock().unwrap().is_empty(), "an inactive target triggers no other fade controls");
}

#[test]
fn a_missing_eq_holds_top_level_auto_continue_and_can_be_retried() {
    use inkue_lib::show::event_loop::schedule_list_follows;
    use std::time::Instant;

    let registry = full_registry();
    let filter_id = uuid::Uuid::new_v4();
    let mut data = registry.create(&CueType::Audio).unwrap().serialize();
    data["audio_effects"] = serde_json::json!([
        {"id": filter_id, "kind": "peak", "enabled": true, "frequency_hz": 1000, "gain_db": 0, "q": 2}
    ]);
    let mut audio = registry.from_json(data).unwrap();
    audio.accept_preloaded_audio(Arc::new(vec![0.1; 9600]), 2, 48_000, Duration::from_millis(100));
    let audio_id = audio.id();
    let mut fade = FadeCue::new();
    fade.target_cue_ids = vec![audio_id];
    fade.target_eq_gain = Some(inkue_lib::cue::types::EqGainDestination { filter_id, gain_db: 6.0 });
    fade.set_continue_mode(ContinueMode::AutoContinue);
    fade.fade_duration_ms = 0;
    let fade_id = fade.id();
    let next = memo(&registry, "Next", ContinueMode::DoNotContinue, Duration::ZERO);
    let next_id = next.id();
    let mut list = list_of(vec![audio, Box::new(fade), next]);
    list.playhead_cue_id = Some(fade_id);
    let (ctx, _, _) = recording_context();
    let mut transport = Transport::new(ctx.clone());
    assert_eq!(transport.go(&mut list).unwrap().triggered, [fade_id]);
    assert_eq!(list.playhead_cue_id, Some(fade_id));
    let held = list.get_mut(&fade_id).unwrap();
    held.tick(&ctx).unwrap();
    assert!(held.pending_start_admission());
    assert!(!held.is_time_complete());
    assert!(!schedule_list_follows(&mut list, &[], &mut Default::default(), Instant::now()));

    transport.go_by_id(&mut list, &audio_id).unwrap();
    let go = transport.go(&mut list).unwrap();
    assert_eq!(go.triggered, [fade_id, next_id]);
}

#[test]
fn an_admitted_eq_releases_its_held_standby_once() {
    use inkue_lib::show::{crossfade::advance_pending, event_loop::release_admitted_playhead};

    let registry = full_registry();
    let filter_id = uuid::Uuid::new_v4();
    for leave_standby in [true, false] {
        let mut data = registry.create(&CueType::Audio).unwrap().serialize();
        data["audio_effects"] = serde_json::json!([
            {"id": filter_id, "kind": "peak", "enabled": true, "frequency_hz": 1000, "gain_db": 0, "q": 2}
        ]);
        let mut audio = registry.from_json(data).unwrap();
        audio.accept_preloaded_audio(Arc::new(vec![0.1; 9600]), 2, 48_000, Duration::from_millis(100));
        let mut fade = FadeCue::new();
        fade.target_cue_ids = vec![audio.id()];
        fade.target_eq_gain = Some(inkue_lib::cue::types::EqGainDestination { filter_id, gain_db: 6.0 });
        fade.set_pre_wait(Duration::from_millis(1));
        fade.set_continue_mode(ContinueMode::DoNotContinue);
        let fade_id = fade.id();
        let next = memo(&registry, "Next", ContinueMode::DoNotContinue, Duration::ZERO);
        let next_id = next.id();
        let mut list = list_of(vec![audio, Box::new(fade), next]);
        let (ctx, _, _) = recording_context();
        let mut transport = Transport::new(ctx.clone());
        transport.go(&mut list).unwrap();
        transport.go(&mut list).unwrap();
        assert_eq!(list.playhead_cue_id, Some(fade_id));
        assert!(!release_admitted_playhead(&mut list));

        if leave_standby {
            list.set_playhead(Some(next_id)).unwrap();
        }
        std::thread::sleep(Duration::from_millis(3));
        list.get_mut(&fade_id).unwrap().tick(&ctx).unwrap();
        advance_pending(&mut list, &ctx);
        assert_eq!(release_admitted_playhead(&mut list), !leave_standby);
        assert_eq!(list.playhead_cue_id, Some(next_id));

        list.set_playhead(Some(fade_id)).unwrap();
        assert!(!release_admitted_playhead(&mut list));
        assert_eq!(list.playhead_cue_id, Some(fade_id));
        assert!(list.get(&fade_id).unwrap().is_running());
    }
}

#[test]
fn a_peaking_eq_bypassed_during_prewait_holds_the_fade() {
    use inkue_lib::commands::cue_cmds::{apply_cue_properties, MergePolicy};
    use inkue_lib::show::{crossfade::advance_pending, event_loop::schedule_list_follows, Workspace};
    use std::time::Instant;

    let registry = full_registry();
    let filter_id = uuid::Uuid::new_v4();
    let effects = serde_json::json!([
        {"id": filter_id, "kind": "peak", "enabled": true, "frequency_hz": 1000, "gain_db": 0, "q": 2}
    ]);
    let mut data = registry.create(&CueType::Audio).unwrap().serialize();
    data["audio_effects"] = effects.clone();
    let mut audio = registry.from_json(data).unwrap();
    audio.accept_preloaded_audio(Arc::new(vec![0.1; 9600]), 2, 48_000, Duration::from_millis(100));
    let audio_id = audio.id();
    let mut fade = FadeCue::new();
    fade.target_cue_ids = vec![audio_id];
    fade.target_eq_gain = Some(inkue_lib::cue::types::EqGainDestination { filter_id, gain_db: 6.0 });
    fade.set_pre_wait(Duration::from_millis(1));
    fade.set_continue_mode(ContinueMode::AutoFollow);
    fade.fade_duration_ms = 0;
    let fade_id = fade.id();
    let mut ws = Workspace::new("Pre-wait EQ admission");
    let list = ws.active_cue_list_mut().unwrap();
    list.push(audio);
    list.push(Box::new(fade));
    list.push(memo(&registry, "Next", ContinueMode::DoNotContinue, Duration::ZERO));
    let (ctx, _, calls) = recording_context();
    let mut transport = Transport::new(ctx.clone());
    transport.go(&mut *list).unwrap();
    transport.go(&mut *list).unwrap();
    assert_eq!(list.playhead_cue_id, Some(fade_id));
    calls.lock().unwrap().clear();

    let mut bypassed = effects;
    bypassed[0]["enabled"] = serde_json::json!(false);
    assert!(apply_cue_properties(
        ctx.audio_engine.as_ref(), ctx.output_engine.as_ref(), &registry, &mut ws, audio_id,
        &serde_json::json!({"audio_effects": bypassed}), MergePolicy::KnownKeysOnly,
    ).unwrap());
    std::thread::sleep(Duration::from_millis(3));
    let list = ws.active_cue_list_mut().unwrap();
    list.get_mut(&fade_id).unwrap().tick(&ctx).unwrap();
    advance_pending(list, &ctx);
    let held = list.get(&fade_id).unwrap();
    assert!(held.holds_playhead());
    assert!(!held.is_time_complete());
    assert!(calls.lock().unwrap().is_empty());
    assert!(!schedule_list_follows(list, &[], &mut Default::default(), Instant::now()));
}

#[test]
fn an_unavailable_eq_after_group_prewait_holds_group_and_inner_sequence() {
    use inkue_lib::show::{crossfade::advance_pending, event_loop::schedule_list_follows};
    use std::time::Instant;

    let registry = full_registry();
    for (mode, continuation) in [
        (GroupMode::Sequential, ContinueMode::AutoFollow),
        (GroupMode::Playlist, ContinueMode::AutoContinue),
        (GroupMode::Simultaneous, ContinueMode::AutoFollow),
    ] {
        let audio = registry.create(&CueType::Audio).unwrap();
        let mut fade = FadeCue::new();
        fade.target_cue_ids = vec![audio.id()];
        fade.target_eq_gain = Some(inkue_lib::cue::types::EqGainDestination {
            filter_id: uuid::Uuid::new_v4(), gain_db: 6.0,
        });
        fade.set_continue_mode(continuation);
        fade.fade_duration_ms = 0;
        let fade_id = fade.id();
        let next = memo(&registry, "Inner next", ContinueMode::DoNotContinue, Duration::ZERO);
        let next_id = next.id();
        let mut group = GroupCue::new();
        group.mode = mode;
        if mode == GroupMode::Playlist {
            group.set_playlist_advance_playhead(false);
        }
        group.set_pre_wait(Duration::from_millis(1));
        group.set_continue_mode(ContinueMode::AutoContinue);
        group.children.push(Box::new(fade));
        if mode != GroupMode::Simultaneous {
            group.children.push(next);
        }
        let group_id = group.id();
        let outer_next = memo(&registry, "Outer next", ContinueMode::DoNotContinue, Duration::ZERO);
        let outer_next_id = outer_next.id();
        let mut list = list_of(vec![audio, Box::new(group), outer_next]);
        list.playhead_cue_id = Some(group_id);
        let (ctx, _, _) = recording_context();
        assert_eq!(Transport::new(ctx.clone()).go(&mut list).unwrap().triggered, [group_id]);
        assert_eq!(list.playhead_cue_id, Some(group_id));
        std::thread::sleep(Duration::from_millis(3));
        list.get_mut(&group_id).unwrap().tick(&ctx).unwrap();
        advance_pending(&mut list, &ctx);
        list.get_mut(&group_id).unwrap().tick(&ctx).unwrap();
        let fade = list.get_recursive(&fade_id).unwrap();
        assert!(fade.pending_start_admission());
        assert!(!fade.is_time_complete());
        let group = list.get(&group_id).unwrap();
        assert!(group.holds_playhead());
        assert!(!group.released_playhead());
        assert!(!group.is_complete());
        if mode != GroupMode::Simultaneous {
            assert_eq!(list.get_recursive(&next_id).unwrap().state(), inkue_lib::cue::types::CueState::Standby);
        }
        assert_eq!(list.get(&outer_next_id).unwrap().state(), inkue_lib::cue::types::CueState::Standby);
        assert!(!schedule_list_follows(&mut list, &[], &mut Default::default(), Instant::now()));
    }
}

#[test]
fn held_audio_go_keeps_standby_and_can_retry_without_skipping_the_cue() {
    use inkue_lib::engine::audio_engine::AudioEngine;
    let reg = full_registry();
    let mut audio = reg.create(&CueType::Audio).unwrap();
    audio.accept_preloaded_audio(Arc::new(vec![0.5; 4096]), 2, 48000, Duration::from_millis(42));
    let audio_id = audio.id();
    let next = memo(&reg, "Next", ContinueMode::DoNotContinue, Duration::ZERO);
    let next_id = next.id();
    let mut list = list_of(vec![audio, next]);
    let (mut offline, _, _) = recording_context();
    offline.audio_engine = AudioEngine::new_silent(&Default::default());
    let error = Transport::new(offline).go(&mut list).err().expect("missing output must hold GO");
    assert!(error.to_string().contains("Audio start held"));
    assert_eq!(list.playhead_cue_id, Some(audio_id));
    assert!(!list.get(&next_id).unwrap().is_running());
    let (online, _, _) = recording_context();
    let result = Transport::new(online).go(&mut list).unwrap();
    assert_eq!(result.triggered, vec![audio_id]);
    assert_eq!(list.playhead_cue_id, Some(next_id));
}

#[test]
fn strict_workspace_default_holds_audio_go_until_patch_is_repaired_or_opt_out() {
    use inkue_lib::engine::device_manager::OutputPatch;

    let reg = full_registry();
    let mut audio = reg.create(&CueType::Audio).unwrap();
    audio.accept_preloaded_audio(Arc::new(vec![0.5; 4096]), 2, 48_000, Duration::from_millis(42));
    let audio_id = audio.id();
    let next = memo(&reg, "Next", ContinueMode::DoNotContinue, Duration::ZERO);
    let next_id = next.id();
    let mut list = list_of(vec![audio, next]);

    let (mut ctx, _, calls) = recording_context();
    let mut patch = OutputPatch::new("Backstage", "default", vec![0, 1]);
    patch.require_named_device = true;
    ctx.default_patch_id = Some(patch.id);
    ctx.output_patches = Arc::new(vec![patch]);
    let result = Transport::new(ctx.clone()).go(&mut list);
    assert!(result.err().expect("a default alias cannot be required").to_string().contains("Backstage"));
    assert_eq!(list.playhead_cue_id, Some(audio_id));
    assert!(calls.lock().unwrap().is_empty());

    Arc::make_mut(&mut ctx.output_patches)[0].require_named_device = false;
    let result = Transport::new(ctx).go(&mut list).unwrap();
    assert_eq!(result.triggered, vec![audio_id]);
    assert_eq!(list.playhead_cue_id, Some(next_id));
    assert!(calls.lock().unwrap().iter().any(|call| matches!(
        call, EngineCall::AudioPlayRouted { device: Some(id) } if id == "default"
    )));
}

#[test]
fn required_output_checks_before_and_after_prewait_without_chaining_on_loss() {
    use inkue_lib::cue::types::CueState;
    use inkue_lib::engine::device_manager::OutputPatch;
    use inkue_lib::show::event_loop::schedule_list_follows;
    use std::time::Instant;

    let reg = full_registry();
    let mut audio = reg.create(&CueType::Audio).unwrap();
    audio.accept_preloaded_audio(Arc::new(vec![0.5; 4096]), 2, 48000, Duration::from_millis(42));
    audio.set_pre_wait(Duration::from_millis(2));
    audio.set_continue_mode(ContinueMode::AutoContinue);
    let audio_id = audio.id();
    let next = memo(&reg, "Next", ContinueMode::DoNotContinue, Duration::ZERO);
    let next_id = next.id();
    let mut list = list_of(vec![audio, next]);
    let (mut ctx, _, calls) = recording_context();
    let mut patch = OutputPatch::new("Backstage", "default", vec![0, 1]);
    patch.require_named_device = true;
    ctx.default_patch_id = Some(patch.id);
    ctx.output_patches = Arc::new(vec![patch]);

    assert!(Transport::new(ctx.clone()).go(&mut list).is_err());
    assert_eq!(list.playhead_cue_id, Some(audio_id));
    assert_eq!(list.get(&audio_id).unwrap().state(), CueState::Standby);
    assert!(calls.lock().unwrap().is_empty());

    Arc::make_mut(&mut ctx.output_patches)[0].device_id = "hw:CARD=Venue".into();
    let result = Transport::new(ctx.clone()).go(&mut list).unwrap();
    assert_eq!(result.triggered, [audio_id]);
    assert_eq!(list.playhead_cue_id, Some(audio_id));
    assert!(matches!(calls.lock().unwrap().as_slice(), [EngineCall::AudioCheckNamedOutput { device }] if device == "hw:CARD=Venue"));
    assert!(!schedule_list_follows(&mut list, &[], &mut Default::default(), Instant::now()));

    Arc::make_mut(&mut ctx.output_patches)[0].device_id = "hw:CARD=Lobby".into();
    std::thread::sleep(Duration::from_millis(3));
    let audio = list.get_mut(&audio_id).unwrap();
    assert!(audio.tick(&ctx).unwrap_err().to_string().contains("changed during Pre-Wait"));
    assert!(audio.take_output_start_hold());
    assert_eq!(audio.state(), CueState::Standby);
    assert_eq!(list.playhead_cue_id, Some(audio_id));
    assert!(!list.get(&next_id).unwrap().is_running());
    assert!(!schedule_list_follows(&mut list, &[], &mut Default::default(), Instant::now()));
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[test]
fn advancing_groups_hold_a_required_audio_route_during_their_own_pre_wait() {
    use inkue_lib::engine::device_manager::OutputPatch;
    use inkue_lib::show::event_loop::release_admitted_playhead;

    let reg = full_registry();
    for mode in [GroupMode::StartFirst, GroupMode::StartRandom, GroupMode::Playlist] {
        let (mut ctx, _, calls) = recording_context();
        let mut patch = OutputPatch::new("Backstage", "default", vec![0, 1]);
        patch.require_named_device = true;
        ctx.default_patch_id = Some(patch.id);
        ctx.output_patches = Arc::new(vec![patch]);

        let mut group = GroupCue::new();
        group.mode = mode;
        group.set_pre_wait(Duration::from_secs(30));
        group.children.push(preloaded_audio(&reg));
        let group_id = group.id();
        let next = reg.create(&CueType::Wait).unwrap();
        let next_id = next.id();
        let mut list = list_of(vec![Box::new(group), next]);

        assert_eq!(Transport::new(ctx.clone()).go(&mut list).unwrap().triggered, [group_id]);
        assert_eq!(list.playhead_cue_id, Some(group_id), "{mode:?}");
        assert!(!release_admitted_playhead(&mut list));
        let group = list.get_mut(&group_id).unwrap();
        group.set_pre_wait(Duration::ZERO);
        let _ = group.tick(&ctx);
        assert!(group.has_withdrawn_output(), "{mode:?}");
        assert!(!release_admitted_playhead(&mut list));
        assert_eq!(list.playhead_cue_id, Some(group_id));
        assert!(calls.lock().unwrap().is_empty());

        let group = list.get_mut(&group_id).unwrap();
        group.reset().unwrap();
        group.set_pre_wait(Duration::from_secs(30));
        Arc::make_mut(&mut ctx.output_patches)[0].device_id = "hw:CARD=Venue".into();
        Transport::new(ctx.clone()).go(&mut list).unwrap();
        assert_eq!(list.playhead_cue_id, Some(group_id));
        let group = list.get_mut(&group_id).unwrap();
        group.set_pre_wait(Duration::ZERO);
        group.tick(&ctx).unwrap();
        assert!(release_admitted_playhead(&mut list));
        assert_eq!(list.playhead_cue_id, Some(next_id));
        assert!(!release_admitted_playhead(&mut list));
        assert!(!list.get(&next_id).unwrap().is_running());
        assert!(calls.lock().unwrap().iter().any(|call| matches!(
            call, EngineCall::AudioPlayRouted { device: Some(id) } if id == "hw:CARD=Venue"
        )));
    }
}

#[test]
fn advancing_groups_advance_during_pre_wait_when_the_selected_audio_patch_is_ordinary() {
    use inkue_lib::cue::audio_cue::AudioCue;
    use inkue_lib::engine::device_manager::OutputPatch;

    let reg = full_registry();
    for mode in [GroupMode::StartFirst, GroupMode::StartRandom, GroupMode::Playlist] {
        let (mut ctx, _, calls) = recording_context();
        let mut protected = OutputPatch::new("Backstage", "default", vec![0, 1]);
        protected.require_named_device = true;
        let ordinary = OutputPatch::new("Rehearsal", "default", vec![0, 1]);
        let mut audio = AudioCue::new();
        audio.output_patch_id = Some(ordinary.id);
        audio.accept_preloaded_audio(Arc::new(vec![0.5; 4096]), 2, 48_000, Duration::from_millis(42));
        ctx.default_patch_id = Some(protected.id);
        ctx.output_patches = Arc::new(vec![protected, ordinary]);

        let mut group = GroupCue::new();
        group.mode = mode;
        group.set_pre_wait(Duration::from_secs(30));
        group.children.push(Box::new(audio));
        let group_id = group.id();
        let next = reg.create(&CueType::Wait).unwrap();
        let next_id = next.id();
        let mut list = list_of(vec![Box::new(group), next]);

        assert_eq!(Transport::new(ctx.clone()).go(&mut list).unwrap().triggered, [group_id]);
        assert_eq!(list.playhead_cue_id, Some(next_id), "{mode:?}");
        assert!(calls.lock().unwrap().is_empty());
        let group = list.get_mut(&group_id).unwrap();
        group.set_pre_wait(Duration::ZERO);
        group.tick(&ctx).unwrap();
        assert!(calls.lock().unwrap().iter().any(|call| matches!(
            call, EngineCall::AudioPlayRouted { device: Some(id) } if id == "default"
        )));
    }
}

#[test]
fn required_output_prewait_keeps_patch_identity_and_channel_order_through_live_edit() {
    use inkue_lib::commands::cue_cmds::{apply_cue_properties, MergePolicy};
    use inkue_lib::cue::group_cue::GroupCue;
    use inkue_lib::cue::types::CueState;
    use inkue_lib::engine::device_manager::OutputPatch;
    use inkue_lib::show::{event_loop::schedule_list_follows, Workspace};
    use std::time::Instant;

    let reg = full_registry();
    for replace_patch in [false, true] {
        let mut ws = Workspace::new("Required audio");
        let mut patch = OutputPatch::new("Backstage", "hw:CARD=Venue", vec![0, 1]);
        patch.require_named_device = true;
        ws.default_output_patch_id = Some(patch.id);
        ws.output_patches.push(patch);

        let mut audio = reg.create(&CueType::Audio).unwrap();
        audio.accept_preloaded_audio(Arc::new(vec![0.5; 4096]), 2, 48_000, Duration::from_millis(42));
        audio.set_pre_wait(Duration::from_millis(2));
        let audio_id = audio.id();
        let mut group = GroupCue::new();
        group.set_continue_mode(ContinueMode::AutoContinue);
        group.children.push(audio);
        let group_id = group.id();
        let next = memo(&reg, "Next", ContinueMode::DoNotContinue, Duration::ZERO);
        let next_id = next.id();
        let list = ws.active_cue_list_mut().unwrap();
        list.push(Box::new(group));
        list.push(next);

        let (mut ctx, _, calls) = recording_context();
        ctx.default_patch_id = ws.default_output_patch_id;
        ctx.output_patches = Arc::new(ws.output_patches.clone());
        let result = Transport::new(ctx.clone()).go(ws.active_cue_list_mut().unwrap()).unwrap();
        assert_eq!(result.triggered, [group_id]);

        assert!(apply_cue_properties(
            ctx.audio_engine.as_ref(), ctx.output_engine.as_ref(), &reg, &mut ws, group_id,
            &serde_json::json!({"name": "Backstage sequence"}), MergePolicy::KnownKeysOnly,
        ).unwrap());
        let group = ws.active_cue_list().unwrap().get(&group_id).unwrap();
        assert_eq!(group.name(), "Backstage sequence");
        assert!(group.pending_start_admission());
        assert!(group.holds_playhead());

        if replace_patch {
            ws.output_patches[0].id = uuid::Uuid::new_v4();
            ws.default_output_patch_id = Some(ws.output_patches[0].id);
        } else {
            ws.output_patches[0].channels.swap(0, 1);
        }
        ctx.default_patch_id = ws.default_output_patch_id;
        ctx.output_patches = Arc::new(ws.output_patches.clone());
        std::thread::sleep(Duration::from_millis(3));
        let list = ws.active_cue_list_mut().unwrap();
        let group = list.get_mut(&group_id).unwrap();
        assert!(group.tick(&ctx).unwrap_err().to_string().contains("changed during Pre-Wait"));
        assert_eq!(list.get_recursive(&audio_id).unwrap().state(), CueState::Standby);

        assert!(apply_cue_properties(
            ctx.audio_engine.as_ref(), ctx.output_engine.as_ref(), &reg, &mut ws, group_id,
            &serde_json::json!({"notes": "Patch repair needed"}), MergePolicy::KnownKeysOnly,
        ).unwrap());
        let list = ws.active_cue_list_mut().unwrap();
        assert!(list.get(&group_id).unwrap().has_withdrawn_output());
        assert!(list.get_mut_recursive(&audio_id).unwrap().take_output_start_hold());
        assert_eq!(list.playhead_cue_id, Some(group_id));
        assert!(!list.get(&next_id).unwrap().is_running());
        assert!(!schedule_list_follows(list, &[], &mut Default::default(), Instant::now()));
        assert!(matches!(calls.lock().unwrap().as_slice(), [EngineCall::AudioCheckNamedOutput { .. }]));
    }
}

#[test]
fn required_output_prewait_holds_group_follow_until_the_child_is_admitted() {
    use inkue_lib::cue::group_cue::GroupCue;
    use inkue_lib::engine::device_manager::OutputPatch;
    use inkue_lib::show::event_loop::schedule_list_follows;
    use std::time::Instant;

    let reg = full_registry();
    for mode in [GroupMode::Simultaneous, GroupMode::Sequential, GroupMode::Playlist] {
        let mut audio = reg.create(&CueType::Audio).unwrap();
        audio.accept_preloaded_audio(Arc::new(vec![0.5; 4096]), 2, 48000, Duration::from_millis(42));
        audio.set_pre_wait(Duration::from_millis(1));
        let audio_id = audio.id();
        let mut group = GroupCue::new();
        group.mode = mode;
        if mode == GroupMode::Playlist {
            group.set_playlist_advance_playhead(false);
        }
        group.set_continue_mode(ContinueMode::AutoContinue);
        group.children.push(audio);
        let group_id = group.id();
        let next = memo(&reg, "Next", ContinueMode::DoNotContinue, Duration::ZERO);
        let mut list = list_of(vec![Box::new(group), next]);
        let (mut ctx, _, calls) = recording_context();
        let mut patch = OutputPatch::new("Backstage", "hw:CARD=Venue", vec![0, 1]);
        patch.require_named_device = true;
        ctx.default_patch_id = Some(patch.id);
        ctx.output_patches = Arc::new(vec![patch]);
        let result = Transport::new(ctx.clone()).go(&mut list).unwrap();
        assert_eq!(result.triggered, [group_id]);
        assert_eq!(list.playhead_cue_id, Some(group_id));
        assert!(!schedule_list_follows(&mut list, &[], &mut Default::default(), Instant::now()));
        std::thread::sleep(Duration::from_millis(2));

        let group = list.get_mut(&group_id).unwrap();
        group.tick(&ctx).unwrap();
        assert!(group.released_playhead());
        assert!(list.get_recursive(&audio_id).unwrap().playing_voice_id().is_some());
        assert!(matches!(calls.lock().unwrap().as_slice(), [
            EngineCall::AudioCheckNamedOutput { .. }, EngineCall::AudioPlayRouted { .. }
        ]));
        assert!(schedule_list_follows(&mut list, &[], &mut Default::default(), Instant::now()));
    }
}

#[test]
fn advancing_playlist_keeps_outer_go_independent_and_preserves_legacy_groups() {
    let reg = full_registry();
    for legacy in [false, true] {
        let (ctx, _) = test_context();
        let mut transport = Transport::new(ctx);
        let first = reg.create(&CueType::Wait).unwrap();
        let second = reg.create(&CueType::Wait).unwrap();
        let (first_id, second_id) = (first.id(), second.id());
        let mut group = GroupCue::new();
        group.mode = GroupMode::Playlist;
        group.set_playlist_loop(true);
        group.children = vec![first, second];
        let group_id = group.id();
        let mut saved = group.serialize();
        if legacy {
            saved.as_object_mut().unwrap().remove("playlist_advance_playhead");
        }
        let group = reg.from_json(saved).unwrap();
        assert_eq!(group.playlist_advance_playhead(), Some(!legacy));
        let outside = reg.create(&CueType::Wait).unwrap();
        let outside_id = outside.id();
        let mut list = list_of(vec![group, outside]);

        assert_eq!(transport.go(&mut list).unwrap().triggered, [group_id]);
        assert!(list.get_recursive(&first_id).unwrap().is_running());
        if legacy {
            assert_eq!(list.playhead_cue_id, Some(group_id));
            assert_eq!(transport.go(&mut list).unwrap().triggered, [group_id]);
            assert!(!list.get_recursive(&first_id).unwrap().is_running());
            assert!(list.get_recursive(&second_id).unwrap().is_running());
            assert_eq!(list.playhead_cue_id, Some(group_id));
        } else {
            assert_eq!(list.playhead_cue_id, Some(outside_id));
            assert!(list.set_playhead(Some(second_id)).is_err());
            transport.go_by_id(&mut list, &group_id).unwrap();
            assert_eq!(list.playhead_cue_id, Some(outside_id));
            assert!(!list.get_recursive(&second_id).unwrap().is_running());
            assert_eq!(transport.go(&mut list).unwrap().triggered, [outside_id]);
            assert!(list.get_recursive(&first_id).unwrap().is_running());
        }
    }
}

#[test]
fn advancing_playlist_second_trigger_keeps_exact_standby_in_both_directions() {
    let reg = full_registry();
    for (trigger, loops, expected) in [
        ("next", true, [2, 3, 0]),
        ("next", false, [2, 3, 3]),
        ("previous", true, [3, 2, 0]),
        ("previous", false, [0, 0, 0]),
        ("ignore", true, [0, 0, 0]),
        ("unknown-old-value", true, [0, 0, 0]),
    ] {
        let (context, _) = test_context();
        let mut transport = Transport::new(context);
        let mut children: Vec<_> = (0..4).map(|_| reg.create(&CueType::Wait).unwrap()).collect();
        children[1].set_disabled(true);
        let child_ids: Vec<_> = children.iter().map(|cue| cue.id()).collect();
        let mut group = GroupCue::new();
        group.mode = GroupMode::Playlist;
        group.set_playlist_loop(loops);
        group.children = children;
        let group_id = group.id();
        let mut saved = group.serialize();
        saved["playlist_second_trigger"] = trigger.into();
        let group = reg.from_json(saved).unwrap();
        let outside = reg.create(&CueType::Wait).unwrap();
        let manual = reg.create(&CueType::Wait).unwrap();
        let manual_id = manual.id();
        let mut list = list_of(vec![group, outside, manual]);

        transport.go(&mut list).unwrap();
        list.set_playhead(Some(manual_id)).unwrap();
        for index in expected {
            transport.go_by_id(&mut list, &group_id).unwrap();
            assert_eq!(list.playhead_cue_id, Some(manual_id), "{trigger}");
            for (child, id) in child_ids.iter().enumerate() {
                assert_eq!(list.get_recursive(id).unwrap().is_running(), child == index, "{trigger}");
            }
            assert!(!list.get(&manual_id).unwrap().is_running());
        }
    }
}

#[test]
fn advancing_playlist_second_trigger_refuses_an_unproven_route_before_stopping_current() {
    use inkue_lib::engine::device_manager::OutputPatch;

    let reg = full_registry();
    let (mut ctx, _, calls) = recording_context();
    let mut patch = OutputPatch::new("Unavailable venue", "default", vec![0, 1]);
    patch.require_named_device = true;
    ctx.default_patch_id = Some(patch.id);
    ctx.output_patches = Arc::new(vec![patch]);

    let mut group = GroupCue::new();
    group.mode = GroupMode::Playlist;
    let group_id = group.id();
    let first = reg.create(&CueType::Wait).unwrap();
    let first_id = first.id();
    let following = preloaded_audio(&reg);
    let following_id = following.id();
    group.children = vec![first, following];
    let mut saved = group.serialize();
    saved["playlist_second_trigger"] = "next".into();
    let mut group = reg.from_json(saved).unwrap();
    group.child_cues_mut().unwrap()[1].accept_preloaded_audio(
        Arc::new(vec![0.0; 48_000]), 1, 48_000, Duration::from_secs(1),
    );
    let manual = reg.create(&CueType::Wait).unwrap();
    let manual_id = manual.id();
    let mut list = list_of(vec![group, manual]);
    let mut transport = Transport::new(ctx);
    transport.go(&mut list).unwrap();
    let first_generation = list.get_recursive(&first_id).unwrap().play_generation();

    let error = transport.go_by_id(&mut list, &group_id).err().unwrap();
    assert!(error.to_string().contains("Second Trigger"));
    assert_eq!(list.playhead_cue_id, Some(manual_id));
    assert!(list.get_recursive(&first_id).unwrap().is_running());
    assert_eq!(list.get_recursive(&first_id).unwrap().play_generation(), first_generation);
    assert_eq!(list.get_recursive(&following_id).unwrap().state(), CueState::Standby);
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn advancing_playlist_keeps_exact_group_standby_on_an_addressed_rerun() {
    let reg = full_registry();
    let (context, _) = test_context();
    let mut transport = Transport::new(context);
    let mut group = GroupCue::new();
    group.mode = GroupMode::Playlist;
    let group_id = group.id();
    group.children = vec![reg.create(&CueType::Wait).unwrap(), reg.create(&CueType::Wait).unwrap()];
    let mut saved = group.serialize();
    saved["playlist_second_trigger"] = "next".into();
    let group = reg.from_json(saved).unwrap();
    let mut list = list_of(vec![group, reg.create(&CueType::Wait).unwrap()]);
    transport.go(&mut list).unwrap();
    list.set_playhead(Some(group_id)).unwrap();
    transport.pause_cue(&mut list, &group_id).unwrap();
    transport.go_by_id(&mut list, &group_id).unwrap();
    assert_eq!(list.playhead_cue_id, Some(group_id));
    assert!(!list.get(&group_id).unwrap().child_cues().unwrap()[1].is_running());

    transport.resume_cue(&mut list, &group_id).unwrap();
    transport.go_by_id(&mut list, &group_id).unwrap();
    assert_eq!(list.playhead_cue_id, Some(group_id));
    assert!(list.get(&group_id).unwrap().child_cues().unwrap()[1].is_running());
}

#[test]
fn advancing_playlist_keeps_standby_when_a_required_start_is_cancelled_and_retries_once() {
    use inkue_lib::engine::device_manager::OutputPatch;
    use inkue_lib::show::event_loop::release_admitted_playhead;

    let reg = full_registry();
    for group_wait in [Duration::ZERO, Duration::from_secs(30)] {
        let (mut ctx, _, calls) = recording_context();
        let mut patch = OutputPatch::new("Venue", "hw:CARD=Venue", vec![0, 1]);
        patch.require_named_device = true;
        ctx.default_patch_id = Some(patch.id);
        ctx.output_patches = Arc::new(vec![patch]);

        let mut first = preloaded_audio(&reg);
        first.set_pre_wait(Duration::from_secs(30));
        let first_id = first.id();
        let second = reg.create(&CueType::Wait).unwrap();
        let second_id = second.id();
        let mut group = GroupCue::new();
        group.mode = GroupMode::Playlist;
        group.set_pre_wait(group_wait);
        group.children = vec![first, second];
        let group_id = group.id();
        let outside = reg.create(&CueType::Wait).unwrap();
        let outside_id = outside.id();
        let mut list = list_of(vec![Box::new(group), outside]);

        Transport::new(ctx.clone()).go(&mut list).unwrap();
        assert_eq!(list.playhead_cue_id, Some(group_id));
        if !group_wait.is_zero() {
            assert!(calls.lock().unwrap().is_empty());
            let group = list.get_mut(&group_id).unwrap();
            group.set_pre_wait(Duration::ZERO);
            group.tick(&ctx).unwrap();
        }
        assert!(!release_admitted_playhead(&mut list));
        list.get_mut_recursive(&first_id).unwrap().stop(&ctx).unwrap();
        list.get_mut(&group_id).unwrap().tick(&ctx).unwrap();
        assert!(!release_admitted_playhead(&mut list));
        assert_eq!(list.playhead_cue_id, Some(group_id));
        assert!(!list.get_recursive(&second_id).unwrap().is_running());
        assert!(!list.get(&outside_id).unwrap().is_running());
        assert!(!calls.lock().unwrap().iter().any(|call| matches!(call, EngineCall::AudioPlayRouted { .. })));

        Transport::new(ctx.clone()).go(&mut list).unwrap();
        assert_eq!(list.playhead_cue_id, Some(group_id));
        list.get_mut_recursive(&first_id).unwrap().set_pre_wait(Duration::ZERO);
        list.get_mut(&group_id).unwrap().tick(&ctx).unwrap();
        assert!(release_admitted_playhead(&mut list));
        assert_eq!(list.playhead_cue_id, Some(outside_id));
        assert!(!release_admitted_playhead(&mut list));
        assert!(list.get_recursive(&first_id).unwrap().playing_voice_id().is_some());
    }
}

#[test]
fn advancing_playlist_does_not_move_outer_standby_when_a_later_route_fails() {
    use inkue_lib::engine::device_manager::OutputPatch;
    use inkue_lib::show::event_loop::release_admitted_playhead;

    let reg = full_registry();
    let (mut ctx, _, calls) = recording_context();
    let mut patch = OutputPatch::new("Venue", "default", vec![0, 1]);
    patch.require_named_device = true;
    ctx.default_patch_id = Some(patch.id);
    ctx.output_patches = Arc::new(vec![patch]);
    let mut group = GroupCue::new();
    group.mode = GroupMode::Playlist;
    let group_id = group.id();
    let third = reg.create(&CueType::Wait).unwrap();
    let third_id = third.id();
    group.children = vec![
        memo(&reg, "First", ContinueMode::DoNotContinue, Duration::ZERO),
        preloaded_audio(&reg),
        third,
    ];
    let outside = reg.create(&CueType::Wait).unwrap();
    let outside_id = outside.id();
    let mut list = list_of(vec![Box::new(group), outside]);

    Transport::new(ctx.clone()).go(&mut list).unwrap();
    assert_eq!(list.playhead_cue_id, Some(outside_id));
    assert!(list.get_mut(&group_id).unwrap().tick(&ctx).is_err());
    list.get_mut(&group_id).unwrap().tick(&ctx).unwrap();
    assert!(!release_admitted_playhead(&mut list));
    assert_eq!(list.playhead_cue_id, Some(outside_id));
    assert!(!list.get_recursive(&third_id).unwrap().is_running());
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn ordinary_hard_stop_preserves_both_dmx_latch_positions() {
    let (mut ctx, _, _) = recording_context();
    let engine = Arc::new(inkue_lib::engine::DmxEngine::new());
    ctx.dmx_engine = engine.clone();
    let mut transport = Transport::new(ctx);
    let mut list = list_of(vec![full_registry().create(&CueType::Wait).unwrap()]);
    for active in [false, true] {
        engine.set_blackout(active);
        transport.go(&mut list).unwrap();
        transport.hard_stop_all(&mut list).unwrap();
        assert!(!list.cues[0].is_running());
        assert_eq!(engine.is_blackout(), active);
        list.playhead_cue_id = Some(list.cues[0].id());
    }
}

#[test]
fn start_command_fires_its_target() {
    let reg = full_registry();
    let target = reg.create(&CueType::Wait).unwrap();
    let target_id = target.id();
    let mut list = list_of(vec![target, command(&reg, CueType::Start, vec![target_id])]);

    go_command(&mut list);

    assert!(
        list.get(&target_id).unwrap().is_running(),
        "a Start Cue must trigger its target",
    );
}

#[test]
fn goto_command_moves_the_playhead_to_its_target() {
    let reg = full_registry();
    let first = memo(&reg, "1", ContinueMode::DoNotContinue, Duration::ZERO);
    let first_id = first.id();
    let mut list = list_of(vec![
        first,
        memo(&reg, "2", ContinueMode::DoNotContinue, Duration::ZERO),
        command(&reg, CueType::Goto, vec![first_id]),
    ]);

    go_command(&mut list);

    assert_eq!(
        list.playhead_cue_id,
        Some(first_id),
        "a Goto Cue must leave the Playhead on its target, not advance past itself",
    );
}

#[test]
fn disarm_command_disables_its_target_and_arm_puts_it_back() {
    let reg = full_registry();
    let target = memo(&reg, "target", ContinueMode::DoNotContinue, Duration::ZERO);
    let target_id = target.id();

    let mut list = list_of(vec![target, command(&reg, CueType::Disarm, vec![target_id])]);
    go_command(&mut list);
    assert!(list.get(&target_id).unwrap().is_disabled(), "Disarm disables its target");

    // Same list, now with an Arm cue at the playhead.
    list.push(command(&reg, CueType::Arm, vec![target_id]));
    go_command(&mut list);
    assert!(!list.get(&target_id).unwrap().is_disabled(), "Arm re-enables it");
}

#[test]
fn target_redirects_goto_until_reset_and_preserves_the_saved_destination() {
    let reg = full_registry();
    let first = memo(&reg, "First", ContinueMode::DoNotContinue, Duration::ZERO);
    let first_id = first.id();
    let mut second = memo(&reg, "Second", ContinueMode::DoNotContinue, Duration::ZERO);
    second.set_number(Some("2".into()));
    let second_id = second.id();
    let goto = command(&reg, CueType::Goto, vec![first_id]);
    let goto_id = goto.id();
    let mut proposal = command(&reg, CueType::Target, vec![goto_id]).serialize();
    proposal["replacement_id"] = serde_json::json!(second_id);
    let mut list = list_of(vec![first, second, goto, reg.from_json(proposal).unwrap()]);
    let (mut desk, ()) = go_command(&mut list);
    let subject = list.get(&goto_id).unwrap();
    assert_eq!(subject.temporary_target(), Some(second_id));
    assert_eq!(subject.serialize()["target_cue_ids"], serde_json::json!([first_id]));
    desk.go_by_id(&mut list, &goto_id).unwrap();
    assert_eq!(list.playhead_cue_id, Some(second_id));
    list.push(command(&reg, CueType::Reset, vec![goto_id]));
    go_command(&mut list);
    desk.go_by_id(&mut list, &goto_id).unwrap();
    assert_eq!(list.playhead_cue_id, Some(first_id));
}

#[test]
fn pause_then_resume_commands_drive_a_running_target() {
    let reg = full_registry();
    let target = reg.create(&CueType::Wait).unwrap();
    let target_id = target.id();
    let mut list = list_of(vec![target, command(&reg, CueType::Pause, vec![target_id])]);

    // Start the target by hand, then let the Pause cue act on it.
    let (ctx, _rx) = test_context();
    list.get_mut(&target_id).unwrap().go(&ctx).unwrap();
    go_command(&mut list);
    assert!(list.get(&target_id).unwrap().is_paused(), "Pause pauses its target");

    list.push(command(&reg, CueType::Resume, vec![target_id]));
    go_command(&mut list);
    assert!(list.get(&target_id).unwrap().is_running(), "Resume puts it back to running");
}

#[test]
fn reset_command_returns_a_running_target_to_standby() {
    let reg = full_registry();
    let target = reg.create(&CueType::Wait).unwrap();
    let target_id = target.id();
    let mut list = list_of(vec![target, command(&reg, CueType::Reset, vec![target_id])]);

    let (ctx, _rx) = test_context();
    list.get_mut(&target_id).unwrap().go(&ctx).unwrap();
    go_command(&mut list);

    assert_eq!(
        list.get(&target_id).unwrap().state(),
        inkue_lib::cue::types::CueState::Standby,
        "Reset must stop the target and return it to Standby",
    );
}

#[test]
fn a_command_cue_never_acts_on_itself() {
    // A Start Cue targeting itself would recurse; the transport filters it out.
    let reg = full_registry();
    let mut list = CueList::new("T");
    let start = reg.create(&CueType::Start).unwrap();
    let real_id = start.id();
    // Aim it at its own id.
    let mut json = start.serialize();
    json["target_cue_ids"] = serde_json::json!(vec![real_id.to_string()]);
    list.push(reg.from_json(json).unwrap());

    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);
    list.playhead_cue_id = Some(real_id);

    let result = transport.go(&mut list).unwrap();

    assert_eq!(result.triggered, vec![real_id], "only the Command cue itself fired");
}

#[test]
fn load_command_prepares_a_video_without_putting_it_on_screen() {
    // The whole point of Load: the file is opened and decoded, but the output
    // stays dark until a Start releases it. Going and pausing — the naive
    // implementation — would show frame 0.
    let reg = full_registry();
    let (ctx, _rx, log) = recording_context();
    let mut transport = Transport::new(ctx);

    let mut vj = reg.create(&CueType::Video).unwrap().serialize();
    vj["file_path"] = serde_json::json!("video/act2.mp4");
    let video = reg.from_json(vj).unwrap();
    let video_id = video.id();
    let mut list = list_of(vec![video, command(&reg, CueType::Load, vec![video_id])]);
    let load_id = list.cues[1].id();
    list.playhead_cue_id = Some(load_id);

    let result = transport.go(&mut list).unwrap();
    assert_eq!(result.triggered, [load_id]);
    assert!(result.prepared);

    assert!(
        log.lock().unwrap().iter().any(|c| matches!(
            c,
            EngineCall::OutputShowContent { preload: true, .. }
        )),
        "Load must reach the output engine as a preload, not an ordinary show",
    );
    assert!(
        list.get(&video_id).unwrap().is_paused(),
        "a loaded cue stands by paused, ready to start",
    );
}

#[test]
fn load_leaves_camera_text_light_and_nested_group_outputs_untouched() {
    use inkue_lib::cue::{
        camera_cue::{CameraCue, CameraSource}, light_cue::{LightCue, ParamTarget},
        text_cue::TextCue, types::CueState,
    };
    use inkue_lib::engine::fixture::{builtin_fixture_types, PatchedFixture};

    let registry = full_registry();
    let fixture = PatchedFixture::new("Wash", 1, 1, builtin_fixture_types().remove(0));
    let mut light = LightCue::new();
    light.targets.push(ParamTarget::Fixture {
        fixture_id: fixture.id.to_string(), param_index: 0, value: 1.0,
    });
    let mut camera = CameraCue::new();
    camera.source = CameraSource::Url { url: "rtsp://rehearsal.invalid/live".into() };
    let mut text = TextCue::new();
    text.text = "Only on GO".into();
    let mut nested = GroupCue::new();
    nested.children.push(registry.from_json(camera.serialize()).unwrap());
    nested.children.push(registry.from_json(text.serialize()).unwrap());
    let mut group = GroupCue::new();
    group.children.push(Box::new(nested));

    let (context, _, calls) = common::recording_context_with(vec![], vec![fixture], vec![]);
    let mut transport = Transport::new(context);
    let targets: Vec<Box<dyn Cue>> = vec![Box::new(light), Box::new(camera), Box::new(text), Box::new(group)];
    for target in targets {
        let target_id = target.id();
        let load = command(&registry, CueType::Load, vec![target_id]);
        let load_id = load.id();
        let next = registry.create(&CueType::Wait).unwrap();
        let next_id = next.id();
        let mut list = list_of(vec![target, load, next]);
        list.playhead_cue_id = Some(load_id);

        let result = transport.go(&mut list).unwrap();
        assert_eq!(result.triggered, [load_id]);
        assert!(!result.prepared);
        assert_eq!(list.playhead_cue_id, Some(next_id));
        for child_id in list.all_cue_ids().into_iter().filter(|id| *id != load_id) {
            assert_eq!(list.get_recursive(&child_id).unwrap().state(), CueState::Standby);
        }
        assert!(calls.lock().unwrap().is_empty());
    }
}

#[test]
fn running_text_stays_visible_during_a_load_cue() {
    let registry = full_registry();
    let mut text = inkue_lib::cue::text_cue::TextCue::new();
    text.text = "Take your seats".into();
    let text_id = text.id();
    let load = command(&registry, CueType::Load, vec![text_id]);
    let load_id = load.id();
    let (context, _, calls) = recording_context();
    let mut transport = Transport::new(context);
    let mut list = list_of(vec![Box::new(text), load]);
    list.playhead_cue_id = Some(text_id);
    transport.go(&mut list).unwrap();
    assert_eq!(list.playhead_cue_id, Some(load_id));
    calls.lock().unwrap().clear();

    let result = transport.go(&mut list).unwrap();
    assert_eq!(result.triggered, [load_id]);
    assert!(calls.lock().unwrap().is_empty());
    assert!(list.get(&text_id).unwrap().is_running());
}

#[test]
fn starting_a_loaded_video_reveals_it_instead_of_reloading() {
    let reg = full_registry();
    let (ctx, _rx, log) = recording_context();
    let mut transport = Transport::new(ctx);

    let mut vj = reg.create(&CueType::Video).unwrap().serialize();
    vj["file_path"] = serde_json::json!("video/act2.mp4");
    let video = reg.from_json(vj).unwrap();
    let video_id = video.id();
    let mut list = list_of(vec![
        video,
        command(&reg, CueType::Load, vec![video_id]),
        command(&reg, CueType::Start, vec![video_id]),
    ]);

    // Load, then Start.
    list.playhead_cue_id = Some(list.cues[1].id());
    transport.go(&mut list).unwrap();
    list.playhead_cue_id = Some(list.cues[2].id());
    transport.go(&mut list).unwrap();

    let calls = log.lock().unwrap();
    let loads = calls.iter().filter(|c| matches!(c, EngineCall::OutputShowContent { .. })).count();
    assert_eq!(loads, 1, "starting a loaded cue must not load the file a second time");
    assert!(
        calls.iter().any(|c| matches!(c, EngineCall::OutputStartPreloaded)),
        "it releases the preload instead",
    );
    drop(calls);
    assert!(list.get(&video_id).unwrap().is_running(), "and the cue is running");
}

#[test]
fn audio_load_stages_a_muted_voice_and_start_releases_it_once() {
    let registry = full_registry();
    let audio = preloaded_audio(&registry);
    let audio_id = audio.id();
    let load = command(&registry, CueType::Load, vec![audio_id]);
    let load_id = load.id();
    let start = command(&registry, CueType::Start, vec![audio_id]);
    let start_id = start.id();
    let (context, events, calls) = recording_context();
    let mut transport = Transport::new(context.clone());
    let mut list = list_of(vec![audio, load, start]);
    list.playhead_cue_id = Some(load_id);

    let result = transport.go(&mut list).unwrap();
    assert_eq!(result.triggered, [load_id]);
    assert!(result.prepared);
    assert!(list.get(&audio_id).unwrap().is_paused());
    assert!(!list.get(&audio_id).unwrap().is_action_started());
    assert!(!list.get(&audio_id).unwrap().supports_seek());
    assert!(list.get(&audio_id).unwrap().all_voice_ids().is_empty());
    assert!(list.get(&audio_id).unwrap().live_audio_params().is_none());
    list.get_mut(&audio_id).unwrap().seek(2_000, &context);
    assert_eq!(list.get(&audio_id).unwrap().action_elapsed(), Duration::ZERO);
    assert_eq!(*calls.lock().unwrap(), [EngineCall::AudioPlayPausedRouted { device: None }]);
    assert!(!events.try_iter().any(|event| matches!(event, CueEvent::ActionStarted { cue_id } if cue_id == audio_id)));

    assert_eq!(transport.go(&mut list).unwrap().triggered, [start_id, audio_id]);
    assert!(list.get(&audio_id).unwrap().is_running());
    assert!(list.get(&audio_id).unwrap().supports_seek());
    assert_eq!(list.get(&audio_id).unwrap().all_voice_ids().len(), 1);
    assert_eq!(*calls.lock().unwrap(), [EngineCall::AudioPlayPausedRouted { device: None }, EngineCall::AudioResumeVoice]);
    assert!(events.try_iter().any(|event| matches!(event, CueEvent::ActionStarted { cue_id } if cue_id == audio_id)));
}

#[test]
fn load_to_time_starts_unsliced_audio_once_at_the_trimmed_offset() {
    use inkue_lib::cue::audio_cue::AudioCue;
    use inkue_lib::engine::device_manager::OutputPatch;

    let registry = full_registry();
    let mut audio = AudioCue::new();
    audio.start_time = Some(Duration::from_secs(1));
    audio.end_time = Some(Duration::from_secs(5));
    audio.accept_preloaded_audio(Arc::new(vec![0.0; 288_000]), 1, 48_000, Duration::from_secs(6));
    let audio_id = audio.id();
    let mut list = list_of(vec![Box::new(audio), command(&registry, CueType::Start, vec![audio_id])]);
    let start_id = list.cues[1].id();
    list.playhead_cue_id = Some(start_id);
    let (mut context, events, calls) = recording_context();
    let frames = common::record_audio_launch_positions(&mut context, &calls);
    let patch = OutputPatch::new("Rehearsal speakers", "default", vec![2, 3]);
    context.default_patch_id = Some(patch.id);
    context.output_patches = Arc::new(vec![patch]);
    let mut transport = Transport::new(context.clone());

    let cue = list.get_mut(&audio_id).unwrap();
    let saved = cue.serialize();
    let cue = (cue as &mut dyn std::any::Any).downcast_mut::<AudioCue>().unwrap();
    cue.load_to_time(-1250).unwrap();
    assert!(cue.load_to_time(4000).is_err(), "end is exclusive");
    assert_eq!(cue.loaded_time_ms(), Some(2750));
    assert_eq!(cue.serialize(), saved);
    assert!(calls.lock().unwrap().is_empty());
    assert!(events.is_empty());

    assert_eq!(transport.go(&mut list).unwrap().triggered, [start_id, audio_id]);
    let cue = list.get(&audio_id).unwrap();
    assert_eq!(cue.loaded_time_ms(), None);
    assert!(cue.action_elapsed() >= Duration::from_millis(2750));
    assert_eq!(*frames.lock().unwrap(), [180_000]);
    assert_eq!(*calls.lock().unwrap(), [EngineCall::AudioPlayRouted { device: Some("default".into()) }]);

    transport.hard_stop_cue(&mut list, &audio_id).unwrap();
    list.playhead_cue_id = Some(audio_id);
    transport.go(&mut list).unwrap();
    assert_eq!(*frames.lock().unwrap(), [180_000, 48_000]);
    let cue = list.get_mut(&audio_id).unwrap();
    cue.hard_stop(&context).unwrap();
    (cue as &mut dyn std::any::Any).downcast_mut::<AudioCue>().unwrap().load_to_time(1500).unwrap();
    transport.stop_all(&mut list).unwrap();
    assert_eq!(list.get(&audio_id).unwrap().loaded_time_ms(), None);
}

#[test]
fn a_timed_load_cue_prepares_exact_audio_silently_and_preserves_the_kickoff_point() {
    use inkue_lib::cue::audio_cue::AudioCue;

    let reg = full_registry();
    let mut audio = AudioCue::new();
    audio.start_time = Some(Duration::from_secs(1));
    audio.end_time = Some(Duration::from_secs(5));
    audio.accept_preloaded_audio(Arc::new(vec![0.1; 288_000]), 1, 48_000, Duration::from_secs(6));
    let audio_id = audio.id();
    let mut load = command(&reg, CueType::Load, vec![audio_id]).serialize();
    load["load_time_ms"] = serde_json::json!(-1250);
    let valid = reg.from_json(load.clone()).unwrap();
    let valid_id = valid.id();
    assert_eq!(valid.serialize()["load_time_ms"], -1250);
    load["id"] = serde_json::json!(uuid::Uuid::new_v4());
    load["load_time_ms"] = serde_json::json!(4000);
    let invalid = reg.from_json(load).unwrap();
    let invalid_id = invalid.id();
    let mut list = list_of(vec![valid, invalid, Box::new(audio)]);
    let (mut context, _, calls) = recording_context();
    let frames = common::record_audio_launch_positions(&mut context, &calls);
    let mut transport = Transport::new(context);

    let result = transport.go(&mut list).unwrap();
    assert_eq!(result.triggered, [valid_id]);
    assert!(result.prepared);
    assert_eq!(list.playhead_cue_id, Some(invalid_id));
    assert_eq!(list.get(&audio_id).unwrap().loaded_time_ms(), Some(2750));
    assert!(calls.lock().unwrap().is_empty());

    assert!(transport.go(&mut list).is_err());
    assert_eq!(list.playhead_cue_id, Some(invalid_id));
    assert_eq!(list.get(&audio_id).unwrap().loaded_time_ms(), Some(2750));
    assert!(calls.lock().unwrap().is_empty());

    list.set_playhead(Some(audio_id)).unwrap();
    assert_eq!(transport.go(&mut list).unwrap().triggered, [audio_id]);
    assert_eq!(*frames.lock().unwrap(), [180_000]);
    assert_eq!(list.get(&audio_id).unwrap().loaded_time_ms(), None);
    transport.hard_stop_cue(&mut list, &audio_id).unwrap();
    list.set_playhead(Some(audio_id)).unwrap();
    transport.go(&mut list).unwrap();
    assert_eq!(*frames.lock().unwrap(), [180_000, 48_000]);

    transport.hard_stop_cue(&mut list, &audio_id).unwrap();
    list.remove_anywhere(&invalid_id).unwrap();
    let valid = list.get_mut(&valid_id).unwrap();
    valid.set_continue_mode(ContinueMode::AutoContinue);
    valid.set_post_wait(Duration::from_millis(1));
    list.set_playhead(Some(valid_id)).unwrap();
    assert!(transport.go(&mut list).is_err());
    assert_eq!(*frames.lock().unwrap(), [180_000, 48_000]);
    list.get_mut(&valid_id).unwrap().set_post_wait(Duration::ZERO);
    let chained = transport.go(&mut list).unwrap();
    assert_eq!(chained.triggered, [valid_id, audio_id]);
    assert_eq!(*frames.lock().unwrap(), [180_000, 48_000, 180_000]);
}

#[test]
fn load_to_time_rejects_changed_media_and_grouping_clears_the_staged_point() {
    use inkue_lib::cue::audio_cue::AudioCue;

    let mut audio = AudioCue::new();
    let duration = Duration::from_secs(2);
    audio.accept_preloaded_audio(Arc::new(vec![0.0; 96_000]), 1, 48_000, duration);
    audio.rate = 0.5;
    assert!(audio.load_to_time(1000).is_err());
    audio.rate = 1.0;
    audio.load_to_time(1000).unwrap();
    audio.accept_preloaded_audio(Arc::new(vec![0.1; 96_000]), 1, 48_000, duration);
    let (context, events, calls) = recording_context();
    assert!(audio.go(&context).is_err());
    assert!(calls.lock().unwrap().is_empty());
    assert!(events.is_empty());
    assert_eq!(audio.loaded_time_ms(), Some(1000));
    audio.load_to_time(1300).unwrap();

    let id = audio.id();
    let mut list = list_of(vec![Box::new(audio)]);
    list.group_cues(&[id]).unwrap();
    assert_eq!(list.get_recursive(&id).unwrap().loaded_time_ms(), None);
}

#[test]
fn a_rejected_start_does_not_report_its_target_as_triggered() {
    let registry = full_registry();
    let audio = registry.create(&CueType::Audio).unwrap();
    let audio_id = audio.id();
    let start = command(&registry, CueType::Start, vec![audio_id]);
    let start_id = start.id();
    let next = registry.create(&CueType::Wait).unwrap();
    let next_id = next.id();
    let (context, _, calls) = recording_context();
    let mut list = list_of(vec![audio, start, next]);
    list.playhead_cue_id = Some(start_id);

    let result = Transport::new(context).go(&mut list).unwrap();
    assert_eq!(result.triggered, [start_id]);
    assert_eq!(list.playhead_cue_id, Some(next_id));
    assert_eq!(list.get(&audio_id).unwrap().state(), inkue_lib::cue::types::CueState::Standby);
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn prepared_audio_waits_for_its_pre_wait_after_start() {
    let registry = full_registry();
    let mut audio = preloaded_audio(&registry);
    audio.set_pre_wait(Duration::from_secs(30));
    let audio_id = audio.id();
    let load = command(&registry, CueType::Load, vec![audio_id]);
    let load_id = load.id();
    let start = command(&registry, CueType::Start, vec![audio_id]);
    let (context, _, calls) = recording_context();
    let mut transport = Transport::new(context.clone());
    let mut list = list_of(vec![audio, load, start]);
    list.playhead_cue_id = Some(load_id);
    transport.go(&mut list).unwrap();
    transport.go(&mut list).unwrap();

    let audio = list.get_mut(&audio_id).unwrap();
    audio.tick(&context).unwrap();
    assert!(audio.is_running());
    assert!(!audio.is_action_started());
    assert_eq!(audio.action_elapsed(), Duration::ZERO);
    assert_eq!(*calls.lock().unwrap(), [EngineCall::AudioPlayPausedRouted { device: None }]);
}

#[test]
fn repatching_a_prepared_audio_cue_discards_the_old_voice_before_start() {
    use inkue_lib::engine::device_manager::OutputPatch;

    let registry = full_registry();
    let audio = preloaded_audio(&registry);
    let audio_id = audio.id();
    let load = command(&registry, CueType::Load, vec![audio_id]);
    let load_id = load.id();
    let start = command(&registry, CueType::Start, vec![audio_id]);
    let (mut context, _, calls) = recording_context();
    let mut list = list_of(vec![audio, load, start]);
    list.playhead_cue_id = Some(load_id);
    Transport::new(context.clone()).go(&mut list).unwrap();

    let patch = OutputPatch::new("House", "default", vec![2, 3]);
    context.default_patch_id = Some(patch.id);
    context.output_patches = Arc::new(vec![patch]);
    Transport::new(context).go(&mut list).unwrap();
    assert_eq!(*calls.lock().unwrap(), [
        EngineCall::AudioPlayPausedRouted { device: None },
        EngineCall::AudioStopVoice { fade_ms: 0 },
        EngineCall::AudioPlayRouted { device: Some("default".into()) },
    ]);
}

#[test]
fn a_command_cue_with_no_target_is_a_harmless_no_op() {
    let reg = full_registry();
    let mut list = list_of(vec![
        memo(&reg, "1", ContinueMode::DoNotContinue, Duration::ZERO),
        command(&reg, CueType::Reset, vec![]),
    ]);

    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);
    list.playhead_cue_id = list.cues.last().map(|c| c.id());

    assert!(transport.go(&mut list).is_ok(), "an untargeted Command cue must not fail GO");
}

#[test]
fn go_on_empty_playhead_returns_empty() {
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);
    let mut list = CueList::new("empty");
    let result = transport.go(&mut list).unwrap();
    assert!(result.triggered.is_empty());
    assert!(result.stopped.is_empty());
}

#[test]
fn addressed_go_on_a_disarmed_mark_leaves_the_playhead_and_next_cue_alone() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut desk = Transport::new(ctx);
    let mut mark = reg.create(&CueType::Wait).unwrap();
    mark.set_disabled(true);
    let next = reg.create(&CueType::Wait).unwrap();
    let (mark_id, next_id) = (mark.id(), next.id());
    let mut list = list_of(vec![mark, next]);
    list.playhead_cue_id = None;
    assert!(desk.go_by_id(&mut list, &mark_id).unwrap().triggered.is_empty());
    assert_eq!(list.playhead_cue_id, None);
    assert!(!list.get(&next_id).unwrap().is_running());

    list.set_playhead(Some(mark_id)).unwrap();
    assert_eq!(desk.go(&mut list).unwrap().triggered, [next_id]);
}

#[test]
fn independent_start_keeps_standby_and_respects_arming_and_continue_mode() {
    let registry = full_registry();
    let (context, _events) = test_context();
    let transport = Transport::new(context);
    let standby = registry.create(&CueType::Wait).unwrap();
    let scheduled = registry.create(&CueType::Wait).unwrap();
    let following = registry.create(&CueType::Wait).unwrap();
    let (standby_id, scheduled_id, following_id) = (standby.id(), scheduled.id(), following.id());
    let mut list = list_of(vec![standby, scheduled, following]);
    list.set_playhead(Some(standby_id)).unwrap();

    list.get_mut(&scheduled_id).unwrap().set_disabled(true);
    assert!(transport.start_independent(&mut list, scheduled_id).unwrap().triggered.is_empty());
    list.get_mut(&scheduled_id).unwrap().set_disabled(false);
    list.get_mut(&scheduled_id).unwrap().set_continue_mode(ContinueMode::AutoFollow);
    assert!(transport.start_independent(&mut list, scheduled_id).unwrap().triggered.is_empty());
    list.get_mut(&scheduled_id).unwrap().set_continue_mode(ContinueMode::DoNotContinue);

    assert_eq!(transport.start_independent(&mut list, scheduled_id).unwrap().triggered, [scheduled_id]);
    assert!(transport.start_independent(&mut list, scheduled_id).unwrap().triggered.is_empty());
    assert!(list.get(&scheduled_id).unwrap().is_running());
    assert!(!list.get(&following_id).unwrap().is_running());
    assert_eq!(list.playhead_cue_id, Some(standby_id));
}

#[test]
fn selecting_standby_and_stopping_one_cue_leaves_other_playback_untouched() {
    let registry = full_registry();
    let (context, _events) = test_context();
    let mut transport = Transport::new(context);
    let mut first = registry.create(&CueType::Wait).unwrap();
    let mut second = registry.create(&CueType::Wait).unwrap();
    let standby = registry.create(&CueType::Wait).unwrap();
    first.set_continue_mode(ContinueMode::DoNotContinue);
    second.set_continue_mode(ContinueMode::DoNotContinue);
    let (first_id, second_id, standby_id) = (first.id(), second.id(), standby.id());
    let mut list = list_of(vec![first, second, standby]);

    transport.start_independent(&mut list, first_id).unwrap();
    transport.start_independent(&mut list, second_id).unwrap();
    transport.pause_cue(&mut list, &second_id).unwrap();
    list.set_playhead(Some(standby_id)).unwrap();
    assert!(list.get(&first_id).unwrap().is_running());
    assert!(list.get(&second_id).unwrap().is_paused());
    assert!(!list.get(&standby_id).unwrap().is_running());

    transport.stop_cue(&mut list, &second_id).unwrap();
    assert!(list.get(&first_id).unwrap().is_running());
    assert_eq!(list.get(&second_id).unwrap().state(), inkue_lib::cue::types::CueState::Standby);
    assert_eq!(list.playhead_cue_id, Some(standby_id));
}

#[test]
fn a_live_follow_edit_cannot_chain_a_scheduled_run() {
    use inkue_lib::show::event_loop::schedule_list_follows;
    use std::{collections::HashMap, time::Instant};

    let registry = full_registry();
    let (context, _events) = test_context();
    let mut transport = Transport::new(context);
    for mode in [ContinueMode::AutoContinue, ContinueMode::AutoFollow] {
        let standby = registry.create(&CueType::Wait).unwrap();
        let mut scheduled = registry.create(&CueType::Wait).unwrap();
        scheduled.set_continue_mode(ContinueMode::DoNotContinue);
        let following = registry.create(&CueType::Wait).unwrap();
        let (standby_id, scheduled_id, following_id) = (standby.id(), scheduled.id(), following.id());
        let mut list = list_of(vec![standby, scheduled, following]);
        list.set_playhead(Some(standby_id)).unwrap();
        assert_eq!(transport.start_independent(&mut list, scheduled_id).unwrap().triggered, [scheduled_id]);

        let old = list.get_mut(&scheduled_id).unwrap();
        let mut edited = registry.from_json(old.serialize()).unwrap();
        edited.set_continue_mode(mode);
        edited.inherit_playback(old);
        list.replace_cue_recursive(&scheduled_id, edited);

        let mut pending = HashMap::new();
        let now = Instant::now();
        assert!(!schedule_list_follows(&mut list, &[], &mut pending, now));
        list.get_mut(&scheduled_id).unwrap().reset().unwrap();
        assert!(!schedule_list_follows(&mut list, &[(scheduled_id, mode, Duration::ZERO)], &mut pending, now));
        assert!(pending.is_empty());
        assert_eq!(list.playhead_cue_id, Some(standby_id));
        assert!(!list.get(&standby_id).unwrap().is_running());

        list.set_playhead(Some(scheduled_id)).unwrap();
        let manual = transport.go(&mut list).unwrap();
        if mode == ContinueMode::AutoContinue {
            assert_eq!(manual.triggered, [scheduled_id, following_id]);
        } else {
            assert_eq!(manual.triggered, [scheduled_id]);
            assert!(schedule_list_follows(&mut list, &[(scheduled_id, mode, Duration::ZERO)], &mut pending, now));
        }
    }
}

#[test]
fn addressed_go_and_start_respect_a_disarmed_ancestor() {
    let reg = full_registry();
    for mode in [CueListMode::Sequential, CueListMode::Cart] {
        let (ctx, _rx) = test_context();
        let mut desk = Transport::new(ctx);
        let child = reg.create(&CueType::Wait).unwrap();
        let child_id = child.id();
        let mut inner = GroupCue::new();
        inner.children.push(child);
        inner.set_disabled(true);
        let mut outer = GroupCue::new();
        outer.children.push(Box::new(inner));
        outer.children.push(reg.create(&CueType::Wait).unwrap());
        let start = command(&reg, CueType::Start, vec![child_id]);
        let start_id = start.id();
        let mut list = list_of(vec![Box::new(outer), start]);
        list.set_mode(mode, None);
        let parked = list.playhead_cue_id;
        assert!(desk.go_by_id(&mut list, &child_id).unwrap().triggered.is_empty());
        assert_eq!(list.playhead_cue_id, parked);
        assert_eq!(desk.go_by_id(&mut list, &start_id).unwrap().triggered, [start_id]);
        assert!(!list.get_recursive(&child_id).unwrap().is_running());
    }
}

#[test]
fn go_triggers_playhead_cue_and_advances() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);

    let m1 = memo(&reg, "1", ContinueMode::DoNotContinue, Duration::ZERO);
    let m2 = memo(&reg, "2", ContinueMode::DoNotContinue, Duration::ZERO);
    let id1 = m1.id();
    let id2 = m2.id();
    let mut list = list_of(vec![m1, m2]);

    let result = transport.go(&mut list).unwrap();
    assert_eq!(result.triggered, vec![id1], "only the playhead cue fires (no continue)");
    assert_eq!(list.playhead_cue_id, Some(id2), "playhead advances to the next cue");
}

#[test]
fn independent_cart_navigation_and_continues_cannot_fire_neighboring_pads() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);
    let mut list = CueList::new("Pads");
    list.set_mode(CueListMode::Cart, None);
    let follow = memo(&reg, "Follow", ContinueMode::AutoFollow, Duration::ZERO);
    let cont = memo(&reg, "Continue", ContinueMode::AutoContinue, Duration::ZERO);
    let next = reg.create(&CueType::Wait).unwrap();
    let (follow_id, continue_id, next_id) = (follow.id(), cont.id(), next.id());
    list.push(follow);
    list.insert(1, cont);
    list.push(next);
    list.set_playhead(Some(next_id)).unwrap();
    assert_eq!(list.playhead_cue_id, None);
    assert!(transport.go(&mut list).unwrap().triggered.is_empty());
    for id in [follow_id, continue_id] {
        assert_eq!(transport.go_by_id(&mut list, &id).unwrap().triggered, vec![id]);
    }
    list.get_mut(&continue_id).unwrap().set_disabled(true);
    assert!(transport.go_by_id(&mut list, &continue_id).unwrap().triggered.is_empty());
    assert!(!list.get(&next_id).unwrap().is_running());
    assert_eq!(list.playhead_cue_id, None);
}

#[test]
fn independent_cart_runs_explicit_target_actions_without_advancing() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);
    let target = reg.create(&CueType::Wait).unwrap();
    let target_id = target.id();
    let start = command(&reg, CueType::Start, vec![target_id]);
    let start_id = start.id();
    let jump = command(&reg, CueType::Goto, vec![target_id]);
    let jump_id = jump.id();
    let mut list = list_of(vec![target, start, jump]);
    list.set_mode(CueListMode::Cart, None);
    assert_eq!(transport.go_by_id(&mut list, &start_id).unwrap().triggered, vec![start_id, target_id]);
    assert!(list.get(&target_id).unwrap().is_running());
    assert_eq!(transport.go_by_id(&mut list, &jump_id).unwrap().triggered, vec![jump_id]);
    assert_eq!(list.playhead_cue_id, None);
}

#[test]
fn saved_legacy_cart_keeps_chaining_until_operator_switches_and_saves() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);
    let first = memo(&reg, "Follow", ContinueMode::AutoFollow, Duration::ZERO);
    let next = reg.create(&CueType::Wait).unwrap();
    let (first_id, next_id) = (first.id(), next.id());
    let mut saved = list_of(vec![first, next]).to_json();
    saved["mode"] = serde_json::json!("cart");
    saved.as_object_mut().unwrap().remove("chain_cart_cues");
    let mut legacy = CueList::from_json(saved, &reg).unwrap();
    assert!(legacy.chain_cart_cues);
    assert_eq!(legacy.playhead_cue_id, Some(first_id));
    assert_eq!(transport.go_by_id(&mut legacy, &first_id).unwrap().triggered, vec![first_id, next_id]);
    assert_eq!(legacy.to_json()["chain_cart_cues"], true);

    legacy.set_mode(CueListMode::Cart, Some(false));
    let mut reloaded = CueList::from_json(legacy.to_json(), &reg).unwrap();
    assert!(reloaded.independent_pads());
    assert_eq!(transport.go_by_id(&mut reloaded, &first_id).unwrap().triggered, vec![first_id]);
    assert_eq!(reloaded.playhead_cue_id, None);
    reloaded.set_mode(CueListMode::Sequential, None);
    assert_eq!(reloaded.playhead_cue_id, Some(first_id));
}

#[test]
fn auto_follow_chains_through_instant_cues() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);

    let m1 = memo(&reg, "1", ContinueMode::AutoFollow, Duration::ZERO);
    let m2 = memo(&reg, "2", ContinueMode::AutoFollow, Duration::ZERO);
    let m3 = memo(&reg, "3", ContinueMode::DoNotContinue, Duration::ZERO);
    let (id1, id2, id3) = (m1.id(), m2.id(), m3.id());
    let mut list = list_of(vec![m1, m2, m3]);

    let result = transport.go(&mut list).unwrap();
    assert_eq!(
        result.triggered,
        vec![id1, id2, id3],
        "Auto-Follow chains through the two instant cues and stops at DoNotContinue"
    );
    assert_eq!(list.playhead_cue_id, None, "playhead ends past the last cue");
}

#[test]
fn auto_continue_with_zero_postwait_chains_once() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);

    let m1 = memo(&reg, "1", ContinueMode::AutoContinue, Duration::ZERO);
    let m2 = memo(&reg, "2", ContinueMode::DoNotContinue, Duration::ZERO);
    let (id1, id2) = (m1.id(), m2.id());
    let mut list = list_of(vec![m1, m2]);

    let result = transport.go(&mut list).unwrap();
    assert_eq!(result.triggered, vec![id1, id2], "zero post-wait Auto-Continue fires the next cue now");
}

#[test]
fn auto_continue_with_nonzero_postwait_defers_the_chain() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);

    // Non-zero post-wait: the chain is the event loop's job, not this synchronous GO.
    let m1 = memo(&reg, "1", ContinueMode::AutoContinue, Duration::from_millis(100));
    let m2 = memo(&reg, "2", ContinueMode::DoNotContinue, Duration::ZERO);
    let id1 = m1.id();
    let mut list = list_of(vec![m1, m2]);

    let result = transport.go(&mut list).unwrap();
    assert_eq!(result.triggered, vec![id1], "a post-wait > 0 must not chain within the same GO");
}

#[test]
fn stop_cue_stops_running_cues_and_reports_them() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);

    // A Wait cue stays Running after GO; a default Stop cue (empty targets)
    // stops everything running when it fires.
    let wait = reg.create(&CueType::Wait).unwrap();
    let stop = reg.create(&CueType::Stop).unwrap();
    let wait_id = wait.id();
    let mut list = list_of(vec![wait, stop]);

    // GO #1 starts the Wait (now Running).
    transport.go(&mut list).unwrap();
    assert!(list.get(&wait_id).unwrap().is_running(), "wait cue should be running after GO");

    // GO #2 fires the Stop cue, which stops the running Wait.
    let result = transport.go(&mut list).unwrap();
    assert!(
        result.stopped.contains(&wait_id),
        "the Stop cue must report the wait it stopped, got {:?}",
        result.stopped
    );
    assert!(!list.get(&wait_id).unwrap().is_running(), "the wait cue must no longer be running");
}

#[test]
fn stop_cue_with_specific_target_spares_the_others() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);

    let wait_a = reg.create(&CueType::Wait).unwrap();
    let wait_b = reg.create(&CueType::Wait).unwrap();
    let (id_a, id_b) = (wait_a.id(), wait_b.id());

    // A Stop cue that targets only wait A (built via the real serializer).
    let mut stop_json = reg.create(&CueType::Stop).unwrap().serialize();
    stop_json["target_cue_ids"] = serde_json::json!([id_a.to_string()]);
    let stop = reg.from_json(stop_json).unwrap();

    let mut list = list_of(vec![wait_a, wait_b, stop]);

    transport.go(&mut list).unwrap(); // A running
    transport.go(&mut list).unwrap(); // B running
    let result = transport.go(&mut list).unwrap(); // Stop targets A only

    assert_eq!(result.stopped, vec![id_a], "only the targeted cue is stopped");
    assert!(!list.get(&id_a).unwrap().is_running(), "A must be stopped");
    assert!(list.get(&id_b).unwrap().is_running(), "B must keep running");
}

#[test]
fn rejected_nested_sequential_child_remains_next_until_retry() {
    use inkue_lib::cue::types::CueState;
    use inkue_lib::engine::audio_engine::AudioEngine;

    let reg = full_registry();
    let audio = preloaded_audio(&reg);
    let id = audio.id();
    let mut inner = reg.create(&CueType::Group).unwrap();
    inner.set_group_mode(GroupMode::Sequential);
    inner.add_child(audio, -1).unwrap();
    let mut outer = reg.create(&CueType::Group).unwrap();
    outer.set_group_mode(GroupMode::Sequential);
    outer.add_child(inner, -1).unwrap();
    let outer_id = outer.id();
    let tail = memo(&reg, "After the group", ContinueMode::DoNotContinue, Duration::ZERO);
    let tail_id = tail.id();
    let mut list = list_of(vec![outer, tail]);

    let (mut offline, _, _) = recording_context();
    offline.audio_engine = AudioEngine::new_silent(&Default::default());
    let error = Transport::new(offline).go(&mut list).err().expect("the child rejected GO");
    assert!(error.to_string().contains("Audio start held"));
    assert_eq!(list.playhead_cue_id, Some(outer_id));
    assert_eq!(list.get(&outer_id).unwrap().state(), CueState::Standby);
    assert_eq!(list.get_recursive(&id).unwrap().state(), CueState::Standby);
    let (online, _, _) = recording_context();
    assert_eq!(Transport::new(online).go(&mut list).unwrap().triggered, vec![outer_id]);
    assert!(list.get_recursive(&id).unwrap().is_running());
    assert_eq!(list.playhead_cue_id, Some(tail_id));
}

#[test]
fn rejected_child_after_pre_wait_cannot_auto_fire_the_following_child() {
    use inkue_lib::engine::audio_engine::AudioEngine;

    let reg = full_registry();
    let mut audio = preloaded_audio(&reg);
    audio.set_pre_wait(Duration::from_millis(1));
    audio.set_continue_mode(ContinueMode::AutoContinue);
    let id = audio.id();
    let next = reg.create(&CueType::Wait).unwrap();
    let next_id = next.id();
    let mut group = reg.create(&CueType::Group).unwrap();
    group.set_group_mode(GroupMode::Sequential);
    group.add_child(audio, -1).unwrap();
    group.add_child(next, -1).unwrap();
    let (mut offline, _, _) = recording_context();
    offline.audio_engine = AudioEngine::new_silent(&Default::default());
    group.go(&offline).unwrap();
    std::thread::sleep(Duration::from_millis(3));
    assert!(group.tick(&offline).is_err());
    group.tick(&offline).unwrap();
    assert!(!group.child_cues().unwrap().iter().find(|c| c.id() == next_id).unwrap().is_running());
    assert_eq!(group.active_child_id(), Some(id), "failed pre-wait must leave the same child on standby");
    let (online, _, _) = recording_context();
    group.go(&online).unwrap();
    std::thread::sleep(Duration::from_millis(3));
    group.tick(&online).unwrap();
    assert!(group.child_cues().unwrap().iter().find(|c| c.id() == id).unwrap().is_running());
}

#[test]
fn rejected_chained_child_keeps_the_running_sibling_and_outer_standby() {
    use inkue_lib::engine::audio_engine::AudioEngine;

    let reg = full_registry();
    let mut first = reg.create(&CueType::Wait).unwrap();
    first.set_continue_mode(ContinueMode::AutoFollow);
    let first_id = first.id();
    let last = preloaded_audio(&reg);
    let last_id = last.id();
    let mut group = reg.create(&CueType::Group).unwrap();
    group.set_group_mode(GroupMode::Sequential);
    group.add_child(first, -1).unwrap();
    group.add_child(last, -1).unwrap();
    let id = group.id();
    let mut list = list_of(vec![group, memo(&reg, "Next", ContinueMode::DoNotContinue, Duration::ZERO)]);
    let (mut offline, _, _) = recording_context();
    offline.audio_engine = AudioEngine::new_silent(&Default::default());
    assert!(Transport::new(offline).go(&mut list).is_err());
    assert_eq!(list.playhead_cue_id, Some(id));
    assert!(list.get_recursive(&first_id).unwrap().is_running());
    let (online, _, _) = recording_context();
    Transport::new(online).go(&mut list).unwrap();
    assert!(list.get_recursive(&last_id).unwrap().is_running());
}

#[test]
fn sequential_group_holds_the_outer_playhead() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx);

    // Build a Sequential group with two memo children via the real serializer.
    let children: Vec<serde_json::Value> = (0..2)
        .map(|i| memo(&reg, &format!("child {i}"), ContinueMode::DoNotContinue, Duration::ZERO).serialize())
        .collect();
    let mut group = reg.create(&CueType::Group).unwrap();
    group.set_group_mode(GroupMode::Sequential);
    let mut gj = group.serialize();
    gj["children"] = serde_json::json!(children);
    let group = reg.from_json(gj).unwrap();
    let group_id = group.id();

    let mut list = list_of(vec![group]);

    // First GO fires the group's first child; because more children remain, the
    // sequential group holds the outer playhead on itself.
    let result = transport.go(&mut list).unwrap();
    assert_eq!(result.triggered, vec![group_id], "the group is the triggered cue");
    assert_eq!(
        list.playhead_cue_id,
        Some(group_id),
        "a running Sequential group with children left must retain the outer playhead"
    );

    // Second GO is absorbed by the group (fires its next child) rather than
    // advancing the outer playhead to a sibling.
    let result2 = transport.go(&mut list).unwrap();
    assert_eq!(result2.triggered, vec![group_id], "the second GO is absorbed by the group");
}

#[test]
fn start_first_moves_standby_and_leaves_manual_children_unstarted() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut transport = Transport::new(ctx.clone());
    let mut first = reg.create(&CueType::Wait).unwrap();
    first.set_continue_mode(ContinueMode::DoNotContinue);
    let unused = reg.create(&CueType::Wait).unwrap();
    let mut disarmed = reg.create(&CueType::Wait).unwrap();
    disarmed.set_disabled(true);
    let (disarmed_id, first_id, unused_id) = (disarmed.id(), first.id(), unused.id());

    let mut group = GroupCue::new();
    group.mode = GroupMode::StartFirst;
    group.children = vec![disarmed, first, unused];
    let saved = group.serialize();
    assert_eq!(saved["group_mode"], "start_first");
    let group = reg.from_json(saved).unwrap();
    let group_id = group.id();
    let outside = reg.create(&CueType::Wait).unwrap();
    let outside_id = outside.id();
    let mut list = list_of(vec![group, outside]);

    assert_eq!(transport.go(&mut list).unwrap().triggered, vec![group_id]);
    assert_eq!(list.playhead_cue_id, Some(outside_id));
    assert!(list.get_recursive(&first_id).unwrap().is_running());
    assert!(!list.get_recursive(&disarmed_id).unwrap().is_running());
    assert_eq!(transport.go(&mut list).unwrap().triggered, vec![outside_id]);
    assert!(list.get_recursive(&first_id).unwrap().is_running());
    assert!(!list.get_recursive(&unused_id).unwrap().is_running());

    list.get_mut_recursive(&first_id).unwrap().stop(&ctx).unwrap();
    let group = list.get_mut(&group_id).unwrap();
    group.tick(&ctx).unwrap();
    assert!(group.is_complete(), "unused children must not keep the group running");
}

#[test]
fn start_first_and_enter_keeps_manual_standby_until_the_last_armed_child_starts() {
    use inkue_lib::show::event_loop::{release_admitted_playhead, schedule_list_follows};

    let reg = full_registry();
    let (ctx, _) = test_context();
    let mut transport = Transport::new(ctx.clone());
    let first = memo(&reg, "First", ContinueMode::DoNotContinue, Duration::ZERO);
    let mut skipped = reg.create(&CueType::Wait).unwrap();
    skipped.set_disabled(true);
    let mut last = reg.create(&CueType::Wait).unwrap();
    last.set_continue_mode(ContinueMode::DoNotContinue);
    let (skipped_id, last_id) = (skipped.id(), last.id());
    let outside = reg.create(&CueType::Wait).unwrap();
    let outside_id = outside.id();
    let mut group = GroupCue::new();
    group.mode = GroupMode::StartFirstAndEnter;
    group.children = vec![first, skipped, last];
    let saved = group.serialize();
    assert_eq!(saved["group_mode"], "start_first_and_enter");
    let group = reg.from_json(saved).unwrap();
    let group_id = group.id();
    let mut list = list_of(vec![group, outside]);

    assert_eq!(transport.go(&mut list).unwrap().triggered, vec![group_id]);
    let group = list.get_mut(&group_id).unwrap();
    group.tick(&ctx).unwrap();
    assert_eq!(group.active_child_id(), Some(last_id));
    assert!(!group.is_complete(), "a manual child still awaits GO");
    assert_eq!(list.playhead_cue_id, Some(group_id));
    assert!(!schedule_list_follows(&mut list, &[], &mut Default::default(), std::time::Instant::now()));
    assert!(!list.get_recursive(&last_id).unwrap().is_running());

    assert_eq!(transport.go(&mut list).unwrap().triggered, vec![group_id]);
    assert_eq!(list.playhead_cue_id, Some(outside_id));
    assert!(list.get_recursive(&last_id).unwrap().is_running());
    assert!(!list.get_recursive(&skipped_id).unwrap().is_running());
    assert!(!list.get(&outside_id).unwrap().is_running());

    list.set_playhead(Some(group_id)).unwrap();
    assert!(!release_admitted_playhead(&mut list), "manual repark is deliberate");
    assert_eq!(list.playhead_cue_id, Some(group_id));
}

#[test]
fn start_first_and_enter_holds_unsupported_continuation_at_the_group_boundary() {
    let reg = full_registry();
    let (ctx, _) = test_context();
    let mut transport = Transport::new(ctx);
    let mut tail = reg.create(&CueType::Wait).unwrap();
    let tail_id = tail.id();
    tail.set_continue_mode(ContinueMode::AutoFollow);
    tail.set_post_wait(Duration::from_secs(1));
    let first = reg.create(&CueType::Wait).unwrap();
    let first_id = first.id();
    let mut group = GroupCue::new();
    group.mode = GroupMode::StartFirstAndEnter;
    group.children = vec![first, tail];
    let group_id = group.id();
    let outside = reg.create(&CueType::Wait).unwrap();
    let outside_id = outside.id();
    let mut list = list_of(vec![Box::new(group), outside]);

    assert!(transport.go(&mut list).err().unwrap().to_string().contains("last armed child"));
    assert_eq!(list.playhead_cue_id, Some(group_id));
    for id in [first_id, tail_id, outside_id] {
        assert_eq!(list.get_recursive(&id).unwrap().state(), CueState::Standby);
    }
    list.get_mut_recursive(&tail_id).unwrap().set_continue_mode(ContinueMode::DoNotContinue);
    list.get_mut(&group_id).unwrap().set_continue_mode(ContinueMode::AutoContinue);
    assert!(transport.go(&mut list).is_err());
    assert_eq!(list.playhead_cue_id, Some(group_id));
    list.get_mut(&group_id).unwrap().set_continue_mode(ContinueMode::DoNotContinue);
    transport.go(&mut list).unwrap();
    assert_eq!(list.get(&group_id).unwrap().active_child_id(), Some(tail_id));
}

#[test]
fn start_first_and_enter_reopens_on_the_exact_child_without_resuming_playback() {
    let reg = full_registry();
    let (ctx, _) = test_context();
    let mut transport = Transport::new(ctx);
    let mut first = reg.create(&CueType::Wait).unwrap();
    first.set_continue_mode(ContinueMode::DoNotContinue);
    let mut second = reg.create(&CueType::Wait).unwrap();
    second.set_continue_mode(ContinueMode::DoNotContinue);
    let (first_id, second_id) = (first.id(), second.id());
    let mut group = GroupCue::new();
    group.mode = GroupMode::StartFirstAndEnter;
    group.children = vec![first, second];
    let group_id = group.id();
    let outside = reg.create(&CueType::Wait).unwrap();
    let outside_id = outside.id();
    let mut list = list_of(vec![Box::new(group), outside]);
    transport.go(&mut list).unwrap();
    assert!(list.get_recursive(&first_id).unwrap().is_running());
    assert_eq!(list.get(&group_id).unwrap().active_child_id(), Some(second_id));

    let saved = list.to_json();
    assert_eq!(saved["playhead_cue_id"], serde_json::json!(group_id));
    assert_eq!(saved["playhead_child_id"], serde_json::json!(second_id));
    assert!(saved["cues"][0].get("playhead_child_id").is_none());
    let mut reopened = CueList::from_json(saved, &reg).unwrap();
    assert_eq!(reopened.playhead_cue_id, Some(group_id));
    assert_eq!(reopened.get(&group_id).unwrap().active_child_id(), Some(second_id));
    for id in [group_id, first_id, second_id] {
        assert_eq!(reopened.get_recursive(&id).unwrap().state(), CueState::Standby);
    }

    transport.go(&mut reopened).unwrap();
    assert!(!reopened.get_recursive(&first_id).unwrap().is_running());
    assert!(reopened.get_recursive(&second_id).unwrap().is_running());
    assert_eq!(reopened.playhead_cue_id, Some(outside_id));
}

#[test]
fn start_first_and_enter_releases_on_automatic_last_start_and_refuses_nested_addressing() {
    use inkue_lib::show::event_loop::release_admitted_playhead;

    let reg = full_registry();
    let (ctx, _) = test_context();
    let mut transport = Transport::new(ctx.clone());
    let mut first = reg.create(&CueType::Wait).unwrap().serialize();
    first["wait_duration_ms"] = serde_json::json!(0);
    let mut first = reg.from_json(first).unwrap();
    first.set_continue_mode(ContinueMode::AutoFollow);
    first.set_pre_wait(Duration::from_secs(30));
    let mut last = reg.create(&CueType::Wait).unwrap();
    last.set_continue_mode(ContinueMode::DoNotContinue);
    let last_id = last.id();
    let mut nested = GroupCue::new();
    let grandchild = reg.create(&CueType::Wait).unwrap();
    let grandchild_id = grandchild.id();
    nested.set_disabled(true);
    nested.children.push(grandchild);

    let mut group = GroupCue::new();
    group.mode = GroupMode::StartFirstAndEnter;
    group.children = vec![first, last, Box::new(nested)];
    let group_id = group.id();
    let outside = reg.create(&CueType::Wait).unwrap();
    let outside_id = outside.id();
    let mut list = list_of(vec![Box::new(group), outside]);
    assert!(list.set_playhead(Some(grandchild_id)).unwrap_err().to_string().contains("direct child"));
    assert_eq!(list.playhead_cue_id, Some(group_id));

    transport.go(&mut list).unwrap();
    assert!(!release_admitted_playhead(&mut list));
    assert!(!list.get_recursive(&last_id).unwrap().is_running());
    let group = list.get_mut(&group_id).unwrap();
    group.child_cues_mut().unwrap()[0].set_pre_wait(Duration::ZERO);
    group.tick(&ctx).unwrap();
    assert!(release_admitted_playhead(&mut list));
    assert_eq!(list.playhead_cue_id, Some(outside_id));
    assert!(list.get_recursive(&last_id).unwrap().is_running());
    assert!(!list.get(&outside_id).unwrap().is_running());
}

#[test]
fn start_first_and_enter_preserves_a_staged_child_when_resuming_the_group() {
    let reg = full_registry();
    let (ctx, _, calls) = recording_context();
    let mut first = reg.create(&CueType::Wait).unwrap();
    first.set_continue_mode(ContinueMode::DoNotContinue);
    let mut second = preloaded_audio(&reg);
    second.preload(&ctx).unwrap();
    let second_id = second.id();
    let mut group = GroupCue::new();
    group.mode = GroupMode::StartFirstAndEnter;
    group.children = vec![first, second];

    group.go(&ctx).unwrap();
    group.pause(&ctx).unwrap();
    group.resume(&ctx).unwrap();
    assert!(group.children[0].is_running());
    assert_eq!(group.active_child_id(), Some(second_id));
    assert!(group.children[1].is_paused());
    assert_eq!(*calls.lock().unwrap(), [EngineCall::AudioPlayPausedRouted { device: None }]);

    group.go(&ctx).unwrap();
    assert!(group.children[1].is_running());
    assert_eq!(
        *calls.lock().unwrap(),
        [EngineCall::AudioPlayPausedRouted { device: None }, EngineCall::AudioResumeVoice],
    );
}

#[test]
fn start_first_and_enter_does_not_resume_audio_loaded_after_its_pause() {
    let reg = full_registry();
    for pre_wait in [Duration::ZERO, Duration::from_secs(30)] {
        let (ctx, _, calls) = recording_context();
        let mut audio = preloaded_audio(&reg);
        audio.set_continue_mode(ContinueMode::DoNotContinue);
        audio.set_pre_wait(pre_wait);
        let audio_id = audio.id();
        let reset = command(&reg, CueType::Reset, vec![audio_id]);
        let load = command(&reg, CueType::Load, vec![audio_id]);
        let (reset_id, load_id) = (reset.id(), load.id());
        let mut group = GroupCue::new();
        group.mode = GroupMode::StartFirstAndEnter;
        group.children.push(audio);
        let group_id = group.id();
        let mut list = list_of(vec![Box::new(group), reset, load]);
        let mut transport = Transport::new(ctx);
        transport.go(&mut list).unwrap();
        transport.pause_cue(&mut list, &group_id).unwrap();
        let original = list.get_recursive(&audio_id).unwrap();
        assert!(original.is_paused());
        assert_eq!(original.playing_voice_id().is_some(), pre_wait.is_zero());
        let generation = original.play_generation();

        // Reset stops the existing child. Load then stages the same UUID
        // silently without advancing its play generation.
        transport.go_by_id(&mut list, &reset_id).unwrap();
        assert_eq!(list.get_recursive(&audio_id).unwrap().state(), CueState::Standby);
        transport.go_by_id(&mut list, &load_id).unwrap();
        let staged = list.get_recursive(&audio_id).unwrap();
        assert!(staged.is_paused());
        assert_eq!(staged.play_generation(), generation);
        assert!(staged.playing_voice_id().is_none());
        assert!(matches!(calls.lock().unwrap().last(), Some(EngineCall::AudioPlayPausedRouted { device: None })));

        transport.resume_cue(&mut list, &group_id).unwrap();
        assert!(list.get(&group_id).unwrap().is_running());
        let staged = list.get_recursive(&audio_id).unwrap();
        assert!(staged.is_paused());
        assert!(staged.playing_voice_id().is_none());
        assert!(!calls.lock().unwrap().iter().any(|call| matches!(call, EngineCall::AudioResumeVoice)));
    }
}

#[test]
fn start_first_and_enter_resumes_only_the_wait_generation_it_paused() {
    let reg = full_registry();
    let (ctx, _) = test_context();
    let mut group = GroupCue::new();
    group.mode = GroupMode::StartFirstAndEnter;
    let mut wait = reg.create(&CueType::Wait).unwrap();
    wait.set_continue_mode(ContinueMode::DoNotContinue);
    group.children.push(wait);

    group.go(&ctx).unwrap();
    group.pause(&ctx).unwrap();
    group.resume(&ctx).unwrap();
    assert!(group.children[0].is_running(), "the matching generation resumes");

    group.pause(&ctx).unwrap();
    let wait = &mut group.children[0];
    let old_generation = wait.play_generation();
    wait.stop(&ctx).unwrap();
    wait.go(&ctx).unwrap();
    wait.pause(&ctx).unwrap();
    assert_ne!(wait.play_generation(), old_generation);
    group.resume(&ctx).unwrap();
    assert!(group.children[0].is_paused(), "the replacement generation stays paused");
}

#[test]
fn start_first_and_enter_keeps_standby_on_a_rejected_final_output_until_retry() {
    use inkue_lib::engine::device_manager::OutputPatch;
    use inkue_lib::show::event_loop::release_admitted_playhead;

    let reg = full_registry();
    let (mut ctx, _, calls) = recording_context();
    let mut patch = OutputPatch::new("Backstage", "default", vec![0, 1]);
    patch.require_named_device = true;
    ctx.default_patch_id = Some(patch.id);
    ctx.output_patches = Arc::new(vec![patch]);

    let first = memo(&reg, "First", ContinueMode::DoNotContinue, Duration::ZERO);
    let mut final_audio = preloaded_audio(&reg);
    final_audio.set_pre_wait(Duration::from_secs(30));
    let final_id = final_audio.id();
    let mut group = GroupCue::new();
    group.mode = GroupMode::StartFirstAndEnter;
    group.children = vec![first, final_audio];
    let group_id = group.id();
    let outside = reg.create(&CueType::Wait).unwrap();
    let outside_id = outside.id();
    let mut list = list_of(vec![Box::new(group), outside]);

    Transport::new(ctx.clone()).go(&mut list).unwrap();
    assert!(Transport::new(ctx.clone()).go(&mut list).is_err());
    assert_eq!(list.get(&group_id).unwrap().active_child_id(), Some(final_id));
    assert!(!release_admitted_playhead(&mut list));

    Arc::make_mut(&mut ctx.output_patches)[0].device_id = "hw:CARD=Venue".into();
    Transport::new(ctx.clone()).go(&mut list).unwrap();
    assert_eq!(list.playhead_cue_id, Some(group_id));
    assert_eq!(list.get(&group_id).unwrap().active_child_id(), Some(final_id));
    assert!(!release_admitted_playhead(&mut list));
    list.get_mut_recursive(&final_id).unwrap().set_pre_wait(Duration::ZERO);
    list.get_mut(&group_id).unwrap().tick(&ctx).unwrap();
    assert!(release_admitted_playhead(&mut list));
    assert_eq!(list.playhead_cue_id, Some(outside_id));
    assert!(calls.lock().unwrap().iter().any(|call| matches!(
        call, EngineCall::AudioPlayRouted { device: Some(id) } if id == "hw:CARD=Venue"
    )));
}

#[test]
fn start_first_and_enter_keeps_final_child_in_standby_when_admission_is_cancelled() {
    use inkue_lib::engine::device_manager::OutputPatch;
    use inkue_lib::show::event_loop::release_admitted_playhead;

    let reg = full_registry();
    let (mut ctx, _, calls) = recording_context();
    let mut patch = OutputPatch::new("Backstage", "hw:CARD=Venue", vec![0, 1]);
    patch.require_named_device = true;
    ctx.default_patch_id = Some(patch.id);
    ctx.output_patches = Arc::new(vec![patch]);

    let first = memo(&reg, "First", ContinueMode::DoNotContinue, Duration::ZERO);
    let mut final_audio = preloaded_audio(&reg);
    final_audio.set_pre_wait(Duration::from_secs(30));
    let final_id = final_audio.id();
    let mut group = GroupCue::new();
    group.mode = GroupMode::StartFirstAndEnter;
    group.children = vec![first, final_audio];
    let group_id = group.id();
    let outside = reg.create(&CueType::Wait).unwrap();
    let outside_id = outside.id();
    let mut list = list_of(vec![Box::new(group), outside]);

    Transport::new(ctx.clone()).go(&mut list).unwrap();
    Transport::new(ctx.clone()).go(&mut list).unwrap();
    assert!(list.get_recursive(&final_id).unwrap().pending_start_admission());
    list.get_mut_recursive(&final_id).unwrap().stop(&ctx).unwrap();

    assert!(!release_admitted_playhead(&mut list));
    assert_eq!(list.playhead_cue_id, Some(group_id));
    let group = list.get(&group_id).unwrap();
    assert_eq!(group.active_child_id(), Some(final_id));
    assert!(group.is_auto_continue_fired());
    assert!(!group.is_complete());
    assert!(!calls.lock().unwrap().iter().any(|call| matches!(call, EngineCall::AudioPlayRouted { .. })));

    Transport::new(ctx.clone()).go(&mut list).unwrap();
    list.get_mut_recursive(&final_id).unwrap().set_pre_wait(Duration::ZERO);
    list.get_mut(&group_id).unwrap().tick(&ctx).unwrap();
    assert!(release_admitted_playhead(&mut list));
    assert_eq!(list.playhead_cue_id, Some(outside_id));
}

#[test]
fn start_first_auto_continue_uses_post_wait_from_action_start_and_can_overlap() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut first = reg.create(&CueType::Wait).unwrap();
    first.set_continue_mode(ContinueMode::AutoContinue);
    first.set_pre_wait(Duration::from_secs(30));
    let second = reg.create(&CueType::Wait).unwrap();
    let mut group = GroupCue::new();
    group.mode = GroupMode::StartFirst;
    group.children = vec![first, second];

    group.go(&ctx).unwrap();
    assert!(!group.children[1].is_running(), "even zero Post-Wait waits for action start");

    group.children[0].set_pre_wait(Duration::ZERO);
    group.children[0].set_post_wait(Duration::from_secs(30));
    group.tick(&ctx).unwrap();
    assert!(!group.children[1].is_running());
    group.pause(&ctx).unwrap();
    group.children[0].set_post_wait(Duration::ZERO);
    group.tick(&ctx).unwrap();
    assert!(!group.children[1].is_running(), "pause holds the chain");

    group.resume(&ctx).unwrap();
    group.tick(&ctx).unwrap();
    assert!(group.children[0].is_running(), "the first child has time remaining");
    assert!(group.children[1].is_running(), "the second child overlaps the first");
}

#[test]
fn start_first_auto_follow_waits_for_natural_end_instead_of_a_stop() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    for stop in [false, true] {
        let mut first = reg.create(&CueType::Wait).unwrap().serialize();
        first["wait_duration_ms"] = serde_json::json!(0);
        let mut first = reg.from_json(first).unwrap();
        first.set_continue_mode(ContinueMode::AutoFollow);
        first.set_pre_wait(Duration::from_secs(30));
        first.set_post_wait(Duration::from_secs(30));
        let mut group = GroupCue::new();
        group.mode = GroupMode::StartFirst;
        group.children = vec![first, reg.create(&CueType::Wait).unwrap()];
        group.go(&ctx).unwrap();
        group.tick(&ctx).unwrap();
        assert!(!group.children[1].is_running(), "the first child's action has not completed");

        if stop {
            group.children[0].stop(&ctx).unwrap();
        } else {
            group.children[0].set_pre_wait(Duration::ZERO);
        }
        group.tick(&ctx).unwrap();
        assert_eq!(group.children[1].is_running(), !stop);
        assert_eq!(group.is_complete(), stop);
    }
}

#[test]
fn start_first_rejects_a_reachable_cue_list_action_before_starting_children() {
    let reg = full_registry();
    let (ctx, _rx) = test_context();
    let mut group = GroupCue::new();
    group.mode = GroupMode::StartFirst;
    group.children = vec![
        reg.create(&CueType::Wait).unwrap(),
        reg.create(&CueType::Stop).unwrap(),
    ];
    assert!(group.go(&ctx).unwrap_err().to_string().contains("main cue list"));
    assert_eq!(group.state(), CueState::Standby);
    assert!(!group.children[0].is_running());

    group.children[0].set_continue_mode(ContinueMode::DoNotContinue);
    group.go(&ctx).unwrap();
    assert!(group.children[0].is_running(), "a manual boundary ends the reachable sequence");
}

/// An Audio cue with a decoded buffer preloaded, so `go()` actually plays it
/// through the (recording) audio engine and assigns it a voice id.
fn preloaded_audio(reg: &CueRegistry) -> Box<dyn Cue> {
    let mut c = reg.create(&CueType::Audio).unwrap();
    // Long duration so nothing auto-completes by wall clock during the test.
    c.accept_preloaded_audio(Arc::new(vec![0.0f32; 4800]), 1, 48_000, Duration::from_secs(30));
    c
}

fn set_gain_count(log: &CallLog) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|c| matches!(c, EngineCall::AudioSetGain { .. }))
        .count()
}

#[test]
fn fade_targeting_a_group_fades_every_child_voice() {
    // Regression: a Fade cue targeting a Group did nothing — the transport only
    // looked at top-level cues and only knew how to read a single Audio voice,
    // so a Group (which owns one voice per child) contributed no voices and the
    // fade had nothing to drive. It now collects the group's voices recursively.
    let reg = full_registry();
    let (ctx, _rx, log) = recording_context();
    let tick_ctx = ctx.clone(); // shares the same recording engine double + log
    let mut transport = Transport::new(ctx);

    // A Simultaneous group with two audio children (both play at once).
    let mut group = GroupCue::new();
    group.mode = GroupMode::Simultaneous;
    group.children.push(preloaded_audio(&reg));
    group.children.push(preloaded_audio(&reg));
    let group_id = group.id();

    // A Fade cue targeting the group: fade the volume down to silence.
    let mut fade = FadeCue::new();
    fade.target_cue_ids = vec![group_id];
    fade.target_volume_db = -60.0;
    fade.fade_duration_ms = 30;
    let fade_id = fade.id();

    let mut list = list_of(vec![Box::new(group), Box::new(fade)]);

    // GO the group: both children play and get voice ids.
    transport.go(&mut list).unwrap();
    // GO the fade: the transport injects the group's child voices into it.
    transport.go(&mut list).unwrap();

    let before = set_gain_count(&log);

    // Tick the fade to completion; it interpolates gain on every injected voice.
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(200) {
        if let Some(fc) = list.get_mut(&fade_id) {
            let _ = fc.tick(&tick_ctx);
        }
        std::thread::sleep(Duration::from_millis(3));
    }

    let applied = set_gain_count(&log) - before;
    assert!(
        applied >= 2,
        "a fade on a group must drive set_voice_gain on every child voice \
         (got {applied} gain updates — 0 would mean the group contributed no voices)"
    );
}

#[test]
fn devamp_forwards_to_every_target_voice_with_mode() {
    // GO on a Devamp cue must resolve its targets' voices and forward the
    // devamp (with the stop-at-end flag) to the audio engine — including the
    // voices of a Group's children, like Fade does.
    let reg = full_registry();
    let (ctx, _rx, log) = recording_context();
    let mut transport = Transport::new(ctx);

    let mut group = GroupCue::new();
    group.mode = GroupMode::Simultaneous;
    group.children.push(preloaded_audio(&reg));
    group.children.push(preloaded_audio(&reg));
    let group_id = group.id();

    let mut devamp = DevampCue::new();
    devamp.target_cue_ids = vec![group_id];
    devamp.stop_at_end = true;

    let mut list = list_of(vec![Box::new(group), Box::new(devamp)]);

    transport.go(&mut list).unwrap(); // group: children play
    transport.go(&mut list).unwrap(); // devamp: forward to the voices

    let devamps: Vec<bool> = log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|c| match c {
            EngineCall::AudioDevamp { stop_at_end } => Some(*stop_at_end),
            _ => None,
        })
        .collect();
    assert_eq!(
        devamps,
        vec![true, true],
        "one devamp per child voice, carrying the stop-at-end flag"
    );
}

#[test]
fn devamp_without_running_target_is_a_no_op() {
    let reg = full_registry();
    let (ctx, _rx, log) = recording_context();
    let mut transport = Transport::new(ctx);

    let idle_audio = preloaded_audio(&reg); // never GO'd — no voice
    let mut devamp = DevampCue::new();
    devamp.target_cue_ids = vec![idle_audio.id()];

    // Playhead starts on the devamp: put it first.
    let mut list = list_of(vec![Box::new(devamp), idle_audio]);
    transport.go(&mut list).unwrap();

    let count = log
        .lock()
        .unwrap()
        .iter()
        .filter(|c| matches!(c, EngineCall::AudioDevamp { .. }))
        .count();
    assert_eq!(count, 0, "an idle target contributes no voices — nothing to devamp");
}

#[test]
fn whole_audio_devamp_requires_a_running_target_and_arms_only_its_voice() {
    use inkue_lib::cue::{audio_cue::AudioCue, devamp_cue::DevampScope, types::CueState};

    let registry = full_registry();
    let (context, _, calls) = recording_context();
    let mut transport = Transport::new(context);
    let mut audio = AudioCue::new();
    audio.accept_preloaded_audio(Arc::new(vec![0.5; 4800]), 1, 48_000, Duration::from_millis(100));
    audio.loop_count = u32::MAX;
    let audio_id = audio.id();
    let mut devamp = DevampCue::new();
    devamp.scope = DevampScope::WholeCue;
    devamp.target_cue_ids = vec![audio_id];
    let devamp_id = devamp.id();
    let next = registry.create(&CueType::Wait).unwrap();
    let next_id = next.id();
    let mut list = list_of(vec![Box::new(audio), Box::new(devamp), next]);

    list.playhead_cue_id = Some(devamp_id);
    assert!(transport.go(&mut list).is_err());
    assert_eq!(list.playhead_cue_id, Some(devamp_id));
    assert!(calls.lock().unwrap().is_empty());

    list.playhead_cue_id = Some(audio_id);
    transport.go(&mut list).unwrap();
    let voice = list.get(&audio_id).unwrap().playing_voice_id().unwrap();
    calls.lock().unwrap().clear();
    let result = transport.go(&mut list).unwrap();
    assert_eq!(result.triggered, [devamp_id]);
    assert_eq!(*calls.lock().unwrap(), [EngineCall::AudioWholeDevamp { voice }]);
    assert!(list.get(&audio_id).unwrap().is_running());
    assert_eq!(list.get(&next_id).unwrap().state(), CueState::Standby);

    transport.pause_cue(&mut list, &audio_id).unwrap();
    calls.lock().unwrap().clear();
    list.playhead_cue_id = Some(devamp_id);
    assert!(transport.go(&mut list).is_err());
    assert_eq!(list.get(&devamp_id).unwrap().state(), CueState::Standby);
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn moving_an_active_source_preserves_the_finished_fade_and_does_not_fire_the_next_cue() {
    use inkue_lib::commands::cue_cmds::{apply_cue_properties, MergePolicy};
    use inkue_lib::show::{event_loop::schedule_list_follows, Workspace};
    use serde_json::json;

    let reg = full_registry();
    let (ctx, _rx, log) = recording_context();
    let mut transport = Transport::new(ctx.clone());
    let mut ws = Workspace::new("Sound check");
    let list = ws.active_cue_list_mut().unwrap();
    let mut audio = preloaded_audio(&reg);
    let audio_id = audio.id();
    audio.set_continue_mode(ContinueMode::AutoContinue);
    list.push(audio);
    let mut fade = FadeCue::new();
    fade.target_cue_ids = vec![audio_id];
    fade.fade_duration_ms = 0;
    fade.target_volume_db = -40.0;
    fade.stop_at_end = false;
    let fade_id = fade.id();
    list.push(Box::new(fade));
    let next = reg.create(&CueType::Midi).unwrap();
    let next_id = next.id();
    list.push(next);

    assert_eq!(transport.go(list).unwrap().triggered, vec![audio_id, fade_id]);
    list.get_mut(&fade_id).unwrap().tick(&ctx).unwrap();
    assert!(log.lock().unwrap().iter().any(|call|
        matches!(call, EngineCall::AudioSetGain { gain } if (gain - 0.01).abs() < 1e-5)));
    let voice_id = list.get(&audio_id).unwrap().playing_voice_id();
    assert!(voice_id.is_some());
    log.lock().unwrap().clear();

    let different_device = json!({"output_patch_id": uuid::Uuid::new_v4()});
    let result = apply_cue_properties(ctx.audio_engine.as_ref(), ctx.output_engine.as_ref(), &reg,
        &mut ws, audio_id, &different_device, MergePolicy::AllKeys);
    assert!(result.unwrap_err().contains("Stop or unload"));
    assert!(log.lock().unwrap().is_empty());

    let edit = json!({"point_mix": {
        "input": 1, "source": {"x": 25, "y": 50},
        "speakers": [{"output": 1, "x": 0, "y": 50}, {"output": 2, "x": 100, "y": 50}]
    }});
    apply_cue_properties(ctx.audio_engine.as_ref(), ctx.output_engine.as_ref(), &reg,
        &mut ws, audio_id, &edit, MergePolicy::AllKeys).unwrap();
    let list = ws.active_cue_list_mut().unwrap();
    assert_eq!(list.get(&audio_id).unwrap().playing_voice_id(), voice_id);
    assert_eq!(*log.lock().unwrap(), [EngineCall::AudioSetLevelMatrix { present: true }]);
    assert!(!schedule_list_follows(list, &[], &mut Default::default(), std::time::Instant::now()));
    assert_eq!(list.playhead_cue_id, Some(next_id));
}

#[test]
fn per_output_trim_edits_preserve_the_voice_and_refuse_a_reordered_patch() {
    use inkue_lib::commands::cue_cmds::{apply_cue_properties, MergePolicy};
    use inkue_lib::engine::device_manager::OutputPatch;
    use inkue_lib::show::Workspace;
    use serde_json::json;

    let reg = full_registry();
    let (mut ctx, _rx, log) = recording_context();
    let mut ws = Workspace::new("Output trim rehearsal");
    let mut patch = OutputPatch::new("House", "hw:CARD=Test", vec![2, 0]);
    patch.require_named_device = true;
    ws.default_output_patch_id = Some(patch.id);
    ws.output_patches.push(patch.clone());
    ctx.default_patch_id = Some(patch.id);
    ctx.output_patches = std::sync::Arc::new(vec![patch.clone()]);
    let list = ws.active_cue_list_mut().unwrap();
    let cue = preloaded_audio(&reg);
    let id = cue.id();
    assert!(cue.serialize()["output_trim"].is_null());
    list.push(cue);
    Transport::new(ctx.clone()).go_by_id(list, &id).unwrap();
    let voice = list.get(&id).unwrap().playing_voice_id().unwrap();
    log.lock().unwrap().clear();

    let edit = json!({"output_trim_workspace": ws.instance_id(),
        "output_trim": {"patch": {"id": patch.id, "device_id": patch.device_id, "channels": [2, 0]},
                         "levels_db": [0.0, -60.0]}});
    let apply = |workspace: &mut Workspace, properties: &serde_json::Value| apply_cue_properties(
        ctx.audio_engine.as_ref(), ctx.output_engine.as_ref(), &reg,
        workspace, id, properties, MergePolicy::AllKeys,
    );
    assert!(apply(&mut ws, &edit).unwrap());
    let saved = ws.active_cue_list().unwrap().get(&id).unwrap();
    assert_eq!(saved.playing_voice_id(), Some(voice));
    assert_eq!(saved.serialize()["output_trim"], edit["output_trim"]);
    assert_eq!(*log.lock().unwrap(), [EngineCall::AudioSetOutputTrims {
        voice, device: patch.device_id.clone(), gains: vec![1.0, 0.0],
    }]);

    let matrix = json!({"level_matrix": [[-6.0, -18.0]]});
    assert!(apply(&mut ws, &matrix).unwrap());
    let changed = OutputPatch::new("Backstage", "hw:CARD=Test", vec![1, 3]);
    ws.default_output_patch_id = Some(changed.id);
    ws.output_patches.push(changed);
    log.lock().unwrap().clear();
    let different_matrix = json!({"level_matrix": [[0.0, -18.0]], "volume_db": -12.0});
    assert!(apply(&mut ws, &different_matrix).unwrap_err().contains("different output assignment"));
    assert_eq!(ws.active_cue_list().unwrap().get(&id).unwrap().serialize()["level_matrix"],
        matrix["level_matrix"]);
    assert!(log.lock().unwrap().is_empty());

    ws.default_output_patch_id = Some(patch.id);
    ws.output_patches[0].channels.reverse();
    log.lock().unwrap().clear();
    assert!(apply(&mut ws, &different_matrix).unwrap_err().contains("different output assignment"));
    assert!(apply(&mut ws, &edit).unwrap_err().contains("different channel assignment"));
    assert!(log.lock().unwrap().is_empty());
    let mut replanted = preloaded_audio(&reg);
    let mut data = replanted.serialize();
    data["output_trim"] = edit["output_trim"].clone();
    let decoded = replanted.extract_decoded_audio().unwrap();
    replanted = reg.from_json(data).unwrap();
    replanted.accept_preloaded_audio(decoded.0, decoded.1, decoded.2, decoded.3);
    ctx.output_patches = std::sync::Arc::new(ws.output_patches.clone());
    assert!(replanted.go(&ctx).unwrap_err().to_string().contains("different channel assignment"));
    assert!(replanted.playing_voice_id().is_none());
    assert!(log.lock().unwrap().is_empty());
}

#[test]
fn a_live_trim_edit_keeps_the_same_voice_and_only_changes_compensation() {
    use inkue_lib::commands::cue_cmds::{apply_cue_properties, MergePolicy};
    use inkue_lib::show::Workspace;
    use serde_json::json;

    let reg = full_registry();
    let (ctx, _rx, log) = recording_context();
    let mut ws = Workspace::new("Sound check");
    let list = ws.active_cue_list_mut().unwrap();
    let audio = preloaded_audio(&reg);
    let id = audio.id();
    assert_eq!(audio.serialize()["trim_db"], json!(0.0));
    list.push(audio);
    Transport::new(ctx.clone()).go_by_id(list, &id).unwrap();
    let voice = list.get(&id).unwrap().playing_voice_id().unwrap();
    log.lock().unwrap().clear();
    let edit = |ws: &mut Workspace, db| apply_cue_properties(
        ctx.audio_engine.as_ref(), ctx.output_engine.as_ref(), &reg,
        ws, id, &json!({"trim_db": db}), MergePolicy::AllKeys,
    );
    assert!(edit(&mut ws, 99.0).is_err());
    assert!(log.lock().unwrap().is_empty());
    assert!(edit(&mut ws, -6.0).unwrap());
    let cue = ws.active_cue_list().unwrap().get(&id).unwrap();
    assert_eq!(cue.playing_voice_id(), Some(voice));
    let saved = reg.from_json(cue.serialize()).unwrap();
    assert_eq!(saved.serialize()["trim_db"], json!(-6.0));
    assert_eq!(*log.lock().unwrap(), [EngineCall::AudioSetTrim { voice, gain: 10.0_f64.powf(-6.0 / 20.0) as f32 }]);
}

#[test]
fn a_batch_edit_on_a_paused_audio_child_keeps_its_voice_and_resume_position() {
    use inkue_lib::commands::cue_cmds::{apply_cue_properties, MergePolicy};
    use inkue_lib::show::Workspace;
    use serde_json::json;

    let reg = full_registry();
    let (ctx, _rx, log) = recording_context();
    let mut transport = Transport::new(ctx.clone());
    let mut ws = Workspace::new("Sound check");
    let list = ws.active_cue_list_mut().unwrap();
    let mut group = GroupCue::new();
    group.mode = GroupMode::Simultaneous;
    let group_id = group.id();
    let child = preloaded_audio(&reg);
    let child_id = child.id();
    group.children.push(child);
    list.push(Box::new(group));
    transport.go(list).unwrap();
    let child = list.get_mut_recursive(&child_id).unwrap();
    let voice = child.playing_voice_id().unwrap();
    child.pause(&ctx).unwrap();
    child.seek(1250, &ctx);
    log.lock().unwrap().clear();

    apply_cue_properties(ctx.audio_engine.as_ref(), ctx.output_engine.as_ref(), &reg,
        &mut ws, group_id, &json!({"notes":"ready"}), MergePolicy::KnownKeysOnly).unwrap();
    apply_cue_properties(ctx.audio_engine.as_ref(), ctx.output_engine.as_ref(), &reg,
        &mut ws, child_id, &json!({"pan":0.5}), MergePolicy::KnownKeysOnly).unwrap();
    assert_eq!(*log.lock().unwrap(), [EngineCall::AudioSetPan { pan: 0.5 }]);
    let child = ws.active_cue_list_mut().unwrap().get_mut_recursive(&child_id).unwrap();
    assert!(child.is_paused());
    assert_eq!(child.playing_voice_id(), Some(voice));
    assert_eq!(child.action_elapsed(), Duration::from_millis(1250));
    child.resume(&ctx).unwrap();
    assert!(child.action_elapsed() >= Duration::from_millis(1250));
}

#[test]
fn undo_and_redo_wait_for_other_lists_nested_pre_waits_and_paused_voices() {
    use inkue_lib::commands::undo_cmds::{apply_history, take_snapshot};
    use inkue_lib::show::{undo_stack::{HistoryDirection, UndoHistory}, Workspace};

    let registry = full_registry();
    let (ctx, _rx, _log) = recording_context();
    let mut ws = Workspace::new("Rehearsal");
    let mut stack = UndoHistory::new();
    let desk = ws.active_cue_list_mut().unwrap();
    let desk_id = desk.id;
    stack.push_action(desk_id, take_snapshot(desk));
    desk.push(memo(&registry, "Desk edit", ContinueMode::DoNotContinue, Duration::ZERO));

    let mut child = preloaded_audio(&registry);
    let child_id = child.id();
    child.set_pre_wait(Duration::from_secs(30));
    child.go(&ctx).unwrap();
    assert!(child.is_running() && child.playing_voice_id().is_none());
    let mut group = GroupCue::new();
    group.children.push(child);
    let other = list_of(vec![Box::new(group)]);
    let other_id = other.id;
    ws.cue_lists.push(other);

    assert!(apply_history(&mut ws, &mut stack, &registry, HistoryDirection::Undo).is_err());
    assert!(stack.can_undo(desk_id) && !stack.can_redo(desk_id));
    assert_eq!(ws.active_cue_list().unwrap().cues.len(), 1);
    ws.cue_list_by_id_mut(other_id).unwrap().get_mut_recursive(&child_id).unwrap().stop(&ctx).unwrap();
    assert!(apply_history(&mut ws, &mut stack, &registry, HistoryDirection::Undo).unwrap());
    assert!(ws.active_cue_list().unwrap().cues.is_empty());

    let child = ws.cue_list_by_id_mut(other_id).unwrap().get_mut_recursive(&child_id).unwrap();
    child.set_pre_wait(Duration::ZERO);
    child.go(&ctx).unwrap();
    child.pause(&ctx).unwrap();
    let voice = child.playing_voice_id().unwrap();
    assert!(apply_history(&mut ws, &mut stack, &registry, HistoryDirection::Redo).is_err());
    assert!(stack.can_redo(desk_id) && !stack.can_undo(desk_id));
    let child = ws.cue_list_by_id_mut(other_id).unwrap().get_mut_recursive(&child_id).unwrap();
    assert_eq!(child.playing_voice_id(), Some(voice));
    child.hard_stop(&ctx).unwrap();
    assert!(apply_history(&mut ws, &mut stack, &registry, HistoryDirection::Redo).unwrap());
    assert_eq!(ws.active_cue_list().unwrap().cues[0].name(), "Desk edit");
}

#[test]
fn wait_and_light_auto_continue_exactly_once_per_run() {
    use inkue_lib::show::event_loop::schedule_list_follows;
    use std::{collections::HashMap, time::Instant};

    let reg = full_registry();
    let (ctx, _rx, _) = recording_context();
    let mut transport = Transport::new(ctx);
    for kind in [CueType::Wait, CueType::Light] {
        for post_wait in [Duration::ZERO, Duration::from_nanos(1)] {
            let mut first = reg.create(&kind).unwrap();
            first.set_continue_mode(ContinueMode::AutoContinue);
            first.set_post_wait(post_wait);
            let first_id = first.id();
            let second = memo(&reg, "Automatic", ContinueMode::DoNotContinue, Duration::ZERO);
            let second_id = second.id();
            let manual = memo(&reg, "Manual", ContinueMode::DoNotContinue, Duration::ZERO);
            let manual_id = manual.id();
            let mut list = list_of(vec![first, second, manual]);
            let mut pending = HashMap::new();
            for _ in 0..2 {
                list.set_playhead(Some(first_id)).unwrap();
                let go = transport.go(&mut list).unwrap();
                if post_wait.is_zero() {
                    assert_eq!(go.triggered, [first_id, second_id]);
                } else {
                    assert_eq!(go.triggered, [first_id]);
                    assert!(schedule_list_follows(&mut list, &[], &mut pending, Instant::now()));
                    assert_eq!(transport.go(&mut list).unwrap().triggered, [second_id]);
                }
                assert!(list.get(&first_id).unwrap().is_running());
                assert!(!schedule_list_follows(&mut list, &[], &mut pending, Instant::now()));
                assert_eq!(list.playhead_cue_id, Some(manual_id));
                list.get_mut(&first_id).unwrap().stop(&transport.context).unwrap();
            }
        }
    }
}

#[test]
fn pause_pre_wait_holds_output_and_restores_the_clock_after_a_live_edit() {
    use inkue_lib::cue::types::CueState;
    use inkue_lib::engine::audio_input::InputPatch;

    let registry = full_registry();
    for (kind, prepare) in [(CueType::Video, false), (CueType::Video, true),
                            (CueType::Audio, false), (CueType::Mic, false)] {
        let patch = InputPatch::new("House mic", "rehearsal", vec![0]);
        let mut json = registry.create(&kind).unwrap().serialize();
        json["file_path"] = serde_json::json!("rehearsal.mov");
        json["input_patch_id"] = serde_json::json!(patch.id);
        let (ctx, events, calls) = common::recording_context_with(vec![], vec![], vec![patch]);
        let mut cue = registry.from_json(json).unwrap();
        if kind == CueType::Audio {
            cue.accept_preloaded_audio(std::sync::Arc::new(vec![0.; 4800]), 1, 48000,
                                       Duration::from_millis(100));
        }
        if prepare {
            cue.preload(&ctx).unwrap();
        }
        calls.lock().unwrap().clear();
        cue.set_pre_wait(Duration::from_millis(40));
        let id = cue.id();
        let mut list = list_of(vec![cue]);
        let mut transport = Transport::new(ctx.clone());
        list.set_playhead(Some(id)).unwrap();
        transport.go(&mut list).unwrap();
        std::thread::sleep(Duration::from_millis(4));
        transport.pause_cue(&mut list, &id).unwrap();
        transport.pause_cue(&mut list, &id).unwrap();

        let held = list.get(&id).unwrap().elapsed();
        assert!(held > Duration::ZERO);
        assert_eq!(list.get(&id).unwrap().state(), CueState::Paused);
        std::thread::sleep(Duration::from_millis(50));
        let old = list.get_mut(&id).unwrap();
        old.tick(&ctx).unwrap();
        assert_eq!(old.elapsed(), held);
        let mut edited = registry.from_json(old.serialize()).unwrap();
        if let Some((samples, channels, rate, duration)) = old.extract_decoded_audio() {
            edited.accept_preloaded_audio(samples, channels, rate, duration);
        }
        edited.inherit_playback(old);
        assert_eq!(edited.elapsed(), held);
        assert_eq!(edited.action_elapsed(), Duration::ZERO);
        list.replace_cue_recursive(&id, edited);
        assert!(calls.lock().unwrap().is_empty());
        assert!(events.try_iter().all(|event| !matches!(event, CueEvent::ActionStarted { .. })));

        // Keep a positive remaining interval while checking Resume, independent
        // of scheduler latency. Then drive the deadline through the normal tick.
        list.get_mut(&id).unwrap().set_pre_wait(held + Duration::from_secs(30));
        transport.resume_cue(&mut list, &id).unwrap();
        transport.resume_cue(&mut list, &id).unwrap();
        let cue = list.get_mut(&id).unwrap();
        assert_eq!(cue.state(), CueState::Running);
        assert!(cue.elapsed() >= held);
        assert_eq!(cue.action_elapsed(), Duration::ZERO);
        cue.tick(&ctx).unwrap();
        assert!(calls.lock().unwrap().is_empty());
        cue.set_pre_wait(held);
        cue.tick(&ctx).unwrap();
        assert!(cue.is_action_started());
        assert_eq!(events.try_iter().filter(|e| matches!(e, CueEvent::ActionStarted { .. })).count(), 1);
        let startup = calls.lock().unwrap();
        match kind {
            CueType::Video if prepare => assert!(matches!(startup.as_slice(), [EngineCall::OutputStartPreloaded])),
            CueType::Video => assert!(matches!(startup.as_slice(), [EngineCall::OutputShowContent { preload: false, .. }])),
            CueType::Audio => assert!(matches!(startup.as_slice(), [EngineCall::AudioPlayRouted { .. }])),
            CueType::Mic => assert!(matches!(startup.as_slice(), [EngineCall::AudioEnsureInputFeed, EngineCall::AudioPlayMicVoice])),
            _ => unreachable!(),
        }
        drop(startup);
        if kind == CueType::Mic {
            calls.lock().unwrap().clear();
            transport.pause_cue(&mut list, &id).unwrap();
            transport.pause_cue(&mut list, &id).unwrap();
            let paused_action = list.get(&id).unwrap().action_elapsed();
            std::thread::sleep(Duration::from_millis(3));
            assert_eq!(list.get(&id).unwrap().action_elapsed(), paused_action);
            transport.resume_cue(&mut list, &id).unwrap();
            transport.resume_cue(&mut list, &id).unwrap();
            assert!(matches!(calls.lock().unwrap().as_slice(),
                             [EngineCall::AudioPauseVoice, EngineCall::AudioResumeVoice]));
        }
    }
}
