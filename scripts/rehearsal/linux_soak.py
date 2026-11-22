"""Exercise the bundled practice show with a PipeWire sink monitor or named input.

Open the Operator rehearsal show on this dedicated display before starting.
Use the sink receiving the show's output or a calibrated line loopback. Each
line of stdout is one cycle's measurements. A missing marker or bad capture fails the run.
"""
import argparse
import array
from collections import deque
from dataclasses import dataclass
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import threading
import time
import uuid
import wave

RATE = 48000
MARKER = b"/rehearsal/complete\0,\0\0\0"


class CaptureFailure(RuntimeError):
    def __init__(self, message, **measurements):
        super().__init__(message)
        self.measurements = measurements


def pipewire_graph():
    return json.loads(subprocess.check_output(["pw-dump"], timeout=5))


@dataclass(frozen=True)
class CaptureInput:
    name: str
    id: int
    serial: int

    @classmethod
    def resolve(cls, name, graph):
        matches = []
        for node in graph:
            props = node.get("info", {}).get("props", {})
            if node.get("type") == "PipeWire:Interface:Node" and props.get("node.name") == name:
                matches.append((node, props))
        if len(matches) != 1 or matches[0][1].get("media.class") != "Audio/Source":
            raise RuntimeError(f"Capture input {name!r} must identify one Audio/Source node by exact name")
        node, props = matches[0]
        serial = props.get("object.serial")
        if type(serial) is not int or serial <= 0:
            raise RuntimeError(f"Capture input {name!r} has no usable PipeWire serial")
        return cls(name, node["id"], serial)


@dataclass(frozen=True)
class Calibration:
    scale: float = 1.0
    quiet_limit: float = 2.0

    @classmethod
    def loopback(cls, reference, noise):
        if reference is None or not 300 <= reference <= 4000:
            raise ValueError("--reference-rms must be between 300 and 4000")
        if noise is None or not 0 <= noise <= reference / 250:
            raise ValueError("--noise-rms must be non-negative and at most the reference RMS divided by 250")
        return cls(reference / 750, max(2.0, noise * 2))


VIRTUAL = Calibration()


def preserve_failure(evidence, monitor, cycle, begin, error):
    end = monitor.position()
    start = max(0, min(begin, end) - RATE // 4, end - 6 * RATE)
    result = {
        "time_utc": datetime.now(timezone.utc).isoformat(),
        "cycle": cycle,
        "error": str(error),
        "cycle_started_at_capture_frame": begin,
        "capture_start_frame": start,
        "capture_end_frame": end,
        **monitor.input_evidence(),
        **getattr(error, "measurements", {}),
    }
    if end > start:
        try:
            samples = monitor.read(start, end)
            if sys.byteorder != "little":
                samples.byteswap()
            with wave.open(str(evidence / "failed-cycle.wav"), "wb") as recording:
                recording.setparams((1, 2, RATE, 0, "NONE", "not compressed"))
                recording.writeframes(samples.tobytes())
        except (OSError, RuntimeError) as capture_error:
            result["wav_error"] = str(capture_error)
    (evidence / "capture-failure.json").write_text(json.dumps(result, indent=2) + "\n")


def keys(*args):
    subprocess.run(["xdotool", *args], check=True, timeout=5)


def rms(samples):
    return math.sqrt(sum(x*x for x in samples) / len(samples)) if samples else 0


def calibrated_fade(tone, middle, late, tail, captured_seconds, elapsed_seconds, calibration=VIRTUAL):
    scale = calibration.scale
    return (680 * scale <= tone <= 820 * scale and 320 * scale <= middle <= 550 * scale
            and 10 * scale <= late <= 160 * scale and tail < calibration.quiet_limit
            and abs(captured_seconds - elapsed_seconds) < .24)


class Monitor:
    """Keep the PipeWire capture connected and retain only recent PCM."""
    def __init__(self, sink, source=None):
        self.changed = threading.Condition()
        self.blocks = deque()
        self.samples = 0
        self.ended = False
        self.source = source
        if source:
            self.node_name = "inkue.rehearsal.capture." + uuid.uuid4().hex
            target = str(source.serial)
            properties = json.dumps({"node.name": self.node_name, "node.dont-fallback": True,
                                     "node.dont-reconnect": True})
        else:
            target, properties = sink, "{ stream.capture.sink=true }"
        self.process = subprocess.Popen([
            "pw-record", "--target", target, "-P", properties,
            "--rate", str(RATE), "--channels", "1", "--format", "s16", "--raw", "-"
        ], stdout=subprocess.PIPE)
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def input_evidence(self):
        return {"capture_input": self.source.name, "capture_input_serial": self.source.serial} if self.source else {}

    def verify_input(self):
        if not self.source:
            return
        graph = pipewire_graph()
        current = CaptureInput.resolve(self.source.name, graph)
        recorders = {
            node["id"] for node in graph if node.get("type") == "PipeWire:Interface:Node"
            and node.get("info", {}).get("props", {}).get("node.name") == self.node_name
        }
        links = [
            node["info"] for node in graph
            if node.get("type") == "PipeWire:Interface:Link"
            and node.get("info", {}).get("input-node-id") in recorders
        ]
        inputs = {link.get("output-node-id") for link in links}
        if (current != self.source or len(recorders) != 1 or inputs != {self.source.id}
                or not any(link.get("state") == "active" for link in links)):
            raise CaptureFailure("Recorder is not connected exclusively to the original capture input",
                                 phase="capture_route")

    def _read(self):
        carry = b""
        try:
            while data := self.process.stdout.read1(4096):
                data = carry + data
                length = len(data) & ~1
                carry = data[length:]
                if not length:
                    continue
                with self.changed:
                    self.blocks.append((self.samples, data[:length]))
                    self.samples += length // 2
                    while self.blocks[0][0] < self.samples - RATE * 15:
                        self.blocks.popleft()
                    self.changed.notify_all()
        finally:
            with self.changed:
                self.ended = True
                self.changed.notify_all()

    def position(self):
        with self.changed:
            return self.samples

    def wait(self, target):
        with self.changed:
            if not self.changed.wait_for(lambda: self.samples >= target or self.ended, timeout=3) or self.samples < target:
                raise RuntimeError(f"PipeWire capture stalled at {self.samples} of {target} samples")

    def read(self, start, end):
        pcm = array.array("h")
        with self.changed:
            if not self.blocks or start < self.blocks[0][0] or end > self.samples:
                raise RuntimeError("Required audio is outside the capture window")
            for offset, data in self.blocks:
                left = max(0, (start - offset) * 2)
                right = min(len(data), (end - offset) * 2)
                if left < right:
                    pcm.frombytes(data[left:right])
        return pcm

    def close(self):
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGINT)
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.reader.join()
        self.process.stdout.close()


