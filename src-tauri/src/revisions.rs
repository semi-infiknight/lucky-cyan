//! Machine-local rolling history of explicit saves and optional timed snapshots.
use std::{fs, io::ErrorKind, path::{Path, PathBuf}, sync::{Arc, Mutex}, time::{Duration, Instant}};
use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{cue::traits::Cue, preferences::RevisionInterval, show::Workspace};

const KEEP: usize = 12;
const HISTORY_BYTES: usize = 64 * 1024 * 1024;
const FORMAT: u8 = 1;
/// Order file captures and history commits for simultaneous Save requests.
pub(crate) static SAVE_ORDER: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RevisionInfo {
    pub id: Uuid,
    pub saved_at: DateTime<Utc>,
    pub bytes: usize,
    #[serde(default)]
    pub kind: RevisionKind,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RevisionKind {
    #[default]
    Save,
    Timed,
}

#[derive(Serialize, Deserialize)]
struct Revision {
    info: RevisionInfo,
    document: String,
}

#[derive(Serialize, Deserialize)]
struct History {
    format: u8,
    source: PathBuf,
    revisions: Vec<Revision>,
}

pub(crate) struct Store {
    root: PathBuf,
}

pub(crate) struct Previous {
    document: String,
    at: DateTime<Utc>,
}

pub(crate) fn live_store() -> Store {
    Store { root: crate::machine_config::config_base_dir().join("Inkue").join("saved-revisions") }
}

/// Resolve directory aliases but retain the file name. Save replaces the named
/// file atomically, including when that name was previously a symlink.
pub(crate) fn absolute_target(path: &Path) -> Result<PathBuf> {
    let name = path.file_name().context("Choose a workspace file name")?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    Ok(fs::canonicalize(parent)?.join(name))
}

pub(crate) fn previous(path: &Path) -> Result<Option<Previous>> {
    let metadata = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(metadata.len() <= HISTORY_BYTES as u64, "Earlier file exceeded the revision size limit");
    let document = fs::read_to_string(path)?;
    // Only native workspaces are revisions. A Save As may name a pre-existing
    // file with arbitrary contents, which is outside the managed history.
    let parsed: serde_json::Value = serde_json::from_str(&document)?;
    ensure!(parsed.get("workspace").is_some_and(|w| w.is_object()) &&
        parsed.get("cue_lists").is_some_and(|l| l.is_array()), "Earlier file was not a native workspace");
    Ok(Some(Previous { document, at: metadata.modified()?.into() }))
}

fn trim_revisions(revisions: &mut Vec<Revision>, budget: usize) {
    let mut bytes = revisions.iter().map(|r| r.document.len()).sum::<usize>();
    while revisions.len() > KEEP || bytes > budget {
        bytes -= revisions.remove(0).document.len();
    }
}

#[derive(Default)]
struct RevisionTimer {
    scope: Option<(Uuid, PathBuf, RevisionInterval)>,
    due: Option<Instant>,
    last_revision: Option<u64>,
}

impl RevisionTimer {
    fn ready<'a>(&mut self, workspace: &'a Workspace, now: Instant) -> Option<(&'a Path, Duration)> {
        let interval = workspace.preferences.general.timed_revisions;
        let eligible = workspace.file_path.as_deref()
            .filter(|_| workspace.is_modified).zip(interval.duration());
        let Some((source, duration)) = eligible else {
            *self = Self::default();
            return None;
        };
        let same_scope = self.scope.as_ref().is_some_and(|(id, path, mode)| {
            *id == workspace.instance_id && path == source && *mode == interval
        });
        if !same_scope {
            *self = Self {
                scope: Some((workspace.instance_id, source.to_owned(), interval)),
                due: Some(now + duration),
                last_revision: None,
            };
            return None;
        }
        (self.due.is_some_and(|due| now >= due)
            && self.last_revision != Some(workspace.revision)
            && !workspace.cue_lists.iter().any(|list| playback_active(&list.cues)))
            .then_some((source, duration))
    }

    fn attempted(&mut self, now: Instant, duration: Duration, revision: Option<u64>) {
        self.due = Some(now + duration);
        if let Some(revision) = revision { self.last_revision = Some(revision); }
    }
}

