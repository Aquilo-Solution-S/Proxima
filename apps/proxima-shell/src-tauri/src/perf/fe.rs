//! Tauri-side appender for FE perf events. The FE batches events client-side
//! and flushes via `perf_log` / `perf_log_field`. NDJSON appended to the
//! session dir; no-op when the session dir is unset.

use std::fs::OpenOptions;
use std::io::Write;

use serde::Deserialize;
use specta::Type;

use super::session;

#[derive(Deserialize, Type)]
pub struct PerfEntry {
    pub kind: String,
    pub name: String,
    pub dur_ms: f64,
    pub bytes: Option<u64>,
}

#[derive(Deserialize, Type)]
pub struct FieldEntry {
    pub cmd: String,
    pub field_path: String,
}

fn append(file: &str, line: serde_json::Value) {
    let Some(dir) = session::dir() else {
        return;
    };
    let path = dir.join(file);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", line);
    }
}

#[tauri::command]
#[specta::specta]
pub fn perf_log(entries: Vec<PerfEntry>) {
    for e in entries {
        append(
            "frontend.json",
            serde_json::json!({
                "ts_ms": std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0),
                "kind": e.kind,
                "name": e.name,
                "dur_ms": e.dur_ms,
                "bytes": e.bytes,
            }),
        );
    }
}

#[tauri::command]
#[specta::specta]
pub fn perf_log_field(entries: Vec<FieldEntry>) {
    for e in entries {
        append(
            "ipc-fields.json",
            serde_json::json!({
                "cmd": e.cmd,
                "field_path": e.field_path,
            }),
        );
    }
}
