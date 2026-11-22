import { resetCueColors, setAgentPerformanceLock } from "./lib/commands";
import { manualHome, manualChapterFor } from "./components/Operator/manualTopic";
// Root application layout — mirrors QLab's three-zone layout.

import { lazy, Suspense, useEffect, useState, useCallback, useRef } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { CueListView } from "./components/CueList/CueListView";
import { CueFindBar, restoreFindFocus } from "./components/CueList/CueFindBar";
import { ActionMenu } from "./components/CueList/ActionMenu";
import { PasteCuePropertiesDialog } from "./components/CueList/PasteCuePropertiesDialog";
import { EditMenu } from "./components/MenuBar/EditMenu";
import { CartView } from "./components/CueList/CartView";
import { ShowModeView } from "./components/ShowMode/ShowModeView";
import { ActiveCuesView } from "./components/ActiveCues/ActiveCuesView";
import { CueListTabs } from "./components/CueList/CueListTabs";
import { InspectorPanel } from "./components/Inspector/InspectorPanel";
import { ClipEditorDock } from "./components/Editor/ClipEditorDock";
import { CurveEditorDock } from "./components/Curve/CurveEditorDock";
import { TransportBar } from "./components/Transport/TransportBar";
import { useTauriEvents } from "./hooks/useTauriEvents";
import { useKeyboardShortcuts } from "./hooks/useKeyboardShortcuts";
import { useWorkspaceStore } from "./stores/workspaceStore";
import { addCue, collectAndSave, acceptAudioSheet, takeLaunchDocument, importQlabWorkspace, openOperatorRehearsal, saveWorkspace, loadWorkspace, newWorkspace, goToCueNumber, toggleOutputWindow, getOutputWindowVisible, openPreferencesWindow, getCueLists, checkRecovery, restoreRecovery, discardRecovery } from "./lib/commands";
import { newWorkspaceFromTemplate, saveWorkspaceTemplate } from "./lib/commands";
import { AboutDialog } from "./components/About/AboutDialog";
import { UpdateDialog } from "./components/Update/UpdateDialog";
import { useUpdateStore } from "./stores/updateStore";
import { InkueMark } from "./components/common/InkueMark";
import { PreflightModal } from "./components/Preflight/PreflightModal";
import { LogViewerModal } from "./components/Logs/LogViewerModal";
import { HealthBanner } from "./components/Health/HealthBanner";
import type { CollectReport, ImportReport, RecoveryInfo, CueType } from "./lib/types";
import { modifierLabel } from "./lib/shortcuts";
import { AgentPanel } from "./components/Operator/AgentPanel";
import { CueReviewHistoryPanel } from "./components/Operator/CueReviewHistoryPanel";
import { SavedRevisionsDialog } from "./components/Operator/SavedRevisionsDialog";
import { TEMPLATE_EXTENSION, templateFilter, WorkspaceLauncher } from "./components/Operator/WorkspaceLauncher";
import { SettingsTransferDialog } from "./components/Operator/SettingsTransferDialog";
import { AudioSheetDialog } from "./components/Operator/AudioSheetDialog";
import { OperatorGuide } from "./components/Operator/OperatorDialog";
import { QlabImportDialog } from "./components/Import/QlabImportDialog";
import type { CueSummary } from "./lib/types";
import { COMMAND_CUE_TYPES, CUE_TYPE_COLORS } from "./lib/types";

// ---------------------------------------------------------------------------
// Recent files
// ---------------------------------------------------------------------------

const OperatorManual = lazy(() => import("./components/Operator/OperatorManual"));
const RECENT_FILES_KEY = "inkue_recent_files";
const MAX_RECENT = 8;

function loadRecentFiles(): string[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(RECENT_FILES_KEY) ?? "[]");
    if (!Array.isArray(parsed)) return [];
    return [...new Set(parsed.filter((path): path is string => typeof path === "string" && path.trim() !== ""))]
      .slice(0, MAX_RECENT);
  } catch { return []; }
}

function pushRecentFile(path: string): string[] {
  const list = loadRecentFiles().filter((p) => p !== path);
  list.unshift(path);
  const trimmed = list.slice(0, MAX_RECENT);
  try { localStorage.setItem(RECENT_FILES_KEY, JSON.stringify(trimmed)); } catch { /* ignore */ }
  return trimmed;
}

// ---------------------------------------------------------------------------
// Window control buttons (minimize / maximize / close)
// ---------------------------------------------------------------------------

