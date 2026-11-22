import { useEffect, useMemo, useState, type RefObject } from "react";
import { cueAccent } from "../../lib/cueColors";
import { flattenCues } from "../../lib/cueTree";
import { findCues } from "../../lib/commands";
import type { CueSummary } from "../../lib/types";
import "./cueFindBar.css";

export function adjacentFindMatch(ids: string[], selected: string | null, delta: -1 | 1) {
  const index = selected ? ids.indexOf(selected) : -1;
  if (index < 0) return delta === 1 ? ids[0] : ids[ids.length - 1];
  return ids[(index + delta + ids.length) % ids.length];
}

export function restoreFindFocus(desk: HTMLElement | null, selected: string | null) {
  if (!desk) return;
  const cart = desk.querySelector<HTMLElement>("[data-cue-cart-focus]");
  const selectedRow = selected && !cart ? desk.querySelector<HTMLElement>(
    `[role="treeitem"][data-cue-id="${CSS.escape(selected)}"]`,
  ) : null;
  const target = cart ?? selectedRow ?? desk.querySelector<HTMLElement>('[role="treeitem"]')
    ?? desk.querySelector<HTMLElement>('[role="tree"]');
  target?.focus({ preventScroll: true });
}

export function CueFindBar({
  inputRef, workspaceId, listId, revision, query, cues, selected,
  onQueryChange, onSelect, onClose,
}: {
  inputRef: RefObject<HTMLInputElement>;
  workspaceId: string;
  listId: string;
  revision: number;
  query: string;
  cues: CueSummary[];
  selected: string | null;
  onQueryChange: (query: string) => void;
  onSelect: (ids: string[]) => void;
  onClose: () => void;
}) {
  const search = query.trim();
  const key = JSON.stringify([workspaceId, listId, revision, search]);
  const [response, setResponse] = useState<{ key: string; ids: string[]; error?: string }>();
  useEffect(() => {
    if (!search) return;
    let active = true;
    const timer = window.setTimeout(() => {
      void findCues(workspaceId, listId, search).then(ids => {
        if (active) setResponse({ key, ids });
      }).catch(error => {
        if (active) setResponse({ key, ids: [], error: String(error) });
      });
    }, 120);
    return () => { active = false; window.clearTimeout(timer); };
  }, [key, listId, search, workspaceId]);

  const content = response?.key === key ? response : undefined;
  const byId = useMemo(() => new Map(flattenCues(cues).map(cue => [cue.id, cue])), [cues]);
  const ids = content?.ids.filter(id => byId.has(id)) ?? [];
  const position = selected ? ids.indexOf(selected) : -1;
  const chooseAdjacent = (delta: -1 | 1) => {
    const id = adjacentFindMatch(ids, selected, delta);
    if (id) onSelect([id]);
  };

  return <section className="cue-find" aria-label="Find in cue list"
    onKeyDown={event => {
      if (event.key.toLowerCase() === "f" && (event.ctrlKey || event.metaKey)) return;
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      }
      event.stopPropagation();
    }}>
    {ids.length > 0 && <ul className="cue-find-results" aria-label="Found cues">
      {ids.map(id => {
        const cue = byId.get(id)!; // Search results are intersected with the displayed cue tree.
        return <li key={id}><button type="button" aria-current={id === selected ? "true" : undefined}
          onClick={() => onSelect([id])}>
          <span className="cue-find-accent" aria-hidden="true" style={{ background: cueAccent(cue) }} />
          <span className="cue-find-number">{cue.number}</span>
          <span className="cue-find-name">{cue.name || "Untitled cue"}</span>
          <span className="cue-find-kind">{cue.cue_type}</span>
        </button></li>;
      })}
    </ul>}
    <div className="cue-find-controls">
      <input
        ref={inputRef} type="search" value={query}
        aria-label="Find cues" placeholder="Find cues…"
        onChange={event => onQueryChange(event.currentTarget.value)}
        onKeyDown={event => {
          if (event.key === "Enter") {
            event.preventDefault();
            if (!event.repeat) chooseAdjacent(event.shiftKey ? -1 : 1);
          }
        }}
      />
      <span className="cue-find-count" role={content?.error ? "alert" : "status"}>
        {search && (content?.error ?? (!content ? "Finding…" : position >= 0
          ? `${position + 1} of ${ids.length}`
          : `${ids.length} ${ids.length === 1 ? "match" : "matches"}`))}
      </span>
      <button type="button" aria-label="Previous match" title="Previous match (Shift+Enter)"
        disabled={!ids.length} onClick={() => chooseAdjacent(-1)}>←</button>
      <button type="button" aria-label="Next match" title="Next match (Enter)"
        disabled={!ids.length} onClick={() => chooseAdjacent(1)}>→</button>
      <button type="button" disabled={!ids.length} onClick={() => onSelect(ids)}>Select Found</button>
      <button type="button" onClick={onClose}>Done</button>
    </div>
  </section>;
}
