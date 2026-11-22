# Linux qualification record

Tested on Debian 13 amd64, using X11, WebKitGTK, libmpv and a native 48 kHz PipeWire stereo sink monitor in an isolated container. The installed desk ran without a physical audio, MIDI or lighting interface.

| Check | Observation |
| --- | --- |
| Build and automated suites | TypeScript and Vite production build passed. 98 frontend tests, 672 native Rust tests, two shared cue-sheet tests and two CLI/MCP integration tests passed. Three native tests declare external fixture or hardware prerequisites. |
| Actual QLab files | One QLab 4 workspace and two QLab 5 workspaces passed native import and reload checks. The check compares emitted and loaded cue counts. Original media playback and patch equivalence still require comparison on the operator's rig. |
| Operator rehearsal | The practice show fired the tone, applied the two-second fade and sent the follow's OSC packet. The initial native PipeWire pass measured 2.041 s from the second GO to the marker, with a silent recorded tail. The cart loaded as a separate list. |
| Live agent approval | A dry run returned cue 1. A disarmed request was rejected. With local arming, a duplicate pending request was rejected. Confirming the original GO advanced the playhead once to cue 2. A further request was rejected until rearming. |
| Crash recovery | After a forced process exit, the installed desktop offered the saved recovery snapshot. The operator restored the practice show and its cue 2 playhead. The saved recovery document included the eight agent audit entries. Playback remained idle on recovery. |
| Performance under competing load | With native compilation running, the ALSA/Pulse fallback logged xruns. A native PipeWire repeated run passed ten cycles and failed its next capture check. A subsequent run on an optimized preview measured a 2.109 s follow and silence after the fade but captured only 1.664 s during the follow interval. This configuration does not pass the timing gate under that load. |

## Installed Linux rehearsal

The later Debian 13 package at source revision `d0a4425` upgraded a real Inkue 1.4.1 installation and opened the practice show through the file manager association. Its 17 measured cycles took 60.4 seconds. OSC follows arrived after 2.003–2.033 seconds, measured tone RMS stayed at 738.0–751.5 and each recorded tail was silent. Device removal held a fresh audio GO. The unrelated OSC cue still fired, the restored output stayed silent until another GO, and that GO reached the follow after 2.028 seconds. The log contained no xrun reports. Tests on this package covered the virtual host. The follow-on candidate adds live-edit and failed-start regressions that require their own installed run.

The optimized baseline installer `inkue-operator-e6c2bf1-amd64.deb` has SHA-256 `86030f884c3d36d71a2a1e30632773fb6f8d508868f3103bff707f7d13c497dd`. On an otherwise idle virtual sink its installed binary completed 17 cycles in 60.8 seconds. The OSC follow arrived in 2.015–2.044 seconds and every recorded tail was silent. Removing and restoring the sink exposed a separate fault: a GO issued during the outage could start late and overlap the following GO. The updated desk checks the output heartbeat before accepting a start and expires a start that misses its first render.

To check an installed Debian candidate, use a fresh evidence directory. The host needs `dbus-run-session`, Xvfb, PipeWire, WirePlumber, their CLI tools and Python 3. The bundled show sends its OSC follow to localhost UDP 15391, which must be available.

```sh
scripts/rehearsal/check-installed-linux.sh /tmp/inkue-release-check 60
scripts/rehearsal/check-installed-linux.sh /tmp/inkue-overnight-check 28800
```

The check starts the desktop with its bundled workspace on an isolated D-Bus session, X11 display and PipeWire graph. It starts a tone, disconnects the virtual output, sends hard stop and a fresh audio GO, and fires an independent OSC cue during the outage. After recreating the output, it measures silence, sends one new GO and checks the tone and fade. Repeated cycles then run for the requested wall time. Review `installation.txt`, `device-loss.json`, `cycles.jsonl` and `completed.txt` in the evidence directory. A missing completion marker means the full run did not pass.

The test receiver is a 48 kHz stereo PipeWire null sink at unity gain, unmuted. The bundled workspace uses its saved master, cue and patch gains. Mono signed 16-bit capture should measure RMS 680–820 at full level, 320–550 at 0.8–1.0 seconds into the fade, 10–160 at 1.55–1.75 seconds, and below 2 after the fade. During the steady tone, 10 ms blocks must also exceed 500 RMS after the startup window. This flags short interruptions that can be masked by the sustained average. The OSC marker must arrive 1.8–2.5 seconds after the second GO, and its PCM capture interval must agree with elapsed wall time within 240 ms. These bounds detect missing audio, duplicate playback, incorrect gain, incomplete fades and capture clock loss. The virtual monitor does not measure physical audio latency.

### Physical line loopback

