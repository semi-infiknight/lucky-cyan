use crate::show::Show;
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    version: u32,
    workspace_sha256: String,
    pub media: BTreeMap<String, String>,
}

fn checksum(path: &Path) -> Result<String> {
    ensure!(path.is_file(), "expected a regular file");
    let mut file = File::open(path)?;
    ensure!(file.metadata()?.is_file(), "expected a regular file");
    let mut hash = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hash.update(&buf[..read]);
    }
    Ok(hex_digest(&hash.finalize()))
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(result, "{byte:02x}").expect("writing to String");
    }
    result
}

impl Manifest {
    pub fn from_workspace(path: &Path) -> Result<Self> {
        let workspace = path.canonicalize()?;
        let root = workspace
            .parent()
            .context("workspace needs a parent directory")?;
        let bytes = fs::read(&workspace)?;
        let show = Show::from_reader(bytes.as_slice())?;
        let mut media = BTreeMap::new();
        for name in show.media_paths()? {
            let relative = Path::new(name);
            ensure!(
                relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_) | Component::CurDir)),
                "cue media must be collected inside the project: {name}"
            );
            let path = root
                .join(relative)
                .canonicalize()
                .with_context(|| format!("locate {name}"))?;
            ensure!(
                path.starts_with(root),
                "media resolves outside the project: {name}"
            );
            media.insert(
                name.to_owned(),
                checksum(&path).with_context(|| format!("hash {name}"))?,
            );
        }
        Ok(Self {
            version: 1,
            workspace_sha256: hex_digest(&Sha256::digest(bytes)),
            media,
        })
    }

    pub fn compare(&self, manifest_path: &Path) -> Result<()> {
        let expected: Self = serde_json::from_reader(fs::File::open(manifest_path)?)
            .context("read reference Inkue file manifest")?;
        ensure!(
            expected.version == self.version,
            "unsupported reference manifest version"
        );
        for (name, hash) in &expected.media {
            match self.media.get(name) {
                Some(actual) if actual == hash => {}
                Some(_) => bail!("media bytes differ: {name}"),
                None => bail!("reference media omitted by this show: {name}"),
            }
        }
        for name in self.media.keys() {
            ensure!(
                expected.media.contains_key(name),
                "additional media in this show: {name}"
            );
        }
        ensure!(
            self.workspace_sha256 == expected.workspace_sha256,
            "workspace bytes differ"
        );
        Ok(())
    }
}
