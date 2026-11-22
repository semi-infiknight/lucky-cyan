import { useEffect, useRef, useState } from "react";
import {
  auditionRunning, getMachineAudioConfig, getOutputPatches, listAudioDevices, previewCue, stopPreview,
} from "../../lib/commands";
import type { DeviceInfo } from "../../lib/types";

const outputKey = "inkue.audition.output";

export function AudioAudition({ cueId }: { cueId: string }) {
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [device, setDevice] = useState(() => localStorage.getItem(outputKey) ?? "");
  const [gain, setGain] = useState(-18);
  const [audioFx, setAudioFx] = useState(false);
  const [message, setMessage] = useState("");
  const [loading, setLoading] = useState(true);
  const [pending, setPending] = useState(false);
  const [playing, setPlaying] = useState<string>();
  const [revision, setRevision] = useState(0);
  const mounted = useRef(false);
  const active = useRef<string>();
  const busy = useRef(false);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (active.current) void stopPreview(active.current).catch(() => {});
    };
  }, []);

  useEffect(() => {
    let current = true;
    setLoading(true);
    setMessage("");
    Promise.all([listAudioDevices(), getMachineAudioConfig(), getOutputPatches()])
      .then(([outputs, main, patches]) => {
        if (!current) return;
        const assigned = new Set([main.device_id, ...patches.map(p => p.device_id)]);
        const choices = outputs.filter(output => output.id.startsWith("pw:") && !assigned.has(output.id));
        setDevices(choices);
        if (!main.device_id?.startsWith("pw:")) {
          setMessage("Select a named PipeWire show output in Audio Preferences.");
        } else if (!choices.length) {
          setMessage("Connect a dedicated PipeWire headphone output, then refresh.");
        }
      })
      .catch(error => { if (current) setMessage(String(error)); })
      .finally(() => { if (current) setLoading(false); });
    return () => { current = false; };
  }, [revision]);

  useEffect(() => {
    if (!playing) return;
    let current = true;
    let timer: ReturnType<typeof setTimeout>;
    const check = async () => {
      try {
        if (await auditionRunning(playing)) {
          if (current) timer = setTimeout(check, 350);
          return;
        }
      } catch (error) {
        if (current) setMessage(String(error));
      }
      if (current) { active.current = undefined; setPlaying(undefined); }
    };
    timer = setTimeout(check, 350);
    return () => { current = false; clearTimeout(timer); };
  }, [playing]);

  async function toggle() {
    if (busy.current) return;
    busy.current = true;
    setPending(true);
    setMessage("");
    try {
      if (active.current) {
        await stopPreview(active.current);
        active.current = undefined;
        if (mounted.current) setPlaying(undefined);
      } else {
        const request = crypto.randomUUID();
        active.current = request;
        const id = await previewCue(cueId, request, device, gain, undefined, undefined, audioFx);
        if (!mounted.current) { await stopPreview(id); return; }
        active.current = id;
        setPlaying(id);
      }
    } catch (error) {
      if (!playing) active.current = undefined;
      if (mounted.current) setMessage(String(error));
    } finally {
      busy.current = false;
      if (mounted.current) setPending(false);
    }
  }

  const selected = devices.some(d => d.id === device) ? device : "";
  return (
    <div role="group" aria-label="Clip audition" style={{ display: "flex", alignItems: "center", flexWrap: "wrap", gap: 8, padding: "6px 12px", fontSize: 12 }}>
      <label>Headphone output{" "}
        <select value={selected} disabled={loading || pending || !!playing}
          onChange={event => { setDevice(event.target.value); localStorage.setItem(outputKey, event.target.value); }}>
          <option value="">{loading ? "Loading outputs…" : "Choose output…"}</option>
          {devices.map(d => <option key={d.id} value={d.id}>{d.name}</option>)}
        </select>
      </label>
      <button type="button" disabled={loading || pending || !!playing} onClick={() => setRevision(n => n + 1)}>Refresh outputs</button>
      <label>Monitor level{" "}
        <select disabled={pending || !!playing} value={gain} onChange={e => setGain(Number(e.target.value))}>
          {[-48, -36, -24, -18, -12, -6, 0].map(n => <option key={n} value={n}>{n} dB</option>)}
        </select>
      </label>
      <label>Audition signal{" "}
        <select disabled={pending || !!playing} value={audioFx ? "effects" : "source"}
          onChange={event => setAudioFx(event.target.value === "effects")}>
          <option value="source">Source only</option>
          <option value="effects">With cue effects</option>
        </select>
      </label>
      <button type="button" disabled={pending || (!playing && !selected)} onClick={() => void toggle()}>
        {playing ? "Stop audition" : "Audition clip"}
      </button>
      <span role="status">{message}</span>
    </div>
  );
}
