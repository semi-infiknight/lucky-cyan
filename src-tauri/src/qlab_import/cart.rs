use crate::show::cart_layout::{CartCell, CartLayout};
use std::collections::HashSet;
use uuid::Uuid;

/// QLab archives use "row x column" (without spaces), one-based, mapped to
/// references to the same archived cue objects as the list's children.
pub(super) fn import_layout(
    list: &serde_json::Value,
    cues: &[serde_json::Value],
) -> Option<CartLayout> {
    let rows = u16::try_from(list["cartRows"].as_u64()?).ok()?;
    let columns = u16::try_from(list["cartColumns"].as_u64()?).ok()?;
    let mut layout = CartLayout::new(rows, columns).ok()?;
    let cells = list["cart"].as_object()?;
    for cue in cues {
        if let Some(id) = cue["id"].as_str().and_then(|id| Uuid::parse_str(id).ok()) {
            layout.pads.insert(id, None);
        }
    }
    let mut taken = HashSet::new();
    for (key, value) in cells {
        let Some((row, column)) = key.split_once('x') else {
            continue;
        };
        let (Ok(row), Ok(column)) = (row.parse(), column.parse()) else {
            continue;
        };
        if CartLayout::validate_size(row, column).is_err() {
            continue;
        }
        let Some(id) = value["uniqueID"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            continue;
        };
        if let Some(pos) = layout.pads.get_mut(&id) {
            let cell = CartCell { row, column };
            if pos.is_none() && taken.insert(cell) {
                *pos = Some(cell);
            }
        }
    }
    layout.validate().ok()?;
    Some(layout)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qlab_cart_preserves_one_based_coordinates_empty_and_unplaced_pads() {
        use serde_json::json;
        let right = Uuid::new_v4();
        let up = Uuid::new_v4();
        let unknown = Uuid::new_v4();
        let source = json!({"cartRows": 8, "cartColumns": 8, "cart": {
            "7x8": {"uniqueID": right.to_string().to_uppercase()},
            "6x7": {"uniqueID": up}, "broken": {"uniqueID": unknown}
        }});
        let grid = import_layout(
            &source,
            &[json!({"id":up}), json!({"id":right}), json!({"id":unknown})],
        )
        .unwrap();
        assert_eq!(grid.pads[&right], Some(CartCell { row: 7, column: 8 }));
        assert_eq!(grid.pads[&up], Some(CartCell { row: 6, column: 7 }));
        assert_eq!(grid.pads[&unknown], None);
    }
}
