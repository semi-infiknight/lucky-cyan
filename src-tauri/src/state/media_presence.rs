//! Missing-media indicators are advisory. Only the worker touches the filesystem.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const RECHECK: Duration = Duration::from_secs(2);
const RETAIN: Duration = Duration::from_secs(120);

struct Entry {
    missing: bool,
    pending: bool,
    checked: Option<Instant>,
    requested: Instant,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<PathBuf, Entry>,
    queued: Vec<PathBuf>,
    running: bool,
    last_prune: Option<Instant>,
}

#[derive(Clone, Default)]
pub(crate) struct MediaPresence(Arc<Mutex<Cache>>);

impl MediaPresence {
    pub(crate) fn collect<T>(
        &self,
        read: impl FnOnce(&mut dyn FnMut(&Path) -> bool) -> T,
        changed: impl Fn() + Send + 'static,
    ) -> T {
        self.collect_with(read, Path::exists, changed)
    }

    fn collect_with<T>(
        &self,
        read: impl FnOnce(&mut dyn FnMut(&Path) -> bool) -> T,
        exists: impl Fn(&Path) -> bool + Send + 'static,
        changed: impl Fn() + Send + 'static,
    ) -> T {
        let mut cache = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if cache
            .last_prune
            .is_none_or(|last| now.duration_since(last) >= RETAIN)
        {
            cache
                .entries
                .retain(|_, e| e.pending || now.duration_since(e.requested) < RETAIN);
            cache.last_prune = Some(now);
        }
        let value = read(&mut |path| {
            let e = cache.entries.entry(path.to_owned()).or_insert(Entry {
                missing: false,
                pending: false,
                checked: None,
                requested: now,
            });
            e.requested = now;
            let missing = e.missing;
            if !e.pending
                && e.checked
                    .is_none_or(|last| now.duration_since(last) >= RECHECK)
            {
                e.pending = true;
                cache.queued.push(path.to_owned());
            }
            missing
        });
        if cache.running || cache.queued.is_empty() {
            return value;
        }
        cache.running = true;
        drop(cache);

        let worker = self.clone();
        if std::thread::Builder::new()
            .name("media-presence".into())
            .spawn(move || loop {
                let paths = {
                    let mut cache = worker.0.lock().unwrap_or_else(|e| e.into_inner());
                    if cache.queued.is_empty() {
                        cache.running = false;
                        return;
                    }
                    std::mem::take(&mut cache.queued)
                };
                let mut refresh = false;
                for path in paths {
                    let missing = !exists(&path);
                    let mut cache = worker.0.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(e) = cache.entries.get_mut(&path) {
                        refresh |= e.missing != missing;
                        e.missing = missing;
                        e.pending = false;
                        e.checked = Some(Instant::now());
                    }
                }
                if refresh {
                    changed();
                }
            })
            .is_err()
        {
            let mut cache = self.0.lock().unwrap_or_else(|e| e.into_inner());
            cache.running = false;
            cache.queued.clear();
            for e in cache.entries.values_mut() {
                e.pending = false;
            }
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn a_blocked_probe_leaves_the_desk_lock_available_and_coalesces_rereads() {
        let cache = MediaPresence::default();
        let (started, waiting) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let (changed, refresh) = mpsc::channel();
        let file = Path::new("/venue/share/unavailable.wav");
        let desk = Mutex::new(());
        {
            let _summary = desk.lock().unwrap();
            assert!(!cache.collect_with(
                |missing| missing(file),
                move |_| {
                    started.send(()).unwrap();
                    resume.recv().unwrap();
                    false
                },
                move || changed.send(()).unwrap()
            ));
        }
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        let go = desk
            .try_lock()
            .expect("GO can acquire the same desk lock during the blocked probe");
        assert!(!cache.collect_with(
            |missing| missing(file),
            |_| panic!("duplicate probe"),
            || {}
        ));
        drop(go);
        release.send(()).unwrap();
        refresh.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(cache.collect_with(
            |missing| missing(file),
            |_| panic!("rechecked too soon"),
            || {}
        ));
    }
}
