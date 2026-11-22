# Bring a version 3 show to a Linux desk

Open a rehearsal copy of a QLab **3.0.14** workspace using **File → Import QLab Workspace… (BETA)**. The historical file extension is `.cues`. Inkue also accepts a renamed `.qlab3` file with the same archived version. Other legacy versions need a separately saved QLab 4 or 5 copy or a rebuild from the cue sheet. Keep the source workspace and its media together. [QLab 4 file extension change](https://qlab.app/docs/v4/general/new-in-v4/)

## Check the imported legacy show

1. Compare cue lists, numbers, names, order and nested children with the original desk. Check pre-waits, post-waits and each manual, auto-continue and auto-follow cue against the caller’s book. Inkue gives legacy numeric cue IDs workspace-specific IDs and reconnects Start targets. Select the Start cue and check its target by name, particularly across lists. Groups and Start cues arrive disarmed for rehearsal.
2. Inspect each Audio cue’s original relative media path. A path starting with `../` can refer outside the show folder. Locate missing files, select a local output, check levels, trims, loops and effects, then rehearse before arming. The importer leaves legacy device settings behind.
3. Review the conversion report. Memos retain their calling information. Script cues keep their source in the notes, have an empty executable and arrive disarmed. Import only reads the archive. A camera, OSC cue or other unverified cue becomes a disabled Memo naming its original class. Check the original inspector to rebuild its behavior. Reassign any hotkey, MIDI, timecode or wall-clock trigger.
4. Save the reviewed Inkue show under a new name, reopen it and have the relief operator trace one follow and its next manual hold. Compare group behavior with the original desk before arming each group. [QLab 3 cue sequences](https://qlab.app/docs/v3/general/cue-sequences/) · [QLab 3 group modes](https://qlab.app/docs/v3/general/group-cues/)

**Reference checks.** The public QLX 1.2 demo imports as two lists containing 50 cues. Find six auto-continues, one auto-follow, a Start linked to *QLXC Chan 1 at Full*, one disarmed Audio cue and 36 disarmed Script cues. The public Q-Chromakey demo imports as one list with 12 cues. Its camera and three OSC cues become disabled Memos. Both files report QLab 3.0.14. The binary archives, script bodies and media are excluded from Inkue. To repeat the read-only regression on local copies, set `INKUE_QLX3_FIXTURE` and `INKUE_CHROMA3_FIXTURE` to the respective files and run `cargo test --test qlab3_archives -- --ignored` from `src-tauri`. The tests check the public files’ hashes and never open a device. [QLX author’s demo](https://github.com/sparks-alec/qlx/blob/e7ccb8fefe72284c7f09fbfd4f66a52e2d3d4ff5/QLX_1.2_Demo.cues) · [Q-Chromakey author’s demo](https://github.com/grifu/Q-Chromakey-Syphon/blob/85e2c861fdbb137b9d7c2263bd4abe9598798bc3/Q-Chromakey-Syphon.cues)

## Rebuild a preshow crossfade

1. In a rehearsal copy, create two Audio cues with different, recognizable tracks. Label both with their original cue numbers. Use the Audio inspector to confirm speaker assignments at low level.
2. Create a Fade cue for the first track. Set the destination to silence, choose the desired duration and enable **Stop targets when the fade ends**. Rehearse starting the second sound at the cue called in the original book.
3. Compare the middle of the transition as well as its endpoints. A relative fade, a fade on a group, and a fade of individual crosspoints have different effects when a performer holds a line or the cue is called twice. Repeat the transition from the same starting levels and compare with the original.
4. For live microphones, confirm which physical input feeds each speaker. A source-show reverb may depend on a macOS Audio Unit. Record the effect, mix and tail length and reproduce the sound on the rehearsal rig before approving it. [Audio workshop](../audio/README.md)

## Replace a projector

Open a test image for each surface in the original. Record the layer, intended screen edges, overlap, masking and any content that must remain transparent. On Linux, recreate the legacy stage and assign it to the physical projector and display a test image before the show media. Check corner alignment, image order and black regions by eye from the audience position. A matching stage name does not establish matching multi-projector blending or masks. [Stage workshop](../video/README.md)

## Rehearse a held vamp and a clock locate

Call an intentional hold in the middle of a line. Check whether the music waits indefinitely, where Devamp exits the loop and whether the following event occurs exactly once. Repeat the hold at more than one position in the slice. Write down the recovery cue that the operator will use if the clock or controller disappears. Then separately test forward clock playback, a backward locate and a forward jump on the venue source. [Show-control workshops](../workshops/README.md)

## Transfer control to the relief operator

Collect the rehearsal show, move the copy to a desk without access to the original media disk and verify its files. Have the relief operator select a cue without firing it, GO from the called number, stop playback, and recover after stopping partway through a sequence. Audit remote triggers separately. QLab Remote screen layouts, AppleScript dictionaries and the entire QLab 3 OSC command set require individual review. Inkue is GPL licensed and has no machine activation transfer step. [Collected-show verification](../standby/README.md) · [Live desk](../live/README.md)

## Source register

The official version 3 contents lists 31 reference topics and nine how-to lessons. All 40 linked pages responded during this review. The workflows above are original Inkue exercises. Device-dependent results require a rehearsal on the destination system. [QLab 3 documentation](https://qlab.app/docs/v3/)

### Desk and workflow

- [What's New In QLab 3?](https://qlab.app/docs/v3/general/new-in-v3/)
- [System Recommendations](https://qlab.app/docs/v3/general/system-recommendations/)
- [A Computer Prepares](https://qlab.app/docs/v3/general/preparing-your-mac/)
- [Getting Started With QLab](https://qlab.app/docs/v3/general/getting-started/)
- [Keyboard Shortcuts](https://qlab.app/docs/v3/general/keyboard-shortcuts/)
- [Workflow Tools](https://qlab.app/docs/v3/general/workflow-tools/)
- [Group Cues](https://qlab.app/docs/v3/general/group-cues/)
- [Cue Sequences](https://qlab.app/docs/v3/general/cue-sequences/)
- [Licenses](https://qlab.app/docs/v3/general/licenses/)
- [QLab 3 Features by License Type](https://qlab.app/docs/v3/general/features/)
- [Templates](https://qlab.app/docs/v3/general/templates/)
- [QLab Remote](https://qlab.app/docs/v3/general/qlab-remote/)
- [Settings](https://qlab.app/docs/v3/general/settings/)

### Audio

- [Audio Cues](https://qlab.app/docs/v3/audio/audio-cues/)
- [Mic Cues](https://qlab.app/docs/v3/audio/mic-cues/)
- [Fading Audio](https://qlab.app/docs/v3/audio/fading-audio/)
- [The Patch Editor](https://qlab.app/docs/v3/audio/patch-editor/)

### Video

- [Video Cues](https://qlab.app/docs/v3/video/video-cues/)
- [Camera Cues](https://qlab.app/docs/v3/video/camera-cues/)
- [Titles Cues](https://qlab.app/docs/v3/video/titles-cues/)
- [Fading Video](https://qlab.app/docs/v3/video/fading-video/)
- [Surfaces](https://qlab.app/docs/v3/video/surfaces/)

### Show control and scripting

- [OSC Cues](https://qlab.app/docs/v3/control/osc-cues/)
- [MIDI Cues](https://qlab.app/docs/v3/control/midi-cues/)
- [MIDI File Cues](https://qlab.app/docs/v3/control/midi-file-cues/)
- [Timecode Cues](https://qlab.app/docs/v3/control/timecode-cues/)
- [Script Cues](https://qlab.app/docs/v3/control/script-cues/)
- [Other Cues](https://qlab.app/docs/v3/control/other-cues/)
- [QLab's OSC API](https://qlab.app/docs/v3/scripting/osc-dictionary-v3/)
- [QLab's AppleScript Dictionary](https://qlab.app/docs/v3/scripting/applescript-dictionary-v3/)
- [Other Resources](https://qlab.app/docs/v3/scripting/other-scripting-resources/)

### Tutorials

- [Crossfade Two Sounds](https://qlab.app/docs/v3/howto/crossfade-sounds/)
- [Use Reverb With Mic Cues](https://qlab.app/docs/v3/howto/use-reverb-with-mic-cues/)
- [Fade In A Group of Audio Cues](https://qlab.app/docs/v3/howto/fade-audio-groups/)
- [Fade Preshow Music](https://qlab.app/docs/v3/howto/fade-preshow-music/)
- [Fade In A Group of Video Cues](https://qlab.app/docs/v3/howto/fade-video-groups/)
- [Replace A Projector](https://qlab.app/docs/v3/howto/replace-a-projector/)
- [Move A Workspace to A New Mac](https://qlab.app/docs/v3/howto/move-workspaces/)
- [Move A QLab 3 License to A New Mac](https://qlab.app/docs/v3/howto/move-licenses/)
- [Trigger QLab With Timecode](https://qlab.app/docs/v3/howto/trigger-qlab-with-timecode/)
