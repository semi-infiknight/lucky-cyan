# MIDI cues

A MIDI cue sends its message list from top to bottom when its action starts. Each message has its own output port and channel. A timed message sends its start value with that list and continues changing over the programmed duration. Use a MIDI File cue when playback timing comes from a Standard MIDI File.

## Suspend MIDI cue triggers during programming

1. Bind a MIDI note to a silent Wait in a rehearsal list. Verify one press starts it and stop it.
2. Open **Preferences → Network → MIDI Input** and select **Suspend incoming MIDI control for this session**. Close Preferences and check the suspension message in the transport bar. Play the bound note. The Wait stays idle, and a local **GO** can still start it.
3. Choose **Restore MIDI input** in the transport bar. New MIDI messages can start cues as usual. Messages discarded during suspension stay discarded.

The override follows the running desk across workspace changes until you restore it or quit. MIDI Learn and MIDI cues that send messages remain available. [QLab Override Controls](https://qlab.app/docs/v5/tools/override-controls/)

## Bind a MIDI control to desk transport

1. Add two silent Wait cues to a rehearsal list. Under **Preferences → Network → MIDI Input**, select the controller's named input, turn on **Fire cues from incoming MIDI**, and suspend incoming MIDI control for the session.
2. Under **Workspace MIDI transport**, select **GO**, choose **Capture MIDI**, and press the pedal. Check the captured message and value. For a Control Change pedal that releases at zero, choose **Above** with a value of `0` if its press value varies. Choose **Save binding** and turn on **Enable workspace transport bindings**.
3. Choose **Restore MIDI input** in the transport bar. Press and release the pedal. Check that the press starts the first Wait and the release leaves the second Wait on standby. Press again to start the second Wait.

You can also enter a binding manually. Inkue accepts Note On, Note Off, Control Change and Program Change on a specific channel or any channel. Where the message has a second value, you can match any value, an exact value, or values above or below a threshold. Bindings must match distinct messages. A workspace binding takes priority when the same message also matches a cue trigger. GO and standby or sequence navigation use the active list. Pause, Resume, Stop and Hard Stop apply across lists. The bindings travel with the saved workspace. Select the input port on each machine. [QLab Workspace MIDI Control](https://reference.qlab.app/docs/v5/networking/using-midi-and-msc/#workspace-midi-control)

## Fade an external fader

1. In a practice workspace, add a MIDI cue and choose **+ Add Message**. Under **Port**, select a virtual MIDI monitor. Choose **Control Change**, channel **3**, and controller **7**.
2. Check **Fade over duration**. Set **Start value** to **20**, **End value** to **100**, and **Duration (seconds)** to **2.0**. Press **Test start** and look for `B2 07 14`. Press **Test end** and look for `B2 07 64`. Each button sends its own value immediately.
3. Clear the monitor and press **GO**. Expect the start value, a series of increasing values, and the exact end value once. Set **Pre-Wait** to **1 second**, repeat the run, and check that the first message waits for the cue's action.
4. Pause halfway through the fade. Check that the monitor stays quiet until **Resume**. **Stop** ends the fade and leaves the receiver at the last value it got. Use a separate cue to set a defined reset value if your show requires one.
5. For a cue list sequence, try **Auto-Continue** with a **Post-Wait** of **0.5 seconds**. The next cue starts halfway through the first second of the fade. Try **Auto-Follow** with **Post-Wait** set to **0** to see the next cue start after the fade's last value.

Key Pressure, Channel Pressure and Pitch Bend can also fade. Controller and pressure values use **0–127**. Pitch Bend uses **−8192–8191** with **0** at center. Control Change controllers **120–127** are channel-mode commands and are available only as single messages. Each row starts with a linear curve. The longest fade row defines the cue duration. Use the receiver's MIDI monitor to verify the channel, destination and exact values.

A timed cue requires every row's output port to be present and uniquely named. Inkue opens the ports before sending the first message. It samples the fade on the show clock, skips repeated values and sends the final value on the first tick at or after the deadline. If the operating system reports a send failure, that port stops sending for the rest of the run and the desk reports the error. Resume continues the same run. Restarting after Stop begins at the authored start value.

## Shape a MIDI fade and reuse the curve

1. In the practice fader cue, open **Fade curve** and choose **Custom**. Click the graph halfway across and about one quarter of the way above the bottom. The two segments should rise slowly to the midpoint, then more quickly to the destination. Drag a point to adjust it, use **Delete** to remove a selected point, or hold **Alt** and drag inside a segment to bend it.
2. Choose **Apply curve**. With start **20** and end **100**, the MIDI monitor should show about **40** halfway through. The exact start and final values stay **20** and **100**. The deadline keeps the final value even if the curve touches its target earlier.
3. Choose **Copy curve**. In another timed MIDI message, open **Fade curve**, choose **Paste curve**, and apply it. This copies the shape while the destination, endpoints and duration stay with that row. A descending fade displays the graph falling from its higher start value.
4. Try the **S-Curve**, **Exponential** or **Parametric** presets. Choose **Reset to linear** and apply to restore the constant-rate change. Close and reopen the workspace, then rerun the cue to check its saved shape.

## Rehearse a pitch bend and reset

1. Open a practice workspace. Connect a virtual MIDI receiver with a message monitor. Select a MIDI cue, open its MIDI inspector and choose **+ Add Message**.
2. Choose the receiver under **Port**, set **Type** to **Pitch Bend**, and set **Ch** to **3**. The bend starts at **0**, the center of its range. Enter **259**.
3. Press **Test**. This sends that row immediately. A hexadecimal monitor should show `E2 03 42`. A successful send means the local output accepted the bytes. Check the receiver to confirm routing.
4. In a second MIDI cue, set the same port, type and channel with bend **0**. GO the first cue and then the second. The second cue sends `E2 00 40` to return the receiver to center. The audible pitch interval depends on the receiver's bend range.
5. Add a **Key Pressure (Aftertouch)** message with note **60** and pressure **90** on channel **3**. Expect `A2 3C 5A`. Change the type to **Channel Pressure** and set pressure **90**. Expect `D2 5A`. A receiver needs to respond to the chosen message for the pressure change to be audible.

## Address a MIDI Show Control cue

1. In a practice workspace, connect a virtual MIDI monitor and add a message to a MIDI cue. Set **Type** to **MIDI Show Control**, then select the monitor under **Port**. New MSC rows start with an empty port, individual device **0**, Sound format **16**, command **GO** and Q number **1**.
2. Set **Device ID** to **7**, leave **Format** at **16**, enter **12.3** under **Q number** and **4** under **Q list**. Leave **Q path** empty. Choose **Test**. The monitor should show `F0 7F 07 02 10 01 31 32 2E 33 00 34 F7`. The second byte is part of the MSC header. The third byte addresses receiver 7.
3. Change **Command** to **STOP**, then **RESUME**, and test each in the monitor. The command byte changes from `01` to `02` to `03`. Add Q path **5** and verify that `00 35` appears before the final `F7`. Save, reopen the workspace and confirm that GO sends the same bytes as Test.
4. For rehearsal with a receiver, set its individual device ID and command format to match, choose its output port, and verify the named cue, list and path. Clearing Q number, list and path gives the receiver an unnumbered command. GO can advance its playback position. STOP and RESUME can affect all eligible transitions on that receiver.

The complete message can use up to 128 bytes. For commands that take a cue address, use digits and periods, include a cue number before a list, and include both before a path. LOAD requires a cue number. Group and all-call destinations stay available for review in the original import notes. Supported imported MSC commands start disabled for receiver and port review.

### Send standby or device commands

1. Keep the destination on the virtual MIDI monitor. Change the command to **LOAD** and give it Q number **12.3**. Check that the command byte is `05` and the number is present.
2. Choose **STANDBY_+**, set Q list to **4**, and press **Test**. Expect `F0 7F 07 02 10 11 34 F7`. The list occupies the first operand position for standby and sequence commands.
3. Select **ALL_OFF**, **RESTORE** or **RESET** in the virtual exercise. The command codes are `08`, `09` and `0A`. Each is followed directly by `F7`. Check the receiver's command behavior before programming a live cue.

**GO_OFF** can specify an exact cue address. The command list also includes previous standby and next and previous sequence. Device commands apply to the addressed receiver.

Message framing and operand examples follow the MSC specification and QLab's command/format table. [MIDI Show Control 1.1 specification](https://www.richmondsounddesign.com/docs/midi-show-control-specification.pdf) · [QLab parameter reference](https://qlab.app/docs/v5/scripting/parameter-reference/) · [ETC Eos MSC formatting](https://www.etcconnect.com/WebDocs/Controls/EosFamilyOnlineHelp/en/Content/23_Show_Control/05_MIDI_Show_Control/Hexadecimal_MIDI.htm)

## Run the desk from a Show Control console

1. Add two silent, top-level Wait cues with numbers `1.2` and `01.2` in a rehearsal list. Under **Preferences → Network → MIDI Input**, choose the named input connected to the console. Set **Show Control device ID** to match the console's destination, then enable **Receive MIDI Show Control**.
2. Send a Sound (General) **GO** addressed to `01.2`. Check that only that cue starts. Send numbered **STOP** and confirm that its countdown pauses. Send numbered **RESUME** and check that the countdown continues from the held point.
3. Stop the rehearsal. Send **STANDBY_+** or **STANDBY_-** and confirm that standby moves one armed row without starting a cue. Send an unnumbered **GO** to start the current standby. Send **ALL_OFF** to stop playback across lists.
4. Suspend incoming MIDI in Preferences. Send a rehearsal command and confirm the desk stays idle. Choose **Restore MIDI input** in the transport bar and send a fresh command.

Inkue accepts Lighting (General), Sound (General) and Video (General) at the configured device ID and the standard all-call ID 127. Numbered GO, STOP, RESUME and LOAD use an exact, unique top-level cue number in the addressed list. With no Q list, they use the active list. LOAD prepares supported media for a separate start. Unnumbered RESUME resumes paused playback across lists while held Load media awaits Start. Unnumbered STOP and LOAD, Q path and RESET remain open. [QLab MIDI and MSC](https://reference.qlab.app/docs/v5/networking/using-midi-and-msc/)

### Address another cue list

1. Right-click a second list's tab and choose **Cue list number…**. Enter `04` and press **Enter**. The number appears on the tab. Imported QLab list numbers are shown here, too. Use digits and periods for MIDI Show Control.
2. Put a silent top-level Wait numbered `42` in both lists. Keep the first list selected. Send **GO** with Q number `42` and Q list `04`. Check **Cue monitor** for the second list's Wait and confirm that the first list still has its original standby. Send **STOP** with the same two operands.
3. In list `04`, make two short sequences using **Auto-Continue** or **Auto-Follow** links. Send **SEQUENCE_+** and **SEQUENCE_-** with Q list `04` to move between sequence starts. Use **STANDBY_+** and **STANDBY_-** to move by one armed row. Check the resulting standby in Cue monitor.

Cue list numbers are literal. A command for `4` cannot resolve list `04`. Imports with an ambiguous list number need that number corrected before receiving scoped commands. [QLab cue list numbering](https://reference.qlab.app/docs/v5/fundamentals/cue-lists/)

## Rehearse one SysEx message

1. In a practice workspace, add a MIDI cue and select **MIDI SysEx**. Leave **Port** empty until you have a virtual MIDI monitor. For this exercise, select only that monitor as the output.
2. Enter `7D 10 20 30` in **SysEx bytes (hex)**. Leave the field to save, then press **Test**. The monitor should receive `F0 7D 10 20 30 F7` in that order. Replacing the input with `F0 7D 10 20 30 F7` sends the same bytes.
3. Clear the monitor, set **Pre-Wait** to **1 second**, and press **GO**. Expect one message at action start. Save and reopen the workspace, select the virtual monitor if its route changed, and repeat.

Each row sends one complete message of up to 4,096 bytes, including its `F0` and `F7` status bytes. Data must use two-digit hex bytes from `00` to `7F`, with an optional matched outer pair. A manufacturer ID starting with `00` uses three bytes. A cue containing SysEx checks every row and opens every named output before sending its first message. A missing or ambiguous port prevents playback. [QLab 5 MIDI Cues](https://qlab.app/docs/v5/networking/midi-cues/)

## Send or receive timecode

Choose a **Timecode** cue to generate outgoing timecode. Select **MTC (MIDI Timecode)** and the named MIDI output port, or choose **LTC (Linear Audio)** and a dedicated audio output patch. Match the receiver's frame rate, enter the start and end addresses, and check the first and last frames on an independent receiver. For an incoming feed that fires cues at specific marks, follow the [timecode workshop](../workshops/README.md).

## Read a migrated cue

Inkue supports individual Note On, Note Off, Control Change, Program Change, Key Pressure, Channel Pressure and Pitch Bend messages. When an imported route has no resolved port, the cue retains its messages and starts disabled. Select the Linux output port that reaches your receiver, rehearse the route and arm the cue.

Imported timed MIDI fades open disabled with their decoded source fields in the cue notes. Open the original show in QLab and read its start value, end value, duration and curve. Rebuild the values and the envelope in the MIDI inspector, then compare a capture from the original show with the new run before arming it. QLab SysEx rows also open disabled for review. Read the original bytes from QLab and rebuild the message and route before arming. Use the same review for other MSC commands, group or all-call addresses, unknown commands and incomplete messages.

The offline regression checks cover channel-voice bytes, fade start and end timing, pause/resume and loss of the output during playback. Validate physical interfaces, downstream device response and end-to-end timing on the show rig.

QLab migration references cover voice commands, timed fades and source field meanings. [QLab 5 MIDI Cues](https://qlab.app/docs/v5/networking/midi-cues/) · [QLab OSC Dictionary, MIDI cue methods](https://qlab.app/docs/v5/scripting/osc-dictionary-v5/)
