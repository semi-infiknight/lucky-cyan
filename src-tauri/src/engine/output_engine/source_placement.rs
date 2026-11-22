//! Inverse placement for textures that preserve the entire image.

use std::{fs::File, path::Path};

use anyhow::{ensure, Context, Result};

use super::{still_jpeg, FitMode, VideoGeometry};

pub(super) enum StillFormat {
    Png,
    Jpeg,
}

impl StillFormat {
    pub(super) fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        if extension.eq_ignore_ascii_case("png") {
            Some(Self::Png)
        } else if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") {
            Some(Self::Jpeg)
        } else {
            None
        }
    }
}

pub(crate) fn supports_still_rotation_path(path: &Path) -> bool {
    StillFormat::from_path(path).is_some()
}

pub(super) fn is_jpeg_still(path: &Path) -> bool {
    matches!(StillFormat::from_path(path), Some(StillFormat::Jpeg))
}

#[derive(Debug, Clone, Copy)]
pub(super) struct StillRaster {
    pub(super) width: u32,
    pub(super) height: u32,
}

impl StillRaster {
    pub(super) fn open(path: &Path) -> Result<Self> {
        match StillFormat::from_path(path).context("Continuous rotation requires a still PNG or JPEG")? {
            StillFormat::Png => Self::open_png(path),
            StillFormat::Jpeg => {
                ensure!(path.is_file(), "Choose a local JPEG file");
                let file = File::open(path).context("Cannot read the still JPEG")?;
                let [width, height] = still_jpeg::raster_size(file)?;
                Ok(Self { width, height })
            }
        }
    }

