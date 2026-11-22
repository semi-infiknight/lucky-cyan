//! Blend modes for the layer compositor.
//!
//! Each mode is a per-pixel operation combining a foreground layer (a cue on a
//! higher layer) with the backdrop (the composite of everything below), QLab
//! style.  The reference implementations here are **pure Rust mirrors of the
//! GLSL** in [`GLSL_BLEND_FN`] — same structure as `warp.rs` — so the math is
//! unit-testable without a GL context; the shader is generated from the same
//! formulas for separable and W3C component blends.
//!
//! Compositing itself is done back-to-front with ping-pong FBOs: GL fixed-
//! function blending cannot read the backdrop, so each step samples the
//! previous composite as a texture and writes the blended result.

use serde::{Deserialize, Serialize};

/// How a visual cue's pixels combine with the layers below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    /// Standard alpha compositing (source-over).
    #[default]
    Normal,
    /// Linear dodge: backdrop + source.
    Add,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    HardLight,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    Difference,
    Exclusion,
    Subtract,
    LinearBurn,
    PinLight,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    /// All modes, in inspector display order.
    pub const ALL: [BlendMode; 20] = [
        BlendMode::Normal,
        BlendMode::Add,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Subtract,
        BlendMode::LinearBurn,
        BlendMode::PinLight,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    /// Stable integer id passed to the composite shader (`u_blend_mode`).
    pub fn shader_id(self) -> i32 {
        match self {
            BlendMode::Normal => 0,
            BlendMode::Add => 1,
            BlendMode::Multiply => 2,
            BlendMode::Screen => 3,
            BlendMode::Overlay => 4,
            BlendMode::SoftLight => 5,
            BlendMode::HardLight => 6,
            BlendMode::Darken => 7,
            BlendMode::Lighten => 8,
            BlendMode::ColorDodge => 9,
            BlendMode::ColorBurn => 10,
            BlendMode::Difference => 11,
            BlendMode::Exclusion => 12,
            BlendMode::Subtract => 13,
            BlendMode::LinearBurn => 14,
            BlendMode::PinLight => 15,
            BlendMode::Hue => 16,
            BlendMode::Saturation => 17,
            BlendMode::Color => 18,
            BlendMode::Luminosity => 19,
        }
    }
}

/// Reference blend of one colour channel (foreground `s`, backdrop `b`, both
/// in [0,1]) for modes that treat each color channel independently.
#[cfg(test)]
fn blend_channel(mode: BlendMode, b: f32, s: f32) -> f32 {
    match mode {
        BlendMode::Normal => s,
        BlendMode::Add => (b + s).min(1.0),
        BlendMode::Multiply => b * s,
        BlendMode::Screen => b + s - b * s,
        BlendMode::Overlay => hard_light(s, b), // overlay(b,s) = hard_light(s,b)
        BlendMode::SoftLight => {
            // W3C soft-light.
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 {
                    ((16.0 * b - 12.0) * b + 4.0) * b
                } else {
                    b.sqrt()
                };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }
        BlendMode::HardLight => hard_light(b, s),
        BlendMode::Darken => b.min(s),
        BlendMode::Lighten => b.max(s),
        BlendMode::ColorDodge => {
            if b <= 0.0 { 0.0 }
            else if s >= 1.0 { 1.0 }
            else { (b / (1.0 - s)).min(1.0) }
        }
        BlendMode::ColorBurn => {
            if b >= 1.0 { 1.0 }
            else if s <= 0.0 { 0.0 }
            else { 1.0 - ((1.0 - b) / s).min(1.0) }
        }
        BlendMode::Difference => (b - s).abs(),
        BlendMode::Exclusion => b + s - 2.0 * b * s,
        BlendMode::Subtract => (b - s).max(0.0),
        BlendMode::LinearBurn => (b + s - 1.0).max(0.0),
        BlendMode::PinLight => {
            if s <= 0.5 {
                b.min(2.0 * s)
            } else {
                b.max(2.0 * s - 1.0)
            }
        }
        BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => {
            unreachable!("Component modes combine complete colors")
        }
    }
}

#[cfg(test)]
fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b * 2.0 * s
    } else {
        // screen(b, 2s − 1)
        let s2 = 2.0 * s - 1.0;
        b + s2 - b * s2
    }
}

#[cfg(test)]
fn color_range(c: [f32; 3]) -> (f32, f32) {
    (c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]))
}

