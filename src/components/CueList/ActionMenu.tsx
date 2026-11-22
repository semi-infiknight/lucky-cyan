// "Action" menu-bar dropdown — operations that act on the cue list as a whole
// rather than on the current selection (those live in the Edit menu).

import { useState } from "react";

import {
  clearAllCueNumbers,
  movePlayheadToSelection,
  renumberAll,
} from "../../lib/cueOperations";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { MenuBarMenu, type MenuBarItem } from "../MenuBar/MenuBarMenu";
import { RenumberDialog } from "./RenumberDialog";

export function ActionMenu({ onDone }: { onDone: () => void }) {
  const [renumberOpen, setRenumberOpen] = useState(false);
  const selectedIds = useWorkspaceStore((s) => s.selectedCueIds);
  const hasSelection = useWorkspaceStore((s) => s.selectedCueId !== null);

  const items: MenuBarItem[] = [
    { type: "item", label: "Renumber All Cues", onClick: () => void renumberAll(onDone) },
    { type: "item", label: "Renumber Selected…", disabled: selectedIds.length === 0,
      onClick: () => setRenumberOpen(true) },
    { type: "item", label: "Clear All Cue Numbers", onClick: () => void clearAllCueNumbers(onDone) },
    { type: "separator" },
    { type: "item", label: "Set Playhead to Selected", disabled: !hasSelection,
      onClick: () => void movePlayheadToSelection(onDone) },
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
