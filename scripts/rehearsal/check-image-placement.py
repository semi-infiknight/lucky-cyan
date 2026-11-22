#!/usr/bin/env python3
"""Read back the production source-placement and blend pass on Linux EGL."""

import math
import re
from pathlib import Path

import moderngl

WIDTH, HEIGHT = 160, 100
ROOT = Path(__file__).resolve().parents[2] / "src-tauri/src/engine/output_engine"
blend = re.search(r'GLSL_BLEND_FN: &str = r#"(.*?)"#;', (ROOT / "blend.rs").read_text(), re.S)[1]
fragment = (ROOT / "composite.frag").read_text().replace("/*__BLEND_FUNCTIONS__*/", blend)
context = moderngl.create_standalone_context(backend="egl", require=330)
program = context.program(
    vertex_shader="""#version 150 core
const vec2 pos[3] = vec2[3](vec2(-1,-1), vec2(3,-1), vec2(-1,3));
out vec2 v_uv;
void main() {
    gl_Position = vec4(pos[gl_VertexID], 0, 1);
    v_uv = pos[gl_VertexID] * 0.5 + 0.5;
}
""",
    fragment_shader=fragment,
)
backdrop = context.texture((1, 1), 4, bytes((0, 0, 255, 255)))
backdrop.use(0)
source = context.texture((WIDTH, HEIGHT), 4)
source.filter = (moderngl.NEAREST, moderngl.NEAREST)
source.use(1)
target = context.simple_framebuffer((WIDTH, HEIGHT), components=4)
target.use()
program["u_backdrop"].value = 0
program["u_layer"].value = 1
program["u_saturation"].value = 1.0
program["u_opacity"].value = 1.0
quad = context.vertex_array(program, [])


def test_raster():
    """The FBO stores a full 160 × 80 picture, stretched to 160 × 100."""
    for gl_y in range(HEIGHT):
        top_y = HEIGHT - 1 - gl_y
        for x in range(WIDTH):
            if 111 <= x <= 119 and 46 <= top_y <= 54:
                yield from (255, 255, 0, 255)
            elif 24 <= x <= 32 and 12 <= top_y <= 25:
                yield from (128, 0, 0, 128)
            else:
                yield from (0, 0, 0, 0)


def draw(degrees, centre_x, picture_size=(160, 80), crop=(0, 0, 1, 1)):
    radians = math.radians(degrees)
    sine, cosine = math.sin(radians), math.cos(radians)
    w, h = picture_size
    program["u_place_picture"].value = True
    program["u_place_x"].value = (cosine * WIDTH / w, sine * HEIGHT / w,
                                  0.5 - (cosine * centre_x + sine * 50) / w)
    program["u_place_y"].value = (-sine * WIDTH / h, cosine * HEIGHT / h,
                                  0.5 - (-sine * centre_x + cosine * 50) / h)
    program["u_source_crop"].value = crop
    quad.render(vertices=3)
    return target.read(components=4)


def pixel(data, x, top_y):
    index = ((HEIGHT - 1 - top_y) * WIDTH + x) * 4
    return tuple(data[index:index + 4])


def has_yellow(data):
    yellow = bytes((255, 255, 0, 255))
    return any(data[i:i + 4] == yellow for i in range(0, len(data), 4))


source.write(bytes(test_raster()))
centred = draw(0, 80)
assert pixel(centred, 115, 50) == (255, 255, 0, 255)
assert pixel(centred, 28, 24) == (128, 0, 127, 255)
assert pixel(centred, 28, 76) == (0, 0, 255, 255)
assert not has_yellow(draw(0, 136)), "Yellow marker is entirely outside the initial stage."
rotated = draw(59, 136)
assert pixel(rotated, 154, 80) == (255, 255, 0, 255)
assert pixel(rotated, 80, 0) == (0, 0, 255, 255)
assert not has_yellow(draw(59, 80, picture_size=(104, 80), crop=(0, 0, 0.65, 1)))

program["u_place_picture"].value = False
quad.render(vertices=3)
assert pixel(target.read(components=4), 28, 18) == (64, 0, 127, 255)
print(f"Full-source rotation, crop, transparency and legacy pass verified on {context.info['GL_RENDERER']}.")
