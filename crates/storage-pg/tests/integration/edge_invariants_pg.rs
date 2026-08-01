//! What the edge table refuses.
//!
//! Every check here is enforced by the schema, not by the caller: the CHECK
//! constraints and the invariant trigger are what make E1–E3 true of any row
//! that reaches the table, including one a hand-written INSERT tries to
//! smuggle in. The Rust-side validators run first in production; these tests
//! prove the floor beneath them.

use proxima_core::{EdgeKind, EntityKind, MemoryId, MemoryOperatorKind, Owner, OwnerRef, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use crate::common::{drop_db, fresh_pg, seed_memory};

const TEST_ABSTRACTION_SCHEMA: &str = "test/edge-invariant-abstraction-v1";
const TEST_PERSPECTIVE_SCHEMA: &str = "test/edge-invariant-perspective-v1";

fn other_owner() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

async fn insert_derived_memory(
    pg: &PgStorage,
    owner: &Owner,
    kind: EntityKind,
) -> Result<MemoryId, sqlx::Error> {
    let memory_id = Uuid::now_v7();
    let schema_id = match kind {
        EntityKind::Perspective => TEST_PERSPECTIVE_SCHEMA,
        _ => TEST_ABSTRACTION_SCHEMA,
    };
    let operator_kind = match kind {
        EntityKind::Perspective => MemoryOperatorKind::AtoP,
        _ => MemoryOperatorKind::AtoA,
    };
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, $4, 1, $5, 'derived', $6,
                 '00000000-0000-0000-0000-000000000371'::uuid,
                 '00000000-0000-0000-0000-000000000372'::uuid, NULL,
                 'test-model', 'v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(schema_id)
    .bind(kind)
    .bind(operator_kind)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(MemoryId::new(memory_id))
}

/// Raw insert, deliberately bypassing every Rust-side validator.
async fn insert_edge_row(
    pg: &PgStorage,
    owner: &Owner,
    source: (EntityKind, Uuid),
    target: (EntityKind, Uuid),
    kind: EdgeKind,
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (source_kind, source_id, target_kind, target_id, kind, owner_kind, owner_id)
         VALUES ($1::text::proxima_core.edge_endpoint_kind, $2,
                 $3::text::proxima_core.edge_endpoint_kind, $4,
                 $5::text::proxima_core.edge_kind, $6, $7)",
    )
    .bind(source.0.as_str())
    .bind(source.1)
    .bind(target.0.as_str())
    .bind(target.1)
    .bind(kind.as_str())
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await
    .map(|_| ())
}

/// E3. A Fact asserts no judgment, so it cannot be the source of an edge
/// pointing at an Abstraction or a Perspective. The rule is one CHECK over two
/// columns — it never has to read another table.
#[tokio::test]
async fn an_upward_memory_edge_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = other_owner();
        let fact = seed_memory(&pg, &owner, EntityKind::Fact, "grounding fact").await?;
        let abstraction = insert_derived_memory(&pg, &owner, EntityKind::Abstraction).await?;

        let err = insert_edge_row(
            &pg,
            &owner,
            (EntityKind::Fact, fact.into_inner()),
            (EntityKind::Abstraction, abstraction.into_inner()),
            EdgeKind::Reference,
        )
        .await
        .expect_err("a Fact cannot point up the F/A/P order");
        assert!(
            err.to_string().contains("edges_layering_chk"),
            "unexpected error: {err}"
        );

        // The same pair the other way round is exactly what the model is for.
        insert_edge_row(
            &pg,
            &owner,
            (EntityKind::Abstraction, abstraction.into_inner()),
            (EntityKind::Fact, fact.into_inner()),
            EdgeKind::Origin,
        )
        .await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// A row asserting that a node relates to itself is not a connection between
