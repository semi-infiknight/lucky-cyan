# Export a media usage report

Open **Preferences → General → Media usage reports** and enable **Record media usage on this desk** before the performance. This opt-in applies immediately to the machine and survives restarting Inkue. The journal spans workspaces and performances. It records the show name, saved workspace path, cue ID, number, name, media type, file path, UTC time and a unique playback ID. An unsaved workspace has an empty workspace path.

## Rehearse a report

1. Use a rehearsal copy with a short Audio cue numbered **101**. Set a two-second pre-wait. Run it once and stop it during the pre-wait.
2. Run it again and let it play to completion. Trigger it a third time and stop it after playback begins. For a longer file, try pausing and resuming before the stop.
3. Choose **Export media usage CSV…**, open the CSV in a spreadsheet and filter to this workspace, cue 101 and the rehearsal time. The canceled pre-wait should contribute zero starts. The two plays that reached the audio engine should have different playback IDs. Each start should have an end with that same ID. A paused play also has separate pause and resume rows.
4. Disable recording and run the cue again. Export another copy. The new play should add no rows. Take the exported copy to the show office. The local journal retains earlier records.

## Read the report

| Event | Interpretation |
| --- | --- |
| start | The playback engine accepted an Audio voice, a Video or Image presentation, or a MIDI File player. A preloaded picture counts on reveal. A rejected start, an unelapsed pre-wait or media held paused by a Load cue contributes no start. An accepted start can still be inaudible because of routing, gain, device errors or a later renderer failure. |
| first_seen_on_resume | A previously unobserved voice or MIDI player resumed. This includes audio held paused by a Load cue or a play already underway before recording was enabled. Its prior duration is unknown. |
| pause / resume | The desk paused or resumed this play. The paired events preserve a single playback ID. Repeated loops within a cue share that ID. Each fresh start gets a new ID. |
| end | The desk released the play or requested a stop. A manual stop can finish its audible fade after the request. A held image can remain on the output. The reason distinguishes stop, hard stop, release, re-trigger, recording disabled and desk closed. Finish or stop all running and paused cues before using Undo or Redo. |
| output_error | The engine reported a failure for an observed voice. The detail column contains the error. Inspect this row even if the cue later reached its scheduled end. |
| gap | The report writer's queue filled. The final column gives the number of observations lost. Mark that performance report incomplete. |

File identity and cue metadata are captured at the start, so an edit midway through a cue does not change its closing row. An unpaired start after an abrupt process exit has an unknown end. Filter by show and time before reporting a performance. For rights submissions, reconcile the results with the cue sheet, venue observations and any required composer or rights-holder data.

CSV text that begins with a spreadsheet formula character has a protective apostrophe prefix. The original cue number, name and media path in the workspace stay intact.

Reports are local. The active journal is stored under `Inkue/reports/media-usage.csv` in the user's configuration directory. Turning recording off closes active observations and retains the file. If Preferences reports a write failure, resolve the storage issue and enable recording again before the next performance.
