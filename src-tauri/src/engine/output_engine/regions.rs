//! Rectangular remapping within one native output raster.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Keep this limit in sync with the arrays in warp.frag and the region editor.
pub(super) const MAX_REGIONS: usize = 8;
const EXTENT: u16 = 10_000;
const FULL_RASTER: [u16; 4] = [0, 0, EXTENT, EXTENT];

/// Direction of a matched pair of projector crops in a shared picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSplitDirection {
    Horizontal,
    Vertical,
}

impl OutputSplitDirection {
    pub(crate) fn overlap_bounds(overlap: u16) -> Result<(u16, u16), &'static str> {
        if overlap > 2_000 {
            return Err("Overlap must be between 0% and 20% of the full picture");
        }
        let start = (EXTENT - overlap) / 2;
        Ok((start, start + overlap))
    }

    fn sources(self, overlap: u16) -> Result<[[u16; 4]; 2], &'static str> {
        let (seam_start, seam_end) = Self::overlap_bounds(overlap)?;
        Ok(match self {
            Self::Horizontal => [[0, 0, seam_end, EXTENT], [seam_start, 0, EXTENT - seam_start, EXTENT]],
            Self::Vertical => [[0, 0, EXTENT, seam_end], [0, seam_start, EXTENT, EXTENT - seam_start]],
        })
    }

    /// The saved warp reads successive source rectangles into full rasters.
    /// Overlap is measured across the full picture, in hundredths of a percent.
    pub(crate) fn regions(self, overlap: u16) -> Result<[OutputRegions; 2], &'static str> {
        Ok(self.sources(overlap)?.map(|source| {
            let mut layout = OutputRegions { count: 1, ..OutputRegions::default() };
            layout.rectangles[0] = OutputRegion { source, destination: FULL_RASTER, seam: None };
            layout
        }))
    }

    pub(crate) fn feathered_regions(self, overlap: u16, gamma: u16, outputs: [Uuid; 2])
        -> Result<[OutputRegions; 2], &'static str>
    {
        let mut layouts = self.regions(overlap)?;
        let seams = [
            OutputSeam { partner: outputs[1], direction: self, overlap, gamma, rising: false },
            OutputSeam { partner: outputs[0], direction: self, overlap, gamma, rising: true },
        ];
        for (layout, seam) in layouts.iter_mut().zip(seams) {
            if !seam.valid() { return Err("Feathering needs a positive overlap and gamma between 1.00 and 3.00"); }
            layout.rectangles[0].seam = Some(seam);
        }
        Ok(layouts)
    }
}

/// The source-space ramp and the peer required for it to take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct OutputSeam {
    partner: Uuid,
    direction: OutputSplitDirection,
    overlap: u16,
    /// Display transfer exponent in hundredths, e.g. 220 for gamma 2.20.
    gamma: u16,
    rising: bool,
}

impl OutputSeam {
    fn valid(self) -> bool {
        (1..=2_000).contains(&self.overlap) && (100..=300).contains(&self.gamma)
    }

    fn matches_region(self, region: &OutputRegion) -> bool {
        self.valid() && region.destination == FULL_RASTER
            && self.direction.sources(self.overlap).is_ok_and(|rects| rects[usize::from(self.rising)] == region.source)
    }

    fn uniform(self) -> [f32; 4] {
        let (start, end) = OutputSplitDirection::overlap_bounds(self.overlap).expect("validated seam");
        let axis = match self.direction { OutputSplitDirection::Horizontal => 1.0, OutputSplitDirection::Vertical => 2.0 };
        let signed_axis = if self.rising { axis } else { -axis };
        [f32::from(start) / f32::from(EXTENT), f32::from(end) / f32::from(EXTENT), signed_axis, 100.0 / f32::from(self.gamma)]
    }
}

/// Source and destination rectangles as [x, y, width, height], top-left origin.
/// Coordinates use hundredths of a percent of the corresponding raster side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct OutputRegion {
    source: [u16; 4],
    destination: [u16; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seam: Option<OutputSeam>,
}

/// Validated region layout. The last matching destination rectangle wins.
/// An empty layout passes the source through unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<OutputRegion>", into = "Vec<OutputRegion>")]
pub struct OutputRegions {
    rectangles: [OutputRegion; MAX_REGIONS],
    count: usize,
}

impl TryFrom<Vec<OutputRegion>> for OutputRegions {
    type Error = &'static str;

    fn try_from(regions: Vec<OutputRegion>) -> Result<Self, Self::Error> {
        if regions.len() > MAX_REGIONS {
            return Err("An output can have at most eight regions");
        }
        if regions
            .iter()
            .any(|region| !valid(region.source) || !valid(region.destination))
        {
            return Err("Source and destination rectangles must have positive size and fit inside the picture");
        }
        if regions.iter().any(|region| region.seam.is_some_and(|seam| regions.len() != 1 || !seam.matches_region(region))) {
            return Err("Paired seam must match one full-raster projector crop");
        }
        let mut layout = Self {
            count: regions.len(),
            ..Self::default()
        };
        layout.rectangles[..layout.count].copy_from_slice(&regions);
        Ok(layout)
    }
}

impl From<OutputRegions> for Vec<OutputRegion> {
    fn from(layout: OutputRegions) -> Self {
        layout.rectangles[..layout.count].to_vec()
    }
}

