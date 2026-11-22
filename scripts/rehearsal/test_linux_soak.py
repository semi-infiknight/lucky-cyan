import contextlib
import io
import unittest
from unittest.mock import patch

import linux_soak as soak


def node(id, name, media_class, serial=57, **props):
    return {"id": id, "type": "PipeWire:Interface:Node", "info": {"props": {
        "node.name": name, "media.class": media_class, "object.serial": serial, **props,
    }}}


def link(source, recorder, state="active"):
    return {"type": "PipeWire:Interface:Link", "info": {
        "output-node-id": source, "input-node-id": recorder, "state": state,
    }}


class LoopbackTests(unittest.TestCase):
    def test_exact_name_requires_one_source_and_a_serial(self):
        source = node(10, "line.in", "Audio/Source")
        recorder = node(11, "readout", "Stream/Input/Audio")
        self.assertEqual(soak.CaptureInput.resolve("line.in", [source, recorder]),
                         soak.CaptureInput("line.in", 10, 57))
        for name, graph in (("line", [source]), ("readout", [recorder]),
                            ("line.in", [source, source]),
                            ("line.in", [node(10, "line.in", "Audio/Source", serial=None)])):
            with self.subTest(name=name, graph=graph), self.assertRaises(RuntimeError):
                soak.CaptureInput.resolve(name, graph)

    def test_recording_route_requires_original_serial_and_only_the_named_source(self):
        source = node(10, "line.in", "Audio/Source")
        recorder = node(11, "readout", "Stream/Input/Audio")
        monitor = soak.Monitor.__new__(soak.Monitor)
        monitor.source = soak.CaptureInput("line.in", 10, 57)
        monitor.node_name = "readout"
        with patch.object(soak, "pipewire_graph", return_value=[source, recorder, link(10, 11)]):
            monitor.verify_input()
        failed = (
            [source, recorder, link(10, 11, "paused")],
            [source, recorder, link(12, 11)],
            [source, recorder, link(10, 11), link(12, 11)],
            [node(10, "line.in", "Audio/Source", serial=99), recorder, link(10, 11)],
            [recorder, link(12, 11)],
        )
        for graph in failed:
            with self.subTest(graph=graph), patch.object(soak, "pipewire_graph", return_value=graph):
                with self.assertRaises(RuntimeError):
                    monitor.verify_input()

    def test_calibration_preserves_relative_fade_and_clock_limits(self):
        self.assertTrue(soak.calibrated_fade(750, 400, 50, 0, 2.0, 2.0))
        self.assertFalse(soak.calibrated_fade(750, 400, 50, 2, 2.0, 2.0))
        calibration = soak.Calibration.loopback(1500, 4)
        self.assertEqual(calibration, soak.Calibration(2.0, 8.0))
        self.assertTrue(soak.calibrated_fade(1500, 800, 100, 7, 2.0, 2.0, calibration))
        for levels in ((3000, 800, 100, 7, 2.0, 2.0), (1500, 800, 100, 8, 2.0, 2.0),
                       (1500, 800, 100, 7, 2.0, 2.25), (1500, 800, 400, 7, 2.0, 2.0)):
            self.assertFalse(soak.calibrated_fade(*levels, calibration))
        for reference, noise in ((None, 2), (1200, None), (1200, 6), (5000, 1), (float("nan"), 0)):
            with self.assertRaises(ValueError):
                soak.Calibration.loopback(reference, noise)

    def test_capture_mode_cannot_enter_device_loss_or_fall_back_for_a_blank_name(self):
        cases = (["--capture-input", "line.in", "--device-loss-log", "ignored", "--osc-port", "53001"],
                 ["--capture-input", ""], ["--capture-input", "line.in", "--measure-input", "--osc-port", "53001"])
        for args in cases:
            with self.subTest(args=args), patch.object(soak.sys, "argv", ["linux_soak.py", *args]):
                with patch.object(soak, "pipewire_graph") as graph, contextlib.redirect_stderr(io.StringIO()):
                    with self.assertRaises(SystemExit) as caught:
                        soak.main()
                    self.assertEqual(caught.exception.code, 2)
                    graph.assert_not_called()

    def test_device_loss_is_bound_to_the_private_rehearsal_graph_and_virtual_sink(self):
        env = {"XDG_RUNTIME_DIR": "/tmp/rehearsal", "INKUE_REHEARSAL_PRIVATE_RUNTIME": "/tmp/rehearsal"}
        with patch.dict(soak.os.environ, {**env, "PIPEWIRE_REMOTE": "outside"}, clear=True):
            with patch.object(soak, "pipewire_graph") as graph, self.assertRaises(RuntimeError):
                soak.private_rehearsal_sink()
            graph.assert_not_called()
        valid = node(31, "rehearsal", "Audio/Sink", **{"factory.name": "support.null-audio-sink"})
        physical = node(30, "rehearsal", "Audio/Sink", **{"factory.name": "api.alsa.pcm.sink"})
        with patch.dict(soak.os.environ, env, clear=True):
            with patch.object(soak, "pipewire_graph", return_value=[valid]):
                self.assertEqual(soak.private_rehearsal_sink(), "31")
            with patch.object(soak, "pipewire_graph", return_value=[physical]), self.assertRaises(RuntimeError):
                soak.private_rehearsal_sink()
            with patch.object(soak, "pipewire_graph", return_value=[valid, physical]), self.assertRaises(RuntimeError):
                soak.private_rehearsal_sink()


if __name__ == "__main__":
    unittest.main()
