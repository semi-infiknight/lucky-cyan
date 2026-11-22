interface VisibleRow {
  cue: { id: string; cue_type: string; children?: unknown[] | null };
  parentGroupId: string | null;
}

export function navigateRows(rows: VisibleRow[], current: string, key: string, expanded: Set<string>) {
  const position = rows.findIndex(({ cue }) => cue.id === current);
  if (position < 0) return null;
  const { cue, parentGroupId } = rows[position];
  const isGroup = cue.cue_type === "group" && !!cue.children?.length;
  switch (key) {
    case "Home": return { select: rows[0].cue.id };
    case "End": return { select: rows[rows.length - 1].cue.id };
    case "ArrowUp": return { select: rows[Math.max(0, position - 1)].cue.id };
    case "ArrowDown": return { select: rows[Math.min(rows.length - 1, position + 1)].cue.id };
    case "ArrowLeft":
      if (isGroup && expanded.has(current)) return { toggle: current };
      return parentGroupId ? { select: parentGroupId } : null;
    case "ArrowRight":
      if (!isGroup) return null;
      return expanded.has(current) ? { select: rows[position + 1].cue.id } : { toggle: current };
    default: return null;
  }
}

export function rangeOfRows(rows: VisibleRow[], anchor: string | null, end: string): string[] {
  const to = rows.findIndex(({ cue }) => cue.id === end);
  if (to < 0) return [];
  const from = rows.findIndex(({ cue }) => cue.id === anchor);
  if (from < 0) return [end];
  return rows.slice(Math.min(from, to), Math.max(from, to) + 1).map(({ cue }) => cue.id);
}


interface NestedCue {
  id: string;
  children?: NestedCue[] | null;
}

/** Groups to disclose when navigating to a search result inside a closed group. */
export function cueAncestors(cues: NestedCue[], id: string | null): string[] {
  if (!id) return [];
  const path: string[] = [];
  function find(items: NestedCue[]): boolean {
    for (const cue of items) {
      if (cue.id === id) return true;
      if (!cue.children?.length) continue;
      path.push(cue.id);
      if (find(cue.children)) return true;
      path.pop();
    }
    return false;
  }
  return find(cues) ? path : [];
}

/** Open the branches containing one or more Find matches. */
export function findCueBranches(cues: NestedCue[], ids: string[]): Set<string> {
  const targets = new Set(ids);
  const branches = new Set<string>();
  function scan(items: NestedCue[]): boolean {
    let matched = false;
    for (const cue of items) {
      const childMatch = cue.children?.length ? scan(cue.children) : false;
      if (childMatch) branches.add(cue.id);
      if (childMatch || targets.has(cue.id)) matched = true;
    }
    return matched;
  }
  scan(cues);
  return branches;
}
