import { useState } from "react";
import type { FixtureParam, FixtureType, ParamKind, PatchedFixture } from "../../lib/types";
import { channelSlots, fixtureParameterAddress } from "../../lib/fixtureChannels";
import "./fixtureProfile.css";

const roles: ParamKind[] = [
  "intensity", "red", "green", "blue", "cyan", "magenta", "yellow", "hue", "saturation",
  "hsi_intensity", "hsv_value", "white", "amber", "uv", "pan", "tilt", "generic",
];
const roleLabels: Partial<Record<ParamKind, string>> = { hsi_intensity: "HSI intensity", hsv_value: "HSV value" };

export function FixtureProfile({ fixture, save, exportSaved, saveToLibrary, close }: {
  fixture: PatchedFixture;
  save: (profile: FixtureType) => Promise<void>;
  exportSaved?: () => Promise<void>;
  saveToLibrary?: () => Promise<void>;
  close: () => void;
}) {
  const [draft, setDraft] = useState(fixture.fixture_type);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  function updateParameter(index: number, edit: Partial<FixtureParam>) {
    setDraft(previous => ({
      ...previous,
      parameters: previous.parameters.map((p, i) => i === index ? { ...p, ...edit } : p),
    }));
  }
  async function apply() {
    if (busy || !draft.name.trim()) return;
    if (draft.parameters.some(({ fine_channel_offset: fine }) =>
      fine !== undefined && (!Number.isInteger(fine) || fine < 0 || fine >= 512))) {
      setError("Enter a fine slot from 1 to 512 relative to the profile.");
      return;
    }
    setBusy(true);
    try {
      await save({ ...draft, name: draft.name.trim(), mode: draft.mode?.trim(), manufacturer: draft.manufacturer?.trim() });
      close();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  }
  async function copySavedProfile(copy: () => Promise<void>) {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await copy();
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
      <table><thead><tr><th>DMX address</th><th>Parameter</th><th>Width</th><th>Fine slot</th><th>Control role</th></tr></thead>
        <tbody>{draft.parameters.map((p, i) => <tr key={i}>
          <td>{channelSlots(fixtureParameterAddress(fixture, p)).map(slot => slot + 1)
            .join(p.fine_channel_offset === undefined ? "–" : " / ")}</td><td>{p.name}</td>
          <td>{p.width === "Bit16" ? "16 bit" : "8 bit"}</td>
          <td>{p.width === "Bit16" && <input className="fixture-profile-fine-slot" type="number" min={1} max={512} step={1}
            aria-label={`Fine slot for ${p.name}`} placeholder={String(p.channel_offset + 2)}
            value={p.fine_channel_offset === undefined ? "" : p.fine_channel_offset + 1}
            onChange={e => updateParameter(i, { fine_channel_offset: e.target.value === "" ? undefined : Number(e.target.value) - 1 })} />}</td>
          <td><select aria-label={`Control role for ${p.name}`} value={p.kind} onChange={e => updateParameter(i, { kind: e.target.value as ParamKind })}>
            {roles.map(role => <option key={role} value={role}>{roleLabels[role] ?? role}</option>)}
          </select></td>
        </tr>)}</tbody>
      </table>
      {draft.parameters.some(parameter => parameter.width === "Bit16") &&
        <p>Fine slots are relative to the fixture. Leave the slot blank to use the channel after coarse.</p>}
      <p>Assign red, green and blue to link additive channels to the color picker. Repeated RGB roles move together. Assign one cyan, magenta and yellow for CMY. For HSI or HSV, assign one hue, one saturation and one matching HSI intensity or HSV value. Use intensity for a separate master dimmer.</p>
      <div className="fixture-profile-actions">
        <button type="button" onClick={apply} disabled={!draft.name.trim()}>Save profile</button>
        {exportSaved && <button type="button" onClick={() => void copySavedProfile(exportSaved)}>Export saved profile…</button>}
        {saveToLibrary && <button type="button" onClick={() => void copySavedProfile(saveToLibrary)}>Add saved profile to library</button>}
        <button type="button" onClick={close}>Cancel</button>
        {error && <span role="alert">{error}</span>}
      </div>
    </fieldset>
  </section>;
}
