//! Run in a separate process so the report settings and single writer belong to this test.
#![cfg(target_os = "linux")]
mod common;

use inkue_lib::{
    cue::{audio_cue::AudioCue, traits::Cue},
    media_usage,
    show::Workspace,
};
use std::{fs, sync::Arc, time::Duration};

#[test]
fn report_starts_only_after_the_engine_accepts_a_voice_and_pairs_each_play() {
    let temp = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_CONFIG_HOME", temp.path());
    media_usage::init();
    tauri::async_runtime::block_on(media_usage::set_media_usage_enabled(true)).unwrap();
    let (mut context, _events, _engine_calls) = common::recording_context();
    let workspace = Workspace::new("Trial show");
    context.set_workspace(&workspace);
    let mut cue = AudioCue::new();
    cue.set_name("Audience entrance".into());
    cue.file_path = Some(temp.path().join("Entrance.wav"));
    assert!(cue.go(&context).is_err());
    cue.accept_preloaded_audio(
        Arc::new(vec![0.25; 48000]),
        1,
        48000,
        Duration::from_secs(1),
    );

    cue.set_pre_wait(Duration::from_secs(4));
    cue.go(&context).unwrap();
    cue.stop(&context).unwrap();

    cue.set_pre_wait(Duration::ZERO);
    cue.preload(&context).unwrap();
    cue.stop(&context).unwrap();

    cue.go(&context).unwrap();
    // An idle serialized copy shares the cue UUID but owns no observation.
    // Resetting it must leave the running play's pause, resume and stop intact.
    let mut idle_copy = common::full_registry().from_json(cue.serialize()).unwrap();
    idle_copy.reset().unwrap();
    cue.pause(&context).unwrap();
    cue.resume(&context).unwrap();
    cue.stop(&context).unwrap();
    cue.go(&context).unwrap();
    cue.reset().unwrap();

    cue.preload(&context).unwrap();
    cue.resume(&context).unwrap();
    cue.hard_stop(&context).unwrap();

    let output = temp.path().join("report.csv");
    tauri::async_runtime::block_on(media_usage::export_media_usage(output.clone())).unwrap();
    media_usage::shutdown();
    let data = fs::read_to_string(output).unwrap();
    assert_eq!(data.matches(",\"start\",").count(), 2);
    assert_eq!(data.matches(",\"first_seen_on_resume\",").count(), 1);
    assert_eq!(data.matches(",\"pause\",").count(), 1);
    assert_eq!(data.matches(",\"resume\",").count(), 1);
    assert_eq!(data.matches(",\"end\",").count(), 3);
    assert!(data.contains("\"hard_stop_requested\""));
    assert!(data.contains("\"released\""));
    assert!(data.contains("\"Trial show\""));
    assert!(data.contains("\"Audience entrance\""));
}
