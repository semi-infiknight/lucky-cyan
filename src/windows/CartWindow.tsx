import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { CartPlaybackGrid } from "../components/CueList/CartView";
import { cartWindowPad, readCartWindow } from "../lib/commands";
import type { CartWindowSnapshot, CueStateChangedEvent, CueTimeUpdateEvent } from "../lib/types";
import { useTimingStore } from "../stores/timingStore";
import { followDeskTheme } from "./deskTheme";

export function CartWindow() {
  const [snapshot, setSnapshot] = useState<CartWindowSnapshot | null>(null);
  const [loadError, setLoadError] = useState("");
  const [actionError, setActionError] = useState("");
  const refresh = useRef<() => void>(() => {});

  useEffect(followDeskTheme, []);

  useEffect(() => {
    let disposed = false;
    let unavailable = false;
    let reading = false;
    let dirty = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let visibleIds = new Set<string>();
    const unlisten: UnlistenFn[] = [];
    const native = getCurrentWindow();

    const read = async () => {
      if (disposed || unavailable || reading) return;
      reading = true;
      dirty = false;
      try {
        const next = await readCartWindow();
        if (disposed || dirty) return;
        if (!next) {
          unavailable = true;
          visibleIds.clear();
          setSnapshot(null);
          setLoadError("This cart has closed.");
          await native.close();
        } else {
          visibleIds = new Set(next.cues.map(cue => cue.id));
          setSnapshot(next);
          setLoadError("");
        }
      } catch (error) {
        if (!disposed && !dirty) { setSnapshot(null); setLoadError(String(error)); }
      } finally {
        reading = false;
        if (dirty) schedule();
      }
    };
    const schedule = () => {
      if (disposed || unavailable) return;
      dirty = true;
      if (reading || timer !== undefined) return;
      timer = setTimeout(() => { timer = undefined; void read(); }, 30);
    };
    refresh.current = schedule;
    // Register before the initial read so a list change during startup is observed.
    const attach = async (registration: Promise<UnlistenFn>) => {
      const remove = await registration;
      if (disposed) remove(); else unlisten.push(remove);
    };
    const setup = async () => {
      try {
        await Promise.all([
          ...["cue-lists-changed", "workspace-modified", "cue-list-refresh", "cue-load-error"]
            .map(event => attach(listen(event, schedule))),
          attach(listen<CueStateChangedEvent>("cue-state-changed", ({ payload }) => {
            if (visibleIds.has(payload.cue_id)) {
              if (payload.new_state === "standby" || payload.new_state === "completed") {
                useTimingStore.getState().clearTiming(payload.cue_id);
              }
              schedule();
            }
          })),
          attach(listen<CueTimeUpdateEvent>("cue-time-update", ({ payload }) => {
            if (visibleIds.has(payload.cue_id)) useTimingStore.getState().setTiming(payload.cue_id, payload);
          })),
        ]);
        schedule();
      } catch (error) {
        if (!disposed) setLoadError(String(error));
      }
    };
    void setup();
    window.addEventListener("focus", schedule);
    return () => {
      disposed = true;
      refresh.current = () => {};
      if (timer !== undefined) clearTimeout(timer);
      window.removeEventListener("focus", schedule);
      unlisten.forEach(remove => remove());
    };
  }, []);

  const title = snapshot?.name;
  useEffect(() => {
    if (title) void getCurrentWindow().setTitle(`Inkue · ${title}`).catch(() => {});
  }, [title]);

  async function act(id: string, action: "fire" | "stop") {
    setActionError("");
    try { await cartWindowPad(id, action); }
    catch (error) { setActionError(String(error)); }
    finally { refresh.current(); }
  }

  return <main className="cart-window" aria-label="Separate cart window">
    <header><h1>{snapshot?.name ?? "Cue cart"}</h1></header>
    {loadError && <p role="alert" className="cart-window-error">{loadError}</p>}
    {actionError && <p role="alert" className="cart-window-error">{actionError}</p>}
    {snapshot ? <CartPlaybackGrid cues={snapshot.cues} layout={snapshot.layout}
      onFire={id => void act(id, "fire")} onStop={id => void act(id, "stop")} />
      : !loadError && <p role="status">Loading cart…</p>}
  </main>;
}
