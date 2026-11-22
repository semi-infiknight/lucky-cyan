use crate::live::{DeskResponseError, LiveDesk};
use anyhow::Result;
use inkue_control::{LiveCue, LiveRunState, LiveStatus};
use std::{
    fmt::Write as _,
    io::{self, IsTerminal, Write as _},
    path::Path,
    thread,
    time::Duration,
};

const RETRY_LIMIT: u8 = 3;

#[derive(Debug, PartialEq, Eq)]
enum WatchFailure {
    Retry,
    Closed,
    Unavailable,
}

fn watch_failure(status: Option<u16>, consecutive_failures: &mut u8) -> WatchFailure {
    match status {
        Some(401 | 403 | 410) => WatchFailure::Closed,
        Some(409) => {
            // A busy workspace confirms that the listener is still serving.
            *consecutive_failures = 0;
            WatchFailure::Retry
        }
        _ => {
            *consecutive_failures += 1;
            if *consecutive_failures >= RETRY_LIMIT {
                WatchFailure::Unavailable
            } else {
                WatchFailure::Retry
            }
        }
    }
}

pub fn watch(desk: &LiveDesk, session_path: &Path) -> Result<()> {
    let mut terminal = io::stdout().lock();
    anyhow::ensure!(
        terminal.is_terminal(),
        "watch requires a terminal. Use status for JSON."
    );
    let mut failures = 0;
    while session_path.is_file() {
        let text = match desk.status() {
            Ok(status) => {
                failures = 0;
                render_status(&status)
            }
            Err(error) => {
                let status = error
                    .downcast_ref::<DeskResponseError>()
                    .map(|reply| reply.status);
                match watch_failure(status, &mut failures) {
                    WatchFailure::Retry => "Live desk status unavailable. Retrying…\n".into(),
                    WatchFailure::Closed => break,
                    WatchFailure::Unavailable => {
                        writeln!(
                            terminal,
                            "\x1b[2J\x1b[HLocal desk unavailable. Start a new status session."
                        )?;
                        anyhow::bail!("could not refresh the live desk status");
                    }
                }
            }
        };
        write!(terminal, "\x1b[2J\x1b[H{text}")?;
        terminal.flush()?;
        thread::sleep(Duration::from_millis(500));
    }
    writeln!(terminal, "\x1b[2J\x1b[HLocal status session closed.")?;
    Ok(())
}

fn terminal_text(value: &str) -> String {
    value.chars().filter(|character| {
        !character.is_control() && !matches!(character,
            '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    }).take(120).collect()
}

fn cue_label(cue: &LiveCue) -> String {
    let number = cue
        .number
        .as_deref()
        .map(terminal_text)
        .filter(|number| !number.is_empty());
    let name = terminal_text(&cue.name);
    let label = if name.trim().is_empty() {
        terminal_text(&cue.kind)
    } else {
        name
    };
    match number {
        Some(number) => format!("{number}  {label}"),
        None => label,
    }
}

fn clock(milliseconds: u64) -> String {
    let tenths = milliseconds / 100;
    format!("{}:{:02}.{}", tenths / 600, tenths / 10 % 60, tenths % 10)
}

fn render_status(status: &LiveStatus) -> String {
    let mut text = format!("{}\n", terminal_text(&status.name));
    for list in &status.lists {
        let focus = if list.id == status.active_cue_list_id {
            " · Open on desk"
        } else {
            ""
        };
        let _ = writeln!(text, "\n{}{focus}", terminal_text(&list.name));
        let standby = list.standby.as_ref().map_or_else(
            || "—".into(),
            |cue| {
                let suffix = if cue.is_disabled { " · Disarmed" } else { "" };
                format!("{}{suffix}", cue_label(cue))
            },
        );
        let _ = writeln!(text, "  Standby  {standby}");
        for run in &list.active {
            let state = match run.state {
                LiveRunState::Running => "Running",
                LiveRunState::Paused => "Paused ",
            };
            let indent = if run.parent_id.is_some() {
                "    "
            } else {
                "  "
            };
            let _ = write!(
                text,
                "{indent}{state}  {}  ·  {} elapsed",
                cue_label(&run.cue),
                clock(run.elapsed_ms)
            );
            if let Some(remaining) = run.remaining_ms {
                let _ = write!(text, "  ·  {} left", clock(remaining));
            }
            let _ = writeln!(text);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn revoked_sessions_end_and_connection_failures_are_bounded() {
        let mut failures = 0;
        for status in [401, 403, 410] {
            assert_eq!(
                watch_failure(Some(status), &mut failures),
                WatchFailure::Closed
            );
        }
        assert_eq!(watch_failure(None, &mut failures), WatchFailure::Retry);
        assert_eq!(watch_failure(Some(409), &mut failures), WatchFailure::Retry);
        assert_eq!(failures, 0);
        for _ in 1..RETRY_LIMIT {
            assert_eq!(watch_failure(None, &mut failures), WatchFailure::Retry);
        }
        assert_eq!(
            watch_failure(None, &mut failures),
            WatchFailure::Unavailable
        );
    }

    #[test]
    fn monitor_labels_standby_and_paused_cues_without_terminal_controls() {
        let id = "49300109-925e-4cc6-9946-6185a3a46fb8";
        let cue_id = "86ff54c5-80a2-414f-8227-4430bd0e663b";
        let value = serde_json::from_value(json!({
            "workspace_id": id, "name": "Run\nDesk", "active_cue_list_id": id, "lists": [
                {"id": id, "name": "Main\u{202e}",
                    "standby": {"id": cue_id, "number": "3", "name": "Wait\u{001b}[2J", "kind": "wait", "is_disabled": true},
                    "active": [{"cue": {"id": cue_id, "number": "2", "name": "Crossing", "kind": "wait", "is_disabled": false},
                        "parent_id": null, "state": "paused", "elapsed_ms": 1900, "remaining_ms": 3200}]
                }
            ]
        })).unwrap();
        let text = render_status(&value);
        assert!(text.contains("RunDesk"));
        assert!(text.contains("Main · Open on desk"));
        assert!(text.contains("Standby  3  Wait[2J · Disarmed"));
        assert!(text.contains("Paused   2  Crossing  ·  0:01.9 elapsed  ·  0:03.2 left"));
        assert!(!text.contains(['\u{001b}', '\u{202e}']));
    }
}
