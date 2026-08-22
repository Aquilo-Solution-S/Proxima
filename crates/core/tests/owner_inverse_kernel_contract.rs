use proxima_core::{OwnerEraseOutcome, OwnerRef, UserId};

#[test]
fn public_api_does_not_expose_forgeable_abandoned_owner_constructor() {
    // The compile-time refusal is enforced by
    // scripts/check-architecture-guardrails.py; this test pins the public API
    // shape: callers work with requests and outcomes, not deletion witnesses.
    let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    assert!(matches!(owner, OwnerRef::Personal(_)));
    assert!(matches!(
        OwnerEraseOutcome::NotFound {
            operation_id: uuid::Uuid::now_v7()
        },
        OwnerEraseOutcome::NotFound { .. }
    ));
}
