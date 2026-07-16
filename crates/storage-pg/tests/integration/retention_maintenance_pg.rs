//! Retention maintenance passes against a transient PG database: owner
//! Fact-retention enforcement (tombstone sweep) and `change_event`
//! pruning, including the per-owner legal-hold gate, dry runs, batching,
//! and pass-lock exclusivity.

use crate::common::{fresh_pg, seed_memory};
use proxima_core::verbs::persist_mcp_call::MCP_CALL_FACT_SCHEMA;
use proxima_core::{EntityKind, MemoryId, Owner, OwnerRef, UserId};
use proxima_storage_pg::access::owner_columns::owner_binds;
use proxima_storage_pg::{ChangeEventPruneOptions, PgStorage, RetentionEnforceOptions};
use uuid::Uuid;

fn unique_owner() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

async fn set_fact_retention(pg: &PgStorage, owner: &Owner, seconds: i64) {
    let (owner_kind, owner_id) = owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.owner_fact_retention
            (owner_kind, owner_id, retention_seconds)
         VALUES ($1, $2, $3)",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(seconds)
    .execute(pg.pool_for_tests())
    .await
    .expect("insert retention config");
}

async fn set_legal_hold(pg: &PgStorage, owner: &Owner) {
    let (owner_kind, owner_id) = owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.owner_legal_holds
            (owner_kind, owner_id, hold_active)
         VALUES ($1, $2, true)
         ON CONFLICT (owner_kind, owner_id) DO NOTHING",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await
    .expect("insert legal hold");
}