The named-input mode checks the input's exact PipeWire identity and active recorder route. It scales the tone, fade and 10 ms continuity bounds by the measured steady RMS divided by 750. The quiet tail must stay below twice the measured idle RMS, with a minimum gate of 2 RMS. The same OSC follow and capture-time bounds apply. Capture onset includes the device, loopback and recorder pipeline. Use the [line-loopback setup](../README.md#measure-a-physical-line-loopback) to qualify a specific interface and cable route without changing the desk's output settings. The automated device-loss sequence uses the separately launched private rehearsal null sink.

A synthetic PipeWire source carried a 440 Hz reference with observed RMS 848.532 and peak 1200, and measured 0 RMS while quiet. This checked named-input recording and calibration readout. Physical interface, eight-hour endurance and venue disconnect/recovery results require a run on the intended rig.

## Follow-on installed candidate

The installer at source revision `ef6a63f` passed 727 native unit and integration tests. Its retried installed short run on the default virtual graph passed all 17 cycles, the file association and the output-loss check. The first attempt captured a silent 10 ms block after reconnection. The first endurance pass then recorded a dropout in its tenth tone. Keep all of these results with the installer. A second endurance attempt used a private PipeWire graph at 48 kHz and a 1024-sample quantum. It stopped at cycle 6 after recording a completely silent 10 ms block. To isolate the host, a separate control drove the same kind of private graph from `pw-play` and measured it using `pw-record`. No Inkue process ran. The control recorded 10 low blocks out of 6,200, including complete silence. PipeWire's graph counters also reported errors on the sink, player and recorder. Both endurance attempts remain failures. Repeat the control and the desk run on the intended Linux rehearsal host with the venue audio interface.

```sh
python3 scripts/rehearsal/check-pipewire-control.py /tmp/inkue-pipewire-control --quantum 1024
INKUE_REHEARSAL_QUANTUM=1024 scripts/rehearsal/check-installed-linux.sh /tmp/inkue-overnight-1024 28800
```

The installed virtual-stage test used an explicitly manual fade fixture and an Xvfb display with MIT-SHM disabled. An 81-point capture confirmed black at zero opacity, red at 75% and full opacity, and black after hard stop. A separate direct full-opacity GO stayed black on one startup with this installer. The final show requires a physical projector check.

## Installed candidate and virtual host diagnosis

Source revision `6b08dc4` passed 761 native and 141 frontend tests. Its installed package passed all 18 virtual stage cases. Its installed audio device-loss check confirmed an independent OSC cue during the outage and silence after reconnection until a new GO. That GO reached the fade follow after 2.020 seconds. Both installed repeat attempts stopped at cycle 3 on a silent 10 ms block.

A quiet repeat of the standalone 48 kHz, 1024-sample PipeWire control recorded 27 low blocks out of 6,200. The first silent block occurred in the 13–14 second capture interval after a steady tone, and other interruptions continued through the 55–56 second interval. The virtual sink, recorder and player reported 18, 24 and 28 graph errors respectively. CPU quota counters showed no new throttling during this control. The available CPUs were shared and both FIFO and round-robin realtime scheduling probes were denied. The virtual test path can therefore produce dropouts with PipeWire's own player and recorder as the only audio clients.

With the control pinned to CPU 4 and the graph quantum set to 2048, all 6,200 capture blocks stayed above threshold, with a minimum RMS of 769.9. The observed startup took 128 ms. The last graph snapshot still showed four sink errors and one recorder error. The PipeWire server, WirePlumber, player and recorder all used ordinary scheduling. Both settings changed together, so their individual effects remain unknown.

At those same settings, the first installed run passed ten cycles before a fade envelope failed while the shared CPU quota was throttling. A repeat with builds paused passed device recovery and all 17 cycles in 60.1 seconds. OSC follows arrived in 2.006–2.042 seconds, the lowest 10 ms tone block was 735.6 RMS and every tail was silent. The virtual host exposed no physical sound device. The overnight and venue checks remain open.

The control writes `host.json` with CPU affinity and cgroup limits, `startup.json` with observed tone onset and process scheduling classes, and `blocks.jsonl` with the capture offsets and RMS of any low 10 ms blocks after the first second. `graph.log` contains the PipeWire counters and `summary.json` records the strict capture result. Review the PCM and graph evidence together when comparing runs. CPU affinity can be set for a private test with `taskset` and shares those CPUs with the rest of the host unless the host has assigned them exclusively.

## Rehearsal candidate `4dfa260`

The installed Debian package passed 18 cycles in 63.3 seconds and 20 virtual-stage checks. Its virtual monitor recorded the first stereo transient on the same sample in both channels, confirmed each subsequent click, and measured the expected playback-rate change. A remote GO aimed at a disarmed cue left the monitor silent. The first eight-hour attempt passed device recovery and completed four cycles. The fifth steady tone contained a silent 10 ms block, so that attempt failed.

