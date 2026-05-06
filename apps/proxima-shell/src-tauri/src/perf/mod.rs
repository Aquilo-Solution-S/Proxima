//! Dev-time performance instrumentation. Active only when
//! `PROXIMA_PERF_SESSION_DIR` is set and points at an existing directory.
//! See `docs/superpowers/specs/2026-05-06-dev-perf-instrumentation-design.md`.

pub mod chrome;
pub mod fe;
pub mod ipc;
pub mod session;