    fn open_png(path: &Path) -> Result<Self> {
        let file = File::open(path).context("Cannot read the still PNG")?;
        let png = png::Decoder::new(file)
            .read_info()
            .context("Continuous rotation requires a still PNG")?;
        let info = png.info();
        ensure!(
            info.animation_control.is_none(),
            "Continuous rotation requires a non-animated PNG"
        );
        ensure!(
            info.exif_metadata.is_none(),
            "Export the PNG with its orientation baked into the picture"
        );
        ensure!(
            info.pixel_dims.is_none_or(|dims| dims.xppu == dims.yppu),
            "Continuous rotation requires square pixels"
        );
        Ok(Self {
            width: info.width,
            height: info.height,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SourcePlacement {
    /// Top-left stage UV to unit coordinates in the unrotated cropped picture.
    pub(super) x: [f32; 3],
    pub(super) y: [f32; 3],
    /// Source crop with top-left origin, before aspect and rotation.
    pub(super) crop: [f32; 4],
}

impl SourcePlacement {
    pub(super) fn new(raster: StillRaster, geometry: VideoGeometry, stage: [u32; 2]) -> Self {
        let (crop_w, crop_h, crop_x, crop_y) = geometry
            .crop_rect_px(raster.width, raster.height)
            .unwrap_or((raster.width, raster.height, 0, 0));
        let [stage_w, stage_h] = stage.map(|value| f64::from(value.max(1)));
        let [source_w, source_h] = [f64::from(crop_w), f64::from(crop_h)];
        let width_ratio = stage_w / source_w;
        let height_ratio = stage_h / source_h;
        let fit = match geometry.fit_mode {
            FitMode::Fit => width_ratio.min(height_ratio),
            FitMode::Fill => width_ratio.max(height_ratio),
            FitMode::Stretch => 1.0,
        };
        let [w, h] = match geometry.fit_mode {
            FitMode::Stretch => [stage_w, stage_h],
            _ => [source_w * fit, source_h * fit],
        };
        let scale = finite_or(geometry.scale, 1.0).clamp(0.01, 8.0);
        let [w, h] = [w * scale, h * scale];
        let cx = stage_w / 2.0 + finite_or(geometry.pan_x, 0.0).clamp(-1.0, 1.0) * w;
        let cy = stage_h / 2.0 + finite_or(geometry.pan_y, 0.0).clamp(-1.0, 1.0) * h;
        let [pivot_u, pivot_v] = geometry.source_rotation_pivot
            .unwrap_or([0.5, 0.5])
            .map(|coordinate| finite_or(coordinate, 0.5).clamp(0.0, 1.0));
        // At zero rotation a different pivot must leave the image in place.
        let pivot_x = cx + (pivot_u - 0.5) * w;
        let pivot_y = cy + (pivot_v - 0.5) * h;
        let degrees = finite_or(
            geometry
                .source_rotation_degrees
                .unwrap_or(f64::from(geometry.rotation)),
            0.0,
        );
        let (sin, cos) = degrees.rem_euclid(360.0).to_radians().sin_cos();
        let x = [
            cos * stage_w / w,
            sin * stage_h / w,
            pivot_u - (cos * pivot_x + sin * pivot_y) / w,
        ];
        let y = [
            -sin * stage_w / h,
            cos * stage_h / h,
            pivot_v - (-sin * pivot_x + cos * pivot_y) / h,
        ];
        Self {
            x: x.map(|value| value as f32),
            y: y.map(|value| value as f32),
            crop: [
                crop_x as f32 / raster.width as f32,
                crop_y as f32 / raster.height as f32,
                crop_w as f32 / raster.width as f32,
                crop_h as f32 / raster.height as f32,
            ],
        }
    }
}

fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(found: [f32; 2], expected: [f32; 2]) {
        assert!(
            found
                .into_iter()
                .zip(expected)
                .all(|(a, b)| (a - b).abs() < 0.0001),
            "found {found:?}, expected {expected:?}"
        );
    }

    fn local(p: SourcePlacement, x: f32, y: f32) -> [f32; 2] {
        [
            p.x[0] * x + p.x[1] * y + p.x[2],
            p.y[0] * x + p.y[1] * y + p.y[2],
        ]
    }

    #[test]
    fn clipped_marker_can_rotate_back_into_the_stage() {
        let raster = StillRaster {
            width: 160,
            height: 80,
        };
        let geometry = VideoGeometry {
            pan_x: 0.35,
            source_rotation_degrees: Some(59.0),
            ..Default::default()
        };
        let placement = SourcePlacement::new(raster, geometry, [160, 100]);
        close(local(placement, 136.0 / 160.0, 0.5), [0.5, 0.5]);
        let angle = 59_f32.to_radians();
        close(
            local(
                placement,
                (136.0 + 35.0 * angle.cos()) / 160.0,
                (50.0 + 35.0 * angle.sin()) / 100.0,
            ),
            [0.5 + 35.0 / 160.0, 0.5],
        );
    }

    #[test]
    fn resolves_crop_before_fit_and_holds_scale_through_rotation() {
        let raster = StillRaster {
            width: 100,
            height: 100,
        };
        let geometry = VideoGeometry {
            crop_left: 0.25,
            crop_top: 0.2,
            crop_bottom: 0.3,
            ..Default::default()
        };
        let placement = SourcePlacement::new(raster, geometry, [200, 150]);
        assert_eq!(placement.crop, [0.25, 0.2, 0.75, 0.5]);
        close(local(placement, 0.0, 1.0 / 18.0), [0.0, 0.0]);
        let turned = SourcePlacement::new(
            raster,
            VideoGeometry {
                source_rotation_degrees: Some(90.0),
                ..geometry
            },
            [200, 150],
        );
        close(local(turned, 5.0 / 6.0, -1.0 / 6.0), [0.0, 0.0]);
    }

    #[test]
    fn a_cropped_and_panned_still_turns_around_its_saved_edge_pivot() {
        let raster = StillRaster { width: 160, height: 100 };
        let stage = [200, 100];
        let geometry = VideoGeometry {
            crop_left: 0.25,
            scale: 0.5,
            pan_x: 0.25,
            pan_y: -0.2,
            source_rotation_pivot: Some([0.0, 0.5]),
            ..Default::default()
        };
        let initial = SourcePlacement::new(raster, geometry, stage);
        let centred = SourcePlacement::new(raster, VideoGeometry {
            source_rotation_pivot: None,
            ..geometry
        }, stage);
        assert_eq!(initial, centred, "a zero-angle pivot edit must not move the picture");

        let turned = SourcePlacement::new(raster, VideoGeometry {
            source_rotation_degrees: Some(90.0),
            ..geometry
        }, stage);
        close(local(initial, 85.0 / 200.0, 0.4), [0.0, 0.5]);
        close(local(turned, 85.0 / 200.0, 0.4), [0.0, 0.5]);
        close(local(initial, 133.0 / 200.0, 0.4), [0.8, 0.5]);
        close(local(turned, 85.0 / 200.0, 0.88), [0.8, 0.5]);
        assert_eq!(turned.crop, [0.25, 0.0, 0.75, 1.0]);
    }

    #[test]
    fn all_fit_modes_preserve_the_unrotated_size_through_a_turn() {
        let raster = StillRaster {
            width: 160,
            height: 80,
        };
        let stage = [160, 100];
        for (fit_mode, expected) in [
            (FitMode::Fit, [0.0, -0.125]),
            (FitMode::Fill, [0.1, 0.0]),
            (FitMode::Stretch, [0.0, 0.0]),
        ] {
            let geometry = VideoGeometry {
                fit_mode,
                ..Default::default()
            };
            let unrotated = SourcePlacement::new(raster, geometry, stage);
            close(local(unrotated, 0.0, 0.0), expected);
            let rotated = SourcePlacement::new(
                raster,
                VideoGeometry {
                    source_rotation_degrees: Some(90.0),
                    ..geometry
                },
                stage,
            );
            // Relative to the stage centre, a quarter turn sends (-80, -50)
            // to (50, -80) in pixels.
            close(local(rotated, 130.0 / 160.0, -30.0 / 100.0), expected);
        }
    }
}
