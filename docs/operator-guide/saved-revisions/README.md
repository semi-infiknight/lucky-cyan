# Restore an earlier saved revision

Choose **File → Saved revisions…**. Select the workspace by its original path, then choose a revision by date, kind and size. **Restore to new file…** creates a separate .inkue document. Pick an unused filename. Open that copy explicitly when you want to rehearse it.

## Rehearse an earlier edit

1. Save a rehearsal show containing a group with an Audio cue and a recognizable cue number. Give the cue a name such as **Before entrance** and save.
2. Rename the cue **After entrance** and save again. In Saved revisions, select the older version and restore it into another folder under a fresh name.
3. Check the current desk. It should still show **After entrance** with its original playhead. Open the restored copy when ready, find **Before entrance**, and run **Check Workspace** to verify the nested media target.
4. Try restoring again to the same destination. The desk asks for an unused name and preserves the existing file.

## What the history captures

Save and Save As record the exact native show document committed to each original path. The first overwrite of a valid existing show also captures its previous on-disk version. Identical consecutive documents share a single revision. A Save As under a new name starts a separate history. The picker can retrieve an earlier show even after its original file has moved or disappeared.

The desk keeps up to 12 distinct revisions and 64 MiB of unescaped workspace documents per original path in the current user's Inkue configuration directory under `saved-revisions`. The oldest entries inside the managed archive roll off first. A single document larger than the budget cannot be archived. If revision storage fails, the health banner reports the incomplete history after the primary save has succeeded.

Restoring beside the original preserves the saved workspace bytes. Restoring elsewhere adjusts supported cue file paths, including those inside groups, relative to the original media location. The archive contains cue programming and paths. To carry the media itself, use **Collect and Save** and verify the collected folder. [Verify a collected show](../standby/README.md)

## Archive edits on a timer

1. Save the rehearsal workspace to a named `.inkue` file. In **Preferences → General → Workspace history**, set **Timed revisions** to **Every minute**, apply it and save the preference with the show.
2. Rename a cue and wait a minute with the desk idle. Open **File → Saved revisions…** and find the entry labeled **Timed snapshot**. Restore it to a fresh path and check the edit there. The original show file still contains the last explicit Save.
3. Try a second edit during a running or paused cue. The snapshot waits until playback is idle. Save the edited workspace explicitly and the archive shares the latest entry when its contents match.

Timed snapshots use the same per-show retention as explicit saves. Off is the default for older shows and new workspaces. The separate crash-recovery prompt offers recent unsaved work after an abnormal exit. [Recovery rehearsal](../qualification/README.md)
