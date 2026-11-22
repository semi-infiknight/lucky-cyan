//! Bounded metadata read for a JPEG whose pixels are ready for stage placement.
//! libmpv still decodes and renders the file.

use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

use anyhow::{bail, ensure, Context, Result};

const MAX_HEADER_BYTES: u64 = 1024 * 1024;
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_MARKERS: usize = 4096;

pub(super) fn raster_size(reader: impl Read) -> Result<[u32; 2]> {
    read_header(&mut BufReader::new(reader.take(MAX_HEADER_BYTES))).map(|(size, _)| size)
}

/// Inspect the compressed marker stream through end-of-image without decoding
/// pixels. Run on the slot worker, never during GO or on the render thread.
pub(super) fn verify_file(path: &Path, cancelled: impl Fn() -> bool) -> Result<()> {
    ensure!(path.is_file(), "Choose a local JPEG file");
    let file = File::open(path).context("Cannot read the still JPEG")?;
    let mut reader = BufReader::with_capacity(64 * 1024, file.take(MAX_FILE_BYTES));
    verify_scans(&mut reader, &cancelled)
}

fn read_header(reader: &mut impl Read) -> Result<([u32; 2], bool)> {
    ensure!(read::<2>(reader)? == [0xff, 0xd8], "Expected a JPEG still");
    let mut size = None;
    let mut exif_seen = false;
    let mut segment = Vec::new();
    for _ in 0..MAX_MARKERS {
        let marker = read_marker(reader)?;
        ensure!(marker != 0xd9, "JPEG has no image scan");
        read_segment(reader, &mut segment)?;
        match marker {
            0xc0..=0xc2 => {
                ensure!(
                    size.is_none(),
                    "Centre rotation requires a single JPEG image"
                );
                size = Some(frame_size(&segment)?);
            }
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                bail!("Centre rotation requires a standard 8-bit JPEG")
            }
            0xda => return Ok((size.context("JPEG has no image frame")?, exif_seen)),
            _ => check_picture_metadata(marker, &segment, &mut exif_seen)?,
        }
    }
    bail!("Too many JPEG header markers")
}

fn check_picture_metadata(marker: u8, segment: &[u8], exif_seen: &mut bool) -> Result<()> {
    match marker {
        0xe0 if segment.starts_with(b"JFIF\0") => {
            let density = segment.get(8..12).context("Truncated JPEG pixel density")?;
            ensure!(
                density[..2] == density[2..] && density[..2] != [0, 0],
                "Centre rotation requires square pixels"
            );
        }
        0xe1 if segment.starts_with(b"Exif\0\0") => {
            ensure!(!*exif_seen, "JPEG has duplicate EXIF headers");
            *exif_seen = true;
            check_exif_orientation(&segment[6..])?;
        }
        0xe2 if segment.starts_with(b"MPF\0") => {
            bail!("Centre rotation requires a single JPEG image")
        }
        _ => {}
    }
    Ok(())
}

fn verify_scans(reader: &mut impl BufRead, cancelled: &impl Fn() -> bool) -> Result<()> {
    let (_, mut exif_seen) = read_header(&mut (&mut *reader).take(MAX_HEADER_BYTES))?;
    let mut in_scan = true;
    let mut segment = Vec::new();
    for _ in 0..MAX_MARKERS {
        let marker = if in_scan {
            after_scan(reader, cancelled)?
        } else {
            read_marker(reader)?
        };
        if marker == 0xd9 {
            return Ok(());
        }
        ensure!(
            !matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf),
            "Centre rotation requires a single JPEG image"
        );
        read_segment(reader, &mut segment)?;
        check_picture_metadata(marker, &segment, &mut exif_seen)?;
        in_scan = marker == 0xda;
    }
    bail!("Too many JPEG scan markers")
}

fn read<const N: usize>(reader: &mut impl Read) -> Result<[u8; N]> {
    let mut bytes = [0; N];
    reader
        .read_exact(&mut bytes)
        .context("Truncated or oversized JPEG data")?;
    Ok(bytes)
}

