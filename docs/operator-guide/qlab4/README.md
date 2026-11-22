# Bring a QLab 4 show to Inkue

Keep the calling script and original QLab workspace available for the first Linux rehearsal. Start with **File → Import QLab Workspace… (BETA)**, open a copy of the `.qlab4` file and read the conversion report before saving the Inkue show under a new name. Record the cue number and expected output for each item that needs rebuilding. A cue count alone cannot tell you whether the right speaker, projector or external machine received a cue.

## Call the same sequence from the booth

1. Choose a passage with a manual GO, a pre-wait, an overlapping sound and a follow. On the QLab desk, write down when the caller says GO, when each effect starts, and which cue waits for the next call.
2. In Inkue, open the same cue list. Use **Cue list options → Click moves playhead** to choose whether a mouse selection also moves the standby. Turn it off to inspect later cues while keeping the show parked. Arrow through an expanded group and check the name and number in the inspector.
3. In **Show Mode**, read **Next GO** aloud and press **Space** with focus on the run sheet. Watch **Active in this list** through the first cue’s tail. Compare both the start of the overlap and the end of the follow. Repeat once from inside the passage. A group that retains the playhead can take another GO before the outer list advances.
4. Rehearse **Pause**, **Resume**, **Stop all** and **Hard stop** with the caller. A cue in another list may still be active. Agree which stop to use for an ordinary hold and which one the caller expects in an emergency. [Inkue live desk drill](../live/README.md)

