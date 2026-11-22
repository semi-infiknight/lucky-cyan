# Verify a collected show on a spare desk

Use this exercise when the relief operator needs to confirm that two machines hold the same saved cues and media. It works with an offline copy and the standalone `inkue-inspect` program.

1. Save the workspace and choose **File → Collect and Save**. Read the report, resolve missing media and collect again if needed. Close the project before making the reference copy.
2. In the collected project folder on the first machine, run the first command below. Keep the reference manifest with the handoff record. It identifies the exact saved workspace bytes and each distinct audio, image, video and MIDI file used by cues across all lists, carts and nested groups.
3. Copy the collected project and the original manifest to the spare. Before opening or editing the show there, run the second command from the spare's project folder.

```sh
inkue-inspect "My show.inkue" manifest > handoff.json
inkue-inspect "My show.inkue" verify handoff.json
```

The verification command exits successfully and reports the number of distinct media files when the recorded SHA-256 values match. If it names a missing or changed media file, recopy that file from the approved source and repeat the check. A workspace mismatch includes changes to saved notes, targets, routing or playhead. Save a new reference on the primary whenever the approved show changes. An absolute media path or a symlink to media outside the collected directory requires recollection. Stop if manifest creation prints an error or exits unsuccessfully.

4. Work on a rehearsal copy on the spare. Open **Check Workspace**, confirm the physical devices and test one signal on each required output. A successful file comparison covers the workspace and its referenced cue media. It cannot check the venue wiring, separately installed plug-ins, external applications, scripts' external dependencies or output ownership.
5. At the desk-to-desk handoff, follow the [single-owner changeover workshop](../workshops/README.md) before connecting the spare to a live receiver.

The CLI reads local files throughout this exercise. No live transport connection is needed.
