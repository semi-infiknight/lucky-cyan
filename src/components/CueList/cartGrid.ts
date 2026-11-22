import type { CartLayout, CueSummary } from "../../lib/types";

export function cartGrid(cues: CueSummary[], layout: CartLayout) {
  const cells = Array<CueSummary | null>(layout.rows * layout.columns).fill(null);
  const outside: CueSummary[] = [];
  for (const cue of cues) {
    const cell = layout.pads[cue.id];
    if (cell && cell.row >= 1 && cell.column >= 1 && cell.row <= layout.rows && cell.column <= layout.columns) {
      cells[(cell.row - 1) * layout.columns + cell.column - 1] = cue;
    } else {
      outside.push(cue);
    }
  }
  return { cells, outside };
}

export function cellAt(index: number, columns: number) {
  return { row: Math.floor(index / columns) + 1, column: index % columns + 1 };
}

/** Arrow keys follow a row or column across holes. Navigation never activates a pad. */
export function focusGridCell(event: React.KeyboardEvent<HTMLElement>, columns: number) {
  const step = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -columns, ArrowDown: columns, Home: 1, End: -1 }[event.key];
  if (!step || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
  const cell = (event.target as HTMLElement).closest<HTMLElement>("[data-cart-index]");
  if (!cell) return;
  const all = event.currentTarget.querySelectorAll<HTMLElement>("[data-cart-index]");
  const index = Number(cell.dataset.cartIndex);
  let next = event.key === "Home" ? 0 : event.key === "End" ? all.length - 1 : index + step;
  while (next >= 0 && next < all.length) {
    if ((event.key === "ArrowLeft" || event.key === "ArrowRight") &&
        Math.floor(next / columns) !== Math.floor(index / columns)) break;
    const button = all[next].querySelector<HTMLButtonElement>("button:not(:disabled)");
    if (button) { button.focus(); break; }
    next += step;
  }
  event.preventDefault();
  event.stopPropagation();
}

export const gridColumns = (columns: number) => `repeat(${columns}, minmax(96px, 1fr))`;
