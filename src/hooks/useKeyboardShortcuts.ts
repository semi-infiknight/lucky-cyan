// Global keyboard shortcut handler, mirroring QLab's key bindings.

import { useEffect, useRef } from "react";

import { flattenCues } from "../lib/cueTree";
import { commandModifier as cmdOrCtrl, cueHotkey, isShowShortcut } from "../lib/shortcuts";
import {
  go,
  hardStopAll,
  fireCueHotkey,
  stopAll,
  stopCue,
  pauseCue,
  resumeCue,
  addCue,
  setPlayhead,
} from "../lib/commands";
// Edit operations are shared with the Edit / Action menus so a shortcut and
// its menu entry can never behave differently.
import {
  copySelection,
  deleteSelection,
  duplicateSelection,
  groupSelection,
  pasteAfterSelection,
  redoAction,
  selectAllCues,
  undoAction,
} from "../lib/cueOperations";
import { useWorkspaceStore } from "../stores/workspaceStore";

export function useKeyboardShortcuts(
  onRefresh: () => void,
  onOpenPreferences?: () => void,
  onSave?: () => void,
  onOpen?: () => void,
  onToggleInspector?: () => void,
  onGoto?: () => void,
  onToggleOutputWindow?: () => void,
  onToggleShowMode?: () => void,
  onToggleSearch?: () => void,
  showMode = false,
  dialogOpen = false,
  onSaveAs?: () => void,
  onContextHelp?: () => void,
  onPasteProperties?: () => void,
) {
  const lastEscapeRef = useRef<number | null>(null);
  const lastGoRef = useRef<number>(0);
  const { selectedCueId, generalPrefs } = useWorkspaceStore();

  useEffect(() => {
    const handler = async (e: KeyboardEvent) => {
      if (dialogOpen || e.defaultPrevented) return;
      if (e.key === "F1" && !e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey && !showMode && onContextHelp) {
        e.preventDefault();
        if (!e.repeat) onContextHelp();
        return;
      }
      const target = e.target as HTMLElement;
      // Let the focused element perform its native activation. Enter may click a
      // button on every keydown, so cancel its repeat before browser defaults run.
      const button = target.closest?.("button, a[href], [role='button']");
      if (button && (e.key === " " || e.key === "Enter")) {
        if (e.repeat) e.preventDefault();
        return;
      }
      if (target.closest?.("select")) return;
      const agentControl = target.closest?.("[data-agent-controls]");
      if (agentControl && e.key !== "Escape") return;
      const binding = cueHotkey(e);
      if (binding && e.isTrusted && !e.repeat && document.hasFocus() && !target.isContentEditable &&
          !target.closest?.("input, textarea, select, [role='textbox']") &&
          target.closest?.("[data-cue-hotkeys]") &&
          !document.querySelector("[aria-modal='true'], dialog[open], [data-cue-hotkeys-blocked]")) {
        const { workspaceInfo, activeCueListId, cues } = useWorkspaceStore.getState();
        const cue = flattenCues(cues).find(item => item.hotkey === binding);
        const owner = cue?.cue_list_owner;
        if (cue && owner && owner.workspace_id === workspaceInfo?.instance_id && owner.list_id === activeCueListId) {
          e.preventDefault();
          await fireCueHotkey(owner.workspace_id, owner.list_id, cue.id, binding).catch(console.error);
          return;
        }
      }
      if (showMode && !isShowShortcut(e)) return;

      // Find opens or focuses the desk bar before the text-input guard.
      if ((e.key === "f" || e.key === "F") && cmdOrCtrl(e)) {
        e.preventDefault();
        onToggleSearch?.();
        return;
      }

      // Ignore the remaining shortcuts when typing in an input / textarea.
      if (
        (target.tagName === "INPUT" && !agentControl) ||
        target.tagName === "TEXTAREA" ||
        target.isContentEditable
      ) {
        return;
      }

      switch (e.key) {
        case " ": {
          // Space → GO (with double-GO protection)
          e.preventDefault();
          const now = Date.now();
          if (e.repeat) break;
          const protection = generalPrefs.double_go_protection_ms;
          if (protection > 0 && now - lastGoRef.current < protection) break;
          lastGoRef.current = now;
          await go().catch(console.error);
          onRefresh();
          break;
        }
        case "Escape": {
          // Count distinct presses, even when the first native stop is still pending.
          e.preventDefault();
          if (e.repeat) break;
          const now = performance.now();
          const hardStop = lastEscapeRef.current !== null && now - lastEscapeRef.current < 500;
          lastEscapeRef.current = hardStop ? null : now;
          if (hardStop) {
            await hardStopAll().catch(console.error);
          } else {
            await stopAll().catch(console.error);
          }
          onRefresh();
          break;
        }
        case "s":
        case "S": {
          if (cmdOrCtrl(e)) {
            e.preventDefault();
            if (e.shiftKey) onSaveAs?.();
            else onSave?.();
          } else if (selectedCueId) {
            await stopCue(selectedCueId).catch(console.error);
            onRefresh();
          }
          break;
        }
        case "o":
        case "O": {
          if (cmdOrCtrl(e)) {
            e.preventDefault();
            onOpen?.();
          }
          break;
        }
        case "i":
        case "I": {
          if (cmdOrCtrl(e)) {
            e.preventDefault();
            onToggleInspector?.();
          }
          break;
        }
        case "p":
        case "P":
        case "[": {
          if (!cmdOrCtrl(e) && selectedCueId) {
            await pauseCue(selectedCueId).catch(console.error);
            onRefresh();
          }
          break;
        }
        case "]": {
          if (selectedCueId) {
            await resumeCue(selectedCueId).catch(console.error);
            onRefresh();
          }
          break;
        }
        case ",": {
          if (cmdOrCtrl(e)) {
            e.preventDefault();
            onOpenPreferences?.();
          }
          break;
        }
        case "ArrowUp": {
          if (cmdOrCtrl(e)) {
            e.preventDefault();
            const { cues, playheadCueId } = useWorkspaceStore.getState();
            const idx = cues.findIndex((c) => c.id === playheadCueId);
            const prevCue = idx > 0 ? cues[idx - 1] : cues[0];
            if (prevCue) {
              await setPlayhead(prevCue.id).catch(console.error);
              onRefresh();
            }
          }
          break;
        }
        case "ArrowDown": {
          if (cmdOrCtrl(e)) {
            e.preventDefault();
            const { cues, playheadCueId } = useWorkspaceStore.getState();
            const idx = cues.findIndex((c) => c.id === playheadCueId);
            const nextCue = idx < cues.length - 1 ? cues[idx + 1] : cues[cues.length - 1];
            if (nextCue) {
              await setPlayhead(nextCue.id).catch(console.error);
              onRefresh();
            }
          }
          break;
        }
        case "a":
        case "A": {
          if (cmdOrCtrl(e)) {
            e.preventDefault();
            selectAllCues();
          }
          break;
        }
        case "g":
        case "G": {
          if (cmdOrCtrl(e)) {
            e.preventDefault();
            await groupSelection(onRefresh);
          } else if (!cmdOrCtrl(e) && !e.shiftKey && !e.altKey) {
            e.preventDefault();
            onGoto?.();
          }
          break;
        }
        case "n":
        case "N": {
          if (cmdOrCtrl(e)) {
            e.preventDefault();
            await addCue("audio").catch(console.error);
            onRefresh();
          }
          break;
        }
        case "d":
        case "D": {
          if (cmdOrCtrl(e) && selectedCueId) {
            e.preventDefault();
            await duplicateSelection(onRefresh);
          }
          break;
        }
        case "z":
        case "Z": {
          if (cmdOrCtrl(e) && e.shiftKey) {
            // Ctrl+Shift+Z → Redo (alternative to Ctrl+Y)
            e.preventDefault();
            await redoAction(onRefresh);
          } else if (cmdOrCtrl(e)) {
            // Ctrl+Z → Undo
            e.preventDefault();
            await undoAction(onRefresh);
          }
          break;
        }
        case "y":
        case "Y": {
          if (cmdOrCtrl(e)) {
            e.preventDefault();
            await redoAction(onRefresh);
          }
          break;
        }
        case "c":
        case "C": {
          if (cmdOrCtrl(e) && selectedCueId) {
            e.preventDefault();
            await copySelection();
          }
          break;
        }
        case "v":
        case "V": {
          if (cmdOrCtrl(e)) {
            e.preventDefault();
            if (e.shiftKey) {
              if (!e.repeat) onPasteProperties?.();
            } else {
              await pasteAfterSelection(onRefresh);
            }
          }
          break;
        }
        case "Delete":
        case "Backspace": {
          if (selectedCueId && cmdOrCtrl(e) === false) {
            await deleteSelection(onRefresh, generalPrefs.confirm_before_delete);
          }
          break;
        }
        case "F5": {
          e.preventDefault();
          onToggleShowMode?.();
          break;
        }
        case "F9": {
          e.preventDefault();
          onToggleOutputWindow?.();
          break;
        }
        default:
          break;
      }
    };

    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [selectedCueId, generalPrefs, onRefresh, onOpenPreferences, onSave, onOpen, onToggleInspector, onGoto, onToggleOutputWindow, onToggleShowMode, onToggleSearch, showMode, dialogOpen, onSaveAs, onContextHelp, onPasteProperties]);
}
