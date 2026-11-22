//! An offline programming format. Only explicit SOUND marks create cues.
//! Example: [SOUND | 1.5 | Rain in alley | atmos/rain.wav | -18]
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CueSheet {
    pub title: String,
    pub cues: Vec<AudioMark>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AudioMark {
    pub number: String,
    pub name: String,
    pub media: String,
    pub level_db: f64,
}

impl CueSheet {
    pub fn from_script(title: &str, script: &str) -> Result<Self> {
        ensure!(script.len() <= 2_000_000, "script exceeds 2 MB");
        let mut cues = Vec::new();
        for (index, line) in script.lines().enumerate() {
            let line = line.trim();
            let Some(mark) = line.strip_prefix("[SOUND") else {
                continue;
            };
            if mark.starts_with(|c: char| c.is_alphanumeric()) {
                continue;
            }
            let position = index + 1;
            let fields = mark
                .trim_start()
                .strip_prefix('|')
                .and_then(|s| s.strip_suffix(']'))
                .with_context(|| {
                    format!("line {position}: expected [SOUND | number | name | media | dB]")
                })?;
            let parts: Vec<_> = fields.split('|').map(str::trim).collect();
            let [number, name, media, level] = parts.as_slice() else {
                bail!("line {position}: expected number, name, media and dB")
            };
            cues.push(AudioMark {
                number: number.to_string(),
                name: name.to_string(),
                media: media.to_string(),
                level_db: level
                    .parse()
                    .with_context(|| format!("line {position}: invalid level"))?,
            });
        }
        let result = Self {
            title: title.trim().to_string(),
            cues,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.title.trim().is_empty() && self.title.len() <= 180,
            "enter a show title of 1–180 bytes"
        );
        ensure!(
            (1..=2000).contains(&self.cues.len()),
            "a sheet needs 1–2000 SOUND marks"
        );
        let mut numbers = HashSet::new();
        for mark in &self.cues {
            let number = &mark.number;
            ensure!(
                !number.trim().is_empty() && number.len() <= 64 && numbers.insert(number),
                "blank, long or duplicate cue number '{number}'"
            );
            ensure!(
                !mark.name.trim().is_empty() && mark.name.len() <= 300,
                "cue {number}: enter a name of 1–300 bytes"
            );
            ensure!(
                mark.level_db.is_finite() && (-60.0..=0.0).contains(&mark.level_db),
                "cue {number}: use a level from -60 to 0 dB"
            );
            ensure!(
                !mark.media.contains([':', '\\'])
                    && !mark.media.chars().any(char::is_control)
                    && mark.media.len() <= 1024
                    && mark
                        .media
                        .split('/')
                        .all(|part| !matches!(part, "" | "." | "..")),
                "cue {number}: use a relative media path with forward slashes"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_marks_create_cues_and_numbers_stay_strings() {
        let sheet = CueSheet::from_script(
            "Act I",
            "A door closes.\n[SOUND | 01a | Rain | fx/rain.wav | -12]",
        )
        .unwrap();
        assert_eq!(sheet.cues.len(), 1);
        assert_eq!(sheet.cues[0].number, "01a");
    }
    #[test]
    fn invalid_marks_reject_the_entire_sheet() {
        for second in [
            "[SOUND | 1 | duplicate | b.wav | -2]",
            "[SOUND | 2 | escape | ../b.wav | -2]",
            "[SOUND | 2 | invalid | a.wav | NaN]",
            "[SOUND | malformed]",
        ] {
            assert!(CueSheet::from_script(
                "Act I",
                &format!("[SOUND | 1 | Valid | a.wav | -12]\n{second}")
            )
            .is_err());
        }
    }
}
