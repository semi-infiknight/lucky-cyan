//! Physical cart cells stay attached to cue IDs, independently of list order.

use std::collections::{BTreeMap, HashSet};

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_AXIS: u16 = 64;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct CartCell {
    pub row: u16,
    pub column: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CartLayout {
    pub rows: u16,
    pub columns: u16,
    /// A null position is a real cue awaiting placement in a full cart.
    pub pads: BTreeMap<Uuid, Option<CartCell>>,
}

impl CartLayout {
    pub fn new(rows: u16, columns: u16) -> Result<Self> {
        Self::validate_size(rows, columns)?;
        Ok(Self {
            rows,
            columns,
            pads: BTreeMap::new(),
        })
    }

    pub(crate) fn validate_size(rows: u16, columns: u16) -> Result<()> {
        ensure!(
            (1..=MAX_AXIS).contains(&rows) && (1..=MAX_AXIS).contains(&columns),
            "Cart dimensions must be between 1 and {MAX_AXIS}"
        );
        Ok(())
    }

    pub fn resize(&mut self, rows: u16, columns: u16) -> Result<()> {
        Self::validate_size(rows, columns)?;
        self.rows = rows;
        self.columns = columns;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        Self::validate_size(self.rows, self.columns)?;
        let mut taken = HashSet::new();
        for cell in self.pads.values().flatten() {
            Self::validate_size(cell.row, cell.column)?;
            ensure!(
                taken.insert(*cell),
                "More than one pad occupies the same cell"
            );
        }
        Ok(())
    }

    /// Retain existing locations (including cells outside a resized grid).
    /// New cues take the first vacant visible cell in row-major order.
    pub fn sync(&mut self, cue_ids: impl Iterator<Item = Uuid> + Clone) {
        let live: HashSet<Uuid> = cue_ids.clone().collect();
        self.pads.retain(|id, _| live.contains(id));
        let taken: HashSet<CartCell> = self.pads.values().flatten().copied().collect();
        let columns = self.columns;
        let mut free = (1..=self.rows)
            .flat_map(|row| (1..=columns).map(move |column| CartCell { row, column }))
            .filter(|cell| !taken.contains(cell));
        for id in cue_ids {
            self.pads.entry(id).or_insert_with(|| free.next());
        }
    }

    /// Placing on another pad exchanges their positions, even when the source
    /// was outside the grid or awaiting placement.
    pub fn place(&mut self, cue_id: Uuid, cell: CartCell) -> Result<()> {
        ensure!(
            (1..=self.rows).contains(&cell.row) && (1..=self.columns).contains(&cell.column),
            "Destination is outside the visible cart"
        );
        let source = *self
            .pads
            .get(&cue_id)
            .ok_or_else(|| anyhow::anyhow!("Cue is no longer in this cart"))?;
        if source == Some(cell) {
            return Ok(());
        }
        let occupant = self
            .pads
            .iter()
            .find_map(|(id, pos)| (*pos == Some(cell)).then_some(*id));
        if let Some(id) = occupant {
            self.pads.insert(id, source);
        }
        self.pads.insert(cue_id, Some(cell));
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CartEdit {
    Resize { rows: u16, columns: u16 },
    Place { cue_id: Uuid, cell: CartCell },
    Compact,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cells_survive_reorder_shrink_restore_add_and_remove() {
        let [a, b, c] = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let mut grid = CartLayout::new(2, 3).unwrap();
        grid.sync([a, b].into_iter());
        let corner = CartCell { row: 2, column: 3 };
        grid.place(a, corner).unwrap();
        grid.resize(1, 2).unwrap();
        grid.sync([b, a, c].into_iter());
        assert_eq!(grid.pads[&a], Some(corner));
        assert_eq!(grid.pads[&c], Some(CartCell { row: 1, column: 1 }));
        grid.resize(2, 3).unwrap();
        grid.place(c, corner).unwrap();
        assert_eq!(grid.pads[&a], Some(CartCell { row: 1, column: 1 }));
        grid.sync([a, c].into_iter());
        assert!(!grid.pads.contains_key(&b));
        let saved = serde_json::to_string(&grid).unwrap();
        assert_eq!(serde_json::from_str::<CartLayout>(&saved).unwrap(), grid);
    }
    #[test]
    fn full_grids_keep_unplaced_cues_and_reject_invalid_edits_atomically() {
        let [a, b] = [Uuid::new_v4(), Uuid::new_v4()];
        let mut grid = CartLayout::new(1, 1).unwrap();
        grid.sync([a, b].into_iter());
        assert_eq!(grid.pads[&b], None);
        let cell = CartCell { row: 1, column: 1 };
        grid.place(b, cell).unwrap();
        assert_eq!(grid.pads[&a], None);
        let before = grid.clone();
        assert!(grid.resize(0, 1).is_err());
        assert!(grid.place(b, CartCell { row: 2, column: 1 }).is_err());
        assert!(grid.place(Uuid::new_v4(), cell).is_err());
        assert_eq!(grid, before);
    }
}
