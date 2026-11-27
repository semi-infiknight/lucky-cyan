# Calling a show with Inkue

Start with the **Operator rehearsal** workspace. You can learn the live desk from the four cues and one cart pad in this show without changing your production file.

## Read the desk

Click **Show Mode** or press **F5**. The **Next GO** panel identifies the cue at the playback position by number and name. The run sheet marks the same cue with a triangle and shows the cue type, playback state and duration. A highlighted editing selection can be somewhere else. Read the standby from the playhead before pressing **Space**.

**Active in this list** shows cues that are still running or paused in the selected list. A minus sign counts down the known remaining time. A plus sign counts up the elapsed time for an open-ended cue. Switching lists changes this display. Cues in other lists can continue playing, and **Stop all** and **Hard stop** reach them too.

| Operator action | At the desk |
| --- | --- |
| Fire the next cue | Click **GO** or press **Space** with focus on the run sheet. On a focused button or pad, **Space** activates that control. The configured double-GO protection interval can suppress a second press. |
| Move the playhead | Open **Go to cue** with **G**, type a cue number, and press **Enter**. Verify the standby before firing. |
| Pause or resume | **Pause** affects the running cues in the current list. **Resume** becomes available when that list has paused cues and no cues running. |
| Stop the show | **Stop all** or **Esc** requests the normal stop across lists. The configured audio stop fade can still be audible. |
| Cut immediately | **Hard stop** or two quick presses of **Esc** cuts audio and video immediately and resets cue state. DMX continues tracking its current look. |
| Cut and take lights to black | Enable **Hard stop + blackout** for this workspace in **Preferences → General → Transport**. The separate transport button holds all DMX universes at zero until **Restore lights**. |
| Mute whole input/output paths safely | Open **Overrides** on the transport bar and switch off DMX output, MIDI in/out, MSC in/out, external network input or timecode input; loopback traffic is never blocked. The same toggles answer QLab OSC messages such as `/overrides/toggleDmxOutput` from an allowlisted controller. Switch them back on before the next cue. |
| Return to programming | Click **Show Mode** again or press **F5**. |

