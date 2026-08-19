//! Owner Fact-retention config against blank `0001_v008.sql`.
//!
//! `proxima_core.owner_fact_retention` is read by `get_graph` on every call,
//! so an absent table is a boot-shaped failure with no test to catch it —
//! which is exactly what happened when the squashed migration dropped the
//! table and left the Rust surface behind.

use proxima_core::storage_ports::{FactRetentionPort, OwnerWritePermit};
use proxima_core::{AccessKind, OwnerRef, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::{PgStorage, RetentionEnforceOptions};
use uuid::{NoContext, Timestamp, Uuid};

/// A `t` whose embedded uuidv7 timestamp is `age_seconds` in the past —
/// what `uuid_extract_timestamp` reads to decide a Fact is past its window.
fn aged_t(age_seconds: u64) -> Uuid {
    let now = u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp())
        .expect("test clock is after the epoch");
    Uuid::new_v7(Timestamp::from_unix(NoContext, now - age_seconds, 0))
}

async fn seed_fact(pool: &sqlx::PgPool, owner: &OwnerRef, t: Uuid) -> Result<(), sqlx::Error> {
    let owner_id = owner.stored_owner_id();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', 'core/agent-note-v1', $2, $1)",
    )
    .bind(t)
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
         VALUES ($1, $1, 'fact', $2, 'core/agent-note-v1')",
    )
    .bind(t)
    .bind(owner_id)
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn fact_retention_upsert_get_clear_and_enforcement_pass() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);

        assert_eq!(
            pg.get_fact_retention(&owner).await?,
            None,
            "an owner with no configured window reads absent, not zero"
        );

        pg.upsert_fact_retention(&permit, 3_600).await?;
        assert_eq!(pg.get_fact_retention(&owner).await?, Some(3_600));

        // Same arbiter as the ON CONFLICT clause: a second upsert replaces.
        pg.upsert_fact_retention(&permit, 60).await?;
        assert_eq!(pg.get_fact_retention(&owner).await?, Some(60));
        let rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.owner_fact_retention
              WHERE owner_id IS NOT DISTINCT FROM $1",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(rows, 1, "upsert must not append a second config row");

        // Three Facts past the 60s window and one inside it: the sweep's
        // expired-Fact predicate has to separate them, and batch_size=1 must
        // make progress across multiple transactions.
        let expired = [aged_t(3_600), aged_t(3_601), aged_t(3_602)];
        for t in expired {
            seed_fact(pool, &owner, t).await?;
        }
        let recent = Uuid::now_v7();
        seed_fact(pool, &owner, recent).await?;

        let dry_run = pg
            .enforce_fact_retention(RetentionEnforceOptions {
                batch_size: 1,
                dry_run: true,
            })
            .await?;
        assert!(dry_run.dry_run);
        let [owner_outcome] = dry_run.owners.as_slice() else {
            panic!("the pass must visit exactly the one configured owner");
        };
        assert_eq!(owner_outcome.owner, owner);
        assert_eq!(owner_outcome.retention_seconds, 60);
        assert!(!owner_outcome.skipped_legal_hold);
        assert_eq!(
            owner_outcome.facts_forgotten, 3,
            "only the Facts older than the window are due"
        );
        let hot_before: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory WHERE owner_id = $1",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        let cooled_before: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.cooled WHERE owner_id = $1",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(hot_before, 4, "dry-run must leave all hot Facts intact");
        assert_eq!(cooled_before, 0, "dry-run must not create cold stubs");

        let held_fact = aged_t(3_603);
        seed_fact(pool, &owner, held_fact).await?;

        // A legal hold suspends the sweep for the owner (docs/13).
        pg.set_legal_hold(&permit).await?;
        let held = pg
            .enforce_fact_retention(RetentionEnforceOptions {
                batch_size: 1,
                dry_run: false,
            })
            .await?;
        let [held_outcome] = held.owners.as_slice() else {
            panic!("the held owner is still visited");
        };
        assert!(held_outcome.skipped_legal_hold);
        assert_eq!(held.owners_skipped_hold, 1);
        assert_eq!(held_outcome.facts_forgotten, 0);
        let held_hot: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(held_fact)
                .fetch_one(pool)
                .await?;
        assert_eq!(held_hot, 1, "legal hold must prevent mutation");
        assert!(pg.clear_legal_hold(&permit).await?);

        let outcome = pg
            .enforce_fact_retention(RetentionEnforceOptions {
                batch_size: 1,
                dry_run: false,
            })
            .await?;
        assert!(!outcome.dry_run);
        assert_eq!(outcome.facts_forgotten, 4);
        assert_eq!(outcome.owners_skipped_hold, 0);

        for t in expired.into_iter().chain([held_fact]) {
            let hot: i64 =
                sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                    .bind(t)
                    .fetch_one(pool)
                    .await?;
            let cooled: i64 =
                sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cooled WHERE t = $1")
                    .bind(t)
                    .fetch_one(pool)
                    .await?;
            let forgets: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint
                   FROM proxima_core.announce
                  WHERE t = $1 AND op = 'forget' AND entity = 'memory'",
            )
            .bind(t)
            .fetch_one(pool)
            .await?;
            assert_eq!(hot, 0, "forgotten Fact must leave the hot set");
            assert_eq!(cooled, 1, "forgotten Fact must leave a cold stub");
            assert_eq!(forgets, 1, "forget announcement must be committed");
        }
        let recent_hot: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
                .bind(recent)
                .fetch_one(pool)
                .await?;
        assert_eq!(recent_hot, 1, "recent Fact remains hot");

        let second = pg
            .enforce_fact_retention(RetentionEnforceOptions {
                batch_size: 1,
                dry_run: false,
            })
            .await?;
        assert_eq!(second.facts_forgotten, 0, "forget pass is idempotent");

        assert!(pg.clear_fact_retention(&permit).await?);
        assert_eq!(pg.get_fact_retention(&owner).await?, None);
        assert!(
            !pg.clear_fact_retention(&permit).await?,
            "clearing an absent window reports no row removed"
        );
        assert!(
            pg.enforce_fact_retention(RetentionEnforceOptions {
                batch_size: 100,
                dry_run: true,
            })
            .await?
            .owners
            .is_empty(),
            "a cleared window takes the owner out of the sweep"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("fact retention test failed");
}
