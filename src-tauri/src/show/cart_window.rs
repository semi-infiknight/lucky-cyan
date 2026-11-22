//! An auxiliary cart stays bound to one open document and one independent cart.
use anyhow::{anyhow, ensure, Result};
use uuid::Uuid;

use super::{cue_list::CueList, workspace::Workspace};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct CartWindowTarget {
    instance_id: Uuid,
    list_id: Uuid,
}

impl CartWindowTarget {
    pub fn bind(workspace: &Workspace, list_id: Uuid) -> Result<Self> {
        let target = Self {
            instance_id: workspace.instance_id,
            list_id,
        };
        target.list(workspace)?;
        Ok(target)
    }

    /// Simple UUIDs avoid ambiguity with the separator and keep labels valid for Tauri.
    pub fn label(self) -> String {
        format!(
            "cart-{}-{}",
            self.instance_id.simple(),
            self.list_id.simple()
        )
    }

    pub fn from_label(label: &str) -> Result<Self> {
        let (inst, list) = label
            .strip_prefix("cart-")
            .and_then(|s| s.split_once('-'))
            .ok_or_else(|| anyhow!("This command requires a cart window"))?;
        let target = Self {
            instance_id: Uuid::parse_str(inst)?,
            list_id: Uuid::parse_str(list)?,
        };
        ensure!(target.label() == label, "Invalid cart window label");
        Ok(target)
    }

    pub fn list(self, ws: &Workspace) -> Result<&CueList> {
        ensure!(
            ws.instance_id == self.instance_id,
            "The original show has closed"
        );
        let list = ws
            .cue_list_by_id(self.list_id)
            .ok_or_else(|| anyhow!("This cart was removed"))?;
        ensure!(
            list.independent_pads(),
            "This window requires independent pads"
        );
        Ok(list)
    }

    pub fn pad_list(self, ws: &mut Workspace, cue_id: Uuid, fire: bool) -> Result<&mut CueList> {
        let list = self.list(ws)?;
        let cue = list
            .get(&cue_id)
            .ok_or_else(|| anyhow!("This pad is no longer in the cart"))?;
        if fire {
            ensure!(!cue.is_disabled(), "This pad is disarmed");
            if let Some(grid) = &list.cart_layout {
                ensure!(
                    grid.pads
                        .get(&cue_id)
                        .copied()
                        .flatten()
                        .is_some_and(|cell| cell.row <= grid.rows && cell.column <= grid.columns),
                    "This pad is outside the visible grid"
                );
            }
        }
        ws.cue_list_by_id_mut(self.list_id)
            .ok_or_else(|| anyhow!("This cart was removed"))
    }
}
