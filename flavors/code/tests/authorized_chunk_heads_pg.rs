//! Direct coverage for `proxima::flavor::authorized_code_chunk_head_candidates`.
//!
//! This is the one authorized-read helper that implements owner-or-World
//! scoping in its own SQL (`crates/storage-pg/src/verbs/query/abstraction_heads.rs`)
//! instead of routing through `Engine::query`'s authz-resolved read set, so
//! the World-visibility test on `authorized_memory_ids` never touched
//! it. These tests exercise the verb directly against seeded
//! `code-chunk-v1` rows: owner scoping, World visibility for a non-owner
//! caller, and the per-owner scoping of the same-natural-key recency dedup.

mod common;

use common::{TestDb, test_owner};
use proxima_code::CodeChunkV1;
use proxima_core::{AbstractionPayload, Owner};
use uuid::Uuid;

/// Seed one `code-chunk-v1` head row (source batch + memories + sidecar) and
/// return its memory id. `batch` orders recency: the verb's dedup compares
/// `source_batch_id`, and `Uuid::now_v7()` batches minted later sort greater.
async fn seed_chunk(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
    file_path: &str,
    chunk_index: i32,
    batch: Uuid,
) -> Result<Uuid, sqlx::Error> {
    let memory_id = Uuid::now_v7();
    let handle = Uuid::now_v7();
    let owner_id = owner.stored_owner_id();
    let kind = proxima_core::OwnerRefKind::of(owner).as_str();
    let _ = batch;
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .bind(kind)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'abstraction', $2, $3, $4)",
    )
    .bind(handle)
    .bind(<CodeChunkV1 as AbstractionPayload>::SCHEMA_ID)
    .bind(owner_id)
    .bind(memory_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id)
         VALUES ($1, $2, 'abstraction', $3)",
    )
    .bind(handle)
    .bind(memory_id)
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_code.code_chunk_v1
            (t, repo_id, file_path, chunk_index, text, language, chunk_type,
             byte_range_start, byte_range_end, line_range_start, line_range_end, state)
         VALUES ($1, $2, $3, $4, 'chunk body', 'rust', 'block', 0, 8, 1, 1, 'Present')",
    )
    .bind(memory_id)
    .bind(repo_id)
    .bind(file_path)
    .bind(chunk_index)
    .execute(pool)
    .await?;
    Ok(memory_id)
}

/// Move one seeded chunk's memories row to the World owner — the same
/// single-row owner transfer `transfer_to_world` performs after
/// `Engine::publish_to_world` authorizes it (the batch row keeps its
/// original write-attribution owner, exactly as in production).
async fn heads(
    pool: &sqlx::PgPool,
    owner: Owner,
    candidates: &[Uuid],
) -> Result<Vec<Uuid>, Box<dyn std::error::Error>> {
    let mut ids = proxima::flavor::authorized_code_chunk_head_candidates(
        pool,
        owner,
        &<CodeChunkV1 as AbstractionPayload>::schema_id(),
        candidates,
    )
    .await?;
    ids.sort_unstable();
    Ok(ids)
}

fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.sort_unstable();
    ids
}

/// (a) An owner-scoped call returns only that owner's heads plus World
/// heads — never another owner's rows. (b) A World-owned (published) chunk
/// head surfaces for a caller with no relationship to the original owner,
/// through this verb directly.
#[tokio::test]
async fn owner_scoping_excludes_foreign_rows_and_world_surfaces_for_non_owner() {
    let db = TestDb::fresh().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = db.pg.pool_for_tests();
        let owner_a = test_owner();
        let owner_b = test_owner();
        let repo_id = Uuid::now_v7();

        let a_row = seed_chunk(pool, &owner_a, repo_id, "a.rs", 0, Uuid::now_v7()).await?;
        let b_row = seed_chunk(pool, &owner_b, repo_id, "b.rs", 0, Uuid::now_v7()).await?;
        // Published row: authored by A, then owner-transferred to World.
        let w_row = seed_chunk(
            pool,
            &proxima_core::OwnerRef::World,
            repo_id,
            "w.rs",
            0,
            Uuid::now_v7(),
        )
        .await?;

        let candidates = [a_row, b_row, w_row];

        assert_eq!(
            heads(pool, owner_a, &candidates).await?,
            sorted(vec![a_row, w_row]),
            "owner A must see A's head and the World head, never B's row"
        );
        assert_eq!(
            heads(pool, owner_b, &candidates).await?,
            sorted(vec![b_row, w_row]),
            "owner B (no relationship to A) must see B's head and the published World head, never A's row"
        );

        Ok(())
    }
    .await;
    result.expect("owner scoping / World visibility test failed");
}

/// The same-natural-key recency dedup is scoped per owner: a newer row from
/// one owner shadows only that owner's older rows at the same
/// (`repo_id`, `file_path`, `chunk_index`) — it must never shadow a
/// World-owned head at the same natural key, and vice versa.
#[tokio::test]
async fn same_natural_key_recency_dedup_is_scoped_per_owner() {
    let db = TestDb::fresh().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = db.pg.pool_for_tests();
        let owner = test_owner();
        let repo_id = Uuid::now_v7();

        // Oldest row at the NK, published to World.
        let world_old = seed_chunk(
            pool,
            &proxima_core::OwnerRef::World,
            repo_id,
            "hot.rs",
            0,
            Uuid::now_v7(),
        )
        .await?;
        // Then two generations of the same owner's row at the same NK.
        let own_old = seed_chunk(pool, &owner, repo_id, "hot.rs", 0, Uuid::now_v7()).await?;
        let own_new = seed_chunk(pool, &owner, repo_id, "hot.rs", 0, Uuid::now_v7()).await?;

        let candidates = [world_old, own_old, own_new];
        assert_eq!(
            heads(pool, owner, &candidates).await?,
            sorted(vec![world_old, own_new]),
            "the owner's newer row shadows only the owner's older row; the \
             World head at the same natural key survives despite being oldest"
        );

        Ok(())
    }
    .await;
    result.expect("per-owner natural-key dedup test failed");
}