#[cfg(test)]
fn luminosity(c: [f32; 3]) -> f32 {
    c[0] * 0.3 + c[1] * 0.59 + c[2] * 0.11
}

#[cfg(test)]
fn saturation(c: [f32; 3]) -> f32 {
    let (lo, hi) = color_range(c);
    hi - lo
}

#[cfg(test)]
fn with_saturation(c: [f32; 3], saturation: f32) -> [f32; 3] {
    let (lo, hi) = color_range(c);
    if hi <= lo {
        return [0.0; 3];
    }
    // Equivalent to SetSat's min/mid/max assignments, including tied channels.
    c.map(|value| (value - lo) * saturation / (hi - lo))
}

#[cfg(test)]
fn with_luminosity(c: [f32; 3], target: f32) -> [f32; 3] {
    let offset = target - luminosity(c);
    let mut result = c.map(|value| value + offset);
    let l = luminosity(result);
    let (lo, hi) = color_range(result);
    // Clip chroma around the target luminosity rather than clipping channels
    // independently, which would shift the hue and luminosity.
    if lo < 0.0 {
        result = result.map(|value| l + (value - l) * l / (l - lo));
    }
    if hi > 1.0 {
        result = result.map(|value| l + (value - l) * (1.0 - l) / (hi - l));
    }
    result.map(|value| value.clamp(0.0, 1.0))
}

#[cfg(test)]
fn blend_rgb(mode: BlendMode, b: [f32; 3], s: [f32; 3]) -> [f32; 3] {
    match mode {
        BlendMode::Hue => with_luminosity(with_saturation(s, saturation(b)), luminosity(b)),
        BlendMode::Saturation => with_luminosity(with_saturation(b, saturation(s)), luminosity(b)),
        BlendMode::Color => with_luminosity(s, luminosity(b)),
        BlendMode::Luminosity => with_luminosity(b, luminosity(s)),
        _ => std::array::from_fn(|channel| blend_channel(mode, b[channel], s[channel])),
    }
}

/// Full pixel composite, reference implementation of the GLSL `main`:
/// blend the colors, then alpha-composite (source-over) with the source
/// alpha scaled by the layer opacity.
///
/// `backdrop`/`source` are straight (non-premultiplied) RGBA.
#[cfg(test)]
pub(crate) fn composite_pixel(
    mode: BlendMode,
    backdrop: [f32; 4],
    source: [f32; 4],
    opacity: f32,
) -> [f32; 4] {
    let sa = (source[3] * opacity).clamp(0.0, 1.0);
    let ba = backdrop[3];
    let rgb = |[r, g, b, _]: [f32; 4]| [r, g, b];
    let blend = blend_rgb(mode, rgb(backdrop), rgb(source));
    let mut out = [0.0f32; 4];
    for c in 0..3 {
        // W3C: result = (1−ba)·s + ba·B(b,s), then source-over with sa.
        let blended = (1.0 - ba) * source[c] + ba * blend[c];
        out[c] = sa * blended + (1.0 - sa) * backdrop[c] * ba;
        // Un-premultiply against the output alpha below.
    }
    out[3] = sa + ba * (1.0 - sa);
    if out[3] > 0.0 {
        for c in 0..3 {
            out[c] /= out[3];
        }
    }
    out
}