fn playback_active(cues: &[Box<dyn Cue>]) -> bool {
    cues.iter().any(|cue| cue.is_running() || cue.is_paused()
        || cue.child_cues().is_some_and(playback_active))
}

pub(crate) struct RecoveryRevision {
    pub owner: (Uuid, u64),
    pub captured_at: DateTime<Utc>,
    pub json: String,
}

pub(crate) fn start_timed_revisions(
    workspace: Arc<Mutex<Workspace>>,
    recovery: Arc<Mutex<Option<Arc<RecoveryRevision>>>>,
) {
    std::thread::Builder::new().name("inkue-timed-revisions".into()).spawn(move || {
        let mut timer = RevisionTimer::default();
        loop {
            std::thread::sleep(Duration::from_secs(3));
            // Reuse the existing recovery capture. This read never serializes a
            // show, waits on the Save lock or writes disk with the desk locked.
            let Ok(show) = workspace.try_lock() else { continue };
            let now = Instant::now();
            let prior_scope = timer.scope.clone();
            let ready = timer.ready(&show, now);
            if timer.scope != prior_scope {
                crate::health::clear("timed-revision-history");
            }
            let Some((path, duration)) = ready else { continue };
            let path = path.to_owned();
            let owner = (show.instance_id, show.revision);
            let interval = show.preferences.general.timed_revisions;
            drop(show);

            let snapshot = recovery.lock().ok().and_then(|cached| cached.clone());
            let Some(snapshot) = snapshot.filter(|captured| captured.owner == owner) else { continue };
            let result = (|| {
                let source = absolute_target(&path)?;
                let document = timed_document(&snapshot.json, &source)?;
                live_store().record_kind(&source, None, document, RevisionKind::Timed, snapshot.captured_at)
            })();
            timer.attempted(Instant::now(), duration, matches!(result, Ok(true)).then_some(owner.1));
            let Ok(show) = workspace.try_lock() else { continue };
            if show.instance_id != owner.0 || show.file_path.as_deref() != Some(path.as_path())
                || show.preferences.general.timed_revisions != interval {
                continue;
            }
            match result {
                Ok(true) => crate::health::clear("timed-revision-history"),
                Ok(false) => {}
                Err(error) => crate::health::set(crate::health::HealthAlert::new(
                    "timed-revision-history", crate::health::HealthLevel::Warning,
                    format!("Could not record a timed revision: {error:#}"),
                )),
            }
        }
    }).expect("Failed to spawn timed revision thread");
}

fn timed_document(json: &str, source: &Path) -> Result<String> {
    ensure!(json.len() <= HISTORY_BYTES, "Workspace exceeded the revision size limit");
    let mut doc: serde_json::Value = serde_json::from_str(json)?;
    doc.as_object_mut().context("Missing recovery workspace")?.remove("recovery_original_path");
    let lists = doc.get_mut("cue_lists").and_then(|value| value.as_array_mut()).context("Missing cue lists")?;
    let base = source.parent().context("Original workspace directory is unknown")?;
    for list in lists {
        if let Some(cues) = list.get_mut("cues") {
            crate::show::workspace::relativize_paths(cues, base);
        }
    }
    Ok(serde_json::to_string_pretty(&doc)?)
}

impl Store {
    fn stem(&self, source: &Path) -> PathBuf {
        use std::fmt::Write;
        let mut key = String::with_capacity(64);
        for byte in Sha256::digest(source.as_os_str().as_encoded_bytes()) {
            write!(&mut key, "{byte:02x}").expect("format into a String");
        }
        self.root.join(key)
    }

