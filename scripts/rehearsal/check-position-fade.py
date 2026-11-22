#!/usr/bin/env python3
"""Measure a native position or scale Fade on a private Linux X11 output.

Run INKUE_BINARY=/absolute/path/to/inkue python3 check-position-fade.py NEW_EVIDENCE_DIRECTORY.
Without INKUE_BINARY, the check uses the installed Inkue. It shares the X11
helpers and the synthetic red clip with check-installed-video.py.
Requires Xvfb, xauth, xdotool, xwd, PipeWire, WirePlumber, and D-Bus.
"""
import argparse
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parent
VIDEO = runpy.run_path(str(ROOT / "check-installed-video.py"))
STEPS = 40
SCALE = .30
LEFT = -.75
RIGHT = .75
DURATION_MS = 900
PATH_DURATION_MS = 4000
WAYPOINT_Y = -.75
END_SCALE = .70


def prepare_show(evidence, waypoint=False, scale=False):
    document = json.loads((ROOT / "fixtures/stage-fade.json").read_text())
    document["workspace"]["name"] = "Rehearsal position Fade"
    document["osc_patches"] = []
    cue_list = document["cue_lists"][0]
    picture, fade = cue_list["cues"][:2]
    picture.update(name="Red picture at saved position", layer_style={"opacity": 1.0},
                   geometry={"scale": SCALE, "pan_x": LEFT, "pan_y": 0})
    fade.update(name="Move the live picture horizontally", fade_brightness=False,
                target_translation={"x": RIGHT, "y": None}, fade_duration_ms=DURATION_MS,
                fade_curve="linear")
    if waypoint:
        fade.update(name="Move the live picture through a waypoint", fade_duration_ms=PATH_DURATION_MS,
                    target_translation={"x": RIGHT, "y": 0}, translation_waypoints=[[0, WAYPOINT_Y]])
    if scale:
        document["workspace"]["name"] = "Rehearsal scale Fade"
        picture["geometry"]["pan_x"] = 0
        fade.update(name="Grow the live picture", fade_duration_ms=PATH_DURATION_MS,
                    target_translation=None, target_scale=END_SCALE)
    cue_list["cues"] = [picture, fade]
    show = evidence / "position-fade.inkue"
    show.write_text(json.dumps(document, indent=2) + "\n")
    shutil.copyfile(ROOT / "fixtures/stage-red.mp4", evidence / "stage-red.mp4")
    return show


