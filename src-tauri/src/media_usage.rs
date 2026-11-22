//! Opt-in media usage on the show-control thread. Files are owned by a separate writer.
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, SecondsFormat, Utc};
use crossbeam_channel::{bounded, Sender};
use serde::Serialize;
use uuid::Uuid;

use crate::{cue::traits::Cue, show::Workspace};

const HEADER: &str = "utc,event,play_id,workspace,workspace_file,cue_id,cue_number,cue_name,cue_type,media_file,detail\r\n";
static SERVICE: OnceLock<Service> = OnceLock::new();

#[derive(Clone)]
pub(crate) struct Show {
    name: String,
    path: String,
}

#[derive(Clone)]
struct Play {
    id: Uuid,
    show: Arc<Show>,
    cue: Uuid,
    number: String,
    name: String,
    kind: String,
    media: String,
    paused: bool,
    voice: Option<Uuid>,
}

enum Action {
    Start(Play),
    Resume(Play),
    End(Uuid, &'static str),
    Paused(Uuid, bool),
    OutputError(Uuid, String),
    EndAll(&'static str),
}
type Response = Sender<Result<(), String>>;
enum Message {
    Event(DateTime<Utc>, Action),
    Configure(bool, Response),
    Snapshot(Sender<Result<Snapshot, String>>),
    Close(Response),
}

struct Service {
    enabled: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    error: Arc<Mutex<Option<String>>>,
    queue: Sender<Message>,
}

#[derive(Serialize)]
pub struct Status {
    enabled: bool,
    dropped_events: u64,
    error: Option<String>,
}

struct Journal {
    dir: PathBuf,
    file: Option<File>,
    active: HashMap<Uuid, Play>,
}

impl Journal {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            file: None,
            active: HashMap::new(),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("media-usage.csv")
    }

    fn enable(&mut self) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.path())?;
        if file.metadata()?.len() == 0 {
            file.write_all(HEADER.as_bytes())?;
        }
        fs::write(self.dir.join("enabled"), [])?;
        self.file = Some(file);
        Ok(())
    }

    fn disable(&mut self) -> io::Result<()> {
        let marker = self.dir.join("enabled");
        if marker.exists() {
            fs::remove_file(marker)?;
        }
        self.event(Utc::now(), Action::EndAll("logging_disabled"))?;
        self.file = None;
        Ok(())
    }

    fn event(&mut self, at: DateTime<Utc>, action: Action) -> io::Result<()> {
        if self.file.is_none() {
            return Ok(());
        }
        match action {
            Action::Start(play) => {
                self.end(at, play.cue, "restarted")?;
                self.write(at, &play, "start", "")?;
                self.active.insert(play.cue, play);
            }
            Action::Resume(play) => {
                if self.active.contains_key(&play.cue) {
                    self.event(at, Action::Paused(play.cue, false))?;
                } else {
                    self.write(at, &play, "first_seen_on_resume", "")?;
                    self.active.insert(play.cue, play);
                }
            }
            Action::End(id, reason) => self.end(at, id, reason)?,
            Action::Paused(id, paused) => {
                if let Some(play) = self.active.get_mut(&id) {
                    if play.paused != paused {
                        play.paused = paused;
                        let current = play.clone();
                        self.write(at, &current, if paused { "pause" } else { "resume" }, "")?;
                    }
                }
            }
            Action::OutputError(voice, detail) => {
                let play = self
                    .active
                    .values()
                    .find(|p| p.voice == Some(voice))
                    .cloned();
                if let Some(play) = play {
                    self.write(at, &play, "output_error", &detail)?;
                }
            }
            Action::EndAll(reason) => {
                for id in self.active.keys().copied().collect::<Vec<_>>() {
                    self.end(at, id, reason)?;
                }
            }
        }
        Ok(())
    }

    fn end(&mut self, at: DateTime<Utc>, id: Uuid, reason: &str) -> io::Result<()> {
        if let Some(play) = self.active.remove(&id) {
            self.write(at, &play, "end", reason)?;
        }
        Ok(())
    }

    fn write(
        &mut self,
        at: DateTime<Utc>,
        play: &Play,
        event: &str,
        reason: &str,
    ) -> io::Result<()> {
        let fields = [
            at.to_rfc3339_opts(SecondsFormat::Millis, true),
            event.into(),
            play.id.to_string(),
            play.show.name.clone(),
            play.show.path.clone(),
            play.cue.to_string(),
            play.number.clone(),
            play.name.clone(),
            play.kind.clone(),
            play.media.clone(),
            reason.into(),
        ];
        let mut row = fields.map(|value| csv_cell(&value)).join(",");
        row.push_str("\r\n");
        self.file
            .as_mut()
            .expect("enabled journal")
            .write_all(row.as_bytes())
    }

    fn snapshot(&self) -> io::Result<Snapshot> {
        let path = self.path();
        let input = match File::open(&path) {
            Ok(file) => {
                let size = file.metadata()?.len();
                Some(file.take(size))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        Ok(Snapshot { input, path })
    }
}

// Quoting protects CSV structure. A separate text prefix prevents spreadsheet
// evaluation of imported labels, cue numbers and unusual relative filenames.
fn csv_cell(value: &str) -> String {
    let content = value.trim_start_matches(|c: char| c.is_whitespace() || c.is_control() || c == '\u{feff}');
    let prefix = if content.starts_with(['=', '+', '-', '@']) { "'" } else { "" };
    format!("\"{}{}\"", prefix, value.replace('"', "\"\""))
}

struct Snapshot {
    input: Option<io::Take<File>>,
    path: PathBuf,
}

impl Snapshot {
    // The worker only marks a byte boundary. Copying years of reporting history
    // proceeds on the export thread while new playback events keep appending.
    fn export(mut self, target: &Path) -> io::Result<()> {
        let dir = target
            .parent()
            .ok_or_else(|| io::Error::other("Choose a file path"))?;
        if target.exists()
            && self.path.exists()
            && fs::canonicalize(target)? == fs::canonicalize(&self.path)?
        {
            return Err(io::Error::other(
                "Choose an export location outside the active journal",
            ));
        }
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        if let Some(input) = self.input.as_mut() {
            io::copy(input, &mut tmp)?;
        } else {
            tmp.write_all(HEADER.as_bytes())?;
        }
        tmp.persist(target)?;
        Ok(())
    }
}

impl Service {
    fn start(dir: PathBuf) -> Self {
        let (queue, rx) = bounded::<Message>(8192);
        let mut journal = Journal::new(dir);
        let restore = journal.dir.join("enabled").is_file();
        let initial_error = if restore {
            journal.enable().err().map(|e| e.to_string())
        } else {
            None
        };
        let enabled = Arc::new(AtomicBool::new(restore && initial_error.is_none()));
        let error = Arc::new(Mutex::new(initial_error));
        let dropped = Arc::new(AtomicU64::new(0));
        let writer_dropped = dropped.clone();
        let writer_enabled = enabled.clone();
        let writer_error = error.clone();
        std::thread::spawn(move || {
            let mut reported_drops = 0;
            while let Ok(message) = rx.recv() {
                let total = writer_dropped.load(Ordering::Relaxed);
                if total != reported_drops {
                    if let Some(file) = journal.file.as_mut() {
                        let gap = format!(
                            "{},gap,,,,,,,,,{} events dropped\r\n",
                            Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                            total - reported_drops
                        );
                        if file.write_all(gap.as_bytes()).is_ok() {
                            reported_drops = total;
                        }
                    }
                }
                match message {
                    Message::Event(at, action) => {
                        if let Err(e) = journal.event(at, action) {
                            crate::health::set(crate::health::HealthAlert::new(
                                "media-usage-write",
                                crate::health::HealthLevel::Warning,
                                format!("Media usage recording stopped. {e}"),
                            ));
                            *writer_error.lock().unwrap() = Some(e.to_string());
                            journal.file = None;
                            journal.active.clear();
                            writer_enabled.store(false, Ordering::Release);
                        }
                    }
                    Message::Configure(value, ack) => {
                        let result = if value {
                            journal.enable()
                        } else {
                            journal.disable()
                        };
                        let result = result.map_err(|e| e.to_string());
                        *writer_error.lock().unwrap() = result.as_ref().err().cloned();
                        writer_enabled.store(journal.file.is_some(), Ordering::Release);
                        if result.is_ok() {
                            crate::health::clear("media-usage-write");
                        }
                        let _ = ack.send(result);
                    }
                    Message::Snapshot(ack) => {
                        let _ = ack.send(journal.snapshot().map_err(|e| e.to_string()));
                    }
                    Message::Close(ack) => {
                        let result = journal.event(Utc::now(), Action::EndAll("desk_closed"));
                        let _ = ack.send(result.map_err(|e| e.to_string()));
                        break;
                    }
                }
            }
        });
        Self {
            enabled,
            dropped,
            error,
            queue,
        }
    }

    fn event(&self, action: Action) {
        if self
            .queue
            .try_send(Message::Event(Utc::now(), action))
            .is_err()
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn export(&self, path: &Path) -> Result<(), String> {
        let (tx, rx) = bounded(1);
        self.queue
            .send(Message::Snapshot(tx))
            .map_err(|_| "Media report writer stopped".to_string())?;
        let snapshot = rx
            .recv()
            .map_err(|_| "Media report writer stopped".to_string())??;
        snapshot.export(path).map_err(|e| e.to_string())
    }

    fn request(&self, command: impl FnOnce(Response) -> Message) -> Result<(), String> {
        let (tx, rx) = bounded(1);
        self.queue
            .send(command(tx))
            .map_err(|_| "Media report writer stopped".to_string())?;
        rx.recv()
            .map_err(|_| "Media report writer stopped".to_string())?
    }
}

pub fn init() {
    SERVICE.get_or_init(|| {
        Service::start(
            crate::machine_config::config_base_dir()
                .join("Inkue")
                .join("reports"),
        )
    });
}

fn enabled() -> Option<&'static Service> {
    SERVICE.get().filter(|s| s.enabled.load(Ordering::Acquire))
}

pub(crate) fn show(ws: &Workspace) -> Option<Arc<Show>> {
    enabled().map(|_| {
        Arc::new(Show {
            name: ws.metadata.name.clone(),
            path: ws
                .file_path
                .as_deref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        })
    })
}

fn observe(show: Option<&Arc<Show>>, cue: &dyn Cue, action: fn(Play) -> Action) {
    if let (Some(service), Some(show), Some(path)) = (enabled(), show, cue.media_file_path()) {
        service.event(action(Play {
            id: Uuid::new_v4(),
            show: show.clone(),
            cue: cue.id(),
            number: cue.number().unwrap_or("").into(),
            name: cue.name().into(),
            kind: cue.cue_type().to_string(),
            media: path.to_string_lossy().into_owned(),
            voice: cue.playing_voice_id(),
            paused: false,
        }));
    }
}

pub(crate) fn start(show: Option<&Arc<Show>>, cue: &dyn Cue) {
    observe(show, cue, Action::Start);
}

pub(crate) fn resume(show: Option<&Arc<Show>>, cue: &dyn Cue) {
    observe(show, cue, Action::Resume);
}

pub(crate) fn paused(cue: Uuid, value: bool) {
    if let Some(s) = enabled() {
        s.event(Action::Paused(cue, value));
    }
}

pub(crate) fn output_error(voice: Uuid, detail: &str) {
    if let Some(s) = enabled() {
        s.event(Action::OutputError(voice, detail.to_string()));
    }
}

pub(crate) fn end(cue: &dyn Cue, reason: &'static str) {
    if let Some(s) = enabled() {
        // Hard stop resets every row, even in long lists of untouched media.
        // Only a live cue or a stale engine handle can own an observation.
        if cue.is_running() || cue.is_paused() || cue.playing_voice_id().is_some() {
            s.event(Action::End(cue.id(), reason));
        }
    }
}

pub fn shutdown() {
    if let Some(s) = SERVICE.get() {
        s.enabled.store(false, Ordering::Release);
        let deadline = Instant::now() + Duration::from_millis(750);
        let (tx, rx) = bounded(1);
        if s.queue.send_deadline(Message::Close(tx), deadline).is_ok() {
            let _ = rx.recv_deadline(deadline);
        }
    }
}

#[tauri::command]
pub fn media_usage_status() -> Status {
    let s = SERVICE.get().expect("media usage initialized");
    Status {
        enabled: s.enabled.load(Ordering::Acquire),
        dropped_events: s.dropped.load(Ordering::Relaxed),
        error: s.error.lock().unwrap().clone(),
    }
}

#[tauri::command]
pub async fn set_media_usage_enabled(enabled: bool) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        SERVICE
            .get()
            .unwrap()
            .request(|ack| Message::Configure(enabled, ack))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn export_media_usage(path: PathBuf) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || SERVICE.get().unwrap().export(&path))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_text_handles_operator_labels_without_evaluating_them() {
        assert_eq!(csv_cell("  =example"), "\"'  =example\"");
        assert_eq!(csv_cell("\t+scene"), "\"'\t+scene\"");
        assert_eq!(csv_cell("1.5"), "\"1.5\"");
        assert_eq!(csv_cell("Act \"B\""), "\"Act \"\"B\"\"\"");
    }

    fn play(cue: Uuid, name: &str) -> Play {
        Play {
            id: Uuid::new_v4(),
            cue,
            show: Arc::new(Show {
                name: "Opening night".into(),
                path: "/tour/show.inkue".into(),
            }),
            number: "1.5".into(),
            name: name.into(),
            kind: "audio".into(),
            media: "/tour/entrance.wav".into(),
            voice: Some(cue),
            paused: false,
        }
    }

    #[test]
    fn retrigger_stop_and_duplicate_stop_produce_distinct_paired_plays() {
        let tmp = tempfile::tempdir().unwrap();
        let mut journal = Journal::new(tmp.path().into());
        journal.enable().unwrap();
        let cue = Uuid::new_v4();
        let first = play(cue, "Crowd, \"offstage\"");
        let second = play(cue, "Crowd, \"offstage\"");
        let first_id = first.id.to_string();
        let second_id = second.id.to_string();
        journal.event(Utc::now(), Action::Start(first)).unwrap();
        journal
            .event(
                Utc::now(),
                Action::OutputError(cue, "renderer failed".into()),
            )
            .unwrap();
        journal
            .event(Utc::now(), Action::Paused(cue, true))
            .unwrap();
        journal
            .event(Utc::now(), Action::Paused(cue, true))
            .unwrap();
        journal
            .event(Utc::now(), Action::Paused(cue, false))
            .unwrap();
        journal.event(Utc::now(), Action::Start(second)).unwrap();
        journal
            .event(Utc::now(), Action::End(cue, "stop_requested"))
            .unwrap();
        journal
            .event(Utc::now(), Action::End(cue, "released"))
            .unwrap();
        let data = fs::read_to_string(journal.path()).unwrap();
        assert_eq!(data.lines().count(), 8);
        assert_eq!(data.matches(&first_id).count(), 5);
        assert_eq!(data.matches(&second_id).count(), 2);
        assert!(data.contains("\"Crowd, \"\"offstage\"\"\""));
        assert!(data.contains("\"restarted\""));
        assert!(data.contains("\"output_error\""));
        assert!(data.contains("\"stop_requested\""));
    }

    #[test]
    fn opt_in_persists_and_exports_a_stable_snapshot_without_blocking_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("reports");
        let service = Service::start(dir.clone());
        assert!(!service.enabled.load(Ordering::Acquire));
        assert!(!dir.exists());
        service
            .request(|ack| Message::Configure(true, ack))
            .unwrap();
        service.event(Action::Start(play(Uuid::new_v4(), "opening")));
        service.request(Message::Close).unwrap();
        let restored = Service::start(dir.clone());
        assert!(restored.enabled.load(Ordering::Acquire));
        let output = tmp.path().join("result.csv");
        restored.export(&output).unwrap();
        let first = fs::read_to_string(&output).unwrap();
        assert!(first.contains("\"desk_closed\""));
        restored.event(Action::Start(play(Uuid::new_v4(), "encore")));
        restored
            .request(|ack| Message::Configure(false, ack))
            .unwrap();
        assert!(!dir.join("enabled").exists());
        assert_eq!(fs::read_to_string(&output).unwrap(), first);
        restored.export(&output).unwrap();
        assert!(fs::read_to_string(output)
            .unwrap()
            .contains("\"logging_disabled\""));
        assert!(restored.export(&dir.join("media-usage.csv")).is_err());
        restored.request(Message::Close).unwrap();
    }

    #[test]
    fn snapshot_length_excludes_concurrent_events_and_disabled_capture_has_no_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let mut journal = Journal::new(tmp.path().join("local"));
        journal
            .event(Utc::now(), Action::Start(play(Uuid::new_v4(), "muted")))
            .unwrap();
        journal.enable().unwrap();
        let before = journal.snapshot().unwrap();
        journal
            .event(Utc::now(), Action::Start(play(Uuid::new_v4(), "live")))
            .unwrap();
        let export = tmp.path().join("snapshot.csv");
        before.export(&export).unwrap();
        assert_eq!(fs::read_to_string(export).unwrap(), HEADER);
        assert!(!fs::read_to_string(journal.path())
            .unwrap()
            .contains("muted"));
    }
}