/// GLSL function bodies shared by the composite shader (`#version 150 core`).
///
/// `blend_rgb` and `blend_channel` mirror the Rust reference above.
pub(crate) const GLSL_BLEND_FN: &str = r#"
float hard_light(float b, float s) {
    if (s <= 0.5) return b * 2.0 * s;
    float s2 = 2.0 * s - 1.0;
    return b + s2 - b * s2;
}
float blend_channel(int mode, float b, float s) {
    if (mode == 0) return s;
    if (mode == 1) return min(b + s, 1.0);
    if (mode == 2) return b * s;
    if (mode == 3) return b + s - b * s;
    if (mode == 4) return hard_light(s, b);
    if (mode == 5) {
        if (s <= 0.5) return b - (1.0 - 2.0 * s) * b * (1.0 - b);
        float d = (b <= 0.25) ? ((16.0 * b - 12.0) * b + 4.0) * b : sqrt(b);
        return b + (2.0 * s - 1.0) * (d - b);
    }
    if (mode == 6) return hard_light(b, s);
    if (mode == 7) return min(b, s);
    if (mode == 8) return max(b, s);
    if (mode == 9) {
        if (b <= 0.0) return 0.0;
        if (s >= 1.0) return 1.0;
        return min(b / (1.0 - s), 1.0);
    }
    if (mode == 10) {
        if (b >= 1.0) return 1.0;
        if (s <= 0.0) return 0.0;
        return 1.0 - min((1.0 - b) / s, 1.0);
    }
    if (mode == 11) return abs(b - s);
    if (mode == 12) return b + s - 2.0 * b * s;
    if (mode == 13) return max(b - s, 0.0);
    if (mode == 14) return max(b + s - 1.0, 0.0);
    if (mode == 15) return (s <= 0.5) ? min(b, 2.0 * s) : max(b, 2.0 * s - 1.0);
    return s;
}
float color_min(vec3 c) {
    return min(min(c.r, c.g), c.b);
}
float color_max(vec3 c) {
    return max(max(c.r, c.g), c.b);
}
float luminosity(vec3 c) {
    return dot(c, vec3(0.3, 0.59, 0.11));
}
float saturation(vec3 c) {
    return color_max(c) - color_min(c);
}
vec3 with_saturation(vec3 c, float s) {
    float lo = color_min(c);
    float hi = color_max(c);
    return (hi > lo) ? (c - lo) * s / (hi - lo) : vec3(0.0);
}
vec3 with_luminosity(vec3 c, float target) {
    vec3 result = c + (target - luminosity(c));
    float l = luminosity(result);
    float lo = color_min(result);
    float hi = color_max(result);
    if (lo < 0.0) result = l + (result - l) * l / (l - lo);
    if (hi > 1.0) result = l + (result - l) * (1.0 - l) / (hi - l);
    return clamp(result, 0.0, 1.0);
}
vec3 blend_rgb(int mode, vec3 b, vec3 s) {
    if (mode == 16) return with_luminosity(with_saturation(s, saturation(b)), luminosity(b));
    if (mode == 17) return with_luminosity(with_saturation(b, saturation(s)), luminosity(b));
    if (mode == 18) return with_luminosity(s, luminosity(b));
    if (mode == 19) return with_luminosity(b, luminosity(s));
    return vec3(blend_channel(mode, b.r, s.r), blend_channel(mode, b.g, s.g),
                blend_channel(mode, b.b, s.b));
}
"#;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn serde_snake_case_roundtrip() {
        for mode in BlendMode::ALL {
            let json = serde_json::to_value(mode).unwrap();
            let back: BlendMode = serde_json::from_value(json).unwrap();
            assert_eq!(mode, back);
        }
        assert_eq!(serde_json::to_value(BlendMode::ColorDodge).unwrap(), "color_dodge");
        assert_eq!(BlendMode::default(), BlendMode::Normal);
    }

    #[test]
    fn shader_ids_are_unique_and_dense() {
        let ids = BlendMode::ALL.map(BlendMode::shader_id);
        assert_eq!(ids, std::array::from_fn(|index| index as i32));
    }

    #[test]
    fn normal_returns_source() {
        assert!(close(blend_channel(BlendMode::Normal, 0.3, 0.8), 0.8));
    }

    #[test]
    fn multiply_and_screen_are_duals() {
        let (b, s) = (0.4, 0.7);
        assert!(close(blend_channel(BlendMode::Multiply, b, s), 0.28));
        assert!(close(blend_channel(BlendMode::Screen, b, s), 0.82));
        // screen(b,s) = 1 − (1−b)(1−s)
        assert!(close(
            blend_channel(BlendMode::Screen, b, s),
            1.0 - (1.0 - b) * (1.0 - s),
        ));
    }

    #[test]
    fn overlay_is_hard_light_swapped() {
        for &(b, s) in &[(0.2, 0.7), (0.8, 0.3), (0.5, 0.5)] {
            assert!(close(
                blend_channel(BlendMode::Overlay, b, s),
                blend_channel(BlendMode::HardLight, s, b),
            ));
        }
    }

    #[test]
    fn darken_lighten_difference() {
        assert!(close(blend_channel(BlendMode::Darken, 0.4, 0.7), 0.4));
        assert!(close(blend_channel(BlendMode::Lighten, 0.4, 0.7), 0.7));
        assert!(close(blend_channel(BlendMode::Difference, 0.4, 0.7), 0.3));
        assert!(close(blend_channel(BlendMode::Subtract, 0.4, 0.7), 0.0));
        assert!(close(blend_channel(BlendMode::Subtract, 0.7, 0.4), 0.3));
    }

    #[test]
    fn dodge_burn_edge_cases_never_divide_by_zero() {
        assert!(close(blend_channel(BlendMode::ColorDodge, 0.5, 1.0), 1.0));
        assert!(close(blend_channel(BlendMode::ColorDodge, 0.0, 0.5), 0.0));
        assert!(close(blend_channel(BlendMode::ColorBurn, 0.5, 0.0), 0.0));
        assert!(close(blend_channel(BlendMode::ColorBurn, 1.0, 0.5), 1.0));
        for mode in BlendMode::ALL {
            for &(b, s) in &[(0.0, 0.0), (1.0, 1.0), (0.0, 1.0), (1.0, 0.0)] {
                let rgb = blend_rgb(mode, [b; 3], [s; 3]);
                assert!(rgb.iter().all(|v| (0.0..=1.0).contains(v)), "{mode:?}({b},{s}) = {rgb:?}");
            }
        }
    }

    #[test]
    fn soft_light_matches_w3c_reference_points() {
        // s = 0.5 leaves the backdrop unchanged.
        for &b in &[0.0, 0.25, 0.5, 0.9] {
            assert!(close(blend_channel(BlendMode::SoftLight, b, 0.5), b));
        }
    }

    #[test]
    fn new_layer_modes_combine_color_and_honor_transparent_backdrops() {
        let backdrop = [0.6, 0.4, 0.2, 1.0];
        let source = [0.25, 0.75, 0.5, 1.0];
        for (mode, expected) in [
            (BlendMode::LinearBurn, [0.0, 0.15, 0.0, 1.0]),
            (BlendMode::PinLight, [0.5, 0.5, 0.2, 1.0]),
            (BlendMode::Hue, [0.18, 0.58, 0.38, 1.0]),
            (BlendMode::Saturation, [0.6405, 0.3905, 0.1405, 1.0]),
            (BlendMode::Color, [0.1155, 0.6155, 0.3655, 1.0]),
            (BlendMode::Luminosity, [0.7345, 0.5345, 0.3345, 1.0]),
        ] {
            let actual = composite_pixel(mode, backdrop, source, 1.0);
            assert!(
                actual.into_iter().zip(expected).all(|(a, b)| close(a, b)),
                "{mode:?}: {actual:?}"
            );
            let clear = composite_pixel(mode, [0.0; 4], source, 0.5);
            assert_eq!(clear, [0.25, 0.75, 0.5, 0.5]);
        }
        let clipped = blend_rgb(BlendMode::Color, [0.5; 3], [1.0, 0.0, 0.0]);
        assert!(clipped
            .into_iter()
            .zip([1.0, 2.0 / 7.0, 2.0 / 7.0])
            .all(|(a, b)| close(a, b)));
    }

    #[test]
    fn composite_full_opacity_normal_replaces_backdrop() {
        let out = composite_pixel(BlendMode::Normal, [0.2, 0.2, 0.2, 1.0], [0.8, 0.4, 0.1, 1.0], 1.0);
        assert!(close(out[0], 0.8) && close(out[1], 0.4) && close(out[2], 0.1) && close(out[3], 1.0));
    }

    #[test]
    fn composite_zero_opacity_keeps_backdrop() {
        let backdrop = [0.2, 0.5, 0.7, 1.0];
        let out = composite_pixel(BlendMode::Multiply, backdrop, [0.9, 0.9, 0.9, 1.0], 0.0);
        for c in 0..4 {
            assert!(close(out[c], backdrop[c]));
        }
    }

    #[test]
    fn composite_transparent_source_keeps_backdrop() {
        let backdrop = [0.3, 0.3, 0.3, 1.0];
        let out = composite_pixel(BlendMode::Add, backdrop, [1.0, 1.0, 1.0, 0.0], 1.0);
        for c in 0..4 {
            assert!(close(out[c], backdrop[c]));
        }
    }

    #[test]
    fn composite_over_transparent_backdrop_shows_source() {
        // First layer over an empty stage: blend must not darken it.
        let out = composite_pixel(BlendMode::Multiply, [0.0, 0.0, 0.0, 0.0], [0.6, 0.7, 0.8, 1.0], 1.0);
        assert!(close(out[0], 0.6) && close(out[1], 0.7) && close(out[2], 0.8) && close(out[3], 1.0));
    }

    #[test]
    fn glsl_source_contains_every_mode_id() {
        for mode in BlendMode::ALL {
            assert!(
                GLSL_BLEND_FN.contains(&format!("mode == {})", mode.shader_id())),
                "GLSL missing mode {mode:?}"
            );
        }
    }
}
