# Program cue lists and carts

Use the bundled **Operator rehearsal** show for these exercises. Cue 1 starts a tone on the configured output. In the sequential list, the playhead marks the cue that receives the next GO. The selected row supplies the cue shown in the inspector. The two can point to different cues.

## Prepare the next cue while editing another

1. Open the rehearsal show from **File → QLab operator guide**. Select **Rehearsal** in the list tabs. When a tab has keyboard focus, **Left/Right** switches lists, **Home/End** selects the first or last list, and **F2** renames the focused list.
2. Click cue 1. With the default settings, the playhead moves to it. Open **Cue list options** (⋯ in the first column header) and clear **Click moves playhead**. This preference is remembered on this desk.
3. Click cue 2. Its row is selected for editing and the playhead stays at cue 1. Press **Enter** on the row to open its inspector. Press **F2** on a focused row to rename the cue in place. Double-click a cue number to edit it. **Enter** commits an inline edit. **Escape** cancels it.
4. Right-click the cue you want to call next and choose **Set Playhead Here**. Read the playhead marker before pressing **Space** to GO. To move the playhead between top-level cues with the keyboard, use **Ctrl+Up/Down** on Linux or **Command+Up/Down** on macOS.

QLab also distinguishes selection from playback position. Operators who use QLab shortcuts by memory should check the Inkue bindings before calling the show. [QLab cue lists](https://qlab.app/docs/v5/fundamentals/cue-lists/) · [QLab keyboard shortcuts](https://qlab.app/docs/v5/general/keyboard-shortcuts/)

## Renumber cues around occupied numbers

1. In a rehearsal workspace, create three unnumbered Memo cues in one list. Leave a separate cue numbered `11` in that list and another numbered `13` in a different list or cart.
2. Select the three unnumbered cues. Choose **Action → Renumber Selected…**. Enter `10` for **Start at** and `1` for **Increment**. The preview shows `10`, `12` and `14` in cue-list order.
3. Choose **Renumber**, then check that the reserved cues still use `11` and `13`. If the workspace changes while the preview is open and the plan differs, review the refreshed numbers before applying. One **Undo** restores all three original numbers.

The selection can include cues inside a group. The desk keeps other cue numbers across the workspace and retains cue links by cue ID. An external controller that sends cue numbers will need the updated numbers for the cues you changed. [QLab Cue Numbering](https://qlab.app/docs/v5/tools/tools-menu/#cue-numbering)

## Call a rehearsal cue by keyboard

A keyboard trigger starts the addressed cue in the active list. A sequential list follows its usual playhead and continue rules. An independent cart fires its addressed pad. An assignment can use **0–9**, a letter other than **G**, **P**, or **S**, or a function key from the picker. Hold **Shift** for a separate binding with letters and function keys. The assignment follows the key reported by the current keyboard layout.

1. In a practice copy, add two five-second Wait cues. Set each to **Do Not Continue**. On the second cue, open **Triggers → Keyboard trigger** and choose **Shift+J**. Save the show.
2. Click the first cue row to give the list keyboard focus. Press **Shift+J**. Watch the second Wait start. Holding the keys should produce one start. Type the same letter into a cue name or the search field and check that it only edits text.
3. Disarm the second cue and try the key again with the list focused. Both Wait cues should stay idle after the previous run finishes. Re-arm it, reopen the saved show, and repeat with focus in Show Mode. On an independent cart, focus a pad in **Trigger pads** mode.
4. Try assigning **Shift+J** to the other cue in the same list. The inspector identifies the existing assignment. To move it, choose **Unassigned** on its original cue first.
5. If your keypad or button controller sends keyboard events, set one button to **F13**, capture it in the inspector, then press it once with the cue list focused. The picker includes **F2–F4**, **F6–F8**, and **F13–F24**. Rehearse with the controller attached to the performance machine.

Assign keys to top-level cues or to direct children of a top-level Sequential group or Playlist. To start a Simultaneous or Start Random group, assign its group row. Moving a bound cue into an ineligible position leaves its assignment inactive. Keyboard triggers work in the focused main desk. Recreate QLab hotkeys in the Inkue inspector after import. [QLab 5 Hotkey Trigger](https://qlab.app/docs/v5/fundamentals/inspector/#hotkey-trigger)

### Record a number-pad cue trigger

1. Put two silent Wait cues on a practice cart. Select the first pad in **Edit layout**, open **Triggers**, and press **Capture key**. Press **4** on the top row or on a number pad with **Num Lock** on. The inspector records the key and the cart stays idle.
2. Switch to **Trigger pads** and focus the second pad. Press **4** to run the first pad. Hold the key briefly to check that there is only one start. Both number keys share the same assignment on a list.
3. Stop playback, return to the first cue, and clear the key with **Unassigned**. Undo restores the assignment and keeps the current standby and armed wall-clock schedules. To leave Capture without an edit, tab away. **Escape** remains the desk stop shortcut. [QLab Hotkey Trigger](https://qlab.app/docs/v5/fundamentals/inspector/#hotkey-trigger)

## Call a cue from the local clock

1. In a practice show, put a five-second Wait on standby and add a second five-second Wait with **Do Not Continue**. Select the second Wait, open **Triggers → Wall clock trigger**, choose a local time about a minute ahead and leave today’s weekday selected.
2. Choose **Save schedule**, then **Arm for this session**. At the scheduled time, the second Wait should start and the manual playhead should stay on the first Wait. Keep the desk awake and check the system clock before arming.
3. Choose **Hard Stop All** and confirm the schedule reads **Saved, disarmed**. Save and reopen the workspace. The time and weekdays are saved with the show. Arming is local to the open show and needs a fresh operator action after opening, recovery, or undo on that list.

The desk schedules top-level manual cues and independent cart pads that have no dependency on the playhead. Ordered groups, nested cues, crossfade entrances, Start and Goto require another trigger. Rebuild wall clock schedules after QLab import and test each with the venue’s time zone and system clock. [QLab Wall Clock Trigger](https://qlab.app/docs/v5/fundamentals/inspector/#wall-clock-trigger)

Each cue and scheduled time starts at most once per local calendar date. Large clock jumps, missed polling windows after sleep, and times skipped at the daylight-saving transition yield idle output. During a repeated local hour, the first observed occurrence consumes that cue’s scheduled time for the date. Editing a schedule withdraws its arming. Continuing rules edited during scheduled playback apply to a later manual run. Hard Stop All withdraws arming from every list in the workspace.

## Find a recent workspace

Open **View → Workspace Launcher…** in programming mode. Search by show name or folder, then choose the matching row. Press **Enter** when the search has one match. Use **Browse…** to open a show from another folder, **New Workspace** to begin an empty show, or **New from Template…** to start from saved programming. Inkue asks what to do with unsaved edits before replacing the workspace. [Inkue template exercise](../settings/README.md) · [QLab Launcher workflow](https://qlab.app/docs/v5/tools/launcher-window/)

## Navigate and edit a group

1. In an unsaved practice copy, select adjacent rows using **Shift+click**. For separated rows, use **Ctrl+click** on Linux or **Command+click** on macOS. Open the row context menu and choose the **Group** action showing the selected cue count.
2. Focus the group row. Press **Right** to open it, then **Right** again to move to its first child. **Up** and **Down** visit the currently visible rows in show order. **Left** moves from a child to its parent or closes the focused open group.
3. Hold **Shift** with **Up** or **Down** to extend and shrink a range. **Home** and **End** move to the first and last visible rows. Keyboard selection leaves the playhead in place.
4. Double-click the pre-wait or post-wait cell to edit it. Wait and Fade cues also support duration editing. Right-click a column header to show or hide columns. These edits affect the running document, so use a copy for rehearsal changes you intend to discard.
5. Select two direct children of a top-level group and choose **Remove from Group**. They appear below their group in their original order. Undo restores both children in one step. If a selected cue no longer belongs to that group when the request arrives, the desk leaves the group and playback alone.

To rehearse disarming, put two five-second Wait cues in a Simultaneous group. Disarm the first child and GO the group. Only the second Wait should run. In a sequential list, a timecode mark or MIDI trigger addressed to a disarmed cue should leave the playhead and the following cue alone. A manual GO from a disarmed row advances to the next armed row.

To reuse a sequence, put a quiet Audio cue and a Fade targeting it in one practice group. Copy and paste the group, open the copy and select the copied Fade. Its target is the copied Audio cue. Give the two Audio cues distinct names to check the assignment, then try the copied sequence at rehearsal level. A target outside the copied group retains its original assignment. **Duplicate Selection** also carries links between selected cues into the duplicated set.

### Program a Load cue for a repeatable rehearsal entrance

1. In a rehearsal copy, find a File Audio cue with a clear sound about three seconds into its saved clip. Use one pass at normal speed, zero Pre-Wait, and **Do not continue**. Allow the file to finish loading.
2. Add a **Load** cue before it. Set the Load cue to zero Pre-Wait and **Do not continue**, enable **Load at a specified time**, and choose the File Audio cue as its only target. Enter **3** in seconds from the clip start.
3. Give GO on the Load cue. Check that the Audio row shows the prepared time while the speakers stay silent. Give GO on the Audio cue and confirm the sound begins at the rehearsal entrance. Stop and start the Audio cue once more to hear its saved clip start restored.
4. Try **-1** to prepare the last second of the clip. An out-of-range offset holds standby at the Load cue and leaves any earlier valid preparation in place. Clear **Load at a specified time** to use the usual preparation at the saved entrance.

Imported QLab Load cues with a nonzero Load Time arrive disarmed. Compare the retained time and the cue target with the original before arming a rehearsal copy.

When the Load cue is immediately followed by the target Audio cue, use **Auto-continue** with zero Post-Wait on the Load cue to prepare and start the entrance with one GO. Keep **Do not continue** for a separate standby and playback call.

[QLab Load cue settings](https://qlab.app/docs/v5/other-cues/transport-cues/#load-cues)

### Place entrances on a Group Timeline

1. In a rehearsal copy, put two four-second Wait cues named **Call A** and **Call B** inside a **Simultaneous** group. Leave both Pre-Waits at zero. Select the group and open **Inspector → Timeline**.
2. Drag Call B’s entrance marker to the right. Its bar moves with it. Release to save, then use **Undo** once to return it to zero. Focus Call B’s **Pre-Wait** field, enter **1.250** seconds, and press **Enter** for an exact entrance. **Escape** cancels a keyboard edit.
3. Set the playhead on the group and give **GO**. Call A’s countdown starts first. Call B starts 1.25 seconds into the group. Stop the group before editing again. Save and reopen the rehearsal to confirm Call B’s Pre-Wait.

The Timeline displays each direct child in cue-list order. An entrance marker shows the child’s Pre-Wait and a bar shows its known duration. A row labeled **No timed entrance** is view-only. Put cue-targeted Fades in the main list, where the transport can resolve their destinations. Undo and Redo restore only the edited Pre-Wait and preserve the current standby and wall-clock arming. Imported QLab Timeline groups open in Inkue as Simultaneous groups. [QLab Timeline tab](https://qlab.app/docs/v5/fundamentals/group-cues/#the-timeline-tab)

To align two cues, drag an entrance near the start or visible end of another armed cue. A guide appears across the lanes and shows the resulting time. Hold **Ctrl** on Linux or **Command** on macOS while dragging to position the entrance freely. Type a value in the Pre-Wait field for millisecond precision.

With the Pre-Wait field focused, press **Alt+Left** or **Alt+Right** to move the entrance by 100 ms. Hold **Shift** as well for 10 ms steps. Press **Enter** to save the adjustment as one edit or **Escape** to discard it.

#### Inspect a late entrance at higher magnification

1. In the same group, set **Call B** to **6.250** seconds. Press **+** twice above the Timeline. Scroll horizontally until Call B’s marker is in view. You can also focus the ruler and tracks with **Tab**, then pan with **Left** and **Right** or jump with **Home** and **End**.
2. Drag Call B’s marker a short distance. Its Pre-Wait field shows the resulting entrance. Use **Undo** to restore **6.250** seconds. The Timeline stays zoomed while you review the edit.
3. Press **Fit** to see the whole group again. You can also use **Ctrl+Equals** and **Ctrl+Minus** to zoom, and **Ctrl+0** to fit, while focus is in the Timeline. On macOS, use **Command**. Zoom and scroll are view controls. [QLab Timeline controls](https://qlab.app/docs/v5/fundamentals/group-cues/#the-timeline-tab)

In a rehearsal group with several direct Wait children, find an early timing reference and choose **Pin** on its Timeline lane. Scroll down to a later cue and compare its entrance against the reference using the shared ruler. Choose **Pin** on another row to replace the reference, or press the active **Pin** button again to clear it. The pin affects only the current Timeline view. The cue list keeps its authored order. [QLab Timeline pinning](https://qlab.app/docs/v5/fundamentals/group-cues/#pin)

Run the group with Timeline open. The vertical line follows the time since the group’s action began, so you can see when it crosses each programmed entrance. Pause to hold the line and resume to continue from the same point. Stop the group to edit its entrances again.

To align a cue with an Audio or Video slice, add the sliced media and a Wait as direct children of a Simultaneous group. Give the media a known clip end, then open **Timeline → Slice guides**. Drag the Wait entrance to a dotted marker. Inkue projects each boundary from the cue’s saved clip window, rate and finite repeat counts. A vamp keeps later times open until it releases, so guides stop at its entrance. [QLab Timeline](https://qlab.app/docs/v5/fundamentals/group-cues/#the-timeline-tab)

### Rotate through alternate cues with Start Random

1. In a practice show, put three short Wait cues in a Group, name them **A**, **B**, and **C**, and add a Memo after the Group. Set the Group mode to **Start Random** and turn on **Advance playhead after GO**.
2. Set the playhead on the Group and press **GO**. Check which Wait started and confirm that the Memo is standing by. Let the Wait finish, then use **Set Playhead Here** on the Group and try again. Across three starts, each Wait runs once.
3. Run the Group again and press **Pause** while the Wait is running. Set the playhead on the Group and press **GO** again. The first Wait stays paused while another available child starts. A paused child that has yet to run in the current rotation remains eligible once it is available.

New Groups and converted QLab 4 or 5 Start Random groups advance standby by default. Existing Inkue Start Random groups retain their saved standby behavior. **Advance playhead after GO** lets you choose either behavior. Reopening the workspace starts a fresh rotation. [QLab 5 Start Random](https://qlab.app/docs/v5/fundamentals/group-cues/#start-random) · [QLab 4 Group Cues](https://qlab.app/docs/v4/general/group-cues/)

## Find a cue by its content

1. In a practice list, add **Understudy entrance** to one cue’s Notes. Put a Text cue with the same phrase inside a collapsed Group. Set the playhead on a different cue.
2. Choose **Edit → Find…** or press **Ctrl+F** on Linux or **Command+F** on macOS. Type `understudy`. Use **Enter** and **Shift+Enter** to visit the matches. The Group opens when you select the Text cue. **Select Found** selects all matching cues for a batch edit. The playhead stays on standby.
3. Choose **Done** or press **Escape** in Find to clear the query and return focus to the cue desk. Switch lists or carts to search another part of the show.

Find checks cue names, numbers, notes, media filenames, Memo and Text content, Network messages, and Script commands and arguments in the active list or cart. It reads cue programming while leaving playback alone. [QLab Find](https://qlab.app/docs/v5/tools/find/)

## Flag cues for review

1. Select a cue and choose **Inspector → Basics → Flag for review**. A solid flag appears beside the cue name or on its cart pad. A hollow flag on a group points to flagged cues inside it.
2. Open **File → Check Workspace**. Find the cue with the warning **Flagged for operator review**. The flag leaves the cue's timing, routing, and armed state as programmed.
3. After you review the cue, clear **Flag for review**. Save the workspace to retain the remaining flags. With playback stopped, move standby to another cue and Undo the flag edit. Redo applies the flag edit again. Both leave standby and any armed wall-clock triggers in place.
4. After importing a QLab 4 or 5 show, compare the marks with the original cue list. Copying a flagged cue or group in Inkue carries its flags to the new cues.

[QLab reference on cue flags](https://qlab.app/docs/v5/fundamentals/inspector/#flagged)

## Prepare a cue with Load

1. Add a Video or Image cue and assign a rehearsal file. Add a **Load** cue that targets it, then add a **Start** cue with the same target.
2. Park the playhead at **Load** and press **GO**. The file prepares with its picture held off the output. The next **GO** runs **Start** and reveals the prepared picture.
3. Try the same setup with a Text cue. **Load** leaves the text and any current overlay alone. **Start** displays the text.

Load prepares Video and Image targets off-screen. Audio uses decoded file data and can hold a silent voice on the main output when no Output Patch applies. Patched audio uses the current routing when started. A **Start** cue honors the target's Pre-Wait. Load also prepares MIDI File data without opening the MIDI output. A Load targeting a Group leaves its children untouched. Camera, Mic, Text, Light, message, Script and other control cues perform their actions only when started. By default, Load prepares from the cue's programmed beginning. [QLab Load cue reference](https://qlab.app/docs/v5/other-cues/transport-cues/#the-load-cue)

## Rehearse from partway through an Audio cue

1. Select an armed, top-level File Audio cue in standby. Choose a recording with an audible change partway through, such as an announcement followed by music. Set its rate to `1`, switch off looping, remove any slices and leave Pre-Wait at zero and Continue at **Do Not Continue**.
2. In **Inspector → Time → Load to Time**, enter `5.000` and click **Load**. The row shows **Loaded** and the desk stays silent. Time is relative to the saved clip start. Enter `-2.000` to load two seconds back from the saved clip end.
3. Press **GO** on the Audio cue or run a **Start** cue that targets it. The sound and progress begin at the loaded point using the cue's current Output Patch. Stop it and start it again to hear the saved opening.
4. Load a time again and choose **Clear**. A Stop, Reset, or Stop All also clears the offset. Try changing the clip boundaries after loading. If the saved window changed, load the new time or clear it before using GO.

Load to Time temporarily prepares only the selected cue. DSP starts with the chosen audio frame, so effects that depend on earlier audio have a fresh history. To save a repeatable File Audio entrance in the show, use a [programmed Load cue](#program-a-load-cue-for-a-repeatable-rehearsal-entrance). [QLab Load to Time](https://qlab.app/docs/v5/tools/tools-menu/#load-to-time)

## Rehearse from partway through a Video cue

1. Choose an armed, top-level Video cue with a visible cut and an audible marker. Let the desk finish checking the file. Use one pass with zero Pre-Wait and **Do Not Continue**. Turn off slices, **Silent Movie Speed**, and **Hold Last Frame**.
2. In **Inspector → Time → Load to Time**, enter `3.000` and click **Load**. Confirm the cue shows **Loaded** and the existing stage picture and audio stay as they were. The offset is relative to the saved clip start. Use `-2.000` to count back from its end.
3. Start the Video cue. The desk waits for the positioned frame before revealing the picture and releasing its soundtrack. Check both against the intended beat, then stop and start the cue again to verify the normal opening.
4. For a repeatable rehearsal, add a **Load** cue, enable **Load at a specified time** and choose the Video cue as its target. Save the show, then run the Load and the target as separate calls. A change to the clip boundaries requires a fresh Load or **Clear**.

The staged entrance applies to one start. Group and cue-sequence preparation remain open. [QLab Load to Time](https://qlab.app/docs/v5/tools/tools-menu/#load-to-time)

## Mark cues that have played

1. Add a five-second Wait cue in a rehearsal copy. In **Inspector → Basics**, give it a Gray base color and a Midnight **Second color**. Set **Pre-Wait** to two seconds.
2. Press **GO**. The row stays Gray through Pre-Wait and changes to Midnight when the countdown starts. Stop the cue after it changes color. The Midnight highlight stays visible through Stop and normal completion.
3. Choose **View → Reset played cue colors** to return every cue in the open workspace to its base color. This leaves playback and the playhead where they are. A programmed Reset cue clears played colors on its target and any children.
4. Try **No highlight** as the second color to remove the cue’s custom highlight after it starts. Choose **Keep original color** to disable the change. Save and reopen the show to see the base color again with the second color still programmed.

QLab 5.2 and later archives can carry a named palette color and an enabled second color. Check those colors in the imported desk or cart during rehearsal. [QLab Inspector second color](https://qlab.app/docs/v5/fundamentals/inspector/#second-color)

## Paste selected settings onto existing cues

1. In a rehearsal copy, create an Audio cue with a quiet file, **Volume** at −18 dB and a pre-wait of 400 ms. Create a second Audio cue and a Video cue with different file targets. Give each destination a recognizable cue number.
2. Select the first Audio cue and press **Ctrl+C** on Linux or **Command+C** on macOS. You can also choose **Copy Cue** in its row menu. Select the two destinations and choose **Paste Cue Properties…** from the row menu, or press **Ctrl+Shift+V** or **Command+Shift+V**. Select **Main audio level** and **Pre-wait and post-wait**, then choose **Apply**.
3. Check both destinations for −18 dB and the 400 ms pre-wait. Check their original files, names, numbers, outputs and continuation settings. Use **Undo** once to restore both cues. Start a Wait cue and try the paste again. Stop the Wait before applying the changes.

The dialog offers property groups common to the copied cue and every destination. Audio-to-Audio adds stereo pan and playback rate. Video-to-Video adds picture geometry, layer settings and picture fades. Wait until the transport is idle and unload any cue that is holding an output. Cue targets, files, assigned patches, trigger assignments and trims stay with the destination. QLab's complete property set, presets and Fade-target special commands remain open. [QLab Paste Cue Properties](https://reference.qlab.app/docs/v5/tools/paste-cue-properties/)

### Reuse a crosspoint matrix

1. In one practice workspace, assign two stereo file cues to the same named Output Patch. Use a patch without active output effects. Wait for both files to finish loading. In the source Audio cue’s **Levels** tab, choose **Use a matrix**. Send input 1 to output 1 at −9 dB and input 2 to output 2 at −15 dB. Put the destination cue at a different Volume so you can check the two controls separately.
2. Copy the source. Select the destination and open **Paste Cue Properties…**. Check **Audio crosspoint routing** and apply. Inspect the destination’s matrix, original Volume and saved patch assignment. Listen quietly to confirm that its file’s input channels carry the expected material in the expected order.
3. Undo. Copy the source again, then swap the device-channel order in the shared patch before opening the paste dialog. **Audio crosspoint routing** is now unavailable for that copy. Restore the intended patch and copy again before pasting. Changing the channel order or **Require named device for Audio GO** after opening the dialog rejects a crosspoint paste when you apply it.

Audio and Video cues with decoded audio can share the matrix and mono routing mode on a standard named patch when their input counts and ordered patch assignments match. Standard patches retain their configured fallback behavior. If the patch requires its named device, every participant must be a File Audio cue and the patch must support that requirement. The paste checks programming, and admission is confirmed at GO. This option is available in the same open workspace for a saved matrix from the Levels tab. Cues using an applied point panner keep their calibrated routes. [Required Audio GO exercise](../audio/README.md#require-the-venue-output-for-an-audio-go) · [QLab Cue Levels Matrix Mixer](https://reference.qlab.app/docs/v5/audio/audio-cues/#the-cue-levels-matrix-mixer)

### Reuse start and end markers on replacement audio

1. In a practice copy, assign an Audio cue a file longer than two seconds. Open its Time tab and set the clip to start at 0.4 seconds and end at 1.6 seconds. In two other Audio cues, load alternate recordings that reach the same marks. Let the files finish loading and stop playback.
2. Copy the first cue, select the two destinations, and open **Paste Cue Properties…**. Choose **Audio clip start and end**. Apply and check the destination markers and the original file assignments. Listen to the alternate takes at rehearsal level and check the duration of any existing loops or follows. One Undo restores both clip windows.
3. Assign one destination a file shorter than 1.6 seconds, wait for it to load, and open the dialog again. The clip window is unavailable for that selection. If a replacement file becomes too short after the dialog opens, Apply rejects the selected paste. Review or edit that cue separately.

Start and end are absolute times measured from the start of each file. An unset start or end uses that destination file’s beginning or end. This group supports Audio cues with loaded media and destinations without slices. The destination keeps its media, loop counts and fades. [QLab Audio start and end times](https://qlab.app/docs/v5/audio/audio-cues/#start-time-and-end-time)

### Reuse timing and curves on Fade cues

1. In a stopped practice show, set a Fade cue to four seconds. In the **Curve** section of its **Fade** tab, unlock the curves and give rising and falling values different shapes. Create two other Fade cues with different targets and programmed values.
2. Copy the first Fade. Select both destination Fades and open **Paste Cue Properties…**. Choose **Fade curve shapes** and apply. Compare both destinations’ rising and falling shapes with the source.
3. Paste **Fade duration** separately to give both destinations the same four-second action. **Undo** once to restore their durations, then again to restore their curves.

The curve choice carries both shapes, their control points and bends, and the lock setting. A selection with a position waypoint cannot accept a zero-second duration. Each Fade keeps its destination values, paths and **Stop targets when the fade ends** setting. [QLab Tools for Fade Cues](https://qlab.app/docs/v5/tools/tools-menu/#tools-for-fade-cues)

## Program and operate a cart

1. Open **Sound checks**, the cart in the rehearsal show. Select **Edit pads**. Click **S1** to select it and inspect its settings. With a pad focused, **Enter** or **Space** selects it in this mode. Arrow keys, **Home** and **End** move focus between available pads. Moving focus sends no playback command.
2. In a compact cart, use the separate **Fire** button to hear the selected sound on the configured show output. The active pad exposes its own **Stop** control. Keep output levels appropriate for the room.
3. Select **Trigger pads** to return to single-action playback. Click a pad, or focus a pad and press **Enter** or **Space**, to fire it. Holding an activation key suppresses repeats. A disarmed or loading pad is unavailable for triggering.
4. Drag a pad to reorder it. To build another cart, use **+** in the list tab strip, then right-click the new list tab and choose **Switch to Cart Mode**.

New carts and QLab-imported carts open as **Independent pads**. Their pads ignore Auto-Continue and Auto-Follow, including post-waits. A pad can still explicitly start, stop or fade its targets. The cart has no next-GO playhead.

An older saved Inkue cart opens with **Legacy chained pads** shown above the grid. To migrate it, save a rehearsal copy, right-click its tab, choose **Use independent pads** and run through any pads that used to start a sequence. To restore the old behavior, choose **Use legacy chained pads**. Save the show to remember your choice. [QLab list and cart mode values](https://qlab.app/docs/v5/scripting/osc-dictionary-v5/)

## Lay out a cart for the operator’s hands

1. Start with a copy of the show. Open a cart and select **Edit pads**. Choose **Rows** and **Columns**, then **Use fixed grid**. For this drill use 2 rows and 3 columns with two cues. Each dimension supports 1 through 64. Inkue fills the first available cells in cue order. Your saved compact carts keep their original layout until you choose a fixed grid.
2. Select the first pad, choose **Move selected pad**, then choose **Row 2, column 3**. The first cell becomes empty. Repeat with an occupied destination to swap two pads. Press **Escape** or choose **Cancel placement** to leave the pads where they are. You can also drag a pad in **Edit pads**. Selecting, placing and resizing send no GO.
3. Choose **Trigger pads** or enter **Show Mode**. Check that the vacant cells occupy the same physical positions. With a pad focused, use the arrow keys to visit another pad in its row or column. Narrow the window and confirm that pads keep their assigned column. Fire the bottom-right pad and identify the output.
4. In **Edit pads**, set the grid to one row and choose **Resize grid**. A pad beyond that row appears under **Outside grid**. Restore the second row and verify it returns to its original cell. A new cue added to a full grid also appears in **Outside grid**. Select it and use **Move selected pad** to exchange it with a visible pad.
5. Save and reopen the show. Verify the two pad positions and the empty cell with the operator. **Use compact layout** returns the cart to responsive cue order. **Undo** restores the previous layout in this editing session. Undoing a layout edit also restores its saved cue-list snapshot, so use it while programming.

A QLab import reads saved row and column counts and the cart’s cell-to-cue references where available. On a source cart with missing assignments, review **Outside grid** for unplaced cues. Compare the imported grid with the original cart before the first GO. Use the separate-window drill below for a second operator display. For a private stereo clip sound check, use the [headphone audition exercise](../audio/README.md). [QLab cue carts](https://qlab.app/docs/v5/fundamentals/cue-carts/)

## Monitor the show from the desk

1. In a practice copy, create two sequential cue lists named `Calls` and `Safety checks`. In each list add two silent Wait cues, set the first to 60 seconds, and leave **Do Not Continue** selected. Park each list's playhead on its first cue.
2. In **Calls**, start the first Wait from the main transport. Select **Cue monitor** in the transport bar. Check the Standby row and the running cue's elapsed and remaining times. Switch the desk to **Safety checks** and start its first Wait. The two lists and their active cues should be visible together.
3. Pause and resume the Wait in the selected list from the main transport. Check that the monitor reports **Paused** and holds the sampled timer until playback resumes. A dash means that the timer sample or the remaining duration is unavailable. Runtime alerts appear below the list status.
4. Choose **Close** to dismiss the monitor and return focus to its transport button. Open it in Show Mode. With the monitor focused, test **Escape** for Stop and two quick presses for Hard Stop on rehearsal cues.

The in-desk Cue monitor displays every list's current playhead and active cues. A disarmed row can be at the playhead, so check **Next GO** in the main desk when calling a cue. Use the main desk for transport and use the [second-display monitor](../automation/README.md#watch-the-desk-from-a-second-display) for a separate status view. [QLab read-only monitoring](https://qlab.app/docs/v5/networking/qlab-remote/#read-only-mode-free)

In programming mode, click or tab to an underlined cue name in the selected list. Activate it to close Cue monitor, reveal the cue row and open its Inspector. Try this with a running child in a collapsed group. The selected cue changes while playback and standby stay in place. To inspect a cue in a different list, select that list on the desk first. Show Mode displays the status without Inspector links.

Select **Audio map** to inspect a saved calibration and the assigned main-device output levels in either mode. [Audio map exercise](../audio/README.md#check-an-audio-map-from-show-mode)

## Run a second cue list in its own window

1. Put two silent Wait cues in a second sequential list. Keep the first list selected on the main desk. Right-click the second list's tab and choose **Open cue list in separate window**. Move it to the second display. In Show Mode, choose **Open cue list window** for the list on screen.
2. Choose **Set standby** beside an armed cue in the separate window. Confirm that its **Next standby** reflects the selection and that the main desk still shows the first list. Press **GO**, or focus the cue table and press **Space**.
3. Watch **Remaining** on the running row. Practice **Pause list** and **Resume list** on the countdown. Start a rehearsal Wait on the main desk and choose **Stop list** in the separate window. **Escape** in that window also stops its list. Check both running states in **Cue monitor**.
4. Close the separate window while its list is running to rehearse handoff. Reopen it to see the current standby. Close and reopen the workspace to start a fresh window session.

The separate window uses the desk's configured Double GO interval. Use the main desk to edit programming. If standby advances on the desk or through a controller before the window's GO arrives, check the refreshed standby and give a fresh GO. [QLab cue list windows](https://reference.qlab.app/docs/v5/fundamentals/cue-lists/)

## Put a cart on a second display

1. In the bundled rehearsal workspace, leave **Rehearsal** active and park its main playhead on cue 1. Right-click the **Sound checks** tab and choose **Open cart in separate window**. Right-clicking the tab lets you open the cart while the main desk stays on Rehearsal. In Show Mode, **Open cart window** is also available when an independent cart is active.
2. Move the new window to the second display. Opening the same cart again brings its existing window forward. You can open a separate window for each independent cart. An older chained cart needs **Use independent pads** on its tab before it can open separately. Rehearse its cue sequences before changing that setting.
3. Focus **S1 · Check tone** in the new window. For a fixed grid, use the arrow keys to move among occupied pads. **Home** and **End** visit the first and last available pads. Navigation sends no GO. Click the intended pad or press **Space** or **Enter** while it has focus. Confirm the short tone plays and the main desk still shows Rehearsal cue 1 as its next GO. In a shared rehearsal show, give a cue in the main list the same number as a cart pad and verify that only the cart pad runs.
4. While a longer pad is running, use its **Stop** button to stop that cue. Close the cart window with the window manager to release the display. Closing the window leaves playback under operator control. Change a pad's name or position from the main programming desk and reopen the cart to verify the saved label and cell. A running window also reflects programming changes.
5. On a practice copy, open a cart window, close the workspace and reopen the same file. The old cart window closes when its source document is replaced. Open a fresh window from the new workspace. Record the two display assignments and the caller’s starting cue on the handoff sheet. [QLab cue cart windows](https://qlab.app/docs/v5/fundamentals/cue-carts/)
