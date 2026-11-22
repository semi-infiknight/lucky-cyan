import { useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent, PointerEvent } from "react";
import type { CueSummary } from "../../lib/types";
import { CUE_TYPE_COLORS } from "../../lib/types";
import { setGroupChildEntrance } from "../../lib/commands";
import { useTimingStore } from "../../stores/timingStore";
import {
  dragEntranceMs, matchingTimelineOwner, MAX_PRE_WAIT_MS, nudgeTimelineEntrance, parseEntranceSeconds,
  snapTimelineEntrance, timelineGuideTimes, timelinePointerDelta, timelineSpanMs,
  timelineActionTime, timelineReferenceOrder, timelineZoomScrollLeft, TIMELINE_ZOOM_LEVELS,
} from "./groupTimeline";
import "./groupTimeline.css";

interface EntranceDrag {
  pointerId: number;
  trackX: number;
  startMs: number;
  widthPx: number;
  spanMs: number;
  guides: number[];
}

interface RowProps {
  child: CueSummary;
  locked: boolean;
  pinned: boolean;
  onPin: (childId: string, button: HTMLButtonElement) => void;
  spanMs: number;
  sharedGuideMs: number | null;
  guidesFor: (movingId: string) => number[];
  onGuide: (ms: number | null) => void;
  commit: (expected: number, ms: number) => Promise<void>;
  onError: (message: string) => void;
}

function TimelineRow({
  child, locked, pinned, onPin, spanMs, sharedGuideMs, guidesFor, onGuide, commit, onError,
}: RowProps) {
  const drag = useRef<EntranceDrag | null>(null);
  const entryStart = useRef<number | null>(null);
  const [preview, setPreview] = useState<number | null>(null);
  const [text, setText] = useState<string | null>(null);
  const timed = Boolean(child.supports_timeline_entrance);
  const editable = timed && !locked && child.state !== "running" && child.state !== "paused";
  const label = [child.number, child.name || "Untitled cue"].filter(Boolean).join(" ");
  const entranceMs = timed ? (preview ?? child.pre_wait_ms) : 0;
  const start = Math.min(100, entranceMs * 100 / spanMs);
  const duration = timed ? child.duration_ms : null;
  const length = Math.min(100 - start, (duration ?? 0) * 100 / spanMs);
  const detail = timed ? (duration === null ? "Duration unknown" : duration / 1000 + " s") : "No timed entrance";

  const save = async (expected: number, ms: number) => {
    if (editable && expected !== ms) {
      setPreview(ms);
      try {
        await commit(expected, ms);
      } finally {
        setPreview(null);
      }
    } else {
      setPreview(null);
    }
  };

  const startDrag = (event: PointerEvent<HTMLSpanElement>) => {
    if (!editable || event.button !== 0 || drag.current) return;
    const track = event.currentTarget.parentElement?.getBoundingClientRect();
    if (!track?.width) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = {
      pointerId: event.pointerId, trackX: event.clientX - track.left, startMs: child.pre_wait_ms,
      widthPx: track.width, spanMs, guides: guidesFor(child.id),
    };
  };

  const dragTo = (event: PointerEvent<HTMLSpanElement>) => {
    const gesture = drag.current;
    if (!gesture || gesture.pointerId !== event.pointerId) return;
    const track = event.currentTarget.parentElement?.getBoundingClientRect();
    if (!track || Math.abs(track.width - gesture.widthPx) > 0.5 || spanMs !== gesture.spanMs) {
      cancelDrag(event);
      return;
    }
    const delta = timelinePointerDelta(gesture.trackX, event.clientX, track.left);
    const ms = dragEntranceMs(gesture.startMs, delta, gesture.widthPx, gesture.spanMs);
    const guide = delta !== 0 && !event.ctrlKey && !event.metaKey
      ? snapTimelineEntrance(ms, gesture.guides, gesture.widthPx, gesture.spanMs) : null;
    return { ms: guide ?? ms, guide };
  };

  const cancelDrag = (event: PointerEvent<HTMLSpanElement>) => {
    if (drag.current?.pointerId !== event.pointerId) return;
    drag.current = null;
    setPreview(null);
    onGuide(null);
  };

  return (
    <li className="group-timeline-row" data-disarmed={child.is_disabled || undefined}
        data-pinned={pinned || undefined} style={{ "--group-lane-color": CUE_TYPE_COLORS[child.cue_type] } as CSSProperties}>
      <div className="group-timeline-row-heading">
        <div className="group-timeline-title">
          <strong title={label}>{label}</strong>
          <small>{detail}{child.is_disabled ? " · Disarmed" : ""}</small>
        </div>
        <button type="button" className="group-timeline-pin"
          aria-label={"Pin " + label + " as reference"} aria-pressed={pinned}
          title={pinned ? "Unpin reference lane" : "Keep this lane visible while scrolling"}
          onClick={event => onPin(child.id, event.currentTarget)}>
          Pin
        </button>
        {timed && (
          <input
            aria-label={"Pre-Wait for " + label + " in seconds"}
            aria-keyshortcuts="Alt+ArrowLeft Alt+ArrowRight Alt+Shift+ArrowLeft Alt+Shift+ArrowRight"
            title="Alt+Left or Right adjusts by 0.1 s. Hold Shift for 0.01 s."
            type="number" min={0} max={MAX_PRE_WAIT_MS / 1000} step={0.001}
            value={text ?? entranceMs / 1000}
            disabled={!editable}
            onFocus={() => { entryStart.current = child.pre_wait_ms; }}
            onChange={event => setText(event.currentTarget.value)}
            onBlur={event => {
              const expected = entryStart.current;
              entryStart.current = null;
              setText(null);
              if (!editable || expected === null || text === null) return;
              const requested = parseEntranceSeconds(event.currentTarget.value);
              if (requested === null) onError("Enter a Pre-Wait between 0 and 86,400 seconds.");
              else void save(expected, requested);
            }}
            onKeyDown={event => {
              if (event.altKey && !event.ctrlKey && !event.metaKey
                  && (event.key === "ArrowLeft" || event.key === "ArrowRight")) {
                event.preventDefault();
                event.stopPropagation();
                const ms = parseEntranceSeconds(event.currentTarget.value);
                if (ms !== null) {
                  setText(String(nudgeTimelineEntrance(ms, event.key === "ArrowRight", event.shiftKey) / 1000));
                }
              } else if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                entryStart.current = null;
                event.currentTarget.blur();
              } else if (event.key === "Enter") {
                event.preventDefault();
                event.currentTarget.blur();
              }
            }}
          />
        )}
      </div>
      <div className="group-timeline-track" aria-hidden="true">
        {length > 0 && <span className="group-timeline-duration" style={{ left: start + "%", width: length + "%" }} />}
        {sharedGuideMs !== null && <span className="group-timeline-guide" style={{ left: sharedGuideMs * 100 / spanMs + "%" }} />}
        <span className="group-timeline-entrance" data-timed={timed} data-editable={editable}
          style={{ left: start + "%" }}
          onPointerDown={startDrag}
          onPointerMove={event => {
            const position = dragTo(event);
            if (editable && position) {
              setPreview(position.ms);
              onGuide(position.guide);
            }
          }}
          onPointerUp={event => {
            const expected = drag.current?.startMs;
            const position = dragTo(event);
            if (!position || expected === undefined) return;
            drag.current = null;
            onGuide(null);
            void save(expected, position.ms);
          }}
          onPointerCancel={cancelDrag}
          onLostPointerCapture={cancelDrag}
        />
      </div>
    </li>
  );
}

