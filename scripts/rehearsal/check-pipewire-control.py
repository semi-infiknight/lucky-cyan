#!/usr/bin/env python3
"""Measure an independent PipeWire tone on a fresh virtual graph."""

import argparse
import array
from contextlib import ExitStack
import json
import math
import os
from pathlib import Path
import resource
import subprocess
import sys
import tempfile
import time
import wave

from linux_soak import Monitor, RATE, rms


def wait(check, message):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(.1)
    raise RuntimeError(message)


def stop(process):
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def cpu_cgroup():
    for entry in Path("/proc/self/cgroup").read_text().splitlines():
        if entry.startswith("0::"):
            group = Path("/sys/fs/cgroup") / entry[3:].lstrip("/")
            return {name: value.read_text().strip() for name in (
                "cpu.max", "cpu.stat", "cpuset.cpus.effective",
                "cpuset.cpus.exclusive.effective", "cpuset.cpus.partition",
            ) if (value := group / name).is_file()}
    return {}


def scheduler_classes(process):
    policies = set()
    for thread in Path(f"/proc/{process.pid}/task").glob("[0-9]*"):
        try:
            policies.add(os.sched_getscheduler(int(thread.name)))
        except ProcessLookupError:
            pass
    return sorted(name for name in ("SCHED_OTHER", "SCHED_FIFO", "SCHED_RR", "SCHED_BATCH",
                                    "SCHED_IDLE", "SCHED_DEADLINE")
                  if getattr(os, name, None) in policies)


def run(evidence, quantum, seconds):
    (evidence / "host.json").write_text(json.dumps({
        "cpu_affinity": sorted(os.sched_getaffinity(0)),
        "cpu_cgroup": cpu_cgroup(),
        "rt_priority_limit": resource.getrlimit(resource.RLIMIT_RTPRIO),
    }, indent=2) + "\n")
    with ExitStack() as stack:
        def start(name, *command):
            log = stack.enter_context((evidence / f"{name}.log").open("wb"))
            process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
            stack.callback(stop, process)
            return process

        def has_server():
            return subprocess.run(["pw-cli", "info", "0"], capture_output=True).returncode == 0

        def has_sink():
            outcome = subprocess.run(["wpctl", "inspect", "@DEFAULT_AUDIO_SINK@"],
                                     capture_output=True, text=True)
            return 'node.name = "rehearsal"' in outcome.stdout

        server = start("pipewire", "pipewire")
        wait(has_server, "The private PipeWire graph did not start")
        for key, value in (("rate", RATE), ("quantum", quantum)):
            subprocess.run(["pw-metadata", "-n", "settings", "0", f"clock.force-{key}",
                            str(value)], check=True)
        policy = start("wireplumber", "wireplumber")
        subprocess.run(["pw-cli", "create-node", "adapter",
                        "{ factory.name=support.null-audio-sink node.name=rehearsal "
                        "media.class=Audio/Sink object.linger=true audio.position=[ FL FR ] }"],
                       check=True)
        wait(has_sink, "The private sink did not become the default")
        subprocess.run(["wpctl", "set-volume", "@DEFAULT_AUDIO_SINK@", "1"], check=True)
        subprocess.run(["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "0"], check=True)
        start("graph", "pw-top", "-b")

        tone_path = Path(os.environ["XDG_RUNTIME_DIR"]) / "control.wav"
        values = array.array("h", (round(1100 * math.sin(2 * math.pi * 440 * i / RATE))
                                   for i in range(RATE))).tobytes()
        with wave.open(str(tone_path), "wb") as tone:
            tone.setparams((1, 2, RATE, 0, "NONE", "not compressed"))
            for _ in range(seconds + 5):
                tone.writeframesraw(values)
        monitor = Monitor("rehearsal")
        stack.callback(monitor.close)
        monitor.wait(RATE // 4)
        origin = monitor.position()
        started = time.monotonic()
        player = start("player", "pw-play", "--target", "rehearsal", str(tone_path))
        warmup_end = origin + RATE
        monitor.wait(warmup_end)
        startup = monitor.read(origin, warmup_end)
        onset = next((i for i, value in enumerate(startup) if abs(value) >= 100), None)
        if onset is None:
            raise RuntimeError("Control tone did not start within one second of capture")
        (evidence / "startup.json").write_text(json.dumps({
            "capture_onset_ms": round(onset * 1000 / RATE, 1),
            "scheduler_classes": {name: scheduler_classes(process) for name, process in (
                ("pipewire", server), ("wireplumber", policy), ("pw-play", player),
                ("pw-record", monitor.process),
            )},
        }, indent=2) + "\n")

        minimum = float("inf")
        low = 0
        block = RATE // 100
        with (evidence / "blocks.jsonl").open("w") as output:
            for second in range(2, seconds + 2):
                end = origin + second * RATE
                monitor.wait(end)
                samples = monitor.read(end - RATE, end)
                levels = [rms(samples[i:i + block]) for i in range(0, len(samples), block)]
                lowest = min(levels)
                low_blocks = [{"capture_ms": (second - 1) * 1000 + i * 10, "rms": round(value, 1)}
                              for i, value in enumerate(levels) if value < 500]
                minimum = min(minimum, lowest)
                low += len(low_blocks)
                output.write(json.dumps({"elapsed_s": second, "low_blocks": len(low_blocks),
                                         "wall_s": round(time.monotonic() - started, 3),
                                         "minimum_rms": round(lowest, 1),
                                         "dropouts": low_blocks}) + "\n")
                output.flush()
        summary = {"graph_quantum": quantum, "10ms_blocks": seconds * 100,
                   "low_blocks": low, "minimum_rms": round(minimum, 1),
                   "cpu_cgroup_after_capture": cpu_cgroup()}
        (evidence / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        return 1 if low else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--quantum", type=int, choices=(64, 128, 256, 512, 1024, 2048), default=1024)
    parser.add_argument("--seconds", type=int, default=62)
    parser.add_argument("--session", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.seconds < 1:
        parser.error("--seconds must be positive")
    evidence = args.evidence.resolve()
    if args.session:
        return run(evidence, args.quantum, args.seconds)
    evidence.mkdir()
    with tempfile.TemporaryDirectory() as runtime:
        env = {**os.environ, "XDG_RUNTIME_DIR": runtime}
        for name in ("CONFIG", "STATE", "DATA", "CACHE"):
            env[f"XDG_{name}_HOME"] = str(Path(runtime) / name.lower())
        for name in ("PIPEWIRE_REMOTE", "PIPEWIRE_RUNTIME_DIR"):
            env.pop(name, None)
        return subprocess.run(["dbus-run-session", "--", sys.executable,
                               str(Path(__file__).resolve()), "--session", "--quantum",
                               str(args.quantum), "--seconds", str(args.seconds), str(evidence)],
                              env=env).returncode


if __name__ == "__main__":
    raise SystemExit(main())
