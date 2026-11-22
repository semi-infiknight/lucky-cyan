// Cart Mode view — cues as a responsive grid of trigger tiles.
// Click fires, drag reorders with live tile reflowing around the drop slot.

import { cueAccent } from "../../lib/cueColors";
import { useState, useRef, useEffect, useMemo } from "react";
import "./cueDesk.css";
import { CartLayoutControls } from "./CartLayoutControls";
import { cartGrid, cellAt, focusGridCell, gridColumns } from "./cartGrid";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { useTimingStore } from "../../stores/timingStore";
import { RunningLed } from "../common/RunningLed";
import { CueFlagMark, cueFlagLabel } from "../common/CueFlagMark";
import type { CartEdit, CartLayout, CueSummary, CueType } from "../../lib/types";
import {
  addCue, editCartLayout, goCue, moveCue, stopCue,
  setAudioFile, setVideoFile, setImageFile, setMidiFile, updateCue,
} from "../../lib/commands";
import { AUDIO_EXTS, VIDEO_EXTS, IMAGE_EXTS, MIDI_EXTS, extensionOf } from "../../lib/mediaTypes";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

function hexToRgba(hex: string, alpha: number): string {
  const m = /^#([0-9a-f]{6})$/i.exec(hex);
  if (!m) return hex;
  const n = parseInt(m[1], 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${alpha})`;
}

function formatDuration(ms: number): string {
  const s = Math.floor(ms / 1000);
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, "0")}`;
}

const isAudioPath = (p: string) => AUDIO_EXTS.has(extensionOf(p));
const isVideoPath = (p: string) => VIDEO_EXTS.has(extensionOf(p));
const isImagePath = (p: string) => IMAGE_EXTS.has(extensionOf(p));
const isMidiPath  = (p: string) => MIDI_EXTS.has(extensionOf(p));
const isMediaPath = (p: string) =>
  isAudioPath(p) || isVideoPath(p) || isImagePath(p) || isMidiPath(p);

type MediaCueType = "audio" | "video" | "image" | "midi_file";

function cueTypeForPath(p: string): MediaCueType {
  if (isVideoPath(p)) return "video";
  if (isImagePath(p)) return "image";
  if (isMidiPath(p)) return "midi_file";
  return "audio";
}
async function setFileForCue(cueType: MediaCueType, cueId: string, path: string) {
  if (cueType === "video") await setVideoFile(cueId, path);
  else if (cueType === "image") await setImageFile(cueId, path);
  else if (cueType === "midi_file") await setMidiFile(cueId, path);
  else await setAudioFile(cueId, path);
}
function basenameNoExt(p: string) {
  return (p.split(/[\\/]/).pop() ?? p).replace(/\.[^.]+$/, "");
}

// ---------------------------------------------------------------------------
// DragGhost — floating card following cursor with inertia-based tilt
// ---------------------------------------------------------------------------