def remote_go(sock, port, address):
    raw = address.encode("ascii") + b"\0"
    packet = raw + bytes(-len(raw) % 4) + b",\0\0\0"
    sock.sendto(packet, ("127.0.0.1", port))


def wait_for_log(path, offset, text, timeout):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        with path.open("rb") as log:
            log.seek(offset)
            if text.encode() in log.read():
                return
        time.sleep(.1)
    raise RuntimeError(f"Desktop did not report {text!r} within {timeout}s")


def private_rehearsal_sink():
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if (not runtime or runtime != os.environ.get("INKUE_REHEARSAL_PRIVATE_RUNTIME")
            or "PIPEWIRE_REMOTE" in os.environ or "PIPEWIRE_RUNTIME_DIR" in os.environ):
        raise RuntimeError("Device loss requires the private graph created by check-installed-linux.sh")
    matches = [node for node in pipewire_graph() if node.get("type") == "PipeWire:Interface:Node"
               and node.get("info", {}).get("props", {}).get("node.name") == "rehearsal"]
    if len(matches) == 1:
        props = matches[0]["info"]["props"]
        if props.get("media.class") == "Audio/Sink" and props.get("factory.name") == "support.null-audio-sink":
            return str(matches[0]["id"])
    raise RuntimeError("Device loss check requires exactly one dedicated rehearsal null sink")


