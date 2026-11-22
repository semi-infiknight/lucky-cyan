// Group Cue main tab: playback mode and per-mode options.

import { useState } from "react";
import type { CueSummary, GroupMode, PlaylistTriggerSetting } from "../../lib/types";
import { setGroupMode, setPlaylistAdvancePlayhead, setPlaylistAutoShuffle, setPlaylistLoop, updateCue } from "../../lib/commands";
import { Section, ToggleRow, inputStyle } from "./Field";
import { Select } from "../common/Select";
import { useWorkspaceStore } from "../../stores/workspaceStore";

const MODE_HINTS: Record<GroupMode, string> = {
  simultaneous: "GO starts every child at once (use child pre-waits for a timeline).",
  sequential: "GO starts the first child; each child triggers the next.",
  start_first: "GO starts the first armed child and moves standby past the group. Continue settings run the child sequence.",
  start_first_and_enter: "GO starts the first armed child and puts standby on the next child. Standby leaves the group after its last child starts.",
  playlist: "Children play one at a time in order.",
  start_random: "GO starts one armed, idle child. Each armed child gets a turn before any repeats.",
};

function PlaylistShuffle({
  cue, owner, isPlaying, onRefresh,
}: {
  cue: CueSummary;
  owner: PlaylistTriggerSetting;
  isPlaying: boolean;
  onRefresh: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const expected = cue.playlist_auto_shuffle;
  if (expected === undefined) return null;

  const setShuffle = async (enabled: boolean) => {
    setBusy(true);
    setError("");
    try {
      await setPlaylistAutoShuffle({
        workspace_id: owner.workspace_id,
        list_id: owner.list_id,
        group_id: cue.id,
        expected,
        enabled,
      });
    } catch (cause) {
      setError(String(cause));
    } finally {
      onRefresh();
      setBusy(false);
    }
  };

  const children = new Map(cue.children?.map(child => [child.id, child]));
  return <>
    <ToggleRow label="Auto-shuffle" checked={expected} disabled={isPlaying || busy} onToggle={setShuffle} />
    {error && <p role="alert">{error}</p>}
    {!!cue.playlist_run_order?.length && (
      <section aria-label="Playback order" style={{ marginTop: 8 }}>
        <div>Playback order</div>
        <ol style={{ margin: "4px 0", paddingInlineStart: 20, fontStyle: "italic" }}>
          {cue.playlist_run_order.map(id => {
            const child = children.get(id);
            if (!child) return null;
            return <li key={id}>{[child.number, child.name].filter(Boolean).join(" · ")}
              {child.is_disabled && " (disarmed)"}</li>;
          })}
        </ol>
      </section>
    )}
  </>;
}

export function GroupTab({
  cue,
  isPlaying,
  playlistSummary,
  onRefresh,
}: {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  cue: any;
  isPlaying: boolean;
  playlistSummary: CueSummary;
  onRefresh: () => void;
}) {
  const mode: GroupMode = cue.group_mode ?? "simultaneous";
  const advances = playlistSummary.playlist_advance_playhead ?? cue.playlist_advance_playhead ?? false;
  const owner = playlistSummary.playlist_second_trigger;
  const workspaceId = useWorkspaceStore(state => state.workspaceInfo?.instance_id);
  const listId = useWorkspaceStore(state => state.activeCueListId);

  return (
    <Section title="Mode">
      <Select
        style={inputStyle}
        value={mode}
        disabled={isPlaying && (mode === "start_first" || mode === "start_first_and_enter" || (mode === "playlist" && advances))}
        onChange={async (e) => {
          await setGroupMode(cue.id, e.target.value as GroupMode).catch(console.error);
          onRefresh();
        }}
      >
        <option value="simultaneous">Simultaneous</option>
        <option value="sequential">Sequential</option>
        <option value="start_first" disabled={isPlaying}>Start First</option>
        <option value="start_first_and_enter" disabled={isPlaying}>Start First And Enter</option>
        <option value="playlist" disabled={isPlaying}>Playlist</option>
        <option value="start_random">Start Random</option>
      </Select>
      <div style={{ fontSize: 11, color: "var(--wc-text-faint)", margin: "6px 0 10px" }}>
        {MODE_HINTS[mode]}
      </div>
      {mode === "playlist" && (
        <>
          <ToggleRow
            label="Advance playhead after GO"
            checked={advances}
            disabled={isPlaying}
            onToggle={async (advance) => {
              await setPlaylistAdvancePlayhead(cue.id, advance).catch(console.error);
              onRefresh();
            }}
          />
          <ToggleRow
            label="Loop back to the first child"
            checked={cue.playlist_loop ?? false}
            onToggle={async (v) => {
              await setPlaylistLoop(cue.id, v).catch(console.error);
              onRefresh();
            }}
          />
          {owner && owner.workspace_id === workspaceId && owner.list_id === listId && (
            <PlaylistShuffle
              key={`${owner.workspace_id}:${owner.list_id}:${playlistSummary.id}`}
              cue={playlistSummary}
              owner={owner}
              isPlaying={isPlaying}
              onRefresh={onRefresh}
            />
          )}
        </>
      )}
      {mode === "start_random" && (
        <ToggleRow
          label="Advance playhead after GO"
          checked={cue.random_advance_playhead ?? false}
          onToggle={async (advance) => {
            await updateCue(cue.id, { random_advance_playhead: advance }).catch(console.error);
            onRefresh();
          }}
        />
      )}
    </Section>
  );
}