fn read_segment(reader: &mut impl Read, segment: &mut Vec<u8>) -> Result<()> {
    let length = usize::from(u16::from_be_bytes(read::<2>(reader)?));
    ensure!(length >= 2, "Invalid JPEG marker length");
    segment.resize(length - 2, 0);
    reader
        .read_exact(segment)
        .context("Truncated or oversized JPEG segment")?;
    Ok(())
}

fn read_marker(reader: &mut impl Read) -> Result<u8> {
    ensure!(read::<1>(reader)?[0] == 0xff, "Expected a JPEG marker");
    let marker = marker_code(reader)?;
    ensure!(
        matches!(marker, 0xc0..=0xfe) && !(0xd0..=0xd8).contains(&marker),
        "Invalid JPEG marker"
    );
    Ok(marker)
}

fn marker_code(reader: &mut impl Read) -> Result<u8> {
    for _ in 0..16 {
        let marker = read::<1>(reader)?[0];
        if marker != 0xff {
            return Ok(marker);
        }
    }
    bail!("Too much padding in a JPEG marker")
}

fn after_scan(reader: &mut impl BufRead, cancelled: &impl Fn() -> bool) -> Result<u8> {
    loop {
        ensure!(!cancelled(), "JPEG verification was cancelled");
        let buffer = reader.fill_buf().context("Cannot read JPEG scan")?;
        ensure!(
            !buffer.is_empty(),
            "Truncated JPEG or file larger than 256 MiB"
        );
        let Some(offset) = buffer.iter().position(|&byte| byte == 0xff) else {
            let length = buffer.len();
            reader.consume(length);
            continue;
        };
        reader.consume(offset + 1);
        // Stuffed FF bytes and restart markers are part of the scan.
        let marker = marker_code(reader)?;
        if matches!(marker, 0x00 | 0xd0..=0xd7) {
            continue;
        }
        ensure!(
            matches!(marker, 0xc0..=0xfe) && marker != 0xd8,
            "Invalid JPEG scan marker"
        );
        return Ok(marker);
    }
}

fn frame_size(data: &[u8]) -> Result<[u32; 2]> {
    let [bits, h0, h1, w0, w1, components, rest @ ..] = data else {
        bail!("Incomplete JPEG frame dimensions");
    };
    ensure!(
        *bits == 8 && matches!(components, 1 | 3) && rest.len() == usize::from(*components) * 3,
        "Centre rotation requires an 8-bit grayscale or colour JPEG"
    );
    let width = u16::from_be_bytes([*w0, *w1]);
    let height = u16::from_be_bytes([*h0, *h1]);
    ensure!(width > 0 && height > 0, "JPEG frame dimensions are missing");
    Ok([u32::from(width), u32::from(height)])
}

