import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCueMonitorSnapshot } from "../../lib/commands";
import type { CueMonitorSnapshot } from "../../lib/types";

const COALESCE_MS = 50;
const BUSY_RETRY_MS = [50, 100, 200];
const METADATA_EVENTS = [
  "cue-state-changed", "playhead-moved", "cue-lists-changed", "cue-list-refresh", "workspace-modified",
];

export function watchCueMonitor(
  workspaceId: string,
  onSnapshot: (snapshot: CueMonitorSnapshot | null) => void,
  onNotice: (text: string) => void,
) {
  let disposed = false;
  let ready = false;
  let reading = false;
  let dirty = false;
  let busyAttempt = 0;
  let listenerNotice = "";
  let fingerprint = "";
  let timer: ReturnType<typeof setTimeout> | undefined;
  const unlisten: UnlistenFn[] = [];

  function schedule(delay: number) {
    if (disposed || !ready || reading || timer !== undefined) return;
    timer = setTimeout(() => { timer = undefined; void read(); }, delay);
  }

  function refresh() {
    dirty = true;
    busyAttempt = 0;
    schedule(COALESCE_MS);
  }

  async function read() {
    reading = true;
    dirty = false;
    let busy = false;
    try {
      const next = await getCueMonitorSnapshot(workspaceId);
      if (disposed || dirty) return;
      if (next === null) {
        busy = true;
        return;
      }
      if (next.workspace_id !== workspaceId) throw new Error("Workspace changed.");
      onNotice(listenerNotice);
      // Native metadata is stable until the show changes. Clock samples are
      // already delivered independently to the timing store.
      const nextFingerprint = JSON.stringify(next);
      if (nextFingerprint !== fingerprint) {
        fingerprint = nextFingerprint;
        onSnapshot(next);
      }
    } catch (error) {
      if (disposed || dirty) return;
      fingerprint = "";
      onSnapshot(null);
      onNotice(String(error));
    } finally {
      reading = false;
      if (!disposed && dirty) {
        schedule(COALESCE_MS);
      } else if (!disposed && busy) {
        const delay = BUSY_RETRY_MS[busyAttempt++];
        if (delay === undefined) onNotice("Desk busy. Choose Refresh to update.");
        else schedule(delay);
      }
    }
  }

  async function attach(registration: Promise<UnlistenFn>) {
    const remove = await registration;
    if (disposed) remove();
    else unlisten.push(remove);
  }

  // Establish the listeners before reading so a change during opening is observed.
  void Promise.allSettled([
    ...METADATA_EVENTS.map(event => attach(listen(event, refresh))),
    attach(listen<{ workspace_id: string }>("cue-monitor-structure-changed", ({ payload }) => {
      if (payload.workspace_id === workspaceId) refresh();
    })),
  ]).then(results => {
    if (disposed) return;
    if (results.some(result => result.status === "rejected")) {
      listenerNotice = "Some cue notifications are unavailable. Choose Refresh to update.";
    }
    ready = true;
    refresh();
  });

  return {
    refresh,
    dispose() {
      disposed = true;
      clearTimeout(timer);
      unlisten.forEach(remove => remove());
    },
  };
}
