# QLab coverage and reference index

The comparison baseline is the QLab 5 manual dated October 28, 2026 for version 5.6.5, with Audio, Video and Lighting licenses. This index covers 86 linked reference and tutorial pages. Each topic links to the corresponding original Inkue exercise and the upstream reference. Index coverage measures research scope. Product parity requires a successful operator exercise on the target build.

Use the [licensed-feature checklist](../license-comparison/README.md) to review individual requirements for a particular show.

## Can this show switch?

| A production depends on | Current position |
| --- | --- |
| GO, numbered cues, fades, follows and carts | Native playback, with an installed Linux sound-check fixture. Compare nested and chained behavior against the source show. |
| Audio matrices | The integrated engine supports up to 128 source and destination indices. Available channels depend on the selected device and patch. Dynamic object positioning and hosting existing Audio Unit effects remain open. |
| Video stages | The importer maps declared stages and the operator can adjust output geometry. Native NDI output and Syphon interchange remain open. |
| The QLab lighting language | The desk has native fixture and DMX tools. Review each translated action and every unsupported command before arming. |
| QLab Collaboration | A desk can grant access to cue notes and review flags for one open workspace. Each edit checks the current value and enters the desk's undo history. Full remote workspace editing and peer permissions remain open. [Cue review exercise](../automation/README.md#review-cue-notes-from-a-second-terminal) |
| Existing AppleScript and remote-control clients | Review imported script sources and reauthor portable commands. Existing QLab-specific AppleScript clients and the full QLab OSC dictionary require adaptation. |
| Opening-night reliability | The latest evidence is in the Linux qualification record. Physical multichannel, projector, MIDI, DMX and device-recovery gates each require the appropriate equipment. |

## System and licensing

Check the tested OS and package in Linux qualification. The application uses GPL-3.0-or-later and has no license tiers. [Inkue exercise](../qualification/README.md)

