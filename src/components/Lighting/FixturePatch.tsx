// Fixture patch (M3): name lighting instruments, place them at DMX addresses,
// identify them (test), and warn about address clashes. Backed by the workspace
// (`list_fixtures` / `add_fixture` / `update_fixture` / `remove_fixture`).

import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  addFixture,
  dmxTestFixture,
  exportFixtureProfile,
  listBuiltinFixtureTypes,
  loadFixturePatch,
  patchImportedFixture,
  readFixtureProfile,
  removeFixture,
  updateFixture,
} from "../../lib/commands";
import type { FixtureConflict, FixtureType, PatchedFixture } from "../../lib/types";
import { fixtureFootprint } from "../../lib/fixtureChannels";
import { FixtureProfile } from "./FixtureProfile";
import { Select } from "../common/Select";
import { DragNumber } from "../common/DragNumber";

const PROFILE_EXTENSION = "inkue-fixture";
const profileFilter = { name: "Inkue Fixture Profile", extensions: [PROFILE_EXTENSION] };

export function FixturePatch({ workspaceId }: { workspaceId: string }) {
  const [fixtures, setFixtures] = useState<PatchedFixture[]>([]);
  const [types, setTypes] = useState<FixtureType[]>([]);
  const [conflicts, setConflicts] = useState<FixtureConflict[]>([]);
  const [identifying, setIdentifying] = useState<string | null>(null);
  const identifyingRef = useRef<string | null>(null);
  identifyingRef.current = identifying;

  const [profileId, setProfileId] = useState<string | null>(null);
  const [newTypeIdx, setNewTypeIdx] = useState<number | "imported">(0);
  const [importedProfile, setImportedProfile] = useState<FixtureType | null>(null);
  const [patchBusy, setPatchBusy] = useState(false);
  const [patchError, setPatchError] = useState("");
  const [newUniverse, setNewUniverse] = useState(1);
  const [newAddress, setNewAddress] = useState(1);

  const refresh = useCallback(async () => {
    const patch = await loadFixturePatch(workspaceId);
    setFixtures(patch.fixtures);
    setConflicts(patch.conflicts);
  }, [workspaceId]);

  useEffect(() => {
    listBuiltinFixtureTypes().then(setTypes).catch(console.error);
    refresh().catch(error => setPatchError(String(error)));
    // Turn any active identify off when the panel closes.
    return () => {
      if (identifyingRef.current) dmxTestFixture(identifyingRef.current, false).catch(() => {});
    };
  }, [refresh]);

  const conflictIds = new Set(conflicts.flatMap((c) => [c.fixture_a, c.fixture_b]));

  const handleAdd = async () => {
    if (patchBusy) return;
    const t = newTypeIdx === "imported" ? importedProfile : types[newTypeIdx];
    if (!t) return;
    // Give repeated copies distinguishable labels until the operator renames them.
    const base = t.name.trim().split(/\s+/)[0];
    let n = 1;
    const labels = new Set(fixtures.map(fixture => fixture.label));
    while (labels.has(`${base} ${n}`)) n += 1;
    const label = `${base} ${n}`;

    setPatchBusy(true);
    setPatchError("");
    try {
      if (newTypeIdx === "imported") {
        await patchImportedFixture({ workspace_id: workspaceId, label, universe: newUniverse,
          base_address: newAddress, profile: t });
      } else {
        await addFixture(label, newUniverse, newAddress, t);
      }
      setNewAddress(Math.min(512, newAddress + fixtureFootprint(t)));
      await refresh();
    } catch (error) {
      setPatchError(String(error));
    } finally {
      setPatchBusy(false);
    }
  };

  const importProfile = async () => {
    if (patchBusy) return;
    setPatchBusy(true);
    setPatchError("");
    try {
      const path = await openDialog({ multiple: false, filters: [profileFilter] });
      if (typeof path !== "string") return;
      setImportedProfile(await readFixtureProfile(path));
      setNewTypeIdx("imported");
    } catch (error) {
      setPatchError(String(error));
    } finally {
      setPatchBusy(false);
    }
  };

  async function exportProfile(fixture: PatchedFixture) {
    const name = fixture.fixture_type.name.replace(/[<>:"/\\|?*]/g, "_");
    const suffix = "." + PROFILE_EXTENSION;
    const path = await saveDialog({ filters: [profileFilter], defaultPath: (name || "Fixture profile") + suffix });
    if (typeof path !== "string") return;
    await exportFixtureProfile(path.toLowerCase().endsWith(suffix) ? path : path + suffix,
      workspaceId, fixture.id, fixture.fixture_type);
  }

  const commit = async (fixture: PatchedFixture, patch: Partial<PatchedFixture>) => {
    await updateFixture({ ...fixture, ...patch }).catch(console.error);
    await refresh();
  };

  const handleRemove = async (id: string) => {
    if (identifying === id) {
      await dmxTestFixture(id, false).catch(() => {});
      setIdentifying(null);
    }
    await removeFixture(id).catch(console.error);
    await refresh();
  };

  const toggleIdentify = async (id: string) => {
    if (identifying === id) {
      await dmxTestFixture(id, false).catch(console.error);
      setIdentifying(null);
      return;
    }
    if (identifying) await dmxTestFixture(identifying, false).catch(() => {});
    await dmxTestFixture(id, true).catch(console.error);
    setIdentifying(id);
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
      {fixtures.length === 0 && (
        <div style={{ color: "var(--wc-text-faint)", fontSize: 12 }}>No fixtures patched yet — add one below.</div>
      )}

      {fixtures.map((f) => {
        const fp = fixtureFootprint(f.fixture_type);
        const last = f.base_address + fp - 1;
        const conflict = conflictIds.has(f.id);
        return (
          <Fragment key={f.id}><div
            style={{ ...rowStyle, borderColor: conflict ? "#b45309" : "var(--wc-border)" }}
          >
            <input
              style={{ ...txtStyle, flex: 1, minWidth: 0 }}
              value={f.label}
              onChange={(e) => commit(f, { label: e.target.value })}
              title="Fixture label"
            />
            <span style={{ color: "var(--wc-text-muted)", fontSize: 10, flexShrink: 0 }}>{f.fixture_type.name}</span>
            <label style={lblStyle}>U</label>
            <DragNumber
              style={{ ...numStyle, width: 42 }} min={1} max={63999} value={f.universe}
              onChange={(e) => commit(f, { universe: Number(e.target.value) })}
              title="Universe"
            />
            <label style={lblStyle}>@</label>
            <DragNumber
              style={{ ...numStyle, width: 48 }} min={1} max={512} value={f.base_address}
              onChange={(e) => commit(f, { base_address: Number(e.target.value) })}
              title="DMX start address"
            />
            <span style={{ color: "var(--wc-text-faint)", fontSize: 10, width: 52, flexShrink: 0 }}>
              →{last} ({fp}ch)
            </span>
            <button
              style={identifying === f.id ? identifyOnBtn : smallBtn}
              onClick={() => toggleIdentify(f.id)}
              title="Identify — drive this fixture to a visible level"
            >
              {identifying === f.id ? "◉" : "○"}
            </button>
            <button style={smallBtn} aria-expanded={profileId === f.id} onClick={() => setProfileId(profileId === f.id ? null : f.id)}>Profile</button>
            <button style={smallBtn} onClick={() => handleRemove(f.id)} title="Remove">✕</button>
          </div>
          {profileId === f.id && <FixtureProfile fixture={f} close={() => setProfileId(null)}
            exportSaved={() => exportProfile(f)}
            save={async fixture_type => { await updateFixture({ ...f, fixture_type }); await refresh(); }} />}
          </Fragment>
        );
      })}

      {conflicts.length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: 2, marginTop: 2 }}>
          {conflicts.map((c, i) => (
            <div key={i} style={{ color: "#f59e0b", fontSize: 11 }}>⚠ {c.message}</div>
          ))}
        </div>
      )}

      {/* Add fixture */}
      <div style={{ ...rowStyle, marginTop: 4, background: "var(--wc-bg-app)" }}>
        <Select
          style={{ ...selStyle, flex: 1, minWidth: 0 }}
          aria-label="Fixture profile to patch"
          value={newTypeIdx}
          disabled={patchBusy}
          onChange={(e) => setNewTypeIdx(e.target.value === "imported" ? "imported" : Number(e.target.value))}
        >
          {types.map((t, i) => (
            <option key={i} value={i}>{t.name}</option>
          ))}
          {importedProfile && <option value="imported">{importedProfile.name} · imported</option>}
        </Select>
        <label style={lblStyle}>U</label>
        <DragNumber
          aria-label="New fixture universe"
          style={{ ...numStyle, width: 42 }} min={1} max={63999} value={newUniverse}
          onChange={(e) => setNewUniverse(Number(e.target.value))}
        />
        <label style={lblStyle}>@</label>
        <DragNumber
          aria-label="New fixture start address"
          style={{ ...numStyle, width: 48 }} min={1} max={512} value={newAddress}
          onChange={(e) => setNewAddress(Number(e.target.value))}
        />
        <button type="button" style={{ ...smallBtn, color: "#a855f7" }} disabled={patchBusy}
          onClick={handleAdd}>{newTypeIdx === "imported" ? "Patch fixture" : "+ add"}</button>
      </div>
      <div style={{ display: "flex", alignItems: "center", flexWrap: "wrap", gap: 6 }}>
        <button type="button" style={smallBtn} disabled={patchBusy} onClick={importProfile}>Import profile…</button>
        {importedProfile && newTypeIdx === "imported" && <small role="status" style={{ color: "var(--wc-text-secondary)" }}>
          {[importedProfile.manufacturer, importedProfile.mode, `${fixtureFootprint(importedProfile)} channels`]
            .filter(Boolean).join(" · ")}
        </small>}
      </div>
      {patchError && <p role="alert">{patchError}</p>}
    </div>
  );
}

const rowStyle: CSSProperties = {
  display: "flex", gap: 6, alignItems: "center",
  border: "1px solid var(--wc-border)", borderRadius: 4, padding: "3px 5px",
};
const lblStyle: CSSProperties = { color: "var(--wc-text-muted)", fontSize: 11, flexShrink: 0 };
const numStyle: CSSProperties = {
  background: "var(--wc-bg-app)", border: "1px solid var(--wc-border-strong)", borderRadius: 4, color: "var(--wc-text)", padding: "2px 4px", fontSize: 12,
};
const txtStyle: CSSProperties = { ...numStyle };
const selStyle: CSSProperties = { ...numStyle, cursor: "pointer" };
const smallBtn: CSSProperties = {
  background: "none", border: "1px solid var(--wc-border-strong)", borderRadius: 4, color: "var(--wc-text-muted)", fontSize: 11, padding: "1px 6px", cursor: "pointer", flexShrink: 0,
};
const identifyOnBtn: CSSProperties = {
  ...smallBtn, color: "#fde047", borderColor: "#a16207", background: "#422006",
};
