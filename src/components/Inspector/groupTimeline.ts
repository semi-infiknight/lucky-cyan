import type { CueSummary, GroupTimelineOwner } from "../../lib/types";
import type { CueTiming } from "../../stores/timingStore";

export function matchingTimelineOwner(
  owner: GroupTimelineOwner | undefined,
  workspaceId: string | undefined,
  listId: string | null,
): GroupTimelineOwner | undefined {
  return owner?.workspace_id === workspaceId && owner?.list_id === listId ? owner : undefined;
}

export const MAX_PRE_WAIT_MS = 86_400_000;

export function timelineActionTime(sample: CueTiming | undefined, workspaceId: string): number | null {
  if (!sample || sample.workspace_id !== workspaceId || !Number.isFinite(sample.action_elapsed_ms)) return null;
  return Math.max(0, sample.action_elapsed_ms);
}

export function timelineSpanMs(children: { pre_wait_ms: number; duration_ms: number | null }[]): number {
  const last = children.reduce((end, cue) => Math.max(end, cue.pre_wait_ms + (cue.duration_ms ?? 0)), 0);
  const interval = Math.max(1000, (last * 1.05) / 4);
  const power = 10 ** Math.floor(Math.log10(interval));
  const step = [1, 2, 2.5, 5, 10].find(value => value * power >= interval) ?? 10;
  return 4 * step * power;
}

/** Bring one reference row to the top of the view without changing cue order. */
export function timelineReferenceOrder<T extends { id: string }>(
  children: readonly T[], referenceId: string | null,
): readonly T[] {
  const index = children.findIndex(child => child.id === referenceId);
  if (index <= 0) return children;
  return [children[index], ...children.slice(0, index), ...children.slice(index + 1)];
}

export const TIMELINE_ZOOM_LEVELS = [1, 2, 4, 8] as const;

export function timelineZoomScrollLeft(
  previousLeft: number, previousWidth: number, viewportWidth: number, nextWidth: number,
): number {
  if (previousWidth <= 0 || nextWidth <= viewportWidth) return 0;
  const center = (previousLeft + viewportWidth / 2) / previousWidth;
  return Math.max(0, Math.min(nextWidth - viewportWidth, center * nextWidth - viewportWidth / 2));
}

// Pointer capture supplies WebView coordinates. Convert each event to the same
// track's coordinates so panning during the drag also moves the marker.
export function timelinePointerDelta(startX: number, clientX: number, trackLeft: number): number {
  return clientX - trackLeft - startX;
}

export function dragEntranceMs(startMs: number, deltaPx: number, widthPx: number, spanMs: number): number {
  if (deltaPx === 0 || widthPx <= 0) return startMs;
  return Math.max(0, Math.min(MAX_PRE_WAIT_MS, Math.round(startMs + deltaPx * spanMs / widthPx)));
}

type TimelineGuideCue = Pick<CueSummary,
  "id" | "pre_wait_ms" | "duration_ms" | "is_disabled" | "supports_timeline_entrance">;

export function timelineGuideTimes(children: TimelineGuideCue[], movingId: string): number[] {
  const times = new Set([0]);
  for (const child of children) {
    if (child.id === movingId || !child.supports_timeline_entrance || child.is_disabled) continue;
    times.add(child.pre_wait_ms);
    if (child.duration_ms !== null) times.add(child.pre_wait_ms + child.duration_ms);
  }
  return [...times].filter(ms => ms <= MAX_PRE_WAIT_MS).sort((first, second) => first - second);
}

export function snapTimelineEntrance(ms: number, candidates: number[], widthPx: number, spanMs: number): number | null {
  if (widthPx <= 0 || spanMs <= 0) return null;
  const toleranceMs = 6 * spanMs / widthPx;
  let nearest: number | null = null;
  let nearestDistance = toleranceMs;
  for (const candidate of candidates) {
    const distance = Math.abs(candidate - ms);
    if (distance < nearestDistance) {
      nearest = candidate;
      nearestDistance = distance;
    }
  }
  return nearest;
}

export function nudgeTimelineEntrance(ms: number, forward: boolean, fine: boolean): number {
  const step = (fine ? 10 : 100) * (forward ? 1 : -1);
  return Math.max(0, Math.min(MAX_PRE_WAIT_MS, ms + step));
}

export function parseEntranceSeconds(text: string): number | null {
  if (!text.trim()) return null;
  const seconds = Number(text);
  if (!Number.isFinite(seconds) || seconds < 0 || seconds > MAX_PRE_WAIT_MS / 1000) return null;
  return Math.round(seconds * 1000);
}
