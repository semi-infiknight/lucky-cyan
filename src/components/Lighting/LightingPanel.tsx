// Floating DMX lighting panel (M1 dev surface): configure universe outputs
// (sACN / Art-Net), poke a channel, toggle blackout, and watch the live values
// streamed from the backend `dmx-monitor` event.

import { useEffect, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  dmxGetOutputs,
  dmxSetChannel,
  dmxSetOutputs,
} from "../../lib/commands";
import type { DmxUniverseSnapshot, OutputProtocol, UniverseOutput } from "../../lib/types";
import { FixturePatch } from "./FixturePatch";
import { FixtureDashboard } from "./FixtureDashboard";
import { GroupManager } from "./GroupManager";
import { DragNumber } from "../common/DragNumber";

export function LightingPanel({ onClose, showMode = false, blackout, onBlackoutChange, workspaceId }: {
  onClose: () => void;
  showMode?: boolean;
  blackout: boolean | null;
  onBlackoutChange: (on: boolean) => void;
  workspaceId?: string;
}) {
  const [section, setSection] = useState<"Patch" | "Live controls" | "Output monitor">("Patch");
  const activeSection = showMode ? "Output monitor" : section;
  const [outputs, setOutputs] = useState<UniverseOutput[]>([]);
  const [snapshot, setSnapshot] = useState<DmxUniverseSnapshot[]>([]);

  // Load the workspace's stored outputs (the backend already bound them to the
  // engine on workspace load).
  useEffect(() => {
    dmxGetOutputs().then(setOutputs).catch(console.error);
  }, []);

  // Live monitor feed.
  useEffect(() => {
    const unlisten = listen<DmxUniverseSnapshot[]>("dmx-monitor", (e) => setSnapshot(e.payload));
    return () => { void unlisten.then((u) => u()).catch(console.error); };
  }, []);

  // Persist + push outputs through the workspace-backed command.
  const pushOutputs = (next: UniverseOutput[]) => {
    setOutputs(next);
    dmxSetOutputs(next).catch(console.error);
  };

  const updateOutput = (i: number, patch: Partial<UniverseOutput>) =>
    pushOutputs(outputs.map((o, j) => (j === i ? { ...o, ...patch } : o)));

  const addOutput = () =>
    pushOutputs([...outputs, { universe: outputs.length + 1, protocol: "Sacn", destination: null, enabled: true }]);

  const removeOutput = (i: number) => pushOutputs(outputs.filter((_, j) => j !== i));


  return (
    <div style={panelStyle} role="region" aria-label="Lighting desk" onClick={(e) => e.stopPropagation()}
      onKeyDown={event => { if (event.key !== "Escape" && event.key !== "F5") event.stopPropagation(); }}>
      {/* Header */}
      <div style={headerStyle}>
        <span style={{ color: blackout ? "#ef4444" : "#a855f7", fontSize: 10 }}>●</span>
        <span style={{ color: "var(--wc-text-secondary)", fontWeight: 600, fontSize: 12, flex: 1 }}>Lighting (DMX)</span>
        <button disabled={blackout === null} aria-pressed={blackout ?? undefined} onClick={() => onBlackoutChange(!blackout)} style={blackout ? blackoutOnBtn : blackoutOffBtn}>
          {blackout ? "Restore lights" : "Blackout"}
        </button>
        <button aria-label="Close lighting desk" onClick={onClose} style={closeBtn}>✕</button>
      </div>

      {!showMode && <div role="group" aria-label="Lighting desk views" style={{ display: "flex", gap: 4, padding: "8px 10px 0" }}>
        {(["Patch", "Live controls", "Output monitor"] as const).map(view =>
          <button key={view} aria-pressed={activeSection === view} onClick={() => setSection(view)}
            style={{ ...viewButtonStyle, ...(activeSection === view ? activeViewStyle : {}) }}>{view}</button>)}
      </div>}
      <div style={{ overflowY: "auto", flex: 1, padding: "8px 10px", display: "flex", flexDirection: "column", gap: 12 }}>
        {activeSection === "Patch" && <>
        {/* Outputs */}
        <Section title="Universe outputs">
          {outputs.map((o, i) => (
            <div key={i} style={{ display: "flex", gap: 6, alignItems: "center" }}>
              <input
                type="checkbox"
                checked={o.enabled}
                onChange={(e) => updateOutput(i, { enabled: e.target.checked })}
                title="Enabled"
              />
              <label style={lblStyle}>U</label>
              <DragNumber min={1} max={63999} value={o.universe}
                onChange={(e) => updateOutput(i, { universe: Number(e.target.value) })}
                style={{ ...numStyle, width: 52 }}
              />
              <select
                value={o.protocol}
                onChange={(e) => updateOutput(i, { protocol: e.target.value as OutputProtocol })}
                style={selStyle}
              >
                <option value="Sacn">sACN</option>
                <option value="ArtNet">Art-Net</option>
              </select>
              <input
                type="text"
                placeholder={o.protocol === "Sacn" ? "multicast" : "dest IP (required)"}
                value={o.destination ?? ""}
                onChange={(e) => updateOutput(i, { destination: e.target.value.trim() || null })}
                style={{ ...txtStyle, flex: 1 }}
              />
              <button onClick={() => removeOutput(i)} style={smallBtn} title="Remove">✕</button>
            </div>
          ))}
          <button onClick={addOutput} style={addBtn}>+ universe</button>
        </Section>

        {/* Fixtures */}
        <Section title="Fixtures (patch)">
          {workspaceId && <FixturePatch key={workspaceId} workspaceId={workspaceId} />}
        </Section>

        {/* Groups */}
        <Section title="Groups">
          <GroupManager />
        </Section>

        </>}
        {activeSection === "Live controls" && workspaceId && <>
        {/* Live dashboard */}
        <Section title="Dashboard (live)">
          <FixtureDashboard key={workspaceId} workspaceId={workspaceId} />
        </Section>

        {/* Test channel */}
        <Section title="Test channel (no fade)">
          <TestChannel key={workspaceId} workspaceId={workspaceId} />
        </Section>

        </>}
        {/* Monitor */}
        {activeSection === "Output monitor" && <Section title="Live output">
          {snapshot.length === 0 ? (
            <div style={{ color: "var(--wc-text-faint)", fontSize: 12 }}>No active universe.</div>
          ) : (
            snapshot.map((u) => {
              const active = u.channels
                .map((v, idx) => ({ addr: idx + 1, v }))
                .filter((c) => c.v > 0);
              return (
                <div key={u.universe} style={{ marginBottom: 4 }}>
                  <span style={{ color: "#a855f7", fontWeight: 600 }}>U{u.universe}</span>
                  {active.length === 0 ? (
                    <span style={{ color: "var(--wc-text-faint)", marginLeft: 8 }}>all zero</span>
                  ) : (
                    <span style={{ marginLeft: 8, display: "inline-flex", flexWrap: "wrap", gap: 6 }}>
                      {active.map((c) => (
                        <span key={c.addr} style={chipStyle}>
                          <span style={{ color: "var(--wc-text-muted)" }}>{c.addr}</span>:
                          <span style={{ color: "var(--wc-text)" }}>{c.v}</span>
                        </span>
                      ))}
                    </span>
                  )}
                </div>
              );
            })
          )}
        </Section>}
      </div>
    </div>
  );
}