def device_loss_check(sock, port, log, evidence):
    """Remove only the named virtual rehearsal sink in our private graph."""
    sink = private_rehearsal_sink()
    monitor = Monitor("rehearsal")
    try:
        monitor.wait(RATE // 4)
        at = monitor.position()
        idle = rms(monitor.read(0, at))
        if idle >= 2:
            raise RuntimeError(f"Workspace opening produced audio before GO, RMS {idle:.1f}")
        remote_go(sock, port, "/inkue/cue/1/go")
        monitor.wait(at + RATE)
        initial = rms(monitor.read(at + RATE // 2, at + RATE))
        if not 680 <= initial <= 820:
            raise RuntimeError(f"Expected one running voice before loss, measured {initial:.1f}")
    finally:
        monitor.close()
    log_offset = log.stat().st_size
    subprocess.run(["pw-cli", "destroy", sink], check=True)
    wait_for_log(log, log_offset, "DeviceNotAvailable", 12)
    time.sleep(2.5)  # Let the watchdog attempt an open on the absent device.
    remote_go(sock, port, "/inkue/hardstop")
    time.sleep(.2)
    remote_go(sock, port, "/inkue/cue/1/go")
    wait_for_log(log, log_offset, "Audio start held", 5)
    # The practice show uses the default 500 ms double-GO protection.
    time.sleep(.65)
    remote_go(sock, port, "/inkue/cue/3/go")
    if sock.recvfrom(256)[0] != MARKER:
        raise RuntimeError("Unrelated OSC cue failed while the audio device was missing")
    subprocess.run(["pw-cli", "create-node", "adapter",
                    "{ factory.name=support.null-audio-sink node.name=rehearsal media.class=Audio/Sink object.linger=true audio.position=[ FL FR ] }"], check=True)
    wait_for_log(log, log_offset, "Audio output device opened on retry", 30)
    subprocess.run(["wpctl", "set-volume", private_rehearsal_sink(), "1"], check=True)
    monitor = Monitor("rehearsal")
    try:
        monitor.wait(RATE)
        after = rms(monitor.read(RATE // 4, RATE))
        if after >= 2:
            raise RuntimeError(f"Sound returned without a new GO after reconnection, RMS {after:.1f}")
        fresh = measured_cycle(sock, None, monitor, port, evidence, 1)
    finally:
        monitor.close()
    return {"before_first_go_rms": round(idle, 1), "playing_before_loss_rms": round(initial, 1), "non_audio_cue_received": True,
            "reconnected_before_go_rms": round(after, 1), "fresh_go": fresh}


def cycle(sock, window, monitor, remote_port, calibration):
    def go(address):
        if remote_port:
            remote_go(sock, remote_port, address)
        else:
            keys("key", "space")

    monitor.verify_input()
    if window:
        keys("windowfocus", "--sync", window)
        keys("key", "g", "sleep", "0.15", "type", "1")
        keys("key", "Return", "sleep", "0.2")
    first_go = monitor.position()
    first_go_at = time.monotonic()
    go("/inkue/cue/1/go")
    time.sleep(1)
    monitor.wait(first_go + RATE)
    fade = monitor.position()
    tone = monitor.read(first_go, fade)
    sustained = rms(tone[-RATE // 4:])
    scale = calibration.scale
    if not 680 * scale <= sustained <= 820 * scale:
        raise CaptureFailure(f"First GO sustained RMS {sustained:.1f}, expected {680 * scale:.1f}–{820 * scale:.1f} ({time.monotonic() - first_go_at:.3f}s)",
                             phase="tone_level", steady_rms=round(sustained, 1))
    onset = next(i for i, value in enumerate(tone) if abs(value) >= 100 * scale)
    # The fixture is a steady tone. Look for short gaps that a large RMS
    # envelope can miss. Stay clear of the initial fade-in and the second GO.
    block = RATE // 100
    lower = onset + RATE // 5
    upper = len(tone) - RATE // 10
    levels = [rms(tone[i:i + block]) for i in range(lower, upper - block + 1, block)]
    if not levels or min(levels) < 500 * scale:
        gaps = [{"capture_frame": first_go + lower + i * block, "rms": round(value, 1)}
                for i, value in enumerate(levels) if value < 500 * scale]
        raise CaptureFailure(f"Sustained tone discontinuity, minimum 10ms RMS {min(levels, default=0):.1f}",
                             phase="tone_continuity", onset_at_capture_frame=first_go + onset,
                             frames_per_block=block, gap_blocks=gaps)

    started = time.monotonic()
    go("/inkue/go")
    message, _ = sock.recvfrom(256)
    delay = time.monotonic() - started
    if message != MARKER or not 1.8 <= delay <= 2.5:
        raise RuntimeError(f"Unexpected OSC marker or follow time ({message!r}, {delay:.3f}s)")
    marker_sample = monitor.position()
    end = marker_sample + RATE // 2
    monitor.wait(end)

    def level(start, stop):
        return rms(monitor.read(fade + int(start * RATE), fade + int(stop * RATE)))

    before, middle, late = level(-.45, -.15), level(.8, 1.0), level(1.55, 1.75)
    tail = rms(monitor.read(end - RATE // 5, end))
    captured_fade = (marker_sample - fade) / RATE
    result = {"follow_s": round(delay, 3), "captured_fade_s": round(captured_fade, 3),
              "capture_onset_ms": round(onset * 1000 / RATE, 1),
              "minimum_tone_block_rms": round(min(levels), 1),
              "tone_rms": round(before, 1), "mid_fade_rms": round(middle, 1),
              "late_fade_rms": round(late, 1), "tail_rms": round(tail, 1)}
    if not calibrated_fade(before, middle, late, tail, captured_fade, delay, calibration):
        raise RuntimeError(f"Unexpected recorded fade or capture clock: {result}")
    monitor.verify_input()
    return result


def measured_cycle(sock, window, monitor, port, evidence, count, calibration=VIRTUAL):
    begin = monitor.position()
    try:
        return cycle(sock, window, monitor, port, calibration)
    except (RuntimeError, OSError) as error:
        if evidence:
            try:
                preserve_failure(evidence, monitor, count, begin, error)
            except OSError as evidence_error:
                error.add_note(f"Could not save the failure evidence: {evidence_error}")
        raise


def measure_input(source):
    monitor = Monitor(None, source)
    try:
        monitor.wait(RATE * 5 // 4)
        monitor.verify_input()
        end = monitor.position()
        samples = monitor.read(end - RATE, end)
        print(json.dumps({**monitor.input_evidence(), "rms": round(rms(samples), 3),
                          "peak": max(map(abs, samples))}))
    finally:
        monitor.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seconds", type=int, default=0, help="total wall time; 0 runs one cycle")
    capture = parser.add_mutually_exclusive_group()
    capture.add_argument("--sink", default="rehearsal", help="PipeWire sink monitor (default: rehearsal)")
    capture.add_argument("--capture-input", help="exact PipeWire Audio/Source node.name for a line loopback")
    parser.add_argument("--measure-input", action="store_true", help="measure one second from --capture-input without triggering cues")
    parser.add_argument("--reference-rms", type=float, help="measured steady tone RMS on the prepared loopback")
    parser.add_argument("--noise-rms", type=float, help="measured quiet loopback RMS before running the show")
    parser.add_argument("--osc-port", type=int,
                        help="drive the dedicated rehearsal desk through its trusted OSC remote on localhost")
    parser.add_argument("--device-loss-log", type=Path, help="exercise a private rehearsal null sink and record handover, then exit")
    parser.add_argument("--failure-evidence", type=Path,
                        help="write PCM and measurements for a failed cycle to this existing directory")
    args = parser.parse_args()
    if args.capture_input is not None and not args.capture_input.strip():
        parser.error("--capture-input requires a nonempty exact node name")
    if args.device_loss_log and (args.capture_input or not args.osc_port or args.sink != "rehearsal" or args.measure_input):
        parser.error("device loss requires trusted OSC and the private rehearsal sink")
    if args.measure_input and (not args.capture_input or args.seconds or args.osc_port):
        parser.error("--measure-input requires --capture-input and runs without OSC or --seconds")
    if not args.capture_input and (args.reference_rms is not None or args.noise_rms is not None):
        parser.error("calibration values require --capture-input")
    calibration = VIRTUAL
    source = None
    if args.capture_input:
        if not args.measure_input:
            try:
                calibration = Calibration.loopback(args.reference_rms, args.noise_rms)
            except ValueError as error:
                parser.error(str(error))
        source = CaptureInput.resolve(args.capture_input, pipewire_graph())
        if args.measure_input:
            measure_input(source)
            return
    window = None
    if args.osc_port is None:
        windows = subprocess.check_output(["xdotool", "search", "--onlyvisible", "--name", "^Inkue$"], text=True).split()
        if len(windows) != 1:
            parser.error("the rehearsal display must have exactly one Inkue main window")
        window = windows[0]
    elif not 1 <= args.osc_port <= 65535:
        parser.error("OSC port must be between 1 and 65535")
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(("127.0.0.1", 15391))
        sock.settimeout(5)
        if args.device_loss_log:
            print(json.dumps(device_loss_check(sock, args.osc_port, args.device_loss_log, args.failure_evidence)), flush=True)
            return
        start, count = time.monotonic(), 0
        monitor = Monitor(args.sink, source)
        try:
            monitor.wait(RATE // 4)
            if source:
                monitor.verify_input()
                end = monitor.position()
                quiet = rms(monitor.read(end - RATE // 4, end))
                if quiet >= calibration.quiet_limit:
                    raise RuntimeError(f"Stop all cues and recheck the loopback before GO. Idle RMS was {quiet:.1f}.")
            while count == 0 or time.monotonic() - start < args.seconds:
                result = measured_cycle(sock, window, monitor, args.osc_port, args.failure_evidence, count + 1, calibration)
                count += 1
                input_details = {**monitor.input_evidence(), "reference_rms": args.reference_rms,
                                 "noise_rms": args.noise_rms} if source else {}
                print(json.dumps({"cycle": count, "elapsed_s": round(time.monotonic()-start, 1),
                                  **input_details, **result}), flush=True)
        finally:
            monitor.close()


if __name__ == "__main__":
    main()
