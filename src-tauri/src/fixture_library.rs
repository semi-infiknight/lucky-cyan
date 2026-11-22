use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::engine::fixture::{decode_fixture_profile, encode_fixture_profile, FixtureType};

const MAX_PROFILE_BYTES: usize = 512 * 1024;

pub(crate) fn folder() -> PathBuf {
    crate::machine_config::config_base_dir().join("Inkue").join("fixture-library")
}

#[derive(Serialize)]
pub struct FixtureLibraryEntry {
    pub id: String,
    pub name: String,
    pub manufacturer: String,
    pub mode: String,
    pub channels: u16,
}

impl FixtureLibraryEntry {
    fn from_profile(id: String, profile: FixtureType) -> Self {
        let channels = profile.footprint();
        Self { id, name: profile.name, manufacturer: profile.manufacturer, mode: profile.mode, channels }
    }
}

#[derive(Default, Serialize)]
pub struct FixtureLibrary {
    pub profiles: Vec<FixtureLibraryEntry>,
    pub unreadable: usize,
}

pub(crate) fn read_profile_file(path: &Path) -> Result<FixtureType, String> {
    decode_fixture_profile(&read_profile_bytes(path)?)
}

fn read_profile_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = std::fs::metadata(path).map_err(|error| format!("Could not open fixture profile: {error}"))?;
    if !metadata.is_file() {
        return Err("Choose a fixture profile file.".into());
    }
    let file = std::fs::File::open(path).map_err(|error| format!("Could not open fixture profile: {error}"))?;
    if !file.metadata().map_err(|error| format!("Could not inspect fixture profile: {error}"))?.is_file() {
        return Err("Choose a fixture profile file.".into());
    }
    let mut bytes = Vec::new();
    file.take((MAX_PROFILE_BYTES + 1) as u64).read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read fixture profile: {error}"))?;
    check_size(&bytes)?;
    Ok(bytes)
}

pub(crate) fn profile_bytes(profile: &FixtureType) -> Result<Vec<u8>, String> {
    let bytes = encode_fixture_profile(profile)?;
    check_size(&bytes)?;
    Ok(bytes)
}

fn check_size(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_PROFILE_BYTES {
        Err("The fixture profile exceeds 512 KiB.".into())
    } else {
        Ok(())
    }
}

fn entry_id(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn entry_path(folder: &Path, id: &str) -> Result<PathBuf, String> {
    if id.len() != 64 || !id.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
        return Err("Invalid fixture library profile ID.".into());
    }
    Ok(folder.join(format!("{id}.inkue-fixture")))
}

pub(crate) fn save(folder: &Path, profile: FixtureType) -> Result<FixtureLibraryEntry, String> {
    let bytes = profile_bytes(&profile)?;
    let id = entry_id(&bytes);
    std::fs::create_dir_all(folder).map_err(|error| format!("Could not open the fixture library: {error}"))?;
    let path = entry_path(folder, &id)?;
    match crate::recovery::write_new_document(&path, &bytes) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if read_entry_bytes(folder, &id)? != bytes {
                return Err("This fixture library entry has changed on disk.".into());
            }
        }
        Err(error) => return Err(format!("Could not save to the fixture library: {error}")),
    }
    Ok(FixtureLibraryEntry::from_profile(id, profile))
}

fn read_entry_bytes(folder: &Path, id: &str) -> Result<Vec<u8>, String> {
    let path = entry_path(folder, id)?;
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("Could not open fixture library entry: {error}"))?;
    if !metadata.is_file() {
        return Err("The fixture library entry must be a regular file.".into());
    }
    let bytes = read_profile_bytes(&path)?;
    if entry_id(&bytes) != id {
        return Err("This fixture library entry has changed on disk.".into());
    }
    Ok(bytes)
}

pub(crate) fn read(folder: &Path, id: &str) -> Result<FixtureType, String> {
    decode_fixture_profile(&read_entry_bytes(folder, id)?)
}

pub(crate) fn list(folder: &Path) -> Result<FixtureLibrary, String> {
    let mut library = FixtureLibrary::default();
    let files = match std::fs::read_dir(folder) {
        Ok(files) => files,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(library),
        Err(error) => return Err(format!("Could not open the fixture library: {error}")),
    };
    for file in files {
        let Ok(file) = file else {
            library.unreadable += 1;
            continue;
        };
        let path = file.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("inkue-fixture") {
            continue;
        }
        if let Some((id, profile)) = path.file_stem().and_then(|stem| stem.to_str())
            .and_then(|id| read(folder, id).ok().map(|profile| (id, profile)))
        {
            library.profiles.push(FixtureLibraryEntry::from_profile(id.to_owned(), profile));
        } else {
            library.unreadable += 1;
        }
    }
    library.profiles.sort_by(|a, b|
        (&a.manufacturer, &a.name, &a.mode, &a.id).cmp(&(&b.manufacturer, &b.name, &b.mode, &b.id)));
    Ok(library)
}

pub(crate) fn remove(folder: &Path, id: &str) -> Result<(), String> {
    let path = entry_path(folder, id)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not remove the fixture library entry: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::fixture::{builtin_fixture_types, PatchedFixture};

    #[test]
    fn fixture_library_retains_distinct_profiles_and_leaves_copies_in_shows() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("profiles");
        assert!(list(&folder).unwrap().profiles.is_empty());

        let mut profile = builtin_fixture_types().into_iter()
            .find(|profile| profile.name.starts_with("Moving Head")).unwrap();
        profile.parameters[0].fine_channel_offset = Some(8);
        let first = save(&folder, profile.clone()).unwrap();
        assert_eq!(save(&folder, profile.clone()).unwrap().id, first.id);
        assert_eq!(first.channels, 9);
        let patched = PatchedFixture::new("Tour head", 3, 20, read(&folder, &first.id).unwrap());
        profile.mode = "Backup".into();
        let alternate = save(&folder, profile).unwrap();
        assert_ne!(first.id, alternate.id);
        assert_eq!(list(&folder).unwrap().profiles.len(), 2);

        std::fs::write(entry_path(&folder, &alternate.id).unwrap(), b"bad profile").unwrap();
        let library = list(&folder).unwrap();
        assert_eq!(library.unreadable, 1);
        assert_eq!(library.profiles[0].id, first.id);
        assert!(remove(&folder, "../profile").is_err());
        remove(&folder, &first.id).unwrap();
        assert!(read(&folder, &first.id).is_err());
        assert_eq!(patched.resolve_channel(0).unwrap().fine_channel, Some(27));
    }
}
