# Rehearse an audio route

A QLab crosspoint connects one file input to one cue output. In Inkue, open **Levels** on an Audio cue or a Video cue with a soundtrack. The matrix uses the same row and column convention. The **HW** number under each output tells you which channel on the playback device receives that output. [QLab Audio cues](https://qlab.app/docs/v5/audio/audio-cues/) · [QLab Audio Output Patch Editor](https://qlab.app/docs/v5/audio/audio-output-patch-editor/)

## Send a stem to a backstage monitor

Suppose channel 3 of a six-channel file contains a backstage call. Your output patch maps its first two outputs to channels 7 and 8 on the venue interface. You want the call on hardware channel 8.

1. In **Preferences → Audio → Output Patches**, choose the venue device and assign channels 7 and 8 to the patch in that order. Give the patch a name the operator recognizes. Expand **Channel names** and enter **Backstage** for **Out 2 · HW 8**. Press **Enter** to save the label.
2. Select the Audio cue and open **Levels**. Set **Output Patch** to the named patch. If a cue uses **Default patch**, check which patch is marked as the workspace default.
3. Select **Use a matrix** if the cue currently uses pan. A new matrix starts with two unity routes, input 1 to output 1 and input 2 to output 2. To isolate the call in this example, set those two cells to `-inf` and commit each value with **Enter**.
4. At input **3**, enter the desired level under **Out 2**. Check for **Backstage** and **HW 8** together in that column header. Renaming a logical output leaves its routing and playing streams in place. Start at a low listening level during the sound check. A value of **0 dB** passes the source at unity gain before cue volume and patch gain.
5. Fire the cue in rehearsal. Check the venue interface meter and listen at the backstage monitor. Stop the cue, save, reopen the show and verify the destination after any device or patch change.

| Matrix location | What it controls |
| --- | --- |
| Input 3 | Third channel encoded in the source media |
| Out 2 | Second output in the selected patch |
| HW 8 | Eighth output on the playback device |

A device must expose the physical channels used by the show. A stereo file carries two source channels even when the inspector displays higher input numbers. With a patch mapping, only its assigned outputs reach hardware. For example, with two assigned outputs, Out 3 and higher display **Unassigned** and remain silent. With no channel mapping, Out 3 addresses hardware channel 3 if the device exposes it.

## Require the venue output for an Audio GO

Use **Require named device for Audio GO** when a cue has to start on a particular interface, such as a backstage call on a dedicated output. A patch with this option holds a new file Audio cue if the device cannot be confirmed or if its channels are unavailable. Standard patches can fall back to the main output when a patch device cannot open.

1. On Linux, start Inkue with `INKUE_AUDIO_HOST=alsa` and use a direct ALSA hardware interface that Inkue can open. If the interface is your main output, select it by name in **Preferences → Audio**. In **Output Patches**, choose the interface, assign its channels and enable **Require named device for Audio GO**.
2. Assign a quiet test Audio cue to the patch. Run **Check Workspace**, then GO and confirm the signal on the interface’s meter. Stop the cue.
3. With playback stopped and the venue output at a low level, disconnect the test interface. Check the workspace again and try GO. The cue should stay ready for retry without sending the file to the main output.
4. Reconnect the interface, confirm its channels in Audio Preferences and run **Check Workspace** again. Retry GO and verify the hardware signal. Save the workspace and check that the option persists after reopening.

An Audio cue with Pre-Wait confirms its required output at GO and checks it again before playback. Try the same exercise with a five-second Pre-Wait. If the route is changed or unavailable when the wait ends, the cue returns to Standby without chaining. When an active child loses its required output, an ordered Group parks that child for retry if it still drives the sequence. Other children that already started continue on their assigned routes.

An active cue keeps the patch and physical channels it started on. Stop or unload it before editing its matrix against a changed default or reordered required patch.

An Inkue audio engine reopen withdraws active file Audio cues that used this setting. Strict GO on Linux requires a direct ALSA `hw:` output. PipeWire’s session manager can move an already-open stream when a sink disappears. File Audio cues on a PipeWire patch with this option stay held until you choose a supported device or clear the option. Video soundtracks, Mic and Timecode use their current routing rules. [QLab audio patch warnings](https://qlab.app/docs/v5/audio/audio-output-patch-editor/#audio-output-patch-warnings)

## Filter the summed output of a patch

1. Select a named hardware main output in **Preferences → Audio**. On Linux use a direct ALSA `hw:` output. Add an Output Patch named **Backstage Filter** for that same device, assign its channels, and enable **Require named device for Audio GO**.
2. Expand **Output FX**, add a high-pass filter, and set it to **600 Hz**. The editor also offers low-pass filters and peaking EQ. Effects run in the order shown.
3. At a low listening level, play a clip with bass and treble through **Backstage Filter**. Compare it with the same clip on a dry patch reaching the same hardware channel. Start two Audio cues on **Backstage Filter** together and compare the combined output with the dry patch still playing. The filter works on the sum of the two filtered cues.
4. Stop the patch’s file Audio cues before editing the chain. Save and reopen the workspace to verify the settings. To check routing loss, disconnect the interface with playback stopped and try GO. The processed cue should stay held.

This workflow supports eight native effects and one processed patch playing at a time on the named main stream, up to 32 hardware channels and 4,096 frames per audio block. Cue effects run before the patch routing. Output FX runs after the patch’s file Audio is summed by hardware channel, before the device master. Use a dry patch and a dedicated hardware channel for LTC. Video soundtracks, Mic and LTC keep their dry paths. Third-party plug-ins and cue-output patch effects require separate support. [QLab Audio Output Patch Editor](https://qlab.app/docs/v5/audio/audio-output-patch-editor/)

## Start two stems from the same sample boundary

1. Export two stems from the same timeline. For this exercise, put a short click at the same source frame in each stem and use the same sample rate for both files.
2. Add a **Simultaneous** Group with both Audio cues. Leave both child pre-waits at zero and assign both cues to the same output device. Route the first click to one physical output and the second to another.
3. Prepare both cues and GO the group. Capture the two outputs together on an interface with a common clock. Compare the first transients in the recording. Repeat several times from a stopped group.

Immediate file starts in a Simultaneous Group share a mixer block on the same output device, including through an immediate nested Simultaneous Group. Separate GO actions, staggered child pre-waits, starts across different devices, and synchronization with Video need separate checks. [QLab introduction to audio](https://qlab.app/docs/v5/audio/introduction-to-audio/)

## Check imported output patches

An imported QLab 5 named patch retains its identifier and master gain. One-to-one unity routes can preserve a reordered device channel assignment, such as cue output 1 feeding device channel 7. Open the imported cue’s Levels inspector, match each HW label against the original patch and test one channel at a time. If the report identifies a macOS device assignment, a more complex patch matrix, patch effects or a solo/mute state, referenced cues need review before arming. For QLab 4 numbered device patches, use the preserved patch name to identify and assign the corresponding Linux device.

## Name the channels in a source file

1. Select an Audio cue with a multichannel recording. In **Levels → Source channel names**, enter **Dialogue** for input 3 and press **Enter**. Choose **121–128** in the source channel selector and enter **Choir** for input 128. The labels identify positions in the file. They do not create additional audio channels.
2. With a matrix enabled, select inputs **1–8**. Input 3 should display **Dialogue** beside **3**. Select **121–128** and confirm **Choir** beside **128**. The output columns still show their patch labels and hardware numbers. Names can also be entered when a cue uses pan.
3. Play a long cue in rehearsal. Rename one of its inputs and check that its play position continues, its output stays audible and its active fade follows its existing curve. Repeat on a Video cue with a soundtrack and check that the picture continues.
4. Save, close and reopen the workspace. Confirm both names on the original Audio cue and the independent names on the Video cue. Clear an input's text and press **Enter** to restore its numbered display.

When a QLab import report includes archived source names, compare the original channel order and enter each name on the corresponding input. The decoded names remain in the cue notes.

## Edit a large routing matrix

**Inputs** pages eight rows at a time. **Outputs** pages four columns at a time. Both selectors reach channel 128. For example, to adjust input 22 feeding output 37, choose **17–24** under Inputs and **37–40** under Outputs. Changing pages preserves the rest of the matrix, including routes on other pages. Use **Find routes** to jump to banks that already have signal paths. The number in parentheses counts the active crosspoints in that bank.

Type a dB value, or drag the grip on the right of a cell vertically. Hold **Shift** while dragging for finer adjustment. The playing cue previews valid changes. Press **Enter**, move focus out of the field or release the grip to commit. A silent crosspoint displays **−∞**. Enter `-inf` or `-60` to silence it. **Use pan instead** removes the saved matrix and returns the cue to pan routing.

## Clean up a recorded announcement with cue effects

1. Select an Audio cue containing a spoken track and open **Audio FX**. Add **High-pass** and try 100 Hz to reduce handling noise and sub-bass.
2. Add **Peaking EQ**. If a test recording has a harsh resonance near 1,200 Hz, try a gain of −4 dB and Q of 2. Higher Q narrows the adjustment. Add **Low-pass** if the effect needs to remove high frequencies above a chosen cutoff.
3. Start the cue from the cue list and listen through its assigned output. Stop, toggle **Enabled** for an effect, and start again to compare. Use the arrow buttons to change processing order and save the workspace. Reopen it and confirm the order, settings and bypass states.

The chain supports up to eight inserts per file Audio cue. It processes the routed source channels before levels, pan and patch routing. Filter frequencies are specified from 20 Hz to 20 kHz and capped below Nyquist on lower-rate devices. Cutoff edits to enabled high-pass and low-pass filters and gain edits to enabled peaking EQ apply during playback. Changes to EQ frequency or Q and changes to the chain take effect on the next GO. Compare the source and the saved chain in the clip editor’s headphone audition. Imported cues with an archived audio effect chain start disarmed. Bring settings for existing Audio Units, output effects and automated parameters as part of your migration notes and compare the result with the original show. [QLab Audio FX](https://qlab.app/docs/v5/audio/audio-cues/#the-audio-fx-tab)

## Sweep a filter on a running cue

1. Use a sustained clip with distinct low and high notes. For example, use cue **2** in the Audio FX practice show and give it enough repeats for a rehearsal. In **Audio FX**, enable **High-pass** and set the saved cutoff to 600 Hz.
2. Add a Fade and target only that Audio cue. Under **Audio effect**, assign its High-pass filter, set the destination to 80 Hz and set the Fade time to five seconds. The chosen Fade curve shapes the cutoff in hertz. Use a separate Fade when you need to change cue volume or playback rate.
3. Start the Audio cue, then start the Fade. Listen for the lower note to grow. Pause the Fade halfway, listen for a steady setting, then resume it. Stop the Fade on another pass to hold its current setting. Stop the Audio cue and start it again to hear the saved 600 Hz starting cutoff.
4. For a takeover check, start another Fade on the same filter. It starts from the current cutoff. During that Fade, edit the filter’s cutoff in the Audio cue and listen for your chosen value. The edit takes over and sets the next GO’s starting cutoff.

The filter keeps its assignment when reordered in the chain. Reassign the Fade in its Inspector after deleting or bypassing its filter. Start the Audio cue before the Fade reaches its action. A Fade without an active filter leaves the sound alone. [QLab audio effect fades](https://qlab.app/docs/v5/audio/fading-audio/#the-audio-fx-tab)

## Bring up part of a recording with peaking EQ

1. In a looped File Audio cue with a sustained lead sound and a higher background texture, add an enabled **Peaking EQ** in **Audio FX**. Set its frequency near the lead sound, for example 1,000 Hz, its Q to 2, and its gain to 0 dB. Use a quiet recording with room for the boost.
2. Target that cue from a Fade. In **Audio effect**, select the Peaking EQ and enter **+6 dB** under **Destination gain (dB)**. Set the Fade time to four seconds. Start the Audio cue, then the Fade. Listen for the selected band to grow while the cue’s volume setting stays where you left it.
3. Pause and resume the Fade during the sweep. On another pass, Stop the Fade to hold its current gain. Stop and start the Audio cue again to hear the saved 0 dB starting point. To rehearse a manual takeover, start the Fade again and edit the EQ gain in the target’s **Audio FX** tab. That edit takes over and becomes the next GO’s starting gain.

Each Fade controls one enabled effect on a running File Audio cue. Peaking EQ gain ranges from −12 to +12 dB. Use a second Fade for cue volume. To repurpose an EQ Fade, choose **None** under **Audio effect**, then enable the controls you need. Rehearse EQ fades using an Inkue build with gain automation. [QLab audio effect fades](https://qlab.app/docs/v5/audio/fading-audio/#the-audio-fx-tab)

If the Audio cue is inactive or its assigned EQ is unavailable when the Fade reaches its action, Inkue holds the Fade and its automatic continuation. Check **Health**, start the Audio cue with the effect enabled, and retry the Fade. For a held Group, stop the Group and retry it from the intended position.

## Fade or monitor a live input

For a whole-cue fade, add a **Fade** cue, select its target, enable **Fade volume**, and set **To (dB)** and **Time (s)**. Enable **Stop targets when the fade ends** if the sound should stop at the end. With matrix routing, cue volume scales all routed channels together. A pan fade uses the stereo pan path. Rehearse any imported crosspoint or pan automation against the original cue sheet.

For a **Mic** cue, first assign the physical input device and channels in **Preferences → Audio → Input Patches**. In the cue’s **Mic** tab, select the input patch, choose **Mono** or **Stereo**, then choose the output patch. A saved macOS device name may resolve to a different device on Linux. Confirm the channel and start monitoring at a low output level before raising the cue.

## Hold the underscore and release it on a musical boundary

Use a practice track with three easy-to-hear sections. Cue **M1** should play an introduction, repeat the middle section while an actor speaks, then play the ending after the caller gives **M2**.

1. Add the track as Audio cue **M1**. Leave the whole cue at one play. In **Time**, expand the waveform into the clip editor. Double-click the waveform at the beginning and end of the phrase you want to repeat.
2. Click the middle segment’s play-count badge and enter `inf`. Check that the first and final segments each show one play. Add a **Devamp** cue immediately after M1, number it **M2**, select M1 under **Targets**, and choose **Continue** under **After the current pass**.
3. GO M1 and wait through at least two repeats. GO M2 halfway through the repeating phrase. Listen for the rest of that phrase and one complete ending. Repeat with M2 close to the start of the phrase. The caller controls the request and the music controls the exit point.
4. On an imported show, compare every marker and segment count with QLab. A zero-play segment, a whole-cue repeat combined with slices, or a separate next cue timed to the release boundary needs reconstruction. The affected cue opens disarmed with its review details. Test that the next sound or lighting cue lands on the intended beat before arming it for the show.

The Cookbook examples use loops to hold a conductor’s mix and to trigger independent cues at slice boundaries. A separate cue placed after a Devamp in Inkue needs its own timing decision. Devamp completes when the operator requests the release, while the target finishes its current pass. [QLab 3 audio inspector](https://qlab.app/docs/v3/audio/audio-cues/) · [QLab 4 Devamp settings](https://qlab.app/docs/v4/control/devamp-cues/) · [Mixing Stems](https://qlab.app/cookbook/mixing-stems/) · [Devamps Demystified](https://qlab.app/cookbook/devamps-demystified/)

### Release a whole Audio loop

1. Add a short File Audio cue with a clear last beat. In **Time**, trim the clip if needed, enable **Loop**, and choose infinite looping. Leave the clip free of slices.
2. Add a top-level Devamp cue. In **Release scope**, choose **Whole cue** and select that Audio cue. Set the Devamp’s Pre-Wait to zero and its Continue mode to **Do Not Continue**.
3. Start the Audio cue and let it repeat. Give the Devamp a GO midway through a pass. Listen for the rest of the pass through the saved End Time, followed by silence. Try the request earlier in another pass. A fresh GO of the Audio cue uses its saved looping again.
4. Try the Devamp with the target stopped, paused, or still in Pre-Wait. Return it to running playback before calling the release. For the first rehearsal, use manual continuation on the Audio cue as well.

The release applies to the playing File Audio voice and stops further repeats. It preserves the saved loop count and the fade envelope of the pass already in progress. Whole Video release, automatic start of another cue on the same sample, and automatic import of whole-cue modes remain open. [QLab 5 Devamp settings](https://qlab.app/docs/v5/other-cues/devamp-cues/#slice-or-cue)

## Place a stationary source between speakers

1. Create an Audio cue with a mono file. In **Levels**, select a stereo output patch whose first two logical outputs feed the left and right speakers. For a larger layout, select a patch containing the additional outputs.
2. Open **2D point panner**. Set **File input** to 1. Each speaker has a logical output and two coordinates. X increases to the right and Y increases down the diagram. Coordinates run from 0 to 100. Place output 1 at X 0, Y 50 and output 2 at X 100, Y 50.
3. Set the source to X 50, Y 50. The two readouts should each show about −3.0 dB. Move the draft source to X 0, Y 50. The left readout should show 0.0 dB and the right −∞ dB. Clicking the layout or typing coordinates only updates this preview.
4. With the desk at a suitable rehearsal level, choose **Apply point mix**. An active voice receives the change immediately. The selected input row in the matrix now contains those levels. Other configured input rows retain their routes. When starting from Pan mode, Apply creates a matrix, so add routes for any additional file inputs you intend to hear.
5. Save, close and reopen the workspace. The Audio cue retains its speaker layout, source position and matrix. GO plays the saved matrix through the current patch. A subsequent manual matrix edit clears the saved point preset. **Undo** restores the previous edit.

Custom speakers use planar distance and constant-power panning. Each speaker drives a single logical output, and the layout belongs to that Audio cue.

## Reuse a calibrated audio map

For this exercise, a prop radio is heard from the left of the audience with some spill to the right. A second sound comes from the hallway on the right.

1. Create two Audio cues with mono practice files and assign the same named stereo Output Patch to both. On the first cue, open **Levels → 2D point panner → New map…** and name the map **Studio A**. Select the first calibration mark and rename it **Prop radio**. Set its levels to 0 dB on output 1 and −12 dB on output 2. Rename the second mark **Hallway** and set its levels to −60 dB on output 1 and −6 dB on output 2. Choose **Save map**.
2. On the first cue, choose **Prop radio** under **Place at mark**. Check the two readouts against the calibration. Click midway between the marks and check that output 1 is approximately −3.0 dB and output 2 is approximately −8.0 dB. Return to **Prop radio** and choose **Apply point mix**. Check the first input row in the level matrix.
3. Open the second cue’s panner, choose **Studio A**, place the source at **Hallway** and apply. With an isolated stereo monitor at rehearsal level, GO each cue and listen for the two positions. Saving or previewing a map leaves every cue’s routes as they were. Apply updates the selected input of the selected cue, including its active voice if it is playing.
4. Edit **Studio A** from the second cue and change the Hallway’s right output to −18 dB. Save the map and compare both cues’ matrices. They still use their previous calibration. Apply the revised mix to the second cue, then save and reopen. Confirm the radio stays at 0 and −12 dB and the hallway uses −18 dB on the right. Delete the map from the library and reopen again to verify both mixes survive.

A mark specifies the whole mix at one location. Between marks, Inkue interpolates each output in power with greater weight on the closest marks. Map revisions match the output patch ID, device and ordered channel assignment. If the assignment changes, create a map for that assignment before applying another mix. Verify physical routing on the destination desk after transferring a workspace. The import review calls out QLab object audio for manual reconstruction. Map-owned audio objects, filters, spread, and QLab object-map import remain open. [QLab The Audio Map Editor](https://qlab.app/docs/v5/audio/audio-map-editor/) · [QLab Object Audio Basics](https://qlab.app/docs/v5/tutorials/object-audio-basics/)

### Verify a calibration on the live outputs

1. Use a sustained practice recording at a low level and stop the other cues. Select the interface by name in **Audio Preferences**. Assign its named patch to the Audio cue. If the interface has four outputs, use a practice patch that maps its first two logical outputs to device channels 3 and 4.
2. Place and apply the source at the **Prop radio** mark. GO the Audio cue. Choose **Edit map…** and check **Live device outputs**. Compare the patch labels and device channel numbers with the interface. For a steady test signal, the two readings should be about 12 dB apart with the example mark levels.
3. Change one level in the mark draft. Confirm the physical meters keep showing the currently playing mix. Save the map, select that mark again, and choose **Apply point mix** to hear and meter the revised route. Use the cue’s main level to make a quiet rehearsal adjustment and confirm both outputs follow it.
4. Stop the cue. The readings should fall below −60 dBFS when the device is quiet. Switch the main output in Preferences to another practice device and check that the map displays an unavailable message. Restore the named interface before continuing.

The calibration meters show peak dBFS for the combined signal sent to each assigned channel of the named main device, after master gain. They can reveal routing and digital overload. The venue interface or console provides downstream confirmation. Cue-specific meters and auxiliary device meters remain open. [QLab 5.6 metering changes](https://qlab.app/release-notes/5.6)

## Compensate for a louder file with main trim

1. Add a sustained Audio cue at a quiet rehearsal level. Set its main level to −24 dB in **Levels**. Add a six-second Fade targeting that Audio cue and set the Fade's audio destination to −12 dB.
2. GO the Audio cue, then the Fade. While it is fading, open the Audio cue's **Trim** tab and set **Main trim (dB)** to −6 dB. The sound drops by 6 dB and keeps following the Fade's written curve. When the Fade reaches −12 dB, the cue contributes the equivalent of −18 dB before patch and desk gains.
3. Stop the Audio cue and GO it again. It starts with the saved −24 dB main level and −6 dB trim. Save and reopen the workspace to check that both values persist. Enter 0 dB in **Main trim (dB)** to restore unity trim. To verify a calibrated output, use an isolated practice cue and read the resulting channel peaks in the map editor.

Main trim applies equally to all channels of a file Audio cue. Its range is −60 dB (silence) to +12 dB, with 0 dB as the default. QLab imports still fold archive trim into the imported levels. Object trims, dedicated trim meters, and trim on Mic and Video cues remain open. [QLab Trim](https://reference.qlab.app/docs/v5/audio/audio-cues/#the-trim-tab)

### Rebuild main trim after importing a QLab cue

1. Find **Review QLab main trim** in **Conversion review** or in the imported cue’s notes. Compare the recorded main level and trim with the source cue’s **Levels** and **Trim** tabs. Use a stopped rehearsal copy at a low listening level.
2. Set the original main level in **Levels** and the original main trim in **Trim** together. The imported main level already includes the trim. For example, an imported −15 dB can come from a source main level of −10 dB and a trim of −5 dB. Enter −10 dB and −5 dB as the two separate controls.
3. Compare the first GO, the end of an absolute Fade and the cue after Stop and a second GO. For a relative QLab Fade, work out the destination from each expected starting level and rehearse those transitions. The current importer uses absolute destinations. Check that **Stop target when done** still stops the intended cue.
4. Verify the physical channel order for cue-output trims. Restore the programmed routing levels from the source before adding those trims with the per-output exercise below. [QLab Trim](https://qlab.app/docs/v5/audio/audio-cues/#the-trim-tab) · [QLab audio Fades](https://qlab.app/docs/v5/audio/fading-audio/)

### Correct one cue output through a named patch

1. On a quiet rehearsal desk, assign a file Audio cue to an Output Patch with different device channels for each logical output. For example, route **Left monitor** to device channel 3 and **Right monitor** to device channel 1. Turn on **Require named device for Audio GO** for that patch. On Linux, use the direct ALSA hardware output for this check.
2. Play a quiet, steady stereo source. Open **Trim → Per-output trim** and set **Left monitor · ch 3** to −6 dB. At the interface or console, device channel 3 should fall by 6 dB while device channel 1 holds. Run a Fade that targets the Audio cue’s main level. Both outputs should follow the Fade and keep their 6 dB difference. Repeat with the cue’s crosspoint matrix enabled.
3. Save and reopen. Run the cue and confirm the saved compensation. Stop the cue, swap the device channel order in the patch and try GO at rehearsal level. Inkue holds the cue and reports the stale trim in preflight. Review the assignment, choose **Clear output trims** in **Trim**, and enter the correction for the new device channel.

Per-output trim multiplies the complete cue signal for that output after the programmed mix. Patches that combine several cue outputs onto the same physical channel need distinct device assignments to use independent trims. Other cues on the patch keep their own levels. [QLab Trim](https://reference.qlab.app/docs/v5/audio/audio-cues/#the-trim-tab)

## Move a sound between calibration marks

1. Use a quiet, sustained mono recording on a practice show. Set the machine’s main audio output to the named interface used by **Studio A**. Assign that same named patch explicitly in the Audio cue’s **Levels** tab. In **2D point panner**, place the file input at **Prop radio** and choose **Apply point mix**.
2. Add a Fade targeting that one Audio cue. In **Audio map movement**, choose **Prop radio** as **Start mark** and **Hallway** as **Destination mark**. Choose **Apply movement**, which makes this a dedicated manual Fade. Set a five-second duration and use the rising curve to shape the travel.
3. GO the Audio cue and then the Fade. The source traverses the straight line between the marks. Listen for the calibrated destination mix. On another pass, pause the Fade halfway, listen to the held position, then resume. Stop the Fade or edit the target’s crosspoints to take control. A fresh GO of the stopped Audio cue starts at the saved **Prop radio** mix.
4. For a return Fade, reverse the two marks. Call it once the first move has landed. If the map revision, start mix, or output assignment is wrong, the desk withholds the movement. Reopening the output stream ends an active movement. Confirm those behaviors during rehearsal before calling a movement live.

Movement updates one input row on the desk’s control clock. This workflow uses the calibrated, named main stream. Other rows and the cue’s saved start mix are preserved. [QLab Fading Object Position](https://qlab.app/docs/v5/audio/fading-audio/)

## Audition a trimmed clip in headphones on Linux

1. Connect the house and the operator headphones as two independently routed PipeWire outputs. Confirm their physical destinations on the venue console or interface. Select the named house output in **Audio Preferences**. In the workspace’s output patches, assign each device by name. Inkue refuses an audition if the live output is unknown, if a patch uses an unresolved alias such as System Default, or if a show patch uses the headphone device. This workflow uses PipeWire, WirePlumber and the `pw-play` utility (Debian package `pipewire-bin`).
2. Add a stereo Audio cue with an obvious opening and set its start and end trims around a short phrase. Set a different cue at the playhead. In the Audio cue’s **Time** tab, expand the waveform to open the clip editor. Choose **Headphone output**, leave **Monitor level** at −18 dB, select **Source only** under **Audition signal**, then click **Audition clip**.
3. Listen for one pass of the trimmed phrase. Confirm the house is silent, the playhead stays on the other cue and any follow cue remains in standby. **Stop audition** ends only the monitor pass. Closing the editor, switching clips, changing output routing and panic stop also cancel it. The monitor level resets to −18 dB when the editor reopens. The headphone device selection is remembered locally.
4. During rehearsal, disconnect the headphones during a second pass. Monitor the house. The audition should end without appearing there. Reconnect the headphones, click **Refresh outputs**, and audition again.

Clip audition uses the decoded source at its original speed with the cue’s volume and pan. Mono feeds both headphone channels. For multichannel files it monitors source channels 1 and 2. Rehearse groups, output maps, cue actions, slice loops and video from the cue list. [QLab 5 Auditioning Cues](https://qlab.app/docs/v5/tools/auditioning-cues/) · [QLab 4 Audition Window](https://qlab.app/docs/v4/tools/the-audition-window/)

### Compare source and cue effects in headphones

1. Add an enabled **High-pass** at 600 Hz in an Audio cue’s **Audio FX** tab. Use a low-volume recording with both low and high notes, such as cue **2** of the Audio FX practice show.
2. In the clip editor, leave the headphone output and monitor level fixed. Audition once with **Source only**. After the pass ends, select **With cue effects** and audition the same trim again. Listen for the low note to recede and the high note to stay audible.
3. Confirm the playhead and house output stayed in place. To hear a different effect setting, edit it under **Audio FX** and start a fresh audition. Each pass starts with the saved, enabled cue inserts in their current order. Use the cue list to rehearse active Fade automation and output patch effects. [QLab Audio FX](https://qlab.app/docs/v5/audio/audio-cues/#the-audio-fx-tab)

## Fade the master of an output patch

1. Set up two output patches, **House** and **Monitor**, on separate monitored channels. In the mixer set both patch faders to **0 dB** and save the workspace. Assign two long audio cues to House and a third to Monitor.
2. Add a **Fade** cue. Under **Output patch master**, select **House** and set **Patch gain (dB)** to **−12**. Set the duration to **4 s**. A patch-only fade can leave its cue target list empty. Its rising or falling curve follows the same action clock as its other targets.
3. Start the first House cue and the Monitor cue, then GO the Fade. Start the second House cue halfway through. Both House cues should follow the same patch level and finish 12 dB below their own cue gains. The Monitor cue should keep its level. A separate Fade on a House cue's own gain multiplies with the patch level.
4. Rehearse a return to **0 dB**. Pause the Fade to hold the level and resume to continue. Stop the Fade midway to hold there. A new Fade on House starts from the held level. Moving House's mixer fader takes control immediately. Earlier Fades cannot write over that adjustment.
5. Double-Escape to Panic, then start a fresh House cue. It should use the saved mixer setting. Reopen the workspace to check the same baseline. Cue-driven changes are live and do not dirty the stored mixer setting. To keep a new baseline, set it in the mixer and explicitly save the workspace.

A patch Fade also affects video soundtrack, mic and LTC voices assigned to that patch. A missing target patch fails preflight and GO. Confirm the local device and physical output in rehearsal.

## Slow a recorded announcement during playback

1. Add an **Audio** cue with a recording of a spoken scene announcement and at least a few seconds of sound after the first sentence. Preview it once at **1×**. Set safe rehearsal levels, then start that cue with **GO**.
2. Add a **Fade** cue that targets the announcement. In the **Audio** section enable **Fade playback rate**, set **To (×)** to **0.5**, and set **Time (s)** to **2**. Turn off **Fade volume** for the exercise. Use the **Curve** editor to choose how the speed changes.
3. Restart the announcement and fire the Fade during the first sentence. The voice should slow and drop in pitch. Pause the Fade midway and listen for a steady rate while the announcement continues. Resume and listen for the half-speed destination. The Active Cues remaining time estimates the tail at the current rate as it changes.
4. Repeat and stop the Fade midway. A second Fade to **1×** should start from the held rate. Editing the announcement’s **Playback rate** in the inspector takes control of that voice. Stopping and starting the announcement again uses its saved starting rate. A Fade’s saved rate destination applies when it starts its action.

Native Audio rates range from **0.03×** to **33×**. A Group target applies rate automation to its running file Audio cues. Trim and loop marks stay in source time. Playback speed and pitch move together. QLab’s Preserve Pitch setting, synchronized Video speed automation, and automatic migration of a Fade’s rate parameter require separate support. Recreate the rate step after import and listen against the source workspace. [QLab Fading Rate](https://qlab.app/docs/v5/audio/fading-audio/#fading-rate) · [QLab Audio Cue rate and pitch](https://qlab.app/docs/v5/audio/audio-cues/#rate-and-pitch)
