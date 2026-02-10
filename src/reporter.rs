use crate::types::{CopytradeEvent, ExitSummary};

/// Emit a copytrade event as a single JSON line to stdout.
pub fn report_event(event: &CopytradeEvent) {
    if let Ok(json) = serde_json::to_string(event) {
        println!("{json}");
    }
}

/// Emit the exit summary as pretty-printed JSON to stdout.
pub fn report_exit_summary(summary: &ExitSummary) {
    if let Ok(json) = serde_json::to_string_pretty(summary) {
        println!("{json}");
    }
}
