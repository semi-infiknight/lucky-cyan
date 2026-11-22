# Transfer desk settings between workspaces

A touring company might want every show to use the same double-GO interval and cue-list layout. An Inkue settings file lets the receiving operator review those values against the open show. Cue lists, cue numbers, media, output patches, devices and network destinations stay in the show or on the local machine.

## Choose a theme for the desk

1. Open **File → Preferences → Personalization** and find **Appearance**.
2. Under **Theme**, choose **Dark**, **Navy**, **Light (Warm Cream)**, or **System (follow OS)**.
3. Select **Apply**, return to the desk, and check that the cue numbers, notes and playhead are easy to read under the room lighting. Save the workspace to keep its choice when you reopen the show.

The Output Mixer and separate cart windows follow the desk theme while they are open.

## Start a show from a workspace template

1. Prepare a rehearsal workspace with your usual cue lists, carts, output patches and any starting cues. Choose **File → Save as Template…** and name the file `Tour starting point.inkue-template`. Your original workspace remains open with its current edits.
2. Choose **File → New from Template…** and select that file. If prompted, decide whether to save or discard edits in the show you are leaving. The new workspace starts idle and puts each sequential list at its first armed cue. Save the new show under its own name before programming it.
3. Change a cue note in the new show and save it. Start from the template again to confirm the original note is still there. Templates refer to media at its existing location. Use **Collect and Save** on the show when the next desk needs a copy of the media. Review physical patches and external triggers on that desk. [QLab workspace templates](https://qlab.app/docs/v5/fundamentals/workspace-templates/)

### Use a template for ordinary New Workspace

Open **View → Workspace Launcher…** and choose **Set default…** in **Default template**. Select the local `.inkue-template` file and check its displayed name and path. **File → New Workspace** now starts from that file on this machine. Use **New Blank Workspace** in the Launcher to bypass the default for one show. Use **Clear default** to restore blank as the ordinary starting point. Clearing the choice leaves the template file in place. If the file moves, select it again in the Launcher. Inkue asks you to resolve a missing default before replacing the open show. [QLab template defaults](https://qlab.app/docs/v5/fundamentals/workspace-templates/#managing-templates)

## Give a relief operator your timing and layout

1. Open the source workspace. In **Preferences**, confirm the double-GO interval and cue row height the crew uses.
2. Choose **File → Export workspace settings…**. Clear the selection, then select **Double-GO protection** and **Cue row height**. Read the current values and choose **Save settings file…**. Save the file as `Tour desk.inkuesettings`.
3. Open the destination workspace and choose **File → Import workspace settings… → Choose settings file…**. Check the source and destination names. For each setting the file contains, the desk displays the current and incoming values.
4. Select the changes you want. A matching value is already in place. Choose **Apply selected settings** and save the workspace. If the workspace changed during review, select **Refresh review**, check the values again and make a fresh selection.
5. Reopen the destination. Confirm the two settings in Preferences, check the first cue number and media file, and inspect its output patch. Rehearse two deliberate GO presses on a silent Wait sequence to verify the interval. Exporting or applying settings sends no GO.

## Settings carried by the transfer file

| Setting | What it changes |
| --- | --- |
| Double-GO protection | Minimum time between successive GO requests. Zero disables the interval. |
| Default audio stop fade | Duration of the default soft stop in milliseconds. |
| Confirm before deleting cues | Confirmation for a keyboard delete. |
| Scroll to playhead | Whether the cue list follows the playhead. |
| Cue row height | Compact, normal or tall rows. |
| Cue color display | A color stripe or a tinted full row. |
| Desk theme | Dark, Navy, Light (Warm Cream), or System. Review both appearances before applying. |
| Show Hard stop + blackout button | Displays a separate local transport action. The operator must press it to cut playback and engage DMX blackout. |
| Renumber cues on future reorders | The destination's policy for numbering after structural list edits. Applying this setting preserves the existing numbers. |

Inkue writes version 2 when you include **Desk theme**. An export with the other fields uses version 1 for older desks. This version of Inkue can read either file. The importer validates the file before showing the review. QLab's workspace settings, patches and templates have their own file formats. For a QLab show, use the workspace importer and check its per-cue report. [QLab workspace settings](https://qlab.app/docs/v5/fundamentals/workspace-settings/) · [QLab migration rehearsal](../../operator-rehearsal/README.md)
