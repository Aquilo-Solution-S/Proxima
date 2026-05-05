//! Phase-2 operators owned by the proxima-code flavor (docs/04
//! §"Phase 2"). M5 ships one F→A — see [`commit_summary`].

pub mod commit_summary;

use proxima_core::operators::OperatorRegistry;

pub use commit_summary::CommitSummaryOperator;

#[must_use]
pub fn f2a_operator_registry() -> OperatorRegistry {
    let mut registry = OperatorRegistry::new();
    registry.register_f2a(CommitSummaryOperator::new());
    registry
}