def span(raw, box, pan_y, scale):
    """Find the red picture's left and right edges in a horizontal strip."""
    x, y, width, height = box
    probe_width, probe_height = max(3, width // 100), max(3, height // 20)
    strip_y = y + round(height * (.5 + scale * pan_y)) - probe_height // 2
    hits = []
    for step in range(1, STEPS):
        offset = step / STEPS
        probe_x = x + round(width * offset) - probe_width // 2
        rgb = VIDEO["sample"](raw, (probe_x, strip_y, probe_width, probe_height))
        if rgb["min"][0] >= 245 and max(rgb["max"][1:]) <= 3:
            hits.append(offset)
    if not hits:
        return {"red_samples": 0, "center": None, "width": None}
    return {"red_samples": len(hits), "center": round((hits[0] + hits[-1]) / 2, 4),
            "width": round(hits[-1] - hits[0], 4)}


def matches(measured, pan, scale):
    if pan is None:
        return measured["red_samples"] == 0
    return (measured["center"] is not None and abs(measured["center"] - (.5 + scale * pan)) < .06
            and scale - .09 <= measured["width"] <= scale + .04)


def checkpoint(evidence, stage, geometry, label, pan, pan_y, scale):
    result = {"expected_pan_x": pan, "expected_pan_y": pan_y, "expected_scale": scale}
    stage_box = (0, 0, *geometry[2:])

    # Poll for the picture independently of compositor or decoding wake-ups.
    # Save both captures even when the stage cannot reach the requested look.
    deadline = time.monotonic() + 4
    while True:
        raw = subprocess.check_output(["xwd", "-silent", "-id", stage], timeout=2)
        measured = span(raw, stage_box, pan_y, scale)
        if matches(measured, pan, scale) or time.monotonic() >= deadline:
            (evidence / f"{label}.xwd").write_bytes(raw)
            result["stage"] = measured
            break
        time.sleep(.1)
    raw = subprocess.check_output(["xwd", "-silent", "-root"], timeout=2)
    (evidence / f"{label}-root.xwd").write_bytes(raw)
    result["root"] = span(raw, geometry, pan_y, scale)
    result["passed"] = all(matches(result[surface], pan, scale) for surface in ("stage", "root"))
    return result


def run(evidence, binary, waypoint, scale):
    show = prepare_show(evidence, waypoint, scale)
    command, wait_until = VIDEO["command"], VIDEO["wait_until"]
    (evidence / "binary.sha256").write_text(command("sha256sum", binary) + "\n")
    with ExitStack() as stack:
        def start(log, *args):
            stream = stack.enter_context((evidence / log).open("w"))
            process = subprocess.Popen(args, stdout=stream, stderr=subprocess.STDOUT)
            stack.callback(VIDEO["stop"], process)

        start("pipewire.log", "pipewire")
        wait_until(lambda: subprocess.run(["pw-cli", "info", "0"], capture_output=True).returncode == 0,
                   "private PipeWire server")
        start("wireplumber.log", "wireplumber")
        command("pw-cli", "create-node", "adapter",
                "{ factory.name=support.null-audio-sink node.name=position-rehearsal "
                "media.class=Audio/Sink object.linger=true audio.position=[ FL FR ] }")
        start("desk.log", binary, str(show))
        wait_until(lambda: f"Opened workspace: {show}" in (evidence / "desk.log").read_text(),
                   "workspace to open")
        window = VIDEO["window"]
        main = wait_until(lambda: window("^Inkue$"), "operator window")
        stage = window("^Inkue Output")
        if not stage:
            command("xdotool", "windowfocus", "--sync", main, "key", "F9")
            stage = wait_until(lambda: window("^Inkue Output"), "stage window")
        time.sleep(2)  # The operator window adopts the identity after native open.
        command("xdotool", "windowraise", stage)
        raw_geometry = command("xdotool", "getwindowgeometry", "--shell", stage)
        (evidence / "stage-window.txt").write_text(raw_geometry + "\n")
        fields = dict(line.split("=", 1) for line in raw_geometry.splitlines())
        geometry = tuple(int(fields[key]) for key in ("X", "Y", "WIDTH", "HEIGHT"))

        origin = 0 if scale else LEFT
        phases = [("saved-origin", ("space",), .6, origin, 0, SCALE)]
        if scale:
            phases.extend([
                ("scale-midpoint", ("space",), PATH_DURATION_MS / 2000, 0, 0, (SCALE + END_SCALE) / 2),
                ("scale-endpoint", (), PATH_DURATION_MS / 2000 + .4, 0, 0, END_SCALE),
            ])
        elif waypoint:
            # At the path apex the sample strip is above the whole straight
            # move. A horizontal-only route cannot satisfy this checkpoint.
            phases.extend([
                ("waypoint", ("space",), PATH_DURATION_MS / 2000, 0, WAYPOINT_Y, SCALE),
                ("position-endpoint", (), PATH_DURATION_MS / 2000 + .4, RIGHT, 0, SCALE),
            ])
        else:
            phases.append(("position-endpoint", ("space",), DURATION_MS / 1000 + .4, RIGHT, 0, SCALE))
        phases.extend([
            ("hard-stopped", ("Escape", "Escape"), .2, None, 0, SCALE),
            ("restarted-at-origin", ("space",), .6, origin, 0, SCALE),
        ])
        results = {}
        for label, keys, delay, pan, pan_y, expected_scale in phases:
            # Focus the desk while leaving the stage raised for root capture.
            command("xdotool", "windowfocus", "--sync", main)
            if label == "restarted-at-origin":
                command("xdotool", "key", "g")
                time.sleep(.3)
                command("xdotool", "type", "--clearmodifiers", "1")
                command("xdotool", "key", "Return")
                time.sleep(.4)
            if keys:
                command("xdotool", "key", "--clearmodifiers", "--delay", "100", *keys)
            time.sleep(delay)
            command("xdotool", "windowraise", stage)
            results[label] = checkpoint(evidence, stage, geometry, label, pan, pan_y, expected_scale)
        (evidence / "pixels.json").write_text(json.dumps(results, indent=2) + "\n")
    return 0 if all(check["passed"] for check in results.values()) else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    route = parser.add_mutually_exclusive_group()
    route.add_argument("--waypoint", action="store_true", help="Measure a two-axis waypoint above the stage centre")
    route.add_argument("--scale", action="store_true", help="Measure the size at the midpoint, endpoint, and after a fresh GO")
    parser.add_argument("--session", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    evidence = args.evidence.resolve()
    configured = os.environ.get("INKUE_BINARY", "inkue")
    binary = shutil.which(configured)
    if not binary:
        parser.error(f"Unable to execute {configured!r}")
    binary = str(Path(binary).resolve())
    if args.session:
        return run(evidence, binary, args.waypoint, args.scale)
    evidence.mkdir()
    with tempfile.TemporaryDirectory() as runtime:
        env = {**os.environ, "INKUE_BINARY": binary, "XDG_RUNTIME_DIR": runtime, "INKUE_HWDEC": "no",
               "LIBGL_ALWAYS_SOFTWARE": "1", "GDK_BACKEND": "x11"}
        for key in ("CONFIG", "DATA", "CACHE", "STATE"):
            env[f"XDG_{key}_HOME"] = str(Path(runtime, key.lower()))
        command = ["dbus-run-session", "--", "xvfb-run", "-a", "-s",
                   "-screen 0 1440x960x24 -nolisten tcp -extension MIT-SHM",
                   sys.executable, str(Path(__file__).resolve()), "--session", str(evidence)]
        if args.waypoint:
            command.append("--waypoint")
        if args.scale:
            command.append("--scale")
        code = subprocess.run(command, env=env).returncode
    with (evidence / "SHA256SUMS").open("w") as hashes:
        for entry in sorted(evidence.iterdir()):
            if entry.name != "SHA256SUMS":
                hashes.write(f"{hashlib.sha256(entry.read_bytes()).hexdigest()}  {entry.name}\n")
    return code


if __name__ == "__main__":
    sys.exit(main())
