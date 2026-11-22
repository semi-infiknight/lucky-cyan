#version 150 core
uniform sampler2D u_backdrop;
uniform sampler2D u_layer;
uniform int   u_blend_mode;
uniform float u_opacity;
// Saturation multiplier, additive brightness, contrast, and hue in turns.
uniform vec4 u_color_controls;
uniform bool u_place_picture;
uniform vec3 u_place_x;
uniform vec3 u_place_y;
uniform vec4 u_source_crop;
in vec2 v_uv;
out vec4 color;

/*__BLEND_FUNCTIONS__*/

vec4 sample_layer() {
    if (!u_place_picture) return texture(u_layer, v_uv);
    vec3 point = vec3(v_uv.x, 1.0 - v_uv.y, 1.0);
    vec2 local = vec2(dot(u_place_x, point), dot(u_place_y, point));
    if (any(lessThan(local, vec2(0.0))) || any(greaterThan(local, vec2(1.0)))) {
        return vec4(0.0);
    }
    vec2 half_texel = min(0.5 / vec2(textureSize(u_layer, 0)), 0.5 * u_source_crop.zw);
    vec2 uv = u_source_crop.xy + clamp(
        local * u_source_crop.zw, half_texel, u_source_crop.zw - half_texel
    );
    // libmpv's transparent RGBA output is premultiplied. Interpolate it in
    // that form, then restore the straight RGB used by the blend equations.
    vec4 sample = texture(u_layer, vec2(uv.x, 1.0 - uv.y));
    if (sample.a > 0.0) sample.rgb = clamp(sample.rgb / sample.a, 0.0, 1.0);
    return sample;
}

// Rotate the HSV hue while preserving the source value and color range.
vec3 rotate_hue(vec3 rgb, float turns) {
    float hi = max(max(rgb.r, rgb.g), rgb.b);
    float lo = min(min(rgb.r, rgb.g), rgb.b);
    float chroma = hi - lo;
    if (chroma == 0.0) return rgb;
    float hue;
    if (hi == rgb.r) hue = (rgb.g - rgb.b) / chroma;
    else if (hi == rgb.g) hue = 2.0 + (rgb.b - rgb.r) / chroma;
    else hue = 4.0 + (rgb.r - rgb.g) / chroma;
    vec3 wheel = abs(mod(hue + 6.0 * turns + vec3(0.0, 4.0, 2.0), 6.0) - 3.0) - 1.0;
    return lo + chroma * clamp(wheel, 0.0, 1.0);
}

void main() {
    vec4 b = texture(u_backdrop, v_uv);
    vec4 s = sample_layer();
    // Use the rendered SDR color values. The effect applies to this layer's
    // source only, before its blend mode. The source alpha is independent.
    if (any(notEqual(u_color_controls, vec4(1.0, 0.0, 1.0, 0.0)))) {
        if (u_color_controls.x != 1.0) {
            float luma = dot(s.rgb, vec3(0.2126, 0.7152, 0.0722));
            s.rgb = mix(vec3(luma), s.rgb, u_color_controls.x);
        }
        if (u_color_controls.z != 1.0) s.rgb = (s.rgb - 0.5) * u_color_controls.z + 0.5;
        s.rgb = clamp(s.rgb + u_color_controls.y, 0.0, 1.0);
        if (u_color_controls.w != 0.0) s.rgb = rotate_hue(s.rgb, u_color_controls.w);
    }
    color = composite_stage(u_blend_mode, b, s.rgb, clamp(s.a * u_opacity, 0.0, 1.0));
}
