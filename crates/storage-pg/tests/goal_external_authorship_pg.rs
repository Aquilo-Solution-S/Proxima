//! Regression: External authorship is admitted exactly at the
//! Proposed seed and rejected for direct Active/Rejected seeds.
//! Mirrors the trigger matrix encoded in
//! `migrations/20260506000050_goal_proposed_rejected.sql`.

mod common;

use common::{create_db, db_url, drop_db};
use std::sync::Arc;

use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::storage::Storage;
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalAuthorshipKind, GoalDraft, GoalState};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{OrgId, Owner, Principal, SchemaId, SchemaVersion, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn schemas_for_test() -> Vec<SchemaInfo> {
    vec![SchemaInfo {
        schema_id: SchemaId::new("test/goal_blob".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Goal,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
        tombstone: None,
        cbor_encoder: None,
    }]
}

fn external_draft(owner: &Owner, state: GoalState, request_id: &str) -> GoalDraft {
    GoalDraft {
        owner: owner.clone(),
        schema_id: SchemaId::new("test/goal_blob".into()),
        schema_version: SchemaVersion::new(1),
        title: "Test goal".to_string(),
        text: "external-authored goal".to_string(),
        payload: b"external payload".to_vec(),
        state,
        parent_goal_ids: vec![],
        supersedes_goal_id: None,
        authorship: GoalAuthorship::External,
        request_id: request_id.to_string(),
    }
}

#[tokio::test]
async fn external_authorship_admitted_at_proposed_seed_only() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        panic!("PG required for tests but admin connect failed");
    }
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(
            registry,
            MemoryStore::new(),
            Box::new(NoAuth::new(Principal::User(user), owner.clone())),
        )
        .with_storage(storage);

        // Proposed seed under External: allowed end-to-end through the verb.
        let proposed = engine
            .write_goal(
                &Credentials::None,
                external_draft(&owner, GoalState::Proposed, "req-proposed"),
            )
            .await?;
        assert!(!proposed.idempotent_replay);

        let row: (GoalState, GoalAuthorshipKind) = sqlx::query_as(
            "SELECT state, authorship_kind FROM proxima_core.goals WHERE goal_id = $1",
        )
        .bind(proposed.goal_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(row, (GoalState::Proposed, GoalAuthorshipKind::External));

        // Active seed under External: trigger rejects.
        let err = engine
            .write_goal(
                &Credentials::None,
                external_draft(&owner, GoalState::Active, "req-active"),
            )
            .await
            .expect_err("Active seed under External must be rejected");
        assert!(
            err.message.contains("goal:"),
            "unexpected error from External/Active seed: {err:?}"
        );

        // Rejected seed under External: trigger rejects (no direct seed
        // into Rejected, regardless of authorship).
        let err = engine
            .write_goal(
                &Credentials::None,
                external_draft(&owner, GoalState::Rejected, "req-rejected"),
            )
            .await
            .expect_err("Rejected seed under External must be rejected");
        assert!(
            err.message.contains("goal:"),
            "unexpected error from External/Rejected seed: {err:?}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("goal_external_authorship_pg test failed");
}
