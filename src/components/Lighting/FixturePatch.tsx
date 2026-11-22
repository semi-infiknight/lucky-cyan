// Fixture patch (M3): name lighting instruments, place them at DMX addresses,
// identify them (test), and warn about address clashes. Backed by the workspace
// (`list_fixtures` / `add_fixture` / `update_fixture` / `remove_fixture`).

import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { ask, open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import {
  addFixture,
  dmxTestFixture,
  exportFixtureProfile,
  listBuiltinFixtureTypes,
  listFixtureLibrary,
  loadFixturePatch,
  patchImportedFixture,
  patchLibraryFixture,
  readFixtureProfile,
  removeFixture,
  removeLibraryFixtureProfile,
  saveLibraryFixtureProfile,
  savePatchedFixtureToLibrary,
  updateFixture,
} from "../../lib/commands";
import type { FixtureConflict, FixtureLibrary, FixtureLibraryEntry, FixtureType, PatchedFixture } from "../../lib/types";
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
  const [profileChoice, setProfileChoice] = useState("builtin:0");
  const [library, setLibrary] = useState<FixtureLibrary>({ profiles: [], unreadable: 0 });
  const [libraryStatus, setLibraryStatus] = useState("");
  const libraryRequests = useRef(0);
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
  const refreshLibrary = useCallback(async () => {
    const request = ++libraryRequests.current;
    const data = await listFixtureLibrary();
    if (request === libraryRequests.current) setLibrary(data);
  }, []);

  useEffect(() => {
    listBuiltinFixtureTypes().then(setTypes).catch(console.error);
    refresh().catch(error => setPatchError(String(error)));
    refreshLibrary().catch(error => setPatchError(String(error)));
    // Turn any active identify off when the panel closes.
    return () => {
      if (identifyingRef.current) dmxTestFixture(identifyingRef.current, false).catch(() => {});
    };
  }, [refresh, refreshLibrary]);

  const conflictIds = new Set(conflicts.flatMap((c) => [c.fixture_a, c.fixture_b]));
  const libraryProfile = library.profiles.find(profile => profileChoice === `library:${profile.id}`);
  const localProfile = profileChoice === "imported" ? importedProfile
    : profileChoice.startsWith("builtin:") ? types[Number(profileChoice.slice("builtin:".length))] : undefined;
  const profileName = libraryProfile?.name ?? localProfile?.name;
  const profileChannels = libraryProfile?.channels ?? (localProfile ? fixtureFootprint(localProfile) : 0);

  const handleAdd = async () => {
    if (patchBusy || !profileName) return;
    // Give repeated copies distinguishable labels until the operator renames them.
    const base = profileName.trim().split(/\s+/)[0];
    let n = 1;
    const labels = new Set(fixtures.map(fixture => fixture.label));
    while (labels.has(`${base} ${n}`)) n += 1;
    const label = `${base} ${n}`;

    setPatchBusy(true);
    setPatchError("");
    try {
      const placement = { workspace_id: workspaceId, label, universe: newUniverse, base_address: newAddress };
      if (libraryProfile) {
        await patchLibraryFixture({ ...placement, profile_id: libraryProfile.id });
      } else if (localProfile) {
        if (profileChoice === "imported") {
          await patchImportedFixture({ ...placement, profile: localProfile });
        } else {
          await addFixture(label, newUniverse, newAddress, localProfile);
        }
      }
      setNewAddress(Math.min(512, newAddress + profileChannels));
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
      setProfileChoice("imported");
    } catch (error) {
      setPatchError(String(error));
    } finally {
      setPatchBusy(false);
    }
  };

  async function selectLibrarySave(entry: FixtureLibraryEntry) {
    setLibraryStatus(`Saved ${entry.name} to the fixture library.`);
    await refreshLibrary();
    setProfileChoice(`library:${entry.id}`);
  }

  async function rememberImportedProfile() {
    if (!importedProfile || patchBusy) return;
    setPatchBusy(true);
    setPatchError("");
    try {
      await selectLibrarySave(await saveLibraryFixtureProfile(importedProfile));
    } catch (cause) {
      setPatchError(String(cause));
    } finally {
      setPatchBusy(false);
    }
  }

  async function removeFromLibrary(profile: FixtureLibraryEntry) {
    if (patchBusy) return;
    setPatchBusy(true);
    setPatchError("");
    try {
      if (!await ask(`Remove ${profile.name} from this machine’s fixture library? Patched shows keep their own copies.`, {
        title: "Remove fixture profile", kind: "warning",
      })) return;
      await removeLibraryFixtureProfile(profile.id);
      setProfileChoice("builtin:0");
      setLibraryStatus(`Removed ${profile.name} from the fixture library.`);
      await refreshLibrary();
    } catch (cause) {
      setPatchError(String(cause));
    } finally {
      setPatchBusy(false);
    }
  }

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
            saveToLibrary={async () => selectLibrarySave(await savePatchedFixtureToLibrary(workspaceId, f.id, f.fixture_type))}
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
          value={profileChoice}
          disabled={patchBusy}
          onChange={e => { setProfileChoice(e.target.value); setLibraryStatus(""); }}
        >
          <optgroup label="Built-in profiles">
            {types.map((t, i) => <option key={i} value={`builtin:${i}`}>{t.name}</option>)}
          </optgroup>
          {library.profiles.length > 0 && <optgroup label="Fixture library">
            {library.profiles.map(profile => <option key={profile.id} value={`library:${profile.id}`}>
              {[profile.manufacturer, profile.name, profile.mode].filter(Boolean).join(" · ")}
            </option>)}
          </optgroup>}
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
        <button type="button" style={{ ...smallBtn, color: "#a855f7" }} disabled={patchBusy || !profileName}
          onClick={handleAdd}>Patch fixture</button>
      </div>
      <div style={{ display: "flex", alignItems: "center", flexWrap: "wrap", gap: 6 }}>
        <button type="button" style={smallBtn} disabled={patchBusy} onClick={importProfile}>Import profile…</button>
        {importedProfile && profileChoice === "imported" && <button type="button" style={smallBtn}
          disabled={patchBusy} onClick={rememberImportedProfile}>Add imported profile to library</button>}
        {libraryProfile && <button type="button" style={smallBtn} disabled={patchBusy}
          onClick={() => void removeFromLibrary(libraryProfile)}>Remove from library…</button>}
        {(libraryProfile || profileChoice === "imported") && profileName && <small style={{ color: "var(--wc-text-secondary)" }}>
          {[libraryProfile?.manufacturer ?? localProfile?.manufacturer, libraryProfile?.mode ?? localProfile?.mode,
            `${profileChannels} channels`].filter(Boolean).join(" · ")}
        </small>}
      </div>
      {libraryStatus && <p role="status">{libraryStatus}</p>}
      {library.unreadable > 0 && <p role="status">
        {library.unreadable} fixture library {library.unreadable === 1 ? "file could" : "files could"} not be read.
      </p>}
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
