// Tab bar for switching between multiple cue lists.

import { useState, useRef, useEffect } from "react";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import {
  addCueList, openCartWindow, openSequentialWindow, removeCueList, renameCueList, setActiveCueList, setCueListMode, setCueListNumber,
  getCuelistTcConfig, setCuelistTcConfig,
} from "../../lib/commands";
import type { CueListSummary, CueListTcConfig, TcRate, TcOnStart, TcOnStop } from "../../lib/types";
import { Select } from "../common/Select";
import { DragNumber } from "../common/DragNumber";

interface TabEdit {
  listId: string;
  field: "name" | "number";
  previous: string | null;
  workspaceId?: string;
}

export function CueListTabs({ onRefresh }: { onRefresh: () => void }) {
  const { cueLists, activeCueListId, refreshCueLists, workspaceInfo } = useWorkspaceStore();
  const workspaceId = workspaceInfo?.instance_id;

  const [editing, setEditing] = useState<TabEdit | null>(null);
  const [editValue, setEditValue] = useState("");
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; id: string } | null>(null);
  const [addBtnHovered, setAddBtnHovered] = useState(false);
  const editInput = useRef<HTMLInputElement>(null);
  const editClosed = useRef(false);
  const [windowError, setWindowError] = useState("");
  const menuList = cueLists.find(list => list.id === contextMenu?.id);

  useEffect(() => {
    if (editing) {
      editInput.current?.focus();
      editInput.current?.select();
    }
  }, [editing]);

  // Close context menu on outside click.
  useEffect(() => {
    if (!contextMenu) return;
    const close = () => setContextMenu(null);
    window.addEventListener("click", close);
    return () => window.removeEventListener("click", close);
  }, [contextMenu]);

  const handleSwitchList = async (id: string) => {
    if (id === activeCueListId) return;
    await setActiveCueList(id).catch(console.error);
    onRefresh();
  };

  const handleAddList = async () => {
    const name = `Cue List ${cueLists.length + 1}`;
    await addCueList(name).catch(console.error);
    await refreshCueLists();
    onRefresh();
  };

  const handleRemoveList = async (id: string) => {
    if (cueLists.length <= 1) return;
    await removeCueList(id).catch(console.error);
    await refreshCueLists();
    onRefresh();
  };

  const handleToggleMode = async (id: string) => {
    const list = cueLists.find((l) => l.id === id);
    if (!list) return;
    const next = list.mode === "cart" ? "sequential" : "cart";
    await setCueListMode(id, next).catch(console.error);
    await refreshCueLists();
    setContextMenu(null);
  };

  const toggleCartChaining = async () => {
    if (menuList?.mode !== "cart") return;
    await setCueListMode(menuList.id, "cart", !menuList.chain_cart_cues).catch(console.error);
    await refreshCueLists();
    setContextMenu(null);
  };

  const startEdit = (list: CueListSummary, field: TabEdit["field"]) => {
    editClosed.current = false;
    const previous = list[field] ?? null;
    setEditing({ listId: list.id, workspaceId, field, previous });
    setEditValue(previous ?? "");
    setWindowError("");
    setContextMenu(null);
  };

  const commitEdit = async () => {
    if (!editing || editClosed.current) return;
    editClosed.current = true;
    const next = editValue.trim() || null;
    setEditing(null);
    if (editValue === (editing.previous ?? "") || next === editing.previous) return;
    try {
      if (editing.field === "name" && next) await renameCueList(editing.listId, next);
      if (editing.field === "number" && editing.workspaceId) {
        await setCueListNumber(editing.workspaceId, editing.listId, editing.previous, next);
      }
      await refreshCueLists();
    } catch (error) {
      setWindowError(String(error));
    }
  };

  const handleEditKey = (e: React.KeyboardEvent) => {
    e.stopPropagation();
    if (e.key === "Enter") { e.preventDefault(); void commitEdit(); }
    if (e.key === "Escape") { e.preventDefault(); editClosed.current = true; setEditing(null); }
  };

  return (
    <div
      style={{
        display: "flex", alignItems: "center", height: 30,
        background: "var(--wc-bg-app)",
        borderBottom: "1px solid var(--wc-border)",
        flexShrink: 0, gap: 1, paddingLeft: 4, paddingRight: 4,
        overflowX: "auto", overflowY: "hidden",
      }}
    >
      <div role="tablist" aria-label="Cue lists and carts" className="cue-list-tabstrip">
      {cueLists.map((list, index) => {
        const isActive = list.id === activeCueListId;
        const isEditing = list.id === editing?.listId;
        const tabLabel = list.number ? `${list.number} ${list.name}` : list.name;

        return (
          <div
            key={list.id}
            role="tab" aria-selected={isActive} aria-label={`${tabLabel}, ${list.mode === "cart" ? "cue cart" : "cue list"}`}
            tabIndex={isActive || (!activeCueListId && index === 0) ? 0 : -1}
            onKeyDown={e => {
              if (e.target !== e.currentTarget || e.ctrlKey || e.metaKey || e.altKey) return;
              const tabs = e.currentTarget.parentElement?.querySelectorAll<HTMLElement>('[role="tab"]');
              let dest = index;
              switch (e.key) {
                case "ArrowLeft": dest = Math.max(0, index - 1); break;
                case "ArrowRight": dest = Math.min(cueLists.length - 1, index + 1); break;
                case "Home": dest = 0; break;
                case "End": dest = cueLists.length - 1; break;
                case "Enter":
                case " ": break;
                case "F2":
                  e.preventDefault(); e.stopPropagation(); startEdit(list, "name"); return;
                default: return;
              }
              e.preventDefault();
              e.stopPropagation();
              if (e.repeat && (e.key === " " || e.key === "Enter")) return;
              tabs?.[dest]?.focus();
              void handleSwitchList(cueLists[dest].id);
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              e.stopPropagation();
              setContextMenu({ x: e.clientX, y: e.clientY, id: list.id });
            }}
            onClick={() => !isEditing && void handleSwitchList(list.id)}
            onDoubleClick={() => startEdit(list, "name")}
            style={{
              display: "flex", alignItems: "center",
              padding: "0 10px", height: 26, borderRadius: 4,
              cursor: "pointer", flexShrink: 0, userSelect: "none",
              background: isActive ? "var(--wc-bg-surface)" : "transparent",
              color: isActive ? "var(--wc-text)" : "var(--wc-text-muted)",
              border: isActive ? "1px solid var(--wc-border-strong)" : "1px solid transparent",
              fontSize: 12, fontWeight: isActive ? 600 : 400,
              minWidth: 80,
            }}
          >
            {isEditing ? (
              <input
                ref={editInput}
                aria-label={editing.field === "number" ? "Cue list number" : "Cue list name"}
                placeholder={editing.field === "number" ? "Cue list number" : undefined}
                value={editValue}
                onChange={(e) => setEditValue(e.target.value)}
                onBlur={() => void commitEdit()}
                onKeyDown={handleEditKey}
                onClick={(e) => e.stopPropagation()}
                style={{
                  background: "transparent", border: "none", outline: "none",
                  color: "inherit", fontSize: "inherit", fontWeight: "inherit",
                  width: "100%", padding: 0,
                }}
              />
            ) : (
              <span style={{ display: "flex", alignItems: "center", gap: 4, overflow: "hidden" }}>
                <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", maxWidth: 120 }}>
                  {tabLabel}
                </span>
                {list.mode === "cart" && (
                  <span style={{
                    fontSize: 9, fontWeight: 700, letterSpacing: "0.04em",
                    color: isActive ? "var(--wc-accent)" : "var(--wc-text-faint)",
                    border: `1px solid ${isActive ? "var(--wc-accent)" : "var(--wc-border)"}`,
                    borderRadius: 3, padding: "0 3px", lineHeight: "14px",
                    flexShrink: 0,
                  }}>
                    CART
                  </span>
                )}
              </span>
            )}
          </div>
        );
      })}

      </div>
      {/* Add cue list button */}
      <button
        onClick={() => void handleAddList()}
        type="button" aria-label="Add cue list" title="Add Cue List"
        onKeyDown={e => { if (e.key === " " || e.key === "Enter") e.stopPropagation(); }}
        style={{
          background: "transparent", border: "none",
          color: addBtnHovered ? "var(--wc-text-secondary)" : "var(--wc-text-faint)",
          cursor: "pointer", fontSize: 16, lineHeight: 1,
          padding: "0 6px", height: 26, borderRadius: 4, flexShrink: 0,
          display: "flex", alignItems: "center",
        }}
        onMouseEnter={() => setAddBtnHovered(true)}
        onMouseLeave={() => setAddBtnHovered(false)}
      >
        +
      </button>

      {/* Per-list timecode sync */}
      {workspaceId && activeCueListId && (
        <CueListTcSync key={`${workspaceId}:${activeCueListId}`} workspaceId={workspaceId} listId={activeCueListId} />
      )}

      {windowError && <div className="cart-window-error" role="alert">
        {windowError} <button type="button" onClick={() => setWindowError("")}>Dismiss</button>
      </div>}

      {/* Context menu */}
      {contextMenu && (
        <div
          style={{
            position: "fixed", left: contextMenu.x, top: contextMenu.y,
            background: "var(--wc-bg-surface)", border: "1px solid var(--wc-border-strong)", borderRadius: 6,
            padding: "4px 0", zIndex: 9999, minWidth: 160,
            boxShadow: "0 8px 24px rgba(0,0,0,0.7)",
          }}
          onClick={(e) => e.stopPropagation()}
        >
          <ContextMenuItem
            label="Rename…"
            onClick={() => {
              const list = cueLists.find((l) => l.id === contextMenu.id);
              if (list) startEdit(list, "name");
            }}
          />
          {menuList && workspaceId && <ContextMenuItem
            label="Cue list number…"
            onClick={() => startEdit(menuList, "number")}
          />}
          <ContextMenuItem
            label={menuList?.mode === "cart"
              ? "Switch to Sequential Mode"
              : "Switch to Cart Mode"}
            onClick={() => void handleToggleMode(contextMenu.id)}
          />
          {menuList?.mode === "cart" && <ContextMenuItem
            label={menuList.chain_cart_cues ? "Separate window requires independent pads" : "Open cart in separate window"}
            disabled={menuList.chain_cart_cues}
            onClick={() => {
              setContextMenu(null);
              setWindowError("");
              void openCartWindow(menuList.id).catch(error => setWindowError(String(error)));
            }}
          />}
          {menuList?.mode === "sequential" && <ContextMenuItem label="Open cue list in separate window"
            onClick={() => {
              setContextMenu(null);
              setWindowError("");
              void openSequentialWindow(menuList.id).catch(error => setWindowError(String(error)));
            }}
          />}
          {menuList?.mode === "cart" && <ContextMenuItem
            label={menuList.chain_cart_cues ? "Use independent pads" : "Use legacy chained pads"}
            onClick={() => void toggleCartChaining()}
          />}
          <div style={{ height: 1, background: "var(--wc-border-strong)", margin: "4px 0" }} />
          <ContextMenuItem
            label="Delete Cue List"
            danger
            disabled={cueLists.length <= 1}
            onClick={() => void handleRemoveList(contextMenu.id)}
          />
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Per-cue-list timecode sync control (lives at the right of the tab bar).
// Scope reads, edits and the open popover to the workspace and list on screen.
// ---------------------------------------------------------------------------

const TC_RATES: TcRate[] = ["24", "25", "29.97", "29.97df", "30"];
const TC_RATE_LABELS: Record<TcRate, string> = {
  "24": "24 fps", "25": "25 fps (PAL)", "29.97": "29.97 fps",
  "29.97df": "29.97df (NTSC DF)", "30": "30 fps",
};
const ON_STOP_LABELS: Record<TcOnStop, string> = {
  continue: "Keep cues running",
  pause: "Pause clock-started cues",
  stop: "Stop clock-started cues",
};

const tcFieldLabel: React.CSSProperties = { fontSize: 10, color: "var(--wc-text-muted)", marginBottom: 3 };
const tcInputStyle: React.CSSProperties = {
  background: "var(--wc-bg-app)", border: "1px solid var(--wc-border-strong)",
  borderRadius: 4, color: "var(--wc-text)", fontSize: 12, padding: "4px 6px",
  width: "100%", boxSizing: "border-box",
};

function CueListTcSync({ workspaceId, listId }: { workspaceId: string; listId: string }) {
  const [cfg, setCfg] = useState<CueListTcConfig | null>(null);
  const [open, setOpen] = useState(false);
  const [anchor, setAnchor] = useState<{ x: number; y: number } | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    let current = true;
    getCuelistTcConfig(workspaceId, listId)
      .then(config => { if (current) setCfg(config); })
      .catch(console.error);
    return () => { current = false; };
  }, [workspaceId, listId]);

  useEffect(() => {
    if (!open) return;
    const close = () => setOpen(false);
    window.addEventListener("click", close);
    return () => window.removeEventListener("click", close);
  }, [open]);

  if (!cfg) return null;

  const apply = (patch: Partial<CueListTcConfig>) => {
    const next = { ...cfg, ...patch };
    setCfg(next);
    setCuelistTcConfig(workspaceId, listId, next).catch(console.error);
  };

  return (
    <div style={{ marginLeft: "auto", position: "relative", flexShrink: 0 }}>
      <button
        ref={btnRef}
        onClick={(e) => {
          e.stopPropagation();
          const r = btnRef.current?.getBoundingClientRect();
          if (r) setAnchor({ x: r.right, y: r.bottom + 4 });
          setOpen((v) => !v);
        }}
        title="Timecode sync for this Cue List"
        style={{
          display: "flex", alignItems: "center", gap: 5, height: 22,
          padding: "0 8px", borderRadius: 4, cursor: "pointer",
          background: "transparent", fontSize: 11, fontWeight: 600, letterSpacing: "0.04em",
          color: cfg.enabled ? "var(--wc-accent)" : "var(--wc-text-faint)",
          border: `1px solid ${cfg.enabled ? "var(--wc-accent)" : "var(--wc-border)"}`,
        }}
      >
        <span style={{
          width: 6, height: 6, borderRadius: "50%",
          background: cfg.enabled ? "var(--wc-accent)" : "var(--wc-text-faint)",
        }} />
        TC SYNC
      </button>

      {open && anchor && (
        <div
          onClick={(e) => e.stopPropagation()}
          style={{
            position: "fixed", left: anchor.x, top: anchor.y, transform: "translateX(-100%)",
            background: "var(--wc-bg-surface)", border: "1px solid var(--wc-border-strong)",
            borderRadius: 6, padding: 12, zIndex: 9999, width: 240,
            boxShadow: "0 8px 24px rgba(0,0,0,0.7)",
            display: "flex", flexDirection: "column", gap: 10,
          }}
        >
          <label style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12 }}>
            <input
              type="checkbox"
              checked={cfg.enabled}
              onChange={(e) => apply({ enabled: e.target.checked })}
              style={{ accentColor: "var(--wc-accent)", width: 14, height: 14 }}
            />
            Sync cues from incoming timecode
          </label>

          <div>
            <div style={tcFieldLabel}>Expected Rate</div>
            <Select
              style={{ ...tcInputStyle, cursor: "pointer" }}
              value={cfg.rate}
              onChange={(e) => apply({ rate: e.target.value as TcRate })}
            >
              {TC_RATES.map((r) => <option key={r} value={r}>{TC_RATE_LABELS[r]}</option>)}
            </Select>
          </div>

          <div>
            <div style={tcFieldLabel}>Freewheel (ms)</div>
            <DragNumber
              min={0}
              max={2000}
              step={50}
              value={cfg.freewheel_ms}
              onChange={(e) => apply({ freewheel_ms: Math.max(0, Math.min(2000, Number(e.target.value) || 0)) })}
              style={{ ...tcInputStyle, fontFamily: "monospace" }}
            />
          </div>

          <div>
            <div style={tcFieldLabel}>On Start</div>
            <Select aria-label="Timecode On Start"
              style={{ ...tcInputStyle, cursor: "pointer" }}
              value={cfg.on_start ?? "future_marks"}
              onChange={e => apply({ on_start: e.target.value as TcOnStart })}>
              <option value="future_marks">Start at incoming marks</option>
              <option value="join_active">Join active cues</option>
            </Select>
            <p style={{ fontSize: 10, color: "var(--wc-text-faint)", lineHeight: 1.4, marginBottom: 0 }}>
              Lookback supports top-level Wait, File Audio and Video cues set to Do Not Continue.
            </p>
          </div>

          <div>
            <div style={tcFieldLabel}>On Stop</div>
            <Select
              style={{ ...tcInputStyle, cursor: "pointer" }}
              value={cfg.on_stop}
              onChange={(e) => apply({ on_stop: e.target.value as TcOnStop })}
            >
              {(["continue", "pause", "stop"] as TcOnStop[]).map((s) => (
                <option key={s} value={s}>{ON_STOP_LABELS[s]}</option>
              ))}
            </Select>
            <p style={{ fontSize: 10, color: "var(--wc-text-faint)", lineHeight: 1.4, marginBottom: 0 }}>
              Applies to top-level Wait, Audio and Video cues started directly by a timecode mark.
            </p>
          </div>

          <div style={{ fontSize: 10, color: "var(--wc-text-faint)", lineHeight: 1.4 }}>
            Enable TC receive in Preferences → Network, then set per-cue trigger
            times in the Inspector → Triggers tab.
          </div>
        </div>
      )}
    </div>
  );
}

function ContextMenuItem({
  label, onClick, danger, disabled,
}: {
  label: string;
  onClick: () => void;
  danger?: boolean;
  disabled?: boolean;
}) {
  const [hov, setHov] = useState(false);
  return (
    <button type="button" disabled={disabled}
      onMouseEnter={() => setHov(true)}
      onMouseLeave={() => setHov(false)}
      onClick={() => { if (!disabled) onClick(); }}
      style={{
        display: "block", width: "100%", padding: "6px 14px",
        background: hov && !disabled ? "var(--wc-bg-hover)" : "transparent",
        border: "none", textAlign: "left", fontSize: 13, cursor: disabled ? "default" : "pointer",
        color: disabled ? "var(--wc-text-faint)" : danger ? "#f87171" : "var(--wc-text)",
      }}
    >
      {label}
    </button>
  );
}