- [QLab 5 Change Log](https://qlab.app/docs/v5/general/change-log/)
- [What's New In QLab 5?](https://qlab.app/docs/v5/general/new-in-qlab-5/)
- [System Recommendations](https://qlab.app/docs/v5/general/system-recommendations/)
- [A Computer Prepares – QLab 5 Edition](https://qlab.app/docs/v5/general/preparing-your-mac/)
- [Keyboard Shortcuts](https://qlab.app/docs/v5/general/keyboard-shortcuts/)
- [Licenses](https://qlab.app/docs/v5/general/licenses/)
- [QLab 5 Features by License Type](https://qlab.app/docs/v5/general/features/)
- [Preferences](https://qlab.app/docs/v5/general/qlab-preferences/)

## Workspace and cue programming

Use File to transfer selected transport and cue-list preferences between Inkue workspaces. QLab settings files and patch transfers require separate conversion. [Desk settings transfer exercise](../settings/README.md)

Program a show in the cue list or cart. Check the imported report before trusting a workspace template, group or output assignment. [Inkue exercise](../programming/README.md)

- [The Workspace](https://qlab.app/docs/v5/fundamentals/workspace/)
- [Workspace Settings](https://qlab.app/docs/v5/fundamentals/workspace-settings/)
- [Workspace Templates](https://qlab.app/docs/v5/fundamentals/workspace-templates/)
- [Saving, Managing, and Moving Workspaces](https://qlab.app/docs/v5/fundamentals/managing-workspaces/)
- [Cues](https://qlab.app/docs/v5/fundamentals/cues/)
- [The Inspector](https://qlab.app/docs/v5/fundamentals/inspector/)
- [Cue Lists](https://qlab.app/docs/v5/fundamentals/cue-lists/)
- [Cue Carts](https://qlab.app/docs/v5/fundamentals/cue-carts/)
- [Group Cues](https://qlab.app/docs/v5/fundamentals/group-cues/)
- [Cue Sequences](https://qlab.app/docs/v5/fundamentals/cue-sequences/)

## Workflow tools

Use Find, the inspector, Check Workspace, relink, Show Mode, the mixer and the floating timer. Cue monitor shows standby and active playback across lists, compares stage stills, previews finished stage motion in a thumbnail, and shows saved audio map marks alongside digital peaks from the assigned main-device channels. Stereo file audition can use its own named headphone sink. Group, video and full-effects audition, global overrides, full-resolution output monitoring, input monitors, and separate audio-map windows remain open. [Cue monitor exercise](../programming/README.md#monitor-the-show-from-the-desk) · [Stage snapshot exercise](../video/README.md#check-the-stage-picture-from-the-desk) · [Live desk](../live/README.md)

Paste selected groups between existing Audio and Video cues as one undoable edit while transport is idle. [Property paste exercise](../programming/README.md#paste-selected-settings-onto-existing-cues)

- [The Tools Menu](https://qlab.app/docs/v5/tools/tools-menu/)
- [Find](https://qlab.app/docs/v5/tools/find/)
- [Paste Cue Properties](https://qlab.app/docs/v5/tools/paste-cue-properties/)
- [The Launcher Window](https://qlab.app/docs/v5/tools/launcher-window/)
- [The Workspace Status Window](https://qlab.app/docs/v5/tools/workspace-status-window/)
- [The File Target Search Tool](https://qlab.app/docs/v5/tools/file-target-search/)
- [Monitor Windows](https://qlab.app/docs/v5/tools/monitor-windows/)
- [The Timecode Status Window](https://qlab.app/docs/v5/tools/timecode-status-window/)
- [Auditioning Cues](https://qlab.app/docs/v5/tools/auditioning-cues/)
- [Override Controls](https://qlab.app/docs/v5/tools/override-controls/)

## Audio

Audio and Video cues can retain millisecond slice markers, finite segment counts and a vamp released by a current-slice Devamp. One unsliced looping File Audio cue or looping MP4/MOV Video — soundtracked movies included, released together with their picture — can also finish its current whole-clip pass; QLab whole-cue Devamps import armed with **Whole cue** scope automatically. Starting another cue on the exact release boundary remains open. Rehearse a hold and exit against the original track. [Musical hold exercise](../audio/README.md#hold-the-underscore-and-release-it-on-a-musical-boundary) · [Whole Audio loop exercise](../audio/README.md#release-a-whole-audio-loop) · [Movie loop exercise](../video/README.md#finish-a-movie-loop-at-its-natural-end)

Route file channels through the level matrix. Check source and destination levels and listen to the collected copy. Place a stationary Audio cue input on a custom speaker layout or reuse a workspace audio map with named calibration marks. Animated trajectories, shared sound objects, filters, spread, 3D positioning, AU plug-in hosting and the full output-patch DSP graph remain open. [Inkue exercise](../audio/README.md)

- [Introduction To Audio in QLab](https://qlab.app/docs/v5/audio/introduction-to-audio/)
- [Object Audio in QLab](https://qlab.app/docs/v5/audio/object-audio/)
- [Audio Cues](https://qlab.app/docs/v5/audio/audio-cues/)
- [Mic Cues](https://qlab.app/docs/v5/audio/mic-cues/)
- [The Audio Output Patch Editor](https://qlab.app/docs/v5/audio/audio-output-patch-editor/)
- [The Audio Map Editor](https://qlab.app/docs/v5/audio/audio-map-editor/)
- [Fading Audio and Audio Effects](https://qlab.app/docs/v5/audio/fading-audio/)

## Video

Rehearse a picture, its stage assignment and a fade. Native NDI and Syphon I/O, every blend/effect mode and projector-specific calibration remain open. [Inkue exercise](../video/README.md)

- [Introduction To Video In QLab](https://qlab.app/docs/v5/video/introduction-to-video/)
- [Video Cues](https://qlab.app/docs/v5/video/video-cues/)
- [Camera Cues](https://qlab.app/docs/v5/video/camera-cues/)
- [Text Cues](https://qlab.app/docs/v5/video/text-cues/)
- [Blend Modes](https://qlab.app/docs/v5/video/blend-modes/)
- [Video Output](https://qlab.app/docs/v5/video/video-output/)
- [Using NDI](https://qlab.app/docs/v5/video/using-ndi/)
- [Fading Video and Video Effects](https://qlab.app/docs/v5/video/fading-video/)

## Lighting

Review the source commands and local fixture patch offline before arming a Light cue. The full QLab fixture library and command language remain open. [Inkue exercise](../lighting/README.md)

- [Introduction to Lighting in QLab](https://qlab.app/docs/v5/lighting/introduction-to-lighting/)
- [Light Cues](https://qlab.app/docs/v5/lighting/light-cues/)
- [The Light Dashboard](https://qlab.app/docs/v5/lighting/light-dashboard/)
- [The Lighting Command Language](https://qlab.app/docs/v5/lighting/lighting-command-language/)
- [The Light Patch Editor](https://qlab.app/docs/v5/lighting/light-patch-editor/)
- [The Light Library](https://qlab.app/docs/v5/lighting/light-library/)

## Network, MIDI and show control

Use an isolated endpoint for OSC follow markers. The native desk also supports MIDI, timecode, and a trusted hardware remote. Verify each controller and timecode source on the rig. Full remote multiuser workspace editing and the QLab iOS client workflow remain open. [Inkue exercise](../workshops/README.md)

- [Collaboration](https://qlab.app/docs/v5/networking/collaboration/)
- [QLab Remote](https://qlab.app/docs/v5/networking/qlab-remote/)
- [Stream Deck](https://qlab.app/docs/v5/networking/stream-deck/)
- [Using OSC with QLab](https://qlab.app/docs/v5/networking/using-osc/)
- [Using MIDI and MSC with QLab](https://qlab.app/docs/v5/networking/using-midi-and-msc/)
- [Show Control Broadcast](https://qlab.app/docs/v5/networking/show-control-broadcast/)
- [Using Timecode with QLab](https://qlab.app/docs/v5/networking/using-timecode/)
- [Network Cues](https://qlab.app/docs/v5/networking/network-cues/)
- [MIDI Cues](https://qlab.app/docs/v5/networking/midi-cues/)
- [MIDI File cues](https://qlab.app/docs/v5/networking/midi-file-cues/)
- [Timecode Cues](https://qlab.app/docs/v5/networking/timecode-cues/)

## Scripting and automation

Inspect a saved file with MCP or approve a gated action locally. An allowlisted OSC controller can read active-list standby and the state of a numbered cue. An imported AppleScript needs an operator-approved portable replacement. [OSC query exercise](../workshops/README.md#read-standby-and-cue-state-from-an-osc-controller) · [Automation guide](../automation/README.md)

- [QLab's OSC Dictionary](https://qlab.app/docs/v5/scripting/osc-dictionary-v5/)
- [OSC Queries](https://qlab.app/docs/v5/scripting/osc-queries/)
- [QLab's AppleScript Dictionary](https://qlab.app/docs/v5/scripting/applescript-dictionary-v5/)
- [Script Cues](https://qlab.app/docs/v5/scripting/script-cues/)
- [Parameter Reference](https://qlab.app/docs/v5/scripting/parameter-reference/)
- [OSC and Scripting Examples](https://qlab.app/docs/v5/scripting/examples/)

## Transport and other cues

Replay the practice list. Target, Reset, Arm, Stop, GoTo, Devamp and related actions need checks in the source show’s exact context. [Inkue exercise](../programming/README.md)

- [Transport Cues](https://qlab.app/docs/v5/other-cues/transport-cues/)
- [Devamp Cues](https://qlab.app/docs/v5/other-cues/devamp-cues/)
- [GoTo Cues](https://qlab.app/docs/v5/other-cues/goto-cues/)
- [Target Cues](https://qlab.app/docs/v5/other-cues/target-cues/)
- [Arm and Disarm Cues](https://qlab.app/docs/v5/other-cues/arm-and-disarm-cues/)
- [Wait Cues](https://qlab.app/docs/v5/other-cues/wait-cues/)
- [Memo Cues](https://qlab.app/docs/v5/other-cues/memo-cues/)

## Tutorial exercises

Use the product workshops and sample show to test each operation at the desk. Educational references covering macOS, object audio and QLab-specific toolkits have different platform or feature requirements. [Inkue exercise](../workshops/README.md)

- [Zero To Audio](https://qlab.app/docs/v5/tutorials/zero-to-audio/)
- [Zero To Video](https://qlab.app/docs/v5/tutorials/zero-to-video/)
- [Zero To Network](https://qlab.app/docs/v5/tutorials/zero-to-network/)
- [Cue Sequences](https://qlab.app/docs/v5/tutorials/cue-sequences/)
- [Audio Workflow](https://qlab.app/docs/v5/tutorials/audio-workflow/)
- [How To Use A Mac](https://qlab.app/docs/v5/tutorials/how-to-use-a-mac/)
- [Basic Computer Networking for the Theater](https://qlab.app/docs/v5/tutorials/basic-networking/)
- [Understanding USB-C](https://qlab.app/docs/v5/tutorials/understanding-usb-c/)
- [Object Audio Basics](https://qlab.app/docs/v5/tutorials/object-audio-basics/)
- [Blend Mode Demo](https://qlab.app/docs/v5/tutorials/blend-mode-demo/)
- [Fading With Paths](https://qlab.app/docs/v5/tutorials/fading-with-paths/)
- [Timecode Tools](https://qlab.app/docs/v5/tutorials/timecode-tools/)
- [Sam's Toolbox](https://qlab.app/docs/v5/tutorials/sams-toolbox/)

## Additional learning material

See the [learning source survey](../learning-sources/README.md) for earlier manuals, specialist tutorials and the Cookbook. Each tutorial in this product manual is written for the Inkue desk. [Linux qualification record](../../operator-rehearsal/verification/README.md)