function WindowControls() {
  const [hovered, setHovered] = useState<"min" | "max" | "close" | null>(null);

  const handleMin   = () => void getCurrentWindow().minimize();
  const handleMax   = () => void getCurrentWindow().toggleMaximize();
  // Close goes through the normal close path so onCloseRequested fires.
  const handleClose = () => void getCurrentWindow().close();

  const btn = (
    key: "min" | "max" | "close",
    label: string,
    color: string,
    hoverColor: string,
    onClick: () => void,
  ) => (
    <button
      key={key}
      title={label}
      onClick={onClick}
      onMouseEnter={() => setHovered(key)}
      onMouseLeave={() => setHovered(null)}
      style={{
        width: 13, height: 13, borderRadius: "50%", border: "none",
        background: hovered === key ? hoverColor : color,
        cursor: "pointer", display: "flex", alignItems: "center",
        justifyContent: "center", padding: 0, flexShrink: 0,
        fontSize: 8,
        color: hovered === key ? "rgba(0,0,0,0.6)" : "transparent",
        transition: "background 0.1s",
      }}
    >
      {hovered === key ? label : ""}
    </button>
  );

  return (
    <div style={{ display: "flex", gap: 7, alignItems: "center", flexShrink: 0 }}>
      {btn("close", "✕", "#ef4444", "#dc2626", handleClose)}
      {btn("min",   "–", "#f59e0b", "#d97706", handleMin)}
      {btn("max",   "▢", "#22c55e", "#16a34a", handleMax)}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Unsaved-changes close confirmation dialog
// ---------------------------------------------------------------------------

function CloseConfirmDialog({
  onSave,
  onDiscard,
  onCancel,
}: {
  onSave: () => void;
  onDiscard: () => void;
  onCancel: () => void;
}) {
  return (
    <div
      style={{
        position: "fixed", inset: 0, zIndex: 99999,
        background: "rgba(0,0,0,0.6)",
        display: "flex", alignItems: "center", justifyContent: "center",
      }}
    >
      <div
        style={{
          background: "var(--wc-bg-surface)", border: "1px solid var(--wc-border-strong)",
          borderRadius: 10, padding: "28px 32px", width: 360,
          boxShadow: "0 16px 48px rgba(0,0,0,0.8)",
        }}
      >
        <div style={{ fontSize: 15, fontWeight: 600, color: "var(--wc-text-bright)", marginBottom: 8 }}>
          Unsaved Changes
        </div>
        <div style={{ fontSize: 13, color: "var(--wc-text-secondary)", marginBottom: 24 }}>
          Do you want to save your workspace before continuing?
        </div>
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
          <DialogBtn label="Cancel"    onClick={onCancel}  />
          <DialogBtn label="Don't Save" onClick={onDiscard} danger />
          <DialogBtn label="Save"       onClick={onSave}    primary />
        </div>
      </div>
    </div>
  );
}

function DialogBtn({
  label, onClick, primary, danger,
}: {
  label: string; onClick: () => void; primary?: boolean; danger?: boolean;
}) {
  const [hov, setHov] = useState(false);
  const bg = primary
    ? hov ? "var(--wc-accent-hover)" : "var(--wc-accent)"
    : danger
      ? hov ? "#dc2626" : "#b91c1c"
      : hov ? "var(--wc-bg-hover)" : "var(--wc-bg-surface)";
  const color = primary ? "var(--wc-accent-fg)" : danger ? "#fff" : "var(--wc-text)";
  return (
    <button
      onClick={onClick}
      onMouseEnter={() => setHov(true)}
      onMouseLeave={() => setHov(false)}
      style={{
        padding: "6px 16px", border: "1px solid var(--wc-border-strong)", borderRadius: 6,
        background: bg, color, fontSize: 13, cursor: "pointer",
      }}
    >
      {label}
    </button>
  );
}

// ---------------------------------------------------------------------------
// Crash-recovery dialog — shown on startup when a recovery snapshot exists
// ---------------------------------------------------------------------------

function RecoveryDialog({
  info,
  onRecover,
  onDiscard,
}: {
  info: RecoveryInfo;
  onRecover: () => void;
  onDiscard: () => void;
}) {
  const label = info.name || "Untitled";
  const when = info.modified_at
    ? new Date(info.modified_at).toLocaleString()
    : "recently";

  return (
    <div
      style={{
        position: "fixed", inset: 0, zIndex: 99999,
        background: "rgba(0,0,0,0.6)",
        display: "flex", alignItems: "center", justifyContent: "center",
      }}
    >
      <div
        style={{
          background: "var(--wc-bg-surface)", border: "1px solid var(--wc-border-strong)",
          borderRadius: 10, padding: "28px 32px", width: 380,
          boxShadow: "0 16px 48px rgba(0,0,0,0.8)",
        }}
      >
        <div style={{ fontSize: 15, fontWeight: 600, color: "var(--wc-text-bright)", marginBottom: 8 }}>
          Recover Unsaved Work
        </div>
        <div style={{ fontSize: 13, color: "var(--wc-text-secondary)", marginBottom: 6 }}>
          Inkue did not close properly.
        </div>
        <div style={{ fontSize: 13, color: "var(--wc-text)", marginBottom: 24 }}>
          Recover unsaved work from{" "}
          <span style={{ color: "var(--wc-text-bright)", fontWeight: 500 }}>"{label}"</span>
          {" "}(last edited {when})?
        </div>
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
          <DialogBtn label="Discard" onClick={onDiscard} danger />
          <DialogBtn label="Recover" onClick={onRecover} primary />
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Goto cue dialog (G key) — type a cue number to move the playhead
// ---------------------------------------------------------------------------

function GotoDialog({
  onClose,
  onParked,
}: {
  onClose: () => void;
  onParked: (workspaceId: string, listId: string, id: string) => void;
}) {
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [owner] = useState(() => {
    const { workspaceInfo, activeCueListId } = useWorkspaceStore.getState();
    return { workspaceId: workspaceInfo?.instance_id, listId: activeCueListId };
  });
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const commit = async () => {
    if (pending) return;
    const q = value.trim();
    if (!q) { onClose(); return; }
    if (!owner.workspaceId || !owner.listId) {
      setError("Open a cue list first.");
      return;
    }
    setPending(true);
    try {
      const id = await goToCueNumber(owner.workspaceId, owner.listId, q);
      onParked(owner.workspaceId, owner.listId, id);
      onClose();
    } catch (error) {
      setError(String(error));
      inputRef.current?.select();
    } finally {
      setPending(false);
    }
  };

  const handleKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") { e.preventDefault(); void commit(); }
    if (e.key === "Escape") { e.preventDefault(); onClose(); }
  };

  return (
    <div
      style={{
        position: "fixed", inset: 0, zIndex: 99998,
        display: "flex", alignItems: "center", justifyContent: "center",
      }}
      onClick={onClose}
    >
      <div
        style={{
          background: "var(--wc-bg-surface)", border: "1px solid var(--wc-text-faint)",
          borderRadius: 8, padding: "16px 20px", width: 280,
          boxShadow: "0 12px 40px rgba(0,0,0,0.8)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div style={{ fontSize: 12, color: "var(--wc-text-secondary)", marginBottom: 8 }}>
          Go to cue number
        </div>
        <input
          ref={inputRef}
          value={value}
          aria-label="Go to cue number"
          aria-invalid={Boolean(error)}
          readOnly={pending}
          onChange={(e) => { setValue(e.target.value); setError(null); }}
          onKeyDown={handleKey}
          placeholder="e.g. 1, 1.5, Intro"
          style={{
            width: "100%", boxSizing: "border-box",
            background: "var(--wc-bg-app)", border: "1px solid var(--wc-border-strong)",
            borderRadius: 5, color: "var(--wc-text-bright)", fontSize: 14,
            padding: "7px 10px", outline: "none",
          }}
        />
        {error && <div role="alert" style={{ fontSize: 11, marginTop: 8 }}>{error}</div>}
        <div style={{ fontSize: 11, color: "var(--wc-text-faint)", marginTop: 8 }}>
          Enter to confirm · Escape to cancel
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// File menu
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Collect & Save result dialog
// ---------------------------------------------------------------------------

function CollectResultDialog({
  report,
  onClose,
}: {
  report: CollectReport;
  onClose: () => void;
}) {
  const hasMissing = report.files_missing.length > 0;
  return (
    <div
      style={{
        position: "fixed", inset: 0, zIndex: 99999,
        background: "rgba(0,0,0,0.6)", display: "flex",
        alignItems: "center", justifyContent: "center",
      }}
      onClick={onClose}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          background: "var(--wc-bg-surface)", border: "1px solid var(--wc-border-strong)",
          borderRadius: 10, padding: "24px 28px", maxWidth: 520, width: "90%",
          boxShadow: "0 16px 48px rgba(0,0,0,0.8)",
        }}
      >
        <h3 style={{ margin: "0 0 16px", fontSize: 15, color: "var(--wc-text)" }}>
          Collect &amp; Save Complete
        </h3>

        <div style={{ fontSize: 12, color: "var(--wc-text-muted)", marginBottom: 14, fontFamily: "monospace", wordBreak: "break-all" }}>
          {report.workspace_path}
        </div>

        <div style={{ display: "flex", flexDirection: "column", gap: 6, fontSize: 13, marginBottom: 16 }}>
          <div style={{ color: "var(--wc-text)" }}>
            <span style={{ color: "#4ade80" }}>✓</span>{" "}
            {report.files_copied} file{report.files_copied !== 1 ? "s" : ""} copied
          </div>
          {report.files_skipped > 0 && (
            <div style={{ color: "var(--wc-text-muted)" }}>
              — {report.files_skipped} already in place (skipped)
            </div>
          )}
          {hasMissing && (
            <div style={{ color: "#f87171" }}>
              ⚠ {report.files_missing.length} file{report.files_missing.length !== 1 ? "s" : ""} missing from disk
            </div>
          )}
        </div>

        {hasMissing && (
          <div style={{
            background: "rgba(239,68,68,0.08)", border: "1px solid rgba(239,68,68,0.3)",
            borderRadius: 6, padding: "8px 12px", marginBottom: 16,
            maxHeight: 140, overflowY: "auto",
          }}>
            {report.files_missing.map((p) => (
              <div key={p} style={{ fontSize: 11, color: "#fca5a5", fontFamily: "monospace", wordBreak: "break-all", marginBottom: 4 }}>
                {p}
              </div>
            ))}
          </div>
        )}

        <div style={{ display: "flex", justifyContent: "flex-end" }}>
          <button
            onClick={onClose}
            style={{
              background: "var(--wc-bg-hover)", border: "1px solid var(--wc-border-strong)",
              borderRadius: 6, color: "var(--wc-text)", cursor: "pointer",
              fontSize: 13, padding: "6px 18px",
            }}
          >
            OK
          </button>
        </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// File menu
// ---------------------------------------------------------------------------

function FileMenu({
  onSave,
  onSaveAs,
  onSaveAsTemplate,
  onOpen,
  onNew,
  onNewFromTemplate,
  onImportQlab,
  onOperatorGuide,
  onManual,
  onAudioSheet,
  onSettingsTransfer,
  onSavedRevisions,
  onCueReviewHistory,
  onCollect,
  onCheck,
  onLogs,
  onPreferences,
  onAbout,
  recentFiles,
  onOpenRecent,
}: {
  onSave: () => void;
  onSaveAs: () => void;
  onSaveAsTemplate: () => void;
  onOpen: () => void;
  onNew: () => void;
  onNewFromTemplate: () => void;
  onImportQlab: () => void;
  onOperatorGuide: () => void;
  onManual: () => void;
  onAudioSheet: () => void;
  onSavedRevisions: () => void;
  onCueReviewHistory: () => void;
  onSettingsTransfer: (mode: "export" | "import") => void;
  onCollect: () => void;
  onCheck: () => void;
  onLogs: () => void;
  onPreferences: () => void;
  onAbout: () => void;
  recentFiles: string[];
  onOpenRecent: (path: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [hovered, setHovered] = useState<string | null>(null);

  const close = () => setOpen(false);

  const act = (fn: () => void) => () => { close(); fn(); };

  const handleQuit = () => {
    close();
    void getCurrentWindow().close();
  };

  const menuItems: Array<
    | { type: "item"; label: string; shortcut?: string; action: () => void; muted?: boolean }
    | { type: "separator" }
  > = [
    { type: "item", label: "New Workspace",    action: act(onNew) },
    { type: "item", label: "New from Template…", action: act(onNewFromTemplate) },
    { type: "item", label: "Open…",           shortcut: `${modifierLabel}+O`,         action: act(onOpen) },
    ...(recentFiles.length > 0
      ? [
          { type: "separator" as const },
          ...recentFiles.map((p) => ({
            type: "item" as const,
            label: p.split(/[\\/]/).pop() ?? p,
            action: act(() => onOpenRecent(p)),
            muted: true,
          })),
        ]
      : []),
    { type: "separator" },
    { type: "item", label: "Save",              shortcut: `${modifierLabel}+S`,         action: act(onSave) },
    { type: "item", label: "Save As…",         shortcut: `${modifierLabel}+Shift+S`,   action: act(onSaveAs) },
    { type: "item", label: "Save as Template…", action: act(onSaveAsTemplate) },
    { type: "item", label: "Saved revisions…", action: act(onSavedRevisions) },
    { type: "item", label: "Cue review history…", action: act(onCueReviewHistory) },
    { type: "item", label: "Collect and Save…",                            action: act(onCollect) },
    { type: "separator" },
    { type: "item", label: "Review audio cue sheet…", action: act(onAudioSheet) },
    { type: "item", label: "Export workspace settings…", action: act(() => onSettingsTransfer("export")) },
    { type: "item", label: "Import workspace settings…", action: act(() => onSettingsTransfer("import")) },
    { type: "item", label: "QLab operator guide…", action: act(onOperatorGuide) },
    { type: "item", label: "Operator manual and tutorials…", action: act(onManual) },
    { type: "item", label: "Import QLab Workspace… (BETA)",                action: act(onImportQlab) },
    { type: "separator" },
    { type: "item", label: "Check Workspace…",                             action: act(onCheck) },
    { type: "item", label: "Logs…",                                        action: act(onLogs) },
    { type: "separator" },
    { type: "item", label: "Preferences",     shortcut: `${modifierLabel}+,`,          action: act(onPreferences) },
    { type: "separator" },
    { type: "item", label: "About Inkue",                                 action: act(onAbout) },
    { type: "separator" },
    { type: "item", label: "Quit",                                         action: handleQuit },
  ];

  return (
    <div style={{ position: "relative", flexShrink: 0 }}>
      {open && (
        <div style={{ position: "fixed", inset: 0, zIndex: 9990 }} onClick={close} />
      )}
      <button
        onClick={(e) => { e.stopPropagation(); setOpen((v) => !v); }}
        style={{
          background: open ? "var(--wc-bg-surface)" : "transparent",
          border: "none", color: "var(--wc-text)", cursor: "pointer",
          fontSize: 12, padding: "3px 8px", borderRadius: 4, userSelect: "none",
        }}
      >
        File
      </button>
      {open && (
        <div
          style={{
            position: "absolute", left: 0, top: "100%", marginTop: 2,
            background: "var(--wc-bg-surface)", border: "1px solid var(--wc-border-strong)", borderRadius: 6,
            padding: "4px 0", minWidth: 220,
            boxShadow: "0 8px 24px rgba(0,0,0,0.7)", zIndex: 9999,
          }}
        >
          {menuItems.map((item, i) =>
            item.type === "separator" ? (
              <div key={i} style={{ height: 1, background: "var(--wc-border-strong)", margin: "4px 0" }} />
            ) : (
              <button
                key={item.label}
                onMouseEnter={() => setHovered(item.label)}
                onMouseLeave={() => setHovered(null)}
                onClick={(e) => { e.stopPropagation(); item.action(); }}
                style={{
                  display: "flex", alignItems: "center", justifyContent: "space-between",
                  width: "100%", padding: "6px 14px",
                  background: hovered === item.label ? "var(--wc-bg-hover)" : "transparent",
                  border: "none",
                  color: item.muted ? "var(--wc-text-secondary)" : "var(--wc-text)",
                  fontSize: 13, cursor: "pointer", textAlign: "left", gap: 24,
                }}
              >
                <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", maxWidth: 180 }}>
                  {item.label}
                </span>
                {item.shortcut && (
                  <span style={{ color: "var(--wc-text-muted)", fontSize: 11, flexShrink: 0 }}>{item.shortcut}</span>
                )}
              </button>
            )
          )}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// View menu
// ---------------------------------------------------------------------------

interface ViewMenuItem {
  label: string;
  checked?: boolean;
  disabled?: boolean;
  onClick: () => void;
  shortcut?: string;
}

function ViewMenu({ items }: { items: ViewMenuItem[] }) {
  const [open, setOpen] = useState(false);
  const [hovered, setHovered] = useState<string | null>(null);

  const close = () => setOpen(false);

  return (
    <div style={{ position: "relative", flexShrink: 0 }}>
      {open && (
        <div style={{ position: "fixed", inset: 0, zIndex: 9990 }} onClick={close} />
      )}
      <button
        onClick={(e) => { e.stopPropagation(); setOpen((v) => !v); }}
        style={{
          background: open ? "var(--wc-bg-surface)" : "transparent",
          border: "none", color: "var(--wc-text)", cursor: "pointer",
          fontSize: 12, padding: "3px 8px", borderRadius: 4, userSelect: "none",
        }}
      >
        View
      </button>
      {open && (
        <div
          style={{
            position: "absolute", left: 0, top: "100%", marginTop: 2,
            background: "var(--wc-bg-surface)", border: "1px solid var(--wc-border-strong)", borderRadius: 6,
            padding: "4px 0", minWidth: 200,
            boxShadow: "0 8px 24px rgba(0,0,0,0.7)", zIndex: 9999,
          }}
        >
          {items.map((item) => (
            <button
              key={item.label}
              type="button"
              disabled={item.disabled}
              onMouseEnter={() => setHovered(item.label)}
              onMouseLeave={() => setHovered(null)}
              onClick={(e) => { e.stopPropagation(); close(); item.onClick(); }}
              style={{
                display: "flex", alignItems: "center", gap: 8,
                width: "100%", padding: "6px 14px",
                background: hovered === item.label ? "var(--wc-bg-hover)" : "transparent",
                border: "none", color: item.disabled ? "var(--wc-text-muted)" : "var(--wc-text)", fontSize: 13,
                cursor: item.disabled ? "default" : "pointer", textAlign: "left",
              }}
            >
              <span style={{ width: 14, textAlign: "center", color: "var(--wc-text-secondary)" }}>
                {item.checked ? "✓" : ""}
              </span>
              <span style={{ flex: 1 }}>{item.label}</span>
              {item.shortcut && (
                <span style={{ color: "var(--wc-text-muted)", fontSize: 11 }}>{item.shortcut}</span>
              )}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Persisted UI layout (panel visibility) — mirrors the column-config pattern.
// ---------------------------------------------------------------------------

const LS_LAYOUT_KEY = "inkue_ui_layout";

interface UiLayout {
  showCueListTabs: boolean;
  inspectorOpen: boolean;
  showSearchBar: boolean;
  showActiveCues: boolean;
  inspectorWidth: number;
}

const INSPECTOR_MIN_WIDTH = 320;
const INSPECTOR_MAX_WIDTH = 560;
const INSPECTOR_DEFAULT_WIDTH = 360;

const clampInspectorWidth = (w: number) =>
  Math.min(INSPECTOR_MAX_WIDTH, Math.max(INSPECTOR_MIN_WIDTH, w));

const DEFAULT_UI_LAYOUT: UiLayout = {
  showCueListTabs: true,
  inspectorOpen: true,
  showSearchBar: true,
  showActiveCues: true,
  inspectorWidth: INSPECTOR_DEFAULT_WIDTH,
};

function loadUiLayout(): UiLayout {
  try {
    const raw = localStorage.getItem(LS_LAYOUT_KEY);
    if (!raw) return DEFAULT_UI_LAYOUT;
    const parsed = JSON.parse(raw) as Partial<UiLayout>;
    return {
      showCueListTabs: parsed.showCueListTabs ?? true,
      inspectorOpen: parsed.inspectorOpen ?? true,
      showSearchBar: parsed.showSearchBar ?? true,
      showActiveCues: parsed.showActiveCues ?? true,
      inspectorWidth: clampInspectorWidth(parsed.inspectorWidth ?? INSPECTOR_DEFAULT_WIDTH),
    };
  } catch {
    return DEFAULT_UI_LAYOUT;
  }
}

function saveUiLayout(layout: UiLayout): void {
  try {
    localStorage.setItem(LS_LAYOUT_KEY, JSON.stringify(layout));
  } catch {
    // ignore (private / storage-full)
  }
}

// ---------------------------------------------------------------------------
// Root component
// ---------------------------------------------------------------------------

function findCueRecursive(cues: CueSummary[], id: string | null): CueSummary | undefined {
  if (!id) return undefined;
  for (const cue of cues) {
    if (cue.id === id) return cue;
    if (cue.children) {
      const found = findCueRecursive(cue.children, id);
      if (found) return found;
    }
  }
  return undefined;
}

/** Quick press-and-flash micro-interaction when a toolbar button is clicked.
 *  Uses the Web Animations API so it needs no React state or re-render and
 *  plays in the button's own accent color (via the brightness boost). */
function pulseButton(el: HTMLElement) {
  el.animate(
    [
      { transform: "scale(1)",    filter: "brightness(1)" },
      { transform: "scale(0.88)", filter: "brightness(1.7)", offset: 0.35 },
      { transform: "scale(1)",    filter: "brightness(1)" },
    ],
    { duration: 260, easing: "cubic-bezier(.2,.7,.3,1)" },
  );
}

export default function App() {
  const { historyNotice, refreshCues, refreshWorkspaceInfo, refreshValidation, refreshHealth, brokenCueIds, loadGeneralPrefs, loadDisplayPrefs, displayPrefs, workspaceInfo, selectedCueId, selectedCueIds, cues, cueLists, activeCueListId } =
    useWorkspaceStore();

  const [inspectorOpen, setInspectorOpen]         = useState(() => loadUiLayout().inspectorOpen);
  const [showCueListTabs, setShowCueListTabs]     = useState(() => loadUiLayout().showCueListTabs);
  const [showSearchBar, setShowSearchBar]         = useState(() => loadUiLayout().showSearchBar);
  const [showActiveCues, setShowActiveCues]       = useState(() => loadUiLayout().showActiveCues);
  const [inspectorWidth, setInspectorWidth]       = useState(() => loadUiLayout().inspectorWidth);
  const [editorCueId, setEditorCueId]             = useState<string | null>(null);
  const [curveCueId, setCurveCueId]               = useState<string | null>(null);
  const [inspectorReload, setInspectorReload]     = useState(0);
  const [editorReload, setEditorReload]           = useState(0);
  const [showMode, setShowMode]                   = useState(false);
  const switchingShowMode = useRef(false);
  const [showModeError, setShowModeError] = useState("");
  const [cueColorResetError, setCueColorResetError] = useState<{ workspaceId: string; message: string } | null>(null);
  const resetPlayedCueColors = async () => {
    const workspaceId = workspaceInfo?.instance_id;
    if (!workspaceId) return;
    setCueColorResetError(null);
    try {
      await resetCueColors(workspaceId);
    } catch (cause) {
      setCueColorResetError({ workspaceId, message: String(cause) });
    }
  };
  const toggleShowMode = async () => {
    if (switchingShowMode.current) return;
    switchingShowMode.current = true;
    setShowModeError("");
    try {
      await setAgentPerformanceLock(!showMode);
      setShowMode(!showMode);
    } catch (error) {
      setShowModeError(String(error));
    } finally {
      switchingShowMode.current = false;
    }
  };
  // Non-null while an unsaved-changes confirmation is pending; the function is
  // the action to run once the user chooses Save or Don't Save (Cancel just
  // clears this). Shared by window-close, New, Open and Import QLab so none
  // of them can silently discard edits.
  const [pendingUnsavedAction, setPendingUnsavedAction] = useState<(() => void) | null>(null);
  const [gotoOpen, setGotoOpen]                   = useState(false);
  const [outputSurfaceVisible, setOutputSurfaceVisible] = useState(false);
  const [loadError, setLoadError]                 = useState<string | null>(null);
  const [saveError, setSaveError]                 = useState<string | null>(null);
  const [collectReport, setCollectReport]         = useState<CollectReport | null>(null);
  const [manualChapter, setManualChapter] = useState<string | null>(null);
  const [settingsTransfer, setSettingsTransfer] = useState<"export" | "import" | null>(null);
  const [pastePropertiesTargets, setPastePropertiesTargets] = useState<string[] | null>(null);
  const [audioSheetOpen, setAudioSheetOpen] = useState(false);
  const [guideOpen, setGuideOpen] = useState(false);
  const [importReport, setImportReport]           = useState<ImportReport | null>(null);
  const [recentFiles, setRecentFiles]             = useState<string[]>(loadRecentFiles);
  const [launcherOpen, setLauncherOpen]          = useState(false);
  const [showAbout, setShowAbout]                 = useState(false);
  const [preflightOpen, setPreflightOpen]         = useState(false);
  const [logsOpen, setLogsOpen]                   = useState(false);
  const [searchQuery, setSearchQuery]             = useState("");
  const [cueReveal, setCueReveal] = useState<{
    workspaceId: string; listId: string; ids: string[]; anchorSelection: boolean; focusRow?: boolean;
  }>();
  const [savedRevisionsOpen, setSavedRevisionsOpen] = useState(false);
  const [cueReviewHistoryOpen, setCueReviewHistoryOpen] = useState(false);
  const [recoveryInfo, setRecoveryInfo]           = useState<RecoveryInfo | null>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const cueDeskRef = useRef<HTMLDivElement>(null);

  // Persist panel visibility + inspector width across launches.
  useEffect(() => {
    saveUiLayout({ showCueListTabs, inspectorOpen, showSearchBar, showActiveCues, inspectorWidth });
  }, [showCueListTabs, inspectorOpen, showSearchBar, showActiveCues, inspectorWidth]);

  // Drag-resize the inspector from its left edge (pointer capture keeps the
  // drag alive even when the cursor leaves the 5 px handle).
  const handleInspectorResizeStart = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const startX = e.clientX;
    const startWidth = inspectorWidth;
    const onMove = (ev: PointerEvent) =>
      setInspectorWidth(clampInspectorWidth(startWidth + (startX - ev.clientX)));
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  };

  // Silent update check shortly after startup (production builds only —
  // dev builds have no signed update artifacts to compare against).
  useEffect(() => {
    if (!import.meta.env.PROD || import.meta.env.MODE === "rehearsal") return;
    const timer = window.setTimeout(() => {
      void useUpdateStore.getState().checkForUpdates({ silent: true });
    }, 5000);
    return () => window.clearTimeout(timer);
  }, []);

  // Apply data-theme whenever display prefs change
  useEffect(() => {
    const root = document.documentElement;
    const theme = displayPrefs.theme ?? "system";

    if (theme === "system") {
      const mq = window.matchMedia("(prefers-color-scheme: dark)");
      const apply = (dark: boolean) => {
        const effective = dark ? "dark" : "light";
        root.setAttribute("data-theme", effective);
        try { localStorage.setItem("wc_theme", effective); } catch { /* ignore */ }
      };
      apply(mq.matches);
      const handler = (e: MediaQueryListEvent) => apply(e.matches);
      mq.addEventListener("change", handler);
      return () => mq.removeEventListener("change", handler);
    } else {
      root.setAttribute("data-theme", theme);
      try { localStorage.setItem("wc_theme", theme); } catch { /* ignore */ }
    }
  }, [displayPrefs.theme]);

  const workspaceBootstrap = useRef<Promise<unknown> | null>(null);
  // Bootstrap
  useEffect(() => {
    if (!workspaceBootstrap.current) {
      const initialCueLists = getCueLists().then((lists) => {
        const store = useWorkspaceStore.getState();
        if (store.cueLists.length === 0 && lists.length > 0) {
          store.setCueLists(lists, lists[0].id);
        }
      }).catch(console.error);
      workspaceBootstrap.current = Promise.all([
        initialCueLists, refreshCues(), refreshWorkspaceInfo(), refreshHealth(),
        useWorkspaceStore.getState().refreshVideoOutputs(), loadGeneralPrefs(), loadDisplayPrefs(),
      ]);
      void refreshValidation();
    }
    void getOutputWindowVisible().then(setOutputSurfaceVisible);

    const unlistenVisible = listen<boolean>("output-window-visible", (e) => {
      setOutputSurfaceVisible(e.payload);
    });
    return () => { void unlistenVisible.then((u) => u()).catch(console.error); };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // -------------------------------------------------------------------------
  // Shared save helpers (used by FileMenu AND CloseConfirmDialog)
  // -------------------------------------------------------------------------

  const saveOwnedWorkspace = useCallback(async (path: string, owner: NonNullable<typeof workspaceInfo>): Promise<boolean> => {
    try {
      await saveWorkspace(path, { instance_id: owner.instance_id, revision: owner.revision, file_path: owner.file_path });
    } catch (err) {
      await refreshWorkspaceInfo();
      setSaveError(String(err));
      return false;
    }
    setRecentFiles(pushRecentFile(path));
    await refreshWorkspaceInfo();
    const active = useWorkspaceStore.getState().workspaceInfo;
    // A pending New/Open action may continue only for this still-clean show.
    return active?.instance_id === owner.instance_id && active.revision === owner.revision
      && active.file_path === path && !active.is_modified;
  }, [refreshWorkspaceInfo]);

  const handleSaveAs = useCallback(async (): Promise<boolean> => {
    const owner = workspaceInfo;
    if (!owner) return false;
    const path = await saveDialog({
      filters: [{ name: "Inkue Workspace", extensions: ["inkue"] }],
      defaultPath: owner.name + ".inkue",
    });
    if (typeof path !== "string") return false;
    return saveOwnedWorkspace(path.endsWith(".inkue") ? path : path + ".inkue", owner);
  }, [workspaceInfo, saveOwnedWorkspace]);

  const handleSave = useCallback(async (): Promise<boolean> => {
    const owner = workspaceInfo;
    if (!owner?.file_path) return handleSaveAs();
    return saveOwnedWorkspace(owner.file_path, owner);
  }, [workspaceInfo, handleSaveAs, saveOwnedWorkspace]);

  const handleSaveAsTemplate = useCallback(async () => {
    const owner = useWorkspaceStore.getState().workspaceInfo;
    if (!owner) return;
    try {
      const path = await saveDialog({
        filters: [templateFilter],
        defaultPath: owner.name + "." + TEMPLATE_EXTENSION,
      });
      if (typeof path !== "string") return;
      const suffix = "." + TEMPLATE_EXTENSION;
      const target = path.toLowerCase().endsWith(suffix) ? path : path + suffix;
      const { instance_id, revision, file_path } = owner;
      await saveWorkspaceTemplate(target, { instance_id, revision, file_path });
    } catch (error) {
      setSaveError(String(error));
    }
  }, []);

  const openWorkspacePath = useCallback(async (path: string) => {
    try {
      await loadWorkspace(path);
    } catch (error) {
      setLoadError(String(error));
      return;
    }
    setRecentFiles(pushRecentFile(path));
    setSearchQuery("");
  }, []);

  /** Run `action` now, or — if the current workspace has unsaved edits —
   *  prompt to save/discard/cancel first. `action` replaces or discards the
   *  current show, so it must never run silently past unsaved work. */
  const guardUnsaved = useCallback((action: () => void) => {
    if (useWorkspaceStore.getState().workspaceInfo?.is_modified) {
      setPendingUnsavedAction(() => action);
    } else {
      action();
    }
  }, []);

  const openDocumentRequest = useCallback(async () => {
    try {
      const path = await takeLaunchDocument();
      if (!path) return;
      if (showMode) {
        setLoadError("Leave Show Mode to open a workspace from the desktop.");
        return;
      }
      guardUnsaved(() => void openWorkspacePath(path));
    } catch (err) { setLoadError(String(err)); }
  }, [showMode, guardUnsaved, openWorkspacePath]);

  const documentHandler = useRef(openDocumentRequest);
  documentHandler.current = openDocumentRequest;
  const [workspaceEventsReady, setWorkspaceEventsReady] = useState(false);
  const waitingForRecovery = useRef(true);
  useEffect(() => {
    // A launch document emits its new workspace before the IPC call returns.
    // Wait until the desk can receive those events before taking the request.
    if (!workspaceEventsReady) return;
    let active = true;
    const subscribed = listen("document-open-requested", () => {
      if (active && !waitingForRecovery.current) void documentHandler.current();
    });
    void subscribed.then(async () => {
      // Let the initial desk reads settle before replacing their workspace.
      await workspaceBootstrap.current;
      if (!active) return;
      const info = await checkRecovery();
      if (!active) return;
      if (info) { setRecoveryInfo(info); return; }
      waitingForRecovery.current = false;
      await documentHandler.current();
    }).catch(err => { if (active) setLoadError(String(err)); });
    return () => { active = false; void subscribed.then(unlisten => unlisten()).catch(console.error); };
  }, [workspaceEventsReady]);

  const handleRecover = useCallback(async () => {
    setRecoveryInfo(null);
    try {
      await restoreRecovery();
      // The operator chose the recovered workspace over the launch document.
      await takeLaunchDocument().catch(() => undefined);
      waitingForRecovery.current = false;
      await refreshCues();
      await refreshWorkspaceInfo();
    } catch (err) {
      waitingForRecovery.current = false;
      console.error("restore recovery failed", err);
      setLoadError(`Recovery failed: ${String(err)}`);
    }
  }, [refreshCues, refreshWorkspaceInfo]);

  const handleDiscardRecovery = useCallback(async () => {
    setRecoveryInfo(null);
    try {
      await discardRecovery();
      waitingForRecovery.current = false;
      await openDocumentRequest();
    } catch (err) { setLoadError(String(err)); }
  }, [openDocumentRequest]);

  const handleOpen = useCallback(async () => {
    const path = await openDialog({
      multiple: false,
      filters: [{ name: "Inkue Workspace", extensions: ["inkue", "wincue"] }],
    });
    if (typeof path === "string") {
      guardUnsaved(() => void openWorkspacePath(path));
    }
  }, [openWorkspacePath, guardUnsaved]);

  const startNewWorkspace = useCallback((instance: string, path: string | null, blank = false) => {
    guardUnsaved(() => {
      // Save and continue may have changed the file path. Re-read the approved
      // workspace and pass its exact ownership to the native replacement.
      const owner = useWorkspaceStore.getState().workspaceInfo;
      if (!owner || owner.instance_id !== instance) {
        setLoadError("The workspace changed before the new show could open. Try again.");
        return;
      }
      const { instance_id, revision, file_path } = owner;
      const expected = { instance_id, revision, file_path };
      const action = path ? newWorkspaceFromTemplate(path, expected) : newWorkspace(expected, blank);
      void action.then(() => setSearchQuery("")).catch(error => setLoadError(String(error)));
    });
  }, [guardUnsaved]);

  const handleNew = useCallback((blank = false) => {
    const instance = useWorkspaceStore.getState().workspaceInfo?.instance_id;
    if (instance) startNewWorkspace(instance, null, blank);
  }, [startNewWorkspace]);

  const handleNewFromTemplate = useCallback(async () => {
    const instance = useWorkspaceStore.getState().workspaceInfo?.instance_id;
    if (!instance) return;
    try {
      const path = await openDialog({ multiple: false, filters: [templateFilter] });
      if (typeof path !== "string") return;
      if (useWorkspaceStore.getState().workspaceInfo?.instance_id !== instance) {
        setLoadError("The workspace changed while choosing a template. Choose the template again.");
        return;
      }
      startNewWorkspace(instance, path);
    } catch (error) {
      setLoadError(String(error));
    }
  }, [startNewWorkspace]);

  const handleImportQlab = useCallback(async () => {
    const path = await openDialog({
      multiple: false,
      filters: [{ name: "QLab Workspace", extensions: ["qlab5", "qlab4", "cues", "qlab3"] }],
    });
    if (typeof path !== "string") return;
    guardUnsaved(() => {
      void (async () => {
        try {
          // The import replaces the current show and is deliberately left
          // unsaved: media resolves against the QLab bundle folder, so
          // nothing is written into the user's QLab project. Save As is the
          // operator's next step.
          setImportReport(await importQlabWorkspace(path));
          setSearchQuery("");
        } catch (err) {
          setLoadError(String(err));
        }
      })();
    });
  }, [guardUnsaved]);

  const handleCollectAndSave = useCallback(async () => {
    const dir = await openDialog({ directory: true });
    if (typeof dir !== "string") return;
    try {
      const report = await collectAndSave(dir);
      setCollectReport(report);
    } catch (err) {
      setLoadError(String(err));
    }
  }, []);

  // -------------------------------------------------------------------------
  // Close-request interception
  // -------------------------------------------------------------------------

  // On Linux, GNOME/Mutter may restore a previous maximised *or* fullscreen state
  // from the session — overriding tauri.conf.json ("maximized": false,
  // "fullscreen": false) — and it applies that state *after* the window is mapped.
  // Normalise the window on mount and once more on the next tick so we reliably win
  // that race and always start at the configured 1280×800 size.
  useEffect(() => {
    const normalize = () => {
      const win = getCurrentWindow();
      void win.setFullscreen(false).catch(() => {});
      void win.unmaximize().catch(() => {});
    };
    normalize();
    const id = setTimeout(normalize, 150);
    return () => clearTimeout(id);
  }, []);

  useEffect(() => {
    const win = getCurrentWindow();
    let unlisten: (() => void) | undefined;

    win.onCloseRequested((event) => {
      const isModified = useWorkspaceStore.getState().workspaceInfo?.is_modified;
      if (isModified) {
        event.preventDefault();
        // destroy() force-closes without re-triggering onCloseRequested.
        setPendingUnsavedAction(() => () => void getCurrentWindow().destroy());
      }
      // Not modified → close proceeds normally.
    }).then((u) => { unlisten = u; });

    return () => unlisten?.();
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const confirmSaveThenContinue = async () => {
    const action = pendingUnsavedAction;
    setPendingUnsavedAction(null);
    const saved = await handleSave();
    // If the user cancelled the Save As dialog, or the save failed
    // (handleSave already surfaced the error), abandon the pending action
    // instead of discarding their edits.
    if (saved) action?.();
  };

  const confirmDiscardThenContinue = () => {
    const action = pendingUnsavedAction;
    setPendingUnsavedAction(null);
    action?.();
  };

  const cancelPendingUnsavedAction = () => setPendingUnsavedAction(null);

  // -------------------------------------------------------------------------
  // Misc
  // -------------------------------------------------------------------------

  const handleLoadError = useCallback((_cueId: string, error: string) => {
    setLoadError(error);
  }, []);

  useTauriEvents({ onLoadError: handleLoadError, onWorkspaceEventsReady: () => setWorkspaceEventsReady(true) });

  const handleRefresh = useCallback(async () => {
    await refreshCues();
  }, [refreshCues]);

  useKeyboardShortcuts(
    handleRefresh,
    () => void openPreferencesWindow(),
    () => void handleSave(),
    () => void handleOpen(),
    () => setInspectorOpen((v) => !v),
    () => setGotoOpen(true),
    () => void handleToggleSurface(),
    () => void toggleShowMode(),
    () => handleOpenSearch(),
    showMode,
    Boolean(launcherOpen || settingsTransfer || pastePropertiesTargets || manualChapter || audioSheetOpen || guideOpen || importReport || preflightOpen || pendingUnsavedAction || recoveryInfo || gotoOpen || showAbout || collectReport || logsOpen || loadError || saveError),
    () => void handleSaveAs(),
    () => setManualChapter(manualChapterFor(selectedCue?.cue_type)),
    () => setPastePropertiesTargets(selectedCueIds.length ? selectedCueIds : selectedCueId ? [selectedCueId] : []),
  );

  const selectedCue = findCueRecursive(cues, selectedCueId) ?? null;

  const handleToggleSurface = async () => {
    await toggleOutputWindow().catch(console.error);
    // outputSurfaceVisible is driven by the "output-window-visible" event from Rust
  };

  const handleCloseSearch = () => {
    setSearchQuery("");
    setShowSearchBar(false);
    requestAnimationFrame(() => {
      restoreFindFocus(cueDeskRef.current, useWorkspaceStore.getState().selectedCueId);
    });
  };

  const handleOpenSearch = () => {
    setShowSearchBar(true);
    requestAnimationFrame(() => searchInputRef.current?.focus());
  };

  const selectDeskCues = (workspaceId: string, listId: string, ids: string[], focusRow = false) => {
    const desk = useWorkspaceStore.getState();
    if (!ids[0] || desk.workspaceInfo?.instance_id !== workspaceId || desk.activeCueListId !== listId
      || !findCueRecursive(desk.cues, ids[0])) return false;
    desk.setSelectedCueId(ids[0]);
    desk.setSelectedCueIds(ids);
    setCueReveal({ workspaceId, listId, ids: [...ids], anchorSelection: true, focusRow });
    setInspectorOpen(true);
    if (desk.cueLists.find(list => list.id === listId)?.mode === "cart") {
      requestAnimationFrame(() => {
        const current = useWorkspaceStore.getState();
        if (current.workspaceInfo?.instance_id !== workspaceId || current.activeCueListId !== listId) return;
        cueDeskRef.current?.querySelector(`[data-cue-id="${CSS.escape(ids[0])}"]`)?.scrollIntoView({ block: "nearest" });
        if (focusRow) restoreFindFocus(cueDeskRef.current, ids[0]);
      });
    }
    return true;
  };

  const revealParkedCue = (workspaceId: string, listId: string, id: string) => {
    const desk = useWorkspaceStore.getState();
    if (desk.workspaceInfo?.instance_id !== workspaceId || desk.activeCueListId !== listId) return;
    setCueReveal({ workspaceId, listId, ids: [id], anchorSelection: false });
    void handleRefresh();
  };

  const handleAddAudio = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("audio", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddStop = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("stop", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddVideo = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("video", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddImage = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("image", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddWait = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("wait", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddGroup = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("group", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddOsc = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("osc", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddFade = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("fade", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddDevamp = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("devamp", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddMidi = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("midi", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddMemo = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("memo", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddMidiFile = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("midi_file", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddLight = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("light", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddMic = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("mic", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddTimecode = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("timecode", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddText = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("text", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddCamera = async () => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue("camera", idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const handleAddCommand = async (cueType: CueType) => {
    const { selectedCueId, cues } = useWorkspaceStore.getState();
    const idx = cues.findIndex((c) => c.id === selectedCueId);
    await addCue(cueType, idx >= 0 ? idx + 1 : -1).catch(console.error);
    await refreshCues();
  };

  const dispatchCueDrag = (cueType: CueType, e: React.MouseEvent) => {
    if (e.button !== 0) return;
    document.dispatchEvent(
      new CustomEvent("inkue:cue-drag-start", {
        detail: { cueType, startX: e.clientX, startY: e.clientY },
      }),
    );
  };

  const titleBarName = workspaceInfo
    ? `${workspaceInfo.name}${workspaceInfo.is_modified ? " •" : ""}`
    : "Inkue";

  return (
    <div
      style={{
        display: "flex", flexDirection: "column", height: "100vh",
        background: "var(--wc-bg-app)", color: "var(--wc-text)",
        fontFamily: "'Segoe UI', system-ui, -apple-system, BlinkMacSystemFont, sans-serif",
        overflow: "hidden",
      }}
      onContextMenu={(e) => e.preventDefault()}
    >
      {historyNotice && <div role="status" className="desk-history-notice">
        {historyNotice} <button type="button" aria-label="Dismiss editing notice"
          onClick={() => useWorkspaceStore.setState({ historyNotice: null })}>×</button>
      </div>}
      {/* Save failure toast */}
      {saveError && (
        <div
          style={{
            position: "fixed", bottom: 20, left: "50%", transform: "translateX(-50%)",
            zIndex: 99999, background: "#7f1d1d", border: "1px solid #ef4444",
            borderRadius: 8, padding: "10px 16px", maxWidth: 520,
            display: "flex", alignItems: "flex-start", gap: 12,
            boxShadow: "0 8px 24px rgba(0,0,0,0.8)",
          }}
        >
          <span style={{ color: "#fca5a5", fontSize: 13, flex: 1 }}>
            <strong style={{ color: "#fecaca" }}>Save failed — the workspace was not written.</strong>
            <br />
            <span style={{ opacity: 0.85, fontFamily: "monospace", fontSize: 11 }}>{saveError}</span>
          </span>
          <button
            onClick={() => setSaveError(null)}
            style={{
              background: "transparent", border: "none", color: "#fca5a5",
              cursor: "pointer", fontSize: 16, padding: 0, lineHeight: 1, flexShrink: 0,
            }}
          >
            ✕
          </button>
        </div>
      )}

      {/* Audio file load error toast */}
      {loadError && (
        <div
          style={{
            position: "fixed", bottom: 20, left: "50%", transform: "translateX(-50%)",
            zIndex: 99999, background: "#7f1d1d", border: "1px solid #ef4444",
            borderRadius: 8, padding: "10px 16px", maxWidth: 520,
            display: "flex", alignItems: "flex-start", gap: 12,
            boxShadow: "0 8px 24px rgba(0,0,0,0.8)",
          }}
        >
          <span style={{ color: "#fca5a5", fontSize: 13, flex: 1 }}>
            <strong style={{ color: "#fecaca" }}>Failed to load audio file.</strong>
            <br />
            <span style={{ opacity: 0.85, fontFamily: "monospace", fontSize: 11 }}>{loadError}</span>
          </span>
          <button
            onClick={() => setLoadError(null)}
            style={{
              background: "transparent", border: "none", color: "#fca5a5",
              cursor: "pointer", fontSize: 16, padding: 0, lineHeight: 1, flexShrink: 0,
            }}
          >
            ✕
          </button>
        </div>
      )}

      {showModeError && <div className="desk-history-notice" role="alert" onClick={() => setShowModeError("")}>
        Show Mode could not change. {showModeError}
      </div>}
      {cueColorResetError && cueColorResetError.workspaceId === workspaceInfo?.instance_id && (
        <div className="desk-history-notice" role="alert">
          Cue colors could not be reset. {cueColorResetError.message}
          <button type="button" aria-label="Dismiss cue color notice" onClick={() => setCueColorResetError(null)}>×</button>
        </div>
      )}

      {/* Goto cue dialog */}
      {gotoOpen && (
        <GotoDialog onClose={() => setGotoOpen(false)} onParked={revealParkedCue} />
      )}

      {/* Unsaved-changes dialog — shared by window-close, New, Open, Import QLab */}
      {pendingUnsavedAction && (
        <CloseConfirmDialog
          onSave={() => void confirmSaveThenContinue()}
          onDiscard={confirmDiscardThenContinue}
          onCancel={cancelPendingUnsavedAction}
        />
      )}

      {/* Collect & Save result dialog */}
      {settingsTransfer && <SettingsTransferDialog mode={settingsTransfer} onClose={() => setSettingsTransfer(null)} />}
      {pastePropertiesTargets && <PasteCuePropertiesDialog targets={pastePropertiesTargets}
        onClose={() => setPastePropertiesTargets(null)} onRefresh={handleRefresh} />}
      {manualChapter && <Suspense fallback={null}><OperatorManual initialChapter={manualChapter} onClose={() => setManualChapter(null)} /></Suspense>}
      {audioSheetOpen && (
        <AudioSheetDialog onClose={() => setAudioSheetOpen(false)}
          onAccept={(sheet, folder) => {
            setAudioSheetOpen(false);
            guardUnsaved(() => {
              void acceptAudioSheet(sheet, folder).then(() => setSearchQuery("")).catch((e) => setLoadError(String(e)));
            });
          }}
        />
      )}
      {guideOpen && (
        <OperatorGuide onClose={() => setGuideOpen(false)}
          onImport={() => { setGuideOpen(false); void handleImportQlab(); }}
          onRehearse={() => {
            setGuideOpen(false);
            guardUnsaved(() => {
              void openOperatorRehearsal().then(() => setSearchQuery("")).catch((e) => setLoadError(String(e)));
            });
          }}
        />
      )}
      {importReport && (
        <QlabImportDialog report={importReport} onClose={() => setImportReport(null)}
          onCheck={() => { setImportReport(null); setPreflightOpen(true); }} />
      )}
      {collectReport && (
        <CollectResultDialog
          report={collectReport}
          onClose={() => setCollectReport(null)}
        />
      )}

      {/* Crash-recovery dialog */}
      {savedRevisionsOpen && <SavedRevisionsDialog currentPath={workspaceInfo?.file_path} onClose={() => setSavedRevisionsOpen(false)} />}
      {launcherOpen && !showMode && <WorkspaceLauncher recentFiles={recentFiles}
        onClose={() => setLauncherOpen(false)}
        onOpen={path => { setLauncherOpen(false); guardUnsaved(() => void openWorkspacePath(path)); }}
        onBrowse={() => { setLauncherOpen(false); void handleOpen(); }}
        onNewBlank={() => { setLauncherOpen(false); handleNew(true); }}
        onNewTemplate={() => { setLauncherOpen(false); void handleNewFromTemplate(); }}
        onNewFromDefault={path => {
          setLauncherOpen(false);
          const instance = useWorkspaceStore.getState().workspaceInfo?.instance_id;
          if (instance) startNewWorkspace(instance, path);
        }} />}
      {cueReviewHistoryOpen && !showMode && workspaceInfo && <CueReviewHistoryPanel
        key={workspaceInfo.instance_id} workspaceId={workspaceInfo.instance_id}
        onClose={() => setCueReviewHistoryOpen(false)} />}
      {recoveryInfo && (
        <RecoveryDialog
          info={recoveryInfo}
          onRecover={() => void handleRecover()}
          onDiscard={() => void handleDiscardRecovery()}
        />
      )}

      {/* About dialog */}
      {showAbout && <AboutDialog onClose={() => setShowAbout(false)} />}
      <UpdateDialog />
      {preflightOpen && <PreflightModal onClose={() => setPreflightOpen(false)} />}
      {logsOpen && <LogViewerModal onClose={() => setLogsOpen(false)} />}

      {/* Custom title bar — two rows: Row 1 holds the window controls, menus and a
          full-width drag area; Row 2 holds the cue toolbar.  Splitting them means
          the toolbar can never squeeze the drag area down to an ungrabbable sliver
          when the window is narrow (the previous single-row layout collapsed it to
          ~40px, so only the "Inkue" label was draggable).  No drag-region on the
          row containers so menus/buttons keep working on Linux/WebKitGTK. */}
      <div
        style={{
          display: "flex", flexDirection: "column",
          background: "var(--wc-bg-surface)", borderBottom: "1px solid var(--wc-border)",
          flexShrink: 0, userSelect: "none", WebkitUserSelect: "none",
        }}
      >
        {/* Row 1 — window controls, File/View menus, draggable workspace title */}
        <div style={{ display: "flex", alignItems: "center", height: 36, padding: "0 12px", gap: 12 }}>
        <WindowControls />
        {!showMode && <FileMenu
          onSave={() => void handleSave()}
          onSaveAs={() => void handleSaveAs()}
          onSaveAsTemplate={() => void handleSaveAsTemplate()}
          onOpen={() => void handleOpen()}
          onNew={() => void handleNew()}
          onNewFromTemplate={() => void handleNewFromTemplate()}
          onSavedRevisions={() => setSavedRevisionsOpen(true)}
          onCueReviewHistory={() => setCueReviewHistoryOpen(true)}
          onSettingsTransfer={setSettingsTransfer}
          onAudioSheet={() => setAudioSheetOpen(true)}
          onManual={() => setManualChapter(manualHome)}
          onOperatorGuide={() => setGuideOpen(true)}
          onImportQlab={() => void handleImportQlab()}
          onCollect={() => void handleCollectAndSave()}
          onCheck={() => setPreflightOpen(true)}
          onLogs={() => setLogsOpen(true)}
          onPreferences={() => void openPreferencesWindow()}
          onAbout={() => setShowAbout(true)}
          recentFiles={recentFiles}
          onOpenRecent={(p) => guardUnsaved(() => void openWorkspacePath(p))}
        />}
        {!showMode && <EditMenu onRefresh={handleRefresh} onFind={handleOpenSearch} />}
        <ViewMenu
          items={[
            { label: "Show Mode",      checked: showMode,             onClick: () => void toggleShowMode(),       shortcut: "F5" },
            { label: "Cue List Tabs",  checked: showCueListTabs,      onClick: () => setShowCueListTabs((v) => !v) },
            { label: "Active Cues",    checked: showActiveCues,       onClick: () => setShowActiveCues((v) => !v) },
            { label: "Find Bar",       checked: showSearchBar,         onClick: showSearchBar ? handleCloseSearch : handleOpenSearch },
            { label: "Inspector",      checked: inspectorOpen,         onClick: () => setInspectorOpen((v) => !v) },
            { label: "Output Surface", checked: outputSurfaceVisible,  onClick: () => void handleToggleSurface() },
            { label: "Workspace Launcher…", disabled: showMode, onClick: () => setLauncherOpen(true) },
            { label: "Reset played cue colors", disabled: !workspaceInfo, onClick: () => void resetPlayedCueColors() },
          ]}
        />
        {!showMode && <ActionMenu onDone={handleRefresh} />}

        {/* Drag region: app name + workspace name */}
        <div
          data-tauri-drag-region
          style={{ flex: 1, minWidth: 40, display: "flex", alignItems: "center", gap: 8, overflow: "hidden" }}
        >
          <div data-tauri-drag-region style={{ flexShrink: 0, pointerEvents: "none", display: "flex" }}>
            <InkueMark size={18} />
          </div>
          <span
            data-tauri-drag-region
            style={{ fontWeight: 700, fontSize: 13, color: "var(--wc-text-bright)", flexShrink: 0, letterSpacing: "-0.01em" }}
          >
            Inkue
          </span>
          <span
            data-tauri-drag-region
            style={{ fontSize: 12, color: "var(--wc-text-muted)", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
          >
            {titleBarName}
          </span>
          {!showMode && brokenCueIds.size > 0 && (
            <button
              onClick={() => setPreflightOpen(true)}
              title="Some cues have problems — click to check"
              style={{
                flexShrink: 0, display: "flex", alignItems: "center", gap: 4,
                background: "rgba(239,68,68,0.15)", border: "1px solid rgba(239,68,68,0.5)",
                borderRadius: 5, color: "#ef4444", cursor: "pointer",
                fontSize: 11, padding: "2px 8px",
              }}
            >
              ⚠ {brokenCueIds.size}
            </button>
          )}
        </div>

        <button className="desk-mode-button" aria-pressed={showMode} aria-keyshortcuts="F5"
          title={showMode ? "Leave Show Mode (F5)" : "Enter Show Mode (F5)"}
          onClick={() => void toggleShowMode()}>Show Mode <kbd>F5</kbd></button>
        <AgentPanel performanceLocked={showMode} />
        </div>{/* end Row 1 */}

        {/* Row 2 — cue toolbar on its own row.  It never competes with the Row 1
            drag area, and wraps onto extra lines when the window is too narrow to
            fit every button, so they all stay reachable.  Hidden in Show Mode. */}
        <div
          style={{ display: showMode ? "none" : "flex", flexWrap: "wrap", gap: 6, padding: "0 12px 6px", alignItems: "center" }}
          onClick={(e) => { const btn = (e.target as HTMLElement).closest("button"); if (btn) pulseButton(btn); }}
        >
          <CueToolbarButton
            type="audio" label="Audio"
            title={`Add Audio Cue after selection (${modifierLabel}+N) · Drag to insert at position`}
            onAdd={handleAddAudio}
            onDragStart={(e) => dispatchCueDrag("audio", e)}
          />
          <CueToolbarButton
            type="video" label="Video"
            title="Add Video Cue after selection · Drag to insert at position"
            onAdd={handleAddVideo}
            onDragStart={(e) => dispatchCueDrag("video", e)}
          />
          <CueToolbarButton
            type="image" label="Image"
            title="Add Image Cue after selection · Drag to insert at position"
            onAdd={handleAddImage}
            onDragStart={(e) => dispatchCueDrag("image", e)}
          />
          <CueToolbarButton
            type="stop" label="Stop"
            title="Add Stop Cue after selection · Drag to insert at position"
            onAdd={handleAddStop}
            onDragStart={(e) => dispatchCueDrag("stop", e)}
          />
          <CueToolbarButton
            type="fade" label="Fade"
            title="Add Fade Cue after selection · Drag to insert at position"
            onAdd={handleAddFade}
            onDragStart={(e) => dispatchCueDrag("fade", e)}
          />
          <CueToolbarButton
            type="wait" label="Wait"
            title="Add Wait Cue after selection · Drag to insert at position"
            onAdd={handleAddWait}
            onDragStart={(e) => dispatchCueDrag("wait", e)}
          />
          <CueToolbarButton
            type="group" label="Group"
            title="Add Group Cue after selection · Drag to insert at position"
            onAdd={handleAddGroup}
            onDragStart={(e) => dispatchCueDrag("group", e)}
          />
          <CueToolbarButton
            type="midi" label="MIDI"
            title="Add MIDI Cue after selection · Drag to insert at position"
            onAdd={handleAddMidi}
            onDragStart={(e) => dispatchCueDrag("midi", e)}
          />
          <CueToolbarButton
            type="midi_file" label="MIDI File"
            title="Add MIDI File Cue (play a .mid to a MIDI port) · Drag to insert at position"
            onAdd={handleAddMidiFile}
            onDragStart={(e) => dispatchCueDrag("midi_file", e)}
          />
          <CueToolbarButton
            type="osc" label="OSC"
            title="Add OSC Cue after selection · Drag to insert at position"
            onAdd={handleAddOsc}
            onDragStart={(e) => dispatchCueDrag("osc", e)}
          />
          <CueToolbarButton
            type="light" label="Light"
            title="Add Light Cue after selection · Drag to insert at position"
            onAdd={handleAddLight}
            onDragStart={(e) => dispatchCueDrag("light", e)}
          />
          <CueToolbarButton
            type="mic" label="Mic"
            title="Add Mic Cue after selection · Drag to insert at position"
            onAdd={handleAddMic}
            onDragStart={(e) => dispatchCueDrag("mic", e)}
          />
          <CueToolbarButton
            type="timecode" label="TC"
            title="Add Timecode Cue after selection · Drag to insert at position"
            onAdd={handleAddTimecode}
            onDragStart={(e) => dispatchCueDrag("timecode", e)}
          />
          <CueToolbarButton
            type="text" label="Text"
            title="Add Text Cue after selection · Drag to insert at position"
            onAdd={handleAddText}
            onDragStart={(e) => dispatchCueDrag("text", e)}
          />
          <CueToolbarButton
            type="camera" label="Cam"
            title="Add Camera Cue (live webcam / capture / IP stream) · Drag to insert at position"
            onAdd={handleAddCamera}
            onDragStart={(e) => dispatchCueDrag("camera", e)}
          />
          <CueToolbarButton
            type="memo" label="Memo"
            title="Add Memo Cue (a note in the stack — does nothing on GO) · Drag to insert at position"
            onAdd={handleAddMemo}
            onDragStart={(e) => dispatchCueDrag("memo", e)}
          />
          <CueToolbarButton
            type="devamp" label="Devamp"
            title="Add Devamp Cue (release a vamping slice loop) · Drag to insert at position"
            onAdd={handleAddDevamp}
            onDragStart={(e) => dispatchCueDrag("devamp", e)}
          />
          <CommandToolbarButton
            onAdd={(t) => void handleAddCommand(t)}
            onDragStart={(t, e) => dispatchCueDrag(t, e)}
          />
          {/* `marginLeft: auto` pins the Inspector toggle to the right edge of
              its flex line, away from the cue-creation buttons it does not
              belong with — and it stays pinned when the row wraps. */}
          <button
            style={{ ...toolbarBtn, marginLeft: "auto" }}
            onClick={() => setInspectorOpen((v) => !v)}
            title={`Toggle Inspector (${modifierLabel}+I)`}
          >
            Inspector
          </button>
        </div>
      </div>

      {/* Runtime health banner (device/network faults) */}
      <HealthBanner />

      {/* Main area */}
      <div style={{ display: "flex", flex: 1, overflow: "hidden" }}>
        {showMode ? (
          <ShowModeView onGoto={() => setGotoOpen(true)} onRefresh={handleRefresh} />
        ) : (
          <>
            <div ref={cueDeskRef} style={{ flex: 1, minWidth: 0, minHeight: 0, overflow: "hidden", display: "flex", flexDirection: "column" }}>
              {showCueListTabs && <CueListTabs onRefresh={handleRefresh} />}
              {showActiveCues && <ActiveCuesView />}

              {cueLists.find(list => list.id === activeCueListId)?.mode === "cart" ? (
                <CartView onRefresh={handleRefresh} onInspect={() => setInspectorOpen(true)} />
              ) : (
                <CueListView
                  reveal={cueReveal && cueReveal.workspaceId === workspaceInfo?.instance_id
                    && cueReveal.listId === activeCueListId ? cueReveal : undefined}
                  onOperatorGuide={() => setGuideOpen(true)}
                  onCueDoubleClick={(cue: CueSummary) => {
                    useWorkspaceStore.getState().setSelectedCueId(cue.id);
                    setInspectorOpen(true);
                  }}
                  onRefresh={handleRefresh}
                />
              )}

              {/* Curve editor dock — fade shapes for the opened Fade Cue */}
              {curveCueId && (
                <CurveEditorDock
                  cueId={curveCueId}
                  onClose={() => setCurveCueId(null)}
                  onSaved={() => {
                    void handleRefresh();
                    setInspectorReload((n) => n + 1);
                  }}
                  reloadToken={editorReload}
                />
              )}

              {/* Clip editor dock — trim + slices for the opened cue */}
              {editorCueId && (
                <ClipEditorDock
                  cueId={editorCueId}
                  onClose={() => setEditorCueId(null)}
                  onSaved={() => {
                    void handleRefresh();
                    setInspectorReload((n) => n + 1);
                  }}
                  reloadToken={editorReload}
                />
              )}

              {showSearchBar && workspaceInfo && activeCueListId && (
                <CueFindBar
                  inputRef={searchInputRef} workspaceId={workspaceInfo.instance_id} listId={activeCueListId}
                  revision={workspaceInfo.revision} query={searchQuery} cues={cues} selected={selectedCueId}
                  onQueryChange={setSearchQuery} onClose={handleCloseSearch}
                  onSelect={ids => selectDeskCues(workspaceInfo.instance_id, activeCueListId, ids)}
                />
              )}
            </div>
            {inspectorOpen && (
              <div
                style={{
                  width: inspectorWidth, position: "relative",
                  borderLeft: "1px solid var(--wc-border)",
                  overflow: "hidden", display: "flex", flexDirection: "column", flexShrink: 0,
                }}
              >
                <div
                  onPointerDown={handleInspectorResizeStart}
                  title="Drag to resize"
                  style={{
                    position: "absolute", left: 0, top: 0, bottom: 0, width: 5,
                    cursor: "ew-resize", zIndex: 2,
                  }}
                />
                <InspectorPanel
                  selectedCue={selectedCue}
                  selectedCueIds={selectedCueIds}
                  onRefresh={handleRefresh}
                  onOpenEditor={(id) => setEditorCueId(id)}
                  onOpenCurveEditor={(id) => setCurveCueId(id)}
                  reloadToken={inspectorReload}
                  onCueSaved={() => setEditorReload((n) => n + 1)}
                />
              </div>
            )}
          </>
        )}
      </div>

      <TransportBar onRefresh={handleRefresh} showMode={showMode}
        onInspectCue={(owner, listId, id) => !showMode && selectDeskCues(owner, listId, [id], true)} />
    </div>
  );
}

const toolbarBtn: React.CSSProperties = {
  padding: "3px 10px", background: "var(--wc-bg-surface)", border: "1px solid var(--wc-border-strong)",
  borderRadius: 4, color: "var(--wc-text)", cursor: "pointer", fontSize: 12,
};

/** Toolbar "+ Command" button: one entry point for the eight cues that act on
 *  other cues. They are real, distinct cue types — this only keeps eight more
 *  buttons out of a toolbar that already wraps. */
function CommandToolbarButton({ onAdd, onDragStart }: {
  onAdd: (type: CueType) => void;
  onDragStart: (type: CueType, e: React.MouseEvent) => void;
}) {
  const [open, setOpen] = useState(false);
  const [hover, setHover] = useState(false);
  const [hoveredItem, setHoveredItem] = useState<string | null>(null);
  const color = CUE_TYPE_COLORS.goto;

  return (
    <div style={{ position: "relative" }}>
      {open && <div style={{ position: "fixed", inset: 0, zIndex: 9990 }} onClick={() => setOpen(false)} />}
      <button
        style={{
          ...toolbarBtn,
          display: "inline-flex", alignItems: "center", gap: 5, userSelect: "none",
          color: hover || open ? color : "var(--wc-text-secondary)",
          borderColor: hover || open ? color : "var(--wc-border-strong)",
          background: hover || open ? "var(--wc-bg-hover)" : "var(--wc-bg-surface)",
          transition: "color 0.12s, border-color 0.12s, background 0.12s",
        }}
        onClick={(e) => { e.stopPropagation(); setOpen((v) => !v); }}
        onMouseEnter={() => setHover(true)}
        onMouseLeave={() => setHover(false)}
        title="Cues that act on other cues — Start, Pause, Goto, Arm…"
      >
        <span style={{ color, fontWeight: 700, fontSize: 11 }}>+</span>
        Command
        <span style={{ fontSize: 9, opacity: 0.7 }}>▾</span>
      </button>
      {open && (
        <div
          style={{
            position: "absolute", left: 0, top: "100%", marginTop: 4, zIndex: 9999,
            background: "var(--wc-bg-surface)", border: "1px solid var(--wc-border-strong)",
            borderRadius: 6, boxShadow: "0 8px 24px rgba(0,0,0,0.6)", padding: 4, minWidth: 262,
          }}
        >
          {COMMAND_CUE_TYPES.map(({ type, label, hint }) => (
            <button
              key={type}
              onMouseEnter={() => setHoveredItem(type)}
              onMouseLeave={() => setHoveredItem(null)}
              onClick={(e) => { e.stopPropagation(); setOpen(false); onAdd(type); }}
              // Arms the drag-to-insert gesture, exactly like the "+ <type>"
              // buttons. It must NOT close the menu: unmounting the button on
              // mousedown means no mouseup lands on it, so `click` never fires
              // and the plain-click path silently does nothing.
              onMouseDown={(e) => { if (e.button === 0) onDragStart(type, e); }}
              style={{
                display: "flex", alignItems: "center", gap: 10, width: "100%",
                height: 26, padding: "0 10px", borderRadius: 4, border: "none", textAlign: "left",
                background: hoveredItem === type ? "var(--wc-bg-hover)" : "transparent",
                cursor: "pointer", whiteSpace: "nowrap",
              }}
            >
              <span style={{ color: CUE_TYPE_COLORS[type], fontSize: 12, fontWeight: 600, width: 52, flexShrink: 0 }}>
                {label}
              </span>
              <span style={{ color: "var(--wc-text-muted)", fontSize: 11 }}>{hint}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** Toolbar "+ <type>" button: neutral by default with only the "+" tinted in the
 *  cue-type color; the whole button illuminates in that color on hover. */
function CueToolbarButton({ type, label, title, onAdd, onDragStart }: {
  type: CueType;
  label: string;
  title: string;
  onAdd: () => void;
  onDragStart: (e: React.MouseEvent) => void;
}) {
  const color = CUE_TYPE_COLORS[type];
  const [hover, setHover] = useState(false);
  return (
    <button
      style={{
        ...toolbarBtn,
        display: "inline-flex", alignItems: "center", gap: 5, userSelect: "none",
        color: hover ? color : "var(--wc-text-secondary)",
        borderColor: hover ? color : "var(--wc-border-strong)",
        background: hover ? "var(--wc-bg-hover)" : "var(--wc-bg-surface)",
        transition: "color 0.12s, border-color 0.12s, background 0.12s",
      }}
      onClick={onAdd}
      onMouseDown={onDragStart}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      title={title}
    >
      <span style={{ color, fontWeight: 700, fontSize: 11 }}>+</span>
      {label}
    </button>
  );
}
