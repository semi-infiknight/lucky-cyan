# Linux operator rehearsal

This build extends Inkue v1.4.1 for operators moving a show from QLab. The upstream Releases link in the project README downloads the original Inkue distribution. The private rehearsal installer and its corresponding source accompany this build.

## First five minutes

1. On Debian 13 amd64 with a graphical session and PipeWire, extract the kit and run the apt command shown in **START HERE.txt**. It names the exact versioned Debian file. This installs the Inkue application and shares its application identity with an existing Inkue installation. Back up existing shows before installing on a rehearsal desk.
2. Launch Inkue. Open **File → QLab operator guide → Open rehearsal show**. Turn the output down and select the intended output in Preferences. The show opens as a new document.
3. Press **Space** to start cue 1. Press **Space** again to start cue 2. Listen for the two-second fade. Cue 3 follows automatically and sends an OSC marker to port 15391 on localhost. Cue 4 holds. Open the **Sound checks** cart to try its independent one-shot.
4. Press **G**, enter **1**, and press **Return** to reset the playhead for another pass. **Esc** stops playback. Press **Esc** twice for a hard stop. **F5** switches between programming and Show Mode.
5. For your own show, choose **File → QLab operator guide → Import QLab workspace**. Keep the original workspace and its media together. Review the reported cue numbers, conversion notes, unsupported cues and missing paths. Export the report, repair file references in **Check Workspace**, then **Collect and Save** to a new folder. Reopen the collected copy and check cues, waits, groups, output patches and channel mappings against your cue sheet.

## Working with a local assistant

For a saved workspace, supply the helper executable and the absolute show path to your MCP client, followed by `mcp`. The helper serves MCP over standard input and output. A saved workspace can be inspected without opening Inkue.

```sh
inkue-inspect "Evening show.inkue" summary
inkue-inspect "Evening show.inkue" list
inkue-inspect "Evening show.inkue" cue CUE_UUID
inkue-inspect "Evening show.inkue" mcp
```

To program new audio cues, put one explicit sound mark on each relevant script line. Each mark supplies the cue number, name, relative media path and gain in dB.

```text
[SOUND | 1 | House to half | standby.wav | -18]
```

Choose **File → Review audio cue sheet**, enter a title and paste the marked script or a generated JSON sheet. Preview the cues and gain settings, choose the media root and open the reviewed list. Inkue validates all file paths before opening the new workspace. An acceptance entry with the cue count is recorded in its programming history. The helper can also generate the JSON using `inkue-inspect "Evening show.inkue" draft "Act I" script.txt`.

To examine an open desk, choose **Agent access → Start read-only session**. Pass the session file displayed there to the helper. Treat this file as a local credential. The default transport command previews the action. **Arm one request** opens a 60-second window. A client can then queue an action, and the operator must select **Confirm action** at the desk. Approval applies once to the reviewed workspace, list and playhead. Disconnect ends the session. Save the workspace to retain its programming history.

```sh
inkue-inspect --live "$SESSION" summary
inkue-inspect --live "$SESSION" transport go
inkue-inspect --live "$SESSION" transport go --request-approval
inkue-inspect --live "$SESSION" mcp
```

### HTTP and OSC clients

The session JSON supplies the TCP port, UDP port and bearer credential. HTTP clients can POST a JSON command to `http://127.0.0.1:<port>/v1/command` with an Authorization Bearer header. OSC clients can send `/inkue/agent/command` to the session’s loopback UDP port (`osc_port`) with two string arguments, the credential and the same JSON command. Read the single JSON string in `/inkue/agent/result` on the sending socket. Results exceeding one datagram can be retrieved through HTTP.

```json
{"command":"transport","action":"go","request_approval":false}
```

The separate OSC remote in Preferences is for trusted show-control hardware. Enabling that legacy remote permits its direct playback commands. Agent clients use the opt-in session above.

### Network cues

Choose **Network / OSC** from the Add Cue menu to send OSC, UTF-8 text or hex bytes over UDP. Choose a patch and payload format in the inspector. **Test send** sends the actual packet immediately. A converted QLab text or hex message opens disarmed for review. If the source contains multiple candidate formats, its notes retain the original payloads for the operator.

