# Rehearse show control and handoff

Use a fresh rehearsal workspace with an ordinary sequential cue list. Add two Wait cues numbered **101** and **102**, name them **Standby** and **Crossing**, and give each a duration of five seconds. Leave both set to manual continuation. Run each once from the desk and confirm it appears among the active cues for five seconds. Save the workspace. These silent cues make remote messages observable without firing a stage effect. [Program a cue list](../programming/README.md)

## Move the playhead from an OSC controller

**Equipment** — an OSC sender on the same computer. On Linux, the following command uses the optional liblo utility `oscsend`.

1. In Preferences, find **OSC Receive**. Set **IP Allowlist** to `127.0.0.1`, set the port to **53001**, then enable **Listen for OSC commands**.
2. Put the playhead on 101. From the same computer send the following OSC message with an empty type signature.

   ```sh
   oscsend 127.0.0.1 53001 /inkue/cue/102/select ''
   ```

3. Check that the playhead moved to 102 and no Wait started. Then send the second message and watch Crossing appear among the active cues.

   ```sh
   oscsend 127.0.0.1 53001 /inkue/cue/102/go ''
   ```

4. Give Crossing a 20-second duration for this step if you need more time. Run 102, then move Standby back to 101 without firing it and stop Crossing by number.

   ```sh
   oscsend 127.0.0.1 53001 /playhead/101 ''
   oscsend 127.0.0.1 53001 /cue/102/stop ''
   ```

   Confirm Crossing leaves the active cues and the playhead remains on 101. Restart Crossing with `/cue/102/start`. Send `/cue/all/stop`, then `/cue/999/stop`. Crossing continues until you stop 102 or it finishes. Restore the original duration before the next workshop.

5. Disable the receiver and repeat the first message with the playhead on 101. It should stay on 101. Restore the previous Preferences values when finished.

If nothing moves, compare the actual receive port, sender IP, cue number and active list with your setup. A hardware controller coming from another computer needs the desk’s rehearsal-network IP as its destination and its own sender IP in the allowlist. For an assistant that needs operator confirmation, use the separate local agent session. [Program with a local assistant](../automation/README.md)