function DragGhost({ cue, x, y, rotation }: { cue: CueSummary; x: number; y: number; rotation: number }) {
  const colorAccent = cueAccent(cue);
  const hasColor    = colorAccent !== "transparent";

  return (
    <div
      className="wc-drag-ghost"
      style={{
        position: "fixed",
        left: x, top: y,
        width: 164,
        transform: `translate(-50%, -50%) rotate(${rotation.toFixed(2)}deg) scale(1.08)`,
        pointerEvents: "none",
        zIndex: 99999,
        background: "var(--wc-bg-surface)",
        border: "1px solid var(--wc-border-strong)",
        borderRadius: 8,
        minHeight: 90,
        display: "flex",
        flexDirection: "column",
        boxShadow: "0 28px 56px rgba(0,0,0,0.65), 0 8px 20px rgba(0,0,0,0.45)",
        overflow: "hidden",
      }}
    >
      {hasColor && (
        <div style={{
          position: "absolute", left: 0, top: 0, bottom: 0, width: 4,
          background: colorAccent, borderRadius: "8px 0 0 8px",
        }} />
      )}
      <div style={{
        display: "flex", justifyContent: "space-between", alignItems: "center",
        padding: hasColor ? "8px 10px 0 14px" : "8px 10px 0 10px", flexShrink: 0,
      }}>
        <span style={{ fontSize: 10, color: "var(--wc-text-faint)", fontFamily: "monospace", lineHeight: 1 }}>
          {cue.number ?? "–"}
        </span>
        <span className="cue-cart-type">{cue.cue_type.replace(/_/g, " ")}</span>
      </div>
      <div style={{
        flex: 1, display: "flex", alignItems: "center",
        padding: hasColor ? "6px 10px 8px 14px" : "6px 10px 8px 10px",
      }}>
        <span style={{
          fontSize: 13, fontWeight: 600, color: "var(--wc-text)", lineHeight: 1.3,
          display: "-webkit-box", WebkitLineClamp: 2, WebkitBoxOrient: "vertical",
          overflow: "hidden", wordBreak: "break-word", width: "100%",
        }}>
          {cue.name || "(untitled)"}
        </span>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// DropSlot — placeholder cell shown at the insertion target position
// ---------------------------------------------------------------------------

function DropSlot() {
  return (
    <div style={{
      border: "2px dashed var(--wc-accent)",
      borderRadius: 8,
      minHeight: 90,
      background: hexToRgba("#3b82f6", 0.06),
      pointerEvents: "none",
    }} />
  );
}

// ---------------------------------------------------------------------------
// CartTile
// ---------------------------------------------------------------------------

interface TileProps {
  cue: CueSummary;
  onMouseDown?: (e: React.MouseEvent) => void;
  editMode: boolean;
  selected: boolean;
  onActivate: () => void;
  onStop: () => void;
  onFire?: () => void;
}

function CartTile({ cue, editMode, selected, onMouseDown, onActivate, onStop, onFire }: TileProps) {
  const timing = useTimingStore((s) => s.timings[cue.id]);
  const [hovered, setHovered] = useState(false);
  const [stopHovered, setStopHovered] = useState(false);

  const isRunning   = cue.state === "running";
  const isPaused    = cue.state === "paused";
  const isCompleted = cue.state === "completed";

  const colorAccent = cueAccent(cue);
  const hasColor    = colorAccent !== "transparent";

  const statusBorderColor = isRunning ? "#22c55e" : isPaused ? "#f59e0b" : "var(--wc-border)";
  const statusTint =
    isRunning ? hexToRgba("#22c55e", 0.07) :
    isPaused  ? hexToRgba("#f59e0b", 0.07) : "var(--wc-bg-surface)";

  const statusShadow =
    isRunning ? `0 0 0 1px ${hexToRgba("#22c55e", 0.25)}` :
    isPaused  ? `0 0 0 1px ${hexToRgba("#f59e0b", 0.25)}` : "none";

  const loopPeriodMs = cue.file_duration_ms ?? cue.duration_ms;
  const progressPct =
    isRunning && timing && loopPeriodMs && loopPeriodMs > 0
      ? Math.min(100, ((timing.action_elapsed_ms % loopPeriodMs) / loopPeriodMs) * 100)
      : null;

  const remainingMs = timing?.remaining_ms ?? null;

  const name = [cue.number, cue.name || "Untitled"].filter(Boolean).join(" ");
  const unavailable = cue.is_disabled || cue.is_loading;
  const stateLabel = cue.is_disabled ? "Disarmed" : cue.is_loading ? "Loading" : isPaused ? "Paused" : isRunning ? "Running" : "";
  const flagLabel = cueFlagLabel(cue);


  return (
    <div
      data-cue-id={cue.id}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => { setHovered(false); setStopHovered(false); }}
      className={selected && editMode ? "cue-cart-pad is-selected" : "cue-cart-pad"}
      style={{
        position: "relative",
        background: statusTint,
        border: `1px solid ${hovered && !isRunning && !isPaused ? "var(--wc-border-strong)" : statusBorderColor}`,
        borderRadius: 8,
        cursor: "pointer",
        userSelect: "none",
        overflow: "hidden",
        display: "flex",
        flexDirection: "column",
        minHeight: 90,
        opacity: !editMode && unavailable ? 0.58 : isCompleted ? 0.8 : 1,
        transition: "border-color 0.1s, opacity 0.2s",
        boxShadow: statusShadow,
      }}
    >
      <button type="button" className="cue-cart-trigger" data-cue-hotkeys={editMode ? undefined : true}
        disabled={!editMode && !!unavailable}
        aria-pressed={editMode ? selected : undefined}
        aria-label={`${editMode ? "Inspect" : "Fire"} cue ${name}${stateLabel ? `, ${stateLabel}` : ""}${flagLabel ? `, ${flagLabel}` : ""}`}
        title={`${editMode ? "Select to edit" : stateLabel === "Disarmed" ? "Disarmed" : "Fire"} · ${name}`}
        onMouseDown={onMouseDown} onClick={onActivate}
      />
      {/* Color stripe */}
      {hasColor && (
        <div style={{
          position: "absolute", left: 0, top: 0, bottom: 0, width: 4,
          background: colorAccent, borderRadius: "8px 0 0 8px", zIndex: 0,
        }} />
      )}

      {/* Header: number + type icon */}
      <div style={{
        display: "flex", justifyContent: "space-between", alignItems: "center",
        padding: hasColor ? "8px 10px 0 14px" : "8px 10px 0 10px", flexShrink: 0,
      }}>
        <span style={{ fontSize: 10, color: "var(--wc-text-faint)", fontFamily: "monospace", lineHeight: 1 }}>
          {cue.number ?? "–"}
        </span>
        <span style={{ display: "flex", alignItems: "center", gap: 5 }}>
          <CueFlagMark cue={cue} />
          <span className="cue-cart-type">{cue.cue_type.replace(/_/g, " ")}</span>
        </span>
      </div>

      {/* Name */}
      <div style={{
        flex: 1, display: "flex", alignItems: "center",
        padding: hasColor ? "4px 10px 6px 14px" : "4px 10px 6px 10px", minHeight: 0,
      }}>
        <span style={{
          fontSize: 13, fontWeight: 600,
          color: isCompleted ? "var(--wc-text-muted)" : "var(--wc-text)",
          lineHeight: 1.3,
          display: "-webkit-box", WebkitLineClamp: 2, WebkitBoxOrient: "vertical",
          overflow: "hidden", wordBreak: "break-word", width: "100%",
        }}>
          {cue.name || "(untitled)"}
        </span>
      </div>

      {/* Footer: status + stop */}
      <div style={{
        display: "flex", alignItems: "center", justifyContent: "space-between",
        padding: "0 10px 6px", paddingLeft: hasColor ? 14 : 10,
        flexShrink: 0, minHeight: 20,
      }}>
        {(isRunning || isPaused) ? (
          <div style={{ display: "flex", alignItems: "center", gap: 5 }}>
            {isRunning && <RunningLed size={7} />}
            {isPaused && (
              <span style={{
                display: "inline-block", width: 7, height: 7, borderRadius: "50%",
                background: "#f59e0b", flexShrink: 0,
              }} />
            )}
            <span style={{ fontSize: 10, color: isRunning ? "#22c55e" : "#f59e0b", fontFamily: "monospace" }}>
              {remainingMs != null && remainingMs >= 0
                ? formatDuration(remainingMs)
                : isRunning ? "running" : "paused"}
            </span>
          </div>
        ) : (
          <span style={{ fontSize: 10, color: "var(--wc-text-faint)" }}>
            {stateLabel || (cue.duration_ms != null ? formatDuration(cue.duration_ms) : "")}
          </span>
        )}
        {editMode && onFire && !unavailable && !isRunning && !isPaused && (
          <button type="button" className="cue-cart-fire" aria-label={`Fire cue ${name}`} onClick={onFire}>Fire</button>
        )}
        {(isRunning || isPaused) && (
          <button
            onMouseEnter={() => setStopHovered(true)}
            onMouseLeave={() => setStopHovered(false)}
            onClick={onStop}
            aria-label={`Stop cue ${name}`}
            title="Stop this cue"
            style={{
              position: "relative", zIndex: 2,
              background: stopHovered ? "#ef4444" : "rgba(239,68,68,0.2)",
              border: "1px solid #ef4444", borderRadius: 4,
              color: "var(--wc-text)", fontSize: 10, fontWeight: 700,
              padding: "1px 5px", cursor: "pointer", letterSpacing: "0.05em",
              transition: "background 0.1s",
            }}
          >
            STOP
          </button>
        )}
      </div>

      {/* Progress bar */}
      {progressPct != null && (
        <div style={{
          position: "absolute", bottom: 0, left: 0, height: 3,
          width: `${progressPct}%`, background: "#22c55e",
          borderRadius: "0 0 0 8px", transition: "width 0.15s linear",
        }} />
      )}

      {/* Hover overlay */}
      {hovered && !isRunning && !isPaused && (
        <div style={{
          position: "absolute", inset: 0,
          background: "rgba(255,255,255,0.04)", borderRadius: 8, pointerEvents: "none",
        }} />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// CartView
// ---------------------------------------------------------------------------

/** Performance surface shared by the programming desk and auxiliary windows.
 * All transport is provided by the owner, so this component cannot change lists. */
export function CartPlaybackGrid({ cues, layout, onFire, onStop }: {
  cues: CueSummary[];
  layout?: CartLayout | null;
  onFire: (id: string) => void;
  onStop: (id: string) => void;
}) {
  const cells = useMemo(() => layout ? cartGrid(cues, layout).cells : cues, [cues, layout]);
  return <div className="cart-window-grid" aria-label="Cart pads"
    style={{ gridTemplateColumns: layout ? gridColumns(layout.columns) : "repeat(auto-fill, minmax(150px, 1fr))" }}
    onKeyDown={event => {
      const columns = layout?.columns ?? Math.max(1, getComputedStyle(event.currentTarget).gridTemplateColumns.split(/\s+/).length);
      focusGridCell(event, columns);
      if (event.key === "Enter" || event.key === " ") {
        event.stopPropagation();
        if (event.repeat) event.preventDefault();
      }
    }}>
    {!cells.length && <p>No pads in this cart.</p>}
    {cells.map((cue, index) => {
      const cell = layout ? cellAt(index, layout.columns) : null;
      const location = cell ? `Row ${cell.row}, column ${cell.column}` : undefined;
      return <div className="cart-fixed-cell" role="group" data-cart-index={index} aria-label={location} key={index}>
        {cue ? <CartTile cue={cue} editMode={false} selected={false}
          onActivate={() => onFire(cue.id)} onStop={() => onStop(cue.id)} />
          : <span role="img" className="cart-empty-cell" aria-label={location + ", empty"} />}
      </div>;
    })}
  </div>;
}

export function CartView({ onRefresh, onInspect }: { onRefresh: () => void; onInspect?: () => void }) {
  const { cues, selectedCueId, setSelectedCueId, setSelectedCueIds, cueLists, activeCueListId } = useWorkspaceStore();
  const list = cueLists.find(list => list.id === activeCueListId);
  const layout = list?.cart_layout;
  const chained = list?.chain_cart_cues;
  const grid = useMemo(() => layout ? cartGrid(cues, layout) : null, [cues, layout]);
  const [placingId, setPlacingId] = useState<string | null>(null);
  const [layoutBusy, setLayoutBusy] = useState(false);
  const busyRef = useRef(false);
  const [editMode, setEditMode] = useState(false);
  const [error, setError] = useState("");
  const containerRef = useRef<HTMLDivElement>(null);
  const latestGrid = useRef({ layout, listId: activeCueListId, editMode });
  useEffect(() => { latestGrid.current = { layout, listId: activeCueListId, editMode }; }, [layout, activeCueListId, editMode]);

  function fixedDrop(x: number, y: number) {
    const { layout: fixed, listId } = latestGrid.current;
    if (!fixed || !listId) return undefined;
    const element = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-cart-index]");
    if (!element || !containerRef.current?.contains(element)) return undefined;
    return { list_id: listId, cell: cellAt(Number(element.dataset.cartIndex), fixed.columns) };
  }
  const onRefreshRef = useRef(onRefresh);
  useEffect(() => { onRefreshRef.current = onRefresh; }, [onRefresh]);
  const cuesRef = useRef(cues);
  useEffect(() => { cuesRef.current = cues; }, [cues]);

  useEffect(() => { setPlacingId(null); }, [activeCueListId, editMode, layout?.rows, layout?.columns]);

  const applyLayout = async (edit: CartEdit) => {
    if (!activeCueListId || busyRef.current) return;
    busyRef.current = true;
    setLayoutBusy(true);
    try {
      await editCartLayout(activeCueListId, edit);
      setPlacingId(null);
      await useWorkspaceStore.getState().refreshCueLists();
    } catch (err) {
      setError(String(err));
    } finally {
      busyRef.current = false;
      setLayoutBusy(false);
    }
  };

  const selectPad = (id: string) => {
    setSelectedCueId(id);
    setSelectedCueIds([id]);
    onInspect?.();
  };

  // Tile reorder drag
  const [draggingCueId, setDraggingCueId] = useState<string | null>(null);
  const [ghostState, setGhostState] = useState<{ x: number; y: number; rotation: number } | null>(null);
  const prevMouseXRef  = useRef<number | null>(null);
  const smoothedVelRef = useRef(0);
  const dragRef = useRef<{
    id: string;
    fromIndex: number;
    startX: number; startY: number;
    active: boolean;
    insertIndex: number | null;
  } | null>(null);

  // New-cue drag from toolbar
  const [newCueDragType, setNewCueDragType] = useState<CueType | null>(null);
  const newCueDragRef = useRef<{
    cueType: CueType;
    startX: number; startY: number;
    active: boolean;
    insertIndex: number | null;
  } | null>(null);

  // File drag (Tauri)
  const [isDraggingFile, setIsDraggingFile] = useState(false);

  // Shared insert index for all drag types
  const [insertIndex, setInsertIndex] = useState<number | null>(null);

  const justDroppedRef = useRef(false);

  // ---------------------------------------------------------------------------
  // Display array — remove dragged tile, insert DropSlot at target position.
  // This drives the live tile reflow during drag.
  // ---------------------------------------------------------------------------

  const displayItems = useMemo((): (CueSummary | null)[] => {
    const activeInsert = insertIndex;
    if (activeInsert === null) return cues;

    if (draggingCueId !== null) {
      // Tile reorder: dragged tile is removed from grid, DropSlot marks landing spot.
      // insertIndex is relative to the filtered array (cues without dragged tile),
      // which matches the `to` index expected by moveCue's remove-then-insert logic.
      const without = cues.filter(c => c.id !== draggingCueId);
      const result: (CueSummary | null)[] = [...without];
      result.splice(Math.min(activeInsert, without.length), 0, null);
      return result;
    }

    // Toolbar or file drag: inject DropSlot without removing a tile.
    const result: (CueSummary | null)[] = [...cues];
    result.splice(Math.min(activeInsert, cues.length), 0, null);
    return result;
  }, [cues, draggingCueId, insertIndex]);

  // ---------------------------------------------------------------------------
  // Grid insert index from cursor — queries only real tiles (DropSlot excluded)
  // ---------------------------------------------------------------------------

  function calcInsertIndex(clientX: number, clientY: number): number {
    const fixed = latestGrid.current.layout;
    if (fixed) {
      const hit = fixedDrop(clientX, clientY);
      return hit ? (hit.cell.row - 1) * fixed.columns + hit.cell.column - 1 : -1;
    }
    const tiles = containerRef.current
      ? (Array.from(containerRef.current.querySelectorAll("[data-cue-id]")) as HTMLElement[])
      : [];
    for (let i = 0; i < tiles.length; i++) {
      const rect = tiles[i].getBoundingClientRect();
      if (clientY < rect.top) return i;
      if (clientY <= rect.bottom && clientX < rect.left + rect.width / 2) return i;
    }
    return tiles.length;
  }

  // ---------------------------------------------------------------------------
  // Document-level drag handlers
  // ---------------------------------------------------------------------------

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (newCueDragRef.current) {
        const drag = newCueDragRef.current;
        if (!drag.active) {
          if (Math.hypot(e.clientX - drag.startX, e.clientY - drag.startY) < 5) return;
          drag.active = true;
          document.body.style.cursor = "copy";
          setNewCueDragType(drag.cueType);
        }
        const idx = calcInsertIndex(e.clientX, e.clientY);
        drag.insertIndex = idx;
        setInsertIndex(idx);
        return;
      }
      if (dragRef.current) {
        const drag = dragRef.current;
        if (!drag.active) {
          if (Math.hypot(e.clientX - drag.startX, e.clientY - drag.startY) < 5) return;
          drag.active = true;
          document.body.style.cursor = "none";
          prevMouseXRef.current = e.clientX;
          smoothedVelRef.current = 0;
          setDraggingCueId(drag.id);
        }
        const idx = calcInsertIndex(e.clientX, e.clientY);
        drag.insertIndex = idx;
        setInsertIndex(idx);
        // Inertia-based tilt
        const dx = e.clientX - (prevMouseXRef.current ?? e.clientX);
        prevMouseXRef.current = e.clientX;
        smoothedVelRef.current = smoothedVelRef.current * 0.78 + dx * 0.22;
        const rotation = Math.max(-13, Math.min(13, smoothedVelRef.current * 0.72));
        setGhostState({ x: e.clientX, y: e.clientY, rotation });
      }
    };

    const resetTileDrag = () => {
      dragRef.current = null;
      document.body.style.cursor = "";
      smoothedVelRef.current = 0;
      prevMouseXRef.current = null;
      setDraggingCueId(null);
      setInsertIndex(null);
      setGhostState(null);
    };

    const onUp = (e: MouseEvent) => {
      if (newCueDragRef.current) {
        const drag = newCueDragRef.current;
        if (drag.active && drag.insertIndex !== null) {
          const container = containerRef.current;
          if (container) {
            const rect = container.getBoundingClientRect();
            const inBounds =
              e.clientX >= rect.left && e.clientX <= rect.right &&
              e.clientY >= rect.top  && e.clientY <= rect.bottom;
            if (inBounds) {
              const destination = fixedDrop(e.clientX, e.clientY);
              if (!latestGrid.current.layout || (destination && latestGrid.current.editMode)) {
                addCue(drag.cueType, destination ? -1 : drag.insertIndex, destination)
                  .then(() => onRefreshRef.current())
                  .catch(e => setError(String(e)));
              }
            }
          }
        }
        newCueDragRef.current = null;
        document.body.style.cursor = "";
        setNewCueDragType(null);
        setInsertIndex(null);
        return;
      }
      if (dragRef.current) {
        const drag = dragRef.current;
        if (drag.active && drag.insertIndex !== null) {
          // insertIndex is already the after-removal index (relative to the filtered array),
          // which is exactly what moveCue expects (it removes first, then inserts at `to`).
          const to   = drag.insertIndex;
          const from = drag.fromIndex;
          if (latestGrid.current.layout && latestGrid.current.editMode) {
            const destination = fixedDrop(e.clientX, e.clientY);
            if (destination) {
              editCartLayout(destination.list_id, { action: "place", cue_id: drag.id, cell: destination.cell })
                .then(() => useWorkspaceStore.getState().refreshCueLists()).catch(e => setError(String(e)));
            }
          } else if (!latestGrid.current.layout && to !== from) {
            moveCue(drag.id, to)
              .then(() => onRefreshRef.current())
              .catch(console.error);
          }
          justDroppedRef.current = true;
          setTimeout(() => { justDroppedRef.current = false; }, 0);
        }
        resetTileDrag();
      }
    };

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (dragRef.current?.active) {
        justDroppedRef.current = true;
        resetTileDrag();
      }
      if (newCueDragRef.current?.active) {
        newCueDragRef.current = null;
        document.body.style.cursor = "";
        setNewCueDragType(null);
        setInsertIndex(null);
      }
    };

    const onNewCueDragStart = (e: Event) => {
      const { cueType, startX, startY } = (e as CustomEvent).detail as {
        cueType: CueType; startX: number; startY: number;
      };
      newCueDragRef.current = { cueType, startX, startY, active: false, insertIndex: null };
    };

    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup",   onUp);
    document.addEventListener("keydown",   onKeyDown);
    document.addEventListener("inkue:cue-drag-start", onNewCueDragStart);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup",   onUp);
      document.removeEventListener("keydown",   onKeyDown);
      document.removeEventListener("inkue:cue-drag-start", onNewCueDragStart);
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ---------------------------------------------------------------------------
  // Tauri file drag-drop
  // ---------------------------------------------------------------------------

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    (async () => {
      const fn_ = await getCurrentWindow().onDragDropEvent(async (event) => {
        const { type } = event.payload;
        if (type === "enter" || type === "over") {
          setIsDraggingFile(true);
          const pos = event.payload.position;
          if (pos) {
            setInsertIndex(calcInsertIndex(pos.x, pos.y));
          }
        } else if (type === "leave") {
          setIsDraggingFile(false);
          setInsertIndex(null);
        } else if (type === "drop") {
          setIsDraggingFile(false);
          setInsertIndex(null);
          const paths: string[] = (event.payload as { paths?: string[] }).paths ?? [];
          const pos = event.payload.position;
          const mediaPaths = paths.filter(isMediaPath);
          if (mediaPaths.length === 0) return;
          // Tauri drag positions use physical pixels. DOM hit testing uses CSS pixels.
          const x = pos ? pos.x / window.devicePixelRatio : 0;
          const y = pos ? pos.y / window.devicePixelRatio : 0;
          let destination = fixedDrop(x, y);
          if (latestGrid.current.layout && (!latestGrid.current.editMode || !destination)) return;
          let at = pos ? calcInsertIndex(x, y) : cuesRef.current.length;
          for (const p of mediaPaths) {
            const ct = cueTypeForPath(p);
            const newId = await addCue(ct, destination ? -1 : at, destination).catch(e => {
              setError(String(e)); return null;
            });
            if (!newId) break;
            destination = undefined;
            await setFileForCue(ct, newId, p).catch(console.error);
            await updateCue(newId, { name: basenameNoExt(p) }).catch(console.error);
            at++;
          }
          await onRefreshRef.current();
        }
      });
      if (cancelled) fn_(); else unlisten = fn_;
    })().catch(console.error);

    return () => { cancelled = true; unlisten?.(); };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // ---------------------------------------------------------------------------
  // Tile mousedown — start potential drag
  // ---------------------------------------------------------------------------

  function startTileDrag(e: React.MouseEvent, cueId: string, originalIndex: number) {
    if (e.button !== 0) return;
    justDroppedRef.current = false;
    dragRef.current = {
      id: cueId, fromIndex: originalIndex,
      startX: e.clientX, startY: e.clientY,
      active: false, insertIndex: null,
    };
  }

  // ---------------------------------------------------------------------------
  // Render
  // ---------------------------------------------------------------------------

  const isEmpty = cues.length === 0;
  const isDragging = draggingCueId !== null || newCueDragType !== null || isDraggingFile;

  const pad = (cue: CueSummary) => <CartTile key={cue.id} cue={cue} editMode={editMode}
    selected={cue.id === selectedCueId}
    onMouseDown={event => {
      if (!layout || editMode) startTileDrag(event, cue.id, cues.findIndex(c => c.id === cue.id));
    }}
    onActivate={() => {
      if (justDroppedRef.current) return;
      if (editMode) selectPad(cue.id);
      else if (!cue.is_disabled && !cue.is_loading) void goCue(cue.id).catch(e => setError(String(e)));
    }}
    onStop={() => void stopCue(cue.id).catch(e => setError(String(e)))}
    onFire={layout ? undefined : () => void goCue(cue.id).catch(e => setError(String(e)))}
  />;
  const selected = cues.find(cue => cue.id === selectedCueId);
  const moving = editMode && !!layout && !!placingId && cues.some(cue => cue.id === placingId);

  return (
    <section className="cue-cart" aria-label="Cue cart" tabIndex={-1} data-cue-cart-focus onKeyDown={e => {
      if (moving && e.key === "Escape") {
        e.preventDefault(); e.stopPropagation(); setPlacingId(null); return;
      }
      if (layout) focusGridCell(e, layout.columns);
      if (!layout && !e.altKey && !e.metaKey && !e.ctrlKey && !e.shiftKey && (e.target as HTMLElement).matches(".cue-cart-trigger")) {
        const grid = containerRef.current;
        if (grid && ["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) {
          const pads = Array.from(grid.querySelectorAll<HTMLButtonElement>(".cue-cart-trigger"));
          const index = pads.indexOf(e.target as HTMLButtonElement);
          const columns = getComputedStyle(grid).gridTemplateColumns.trim().split(/\s+/).length;
          const step = e.key === "ArrowUp" ? -columns : e.key === "ArrowDown" ? columns : e.key === "ArrowLeft" || e.key === "End" ? -1 : 1;
          let dest = e.key === "Home" ? 0 : e.key === "End" ? pads.length - 1 : index + step;
          while (pads[dest]?.disabled) dest += step;
          pads[dest]?.focus();
          e.preventDefault();
          e.stopPropagation();
          return;
        }
      }
      if (e.key !== " " && e.key !== "Enter") return;
      // Native buttons own these activation keys. A held key fires at most once.
      e.stopPropagation();
      if (e.repeat) e.preventDefault();
    }}>
      <div className="cue-cart-bar">
        <div className="cue-cart-switch" role="group" aria-label="Pad interaction">
          <button type="button" aria-pressed={!editMode} onClick={() => setEditMode(false)}>Trigger pads</button>
          <button type="button" aria-pressed={editMode} onClick={() => setEditMode(true)}>Edit pads</button>
        </div>
        <span>{chained ? "Legacy chained pads" : "Independent pads"}{layout && ` · ${layout.rows} × ${layout.columns}`} · {cues.length} {cues.length === 1 ? "cue" : "cues"}</span>
      </div>
      {editMode && <div className="cart-layout-tools">
        <CartLayoutControls key={activeCueListId} layout={layout} onEdit={edit => void applyLayout(edit)} busy={layoutBusy} />
        {layout && <button type="button" disabled={!selected || layoutBusy} aria-pressed={moving}
          onClick={() => setPlacingId(moving ? null : selected?.id ?? null)}>
          {moving ? "Cancel placement" : "Move selected pad"}
        </button>}
      </div>}
      {moving && <div className="cart-placement-status" role="status">Choose a destination for {cues.find(c => c.id === placingId)?.name || "the selected pad"}. An occupied cell swaps pads.</div>}
      {editMode && grid && grid.outside.length > 0 && <div className="cart-overflow">
        <span>Outside grid</span>
        {grid.outside.map(cue => <button key={cue.id} type="button" aria-pressed={cue.id === selectedCueId}
          onClick={() => { setPlacingId(null); selectPad(cue.id); }}>{cue.number} {cue.name || cue.cue_type}</button>)}
      </div>}
      {error && <div role="alert" className="cue-cart-error">{error}
        <button type="button" onClick={() => setError("")}>Dismiss</button>
      </div>}
    {layout && !editMode ? <CartPlaybackGrid cues={cues} layout={layout}
      onFire={id => void goCue(id).catch(e => setError(String(e)))}
      onStop={id => void stopCue(id).catch(e => setError(String(e)))} /> :
    <div
      ref={containerRef}
      style={{
        flex: 1, overflow: "auto", padding: 12,
        display: "grid",
        gridTemplateColumns: layout ? gridColumns(layout.columns) : "repeat(auto-fill, minmax(160px, 1fr))",
        gap: 8,
        alignContent: "start",
      }}
      onDragOver={(e) => e.preventDefault()}
      onDrop={(e) => e.preventDefault()}
    >
      {grid && layout && grid.cells.map((cue, index) => {
        const cell = cellAt(index, layout.columns);
        const location = `Row ${cell.row}, column ${cell.column}`;
        return <div key={index} role="group" data-cart-index={index} className="cart-fixed-cell" aria-label={location}
          data-drop-target={isDragging && insertIndex === index || undefined}>
          {moving ? <button type="button" disabled={layoutBusy} className="cart-placement-target"
            aria-label={`Place pad at ${location}${cue ? ", swap with " + (cue.name || cue.cue_type) : ""}`}
            onClick={() => void applyLayout({ action: "place", cue_id: placingId!, cell })}>
            <small>{location}</small>{cue ? cue.name || cue.cue_type : "Empty"}
          </button> : cue ? pad(cue) : <span role="img" className="cart-empty-cell" aria-label={location + ", empty"} />}
        </div>;
      })}
      {!grid && isEmpty && !isDragging && (
        <div style={{
          gridColumn: "1 / -1", padding: 40, textAlign: "center",
          color: "var(--wc-text-faint)", fontSize: 14,
        }}>
          Drag cues from the toolbar to add them here.
        </div>
      )}

      {!grid && isEmpty && isDragging && <DropSlot key="__drop_slot__" />}
      {!grid && !isEmpty && displayItems.map(item => item ? pad(item) : <DropSlot key="__drop_slot__" />)}

      {/* Floating ghost with inertia tilt */}
      {draggingCueId !== null && ghostState !== null && (() => {
        const cue = cues.find(c => c.id === draggingCueId);
        return cue
          ? <DragGhost cue={cue} x={ghostState.x} y={ghostState.y} rotation={ghostState.rotation} />
          : null;
      })()}
    </div>}
    </section>
  );
}
