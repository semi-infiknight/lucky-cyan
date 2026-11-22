#version 150 core
uniform sampler2D u_backdrop;
uniform sampler2D u_layer;
uniform int   u_blend_mode;
uniform float u_opacity;
uniform float u_saturation;
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

void main() {
    vec4 b = texture(u_backdrop, v_uv);
    vec4 s = sample_layer();
    // Use the rendered SDR color values. The effect applies to this layer's
    // source only, before its blend mode. The source alpha is independent.
    if (u_saturation != 1.0) {
        float luma = dot(s.rgb, vec3(0.2126, 0.7152, 0.0722));
        s.rgb = clamp(mix(vec3(luma), s.rgb, u_saturation), 0.0, 1.0);
    }
    float sa = clamp(s.a * u_opacity, 0.0, 1.0);
    float ao = sa + b.a * (1.0 - sa);
    vec3 blended = (1.0 - b.a) * s.rgb + b.a * blend_rgb(u_blend_mode, b.rgb, s.rgb);
    vec3 rgb = sa * blended + (1.0 - sa) * b.a * b.rgb;
    if (ao > 0.0) rgb /= ao;
    color = vec4(rgb, ao);
}
