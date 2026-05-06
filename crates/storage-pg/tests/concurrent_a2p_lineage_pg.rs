//! Concurrent A→P consolidation must not fork the Supersedes lineage.

mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::operators::{ConsolidateA2PRequest, NewPerspective, PersonalitySnapshot};
use proxima_core::storage::Storage;
use proxima_core::{
    CORE_DERIVED_FROM_RELATION, CORE_SUPERSEDES_RELATION, FlavorRegistry, MemoryId, PersonalityId,
    Principal, SchemaId, SchemaVersion,
};
use sqlx::Executor;
use time::OffsetDateTime;
use uuid::Uuid;

const TEST_PERSPECTIVE_SCHEMA_ID: &str = "proxima-test/concurrent-a2p-perspective-v1";
const TEST_PERSPECTIVE_TABLE: &str = "proxima_test.concurrent_a2p_perspective_v1";

async fn apply_test_sidecar(pool: &sqlx::PgPool) -> sqlx::Result<()> {
    pool.execute(
        "CREATE SCHEMA IF NOT EXISTS proxima_test; \
         CREATE TABLE IF NOT EXISTS proxima_test.concurrent_a2p_perspective_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             summary text NOT NULL \
         );",
    )
    .await
    .map(|_| ())
}

fn personality() -> PersonalitySnapshot {
    PersonalitySnapshot {
        personality_id: PersonalityId::new("proxima-test/lineage"),
        captured_at: OffsetDateTime::now_utc(),
    }
}

fn perspective(seed: u64) -> NewPerspective {
    NewPerspective {
        schema_id: SchemaId::new(TEST_PERSPECTIVE_SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(1),
        text: format!("perspective-{seed}"),
        typed_payload: serde_json::json!({ "summary": format!("perspective-{seed}") }),
        provenance: Vec::new(),
        embedding: vec![0.1, 0.2, 0.3, 0.4],
        embedding_model_id: "stub-embed".into(),
    }
}

async fn seed_prior_perspective(
    pool: &sqlx::PgPool,
    owner_kind: &str,
    owner_principal_id: Uuid,
    owner_org_id: Uuid,
) -> sqlx::Result<Uuid> {
    let p0 = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories \
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id, \
             schema_id, schema_version, kind, text, operator_kind, model_id, prompt_version, \
             personality_id) \
         VALUES ($1, $2, $3, $4, $5, 1, 'Perspective', 'p0', 'AtoP', 'stub-llm', 'v1', $6)",
    )
    .bind(p0)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(TEST_PERSPECTIVE_SCHEMA_ID)
    .bind("proxima-test/lineage")
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_test.concurrent_a2p_perspective_v1 (memory_id, summary) \
         VALUES ($1, 'p0')",
    )
    .bind(p0)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO proxima_core.a2p_invocations \
            (owner_principal_kind, owner_principal_id, \
             operator_id, prompt_version, model_id, personality_id, \
             context_hash, input_hash, head_memory_id) \
         VALUES ($1, $2, 'proxima-test/lineage-op', 'v1', 'stub-llm', \
                 'proxima-test/lineage', $3, $4, $5)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(&[1u8; 32][..])
    .bind(&[2u8; 32][..])
    .bind(p0)
    .execute(pool)
    .await?;

    Ok(p0)
}

#[tokio::test]
async fn concurrent_a2p_does_not_fork_lineage() -> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    let result = async {
        pg.run_migrations().await?;
        apply_test_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let (owner_kind, owner_principal_id, owner_org_id) = match &owner.principal {
            Principal::User(uid) => ("User", uid.into_inner(), owner.org_id.into_inner()),
            Principal::Group(gid) => ("Group", gid.into_inner(), owner.org_id.into_inner()),
        };
        let p0 =
            seed_prior_perspective(pg.pool(), owner_kind, owner_principal_id, owner_org_id).await?;

        let frozen = FlavorRegistry::new().freeze();
        let provenance_relation = frozen
            .resolve_relation(CORE_DERIVED_FROM_RELATION)
            .expect("core/derived-from registered");
        let supersedes_relation = frozen
            .resolve_relation(CORE_SUPERSEDES_RELATION)
            .expect("core/supersedes registered");

        let snap_a = personality();
        let snap_b = snap_a.clone();
        let owner_a = owner.clone();
        let owner_b = owner.clone();
        let p1 = perspective(1);
        let p2 = perspective(2);

        let fut_a = async {
            let perspectives = [p1];
            let req = ConsolidateA2PRequest {
                owner: owner_a,
                operator_id: "proxima-test/lineage-op",
                provenance_relation,
                supersedes_relation,
                model_id: "stub-llm",
                prompt_version: "v1",
                personality: &snap_a,
                context_hash: [1u8; 32],
                input_hash: [3u8; 32],
                prior_head: Some(MemoryId::new(p0)),
                perspectives: &perspectives,
                output_sidecar_table: TEST_PERSPECTIVE_TABLE,
            };
            pg.consolidate_a2p(&req).await
        };
        let fut_b = async {
            let perspectives = [p2];
            let req = ConsolidateA2PRequest {
                owner: owner_b,
                operator_id: "proxima-test/lineage-op",
                provenance_relation,
                supersedes_relation,
                model_id: "stub-llm",
                prompt_version: "v1",
                personality: &snap_b,
                context_hash: [1u8; 32],
                input_hash: [4u8; 32],
                prior_head: Some(MemoryId::new(p0)),
                perspectives: &perspectives,
                output_sidecar_table: TEST_PERSPECTIVE_TABLE,
            };
            pg.consolidate_a2p(&req).await
        };

        let (out_a, out_b) = tokio::join!(fut_a, fut_b);
        assert_eq!(out_a?.perspective_ids.len(), 1);
        assert_eq!(out_b?.perspective_ids.len(), 1);

        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.edges \
             WHERE relation_id = 'core/supersedes' AND target_memory_id = $1",
        )
        .bind(p0)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(
            count, 1,
            "expected exactly one Supersedes edge to the original prior_head",
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}
