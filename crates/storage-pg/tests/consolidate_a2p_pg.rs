//! Regression for A→P invocation owner scoping.
//!
//! `load_a2p_abstractions` reads abstractions by principal only — it
//! does not filter by `owner_org_id`. The idempotency / lineage queries
//! must use the same scope, otherwise the same principal under a
//! different `org_id` re-runs the operator and writes duplicate
//! Perspectives without lineage.

mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::operators::{A2PInvocationKey, A2PLineageKey};
use proxima_core::storage::Storage;
use proxima_core::{MemoryId, OrgId, Owner};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread")]
async fn a2p_invocation_lookups_are_principal_scoped_not_org_scoped() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };
    pg.run_migrations().await.expect("run migrations");

    let owner_a = owner_fixture();
    let owner_b = Owner {
        principal: owner_a.principal.clone(),
        org_id: OrgId::new(Uuid::now_v7()),
    };
    assert_ne!(
        owner_a.org_id.into_inner(),
        owner_b.org_id.into_inner(),
        "fixture invariant: orgs differ",
    );

    let head_memory_id = Uuid::now_v7();
    let head_owner_org_id = owner_a.org_id.into_inner();
    let principal_id = match &owner_a.principal {
        proxima_core::Principal::User(u) => u.into_inner(),
        proxima_core::Principal::Group(g) => g.into_inner(),
    };
    let context_hash = [1u8; 32];
    let input_hash = [2u8; 32];

    sqlx::query(
        "INSERT INTO proxima_core.memories \
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id, \
             schema_id, schema_version, kind, text, operator_kind, model_id, \
             prompt_version, personality_id) \
         VALUES ($1, 'User', $2, $3, 'test/persp', 1, 'Perspective', '', 'AtoP', \
                 'test-model', 'v1', 'default')",
    )
    .bind(head_memory_id)
    .bind(principal_id)
    .bind(head_owner_org_id)
    .execute(pg.pool())
    .await
    .expect("insert seed memory");

    sqlx::query(
        "INSERT INTO proxima_core.a2p_invocations \
            (owner_principal_kind, owner_principal_id, \
             operator_id, prompt_version, model_id, personality_id, \
             context_hash, input_hash, head_memory_id) \
         VALUES ('User', $1, 'test/op', 'v1', 'test-model', 'default', $2, $3, $4)",
    )
    .bind(principal_id)
    .bind(&context_hash[..])
    .bind(&input_hash[..])
    .bind(head_memory_id)
    .execute(pg.pool())
    .await
    .expect("insert seed a2p_invocation");

    let inv_key = A2PInvocationKey {
        operator_id: "test/op",
        prompt_version: "v1",
        model_id: "test-model",
        personality_id: "default",
        context_hash: &context_hash,
        input_hash: &input_hash,
    };
    let lineage_key = A2PLineageKey {
        operator_id: inv_key.operator_id,
        prompt_version: inv_key.prompt_version,
        model_id: inv_key.model_id,
        personality_id: inv_key.personality_id,
    };

    // Same org as the seed: row visible.
    assert!(
        pg.has_a2p_invocation(&owner_a, &inv_key).await.unwrap(),
        "same-org has_a2p_invocation must return true",
    );
    assert_eq!(
        pg.lookup_prior_a2p_head(&owner_a, &lineage_key)
            .await
            .unwrap()
            .map(MemoryId::into_inner),
        Some(head_memory_id),
        "same-org lookup_prior_a2p_head must return seed head",
    );

    // Different org, same principal: row must still be visible. This is
    // the regression — pre-fix, owner_org_id was part of both predicates
    // and these calls returned (false, None), causing consolidate_a2p to
    // write a duplicate Perspective without prior-head lineage.
    assert!(
        pg.has_a2p_invocation(&owner_b, &inv_key).await.unwrap(),
        "cross-org has_a2p_invocation must return true (principal-only scope)",
    );
    assert_eq!(
        pg.lookup_prior_a2p_head(&owner_b, &lineage_key)
            .await
            .unwrap()
            .map(MemoryId::into_inner),
        Some(head_memory_id),
        "cross-org lookup_prior_a2p_head must return seed head (principal-only scope)",
    );

    drop(pg);
    let _ = drop_db(&db).await;
}
