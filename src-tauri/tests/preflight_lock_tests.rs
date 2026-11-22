mod common;

use std::{
    path::PathBuf,
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};

use inkue_lib::{
    commands::preflight_cmds::check_workspace_with_probe,
    cue::{audio_cue::AudioCue, memo_cue::MemoCue, traits::Cue},
    show::{transport::Transport, Workspace},
};

#[test]
fn blocked_media_stat_allows_main_go_and_rejects_an_edited_show() {
    let mut ws = Workspace::new("Venue");
    let mut audio = AudioCue::new();
    audio.file_path = Some(PathBuf::from("offline/act.wav"));
    let memo = MemoCue::new();
    let memo_id = memo.id();
    let list = ws.active_cue_list_mut().unwrap();
    list.push(Box::new(memo));
    list.push(Box::new(audio));
    list.playhead_cue_id = Some(memo_id);

    let desk = Arc::new(Mutex::new(ws));
    let scanner = Arc::clone(&desk);
    let (started, waiting) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    let scan = std::thread::spawn(move || {
        check_workspace_with_probe(&scanner, Vec::new(), |_| {
            started.send(()).unwrap();
            resume.recv().unwrap();
            false
        })
    });
    waiting.recv_timeout(Duration::from_secs(5)).unwrap();
    let (context, _events, _calls) = common::recording_context();
    let mut transport = Transport::new(context);
    {
        let mut ws = desk
            .try_lock()
            .expect("the stalled stat must leave the main desk available");
        let result = transport.go(ws.active_cue_list_mut().unwrap()).unwrap();
        assert_eq!(result.triggered, vec![memo_id]);
        ws.mark_modified();
    }
    release.send(()).unwrap();
    assert!(scan.join().unwrap().unwrap_err().contains("show changed"));
}
