mod common;

use std::{sync::Arc, time::Duration};

use common::{recording_context, EngineCall};
use inkue_lib::{
    cue::{
        audio_cue::AudioCue,
        fade_cue::FadeCue,
        point_mix::{AudioMap, AudioMapPatch, AudioMark, MapMotion, PointMix, Position},
        traits::Cue,
        types::ContinueMode,
    },
    engine::device_manager::OutputPatch,
    show::{cue_list::CueList, transport::Transport, Workspace},
};
use uuid::Uuid;

#[test]
fn a_map_motion_resolves_the_named_live_voice_and_preserves_its_saved_start() {
    let patch = OutputPatch::new("Rehearsal", "test:calibrated", vec![0, 1]);
    let mark = |x, y, levels| AudioMark {
        id: Uuid::new_v4(),
        name: format!("Mark {x}"),
        position: Position { x, y },
        levels,
    };
    let map = AudioMap {
        id: Uuid::new_v4(),
        revision: Uuid::new_v4(),
        name: "Rehearsal".into(),
        patch: AudioMapPatch::from_output_patch(&patch),
        marks: vec![mark(0., 10., vec![0., -60.]), mark(100., 90., vec![-60., 0.])],
    };
    let point = PointMix { input: 1, source: map.marks[0].position.clone(), speakers: vec![], map: Some(map.clone()) };
    let mut saved = vec![vec![], vec![-12., -8.]];
    point.apply(&mut saved, 2).unwrap();
    let mut workspace = Workspace::new("Rehearsal");
    workspace.audio_maps = vec![map.clone()];
    let (mut context, _, calls) = recording_context();
    context.output_patches = Arc::new(vec![patch.clone()]);
    context.set_workspace(&workspace);
    let mut list = CueList::new("Run");
    let mut audio = AudioCue::new();
    let target = audio.id();
    audio.output_patch_id = Some(patch.id);
    audio.point_mix = Some(point);
    audio.level_matrix = Some(saved.clone());
    audio.accept_preloaded_audio(Arc::new(vec![0.; 96000]), 2, 48000, Duration::from_secs(1));
    let mut fade = FadeCue::new();
    let motion = fade.id();
    fade.target_cue_ids = vec![target];
    fade.set_continue_mode(ContinueMode::DoNotContinue);
    fade.fade_volume = false;
    fade.fade_duration_ms = 0;
    fade.target_map_motion = Some(MapMotion {
        map_id: map.id, revision: map.revision, start_mark: map.marks[0].id, end_mark: map.marks[1].id,
    });
    list.push(Box::new(audio));
    list.push(Box::new(fade));
    let mut transport = Transport::new(context.clone());
    transport.go_by_id(&mut list, &target).unwrap();
    let voice = list.get(&target).unwrap().playing_voice_id().unwrap();
    transport.go_by_id(&mut list, &motion).unwrap();
    list.get_mut(&motion).unwrap().tick(&context).unwrap();
    assert!(list.get(&motion).unwrap().is_time_complete());
    assert!(calls.lock().unwrap().contains(&EngineCall::AudioMapMotionStart { voice, device: patch.device_id }));
    assert!(calls.lock().unwrap().contains(&EngineCall::AudioMapMotionUpdate { voice }));
    assert_eq!(list.get(&target).unwrap().serialize()["level_matrix"], serde_json::json!(saved));

    workspace.audio_maps.clear();
    context.set_workspace(&workspace);
    calls.lock().unwrap().clear();
    list.get_mut(&motion).unwrap().reset().unwrap();
    Transport::new(context.clone()).go_by_id(&mut list, &motion).unwrap();
    assert!(list.get(&motion).unwrap().is_time_complete());
    assert!(!calls.lock().unwrap().iter().any(|call| matches!(call, EngineCall::AudioMapMotionStart { .. })));
}
