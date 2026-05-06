//! Phase-2 operators owned by the proxima-code flavor (docs/04
//! §"Phase 2").

pub mod commit_summary;
pub mod development_perspective;

use proxima_core::operators::OperatorRegistry;

pub use commit_summary::CommitSummaryOperator;
pub use development_perspective::CodeDevelopmentPerspectiveOperator;

#[must_use]
pub fn operator_registry() -> OperatorRegistry {
    let mut registry = OperatorRegistry::new();
    registry.register_f2a(CommitSummaryOperator::new());
    registry.register_a2p(CodeDevelopmentPerspectiveOperator::new());
    registry
}

#[must_use]
pub fn f2a_operator_registry() -> OperatorRegistry {
    operator_registry()
}
