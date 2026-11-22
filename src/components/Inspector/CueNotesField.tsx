import { useEffect, useId, useRef, useState } from "react";
import { setCueNotes } from "../../lib/commands";
import { findCue } from "../../lib/cueTree";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { Field, inputStyle } from "./Field";

interface NoteDraft {
  base: string;
  text: string;
  saved: string;
}

function cleanNote(note: string): NoteDraft {
  return { base: note, text: note, saved: note };
}

export function observeSavedNote(draft: NoteDraft, saved: string): NoteDraft {
  if (draft.saved === saved) return draft;
  if (draft.text === draft.base || draft.text === saved) return cleanNote(saved);
  return { ...draft, saved };
}

export function noteSaveRequest(draft: NoteDraft, useSavedVersion: boolean) {
  if (draft.text === draft.saved) return null;
  if (!useSavedVersion && (draft.text === draft.base || draft.saved !== draft.base)) return null;
  return { expectedNote: useSavedVersion ? draft.saved : draft.base, note: draft.text };
}

export function CueNotesField({ cueId }: { cueId: string }) {
  const workspaceId = useWorkspaceStore(s => s.workspaceInfo?.instance_id);
  const listId = useWorkspaceStore(s => s.activeCueListId);
  const savedNote = useWorkspaceStore(s => findCue(s.cues, cueId)?.notes);
  return <Field label="Notes">
    {workspaceId && listId && savedNote !== undefined
      ? <ScopedCueNotes key={`${workspaceId}/${listId}/${cueId}`}
          workspaceId={workspaceId} listId={listId} cueId={cueId} savedNote={savedNote} />
      : <textarea aria-label="Notes" style={inputStyle} disabled />}
  </Field>;
}

function ScopedCueNotes({ workspaceId, listId, cueId, savedNote }: {
  workspaceId: string;
  listId: string;
  cueId: string;
  savedNote: string;
}) {
  const [draft, setDraft] = useState(() => cleanNote(savedNote));
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const inFlight = useRef(false);
  const messageId = useId();
  const conflict = draft.saved !== draft.base;

  useEffect(() => setDraft(current => observeSavedNote(current, savedNote)), [savedNote]);

  async function save(useSavedVersion = false) {
    const request = noteSaveRequest(draft, useSavedVersion);
    if (inFlight.current || !request
        || useWorkspaceStore.getState().workspaceInfo?.instance_id !== workspaceId) return;
    inFlight.current = true;
    setSaving(true);
    setError("");
    try {
      const result = await setCueNotes(workspaceId, listId, cueId, request.expectedNote, request.note);
      setDraft(current => {
        if (result.status === "conflict") return observeSavedNote(current, result.note);
        // A saved note observed during the request can be newer than its reply.
        const saved = current.saved !== draft.saved ? current.saved : result.note;
        return current.text === request.note ? cleanNote(saved) : { ...current, base: saved, saved };
      });
    } catch (cause) {
      setError(String(cause));
    } finally {
      inFlight.current = false;
      setSaving(false);
    }
  }

  return <>
    <textarea
      aria-label="Notes"
      aria-describedby={conflict || error ? messageId : undefined}
      style={{ ...inputStyle, resize: "vertical", minHeight: 56 }}
      value={draft.text}
      readOnly={saving}
      onChange={event => {
        const text = event.currentTarget.value;
        setDraft(current => text === current.saved ? cleanNote(text) : { ...current, text });
      }}
      onBlur={() => void save()}
    />
    {(conflict || error) && <div className="inspector-error" id={messageId}>
      <p role="alert">{error || "The saved note changed. Your draft is still in the field above."}</p>
      {conflict ? <>
        <p>Compare the saved note before saving your draft or reloading the saved text.</p>
        <pre style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", maxHeight: 160, overflowY: "auto" }}>
          {draft.saved || "Empty saved note"}
        </pre>
        <button type="button" disabled={saving} onClick={() => void save(true)}>Save my draft</button>
        <button type="button" disabled={saving} onClick={() => {
          setDraft(cleanNote(draft.saved));
          setError("");
        }}>Reload saved note</button>
      </> : <button type="button" disabled={saving || !noteSaveRequest(draft, false)}
        onClick={() => void save()}>Retry save</button>}
    </div>}
  </>;
}