interface TimelineProps {
  group: CueSummary;
  workspaceId: string | undefined;
  listId: string | null;
  onRefresh: () => void;
}

function TimelinePlayhead({ groupId, workspaceId, spanMs }: {
  groupId: string; workspaceId: string; spanMs: number;
}) {
  const time = useTimingStore(state => timelineActionTime(state.timings[groupId], workspaceId));
  if (time === null) return null;
  const elapsed = `Group position ${(time / 1000).toFixed(1)} s`;
  return <div className="group-timeline-playhead-rail">
    <div className="group-timeline-playhead" role="img" aria-label={elapsed}
      style={{ left: `${Math.min(100, time * 100 / spanMs)}%` }} />
  </div>;
}

export function GroupTimelineTab({ group, workspaceId, listId, onRefresh }: TimelineProps) {
  const saving = useRef(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const [guideMs, setGuideMs] = useState<number | null>(null);
  const [referenceId, setReferenceId] = useState<string | null>(null);
  const referenceButton = useRef<HTMLButtonElement | null>(null);
  const viewport = useRef<HTMLDivElement>(null);
  const zoomAnchor = useRef<{ left: number; width: number } | null>(null);
  const [zoomIndex, setZoomIndex] = useState(0);
  const zoom = TIMELINE_ZOOM_LEVELS[zoomIndex];
  const tickCount = zoom * 4;

  function changeZoom(next: number) {
    const index = Math.max(0, Math.min(TIMELINE_ZOOM_LEVELS.length - 1, next));
    const element = viewport.current;
    if (!element || index === zoomIndex) return;
    zoomAnchor.current = { left: element.scrollLeft, width: element.scrollWidth };
    setZoomIndex(index);
  }

  useLayoutEffect(() => {
    const element = viewport.current;
    const anchor = zoomAnchor.current;
    zoomAnchor.current = null;
    if (element && anchor) {
      element.scrollLeft = timelineZoomScrollLeft(anchor.left, anchor.width, element.clientWidth, element.scrollWidth);
    }
  }, [zoomIndex]);

  function toggleReference(childId: string, button: HTMLButtonElement) {
    referenceButton.current = button;
    setReferenceId(previous => previous === childId ? null : childId);
  }

  useLayoutEffect(() => {
    const button = referenceButton.current;
    referenceButton.current = null;
    if (button?.isConnected && document.activeElement !== button) {
      button.focus({ preventScroll: true });
    }
  }, [referenceId]);

  function handleZoomShortcut(event: KeyboardEvent<HTMLElement>) {
    if (!(event.ctrlKey || event.metaKey) || event.altKey || event.shiftKey) return;
    const next = event.key === "0" ? 0 : event.key === "-" ? zoomIndex - 1
      : event.key === "=" || event.key === "+" ? zoomIndex + 1 : null;
    if (next === null) return;
    event.preventDefault();
    event.stopPropagation();
    changeZoom(next);
  }

  function handlePanShortcut(event: KeyboardEvent<HTMLDivElement>) {
    if (event.target !== event.currentTarget || event.ctrlKey || event.metaKey || event.altKey) return;
    const element = event.currentTarget;
    switch (event.key) {
      case "ArrowLeft": element.scrollLeft -= element.clientWidth / 8; break;
      case "ArrowRight": element.scrollLeft += element.clientWidth / 8; break;
      case "ArrowUp": element.scrollTop -= element.clientHeight / 8; break;
      case "ArrowDown": element.scrollTop += element.clientHeight / 8; break;
      case "PageUp": element.scrollTop -= element.clientHeight * .8; break;
      case "PageDown": element.scrollTop += element.clientHeight * .8; break;
      case "Home": element.scrollLeft = 0; break;
      case "End": element.scrollLeft = element.scrollWidth; break;
      default: return;
    }
    event.preventDefault();
    event.stopPropagation();
  }

  const owner = matchingTimelineOwner(group.timeline_owner, workspaceId, listId);
  const children = group.children ?? [];
  const spanMs = timelineSpanMs(children.map(child => child.supports_timeline_entrance ? child
    : { pre_wait_ms: 0, duration_ms: 0 }));
  const active = group.state === "running" || group.state === "paused";
  const commit = async (child: CueSummary, expected: number, ms: number) => {
    if (!owner || active || saving.current) return;
    saving.current = true;
    setPending(true);
    setError("");
    try {
      await setGroupChildEntrance({
        ...owner, group_id: group.id, child_id: child.id,
        expected_pre_wait_ms: expected, pre_wait_ms: ms,
      });
    } catch (cause) {
      setError(String(cause));
    } finally {
      await onRefresh();
      saving.current = false;
      setPending(false);
    }
  };

  if (!owner) return <section className="group-timeline" aria-label="Group child entrances">
    <p className="group-timeline-status" role="status">Loading this group’s entrances…</p>
  </section>;

  return (
    <section className="group-timeline" aria-label="Group child entrances" aria-busy={pending}
      onKeyDown={handleZoomShortcut}>
      <header className="group-timeline-heading">
        <div>
          <h3>Child entrances</h3>
          <p>{guideMs === null ? "Relative to the group’s start" : `Aligned at ${guideMs / 1000} s`}</p>
        </div>
        <span>Pre-Wait (s)</span>
      </header>
      {active && <p className="group-timeline-status" role="status">Stop the group to edit entrances.</p>}
      {error && <p className="inspector-error" role="alert">{error}</p>}
      {children.length ? (
        <>
          <div className="group-timeline-zoom" role="group" aria-label="Timeline zoom">
            <button type="button" aria-label="Zoom out" aria-disabled={zoomIndex === 0}
              onClick={() => changeZoom(zoomIndex - 1)}>−</button>
            <output aria-label="Timeline magnification">{zoom}×</output>
            <button type="button" aria-label="Zoom in" aria-disabled={zoomIndex === TIMELINE_ZOOM_LEVELS.length - 1}
              onClick={() => changeZoom(zoomIndex + 1)}>+</button>
            <button type="button" aria-label="Fit timeline" aria-disabled={zoomIndex === 0}
              onClick={() => changeZoom(0)}>Fit</button>
          </div>
          <div className="group-timeline-viewport" ref={viewport} tabIndex={0}
            role="region" aria-label="Timeline ruler and cue tracks"
            style={{ "--group-timeline-zoom": zoom } as CSSProperties}
            onKeyDown={handlePanShortcut}>
            <div className="group-timeline-canvas">
              <div className="group-timeline-ruler" aria-hidden="true">
                {Array.from({ length: tickCount + 1 }, (_, tick) => (
                  <span key={tick} style={{ left: tick * 100 / tickCount + "%" }}>
                    {spanMs * tick / (tickCount * 1000)} s
                  </span>
                ))}
              </div>
              <ul className="group-timeline-rows">
                {timelineReferenceOrder(children, referenceId).map(child => <TimelineRow
                  key={child.id} child={child} spanMs={spanMs} pinned={child.id === referenceId}
                  onPin={toggleReference} locked={active || pending} sharedGuideMs={guideMs}
                  guidesFor={(movingId) => timelineGuideTimes(children, movingId)} onGuide={setGuideMs}
                  commit={(expected, ms) => commit(child, expected, ms)} onError={setError} />)}
              </ul>
              {active && <TimelinePlayhead groupId={group.id} workspaceId={owner.workspace_id} spanMs={spanMs} />}
            </div>
          </div>
        </>
      ) : <p className="group-timeline-status">Add a cue to the group to set its entrance.</p>}
    </section>
  );
}
