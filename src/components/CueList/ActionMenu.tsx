// "Action" menu-bar dropdown — operations that act on the cue list as a whole
// rather than on the current selection (those live in the Edit menu).

import { useState } from "react";
import { confirm, message } from "@tauri-apps/plugin-dialog";

import {
  clearAllCueNumbers,
  movePlayheadToSelection,
  renumberAll,
} from "../../lib/cueOperations";
import { resetWorkspaceTransport } from "../../lib/commands";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { MenuBarMenu, type MenuBarItem } from "../MenuBar/MenuBarMenu";
import { RenumberDialog } from "./RenumberDialog";

export function ActionMenu({ onDone }: { onDone: () => void }) {
  const [renumberOpen, setRenumberOpen] = useState(false);
  const selectedIds = useWorkspaceStore((s) => s.selectedCueIds);
  const hasSelection = useWorkspaceStore((s) => s.selectedCueId !== null);
  const workspaceId = useWorkspaceStore((s) => s.workspaceInfo?.instance_id);

  async function resetWorkspace() {
    if (!workspaceId) return;
    try {
      if (await confirm("Stop playback in every list, clear temporary cue changes, and return to the first cue in each list?", {
        title: "Reset Workspace", kind: "warning",
      })) {
        await resetWorkspaceTransport(workspaceId);
      }
    } catch (error) {
      await message(String(error), { title: "Reset Workspace", kind: "error" });
    }
  }

  const items: MenuBarItem[] = [
    { type: "item", label: "Renumber All Cues", onClick: () => void renumberAll(onDone) },
    { type: "item", label: "Renumber Selected…", disabled: selectedIds.length === 0,
      onClick: () => setRenumberOpen(true) },
    { type: "item", label: "Clear All Cue Numbers", onClick: () => void clearAllCueNumbers(onDone) },
    { type: "separator" },
    { type: "item", label: "Set Playhead to Selected", disabled: !hasSelection,
      onClick: () => void movePlayheadToSelection(onDone) },
    { type: "separator" },
    { type: "item", label: "Reset Workspace…", disabled: !workspaceId,
      onClick: () => void resetWorkspace() },
  ];

  return (
    <>
      <MenuBarMenu label="Action" title="Cue-list actions" items={items} minWidth={220} />
      {renumberOpen && (
        <RenumberDialog
          targetIds={selectedIds}
          onClose={() => setRenumberOpen(false)}
          onRefresh={onDone}
        />
      )}
    </>
  );
}
