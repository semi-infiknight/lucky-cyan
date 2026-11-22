import { useEffect, useState } from "react";
import type { CartEdit, CartLayout } from "../../lib/types";

export function CartLayoutControls({ layout, onEdit, busy }: {
  layout?: CartLayout | null;
  onEdit: (edit: CartEdit) => void;
  busy: boolean;
}) {
  const [rows, setRows] = useState(String(layout?.rows ?? 4));
  const [columns, setColumns] = useState(String(layout?.columns ?? 4));
  useEffect(() => {
    setRows(String(layout?.rows ?? 4));
    setColumns(String(layout?.columns ?? 4));
  }, [layout?.rows, layout?.columns]);
  return <form className="cart-layout-controls" aria-label="Cart layout" onSubmit={event => {
    event.preventDefault();
    onEdit({ action: "resize", rows: Number(rows), columns: Number(columns) });
  }}>
    <label>Rows <input type="number" min={1} max={64} required value={rows}
      onChange={e => setRows(e.target.value)} /></label>
    <label>Columns <input type="number" min={1} max={64} required value={columns}
      onChange={e => setColumns(e.target.value)} /></label>
    <button disabled={busy} type="submit">{layout ? "Resize grid" : "Use fixed grid"}</button>
    {layout && <button type="button" disabled={busy} onClick={() => onEdit({ action: "compact" })}>Use compact layout</button>}
  </form>;
}
