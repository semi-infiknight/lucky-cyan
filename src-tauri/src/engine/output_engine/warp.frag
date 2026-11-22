#version 150 core
uniform sampler2D u_tex;
uniform sampler2D u_mask;
uniform bool u_has_mask;
uniform mat3  u_hinv;
uniform vec2  u_size;
uniform vec4  u_trim;
uniform vec4  u_feather;
uniform int   u_region_count;
// x, y, right, bottom in normalized top-left coordinates.
uniform vec4  u_region_source[8];
uniform vec4  u_region_destination[8];
// Shared source span, signed axis (0 off; +/-1 X; +/-2 Y), inverse gamma.
uniform vec4 u_paired_feather;
out vec4 color;

bool source_at(vec2 destination, out vec2 source) {
    source = destination;
    if (u_region_count == 0) return true;
    // The last region in the layout is drawn on top.
    for (int i = u_region_count - 1; i >= 0; --i) {
        vec4 box = u_region_destination[i];
        if (all(greaterThanEqual(destination, box.xy)) && all(lessThan(destination, box.zw))) {
            vec4 crop = u_region_source[i];
            vec2 local = (destination - box.xy) / (box.zw - box.xy);
            vec2 mapped = mix(crop.xy, crop.zw, local);
            // Keep filtering inside the crop at pixel-aligned edges.
            vec2 inset = min(0.5 / vec2(textureSize(u_tex, 0)), 0.5 * (crop.zw - crop.xy));
            source = clamp(mapped, crop.xy + inset, crop.zw - inset);
            return true;
        }
    }
    return false;
}

float paired_gain(vec2 uv) {
    float axis = u_paired_feather.z;
    if (axis == 0.0) return 1.0;
    // Paired crops each fill one raster. Use the geometric source coordinate
    // before the sampling clamp so both ramps refer to the same seam.
    vec4 crop = u_region_source[0];
    vec2 source = mix(crop.xy, crop.zw, uv);
    float position = abs(axis) == 1.0 ? source.x : source.y;
    float span = u_paired_feather.y - u_paired_feather.x;
    float progress = clamp((position - u_paired_feather.x) / span, 0.0, 1.0);
    float light = axis > 0.0 ? progress : 1.0 - progress;
    // Encode complementary light weights for the matched projector transfer.
    if (light == 0.0 || light == 1.0) return light;
    return pow(light, u_paired_feather.w);
}

void main() {
    vec2 win = vec2(gl_FragCoord.x / u_size.x, 1.0 - gl_FragCoord.y / u_size.y);
    vec3 t = u_hinv * vec3(win, 1.0);
    if (t.z == 0.0) { color = vec4(0.0, 0.0, 0.0, 1.0); return; }
    vec2 uv = t.xy / t.z;
    vec2 src;
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || !source_at(uv, src)) {
        color = vec4(0.0, 0.0, 0.0, 1.0);
    } else {
        vec4 inside = vec4(uv.x, 1.0 - uv.x, uv.y, 1.0 - uv.y) - u_trim;
        vec4 ramp = clamp(inside / max(u_feather, vec4(1e-8)), 0.0, 1.0);
        vec4 gain = mix(step(vec4(0.0), inside), ramp, greaterThan(u_feather, vec4(0.0)));
        vec3 rgb = texture(u_tex, vec2(src.x, 1.0 - src.y)).rgb;
        float luma = u_has_mask ? texture(u_mask, uv).r : 1.0;
        color = vec4(rgb * luma * gain.x * gain.y * gain.z * gain.w * paired_gain(uv), 1.0);
    }
}
