import { useState } from "react";
import type { FixtureType, ParamKind, PatchedFixture } from "../../lib/types";
import "./fixtureProfile.css";

const roles: ParamKind[] = [
  "intensity", "red", "green", "blue", "cyan", "magenta", "yellow", "hue", "saturation",
  "hsi_intensity", "hsv_value", "white", "amber", "uv", "pan", "tilt", "generic",
];
const roleLabels: Partial<Record<ParamKind, string>> = { hsi_intensity: "HSI intensity", hsv_value: "HSV value" };

export function FixtureProfile({ fixture, save, exportSaved, close }: {
  fixture: PatchedFixture;
  save: (profile: FixtureType) => Promise<void>;
  exportSaved?: () => Promise<void>;
  close: () => void;
}) {
  const [draft, setDraft] = useState(fixture.fixture_type);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  function updateRole(index: number, kind: ParamKind) {
    setDraft(previous => ({
      ...previous,
      parameters: previous.parameters.map((p, i) => i === index ? { ...p, kind } : p),
    }));
  }
  async function apply() {
    if (busy || !draft.name.trim()) return;
    setBusy(true);
    try {
      await save({ ...draft, name: draft.name.trim(), mode: draft.mode?.trim(), manufacturer: draft.manufacturer?.trim() });
      close();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  }
  async function exportProfile() {
    if (!exportSaved || busy) return;
    setBusy(true);
    setError("");
    try {
      await exportSaved();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }
  return <section className="fixture-profile" aria-label={`Fixture profile for ${fixture.label}`}>
    <fieldset disabled={busy}>
      <legend>{fixture.label} · profile</legend>
      <div className="fixture-profile-fields">
        <label>Manufacturer<input value={draft.manufacturer ?? ""} onChange={e => setDraft({ ...draft, manufacturer: e.target.value })} /></label>
        <label>Model / definition<input value={draft.name} required onChange={e => setDraft({ ...draft, name: e.target.value })} /></label>
        <label>Mode<input value={draft.mode ?? ""} onChange={e => setDraft({ ...draft, mode: e.target.value })} /></label>
      </div>
      <table><thead><tr><th>Address</th><th>Parameter</th><th>Width</th><th>Control role</th></tr></thead>
        <tbody>{draft.parameters.map((p, i) => <tr key={i}>
          <td>{fixture.base_address + p.channel_offset}
            {p.width === "Bit16" && `–${fixture.base_address + p.channel_offset + 1}`}</td><td>{p.name}</td>
          <td>{p.width === "Bit16" ? "16 bit" : "8 bit"}</td>
          <td><select aria-label={`Control role for ${p.name}`} value={p.kind} onChange={e => updateRole(i, e.target.value as ParamKind)}>
            {roles.map(role => <option key={role} value={role}>{roleLabels[role] ?? role}</option>)}
          </select></td>
        </tr>)}</tbody>
      </table>
      <p>Assign red, green and blue to link additive channels to the color picker. Repeated RGB roles move together. Assign one cyan, magenta and yellow for CMY. For HSI or HSV, assign one hue, one saturation and one matching HSI intensity or HSV value. Use intensity for a separate master dimmer.</p>
      <div className="fixture-profile-actions">
        <button type="button" onClick={apply} disabled={!draft.name.trim()}>Save profile</button>
        {exportSaved && <button type="button" onClick={exportProfile}>Export saved profile…</button>}
        <button type="button" onClick={close}>Cancel</button>
        {error && <span role="alert">{error}</span>}
      </div>
    </fieldset>
  </section>;
}
