import { useEffect, useRef, useState } from "react";
import { getPreferences, setMasterVolume } from "../../lib/commands";
import { useTransportStore } from "../../stores/transportStore";

const MIN_DB = -60;
const PEAK_HOLD_MS = 1500;
const ticks = [-48, -36, -24, -12, -6, 0];
const fill = (db: number) => Math.max(0, Math.min(1, 1 - db / MIN_DB));

export function MasterOutput() {
  const [volume, setVolume] = useState(0);
  const meter = useRef<HTMLDivElement>(null);
  const volumeEdited = useRef(false);

  useEffect(() => {
    let mounted = true;
    void getPreferences().then(prefs => {
      if (!mounted || volumeEdited.current) return;
      const db = prefs.audio.default_volume_db;
      setVolume(db);
      return setMasterVolume(db);
    }).catch(console.error);
    return () => { mounted = false; };
  }, []);

  // The audio meters repaint independently of React. A decayed meter sleeps
  // until a new peak arrives. Decay speed is independent of monitor refresh rate.
  useEffect(() => {
    const channels = ["l", "r"] as const;
    const values = channels.map(() => ({ level: 0, hold: 0, until: 0 }));
    let previous = performance.now();
    let frame = 0;

    function draw(now: number) {
      frame = 0;
      const seconds = (now - previous) / 1000;
      previous = now;
      values.forEach((value, index) => {
        value.level = Math.max(0, value.level - seconds * 20 / -MIN_DB);
        if (now >= value.until) value.hold = Math.max(0, value.hold - seconds * 8 / -MIN_DB);
        meter.current?.style.setProperty(`--level-${channels[index]}`, `${value.level * 100}%`);
        meter.current?.style.setProperty(`--hold-${channels[index]}`, `${value.hold * 100}%`);
      });
      if (values.some(value => value.hold > 0 || value.level > 0)) frame = requestAnimationFrame(draw);
    }

    function sample(left: number, right: number) {
      const now = performance.now();
      [left, right].forEach((peak, index) => {
        const level = fill(peak > 0 ? 20 * Math.log10(peak) : -Infinity);
        const value = values[index];
        value.level = Math.max(value.level, level);
        if (level >= value.hold && level > 0) {
          value.hold = level;
          value.until = now + PEAK_HOLD_MS;
        }
      });
      if (!frame && values.some(value => value.level > 0)) {
        previous = now;
        frame = requestAnimationFrame(draw);
      }
    }

    const current = useTransportStore.getState();
    sample(current.masterPeakL, current.masterPeakR);
    const unsubscribe = useTransportStore.subscribe((state, prev) => {
      if (state.masterLevelSequence !== prev.masterLevelSequence) {
        sample(state.masterPeakL, state.masterPeakR);
      }
    });
    return () => { unsubscribe(); cancelAnimationFrame(frame); };
  }, []);

  return <div className="desk-output" ref={meter}>
    <div className="desk-output-heading"><label htmlFor="desk-master-gain">Master</label>
      <output htmlFor="desk-master-gain">{volume.toFixed(1)} dB</output></div>
    <div className="desk-meter-ticks" aria-hidden="true">{ticks.map(tick =>
      <span key={tick} style={{left: `${fill(tick) * 100}%`}}>{tick}</span>)}</div>
    {(["l", "r"] as const).map(ch => <div className="desk-meter-row" key={ch} aria-hidden="true">
      <small>{ch.toUpperCase()}</small><div className="desk-meter-track">
        <i style={{clipPath: `inset(0 calc(100% - var(--level-${ch}, 0%)) 0 0)`}} />
        <b style={{left: `var(--hold-${ch}, 0%)`}} />
      </div></div>)}
    <input id="desk-master-gain" aria-label="Master output level" aria-valuetext={`${volume.toFixed(1)} dB`}
      type="range" min={MIN_DB} max={0} step={0.5} value={volume} onChange={e => {
        const db = Number(e.currentTarget.value);
        volumeEdited.current = true;
        setVolume(db);
        void setMasterVolume(db).catch(console.error);
      }} />
  </div>;
}
