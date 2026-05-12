//! Wake harness seam.
//!
//! v1 replaces the subprocess target adapter with
//! `proxima_core::harness::HarnessAdapter`. This module keeps the old
//! module path as a short-lived alias for existing call sites.

pub use crate::harness::{
    HarnessAdapter as TargetAdapter, HarnessContext as TargetContext,
    HarnessError as TargetAdapterError, HarnessOutcome as TargetOutcome,
    HarnessOutcomeKind as TargetOutcomeKind, HarnessProgram as TargetInvocation,
};