The next standalone control used the same 48 kHz graph, 2048-sample quantum and CPU affinity as the failed endurance run. With Inkue closed it recorded 34 low 10 ms blocks out of 9,000, including complete silence. The cgroup recorded zero throttled periods in that run. An installed test raised Inkue’s request from 256 to 2048 samples and checked in the app log that both opens used the new size. It passed device recovery and 14 cycles. The recording of cycle 15 contained 4,096 consecutive zero samples during the steady tone. PipeWire graph errors were recorded on the sink, Inkue and the capture client. These results keep the virtual endurance gate open under both request sizes.

The runner now saves `capture-failure.json` and up to six seconds of mono PCM in `failed-cycle.wav` on a failed repeat. For a continuity failure the JSON identifies the low blocks by capture frame. `INKUE_REHEARSAL_TRACE=1` saves a `pw-top` transcript for the private graph. `INKUE_REHEARSAL_APP_BUFFER` sets the application buffer request only in the isolated run. The application buffer request and `INKUE_REHEARSAL_QUANTUM` are separate settings. Keep them with the evidence and retain the same pass thresholds when comparing settings.

## Installed rehearsal candidate `e82ae43`

The clean Debian package passed 844 native tests, 146 frontend tests and seven import checks across six published QLab archives. On the installed desktop, all 20 stage checks, four straight position Fade checks and five waypoint checks passed. Binary OSC and plain UDP started an armed cue while standby stayed parked. The CMY rehearsal captured the complete snap and 30 points during the timed look on loopback Art-Net.

On a named virtual stereo output, an Audio Map Fade moved a source from left through centre to right. Hard Stop left both channels silent and a fresh GO returned the source to its saved left mark. Sweeping a built-in high-pass filter from 600 Hz to 80 Hz raised the measured 180 Hz tone from −20.91 dB to −0.17 dB relative to the 1,800 Hz tone. A fresh GO restored −20.91 dB.

The first audio check used a 256-frame application request on a 2048-frame graph. The desk held a GO while the sink was gone. The first fresh GO after reconnection contained a silent 10 ms block and failed the check. On the same binary, with both requests set to 2048 frames, device recovery and all 18 repeats passed in 63.5 seconds. OSC follows took 2.008–2.040 seconds, the lowest steady 10 ms block measured 735.6 RMS, and every tail was silent. Both runs remain part of the evidence. The overnight and physical venue checks remain open.

## Installed rehearsal candidate `63e1613`

The Debian 13 package passed 1,069 native tests, 189 frontend tests, 13 archived QLab checks and 10 standalone inspector tests. The installed desk passed 18 short virtual audio cycles and one device disconnect and recovery. Loopback OSC confirmed the exact next standby after a final-Wait handoff and after stepping a running Wait Playlist in both directions.

The silent two-second video loop played through its final blue frame and cleared the stage at natural EOF after Whole Cue Devamp. A first attempt showed the expected stage colors and failed one timing bound while polling full-window XWD captures. The repeat sampled one stage pixel during timing and preserved full-window images at the observed transitions. On the same installed binary, the blue tail appeared after 996 ms and black followed 984 ms later. A fresh GO restored the loop. The kit carries the matching source revision and installed checksum in `Evidence/release-verification.json`.

## Installed candidate `3684d80`

The Debian package installed and recovered after the virtual output disappeared. With both the graph and application set to 2,048 frames, the recovered output stayed silent until a fresh GO. Thirteen repeat cycles passed. The fourteenth captured three consecutive silent 10 ms blocks during a steady tone and failed the check. Earlier standalone PipeWire controls also recorded silence with Inkue closed on this host. The dropout’s cause remains undetermined, and `63e1613` is the latest rehearsal kit that passed the short run.

## Qualification still required

The Linux launch gate includes an uninterrupted overnight run on the release binary and a rehearsal on the intended audio interface with a representative collected show. The latter checks monitoring by ear, channel routing, device loss and recovery, hard stop, and manual takeover on the venue's backup machine. Distribution signing and operator sign-off also remain open. Windows and macOS rehearsals follow Linux qualification. The technical proposal to FonograF is a private draft.

The full roadmap also calls for broader routing and cue parity, NDI, object-based audio, coordinated backup machines and collaborative editing. Existing video, lighting and timecode capabilities have automated coverage. A virtual audio rehearsal does not certify those physical output paths.

## Sources for migration checks

- QLab 4 fade workspace from [qlab-script-backup](https://github.com/robotlightsyou/qlab-script-backup/tree/543fd36)
- QLab 5 show from [qlab_redundancy](https://github.com/isidorjokull/qlab_redundancy/tree/11a5f2d)
- QLab 5 show from [projection-mapping-experiments](https://github.com/brianmarkowitz/projection-mapping-experiments/tree/7cd842e)
- Target cue behavior in the [QLab 5 reference](https://reference.qlab.app/docs/v5/other-cues/target-cues/)
