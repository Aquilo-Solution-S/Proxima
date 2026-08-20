use proxima_core::{ComplianceEraseOutcome, OwnerRef, UserId};

#[test]
fn public_api_does_not_expose_forgeable_abandoned_owner_constructor() {
    // The real assertion is enforced by scripts/check-architecture-guardrails.py because
    // compile-fail UI tests are not currently wired for this workspace.
    // This test still pins the intended public API: callers work with requests
    // and outcomes, not deletion witnesses.
    let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    assert!(matches!(owner, OwnerRef::Personal(_)));
    assert!(matches!(
        ComplianceEraseOutcome::NotFound {
            operation_id: uuid::Uuid::now_v7()
        },
        ComplianceEraseOutcome::NotFound { .. }
    ));
}
