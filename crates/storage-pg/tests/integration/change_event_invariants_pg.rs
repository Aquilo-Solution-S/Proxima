//! DB-level guard tests for `change_event` endpoint invariants.
//!
//! `change_event_endpoint_chk` enforces the same "exactly one of
//! memory_id/goal_id per endpoint" rule (plus not-null companions) that
//! the pull-read decode in `change_event.rs` relies on, so a raw INSERT
//! cannot persist an undecodable row. Mirrors the edges endpoint CHECKs.

use proxima_core::{Owner, OwnerPrincipalKind, Principal};
use uuid::Uuid;

fn owner_parts(owner: &Owner) -> (OwnerPrincipalKind, Uuid, Uuid) {
    let kind = OwnerPrincipalKind::of(&owner.principal);
    let principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    (kind, principal_id, owner.org_id.into_inner())
}

#[tokio::test]
async fn check_rejects_undecodable_change_event_rows() {
    let Some((pg, db_name)) = crate::common::fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;

        let owner = crate::common::owner_fixture();
        let (owner_kind, owner_principal_id, owner_org_id) = owner_parts(&owner);

        // Positive control: a well-formed EntityAppend (memory side) is
        // accepted. entity_memory_id has no FK, so a synthetic id is fine.
        sqlx::query(
            "INSERT INTO proxima_core.change_event
                (seq, owner_principal_kind, owner_principal_id, owner_org_id,
                 kind, entity_kind, entity_memory_id, entity_schema_id, entity_schema_version)
             VALUES ($1, $2, $3, $4, 'EntityAppend', 'Fact', $5, 'proxima/test', 1)",
        )
        .bind(Uuid::now_v7())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(Uuid::now_v7())
        .execute(pg.pool())
        .await?;

        // 1. EntityAppend with neither entity endpoint -> XOR violated.
        let err = sqlx::query(
            "INSERT INTO proxima_core.change_event
                (seq, owner_principal_kind, owner_principal_id, owner_org_id,
                 kind, entity_kind, entity_schema_id, entity_schema_version)
             VALUES ($1, $2, $3, $4, 'EntityAppend', 'Fact', 'proxima/test', 1)",
        )
        .bind(Uuid::now_v7())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .execute(pg.pool())
        .await
        .expect_err("EntityAppend with no entity endpoint must be rejected");
        assert!(err.to_string().contains("change_event_endpoint_chk"));

        // 2. EntityAppend that also populates an edge endpoint -> cross-population.
        let err = sqlx::query(
            "INSERT INTO proxima_core.change_event
                (seq, owner_principal_kind, owner_principal_id, owner_org_id,
                 kind, entity_kind, entity_memory_id, entity_schema_id,
                 entity_schema_version, edge_source_memory_id)
             VALUES ($1, $2, $3, $4, 'EntityAppend', 'Fact', $5, 'proxima/test', 1, $6)",
        )
        .bind(Uuid::now_v7())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .execute(pg.pool())
        .await
        .expect_err("EntityAppend carrying an edge endpoint must be rejected");
        assert!(err.to_string().contains("change_event_endpoint_chk"));

        // 3. EntityAppend missing schema columns -> not-null companion violated.
        let err = sqlx::query(
            "INSERT INTO proxima_core.change_event
                (seq, owner_principal_kind, owner_principal_id, owner_org_id,
                 kind, entity_kind, entity_memory_id)
             VALUES ($1, $2, $3, $4, 'EntityAppend', 'Fact', $5)",
        )
        .bind(Uuid::now_v7())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(Uuid::now_v7())
        .execute(pg.pool())
        .await
        .expect_err("EntityAppend without schema columns must be rejected");
        assert!(err.to_string().contains("change_event_endpoint_chk"));

        // 4. EdgeAppend with both source endpoints set -> edge XOR violated.
        // edge_source_goal_id has no FK on change_event, so a synthetic id
        // reaches the CHECK rather than tripping referential integrity first.
        let err = sqlx::query(
            "INSERT INTO proxima_core.change_event
                (seq, owner_principal_kind, owner_principal_id, owner_org_id,
                 kind, edge_id, edge_relation,
                 edge_source_memory_id, edge_source_goal_id, edge_target_memory_id)
             VALUES ($1, $2, $3, $4, 'EdgeAppend', $5, 'test/relation', $6, $7, $8)",
        )
        .bind(Uuid::now_v7())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .bind(Uuid::now_v7())
        .execute(pg.pool())
        .await
        .expect_err("EdgeAppend with both source endpoints must be rejected");
        assert!(err.to_string().contains("change_event_endpoint_chk"));

        Ok(())
    }
    .await;

    let _ = crate::common::drop_db(&db_name).await;
    result.expect("change_event endpoint CHECK rejects undecodable rows");
}
