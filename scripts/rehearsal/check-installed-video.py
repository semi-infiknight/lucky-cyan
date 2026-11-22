#!/usr/bin/env python3
"""Check installed Debian video output on a private Xvfb software receiver.

Usage: python3 scripts/rehearsal/check-installed-video.py NEW_EVIDENCE_DIRECTORY
Requires Inkue, Xvfb, xauth, xdotool, xwd, PipeWire, WirePlumber, and D-Bus.
The bundled 320x180, 12 fps, five-second silent H.264 clip is entirely solid red.
"""
import argparse
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time

STATES = [
    ("visible-control", "/inkue/cue/4/go", 3.0, (245, 255)),
    ("control-stopped", "/inkue/hardstop", 1.0, (0, 2)),
    ("video-zero", "/inkue/cue/1/go", 2.1, (0, 2)),
    ("fade-75", "/inkue/cue/2/go", 2.4, (185, 196)),
    ("reference-100", "/inkue/cue/3/go", 2.4, (245, 255)),
    ("hard-stop", "/inkue/hardstop", 1.0, (0, 2)),
    ("repeat-visible-control", "/inkue/cue/4/go", 3.0, (245, 255)),
    ("repeat-stopped", "/inkue/hardstop", 1.0, (0, 2)),
    ("prepared-in-blackout", "/inkue/cue/5/go", 1.5, (0, 2)),
    ("prepared-pre-wait", "/inkue/cue/6/go", .25, (0, 2)),
    ("prepared-revealed", None, 2.0, (245, 255)),
    ("prepared-stopped", "/inkue/hardstop", 1.0, (0, 2)),
    ("pause-load", "/inkue/cue/5/go", .75, (0, 2)),
    ("pause-pre-wait-start", "/inkue/cue/6/go", .15, (0, 2)),
    ("paused-past-entrance", "/inkue/pause", 2.0, (0, 2)),
    ("resumed-remainder", "/inkue/resume", .2, (0, 2)),
    ("resumed-revealed", None, 1.7, (245, 255)),
    ("resumed-stopped", "/inkue/hardstop", 1.0, (0, 2)),
]


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def wait_until(check, description, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(.1)
    raise RuntimeError("Timed out waiting for " + description)


def window(pattern):
    result = subprocess.run(["xdotool", "search", "--onlyvisible", "--name", pattern],
                            capture_output=True, text=True)
    return result.stdout.splitlines()[0] if result.stdout else None


def stop(process):
    process.terminate()
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def sample(raw, box):
    # The receiver emits little-endian BGR or BGRX XImages with 24-bit color depth.
    fields = struct.unpack_from(">25I", raw)
    width, height = fields[4:6]
    if (fields[1:4] != (7, 2, 24) or fields[7] != 0 or fields[11] not in (24, 32)
            or fields[14:17] != (0xff0000, 0xff00, 0xff)):
        raise ValueError("Unexpected XWD format for the software receiver")
    x, y, w, h = box
    if x < 0 or y < 0 or x + w > width or y + h > height:
        raise ValueError(f"Stage {box} is outside capture {width}x{height}")
    data = fields[0] + 12 * fields[19]
    pixels = []
    for row in range(1, 10):
        for col in range(1, 10):
            offset = data + (y + row * h // 10) * fields[12] + (x + col * w // 10) * (fields[11] // 8)
            pixels.append(list(raw[offset:offset+3][::-1]))
    return {"center": pixels[40], "min": list(map(min, zip(*pixels))),
            "max": list(map(max, zip(*pixels))), "samples": len(pixels)}


def inspect(evidence, stage, geometry, label, red_range, half=None):
    state = {}
    for surface in ("stage", "root"):
        name = label if surface == "stage" else label + "-root"
        capture = evidence / (name + ".xwd")
        selector = ["-id", stage] if surface == "stage" else ["-root"]
        subprocess.run(["xwd", "-silent", *selector, "-out", str(capture)], check=True)
        box = (0, 0, *geometry[2:]) if surface == "stage" else geometry
        if half is not None:
            x, y, width, height = box
            box = (x + half * (width // 2), y, width // 2, height)
        state[surface] = sample(capture.read_bytes(), box)
    state["passed"] = all(
        red_range[0] <= result["min"][0] <= result["max"][0] <= red_range[1]
        and max(result["max"][1:]) <= 3 for result in state.values())
    return state


def send_osc(osc, port, address):
    addr = address.encode() + b"\0"
    osc.sendto(addr + bytes(-len(addr) % 4) + b",\0\0\0", ("127.0.0.1", port))


def run(evidence):
    fixture_dir = Path(__file__).resolve().parent / "fixtures"
    show = evidence / "stage-fade.inkue"
    shutil.copyfile(fixture_dir / "stage-fade.json", show)
    shutil.copyfile(fixture_dir / "stage-red.mp4", evidence / "stage-red.mp4")
    # Distinct paths let the desk log identify the cue even when the picture is black.
    shutil.copyfile(fixture_dir / "stage-red.mp4", evidence / "stage-red-control.mp4")
    shutil.copyfile("/usr/share/doc/inkue/desk-build.json", evidence / "build.json")
    (evidence / "package.txt").write_text(command("dpkg-query", "-W", "inkue") + "\n")
    binary = shutil.which("inkue")
    (evidence / "binary.sha256").write_text(command("sha256sum", binary) + "\n")
    with ExitStack() as stack:
        def start(log, *args):
            stream = stack.enter_context((evidence / log).open("w"))
            proc = subprocess.Popen(args, stdout=stream, stderr=subprocess.STDOUT)
            stack.callback(stop, proc)
        with socket.socket(type=socket.SOCK_DGRAM) as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        conf = Path(os.environ["XDG_CONFIG_HOME"]) / "Inkue"
        conf.mkdir(parents=True)
        (conf / "osc.json").write_text(json.dumps(
            {"enabled": True, "port": port, "allowed_ips": ["127.0.0.1"]}))
        start("pipewire.log", "pipewire")
        wait_until(lambda: subprocess.run(["pw-cli", "info", "0"], capture_output=True).returncode == 0,
                   "private PipeWire server")
        start("wireplumber.log", "wireplumber")
        command("pw-cli", "create-node", "adapter",
                "{ factory.name=support.null-audio-sink node.name=stage-rehearsal "
                "media.class=Audio/Sink object.linger=true audio.position=[ FL FR ] }")
        start("desk.log", binary, str(show))
        wait_until(lambda: f"Opened workspace: {show}" in (evidence / "desk.log").read_text(),
                   "workspace to open")
        stage = window("^Inkue Output")
        if not stage:
            main = wait_until(lambda: window("^Inkue$"), "operator window")
            command("xdotool", "windowfocus", "--sync", main, "key", "F9")
            stage = wait_until(lambda: window("^Inkue Output"), "stage window")
        # Exercise a second-open as a touring operator loading the show from Files.
        subprocess.run([binary, str(show)], timeout=8, check=True)
        wait_until(lambda: (evidence / "desk.log").read_text().count(f"Opened workspace: {show}") >= 2,
                   "running desk to load the show")
        # The file-open log precedes the desk adopting the new workspace identity.
        # Time the first render from an authorized GO after that transition.
        time.sleep(2)
        command("xdotool", "windowraise", stage)
        geometry = command("xdotool", "getwindowgeometry", "--shell", stage)
        (evidence / "stage-window.txt").write_text(command("xdotool", "getwindowname", stage) + "\n" + geometry + "\n")
        fields = dict(line.split("=", 1) for line in geometry.splitlines())
        box = tuple(int(fields[key]) for key in ("X", "Y", "WIDTH", "HEIGHT"))
        result = {}
        onset = {}
        last_go_at = None
        with socket.socket(type=socket.SOCK_DGRAM) as osc:
            for label, address, seconds, red_range in STATES:
                start_at = time.monotonic()
                if address is not None:
                    last_go_at = start_at
                    send_osc(osc, port, address)
                deadline = start_at + seconds
                if label in ("visible-control", "repeat-visible-control", "prepared-revealed"):
                    # The prepared GO occurs in the preceding checkpoint.
                    # Include its programmed 1,500 ms wait in the observed latency.
                    first_red = None
                    while time.monotonic() < deadline:
                        raw = subprocess.check_output(["xwd", "-silent", "-id", stage], timeout=2)
                        rgb = sample(raw, (0, 0, *box[2:]))["center"]
                        if rgb[0] >= 245 and max(rgb[1:]) <= 3:
                            first_red = round((time.monotonic() - last_go_at) * 1000)
                            break
                        time.sleep(.075)
                    onset[label] = first_red
                time.sleep(max(0, deadline - time.monotonic()))
                result[label] = inspect(evidence, stage, box, label, red_range)
            # Load a second show on the same desk. Its transform maps the
            # whole frame into the left half of the projector raster.
            layout = evidence / "stage-left-region.inkue"
            document = json.loads(show.read_text())
            document["preferences"] = {"display": {"output_transform": {"regions": [
                {"source": [0, 0, 10000, 10000], "destination": [0, 0, 5000, 10000]},
            ]}}}
            layout.write_text(json.dumps(document))
            subprocess.run([binary, str(layout)], timeout=8, check=True)
            wait_until(lambda: f"Opened workspace: {layout}" in (evidence / "desk.log").read_text(),
                       "workspace with the stage region to open")
            # Native open precedes the desk loading the new workspace identity.
            # A numbered remote GO during that transition is intentionally held.
            time.sleep(2)
            stage = wait_until(lambda: window("^Inkue Output"), "stage region window")
            command("xdotool", "windowraise", stage)
            fields = dict(line.split("=", 1) for line in command(
                "xdotool", "getwindowgeometry", "--shell", stage).splitlines())
            box = tuple(int(fields[key]) for key in ("X", "Y", "WIDTH", "HEIGHT"))
            send_osc(osc, port, "/inkue/cue/4/go")
            time.sleep(3)
            result["mapped-left-half"] = inspect(evidence, stage, box, "mapped-left-half", (245, 255), half=0)
            result["uncovered-right-half"] = inspect(evidence, stage, box, "uncovered-right-half", (0, 2), half=1)
        (evidence / "first-red-ms.json").write_text(json.dumps(onset, indent=2) + "\n")
        (evidence / "pixels.json").write_text(json.dumps(result, indent=2) + "\n")
        return all(item["passed"] for item in result.values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--session", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    evidence = args.evidence.resolve()
    if args.session:
        return 0 if run(evidence) else 1
    evidence.mkdir()
    with tempfile.TemporaryDirectory() as runtime:
        env = {**os.environ, "XDG_RUNTIME_DIR": runtime, "INKUE_HWDEC": "no",
               "LIBGL_ALWAYS_SOFTWARE": "1", "GDK_BACKEND": "x11"}
        for key in ("CONFIG", "DATA", "CACHE", "STATE"):
            env[f"XDG_{key}_HOME"] = str(Path(runtime, key.lower()))
        # Separate IPC namespaces can prevent Mesa from presenting with MIT-SHM.
        code = subprocess.run(["dbus-run-session", "--", "xvfb-run", "-a", "-s",
                               "-screen 0 1440x960x24 -nolisten tcp -extension MIT-SHM",
                               sys.executable, str(Path(__file__).resolve()), "--session", str(evidence)],
                              env=env).returncode
    with (evidence / "SHA256SUMS").open("w") as hashes:
        for path in sorted(evidence.iterdir()):
            if path.name != "SHA256SUMS":
                hashes.write(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n")
    return code


if __name__ == "__main__":
    sys.exit(main())
