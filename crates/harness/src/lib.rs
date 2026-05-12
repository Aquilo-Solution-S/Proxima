//! Proxima Harness — in-process LLM loop driver.
//!
//! Implements `proxima_core::harness::HarnessAdapter` via
//! [`HarnessLoop`]. See
//! `docs/superpowers/specs/2026-05-12-proxima-harness-design.md`.

#![forbid(unsafe_code)]

pub mod conversation;
pub mod loop_driver;
pub mod program;
pub mod providers;
pub mod tools;
pub mod trace;

pub use loop_driver::HarnessLoop;
pub use proxima_core::harness::{
    HarnessAdapter, HarnessContext, HarnessError, HarnessOutcome, HarnessOutcomeKind,
    HarnessProgram, ProviderTarget, SubstrateToolBinding,
};