To rehearse remote pause, give 102 a 20-second duration and put 101 in standby. With OSC Receive enabled, send `/cue/102/start`, then `/inkue/pause`. Cue 102 should hold its countdown. Send `/cue/102/resume` to continue it and check that standby stays on 101. To pause only 102, send `/cue/102/pause`. Inkue also accepts `/inkue/cue/102/pause` and `/inkue/cue/102/resume`. Use no arguments and address a unique top-level cue in the active list. `/inkue/resume` resumes paused cues in the active list. `/inkue/pause_toggle` pauses running cues in that list or resumes its paused cues when none are running. Stop 102 and restore its duration and the previous OSC settings. [Figure 53 OSC dictionary](https://qlab.app/docs/v5/scripting/osc-dictionary-v5/)

## Read standby and cue state from an OSC controller

Keep the two silent Wait cues. Give 102 a duration of 20 seconds. Enable **OSC Receive** on port **53001** and add `127.0.0.1` to **IP Allowlist**. OSC Feedback can stay disabled. Set standby to 101, then run this in a local terminal with Python 3.

```sh
python3 - <<'PY'
import json
import socket

def osc_string(text):
    data = text.encode() + b"\0"
    return data + b"\0" * (-len(data) % 4)

with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as peer:
    peer.connect(("127.0.0.1", 53001))
    peer.settimeout(1)

    def query(path):
        peer.send(osc_string(path) + osc_string(","))
        reply = peer.recv(4096)
        prefix = osc_string("/reply" + path) + osc_string(",s")
        assert reply.startswith(prefix), f"unexpected reply to {path}"
        result = json.loads(reply[len(prefix):].split(b"\0", 1)[0])
        print(path, result["status"], result["data"])
        return result

    standby = query("/cue/playhead/number")
    print("Workspace ID", standby["workspace_id"])
    query("/cue/102/number")
    query("/cue/102/isRunning")
    query("/cue/102/isPaused")
    query("/cue/999/number")  # Leave 999 unassigned for this exercise.
PY
```

Standby returns `101` and both state queries return `False`. Cue 999 returns `error` with `None` displayed for its JSON `null` result. Set standby to 102 and press **GO**. Repeat the script during its action to see `isRunning` return `True`. Press **Pause**, run the script again, and check that `isPaused` is `True`. Stop 102 and restore its duration and the previous OSC settings.

Send each query as a binary OSC message with no arguments. Inkue returns one JSON string at `/reply` followed by the request address and sends it to the sender’s UDP port. The reply supplies `address`, `workspace_id` for this open workspace instance, `status`, and `data`. A short address such as `/cue/102/number` uses a unique top-level number in the active list. Query numbers can be up to 96 bytes. If the desk is busy and a request times out, retry to get a fresh reading. [Figure 53 OSC dictionary](https://qlab.app/docs/v5/scripting/osc-dictionary-v5/)

### Read a group child or a cue in a different list

1. In a rehearsal copy, add another cue list and put a silent Wait cue numbered `201.1` inside a group. Return to the original list. Use the terminal script above and keep the `workspace_id` from its standby reply.
2. Send `/workspace/{workspace_id}/cue/201.1/uniqueID`, replacing the placeholder with the UUID from that reply. The result is the child’s cue UUID. Use `/workspace/{workspace_id}/cue_id/{cue_id}/isRunning` to monitor the child. `/cue_id/{cue_id}/isPaused` also works for the current open workspace. An unnumbered cue can answer by ID, and a query for its number returns `error` with `null`.
3. Give one other cue the same number in a rehearsal copy. The workspace-wide number lookup reports `error`. The cue ID continues to identify the original child. Reopen the show and get a fresh `workspace_id` before sending scoped queries. A request for a previous workspace instance receives no reply.
4. Save the rehearsal copy as `RehearsalBlue.inkue` and restore a unique cue number. Send `/workspace/RehearsalBlue/cue/201.1/uniqueID` or `/workspace/RehearsalBlue.inkue/cue/201.1/uniqueID`. Match the exact display title or saved filename and its letter case. Use the workspace UUID for titles containing spaces, non-ASCII characters, or OSC pattern punctuation. A name follows the workspace currently open with the matching name. The UUID identifies one open instance.

Workspace-wide queries search up to 4,096 lists and 4,096 cue rows in total, including group children and cart pads. A request beyond that limit reports `error`. `/cue/playhead/number` and `/cue/playhead/uniqueID` refer to standby in the active list, including the direct child that GO would start inside a Start First And Enter group. [Figure 53 OSC cue addressing](https://qlab.app/docs/v5/scripting/osc-dictionary-v5/#addressing-cues)

## Recover from an occupied OSC port

Use the same silent rehearsal cues. In **Preferences → Network**, select the loopback interface or **Automatic**, leave OSC receive disabled, and set its port to **53002** and its allowlist to `127.0.0.1`.

1. In a local terminal with Python 3, hold the rehearsal port open.

   ```sh
   python3 -c 'import socket; sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); sock.bind(("127.0.0.1", 53002)); input("Press Enter to release UDP port 53002. ")'
   ```

2. In Inkue enable **Listen for OSC commands**. The **Listener status** row should show that Inkue could not listen on port 53002. Select **Retry** while the terminal still holds the port and confirm the failure remains visible.
3. Press Enter in the terminal to release the port. Select **Retry** if the status still shows the failure. Confirm **Listening on** shows the bound address and port. `0.0.0.0` means the listener is bound to all IPv4 interfaces, including loopback.
4. Put the playhead on 101. Send the select message from the previous exercise to `127.0.0.1` on port 53002. Confirm the playhead moves to 102 and no Wait starts. Disable OSC receive and confirm the status reads **Disabled**. Restore the previous network interface and OSC settings.

[Figure 53 OSC listening status and retry release notes](https://qlab.app/release-notes/5.6/)

## Trigger a rehearsal cue with plain text UDP

Use the two Wait cues from the start of this chapter. In **Preferences → Network**, set **IP Allowlist** to `127.0.0.1`, set **Plain Text Receive** to port **53535**, then enable its listener. Look for **Listening on** and confirm the UDP endpoint. The binary **OSC Receive** listener is independent and can stay off.

1. With 101 in standby, send the text below from a local terminal with Python 3. Confirm the playhead moves to 102 and neither Wait starts.

   ```sh
   python3 -c 'import socket; sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); sock.sendto(b"/inkue/cue/102/select", ("127.0.0.1", 53535))'
   ```

2. Change `select` to `go` in the command and send it. Only 102 should run. Wait for it to finish, then disarm 101. Wait for the desk to show the disarmed cue and send `/inkue/cue/101/go`. Both Waits should stay idle and the playhead should stay where it was.
3. Arm 101 again, park standby on 101 and wait for both cues to stop. Send `/cue/102/start`. Crossing runs while Standby remains at the playhead. Wait for Crossing to stop. Set its Continue mode to Auto-Follow and send the same message. Confirm that neither cue runs. Restore its manual Continue mode and repeat.
4. Remove `127.0.0.1` from the allowlist and repeat the select test. The desk should ignore it even though the socket remains bound. Disable the plain text listener and restore the rehearsal settings.

Send one UTF-8 command of up to 1,024 bytes per datagram. Inkue addresses for `seek`, `seek/relative` or `seek/percent` take one numeric argument. On either receive port, `/playhead/{number}` and `/playbackPosition/{number}` set standby, `/cue/{number}/stop` applies the desk's soft stop, and `/cue/{number}/start` starts the cue independently. The matching Inkue paths are `/inkue/cue/{number}/select`, `/inkue/cue/{number}/stop`, and `/inkue/cue/{number}/start`. These addresses take a unique, literal top-level cue number in the active list and no arguments. Independent Start requires a manual cue or independent cart pad. Ordered groups, Start and GoTo actions, crossfade entrances and workspace selectors need separate programming. Plain text also accepts QLab `/go` and `/stop`. [QLab workspace network settings](https://qlab.app/docs/v5/fundamentals/workspace-settings/) · [QLab OSC dictionary](https://qlab.app/docs/v5/scripting/osc-dictionary-v5/)

## Bind a footswitch and check its release

**Equipment** — a MIDI footswitch, pad or virtual controller sending Control Change. Set it to send value 127 on press and 0 on release.

1. In Preferences under **MIDI Triggers**, choose **Fire cues from incoming MIDI** and the named input port. If the port is absent, connect the controller and reopen Preferences.
2. Select 101. Open **Triggers**, enable its MIDI trigger and choose **Learn**. Press the pedal. Check the learned message type, channel and controller number against the controller.
3. Set the value filter to **Exactly** and enter **127**. Start with both Wait cues stopped. Press the pedal once, hold it through the end of the five seconds, then release it.
4. One press should start Standby once. Release should leave the active-cue display empty. Press again to confirm a second deliberate press works. If release fires, inspect the learned message and release value. A keyboard that sends Note Off on release needs a matching Note On binding for the press.
5. Save and reopen the show, confirm the input port, and run the press-and-release test again. Record the port and switch values on the handoff sheet.

## Land on a timecode mark

**Equipment** — a MIDI timecode generator or software source with a MIDI route to the desk. Use 25 fps for this exercise. An LTC source can be substituted using a dedicated audio input. Keep the LTC signal off the audience mix.

1. With the desk already open, enable **Timecode Receive** in Preferences. Choose **MTC** and select the input port. For LTC, choose **LTC** and the dedicated input device, with the clock wired to the first channel. Open the Cue List’s clock settings, enable **Sync cues from incoming timecode** and select 25 fps.
2. On 101, enable **Triggers → Timecode trigger**. Leave **Real Time (milliseconds)** clear. Set **Rate** to 25 fps, enter `01:00:03:00` and choose **Apply Trigger**. Repeat on 102 at `01:00:11:00`. Check the two saved addresses.
3. Roll the generator continuously from `01:00:00:00` through `01:00:18:00`. Observe the receiving desk’s clock and each active Wait. Expect two starts in sequence with a gap of about three seconds between the end of 101 and start of 102.
4. With transport stopped, locate the MTC sender exactly to `01:00:03:00`. If the sender emits a Full Frame locate, the address appears dim and 101 stays idle. Then locate to `01:00:08:00` and start the clock. Expect only 102. For LTC, start from `01:00:08:00` and check the same cue result.
5. Return to `01:00:00:00` and roll forward. Expect 101 and 102 to fire again at their marks. Pause the clock for more than half a second before the first mark, relocate the source to `01:00:08:00` and roll through the second mark. 102 should fire, and the gap in signal should leave 101 idle.
6. Change the Cue List’s expected rate to 30 fps while leaving the generator at 25 fps, clear the active cues and repeat the continuous pass. The clock still displays 25 fps and both cues stay idle. Restore 25 fps and repeat after disabling and re-enabling the receiver, without restarting the desk.

Inkue starts cues at received marks during forward playback. A forward jump can fire a cue at the first received moving frame at its destination. Marks skipped by the jump stay idle. A stop or MTC locate cancels pending starts from before the clock change. A backward locate re-arms marks that the clock crosses again. Restart with pre-roll before the first required mark. QLab 5 lookback and offset playback require separate support. Record the starts and signal status with the generator and frame rate used in the show. [Figure 53 timecode settings](https://qlab.app/docs/v5/fundamentals/cue-lists/#the-timecode-tab) · [MIDI Association timecode specification](https://midi.org/midi-time-code)

### Lose the clock during a Wait

1. In a rehearsal list, program two 20-second top-level **Wait** cues and set both to **Do Not Continue**. Give the first an upcoming timecode mark, and start the second by hand just before you roll. Open the list’s timecode settings. Choose **Pause clock-started Wait cues** under **On Stop**, and set **Freewheel** to 800 ms.
2. Run the timecode source through the mark. Interrupt the input for half a second and confirm both waits keep running. Disconnect it for more than 800 ms. The first Wait should pause, and the manually started one should keep counting.
3. Send an MTC Full Frame locate. The first Wait stays paused. Start sending moving frames again and check that it resumes from the held time. Repeat the loss, then GO a **Pause** cue that targets the clock-paused Wait. On clock return it stays paused until you use **Resume**.
4. Try **Stop clock-started Wait cues** on a fresh run. After the same gap the first Wait returns to standby. Restoring reception leaves it there until a later trigger or GO.

Freewheel measures time since the last decoded frame observation, with a minimum of two normal observation intervals. At 25 fps that minimum is about 160 ms for MTC or 80 ms for LTC. A paused Wait excludes the time from the freewheel deadline until the next moving frame. The transport clock indicator has its own timeout, so it can change at a different moment. Only directly clock-started top-level Wait cues use this loss policy. Audio, Video, and cues reached through groups or continuation need separate support. Switching the receiver, disabling the list, or choosing **Keep cues running** relinquishes any clock-owned hold. Resume that Wait from the desk. [Figure 53 timecode settings](https://qlab.app/docs/v5/fundamentals/cue-lists/#the-timecode-tab)

### Monitor the clock without firing cues

1. Set two upcoming marks on short Wait cues several seconds apart. Enable the receiver and the cue list’s **TC Sync**, then open **Preferences → Network → Timecode Receive**. Select **Suspend timecode triggers and clock-loss actions for this session**.
2. Roll MTC or LTC through the first mark. Check the moving address next to the suspension message in the transport bar. The first Wait stays idle. Manual GO still works.
3. While the address is moving between the marks, select **Restore timecode triggers** in the transport bar. The desk takes the first fresh clock update as a starting position. Only the second Wait fires when its mark arrives.

Suspension applies to all lists and workspaces until restored or Inkue quits. A Wait running when you suspend keeps running. A Wait already paused by clock loss stays paused for manual Resume. Clock-loss actions apply again to Waits subsequently started by timecode. Saved cue marks, receiver settings and outgoing Timecode cues keep their programming.

A **Timecode cue** generates outgoing MTC or LTC. To test that path, route its selected MIDI output or LTC audio patch to an independent monitor, pick a frame rate and set an end time. Keep outgoing timecode disconnected from this exercise’s receiving port until the routing is understood.

## Coordinate a sound and lighting mark

**Crew and equipment** — a caller, a sound operator, a lighting operator, one timecode generator, a monitored audio output and a test fixture or DMX receiver. A crew member can keep the timestamp log while the other operator handles both cue paths.

1. In a rehearsal copy, make one short Audio cue with a clear onset and one Light cue with a visible transition. Send the Audio cue to the rehearsal monitor. Route the Light cue to an isolated fixture or monitor. Check both by hand before enabling their triggers.
2. At 25 fps, set the Audio trigger to `01:00:05:00` and the Light trigger to `01:00:08:00`. Label both with the department and action. Put the shared rate, the two addresses, the trigger source and the receiver assignment on one cue sheet.
3. With both departments ready and the caller's permission, roll from `01:00:02:00`. Have each operator record the received timecode and the actual output event. Compare the reports with the cue sheet. Repeat with the expected production rate before committing the programming.
4. Stop the generator between the marks and isolate the stage outputs. Practice stopping active cues, confirming each receiver, and rolling again from upstream of both marks. Verify that each receiver saw one intended action for the authorized second run. Write down which operator clears a running cue and who authorizes the next roll.

This exercise checks handoff, clock source and independently observable outputs. For a show that starts audio and lighting on the same frame, give both cues the same mark and measure each physical output on the venue rig. [ARKO Stage Arts Academy timecode programme](https://www.culture.go.kr/portal/cltKnw/cltEdu/view.do?indRow=84&menuNo=200144&rcrtSn=222061&sWord=&selectType=&viewTp=3&weekju=&yearmonth=)

## Transfer the show to a relief operator

1. Add one short local audio file to the rehearsal workspace. Give the cue a recognizable number and name. Use a file stored outside the folder you will transfer so the test can detect a bad media path.
2. Run **Check Workspace** and resolve missing assets. Choose **File → Collect and Save…** into a fresh folder. Give the relief operator the entire collected folder and a sheet recording the show revision, starting cue, output device and channels, MIDI ports, OSC sender and port, frame rate, and first timecode mark.
3. On the destination Linux desk, keep the original media folder unavailable. Open the collected workspace and run **Check Workspace** again. Identify the local device for each patch and select the named MIDI/timecode ports. Rehearse the audio cue on an isolated monitor at low level.
4. Have the relief operator run the two silent cues from the local desk, then repeat each external control exercise used by the show. Record the observed outcome beside each patch and trigger. A transferred file alone cannot demonstrate hardware routing.
5. For a spare machine, nominate the computer that owns each stage output. Disconnect or isolate those outputs from the spare until the crew has transferred ownership. Practice the changeover on test destinations, check the same cue number on both desks, and send one event. The receiving device should see exactly one event. Record who authorizes the transfer and which links they switch.

| Handoff item | Example of an observable result |
| --- | --- |
| Show and media | Destination reopened the collected show with the source disk absent. The called audio cue played on the identified monitor. |
| Network and MIDI | Cue 102 moved into standby without running. A single footswitch press ran 101 once. |
| Clock | Both marks fired on a continuous 25 fps pass. The operator recorded what happened after a locate. |
| Changeover | A test receiver counted one event after output ownership moved. The outgoing desk was isolated. |

Keep results for the installed build and hardware actually used. These exercises describe acceptance criteria. Venue results belong in the show’s run sheet. [Release qualification](../qualification/README.md) · [Training sources](../learning-sources/README.md)