QLab operators will recognize the distinction between selection and playback position, and the use of numbered cues to call the show. [QLab workspace reference](https://qlab.app/docs/v5/fundamentals/workspace/) · [QLab cue lists](https://qlab.app/docs/v5/fundamentals/cue-lists/)

## Rehearse a hard stop with lights

1. Open a rehearsal copy in programming mode. In **DMX → Patch**, disable every universe output or use an empty output list. Under **Live controls**, set test channel 1 of universe 1 to 128. Check the value under **Output monitor**. This drill uses the desk’s internal DMX monitor.
2. In **Preferences → General → Transport**, enable **Hard stop + blackout**, apply preferences and save the workspace. Enter **Show Mode** with **F5**.
3. Start a cue. Click **Hard stop**. Verify playback has stopped and the monitor still shows channel 1 at 128.
4. Start the cue again. Click **Hard stop + blackout**. Read **DMX blackout active** on the transport. All lists stop and the output monitor shows zero levels. Close the DMX panel and reopen it to verify that the transport still indicates blackout.
5. Click **Restore lights**. The monitor should show 128 again and playback should remain stopped. A Light cue or fade that changed the tracked values during blackout can produce a different look on restore. Agree on the expected restore look with the lighting operator.

The button setting travels with the saved show. The active blackout belongs to the running engine and holds across workspace changes until released during that desk session. Every DMX slot is zeroed, including parameters such as pan or shutter. Actual fixture behavior follows the fixture’s personality. For QLab operators, **Override Controls** is the name of QLab’s window for suspending signal inputs and outputs, including its **Art-Net and USB DMX** output override. Inkue’s blackout sends zero-valued frames over enabled sACN or Art-Net routes. [QLab 5 Override Controls](https://qlab.app/docs/v5/tools/override-controls/)

## Edit during rehearsal

Live level, pan and matrix controls change their corresponding properties. A name, note or channel-label edit preserves the current gain, pause state and pending timing. A continuing cue keeps any fade it has already received unless you explicitly adjust the affected value. Cursor movement and cue edits leave completed Auto-Continues consumed. Stop or unload a cue before reassigning its output.

Use Undo or Redo after all cues have stopped, including waits and paused cues in other lists. If an output rejects a cue before starting, check the error, verify the current standby and retry when that route is available. For a group with several outputs, check which children already started before retrying.

## Run the two-GO rehearsal

1. In programming mode, choose **File → QLab operator guide → Open rehearsal show**. Select the **Rehearsal** list. Verify that cue **1 · Standby tone** is next and the intended sound-check output is selected.
2. Enter Show Mode and press **Space** once. Listen for the tone. Cue **2 · Fade out** should become the next GO while the tone continues playing.
3. Give the next GO once the first cue is audible. Listen for the two-second fade to silence. Cue **3 · Rehearsal complete** fires on the fade’s auto-follow and sends `/rehearsal/complete` to the local OSC listener on UDP port 15391. The next manual cue is **4 · Hold**.
4. Select **Sound checks** from the list selector in Show Mode. Click pad **S1 · Check tone** for a two-second one-shot. Return to **Rehearsal**. Press **G**, enter **1**, and confirm the standby before repeating.
5. Try the interruption drill. Start cue 1, press **Pause**, listen, then press **Resume**. Press **Stop all** and verify the outputs have settled. Start it again and try **Hard stop**. Discuss with the stage manager which instruction calls for each stop.

The bundled audio and marker make this a repeatable desk check. Venue validation needs the patched loudspeakers, screens, lighting network and monitoring positions that will be used for the performance. [Linux rehearsal record](../../operator-rehearsal/verification/README.md)

## Rehearse waits, follows and groups

Save a rehearsal copy before changing cue timing. Use two short audio cues with distinguishable sounds and give them consecutive numbers. In the inspector, choose a continue mode for the first cue. Compare these three passes.

- **Manual** leaves the second cue waiting for another GO.
- **Auto-continue** starts the next cue from the first cue’s start timing. A zero post-wait chains immediately. With a longer first sound, set a one-second post-wait to hear the second enter before the first finishes.
- **Auto-follow** starts the next cue when the first completes. The two-second fade and its OSC marker in the bundled show demonstrate this relationship.

Try a pre-wait on an audio cue and compare the interval from GO to audible onset. Run the sequence from the cue before it as well as from the cue itself. Record the exact standby, overlap and stop behavior expected by the caller. [QLab cue sequences](https://qlab.app/docs/v5/fundamentals/cue-sequences/) · [QLab tutorials](https://qlab.app/docs/v5/tutorials/)

A **Sequential** group accepts further GO actions to advance its children according to their settings. For an ordered group, Next GO shows the next child supplied by the transport and its parent group names. A **Simultaneous** group starts its children together. Child pre-waits can stagger their entrances. **Playlist** runs one child at a time and can be configured to loop. **Start Random** chooses a child at runtime, so the desk shows the group as the standby.

### Rehearse a Start First group

1. In a rehearsal copy, add a Group named **Preset checks**. Select **Start First** in its inspector. Set the group's own Continue mode to **Do not continue**.
2. Inside the group, add two Wait cues. Give **Check A** a four-second duration and **Check B** a two-second duration. Put a third Wait named **House call** immediately below the group in the main cue list.
3. Set Check A to **Auto-continue** with a one-second Post-Wait. Give GO on the group. Standby moves to House call as soon as Check A starts. Check B starts a second into Check A and both timers run together. Give another GO while the timers are running to start House call.
4. Stop, park on Preset checks and change Check A to **Auto-follow**. Check B now starts when Check A finishes. Try **Do not continue** on Check A and repeat. Check B stays idle even when you give GO for House call.

Start First skips disarmed children in Inkue. An ordinary group Pre-Wait counts down after standby moves on. A File Audio cue that requires a protected output can hold standby at the group until the route accepts the start. Pause the group to hold its internal timing. Stop the group to cancel remaining children and pending continuation. Use the Group row for standby and GO. Keep nested Groups, Fades and cue-targeting commands such as Stop and Devamp in the main list for this workflow. [QLab 5 Group Cues](https://qlab.app/docs/v5/fundamentals/group-cues/#start-first) · [QLab 5 Inspector timing](https://qlab.app/docs/v5/fundamentals/inspector/#post-wait)

After importing a QLab 4 or 5 **Start First** group, check its mode and the conversion report. Automatic conversion covers the main level of sequential lists, with direct Audio, Video, Memo or Wait children, at least one armed child, and **Do not continue** on the group and its last armed child. A nonzero Memo Pre-Wait or a change to a child’s arming during conversion requires review. Check the assigned media and output routes before GO. Groups needing review open disarmed with their children and a note. Compare the source sequence, rebuild any nested groups or targeted actions in the main list, and rehearse before arming. [QLab 4 Group Cues](https://qlab.app/docs/v4/general/group-cues/) · [QLab 5 Group Cues](https://qlab.app/docs/v5/fundamentals/group-cues/#start-first)

### Rehearse Start First And Enter

1. Add a group named **Deck checks** to a sequential cue list. Choose **Start First And Enter** and put two short Wait cues inside it. Set both children to **Do not continue**. Add a third cue after the group in the main list.
2. Park on Deck checks and give GO. The first child starts and the second appears under **Next GO** in Show Mode. Wait for the first child to finish. The second child remains on standby until you give GO again.
3. Give GO and watch standby move to the cue after the group while the second timer is still running. Try the same exercise with the first child set to **Auto-follow** and verify that the second starts on its own.
4. Repeat with the first child set to **Auto-continue** and a Post-Wait shorter than its timer. Verify that the two timers overlap and standby leaves the group when the last starts.
5. Stop playback, collapse the group, press **G**, and enter the second Wait’s cue number. The desk opens the group and scrolls to the Wait while the Inspector keeps its previous selection. Save and reopen the show. The second Wait remains on standby and playback opens idle. Move the cue that follows the group, then undo that move to check that structural Undo restores the exact child on standby.

Disarmed children are skipped. A protected File Audio start keeps standby at the group until its output accepts the child. If the output fails, restore the route before retrying. Stop the group before changing its child order or membership. Set the group’s own Continue to **Do not continue**. To rehearse an immediate handoff, set the final armed child to a Wait with **Auto-continue**, zero Pre-Wait, and zero Post-Wait. Give **GO** from the main sequential list. The next armed cue outside the group starts when that Wait starts. Moving standby during the internal sequence cancels the automatic handoff. For media at the group boundary, move the last cue into the main list and program continuation there. Program nested groups and cue-list actions such as Fades and Stops in the main list for this workflow. [QLab 5 Group Cues](https://qlab.app/docs/v5/fundamentals/group-cues/#start-first-and-enter)

**Delay Auto-Continue.** Give the final Wait a five-second duration, zero Pre-Wait, **Auto-continue**, and a two-second Post-Wait. Give GO until the final Wait starts. The outer standby starts two seconds into the Wait, so the timers overlap for three seconds. Try a seven-second Post-Wait. The final Wait finishes first, and the outer cue starts two seconds later. Pause the group during that gap and resume to finish the remaining delay. Moving standby cancels the handoff.

**Follow a timed final Wait.** Keep **Deck checks** manual. Set its final direct Wait to a duration of five seconds, **Auto-follow**, and zero Post-Wait. Set the first armed cue after the group to a manual Wait named **House ready**. From the main list, give GO until the final child starts. **Next GO** now reads House ready. Its timer should stay idle through the final child's five seconds and start once as the child finishes.

Next, give the final Wait a three-second Post-Wait. Keep its duration at five seconds. House ready stays on standby for all eight seconds. Pause the group in the gap after the child finishes, wait past the original deadline, and resume. House ready starts once the remaining Post-Wait expires. On a fresh run move standby to a different cue during the gap, then return it to House ready. House ready now waits for a manual GO.

Clear the Post-Wait and repeat with two seconds of Pre-Wait on the final child. Pause the group partway through the countdown, wait past the original finish time, and resume. House ready starts after the remaining time. On a fresh run, move standby away and back during the countdown. House ready now needs a manual GO. [QLab 5 Start First And Enter](https://qlab.app/docs/v5/fundamentals/group-cues/#start-first-and-enter) · [QLab 5 sequence timing](https://qlab.app/docs/v5/fundamentals/workspace/#continue-mode)

QLab 4 and 5 imports use this mode for top-level groups with supported direct Audio, Video, Memo or Wait children. Both the group and its last armed child need a manual Continue setting. For other groups with this source mode, the import retains the children and disarms the group with a conversion note. Review its original sequence and choose the correct Inkue group mode before arming. Run the migration exercise to compare the source standby and reopen the imported copy. [Start First And Enter migration exercise](../qlab4/README.md#verify-a-migrated-start-first-and-enter-passage) · [QLab 4 Group Cues](https://qlab.app/docs/v4/general/group-cues/) · [QLab 5 Group Cues](https://qlab.app/docs/v5/fundamentals/group-cues/#start-first-and-enter)

### Run a Playlist while the main list continues

1. In a rehearsal copy, put two short Wait cues in a Playlist Group named **Doors checks**. Insert a third Wait, **House call**, immediately below the group. Set the group's own Continue mode to **Do not continue**.
2. In **Inspector → Group**, turn on **Advance playhead after GO** and **Loop back to the first child**. Give GO on Doors checks. Its first child starts and House call is now on standby.
3. Give GO on House call. The Playlist keeps following its internal order while House call runs. Let the Playlist wrap once and check that the main-list playhead stays where you left it. Stop Doors checks to end the loop.

New Playlist groups advance standby by default. Playlist groups saved by an earlier Inkue build open with their existing standby behavior. An active Playlist keeps running on a repeated trigger when **Second Trigger** is set to **Keep playing**. A required File Audio output can hold the initial standby until admission. If a later track loses its route, fix the Output Patch and explicitly retry the Playlist. Use direct playback cues in an advancing Playlist. Place nested groups, Fades and cue-targeting commands in the main list. [QLab Playlist Groups](https://qlab.app/docs/v5/fundamentals/group-cues/#the-playlist-tab)

When importing a simple QLab Playlist, confirm that the **Loop back to the first child** checkbox matches the source. Check the handoff against the original. Inkue uses the preceding child’s completion to advance, so it removes the duration QLab displays as each child’s Playlist Post-Wait. Crossfades, shuffle, second-trigger actions and ambiguous archive settings leave the imported group disarmed for review. [QLab Playlist settings](https://qlab.app/docs/v5/fundamentals/group-cues/#the-playlist-tab)
#### Step a running Playlist with its hotkey

1. Add three 30-second Wait cues named **Lobby**, **Balcony**, and **House** to an advancing Playlist in a sequential list. Give each zero Pre-Wait. Assign a keyboard trigger to the Playlist in **Inspector → Triggers**.
2. Set **Second Trigger** to **Play next**. Give GO on the Playlist, then park standby on a later cue in the main list. Press the Playlist hotkey while Lobby is counting down. Balcony takes over and main-list standby stays where you parked it.
3. Stop the Playlist and choose **Play previous**. Turn on **Loop back to the first child** and start again. The same hotkey moves from Lobby to House. Turn off looping and repeat to confirm that the first child keeps playing at the start boundary.

The addressed trigger follows the current Playlist order and skips disarmed children. With **Auto-shuffle** off, this is the authored order. A destination Wait needs a positive duration and zero Pre-Wait. Direct File Audio can also step on the main output. Both files must have finished decoding, use mono or stereo, play once at normal speed and start at the beginning. Use zero Pre-Wait and clear cue fades, slices, effects, matrix routing, Load to Time, and cue or workspace Output Patch assignments. Crossfades and imported QLab Second Trigger settings require review. [QLab Second Triggers](https://qlab.app/docs/v5/fundamentals/inspector/#second-triggers)

**Rehearse a music cut.** Replace Lobby and Balcony with two distinct, 30-second mono or stereo files. Set **Play next**, give GO on the Playlist, and park the main-list standby on House call. Press the Playlist hotkey during the first track. Listen for the cut to the second track and check that House call stays on standby. The main mixer starts the new file and cuts the old file in the same output block. Stop the group, give Balcony a Pre-Wait, and repeat. The desk declines the step and Lobby keeps playing. Restore zero Pre-Wait before the next run.

#### Shuffle a Playlist without changing its cue order

1. Give an advancing Playlist four 30-second Wait children named **North**, **South**, **East**, and **West**. In **Inspector → Group**, enable **Auto-shuffle** and **Loop back to the first child**. Give GO on the group.
2. Check **Playback order** in the same inspector. Let the first Wait finish or use the Playlist hotkey from the previous exercise to step to the next Wait. Watch the sequence wrap to its first entry when it reaches the end. With **Play previous**, the hotkey follows the same order backwards.
3. Stop the Playlist and start it again. The desk draws a fresh order, which can happen to match the previous one. Save and reopen the workspace. The four children are still in their authored row order and Auto-shuffle remains enabled. The next GO draws the playback order.

A fresh GO creates the order. Looping reuses it for that run. Group Load leaves shuffle and playback idle. Stop the Playlist before rearranging its children or changing Auto-shuffle. Confirm imported shuffle settings separately before arming a migrated Playlist. [QLab Auto-shuffle](https://qlab.app/docs/v5/fundamentals/group-cues/#auto-shuffle)

## Work a cart

Use a cart for calls that may arrive out of order, such as a bell, a sting or a spare announcement. In Show Mode, click a pad to fire it. A running pad has its own **Stop** control. Use the list selector to return to your sequential run sheet. Compare unfamiliar imported cart behavior on a copy before using a pad in performance. [QLab cue carts](https://qlab.app/docs/v5/fundamentals/cue-carts/)

## Recover and hand off

After an abnormal exit, the next launch can offer **Recover Unsaved Work**. Choose **Recover** to inspect the restored workspace, list and playhead. Recovery opens with playback idle. Confirm what actually reached the stage with the caller before giving another GO.

For a handoff, stop playback, save or **Collect and Save** the show and reopen the collected copy on the receiving machine. Use **Check Workspace** to review conversion notes, missing media and unresolved targets. A collected show carries media and cue programming. Hardware names and physical patch connections require a check on the receiving desk. Run the first cue, a follow and the final stop on the intended outputs. Hand over the confirmed cue number, active list, output patches and any fixes still due.

The current build has a working Linux audio rehearsal and recovery path. Lighting behavior, video stages, plug-ins and other show-specific dependencies require their own checks before calling a migrated show in front of an audience. [QLab migration and Linux rehearsal guide](../../operator-rehearsal/README.md)
