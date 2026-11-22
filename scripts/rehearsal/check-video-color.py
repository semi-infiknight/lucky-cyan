#!/usr/bin/env python3
"""Read back the production layer compositor on Linux EGL. Requires moderngl."""

import re
from pathlib import Path

import moderngl

ROOT = Path(__file__).resolve().parents[2] / "src-tauri/src/engine/output_engine"
blend_source = re.search(r'GLSL_BLEND_FN: &str = r#"(.*?)"#;', (ROOT / "blend.rs").read_text(), re.S)[1]
fragment = (ROOT / "composite.frag").read_text().replace("/*__BLEND_FUNCTIONS__*/", blend_source)
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
quad = context.vertex_array(program, [])
target = context.simple_framebuffer((1, 1), components=4)
backdrop, source = (context.texture((1, 1), 4) for _ in range(2))
backdrop.use(0)
source.use(1)
program["u_backdrop"].value = 0
program["u_layer"].value = 1
target.use()


def composite(background, foreground, saturation=1.0, mode=0, opacity=1.0):
    backdrop.write(bytes(background))
    source.write(bytes(foreground))
    program["u_saturation"].value = saturation
    program["u_blend_mode"].value = mode
    program["u_opacity"].value = opacity
    quad.render(vertices=3)
    return tuple(target.read(components=4))


def check(found, expected):
    assert all(abs(a - b) <= 1 for a, b in zip(found, expected)), (found, expected)


black = (0, 0, 0, 255)
red = (255, 0, 0, 255)
green = (0, 255, 0, 255)
blue = (0, 0, 255, 255)
check(composite(black, red), red)
mono = composite(black, red, saturation=0)
check(mono, (54, 54, 54, 255))
assert mono[0] == mono[1] == mono[2]
check(composite(black, (150, 102, 90, 255), saturation=2), (189, 93, 69, 255))
# Saturation affects the source only. At half opacity the green layer stays green.
check(composite(green, red, saturation=0, opacity=0.5), (27, 155, 27, 255))
# Multiply runs after monochrome. Lower-layer color still affects the result.
check(composite((128, 255, 64, 255), red, saturation=0, mode=2), (27, 54, 14, 255))
# Zero source alpha leaves the underlying layer intact, even in Add mode.
check(composite(blue, (255, 0, 0, 0), saturation=0, mode=1), blue)
check(composite((0, 0, 0, 0), (255, 0, 0, 128), saturation=0), (54, 54, 54, 128))
# A subsequent neutral overlay must retain its own RGB after a cue effect.
check(composite(mono, blue), blue)
print(f"Video saturation, layer blending, alpha and neutral overlays passed on {context.info['GL_RENDERER']}.")
