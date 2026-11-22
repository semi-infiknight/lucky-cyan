#!/usr/bin/env python3
"""Linux EGL pixel acceptance for projector masks and stage regions. Requires moderngl."""

import struct
from pathlib import Path

import moderngl

SIZE = 100
CLEAR = (0, 0, 0, 0)
IDENTITY = (1, 0, 0, 0, 1, 0, 0, 0, 1)
ROOT = Path(__file__).resolve().parents[2]
shader = ROOT / "src-tauri/src/engine/output_engine/warp.frag"
context = moderngl.create_standalone_context(backend="egl", require=330)
program = context.program(
    vertex_shader="""#version 150 core
    const vec2 pos[3] = vec2[3](vec2(-1,-1), vec2(3,-1), vec2(-1,3));
    void main() { gl_Position = vec4(pos[gl_VertexID], 0.0, 1.0); }
    """,
    fragment_shader=shader.read_text(),
)
quad = context.vertex_array(program, [])
source = context.texture((1, 1), 3, bytes([255, 255, 255]))
source.use(0)
target = context.simple_framebuffer((SIZE, SIZE))
target.use()
program["u_size"].value = (SIZE, SIZE)
program["u_tex"].value = 0


def draw(trim=CLEAR, feather=CLEAR, inverse=IDENTITY, image_mask=False, regions=(), components=1, paired=CLEAR):
    program["u_has_mask"].value = image_mask
    program["u_mask"].value = 1
    program["u_trim"].value = trim
    program["u_feather"].value = feather
    program["u_paired_feather"].value = paired
    program["u_hinv"].write(struct.pack("9f", *inverse))
    program["u_region_count"].value = len(regions)
    if regions:
        sources, destinations = zip(*regions)
        for name, rectangles in [("u_region_source", sources), ("u_region_destination", destinations)]:
            values = [coordinate for rectangle in rectangles for coordinate in rectangle]
            values.extend([0.0] * (program[name].array_length * 4 - len(values)))
            program[name].write(struct.pack(f"{len(values)}f", *values))
    quad.render(vertices=3)
    return target.read(components=components, alignment=1)


def pixel(frame, x, y):
    return frame[(SIZE - 1 - y) * SIZE + x]


def check_pixel(frame, x, y, expected):
    measured = pixel(frame, x, y)
    assert abs(measured - expected) <= 1, (x, y, measured, expected)


assert min(draw()) == 255
hard = draw(trim=(0.08, 0.16, 0.22, 0.30))
for x, y in [(7, 50), (84, 50), (50, 21), (50, 70)]:
    check_pixel(hard, x, y, 0)
for x, y in [(8, 50), (83, 50), (50, 22), (50, 69)]:
    check_pixel(hard, x, y, 255)

soft = draw(trim=(0.1, 0, 0, 0), feather=(0.2, 0, 0.2, 0))
check_pixel(soft, 19, 50, round(0.475 * 255))
check_pixel(soft, 19, 9, round(0.475 * 0.475 * 255))
check_pixel(soft, 60, 50, 255)

# Match the two projectors across a 20% overlap in the source.
left = draw(feather=(0, 0.2, 0, 0))
right = draw(feather=(0.2, 0, 0, 0))
for i in range(20):
    assert abs(pixel(left, 80 + i, 50) + pixel(right, i, 50) - 255) <= 1

# Both paired ramps use the same source coordinates, then encode the matched
# projector transfer exponent. A subpixel shift matches the test raster phases.
whole = (0, 0, 1, 1)
phase = 0.67 - 0.4 / 0.6
for axis in [1, 2]:
    left_crop, right_crop = ((0, 0, .6, 1), (.4, 0, 1, 1)) if axis == 1 else ((0, 0, 1, .6), (0, .4, 1, 1))
    dx, dy = (phase, 0) if axis == 1 else (0, phase)
    shift = (1, 0, 0, 0, 1, 0, dx, dy, 1)
    for gamma in [1, 2.2]:
        left = draw(regions=((left_crop, whole),), paired=(.4, .6, -axis, 1 / gamma))
        right = draw(regions=((right_crop, whole),), paired=(.4, .6, axis, 1 / gamma), inverse=shift)
        for index in range(33):
            first = pixel(left, 67 + index, 50) if axis == 1 else pixel(left, 50, 67 + index)
            second = pixel(right, index, 50) if axis == 1 else pixel(right, 50, index)
            combined = (first / 255) ** gamma + (second / 255) ** gamma
            assert abs(combined - 1.0) < .008, (axis, gamma, index, combined)
        check_pixel(left, 35, 35, 255)
        check_pixel(right, 50, 50, 255)

# A venue trim still cuts the region after its paired ramp is calculated.
combined_trim = draw(regions=(((0, 0, .6, 1), whole),), paired=(.4, .6, -1, 1), trim=(0, 0, .2, 0))
check_pixel(combined_trim, 83, 9, 0)
check_pixel(combined_trim, 83, 50, round((.6 - .6 * .835) / .2 * 255))