/// two things, and no node write can mean it.
#[tokio::test]
async fn a_self_loop_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = other_owner();
        let fact = seed_memory(&pg, &owner, EntityKind::Fact, "alone").await?;
        let err = insert_edge_row(
            &pg,
            &owner,
            (EntityKind::Fact, fact.into_inner()),
            (EntityKind::Fact, fact.into_inner()),
            EdgeKind::Reference,
        )
        .await
        .expect_err("self-loop");
        assert!(
            err.to_string().contains("edges_no_self_loop_chk"),
            "unexpected error: {err}"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// E1. Both endpoints exist, or there is no row.
#[tokio::test]
async fn an_absent_endpoint_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = other_owner();
        let fact = seed_memory(&pg, &owner, EntityKind::Fact, "present").await?;

        let missing_target = insert_edge_row(
            &pg,
            &owner,
            (EntityKind::Fact, fact.into_inner()),
            (EntityKind::Fact, Uuid::now_v7()),
            EdgeKind::Reference,
        )
        .await
        .expect_err("absent target");
        assert!(
            missing_target
                .to_string()
                .contains("target endpoint not found"),
            "unexpected error: {missing_target}"
        );

        let missing_source = insert_edge_row(
            &pg,
            &owner,
            (EntityKind::Fact, Uuid::now_v7()),
            (EntityKind::Fact, fact.into_inner()),
            EdgeKind::Reference,
        )
        .await
        .expect_err("absent source");
        assert!(
            missing_source
                .to_string()
                .contains("source endpoint not found"),
            "unexpected error: {missing_source}"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// A declared endpoint kind that disagrees with the stored row is refused, so
/// a caller cannot widen what the layering rule sees by lying about a kind.
#[tokio::test]
async fn a_lying_endpoint_kind_is_refused() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = other_owner();
        let fact = seed_memory(&pg, &owner, EntityKind::Fact, "really a fact").await?;
        let perspective = insert_derived_memory(&pg, &owner, EntityKind::Perspective).await?;

        // Claim the Fact is a Perspective, which would make the upward edge
        // below look legal to the layering CHECK.
        let err = insert_edge_row(
            &pg,
            &owner,
            (EntityKind::Perspective, fact.into_inner()),
            (EntityKind::Perspective, perspective.into_inner()),
            EdgeKind::Reference,
        )
        .await
        .expect_err("declared source kind must match the stored row");
        assert!(
            err.to_string().contains("source kind"),
            "unexpected error: {err}"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// E2. The row is owned by the source owner — always, with no per-relation
/// policy cell left to consult.
#[tokio::test]
async fn an_edge_owner_that_is_not_the_source_owner_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = other_owner();
        let stranger = other_owner();
        let source = seed_memory(&pg, &owner, EntityKind::Fact, "mine").await?;
        let target = seed_memory(&pg, &owner, EntityKind::Fact, "also mine").await?;

        let err = insert_edge_row(
            &pg,
            &stranger,
            (EntityKind::Fact, source.into_inner()),
            (EntityKind::Fact, target.into_inner()),
            EdgeKind::Reference,
        )
        .await
        .expect_err("edge owner must be the source owner");
        assert!(
            err.to_string().contains("owner is not the source owner"),
            "unexpected error: {err}"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// A cross-owner TARGET is admitted when the source owner owns the row. That
/// is what makes cross-owner provenance expressible at all.
#[tokio::test]
async fn a_cross_owner_target_is_admitted() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = other_owner();
        let stranger = other_owner();
        let source = insert_derived_memory(&pg, &owner, EntityKind::Abstraction).await?;
        let target = seed_memory(&pg, &stranger, EntityKind::Fact, "theirs").await?;

        insert_edge_row(
            &pg,
            &owner,
            (EntityKind::Abstraction, source.into_inner()),
            (EntityKind::Fact, target.into_inner()),
            EdgeKind::Origin,
        )
        .await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// E5, at the level of the table: the primary key IS the row, so the same
/// content cannot be present twice — and the kind is part of the key, so
/// origin and reference between one pair are two different rows.
#[tokio::test]
async fn the_same_row_cannot_exist_twice() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = other_owner();
        let source = insert_derived_memory(&pg, &owner, EntityKind::Abstraction).await?;
        let target = seed_memory(&pg, &owner, EntityKind::Fact, "target").await?;

        insert_edge_row(
            &pg,
            &owner,
            (EntityKind::Abstraction, source.into_inner()),
            (EntityKind::Fact, target.into_inner()),
            EdgeKind::Origin,
        )
        .await?;
        let err = insert_edge_row(
            &pg,
            &owner,
            (EntityKind::Abstraction, source.into_inner()),
            (EntityKind::Fact, target.into_inner()),
            EdgeKind::Origin,
        )
        .await
        .expect_err("the key is the row");
        assert!(
            err.to_string().contains("edges_pkey"),
            "unexpected error: {err}"
        );

        insert_edge_row(
            &pg,
            &owner,
            (EntityKind::Abstraction, source.into_inner()),
            (EntityKind::Fact, target.into_inner()),
            EdgeKind::Reference,
        )
        .await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}
