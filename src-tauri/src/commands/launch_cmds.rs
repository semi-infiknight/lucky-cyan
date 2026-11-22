//! File manager and positional document opens. Playback still requires transport.
use std::{path::{Path, PathBuf}, sync::Mutex};

#[derive(Default)]
pub struct LaunchDocument(pub Mutex<Option<Result<String, String>>>);

pub fn document_argument<'a>(args: impl IntoIterator<Item = &'a str>, cwd: &Path) -> Option<Result<String, String>> {
    let mut files = args.into_iter().filter_map(|arg| {
        if arg.starts_with('-') { return None; }
        let path = if arg.starts_with("file:") {
            tauri::Url::parse(arg).ok()?.to_file_path().ok()?
        } else {
            PathBuf::from(arg)
        };
        let ext = path.extension()?.to_str()?;
        (ext.eq_ignore_ascii_case("inkue") || ext.eq_ignore_ascii_case("wincue"))
            .then(|| cwd.join(path).to_string_lossy().into_owned())
    });
    let first = files.next()?;
    Some(if files.next().is_some() {
        Err("Open one workspace at a time.".to_owned())
    } else { Ok(first) })
}

#[tauri::command]
pub fn take_launch_document(state: tauri::State<'_, LaunchDocument>) -> Result<Option<String>, String> {
    state.0.lock().map_err(|e| e.to_string())?.take().transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_accepts_one_local_document_and_resolves_its_origin() {
        let cwd = Path::new("/tmp/shows");
        assert_eq!(document_argument(["Evening rehearsal.INKUE"], cwd).unwrap().unwrap(),
            cwd.join("Evening rehearsal.INKUE").to_string_lossy());
        #[cfg(unix)]
        assert_eq!(document_argument(["file:///tmp/Evening%20show.inkue"], cwd).unwrap().unwrap(),
            "/tmp/Evening show.inkue");
        assert!(document_argument(["a.inkue", "b.inkue"], cwd).unwrap().is_err());
        assert!(document_argument(["--help", "script.sh"], cwd).is_none());
    }
}