## Repeating the Linux audio check

Use the bundled practice show on an isolated X11 display with one visible Inkue main window and an isolated PipeWire sink. Install `xdotool`, `pipewire-bin` and Python 3. Point Inkue at the same sink and close the operator guide and other dialogs. The runner drives the actual desktop, records from the sink monitor and listens to the actual OSC datagram. It verifies recorded tone, declining fade levels, silence and follow timing on every cycle. For a long run, enable the existing OSC remote on the dedicated test desk, restrict its allowed IPs to 127.0.0.1 and pass its port to the runner. This drives the same live transport without desktop input. The overnight command occupies the desk for eight hours.

```sh
python3 scripts/rehearsal/linux_soak.py --sink rehearsal
python3 scripts/rehearsal/linux_soak.py --sink rehearsal --osc-port 53001 --seconds 28800 > rehearsal.jsonl
```

A virtual sink exercises the software signal path. Venue audio hardware, device disconnect/recovery and the operator's own show require a separate dress rehearsal. Windows and macOS operator rehearsals follow Linux qualification.

### Measure a physical line loopback

Use the intended Linux interface and an isolated rehearsal output. Run a line cable from the output under test to a dedicated line input. Keep the house disconnected and turn off direct and software monitoring for the return so it cannot feed back. In Inkue, select and verify the intended hardware output and practice-show patch yourself. The runner reads the named input while the desk uses the output already selected.

1. Use `wpctl status -n` and `wpctl inspect NODE_ID` to find the return's exact `node.name`. Choose the **Audio/Source** that receives the cabled channel. Capture is mono. For a source that combines channels, confirm the mapping on the interface and qualify each required output separately.
2. With the desk and line quiet, run the first command below and save its `rms` as the noise reference. Manually GO practice cue 1 and repeat the measurement for the steady tone. Set the input gain manually for a steady reference between 300 and 4,000 RMS with plenty of headroom. Noise RMS must be at most the steady reference divided by 250. If you change gain, repeat both readings. Stop all cues and return the playhead to cue 1.
3. Create a new evidence directory. Run one cycle with those measured values and review the readings. If it passes, repeat with the intended duration on a quiet, dedicated rehearsal desk. Leaving out `--osc-port` uses the focused desktop. When using the existing localhost OSC remote, enable it manually on that desk and check the port before starting.

```sh
capture='your.exact.line-input.node.name'
python3 scripts/rehearsal/linux_soak.py --capture-input "$capture" --measure-input
mkdir loopback-check
python3 scripts/rehearsal/linux_soak.py --capture-input "$capture" \
  --reference-rms 1200 --noise-rms 3.4 --failure-evidence loopback-check \
  > loopback-check/cycles.jsonl
```

Use your own measurements in place of the example RMS values. Add `--seconds 28800` and use a fresh evidence directory for the eight-hour run. The runner records the source name and serial, RMS references and each cycle's result. A missing, rerouted or replaced input fails the check. Preserve the process exit status, installation revision, interface and cable route, PipeWire rate and quantum, Inkue buffer request, and both calibration measurements with the result. [What the loopback check measures](verification/README.md#physical-line-loopback)

## Target cues

Add **Command → Target** in the cue toolbar. Choose a Stop, Devamp or Command cue, then choose its numbered replacement destination in the same list. Running Target redirects the subject until an explicit Reset or hard stop. Saving and reopening restores its programmed destination. The preflight check reports invalid assignments. A Goto destination must be at the top level of the list.

## Building this installer

Install the Linux development dependencies described in the project README and install frontend dependencies with `pnpm install --frozen-lockfile`. Run `scripts/rehearsal/package-linux.sh`. The rehearsal build identifies itself in About and uses manual updates. Its Debian package depends on the distribution's libmpv, GTK, WebKitGTK and PipeWire libraries. The bundle script records the minimum glibc version required by the compiled executable.

Use the source Git revision in the package’s `/usr/share/doc/inkue/desk-build.json` to rebuild it with revision metadata. The source ZIP records that revision in its ZIP comment. Building directly from the ZIP produces a local-version package. Use a separate build and rehearsal machine for the timing checks so compiling cannot interfere with playback.
