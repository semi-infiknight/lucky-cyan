# Program with a local assistant

Start with a saved copy of the practice show. Run the standalone helper on the same computer as the show file.

```sh
inkue-inspect "Operator rehearsal.inkue" summary
inkue-inspect "Operator rehearsal.inkue" list
```

Locate S1 in the Sound checks cart and inspect its stable ID using `inkue-inspect "Operator rehearsal.inkue" cue ID`. To give a local MCP client the same view, configure the executable as its server command with the absolute show path and `mcp` as separate arguments.

## Turn a stage direction into a cue

Write the following line in a plain-text script. It supplies an explicit cue number, display name, media path and gain.

```text
[SOUND | S1 | Standby | standby.wav | -18]
```

Choose **File → Review audio cue sheet**, paste the script and select **Preview cue list**. Check that the single cue is named Standby and shows −18 dB. Choose the folder containing standby.wav and open the reviewed sheet. The desk creates a new unsaved show. Save a copy before continuing. Missing or out-of-folder media need correction before acceptance.

## Request one GO

1. Open **Agent access** and start a read-only session. The displayed file gives a local client access to this session. Keep it private.
2. Preview the next action with the command below and compare the cue number and name to the desk.
3. Choose **Arm one request**. Have the client repeat the command with `--request-approval`. Read the pending action at the desk and choose **Confirm action** or **Decline**.
4. Check the playhead and the outcome. The session returns to its disarmed state. Disconnect when finished and save the show to retain the audit.

```sh
inkue-inspect --live "$SESSION" transport go
inkue-inspect --live "$SESSION" transport go --request-approval
```

The localhost HTTP and OSC interfaces accept the same command vocabulary. The original hardware OSC remote in Preferences can fire cues directly when enabled. Use the agent session for assistants. Network access, show feedback and control permissions have different meanings across desks. [QLab's networking manual](https://qlab.app/docs/v5/networking/)

## Watch the desk from a second display

1. Open **Agent access** and start a read-only session. Copy the path under **Status-only file for a local display**. This file grants access to standby and active cue status on the same computer.
2. Open a terminal on the second display and set `MONITOR_FILE` to that path. Run the command below. Each cue list shows its standby cue, running and paused cues, and their elapsed and known remaining times. Cues in groups are indented.

   ```sh
   inkue-inspect --live "$MONITOR_FILE" watch
   ```

3. Rehearse with a silent Wait. Compare standby before GO, then check Running and Paused while operating the main desk. Close the monitor with **Ctrl+C** or select **Disconnect** in Agent access to revoke its session credentials. The monitor exits when the session closes or the desk cannot be reached after repeated attempts. Show Mode keeps a connected monitor available.

Use `status` in place of `watch` for a single JSON snapshot. Repeated status checks leave the show and its programming history untouched. The status credential works over loopback HTTP and permits only this snapshot. [QLab Remote monitor workflow](https://qlab.app/docs/v5/networking/qlab-remote/#read-only-mode-free)

## Review cue notes from a second terminal

1. Open a disposable practice show. In **Agent access**, select **Start read-only session**, then **Enable cue review**. Copy **Private cue review file** and set `REVIEW_FILE` to that path in a terminal on the desk. The credential grants access to list names, cue labels, notes and review flags for the current open workspace.
2. Find a list with `review-lists`. Set `LIST_ID` to its ID, run `review-cues`, and set `CUE_ID` to the rehearsal cue's ID. Read the note, save its exact text, and test an edit.

   ```sh
   inkue-inspect --live "$REVIEW_FILE" review-lists
   inkue-inspect --live "$REVIEW_FILE" review-cues "$LIST_ID"
   inkue-inspect --live "$REVIEW_FILE" review-note "$LIST_ID" "$CUE_ID" \
     | python3 -c 'import json,sys; sys.stdout.write(json.load(sys.stdin)["note"])' > before.txt
   printf '%s' 'Hold for the stage manager' > revised.txt
   inkue-inspect --live "$REVIEW_FILE" review-set-note "$LIST_ID" "$CUE_ID" \
     --expected-file before.txt --note-file revised.txt
   ```

3. Confirm the note in the main desk. With playback stopped, move standby to another cue and use Undo in that list to restore the note. Check that standby stays where you parked it. Change the note on the desk after the terminal captures it. Repeat the terminal edit and check that the desk preserves the newer text. Start an unsaved change in the inspector, apply a different note from the terminal, and leave the field. Compare both versions in the inspector. Choose **Save my draft** or **Reload saved note**.
4. Revoke review in **Agent access** and repeat `review-lists` using the old credential. Entering **Show Mode**, opening another show, or disconnecting the parent session also revokes it.

The two note files may each contain up to 2,000 UTF-8 bytes, including line breaks. A successful edit enters the desk's undo history. The review listener binds to localhost. Keep the credential private. A second computer requires an existing authenticated tunnel to the desk or a terminal session on it. [QLab Collaboration](https://qlab.app/docs/v5/networking/collaboration/)

### Flag a cue from the review terminal

With cue review enabled in the same practice show, choose an unflagged cue. Read its flag and set it using the value you observed.

```sh
inkue-inspect --live "$REVIEW_FILE" review-flag "$LIST_ID" "$CUE_ID"
inkue-inspect --live "$REVIEW_FILE" review-set-flag "$LIST_ID" "$CUE_ID" \
  --expected clear --flag set
```

Check the flag beside the cue and the new **Flag set** entry in **File → Cue review history…**. Repeat the write with `--expected clear` to see a conflict. When you finish reviewing, read the flag again and use `--expected set --flag clear` to clear it. With playback stopped, Undo or Redo only changes the flag in that cue list. Standby and armed wall-clock triggers stay in place.

### Rename a cue from the review terminal

In **Agent access → Cue review**, check **Allow cue renaming** before enabling the session. Use the newly issued review file. Run `review-cues` to look up the cue's exact current name, including any spaces, then use that name with `--expected`.

```sh
inkue-inspect --live "$REVIEW_FILE" review-cues "$LIST_ID"
inkue-inspect --live "$REVIEW_FILE" review-set-name "$LIST_ID" "$CUE_ID" \
  --expected 'Opening call' --name 'Opening call after blackout'
```

Check the updated name and the **Cue renamed** row in **File → Cue review history…**. If the expected name has changed, read the cue again and reconcile your edit. On the primary desk, Undo and Redo restore the name in place. To change a reviewer's permission, revoke the session and enable a new one with the desired setting.

## Check review edits at the primary desk

1. In a practice show, apply a review note edit from the terminal exercise above. In programming mode on the primary desk, choose **File → Cue review history…**. Find the new entry by its cue list, cue number and time.
2. With playback stopped, Undo the review edit in its cue list. Check the restored note in the Inspector, then refresh the history. The entry still records the original review write.
3. Rename the cue on the desk and choose **Refresh** in the history. The recorded entry shows the current label.

Cue review history displays up to 50 review edits from the workspace’s most recent 4,096 programming events. Each row records the edit time, the type of change, and the cue’s current label or marks the target as removed. The panel displays review activity while the regular desk controls remain available. [QLab collaboration and shared undo](https://qlab.app/docs/v5/networking/collaboration/#undo-and-redo)

## Lock assistant transport during a performance

Enter **Show Mode** with **F5**. Entries in **Agent access** remain available for inspection. Entering Show Mode clears the 60-second arm state and cancels any action awaiting approval. An assistant can still preview a proposed action. Every approval request stays blocked while Show Mode is in use. The main desk keeps its normal GO, Stop and Hard stop controls. When you return to programming, explicitly arm a new request if you need one.
