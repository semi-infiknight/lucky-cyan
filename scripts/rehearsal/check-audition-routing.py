"""Verify the actual clip-audition player in a private, two-sink PipeWire session.

Build the Inkue library test executable with
`cargo test --manifest-path src-tauri/Cargo.toml --lib --no-run`.
Pass the path printed by Cargo to this script.
"""
from contextlib import ExitStack
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

from linux_soak import Monitor, RATE

HOUSE = "inkue-test-house"
PHONES = "inkue-test-headphones"


def run(*args):
    return subprocess.check_output(args, timeout=4, text=True, stderr=subprocess.DEVNULL)


def node_id(name):
    nodes = json.loads(run("pw-dump"))
    return next((n["id"] for n in nodes if n.get("info", {}).get("props", {}).get("node.name") == name), None)


def wait_for(check):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            if value := check():
                return value
        except subprocess.CalledProcessError:
            pass
        time.sleep(.05)
    raise RuntimeError("PipeWire did not reach the expected graph state")


def stop(process):
    if process.poll() is None:
        process.kill()
    process.wait(timeout=3)


def tone_amplitude(pcm, frequency):
    start = next(i for i, sample in enumerate(pcm) if abs(sample) > 200) + RATE // 4
    steady = pcm[start:start + RATE]
    assert len(steady) == RATE
    angle = math.tau * frequency / RATE
    real = math.fsum(sample * math.cos(angle * i) for i, sample in enumerate(steady))
    imaginary = math.fsum(sample * math.sin(angle * i) for i, sample in enumerate(steady))
    return math.hypot(real, imaginary)


def graph_check(binary):
    with ExitStack() as cleanup:
        def launch(args, **kwargs):
            process = subprocess.Popen(args, **kwargs)
            cleanup.callback(stop, process)
            return process

        def tone(signal):
            env = dict(os.environ, INKUE_AUDITION_GRAPH_SIGNAL=signal)
            return launch([binary, "--ignored", "isolated_graph_tone"], env=env)

        launch(["pipewire"], stderr=subprocess.DEVNULL)
        wait_for(lambda: run("pw-cli", "info", "0"))
        launch(["wireplumber"], stderr=subprocess.DEVNULL)
        for name in (HOUSE, PHONES):
            run("pw-cli", "create-node", "adapter",
                f"{{ factory.name=support.null-audio-sink node.name={name} media.class=Audio/Sink object.linger=true audio.position=[ FL FR ] }}")
        house_id = str(wait_for(lambda: node_id(HOUSE)))
        phones_id = str(wait_for(lambda: node_id(PHONES)))
        wait_for(lambda: run("wpctl", "inspect", house_id))
        run("wpctl", "set-default", house_id)
        wait_for(lambda: HOUSE in run("wpctl", "inspect", "@DEFAULT_AUDIO_SINK@"))
        for sink in (house_id, phones_id):
            run("wpctl", "set-volume", sink, "1")
            run("wpctl", "set-mute", sink, "0")
        house = Monitor(HOUSE)
        cleanup.callback(house.close)
        phones = Monitor(PHONES)
        cleanup.callback(phones.close)
        phones.wait(RATE // 4)
        house.wait(RATE // 4)
        begin_house = house.position()
        def capture(signal):
            begin = phones.position()
            assert tone(signal).wait(timeout=10) == 0
            phones.wait(phones.position() + RATE // 5)
            pcm = phones.read(begin, phones.position())
            audible = [i for i, n in enumerate(pcm) if abs(n) > 200]
            assert audible, f"no {signal} audition signal reached headphones"
            seconds = (audible[-1] - audible[0]) / RATE
            assert 2.85 < seconds < 3.15, f"44.1 kHz {signal} clip lasted {seconds}s on 48 kHz graph"
            return pcm, seconds

        dry, seconds = capture("source")
        wet, _ = capture("effects")
        low_ratio = tone_amplitude(wet, 440) / tone_amplitude(dry, 440)
        high_ratio = tone_amplitude(wet, 3000) / tone_amplitude(dry, 3000)
        assert low_ratio < .12, f"wet audition retained {low_ratio:.3f} of the low test tone"
        assert .90 < high_ratio < 1.10, f"wet audition retained {high_ratio:.3f} of the high test tone"

        # Remove headphones while a second audition is still producing sound.
        before = phones.position()
        playback = tone("effects")
        wait_for(lambda: max(map(abs, phones.read(before, phones.position())), default=0) > 200)
        run("pw-cli", "destroy", phones_id)
        assert playback.wait(timeout=10) == 0

        # A missing named target must also be silent from the outset.
        assert tone("effects").wait(timeout=10) == 0
        house.wait(house.position() + RATE // 4)
        peak = max(map(abs, house.read(begin_house, house.position())))
        assert peak <= 2, f"audition reached the live default sink, peak {peak}"
        print(f"headphone clip {seconds:.3f} s, wet/dry {low_ratio:.3f} at 440 Hz and {high_ratio:.3f} at 3 kHz, "
              f"live sink peak {peak} through playback, unplug and missing target")


if __name__ == "__main__":
    binary = str(Path(sys.argv[1]).resolve())
    if len(sys.argv) == 3 and sys.argv[2] == "--private-session":
        graph_check(binary)
    else:
        with tempfile.TemporaryDirectory() as root:
            env = os.environ.copy()
            for key in ("XDG_RUNTIME_DIR", "PIPEWIRE_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"):
                env[key] = root
            env.pop("PIPEWIRE_REMOTE", None)
            env["INKUE_AUDITION_GRAPH_TEST"] = "1"
            subprocess.run(["dbus-run-session", "--", sys.executable, __file__, binary, "--private-session"], env=env, check=True)
