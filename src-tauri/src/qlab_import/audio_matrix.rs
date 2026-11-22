//! Convert static QLab input, output and crosspoint faders to cue-level dB routes.
use serde_json::Value;
use std::collections::BTreeMap;

pub struct ImportedMatrix {
    pub rows: Vec<Vec<f64>>,
    pub exceeds_capacity: bool,
}

pub fn read(levels: &Value) -> Option<ImportedMatrix> {
    let entries = levels.get("entries")?;
    let values: Vec<_> = match entries {
        Value::Object(map) => map.values().collect(),
        Value::Array(items) => items.iter().collect(),
        _ => return None,
    };
    // Zero denotes the master row or column. Positive channel numbers start at one.
    let mut gains = BTreeMap::new();
    for item in values {
        let (Some(row), Some(col)) = (item["row"].as_u64(), item["column"].as_u64()) else {
            continue;
        };
        let gain = item["initialLevel"].as_f64().unwrap_or(1.0)
            * item["trimLevel"].as_f64().unwrap_or(1.0);
        gains.insert((row, col), gain);
    }
    let declared =
        levels["rows"].as_u64().unwrap_or(0) > 1 && levels["columns"].as_u64().unwrap_or(0) > 1;
    if !declared && !gains.keys().any(|&(r, c)| r > 0 && c > 0) {
        return None;
    }
    let mut result = ImportedMatrix {
        rows: Vec::new(),
        exceeds_capacity: false,
    };
    for (&(row, col), &gain) in &gains {
        if row == 0 || col == 0 {
            continue;
        }
        let level =
            gain * gains.get(&(row, 0)).unwrap_or(&1.0) * gains.get(&(0, col)).unwrap_or(&1.0);
        if level <= 0.0 {
            continue;
        }
        if row > crate::engine::voice::MATRIX_INPUTS as u64
            || col > crate::engine::voice::MATRIX_OUTPUTS as u64
        {
            result.exceeds_capacity = true;
            continue;
        }
        result
            .rows
            .resize_with(result.rows.len().max(row as usize), Vec::new);
        let output = &mut result.rows[row as usize - 1];
        output.resize(output.len().max(col as usize), -60.0);
        output[col as usize - 1] = (20.0 * level.log10()).max(-60.0);
    }
    // An explicitly silent matrix must not fall back to stereo pan.
    if result.rows.is_empty() {
        result.rows.push(vec![-60.0]);
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn combines_input_output_and_cell_gains_without_applying_master_twice() {
        let levels = json!({"rows":1025,"columns":1025,"entries":[
            {"row":0,"column":0,"initialLevel":0.1}, {"row":5,"column":0,"initialLevel":0.5},
            {"row":0,"column":9,"initialLevel":0.5}, {"row":5,"column":9,"initialLevel":1.0,"trimLevel":0.5},
            {"row":8,"column":129,"initialLevel":0.5}
        ]});
        let matrix = read(&levels).unwrap();
        assert!((matrix.rows[4][8] - 20.0 * 0.125_f64.log10()).abs() < 1e-6);
        assert_eq!(matrix.rows[4][7], -60.0);
        assert!(matrix.exceeds_capacity);
        assert_eq!(
            read(&json!({"rows":1025,"columns":1025,"entries":[]}))
                .unwrap()
                .rows,
            vec![vec![-60.0]]
        );
    }
}