impl OutputRegions {
    /// No remapping is configured.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub(super) fn seam_partner(&self) -> Option<Uuid> {
        self.rectangles[0].seam.map(|seam| seam.partner)
    }

    pub(super) fn matches_seam(&self, id: Uuid, peer: &Self) -> bool {
        self.rectangles[0].seam.is_some_and(|seam| seam.partner != id
            && peer.rectangles[0].seam == Some(OutputSeam { partner: id, rising: !seam.rising, ..seam }))
    }

    pub(super) fn clear_seam(&mut self) {
        self.rectangles[0].seam = None;
    }

    /// Pack normalized source and destination bounds for consecutive vec4 uniforms.
    pub(super) fn uniforms(&self) -> RegionUniforms {
        let mut bounds = RegionUniforms {
            source: [0.0; MAX_REGIONS * 4],
            destination: [0.0; MAX_REGIONS * 4],
            count: self.count,
            seam: self.rectangles[0].seam.map_or([0.0; 4], OutputSeam::uniform),
        };
        for (index, region) in self.rectangles[..self.count].iter().enumerate() {
            let offset = index * 4;
            bounds.source[offset..offset + 4].copy_from_slice(&normalized_bounds(region.source));
            bounds.destination[offset..offset + 4]
                .copy_from_slice(&normalized_bounds(region.destination));
        }
        bounds
    }
}

pub(super) struct RegionUniforms {
    pub source: [f32; MAX_REGIONS * 4],
    pub destination: [f32; MAX_REGIONS * 4],
    pub count: usize,
    pub seam: [f32; 4],
}

fn valid([x, y, width, height]: [u16; 4]) -> bool {
    width > 0
        && height > 0
        && x.saturating_add(width) <= EXTENT
        && y.saturating_add(height) <= EXTENT
}

fn normalized_bounds([x, y, width, height]: [u16; 4]) -> [f32; 4] {
    // Use absolute right/bottom coordinates so an adjoining region's left or
    // top converts to exactly the same float at the shared boundary.
    [x, y, x + width, y + height].map(|value| f32::from(value) / f32::from(EXTENT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn layout_serializes_in_order_and_packs_exact_shared_edges() {
        let json = json!([
            {"source": [1001, 254, 3333, 9746], "destination": [0, 0, 3333, 10000]},
            {"source": [3333, 0, 6667, 10000], "destination": [3333, 0, 6667, 10000]}
        ]);
        let layout: OutputRegions = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(layout).unwrap(), json);
        let bounds = layout.uniforms();
        assert_eq!(bounds.count, 2);
        assert_eq!(&bounds.source[..4], &[0.1001, 0.0254, 0.4334, 1.0]);
        assert_eq!(bounds.destination[2], bounds.destination[4]);
    }

    #[test]
    fn paired_crops_reuse_full_rasters_with_the_requested_overlap() {
        let [left, right] = OutputSplitDirection::Horizontal.regions(1_001).unwrap();
        let left = left.rectangles[0];
        let right = right.rectangles[0];
        assert_eq!(left.source[2] - right.source[0], 1_001);
        assert_eq!(right.source[0] + right.source[2], EXTENT);
        assert_eq!(left.destination, [0, 0, EXTENT, EXTENT]);
        assert_eq!(right.destination, left.destination);

        let [top, bottom] = OutputSplitDirection::Vertical.regions(0).unwrap();
        assert_eq!(top.rectangles[0].source, [0, 0, EXTENT, 5_000]);
        assert_eq!(bottom.rectangles[0].source, [0, 5_000, EXTENT, 5_000]);
        assert!(OutputSplitDirection::Horizontal.regions(2_001).is_err());
    }

    #[test]
    fn paired_feather_roundtrips_and_rejects_unrelated_crop_geometry() {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let [left, right] = OutputSplitDirection::Horizontal.feathered_regions(2_000, 220, [first, second]).unwrap();
        assert_eq!(left.seam_partner(), Some(second));
        assert!(left.matches_seam(first, &right));
        assert!(!right.matches_seam(second, &right));
        assert_eq!(left.uniforms().seam, [0.4, 0.6, -1.0, 100.0 / 220.0]);
        assert_eq!(right.uniforms().seam, [0.4, 0.6, 1.0, 100.0 / 220.0]);
        let saved = serde_json::to_value(left).unwrap();
        assert_eq!(serde_json::from_value::<OutputRegions>(saved.clone()).unwrap(), left);
        let mut invalid = saved;
        invalid[0]["source"][2] = json!(5_995);
        assert!(serde_json::from_value::<OutputRegions>(invalid).is_err());
        assert!(OutputSplitDirection::Horizontal.feathered_regions(0, 220, [first, second]).is_err());
        assert!(OutputSplitDirection::Vertical.feathered_regions(2_000, 99, [first, second]).is_err());
    }

    #[test]
    fn invalid_rectangles_and_excessive_layouts_fail_validation() {
        let source = [0, 0, 10000, 10000];
        for bad in [[0, 0, 0, 1000], [9999, 0, 2, 1000], [0, 65535, 5000, 65535]] {
            assert!(serde_json::from_value::<OutputRegions>(json!([
                {"source": bad, "destination": source}
            ]))
            .is_err());
            assert!(serde_json::from_value::<OutputRegions>(json!([
                {"source": source, "destination": bad}
            ]))
            .is_err());
        }
        let repeated = json!({"source": source, "destination": source});
        assert!(
            serde_json::from_value::<OutputRegions>(json!(vec![repeated; MAX_REGIONS + 1]))
                .is_err()
        );
    }
}