**Checkpoint.** Record the next manual cue, the expected overlap and the stopping behavior. Mark the passage for reprogramming if the operator needs an extra GO or if a follow crosses an intended hold. [QLab 4 cue sequences](https://qlab.app/docs/v4/general/cue-sequences/)

## Verify a migrated Start First And Enter passage

Build this silent passage in a rehearsal copy of QLab 4 or 5, or find a passage with the same shape in your show. Set Group **49 Deck handoff** to **Start First And Enter** and set its Continue mode to **Do not continue**. Set every Wait to **Do not continue** and zero Pre-Wait.

| Cue | Placement | Duration | Armed |
| --- | --- | --- | --- |
| **49.1 Stage clear** | First child of 49 | 5 s | Yes |
| **49.2 Spare call** | Second child of 49 | 2 s | No |
| **49.3 Doors check** | Last child of 49 | 8 s | Yes |
| **50 House ready** | Main list immediately after 49 | 3 s | Yes |

1. On the source desk, put 49 on standby. Give one GO and write down where standby lands. Wait for Stage clear to finish, then give a second GO and record which cue runs and what stands by during its countdown.
2. In Inkue, use **File → Import QLab Workspace… (BETA)** to open the rehearsal copy. In the conversion report, check Group 49 and its children. Expand the group. In its inspector, confirm **Start First And Enter**, confirm cue 49.2 is disarmed and check the final child’s Continue setting.
3. Enable **Cue list options → Click moves playhead** and click **49.1 Stage clear** directly. Open **Show Mode** and give GO. Stage clear runs and **Next GO** shows **49.3 Doors check**. Let the first countdown finish. Doors check remains on standby. Give GO again. The eight-second countdown runs while **50 House ready** is on standby. Give a third GO to start House ready.
4. Stop playback. In programming mode, expand 49 and click **49.3 Doors check**. Use **File → Save As…** to save a new `Deck handoff rehearsal.inkue`. Open that file again. Confirm playback opens idle with 49.3 on standby. Give GO once. Only Doors check should start, with 50 next.

Inkue automatically converts qualifying QLab 4 and 5 groups from the main level of a sequential list. The group must have a manual Continue setting and direct Audio, Video, Memo or Wait children with recognized Continue settings. A group marked for review keeps its children and opens disarmed. Read its conversion note, compare the source order and armed states, and rebuild nested groups or cue-targeted actions in the main list where needed. For this exercise, keep the final child manual and call the first cue after the group separately. Make structural edits to the passage while transport is idle. Test an imported Audio or Video passage again on its intended route before using it live. [QLab 4 Group Cues](https://qlab.app/docs/v4/general/group-cues/) · [QLab 5 Group Cues](https://qlab.app/docs/v5/fundamentals/group-cues/#start-first-and-enter)

## Transfer a cart used by touch or muscle memory

Open the imported cart in programming mode. Choose **Edit pads** and select several pads to inspect their labels and targets. Switch to **Trigger pads** and fire a pad between two easily recognized neighbors. Only the chosen pad and actions it explicitly targets should run. New and QLab-imported carts have no playhead. Older native Inkue files can open in **Legacy chained pads** mode.

Take a picture or diagram of the QLab operator’s grid and compare every occupied position. Inkue can retain QLab’s saved grid and cell references. Compare every empty cell and check **Edit pads → Outside grid** for unplaced cues. Older compact Inkue carts still flow across the available width. Rehearse the fixed layout on the actual touchscreen. For a second display, right-click the cart tab and choose **Open cart in separate window**. Leave the main desk parked on a known standby while testing its pads. A pad’s **Fire** button uses the show outputs. Plan a separate output check for a QLab workflow that depended on private audition routing. [Inkue cart controls](../programming/README.md) · [QLab 4 cart tutorial](https://qlab.app/docs/v4/tutorials/intro-to-cue-carts/)

## Transfer the sound design and collect the media

| QLab 4 rehearsal task | What to do in Inkue | What to record |
| --- | --- | --- |
| Move from a design machine to the booth | Use **File → Collect and Save…**, reopen the collected show with the original source disk absent, and inspect missing targets. | After reopening, the desk waits at the intended standby and the copied media play when called. |
| Design offline, then patch the real interface | Match each saved output patch to Linux hardware. Open the audio matrix and send a single source to each physical destination in turn. Test the highest used channel as well as left and right. | Named speaker, device, physical channel, meter response and audible location. |
| Fade pre-show music or a crossfade | Check the incoming and outgoing targets, fade duration and stop-at-end setting. Begin with the outgoing cue already running, then repeat from a cold start. | Onset of the incoming sound, length of the overlap, and silence from the outgoing source at the end. |
| Hold a musical vamp for a live call | Run several complete repetitions before pressing the Devamp cue. Check the loop exit on the next musical boundary and listen for the first sound after it. | The bar or slice that exits, the next cue number, and any gap or repeat. |

For announcement ducking, let one pre-show track finish while the music is down. Listen to the first bar of the next track before restoring the level. A fade that only reached the outgoing track can leave the next one unexpectedly loud. [QLab 4 pre-show ducking](https://qlab.app/docs/v4/tutorials/how-to-fade-preshow-music/)

A QLab production that uses Audio Units, patch-level processing, rate/pitch changes or animated crosspoints deserves an A/B listen with its original rendered reference. Existing Audio Unit plug-ins need a replacement signal path on Linux. [Inkue audio programming](../audio/README.md) · [Media handoff and control workshops](../workshops/README.md)

## Restore a picture and a lighting look

On the original desk, photograph a framing chart and one representative layered image. In Inkue, use **Preferences → Outputs** to identify the physical screen, then check the cue’s output, layer, opacity, crop and fit. Run the dissolve with both layers active. If a group fade used different starting opacities, check the midpoint as well as the final frame. Compare the alignment at the corners, the outgoing layer at the end, and the screen after a stop. Moving geometry fades, projector regions, unsupported blend modes and macOS interchange such as Syphon require separate review. [Inkue video alignment](../video/README.md) · [QLab 4 projector replacement](https://qlab.app/docs/v4/tutorials/how-to-replace-a-projector/)

An imported Light cue arrives disarmed. In **DMX**, patch the fixtures and groups to the venue universe and addresses. In the cue's **Light** inspector choose **Review imported light commands**. Resolve the lines and compare the proposed target levels before applying them. Start with one fixture, then check a combined look, its fade and blackout. For a show that used QLab’s Pull From Cue palettes, change one source look and verify every destination that should track it. Inkue expands reviewed group and range commands into individual targets when they are applied. [Inkue lighting review and output checks](../lighting/README.md) · [QLab 4 Pull From Cue](https://qlab.app/docs/v4/tutorials/intro-to-pull-from-cue/)

## Replace or reconnect show control

Use a silent Wait cue for the first external trigger. Move its playhead using an addressed OSC select message, then send the separate GO message. Match the receive port, active list, cue number and sender address. Bind a pedal press and its release as separate cases. For timecode, enable the intended list’s **TC SYNC**, check the show’s frame rate and pre-roll, then run through a cue mark. QLab 4 used arriving marks to trigger cues. Test a clock stop and a locate to establish how the Inkue cue behaves after its trigger. Record the first observed firing frame and the behavior when the source returns. [Inkue OSC, MIDI and timecode drills](../workshops/README.md)

A QLab 4 AppleScript cue imports disarmed with its source retained for review. Give the replacement automation an observable rehearsal result, such as selecting a silent Wait. Test each external OSC address and argument, MIDI message, receiving port and acknowledgement the operator relies on. QLab Remote, AppleScript clients and the complete QLab OSC query dictionary require their own integration review. Imported MIDI Show Control, SysEx or timed MIDI voice fades can need reauthoring. [Inkue automation](../automation/README.md) · [MIDI migration behavior](../midi/README.md)

## Remap operator tools

Ask the relief operator to perform their three most frequent edits from a blank selection. Include a cue-number search, a multi-cue edit and an isolated rehearsal. QLab 4 operators may depend on Fancy Paste to copy just one property, a private Audition Window to fold many outputs down to headphones, or Override Controls to suppress remote messages during rehearsal. Identify the equivalent operation or an agreed replacement for each dependency. Confirm where the sound goes and which external messages a rehearsal sends.

For streamed shows, check the audience mix and the return from a separate receiving computer. The QLab 4 Zoom and livestream guides use Mac audio and video routes that may have no direct equivalent on the Linux host. Validate the selected Linux capture devices, picture framing and a return path free of feedback. Check Text cues against a projected reference after installing the venue fonts. A collected show can still render different line breaks. [QLab 4 moving workspaces](https://qlab.app/docs/v4/tutorials/how-to-move-workspaces/) · [QLab 4 Zoom guide](https://qlab.app/docs/v4/tutorials/qlab-zoom-guide/)

## Hand the show back to the caller

Keep a cue-by-cue exception sheet with columns for **cue number**, **observed result on QLab 4**, **observed result on this Inkue build**, **device or route**, **decision** and **operator initials**. Repeat the check on the installed Linux build with the actual console, clock, audio interface and displays after reopening the collected show. A pending device check or an unapproved change stays on the sheet. [Linux qualification exercises](../qualification/README.md) · [QLab 4 reference and tutorial sources](sources/README.md)