fn check_exif_orientation(tiff: &[u8]) -> Result<()> {
    let little_endian = match tiff.get(0..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => bail!("Invalid JPEG EXIF byte order"),
    };
    let u16_at = |bytes: [u8; 2]| {
        if little_endian {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        }
    };
    let u32_at = |bytes: [u8; 4]| {
        if little_endian {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    };
    let Some(&[_, _, m0, m1, o0, o1, o2, o3]) = tiff.first_chunk::<8>() else {
        bail!("Incomplete JPEG EXIF header");
    };
    ensure!(u16_at([m0, m1]) == 42, "Invalid JPEG EXIF header");
    let offset = u32_at([o0, o1, o2, o3]) as usize;
    ensure!(offset >= 8, "Invalid JPEG EXIF directory offset");
    let directory = tiff.get(offset..).context("Missing JPEG EXIF directory")?;
    let Some((&count, entries)) = directory.split_first_chunk::<2>() else {
        bail!("Missing JPEG EXIF directory entries");
    };
    let count = usize::from(u16_at(count));
    ensure!(
        entries.len() >= count * 12 + 4,
        "Truncated JPEG EXIF directory"
    );
    let entries = &entries[..count * 12];
    let mut found_orientation = false;
    for field in entries.chunks_exact(12) {
        if u16_at([field[0], field[1]]) != 0x0112 {
            continue;
        }
        ensure!(
            !found_orientation
                && u16_at([field[2], field[3]]) == 3
                && u32_at([field[4], field[5], field[6], field[7]]) == 1,
            "Invalid or duplicate JPEG EXIF orientation"
        );
        found_orientation = true;
        ensure!(
            u16_at([field[8], field[9]]) == 1,
            "Export the JPEG with its orientation baked into the picture"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(bytes: &mut Vec<u8>, marker: u8, payload: &[u8]) {
        bytes.extend_from_slice(&[0xff, marker]);
        bytes.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(payload);
    }

    fn header(app: &[(u8, &[u8])]) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8];
        for (marker, payload) in app {
            segment(&mut bytes, *marker, payload);
        }
        // 120 × 64 progressive grayscale.
        segment(&mut bytes, 0xc2, &[8, 0, 64, 0, 120, 1, 1, 0x11, 0]);
        segment(&mut bytes, 0xda, &[1, 1, 0, 0, 0, 0]);
        bytes
    }

    const EXIF_LE: &[u8] =
        b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x01\0\0\0\0\0\0\0";
    const EXIF_BE: &[u8] =
        b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x01\0\0\0\0\0\0";

    #[test]
    fn reads_dimensions_with_no_exif_or_baked_orientation_in_either_byte_order() {
        for metadata in [&[][..], &[(0xe1, EXIF_LE)], &[(0xe1, EXIF_BE)]] {
            assert_eq!(raster_size(&header(metadata)[..]).unwrap(), [120, 64]);
        }
    }

    #[test]
    fn inspects_late_metadata_through_baseline_and_progressive_scans() {
        use std::io::Cursor;

        for multiple_scans in [false, true] {
            let mut bytes = header(&[]);
            if !multiple_scans {
                let frame_marker = bytes.iter().position(|byte| *byte == 0xc2).unwrap();
                bytes[frame_marker] = 0xc0;
            }
            bytes.extend_from_slice(&[0x20, 0xff, 0, 0x21, 0xff, 0xd0, 0x22]);
            if multiple_scans {
                segment(&mut bytes, 0xda, &[1, 1, 0, 0, 0, 0]);
                bytes.extend_from_slice(&[0x40, 0x41]);
            }
            let mut normal = bytes.clone();
            normal.extend_from_slice(&[0xff, 0xd9]);
            verify_scans(&mut Cursor::new(normal), &|| false).unwrap();

            let mut late_orientation = EXIF_LE.to_vec();
            late_orientation[24] = if multiple_scans { 4 } else { 6 };
            segment(&mut bytes, 0xe1, &late_orientation);
            bytes.extend_from_slice(&[0xff, 0xd9]);
            let error = verify_scans(&mut Cursor::new(bytes), &|| false).unwrap_err();
            assert!(error.to_string().contains("orientation baked"));
        }
    }

    #[test]
    fn rejects_unbaked_orientation_and_ambiguous_still_metadata() {
        let mut oriented = EXIF_LE.to_vec();
        for orientation in 2..=8 {
            oriented[24] = orientation;
            let error = raster_size(&header(&[(0xe1, &oriented)])[..]).unwrap_err();
            assert!(error.to_string().contains("orientation baked"));
        }
        assert!(raster_size(&header(&[(0xe1, EXIF_LE), (0xe1, EXIF_BE)])[..]).is_err());
        assert!(raster_size(&header(&[(0xe1, &EXIF_LE[..24])])[..]).is_err());
        assert!(raster_size(&header(&[(0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x02")])[..]).is_err());
        assert!(raster_size(&header(&[(0xe2, b"MPF\0")])[..]).is_err());
    }
}