function TestChannel({ workspaceId }: { workspaceId: string }) {
  const [universe, setUniverse] = useState(1);
  const [address, setAddress] = useState(1);
  const [value, setValue] = useState(0);
  const [error, setError] = useState("");

  function poke(nextUniverse: number, nextAddress: number, nextValue: number) {
    setUniverse(nextUniverse);
    setAddress(nextAddress);
    setValue(nextValue);
    setError("");
    void dmxSetChannel(workspaceId, nextUniverse, nextAddress, nextValue).catch(e => setError(String(e)));
  }

  return <>
    <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
      <label style={lblStyle}>U</label>
      <DragNumber min={1} value={universe}
        onChange={e => poke(Number(e.target.value), address, value)}
        style={{ ...numStyle, width: 48 }} />
      <label style={lblStyle}>ch</label>
      <DragNumber min={1} max={512} value={address}
        onChange={e => poke(universe, Number(e.target.value), value)}
        style={{ ...numStyle, width: 56 }} />
      <input type="range" min={0} max={255} value={value}
        onChange={e => poke(universe, address, Number(e.target.value))}
        style={{ flex: 1 }} />
      <span style={{ color: "var(--wc-text)", width: 30, textAlign: "right" }}>{value}</span>
    </div>
    {error && <div role="alert">{error}</div>}
  </>;
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
      <div style={{ color: "var(--wc-text-muted)", fontSize: 10, textTransform: "uppercase", letterSpacing: 0.5 }}>{title}</div>
      {children}
    </div>
  );
}

