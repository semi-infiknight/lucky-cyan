//! File-backed projector mattes are normalized once at import and embedded in
//! the show. Reopening and collecting the show never require the original PNG.
use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
    sync::Arc,
};

use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const MAX_DIM: u32 = 4096;
const MAX_FILE: usize = 32 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct StoredMask {
    name: String,
    png: String,
}

#[derive(Debug)]
struct MaskData {
    stored: StoredMask,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// Comparing render configurations every tick must never walk the pixels.
#[derive(Clone, Debug)]
pub struct MaskImage(Arc<MaskData>);
impl PartialEq for MaskImage {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Serialize for MaskImage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.stored.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for MaskImage {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let stored = StoredMask::deserialize(d)?;
        let load = || -> Result<Self> {
            ensure!(
                stored.png.len() <= MAX_FILE.div_ceil(3) * 4,
                "Embedded mask is too large"
            );
            let bytes = STANDARD
                .decode(&stored.png)
                .context("Invalid embedded mask data")?;
            let (width, height, pixels) = decode(&bytes)?;
            Ok(Self(Arc::new(MaskData {
                stored,
                width,
                height,
                pixels,
            })))
        };
        load().map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize)]
pub struct MaskPreview<'a> {
    name: &'a str,
    png: &'a str,
    width: u32,
    height: u32,
}

impl MaskImage {
    pub fn import_file(path: &Path) -> Result<Self> {
        ensure!(path.is_absolute(), "Choose an absolute PNG file path");
        let metadata = path.metadata().context("Cannot read mask file")?;
        ensure!(metadata.is_file(), "Select a PNG file");
        ensure!(
            metadata.len() <= MAX_FILE as u64,
            "Mask file exceeds 32 MiB"
        );
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_FILE as u64 + 1)
            .read_to_end(&mut bytes)?;
        let (width, height, pixels) = decode(&bytes)?;
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, width, height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&pixels)?;
        }
        let name = path
            .file_name()
            .context("Mask filename missing")?
            .to_string_lossy()
            .into_owned();
        let stored = StoredMask {
            name,
            png: STANDARD.encode(png),
        };
        Ok(Self(Arc::new(MaskData {
            stored,
            width,
            height,
            pixels,
        })))
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.0.width, self.0.height)
    }
    pub fn pixels(&self) -> &[u8] {
        &self.0.pixels
    }
    pub fn preview(&self) -> MaskPreview<'_> {
        MaskPreview {
            name: &self.0.stored.name,
            png: &self.0.stored.png,
            width: self.0.width,
            height: self.0.height,
        }
    }
}

fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    ensure!(bytes.len() <= MAX_FILE, "Mask file exceeds 32 MiB");
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let png = decoder
        .read_header_info()
        .context("Cannot read mask PNG header")?;
    ensure!(
        png.width <= MAX_DIM && png.height <= MAX_DIM,
        "Mask dimensions exceed 4096 pixels per side"
    );
    let mut reader = decoder.read_info().context("Cannot decode mask PNG")?;
    ensure!(
        reader.info().animation_control.is_none(),
        "Choose a still PNG for the mask"
    );
    let mut pixels = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut pixels)?;
    let channels = frame.color_type.samples();
    let source = &pixels[..frame.buffer_size()];
    let luma = match frame.color_type {
        png::ColorType::Grayscale => pixels,
        png::ColorType::Indexed => bail!("Could not expand indexed mask palette"),
        _ => source
            .chunks_exact(channels)
            .map(|p| {
                let (gray, alpha) = match p {
                    [gray, a] => (u32::from(*gray), u32::from(*a)),
                    [r, g, b, ..] => (
                        (54 * *r as u32 + 183 * *g as u32 + 19 * *b as u32 + 128) >> 8,
                        u32::from(p.get(3).copied().unwrap_or(255)),
                    ),
                    _ => unreachable!("PNG decoder supplied an unsupported channel layout"),
                };
                ((gray * alpha + 127) / 255) as u8
            })
            .collect(),
    };
    Ok((frame.width, frame.height, luma))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_assets_and_missing_paths_are_rejected() {
        assert!(MaskImage::import_file(Path::new("relative.png")).is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(MaskImage::import_file(&dir.path().join("missing.png")).is_err());
        assert!(MaskImage::import_file(dir.path()).is_err());
        let path = dir.path().join("broken.png");
        std::fs::write(&path, b"not a png").unwrap();
        assert!(MaskImage::import_file(&path).is_err());
        assert!(
            serde_json::from_value::<MaskImage>(serde_json::json!({"name":"bad","png":"!!!"}))
                .is_err()
        );
    }

    #[test]
    fn embedded_png_survives_removal_of_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scrim.png");
        let mut encoder = png::Encoder::new(File::create(&path).unwrap(), 3, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 128])
            .unwrap();
        let mask = MaskImage::import_file(&path).unwrap();
        assert_eq!(mask.pixels(), [255, 0, 128]);
        std::fs::remove_file(path).unwrap();
        let stored = serde_json::to_string(&mask).unwrap();
        let restored: MaskImage = serde_json::from_str(&stored).unwrap();
        assert_eq!(restored.dimensions(), (3, 1));
        assert_eq!(restored.pixels(), mask.pixels());
        assert_eq!(restored.preview().name, "scrim.png");
    }
}
