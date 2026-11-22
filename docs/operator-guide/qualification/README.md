# Operator acceptance and coverage

## Release objective

An experienced QLab operator can migrate, program, rehearse, collect and run a representative show on Linux. The reference for feature comparison is QLab 5 with Audio, Video and Lighting licenses. Preserve stable cue numbers, targets, media bindings and intentional silences. Record every loss that needs a human decision. The first release targets Debian 13 amd64. Windows and macOS rehearsals follow Linux qualification.

## Operator exercises

| Exercise | Observable acceptance condition |
| --- | --- |
| Migrate a cue list and a cart | Open an actual QLab workspace. Compare cue counts, waits, follows, target references, media and patch assignments. Unsupported behavior identifies the affected cue and a corrective action. |
| Edit a live desk | Change selection without firing a cue. Navigate nested groups, rename a cue, and return to the playhead. Test key repeat protection and hard stop. Apply a route edit after a finished fade on a continuing cue. Gain must stay at its faded value and the next manual cue must remain on standby. Check Undo with a paused cue in a different list. Retry a rejected audio start without skipping the cue. |
| Program a multichannel file | Route discrete inputs above stereo to distinct physical outputs. Editing one crosspoint must preserve out-of-view routes. Compare decoded sample amplitudes and silence with a reference. |
| Operate a cart | Trigger and retrigger individual pads. Compare chaining and playhead behavior with the source show. Test intentional differences against a saved legacy Inkue cart. |
| Program from a script | Preview marked audio cues, resolve every media path, approve once, save, reopen and inspect programming history. Keep GO available to the local operator. |
| Verify a lighting or video look | Confirm native output on the intended projector, stage or fixture. Check opacity, layer order, fade, patch and blackout. Any unsupported import property stays visible in review. |
| Recover | Force exit on a rehearsal copy, restore unsaved work, verify playhead, patches and notes, then confirm transport waits for an explicit GO. |
| Qualify playback | Run the installed binary overnight and measure every OSC follow and PCM envelope. The runner fails on missing packets, incorrect levels or recording-clock drift. Listen through the venue interface with the operator's actual show and record dropouts, response time and device recovery. |

## Release decision

A passing build and a passing short virtual rehearsal qualify a private rehearsal candidate. Production approval requires observed venue behavior and a completed endurance run on the shipped installer. Record the binary checksum and unresolved operator steps with the decision. Keep the proposal to FonograF as a private draft.

## Coverage owner

Each topic in the operator manual describes an executable workflow, its limits and its source references. Mark a capability present only with a source implementation and evidence through a normal entry point. A partial mapping and an untested capability each remain open for parity.

## Independent review findings

The context-free reviewer worked from the operator exercises and compared them with the implementation. Checking a muted movie revealed that the embedded audio levels could be lost on import. Testing a route through two logical outputs revealed that aliased physical channels could overwrite one another. Inspecting a live matrix update and the voice pool identified callback snapshots that could outlive their writer-side retirement. The corresponding regressions now cover the archived movie, summed physical routes and an audio reader holding an old snapshot during edits.

A spare-machine walkthrough also exposed a missing way to check copied media. Use the [file verification exercise](../standby/README.md) before testing the spare's devices. [Elephant–Goldfish Method](https://egm.dev/)
