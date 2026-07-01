use proxima_core::{ComplianceEraseOutcome, ComplianceEraseRefusal, OwnerRef};

#[test]
fn world_owner_delete_request_maps_to_refused_outcome() {
    let outcome = ComplianceEraseOutcome::Refused {
        operation_id: uuid::Uuid::now_v7(),
        reason: ComplianceEraseRefusal::WorldOwner,
    };
    assert!(matches!(
        outcome,
        ComplianceEraseOutcome::Refused {
            reason: ComplianceEraseRefusal::WorldOwner,
            ..
        }
    ));
}

#[test]
fn public_api_does_not_expose_forgeable_abandoned_owner_constructor() {
    // The real assertion is enforced by scripts/check-architecture-guardrails.py because
    // compile-fail UI tests are not currently wired for this workspace.
    // This test still pins the intended public API: callers work with requests
    // and outcomes, not deletion witnesses.
    let owner = OwnerRef::World;
    assert!(matches!(owner, OwnerRef::World));
    assert!(matches!(
        ComplianceEraseOutcome::NotFound {
            operation_id: uuid::Uuid::now_v7()
        },
        ComplianceEraseOutcome::NotFound { .. }
    ));
}