/// Releasing a hold is a DELETE — `owner_legal_holds_active_chk` forbids
/// inactive rows, mirroring `clear_legal_hold` in `fact_retention.rs`.
async fn clear_legal_hold(pg: &PgStorage, owner: &Owner) {
    let (owner_kind, owner_id) = owner_binds(owner);
    sqlx::query(
        "DELETE FROM proxima_core.owner_legal_holds
          WHERE owner_kind = $1
            AND owner_id IS NOT DISTINCT FROM $2",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await
    .expect("delete legal hold");
}

/// Age a memory row past a retention window. `created_at` is temporal
/// metadata deliberately left out of the append-only trigger whitelist so
/// tests can fabricate retention scenarios (`0010_v006.sql`).
async fn backdate_memory(pg: &PgStorage, memory_id: MemoryId, days: i32) {
    sqlx::query(
        "UPDATE proxima_core.memories
            SET created_at = now() - make_interval(days => $2)
          WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .bind(days)
    .execute(pg.pool_for_tests())
    .await
    .expect("backdate memory");
}

async fn backdate_owner_change_events(pg: &PgStorage, owner: &Owner, days: i32) {
    let (owner_kind, owner_id) = owner_binds(owner);
    sqlx::query(
        "UPDATE proxima_core.change_event
            SET created_at = now() - make_interval(days => $3)
          WHERE owner_kind = $1
            AND owner_id IS NOT DISTINCT FROM $2",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(days)
    .execute(pg.pool_for_tests())
    .await
    .expect("backdate change events");
}

/// A Fact row carrying the MCP-call audit schema, aged past every window.
/// Inserted directly: the sweep predicate only consults `memories` columns.
async fn seed_backdated_audit_fact(pg: &PgStorage, owner: &Owner) -> MemoryId {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, created_at)
         VALUES ($1, $2, $3, $4, 1, now() - interval '30 days')",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(MCP_CALL_FACT_SCHEMA)
    .execute(pg.pool_for_tests())
    .await
    .expect("insert audit fact");
    MemoryId::new(memory_id)
}

async fn tombstoned_at_is_set(pg: &PgStorage, memory_id: MemoryId) -> bool {
    sqlx::query_scalar(
        "SELECT tombstoned_at IS NOT NULL
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("read tombstoned_at")
}

async fn entity_delete_events_for(pg: &PgStorage, memory_id: MemoryId) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*)
           FROM proxima_core.change_event
          WHERE kind = 'EntityDelete'
            AND entity_memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("count EntityDelete events")
}

async fn owner_change_event_count(pg: &PgStorage, owner: &Owner) -> i64 {
    let (owner_kind, owner_id) = owner_binds(owner);
    sqlx::query_scalar(
        "SELECT count(*)
           FROM proxima_core.change_event
          WHERE owner_kind = $1
            AND owner_id IS NOT DISTINCT FROM $2",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_one(pg.pool_for_tests())
    .await
    .expect("count owner change events")
}

#[tokio::test]
async fn retention_tombstones_only_expired_non_audit_facts() {
    let (pg, _db) = fresh_pg().await;
    let owner = unique_owner();
    set_fact_retention(&pg, &owner, 86_400).await;

    let expired_a = seed_memory(&pg, &owner, EntityKind::Fact, "expired a")
        .await
        .expect("seed fact");
    let expired_b = seed_memory(&pg, &owner, EntityKind::Fact, "expired b")
        .await
        .expect("seed fact");
    let fresh = seed_memory(&pg, &owner, EntityKind::Fact, "fresh")
        .await
        .expect("seed fact");
    backdate_memory(&pg, expired_a, 2).await;
    backdate_memory(&pg, expired_b, 2).await;
    let audit = seed_backdated_audit_fact(&pg, &owner).await;

    // An unconfigured owner's expired Fact must never be swept.
    let bystander = unique_owner();
    let bystander_fact = seed_memory(&pg, &bystander, EntityKind::Fact, "no window")
        .await
        .expect("seed fact");
    backdate_memory(&pg, bystander_fact, 30).await;

    let outcome = pg
        .enforce_fact_retention(RetentionEnforceOptions::default())
        .await
        .expect("enforce retention");
    assert_eq!(outcome.facts_tombstoned, 2);
    assert_eq!(outcome.owners_skipped_hold, 0);
    assert!(!outcome.dry_run);
    assert_eq!(outcome.owners.len(), 1);
    let owner_outcome = &outcome.owners[0];
    assert_eq!(owner_outcome.owner, owner);
    assert_eq!(owner_outcome.retention_seconds, 86_400);
    assert_eq!(owner_outcome.facts_tombstoned, 2);
    assert!(!owner_outcome.skipped_legal_hold);

    assert!(tombstoned_at_is_set(&pg, expired_a).await);
    assert!(tombstoned_at_is_set(&pg, expired_b).await);
    assert!(!tombstoned_at_is_set(&pg, fresh).await);
    assert!(
        !tombstoned_at_is_set(&pg, audit).await,
        "MCP-call audit Facts are indefinite controller evidence"
    );
    assert!(!tombstoned_at_is_set(&pg, bystander_fact).await);

    // The Facts left the live set, so the pull log must say so.
    assert_eq!(entity_delete_events_for(&pg, expired_a).await, 1);
    assert_eq!(entity_delete_events_for(&pg, expired_b).await, 1);
    assert_eq!(entity_delete_events_for(&pg, fresh).await, 0);

    // Idempotent: a second pass finds nothing left to tombstone.
    let rerun = pg
        .enforce_fact_retention(RetentionEnforceOptions::default())
        .await
        .expect("re-run retention");
    assert_eq!(rerun.facts_tombstoned, 0);
    assert_eq!(entity_delete_events_for(&pg, expired_a).await, 1);
}

#[tokio::test]
async fn retention_skips_owner_under_legal_hold() {
    let (pg, _db) = fresh_pg().await;
    let owner = unique_owner();
    set_fact_retention(&pg, &owner, 3_600).await;
    let expired = seed_memory(&pg, &owner, EntityKind::Fact, "held")
        .await
        .expect("seed fact");
    backdate_memory(&pg, expired, 2).await;
    set_legal_hold(&pg, &owner).await;

    let outcome = pg
        .enforce_fact_retention(RetentionEnforceOptions::default())
        .await
        .expect("enforce retention");
    assert_eq!(outcome.facts_tombstoned, 0);
    assert_eq!(outcome.owners_skipped_hold, 1);
    assert!(outcome.owners[0].skipped_legal_hold);
    assert!(!tombstoned_at_is_set(&pg, expired).await);
    assert_eq!(entity_delete_events_for(&pg, expired).await, 0);

    // Releasing the hold releases enforcement.
    clear_legal_hold(&pg, &owner).await;
    let outcome = pg
        .enforce_fact_retention(RetentionEnforceOptions::default())
        .await
        .expect("enforce retention after release");
    assert_eq!(outcome.facts_tombstoned, 1);
    assert_eq!(outcome.owners_skipped_hold, 0);
    assert!(tombstoned_at_is_set(&pg, expired).await);
}

#[tokio::test]
async fn retention_dry_run_counts_without_mutating() {
    let (pg, _db) = fresh_pg().await;
    let owner = unique_owner();
    set_fact_retention(&pg, &owner, 3_600).await;
    let expired = seed_memory(&pg, &owner, EntityKind::Fact, "would expire")
        .await
        .expect("seed fact");
    backdate_memory(&pg, expired, 2).await;

    let outcome = pg
        .enforce_fact_retention(RetentionEnforceOptions {
            dry_run: true,
            ..RetentionEnforceOptions::default()
        })
        .await
        .expect("dry-run retention");
    assert!(outcome.dry_run);
    assert_eq!(outcome.facts_tombstoned, 1);
    assert!(!tombstoned_at_is_set(&pg, expired).await);
    assert_eq!(entity_delete_events_for(&pg, expired).await, 0);
}

#[tokio::test]
async fn retention_batches_until_drained() {
    let (pg, _db) = fresh_pg().await;
    let owner = unique_owner();
    set_fact_retention(&pg, &owner, 3_600).await;
    let mut expired = Vec::new();
    for i in 0..5 {
        let memory_id = seed_memory(&pg, &owner, EntityKind::Fact, &format!("expired {i}"))
            .await
            .expect("seed fact");
        backdate_memory(&pg, memory_id, 2).await;
        expired.push(memory_id);
    }

    let outcome = pg
        .enforce_fact_retention(RetentionEnforceOptions {
            batch_size: 2,
            dry_run: false,
        })
        .await
        .expect("enforce retention in batches");
    assert_eq!(outcome.facts_tombstoned, 5);
    for memory_id in expired {
        assert!(tombstoned_at_is_set(&pg, memory_id).await);
        assert_eq!(entity_delete_events_for(&pg, memory_id).await, 1);
    }
}

#[tokio::test]
async fn change_event_prune_respects_horizon_and_hold() {
    let (pg, _db) = fresh_pg().await;

    let stale_owner = unique_owner();
    seed_memory(&pg, &stale_owner, EntityKind::Fact, "old events")
        .await
        .expect("seed fact");
    backdate_owner_change_events(&pg, &stale_owner, 10).await;
    let stale_before = owner_change_event_count(&pg, &stale_owner).await;
    assert!(stale_before > 0, "ingest must have logged change events");

    let fresh_owner = unique_owner();
    seed_memory(&pg, &fresh_owner, EntityKind::Fact, "fresh events")
        .await
        .expect("seed fact");
    let fresh_before = owner_change_event_count(&pg, &fresh_owner).await;

    let held_owner = unique_owner();
    seed_memory(&pg, &held_owner, EntityKind::Fact, "held events")
        .await
        .expect("seed fact");
    backdate_owner_change_events(&pg, &held_owner, 10).await;
    let held_before = owner_change_event_count(&pg, &held_owner).await;
    set_legal_hold(&pg, &held_owner).await;

    let seven_days = 7 * 86_400;

    // Dry run first: counts, no deletion.
    let dry = pg
        .prune_change_events(ChangeEventPruneOptions {
            older_than_seconds: seven_days,
            batch_size: 1000,
            dry_run: true,
        })
        .await
        .expect("dry-run prune");
    assert!(dry.dry_run);
    assert_eq!(
        owner_change_event_count(&pg, &stale_owner).await,
        stale_before
    );

    let outcome = pg
        .prune_change_events(ChangeEventPruneOptions {
            older_than_seconds: seven_days,
            batch_size: 2,
            dry_run: false,
        })
        .await
        .expect("prune change events");
    assert_eq!(
        outcome.events_pruned,
        u64::try_from(stale_before).expect("count")
    );
    assert_eq!(outcome.owners_skipped_hold, 1);

    assert_eq!(owner_change_event_count(&pg, &stale_owner).await, 0);
    assert_eq!(
        owner_change_event_count(&pg, &fresh_owner).await,
        fresh_before,
        "events inside the horizon stay"
    );
    assert_eq!(
        owner_change_event_count(&pg, &held_owner).await,
        held_before,
        "a held owner's events are never pruned"
    );

    let held_outcome = outcome
        .owners
        .iter()
        .find(|owner| owner.owner == held_owner)
        .expect("held owner reported");
    assert!(held_outcome.skipped_legal_hold);
    assert_eq!(held_outcome.events_pruned, 0);
}

#[tokio::test]
async fn retention_maintenance_lock_excludes_concurrent_passes() {
    let (pg, _db) = fresh_pg().await;

    let held = pg
        .try_retention_maintenance_lock()
        .await
        .expect("first lock query")
        .expect("first pass acquires the lock");

    assert!(
        pg.try_retention_maintenance_lock()
            .await
            .expect("second lock query")
            .is_none(),
        "second concurrent pass must skip"
    );

    // The retention lock is independent of the embedding-maintenance lock.
    let embedding = pg
        .try_embedding_maintenance_lock()
        .await
        .expect("embedding lock query")
        .expect("embedding maintenance is not serialized against retention");
    drop(embedding);

    drop(held);
    for _ in 0..50 {
        if pg
            .try_retention_maintenance_lock()
            .await
            .expect("re-acquire lock query")
            .is_some()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("dropping the guard must release the retention lock");
}