const panelStyle: CSSProperties = {
  position: "fixed", bottom: 104, right: 16, width: "min(560px, calc(100vw - 32px))", maxHeight: "min(560px, calc(100vh - 130px))",
  background: "var(--wc-bg-deepest)", border: "1px solid var(--wc-border-strong)", borderRadius: 8,
  boxShadow: "0 8px 32px rgba(0,0,0,0.7)", zIndex: 9999,
  display: "flex", flexDirection: "column", fontFamily: "inherit", fontSize: 12,
};
const headerStyle: CSSProperties = {
  display: "flex", alignItems: "center", padding: "6px 10px",
  borderBottom: "1px solid var(--wc-border)", gap: 8, flexShrink: 0,
};
const viewButtonStyle: CSSProperties = {
  padding: "5px 9px", cursor: "pointer", color: "var(--wc-text-secondary)",
  borderRadius: 4, border: "1px solid var(--wc-border)", background: "var(--wc-bg-app)", fontSize: 12,
};
const activeViewStyle: CSSProperties = {
  background: "var(--wc-bg-surface)", borderColor: "var(--wc-text-secondary)", color: "var(--wc-text)", fontWeight: 600,
};
const lblStyle: CSSProperties = { color: "var(--wc-text-muted)", fontSize: 11 };
const numStyle: CSSProperties = {
  background: "var(--wc-bg-app)", border: "1px solid var(--wc-border-strong)", borderRadius: 4, color: "var(--wc-text)", padding: "2px 4px", fontSize: 12,
};
const txtStyle: CSSProperties = { ...numStyle };
const selStyle: CSSProperties = { ...numStyle, cursor: "pointer" };
const chipStyle: CSSProperties = {
  background: "var(--wc-bg-app)", border: "1px solid var(--wc-border)", borderRadius: 4, padding: "0 5px", fontSize: 11,
};
const smallBtn: CSSProperties = {
  background: "none", border: "1px solid var(--wc-border-strong)", borderRadius: 4, color: "var(--wc-text-muted)", fontSize: 11, padding: "1px 6px", cursor: "pointer",
};
const addBtn: CSSProperties = { ...smallBtn, alignSelf: "flex-start", color: "#a855f7" };
const closeBtn: CSSProperties = {
  background: "none", border: "none", color: "var(--wc-text-muted)", fontSize: 16, cursor: "pointer", lineHeight: 1, padding: "0 2px",
};
const blackoutOffBtn: CSSProperties = {
  background: "none", border: "1px solid var(--wc-border-strong)", borderRadius: 4, color: "var(--wc-text-secondary)", fontSize: 11, padding: "1px 8px", cursor: "pointer",
};
const blackoutOnBtn: CSSProperties = {
  background: "#7f1d1d", border: "1px solid #ef4444", borderRadius: 4, color: "#fecaca", fontSize: 11, padding: "1px 8px", cursor: "pointer", fontWeight: 700,
};
