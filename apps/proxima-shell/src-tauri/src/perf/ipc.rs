//! IPC call recorder. Each instrumented Tauri command wraps its body in
//! `perf::ipc::record(name, req_bytes, async move { ... })`. When the
//! session dir is unset, this is a no-op pass-through.
//!
//! Output: NDJSON appended to `<session>/ipc.json`.
//!
//! `req_bytes` is computed eagerly (synchronously) by the caller via
//! `req_size(&args)` so the future can take ownership of the args.

use std::fs::OpenOptions;
use std::io::Write;
use std::time::Instant;

use serde::Serialize;
use serde_json::json;

use super::session;

pub fn req_size<A: Serialize>(args: &A) -> usize {
    if session::dir().is_none() {
        return 0;
    }
    serde_json::to_vec(args).map_or(0, |v| v.len())
}

fn millis_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

pub async fn record<R, F>(cmd: &'static str, req_bytes: usize, fut: F) -> R
where
    R: Serialize,
    F: std::future::Future<Output = R>,
{
    let Some(dir) = session::dir() else {
        return fut.await;
    };

    let started = Instant::now();
    let result = fut.await;
    let dur_ms = millis_u64(started.elapsed().as_millis());
    let resp_bytes = serde_json::to_vec(&result).map_or(0, |v| v.len());

    let line = json!({
        "ts_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| millis_u64(d.as_millis())),
        "cmd": cmd,
        "req_bytes": req_bytes,
        "resp_bytes": resp_bytes,
        "dur_ms": dur_ms,
        "ok": true,
    });

    let path = dir.join("ipc.json");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
    result
}