# Translating the picture 10 pixels carries its mask with it.
translated = draw(trim=(0.2, 0, 0, 0), inverse=(1, 0, 0, 0, 1, 0, -0.1, 0, 1))
check_pixel(translated, 29, 50, 0)
check_pixel(translated, 30, 50, 255)
# Top row is black / white; bottom row is half gray / white. PNG storage
# is top-down and the shader samples the mask in unwarped picture space.
matte = context.texture((2, 2), 1, bytes([0, 255, 128, 255]))
matte.repeat_x = matte.repeat_y = False
matte.use(1)
imaged = draw(image_mask=True)
check_pixel(imaged, 24, 24, 0)
check_pixel(imaged, 75, 24, 255)
check_pixel(imaged, 24, 75, 128)
cropped = draw(trim=(0, .3, 0, 0), image_mask=True)
check_pixel(cropped, 75, 24, 0)
check_pixel(cropped, 24, 75, 128)
assert min(draw()) == 255  # clear mask restores the field

# Image rows in a GL color texture start at the bottom. Each test quadrant
# is 2x2 so a mistaken coordinate orientation or linear bleed is visible.
RED, GREEN, BLUE, GOLD, BLACK = (255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 215, 0), (0, 0, 0)
card = context.texture((4, 4), 3, bytes(
    (BLUE * 2 + GOLD * 2) * 2 + (RED * 2 + GREEN * 2) * 2
))
card.repeat_x = card.repeat_y = False
card.use(0)
whole = (0, 0, 1, 1)
left_half = (0, 0, .5, 1)
right_half = (.5, 0, 1, 1)


def check_rgb(frame, x, y, expected):
    index = ((SIZE - 1 - y) * SIZE + x) * 3
    measured = tuple(frame[index:index + 3])
    assert all(abs(a - b) <= 1 for a, b in zip(measured, expected)), (x, y, measured, expected)


swapped = draw(regions=((right_half, left_half), (left_half, right_half)), components=3)
for x, y, expected in [(25, 25, GREEN), (75, 25, RED), (25, 75, GOLD), (75, 75, BLUE)]:
    check_rgb(swapped, x, y, expected)
check_rgb(swapped, 49, 10, GREEN)
check_rgb(swapped, 50, 10, RED)

# The two saved paired crops fill their separate rasters from one silent chart.
left_output = draw(regions=((left_half, whole),), components=3)
right_output = draw(regions=((right_half, whole),), components=3)
for x in [0, 99]:
    check_rgb(left_output, x, 25, RED)
    check_rgb(left_output, x, 75, BLUE)
    check_rgb(right_output, x, 25, GREEN)
    check_rgb(right_output, x, 75, GOLD)
top_output = draw(regions=(((0, 0, 1, .5), whole),), components=3)
bottom_output = draw(regions=(((0, .5, 1, 1), whole),), components=3)
for y in [0, 99]:
    check_rgb(top_output, 25, y, RED)
    check_rgb(top_output, 75, y, GREEN)
    check_rgb(bottom_output, 25, y, BLUE)
    check_rgb(bottom_output, 75, y, GOLD)

duplicate = draw(regions=((left_half, left_half), (left_half, right_half)), components=3)
check_rgb(duplicate, 25, 25, RED)
check_rgb(duplicate, 75, 25, RED)
overlapping = draw(regions=((left_half, whole), ((.5, .5, 1, 1), (.2, .2, .4, .4))), components=3)
check_rgb(overlapping, 99, 10, RED)  # scaled crop has no green bleed
check_rgb(overlapping, 30, 30, GOLD)  # later region is painted on top

gaps = draw(regions=((right_half, (.10, .10, .50, .90)),), components=3)
check_rgb(gaps, 10, 9, BLACK)
check_rgb(gaps, 10, 10, GREEN)
check_rgb(gaps, 50, 25, BLACK)
check_rgb(gaps, 25, 75, GOLD)

# Final trim follows the destination coordinate, which has been shifted
# ten pixels by the projector warp. Physical mask assets remain in that space.
shifted = draw(regions=((right_half, left_half),), trim=(.2, 0, 0, 0),
               inverse=(1, 0, 0, 0, 1, 0, -.1, 0, 1), components=3)
check_rgb(shifted, 29, 10, BLACK)
check_rgb(shifted, 30, 10, GREEN)
check_rgb(shifted, 70, 10, BLACK)
matte.use(1)
masked = draw(regions=((right_half, left_half),), image_mask=True, components=3)
check_rgb(masked, 10, 10, BLACK)  # black top-left matte even though source is bright top-right

all_regions = draw(regions=((left_half, whole),) * 7 + ((right_half, whole),), components=3)
check_rgb(all_regions, 25, 25, GREEN)
check_rgb(draw(components=3), 25, 25, RED)  # clearing layout restores the complete source
print(f"Output mask and stage region pixel checks passed on {context.info['GL_RENDERER']}.")