    fn read(&self, source: &Path) -> Result<History> {
        let path = self.stem(source).with_extension("history");
        match fs::File::open(path) {
            Ok(file) => {
                ensure!(file.metadata()?.len() <= (2 * HISTORY_BYTES + 1024 * 1024) as u64,
                    "Revision archive exceeds its storage limit");
                let log: History = serde_json::from_reader(std::io::BufReader::new(file))?;
                ensure!(log.format == FORMAT && log.source == source, "Unrecognized revision archive");
                Ok(log)
            }
            Err(e) if e.kind() == ErrorKind::NotFound =>
                Ok(History { format: FORMAT, source: source.into(), revisions: vec![] }),
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) fn record(&self, source: &Path, before: Option<Previous>, saved: String) -> Result<()> {
        self.record_kind(source, before, saved, RevisionKind::Save, Utc::now())?;
        Ok(())
    }

    fn record_kind(&self, source: &Path, before: Option<Previous>, saved: String, kind: RevisionKind, captured_at: DateTime<Utc>) -> Result<bool> {
        ensure!(saved.len() <= HISTORY_BYTES, "Saved file exceeded the revision size limit");
        fs::create_dir_all(&self.root)?;
        let stem = self.stem(source);
        let marker = stem.with_extension("origin");
        if !marker.exists() {
            if let Err(e) = crate::recovery::write_new_document(&marker, &serde_json::to_vec(source)?) {
                if e.kind() != ErrorKind::AlreadyExists { return Err(e.into()); }
            }
        }
        // All processes updating this show's archive lock its stable origin file.
        // The atomically replaced history file itself cannot serve as the lock.
        let binding = fs::OpenOptions::new().read(true).write(true).open(&marker)?;
        if kind == RevisionKind::Timed {
            match binding.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        } else {
            binding.lock()?;
        }
        ensure!(serde_json::from_reader::<_, PathBuf>(&binding)? == source, "Revision origin mismatch");
        let mut log = self.read(source)?;
        if kind == RevisionKind::Timed && (fs::metadata(source).and_then(|m| m.modified()).ok()
            .is_some_and(|at| DateTime::<Utc>::from(at) > captured_at)
            || log.revisions.last().is_some_and(|v| v.info.saved_at > captured_at)) {
            return Ok(true);
        }
        let mut changed = false;
        let current = Previous { document: saved, at: Utc::now() };
        let previous = before.filter(|item| !log.revisions.iter().any(|v| v.document == item.document));
        for (item, kind) in previous.into_iter().map(|item| (item, RevisionKind::Save))
            .chain(std::iter::once((current, kind))) {
            if let Some(last) = log.revisions.last_mut().filter(|v| v.document == item.document) {
                if last.info.kind == RevisionKind::Timed && kind == RevisionKind::Save {
                    last.info.kind = kind;
                    last.info.saved_at = item.at;
                    changed = true;
                }
                continue;
            }
            let info = RevisionInfo { id: Uuid::new_v4(), saved_at: item.at, bytes: item.document.len(), kind };
            log.revisions.push(Revision { info, document: item.document });
            changed = true;
        }
        if changed {
            trim_revisions(&mut log.revisions, HISTORY_BYTES);
            crate::recovery::write_document(&stem.with_extension("history"), &serde_json::to_vec(&log)?)?;
        }
        Ok(true)
    }

    pub(crate) fn sources(&self) -> Result<Vec<PathBuf>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(d) => d,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut sources = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("origin") { continue; }
            if let Ok(source) = serde_json::from_slice::<PathBuf>(&fs::read(&path)?) {
                if self.stem(&source).with_extension("origin") == path && path.with_extension("history").is_file() {
                    sources.push(source);
                }
            }
        }
        sources.sort();
        Ok(sources)
    }

    pub(crate) fn list(&self, source: &Path) -> Result<Vec<RevisionInfo>> {
        Ok(self.read(source)?.revisions.into_iter().rev().map(|r| r.info).collect())
    }

    pub(crate) fn restore(&self, source: &Path, id: Uuid, destination: &Path, active: Option<&Path>) -> Result<()> {
        let target = absolute_target(destination)?;
        ensure!(target.extension().and_then(|s| s.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("inkue")),
            "Choose a new file ending in .inkue");
        if target == source || active.and_then(|p| absolute_target(p).ok()).as_ref() == Some(&target) {
            bail!("Choose a separate copy, outside the original or open workspace path");
        }
        let history = self.read(source)?;
        let entry = history.revisions.iter().find(|rev| rev.info.id == id)
            .context("That revision is no longer in this workspace's history")?;
        let old_base = source.parent().context("Original workspace directory is unknown")?;
        let new_base = target.parent().context("Choose a workspace directory")?;
        let bytes = if old_base == new_base {
            entry.document.as_bytes().to_vec()
        } else {
            let mut doc: serde_json::Value = serde_json::from_str(&entry.document)?;
            let lists = doc.get_mut("cue_lists").and_then(|v| v.as_array_mut()).context("Missing cue lists")?;
            for list in lists {
                if let Some(cues) = list.get_mut("cues") {
                    crate::show::workspace::absolutize_paths(cues, old_base);
                    crate::show::workspace::relativize_paths(cues, new_base);
                }
            }
            serde_json::to_vec_pretty(&doc)?
        };
        crate::recovery::write_new_document(&target, &bytes)
            .with_context(|| format!("Could not create a new revision copy at {}. Choose an unused filename", target.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn document(take: usize) -> String {
        serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "workspace": {"name": "Tour", "take": take},
            "cue_lists": [{"cues": [{"children": [
                {"file_path": "media/entrance.wav", "opaque_vendor_field": 17}
            ]}]}]
        })).unwrap()
    }

    fn setup() -> (tempfile::TempDir, Store, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        fs::create_dir(root.join("show")).unwrap();
        let store = Store { root: root.join("history") };
        (tmp, store, root.join("show/Tour.inkue"))
    }

    #[test]
    fn timed_revisions_require_a_saved_dirty_show_and_restart_with_the_open_instance() {
        let (_tmp, _store, source) = setup();
        let mut show = Workspace::new("Tour");
        let mut timer = RevisionTimer::default();
        let now = Instant::now();
        let minute = Duration::from_secs(60);
        show.mark_modified();
        show.preferences.general.timed_revisions = RevisionInterval::OneMinute;
        assert!(timer.ready(&show, now).is_none());
        show.file_path = Some(source.clone());
        assert!(timer.ready(&show, now).is_none());
        assert_eq!(timer.ready(&show, now + minute), Some((source.as_path(), minute)));
        timer.attempted(now + minute, minute, Some(show.revision));
        assert!(timer.ready(&show, now + minute * 2).is_none());
        show.mark_modified();
        assert!(timer.ready(&show, now + minute * 2).is_some());
        show.instance_id = Uuid::new_v4();
        assert!(timer.ready(&show, now + minute * 2).is_none());
        assert!(timer.ready(&show, now + minute * 3).is_some());
        show.preferences.general.timed_revisions = RevisionInterval::Off;
        assert!(timer.ready(&show, now + minute * 4).is_none());
    }

    #[test]
    fn timed_revisions_preserve_the_primary_file_and_a_matching_save_shares_the_entry() {
        let (_tmp, store, source) = setup();
        fs::write(&source, document(0)).unwrap();
        store.record(&source, None, document(0)).unwrap();
        store.record_kind(&source, None, document(1), RevisionKind::Timed, Utc::now()).unwrap();
        let history = store.list(&source).unwrap();
        assert_eq!(history[0].kind, RevisionKind::Timed);
        assert_eq!(fs::read_to_string(&source).unwrap(), document(0));
        let earlier = previous(&source).unwrap();
        store.record(&source, earlier, document(1)).unwrap();
        let saved = store.list(&source).unwrap();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].kind, RevisionKind::Save);
        assert_eq!(saved[0].id, history[0].id);
    }

    #[test]
    fn a_delayed_timed_snapshot_does_not_follow_a_newer_save() {
        let (_tmp, store, source) = setup();
        let capture = Utc::now() - chrono::Duration::seconds(10);
        fs::write(&source, document(2)).unwrap();
        store.record(&source, None, document(2)).unwrap();
        store.record_kind(&source, None, document(1), RevisionKind::Timed, capture).unwrap();
        assert_eq!(store.list(&source).unwrap().len(), 1);
        assert_eq!(store.list(&source).unwrap()[0].kind, RevisionKind::Save);
    }

    #[test]
    fn converting_a_recovery_snapshot_rebases_media_and_discards_its_recovery_pointer() {
        let (_tmp, _store, source) = setup();
        let media = source.with_file_name("entrance.wav");
        let recovery = json!({
            "recovery_original_path": source, "workspace": {"name": "Tour"},
            "cue_lists": [{"cues": [{"children": [{"file_path": media}]}]}],
        });
        let converted: Value = serde_json::from_str(&timed_document(&recovery.to_string(), &source).unwrap()).unwrap();
        assert!(converted.get("recovery_original_path").is_none());
        assert_eq!(converted["cue_lists"][0]["cues"][0]["children"][0]["file_path"], "entrance.wav");
    }

    #[test]
    fn distinct_committed_documents_roll_without_touching_user_files() {
        let (_tmp, store, source) = setup();
        fs::write(&source, b"primary show on disk").unwrap();
        let mut oldest = None;
        for generation in 0..KEEP + 2 {
            store.record(&source, None, document(generation)).unwrap();
            if oldest.is_none() { oldest = Some(store.list(&source).unwrap()[0].id); }
        }
        let list = store.list(&source).unwrap();
        assert_eq!(list.len(), KEEP);
        assert!(list.iter().all(|r| Some(r.id) != oldest));
        let latest = list[0].id;
        store.record(&source, None, document(KEEP + 1)).unwrap();
        assert_eq!(store.list(&source).unwrap()[0].id, latest);
        assert_eq!(fs::read(&source).unwrap(), b"primary show on disk");
        assert_eq!(store.sources().unwrap(), vec![source]);
    }

    #[test]
    fn earlier_file_and_current_save_survive_missing_original_and_relocate_nested_media() {
        let (tmp, store, source) = setup();
        let before = document(0);
        fs::write(&source, &before).unwrap();
        let prior = previous(&source).unwrap();
        store.record(&source, prior, document(1)).unwrap();
        fs::remove_file(&source).unwrap();
        let list = store.list(&source).unwrap();
        assert_eq!(list.len(), 2);
        let same_dir = source.with_file_name("Old take.inkue");
        store.restore(&source, list[1].id, &same_dir, None).unwrap();
        assert_eq!(fs::read_to_string(&same_dir).unwrap(), before);
        let other = tmp.path().join("Relocated.inkue");
        store.restore(&source, list[1].id, &other, None).unwrap();
        let restored: Value = serde_json::from_slice(&fs::read(&other).unwrap()).unwrap();
        let child = &restored["cue_lists"][0]["cues"][0]["children"][0];
        assert_eq!(child["file_path"], "show/media/entrance.wav");
        assert_eq!(child["opaque_vendor_field"], 17);
        assert_eq!(restored["workspace"]["take"], 0);
        assert!(store.restore(&source, list[1].id, &source, None).is_err());
        let active = tmp.path().join("Open.inkue");
        assert!(store.restore(&source, list[1].id, &active, Some(&active)).is_err());
        assert!(!active.exists());
        fs::write(&other, b"unrelated existing file").unwrap();
        assert!(store.restore(&source, list[1].id, &other, None).is_err());
        assert_eq!(fs::read(&other).unwrap(), b"unrelated existing file");
    }

    #[test]
    fn small_byte_budget_evicts_only_oldest_managed_versions() {
        let (_tmp, store, source) = setup();
        for i in 0..3 { store.record(&source, None, document(i)).unwrap(); }
        let mut revisions = store.read(&source).unwrap().revisions;
        let newest = revisions[2].info.id;
        let budget = revisions[1].document.len() + revisions[2].document.len();
        trim_revisions(&mut revisions, budget);
        assert_eq!(revisions.len(), 2);
        assert_eq!(revisions[1].info.id, newest);
    }

    #[test]
    fn damaged_history_is_preserved_and_other_sources_remain_independent() {
        let (_tmp, store, source) = setup();
        store.record(&source, None, document(1)).unwrap();
        let path = store.stem(&source).with_extension("history");
        fs::write(&path, b"broken archive").unwrap();
        assert!(store.record(&source, None, document(2)).is_err());
        assert_eq!(fs::read(path).unwrap(), b"broken archive");
        let other = source.with_file_name("Evening.inkue");
        store.record(&other, None, document(3)).unwrap();
        assert_eq!(store.list(&other).unwrap().len(), 1);
    }

    #[test]
    fn independent_writers_keep_both_concurrent_revisions() {
        let (_tmp, store, source) = setup();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for i in 0..2 {
                let (store, source, barrier) = (&store, &source, &barrier);
                scope.spawn(move || { barrier.wait(); store.record(source, None, document(i)).unwrap(); });
            }
        });
        assert_eq!(store.list(&source).unwrap().len(), 2);
    }
}
