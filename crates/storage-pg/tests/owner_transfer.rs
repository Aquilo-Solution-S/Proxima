//! P2: an owner-to-owner transfer is an in-place series transfer.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;

use proxima_core::compliance::{
    ComplianceEraseOutcome, ComplianceEraseTarget, ComplianceExportTarget, ComplianceSidecarTables,
    EraseAuthorization, ExportAuthorization,
};
use proxima_core::storage_ports::MemoryAuthoringPort;
use proxima_core::storage_ports::{
    ChangeEventPort, ComplianceErasePort, McpCallReadPort, MemoryReadPort, OwnerTransferPort,
    OwnerWritePermit,
};
use proxima_core::verbs::fact_ingest::FactIngestOutcome;
use proxima_core::verbs::fact_ingest::{CitationSpec, FactWriteCommand};
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::mcp_call_history::McpCallHistoryRequest;
use proxima_core::verbs::persist_mcp_call::McpCallLoggedV1;
use proxima_core::verbs::query::QueryRequest;
use proxima_core::{
    AccessKind, ChangeEventKind, ColdObjectStore, EntityId, EntityRef, GoalId, GroupId, MemoryId,
    OwnerRef, SchemaId, SchemaVersion, StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::sidecars::PgMemorySidecar;
use proxima_storage_pg::verbs::fact_ingest::{ingest_fact_atomic, ingest_fact_in_tx};
use proxima_storage_pg::verbs::forget::{MemoryColdStore, hydrate_memory};
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
use proxima_storage_pg::verbs::wake_timeseries::{
    WakeConfigDraft, WakeTriggerKind, insert_wake_config, write_armed_goal,
};
use proxima_storage_pg::{PgStorage, core_pg_sidecars};
use uuid::Uuid;

/// The five sidecar legs exactly as the engine assembles them: from the
/// frozen flavor registry. Passing empty slices here would silently skip
/// the owner-pinned leg, which is the difference these tests exist to
/// measure.
fn contract_sidecar_tables() -> ComplianceSidecarTables {
    ComplianceSidecarTables::for_registry(
        &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
    )
}

fn draft() -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("core/test-fact-v1".to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: Some("src".into()),
        ingest_key: Some("k1".into()),
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

/// Transfers go owner -> owner. The destination is a group: group-manage is
/// the receiving side's consent, so a group is the only destination the
/// engine gate admits. Its `owners` row is deliberately NOT pre-created —
/// the transfer transaction must mint it (`ensure_owner_row`) or every
/// `owner_id` FK below fails.
/// One tool call, written the way production writes it: the typed payload
/// through the ordinary Fact ingest, so the Memory stamps
/// `sidecar_tables` and the sidecar's `owner_id` is filled by the
/// owner-pinned INSERT rather than by this test.
async fn ingest_mcp_call_fact(
    pool: &sqlx::PgPool,
    permit: &OwnerWritePermit,
) -> Result<FactIngestOutcome, StorageError> {
    let payload = McpCallLoggedV1 {
        tool_name: "core_remember".into(),
        actor_oid: "oid-1".into(),
        actor_upn: "alice@example.test".into(),
        ok: true,
        error: None,
        latency_ms: 5,
        io_byte_len: 10,
        io_truncated: false,
        io_content_hash: [3_u8; 32],
    };
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Unavailable(format!("begin mcp call ingest: {err}")))?;
    let sidecar = payload.clone();
    let outcome = ingest_fact_in_tx(&mut tx, permit, &payload, None, move |tx, outcome| {
        Box::pin(async move { sidecar.insert_memory_sidecar(tx, outcome.memory_id).await })
    })
    .await?;
    tx.commit()
        .await
        .map_err(|err| StorageError::Unavailable(format!("commit mcp call ingest: {err}")))?;
    Ok(outcome)
}

/// A Fact carrying the mcp-call schema, for appending a second version to a
/// series whose first version is a real tool call.
fn mcp_draft() -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(
            proxima_core::verbs::persist_mcp_call::MCP_CALL_FACT_SCHEMA.to_string(),
        ),
        ..draft()
    }
}

/// How many of this Memory's hydrated payloads carry an actor identity,
/// read as `owner`.
async fn hydrated_actor_payloads(
    pg: &PgStorage,
    owner: OwnerRef,
    memory_id: MemoryId,
) -> Result<usize, StorageError> {
    let response = pg
        .query_memories(
            &QueryRequest {
                memory_ids: vec![memory_id],
                include_payloads: true,
                ..QueryRequest::for_owner(owner)
            },
            &[],
        )
        .await?;
    Ok(response
        .memories
        .iter()
        .filter(|memory| {
            memory
                .payload
                .as_ref()
                .and_then(|payload| payload.to_protocol_json().ok())
                .is_some_and(|json| json.get("actor_upn").is_some())
        })
        .count())
}

/// Which owners the `mcp_call_logged_v1` rows for `t` are pinned to.
async fn pinned_owners(pool: &sqlx::PgPool, t: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT owner_id FROM proxima_core.mcp_call_logged_v1 WHERE t = $1")
        .bind(t)
        .fetch_all(pool)
        .await
}

fn history_request(owner: OwnerRef) -> McpCallHistoryRequest {
    McpCallHistoryRequest {
        owner,
        actor_oid: None,
        before: None,
        limit: 10,
        include_body: false,
    }
}

async fn export_bundle(
    pg: &PgStorage,
    owner: OwnerRef,
) -> Result<proxima_core::compliance::ComplianceExportBundle, StorageError> {
    let target = match owner {
        OwnerRef::Personal(user_id) => ComplianceExportTarget::PersonalOwner { user_id },
        OwnerRef::Group(group_id) => ComplianceExportTarget::GroupOwner { group_id },
    };
    pg.export_owner_bundle(
        &ExportAuthorization::new_for_tests(target),
        &contract_sidecar_tables(),
    )
    .await
}

/// The exported `mcp_call_logged_v1` rows, whole.
///
/// Returning the rows rather than a count is deliberate. Counting them cannot
/// tell a row from a scalar, and the owner-pinned export once emitted the
/// primary key where the row belonged (`to_jsonb(t)` resolves to the column
/// named `t`, not the range table, when only one table is in scope). The
/// cardinality was right and the bundle was empty of content.
fn mcp_rows_in(
    bundle: &proxima_core::compliance::ComplianceExportBundle,
) -> Vec<&serde_json::Value> {
    bundle
        .sidecars
        .iter()
        .filter(|sidecar| sidecar.table == "proxima_core.mcp_call_logged_v1")
        .flat_map(|sidecar| sidecar.rows.iter())
        .collect()
}

fn destination() -> OwnerRef {
    OwnerRef::Group(GroupId::new(Uuid::now_v7()))
}

async fn fresh_pg() -> (String, PgStorage) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect");
    pg.run_migrations().await.expect("migrate");
    (db_name, pg)
}

async fn fresh_pg_with_cold() -> (String, PgStorage, Arc<MemoryColdStore>) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let cold = Arc::new(MemoryColdStore::default());
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect")
        .with_cold(cold.clone());
    pg.run_migrations().await.expect("migrate");
    (db_name, pg, cold)
}

#[tokio::test]
async fn transfer_moves_same_memory_t_and_sidecar() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let written = ingest_fact_atomic(pool, &permit, &draft(), None).await?;
        let t = written.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
             VALUES ($1, $1, 'pub', 'body')",
        )
        .bind(t)
        .execute(pool)
        .await?;
        let content_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/test-fact-v1', $2)
             RETURNING content_id",
        )
        .bind(owner.stored_owner_id())
        .bind(vec![7_u8; 32])
        .fetch_one(pool)
        .await?;
        sqlx::query("UPDATE proxima_core.memory SET content_id = $2 WHERE t = $1")
            .bind(t)
            .bind(content_id)
            .execute(pool)
            .await?;

        let first = pg
            .transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
            .await?;
        assert!(first);
        let content_owner: Uuid = sqlx::query_scalar(
            "SELECT c.owner_id
               FROM proxima_core.content c
               JOIN proxima_core.memory m ON m.content_id = c.content_id
              WHERE m.t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            content_owner,
            dest.stored_owner_id(),
            "transfer must re-home Content with the Memory"
        );
        let old_left: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.content WHERE content_id = $1",
        )
        .bind(content_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            old_left, 0,
            "origin Content is GC'd after an exclusive transfer"
        );
        let owner_id: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(owner_id, dest.stored_owner_id());
        let sketch_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.sketch WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            sketch_owner,
            dest.stored_owner_id(),
            "the hot sketch must follow its transferred Memory"
        );
        let dest_sketches =
            MemoryReadPort::load_sketches(&pg, &[dest], &[written.memory_id]).await?;
        assert_eq!(
            dest_sketches.len(),
            1,
            "the destination must read the moved sketch"
        );
        let notes: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.agent_note_v1 WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(notes, 1);
        let keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE ingest_key = 'k1'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(keys, 0);
        let replay = pg
            .transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
            .await?;
        assert!(
            !replay,
            "a series that already left this owner is a clean false"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("memory transfer failed");
}

#[tokio::test]
async fn transfer_rehomes_cooled_versions_and_remints_object_key() {
    let (db_name, pg, cold) = fresh_pg_with_cold().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        use proxima_storage_pg::verbs::forget::cold_object_key;

        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let mut first_draft = draft();
        first_draft.citation = Some(
            CitationSpec::v1(
                "core/test-cited-object-v1",
                [9_u8; 32],
                "core/test-citation-mapping-v1",
            )
            .into(),
        );
        let first = ingest_fact_atomic(pool, &permit, &first_draft, None).await?;
        let cited_object_id = first
            .cited_object_id
            .expect("citation-bearing write returns its object");
        let personal_content_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/test-fact-v1', $2)
             RETURNING content_id",
        )
        .bind(owner.stored_owner_id())
        .bind(vec![11_u8; 32])
        .fetch_one(pool)
        .await?;
        sqlx::query("UPDATE proxima_core.memory SET content_id = $2 WHERE t = $1")
            .bind(first.memory_id.into_inner())
            .bind(personal_content_id)
            .execute(pool)
            .await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, first.memory_id).await?;
        let mut later = draft();
        later.handle = Some(first.handle);
        later.ingest_key = Some("k2".into());
        let second = ingest_fact_atomic(pool, &permit, &later, None).await?;
        assert_eq!(second.handle, first.handle);

        let transferred = pg
            .transfer_to_owner(&permit, EntityId::Memory(second.memory_id), dest)
            .await?;
        assert!(transferred);
        let cooled_owner: uuid::Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.cooled WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            cooled_owner,
            dest.stored_owner_id(),
            "transfer must re-home cooled versions with the series"
        );
        let object_key: String =
            sqlx::query_scalar("SELECT object_key FROM proxima_core.cooled WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        let expected = cold_object_key(first.memory_id.into_inner());
        assert_eq!(object_key, expected);
        assert!(
            !object_key.contains(&owner.stored_owner_id().to_string())
                && !object_key.contains(&dest.stored_owner_id().to_string()),
            "the cold key is owner-free, so a transfer never has to re-mint it"
        );
        let blob_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(cited_object_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            blob_owner,
            dest.stored_owner_id(),
            "a cooled citation follows its transferred series"
        );
        let stale_ingest_keys: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE t = $1",
        )
        .bind(first.memory_id.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            stale_ingest_keys, 0,
            "transfer removes ingest keys for cooled versions too"
        );
        let cooled_content_owner: Uuid = sqlx::query_scalar(
            "SELECT c.owner_id
               FROM proxima_core.cooled cooled
               JOIN proxima_core.content c ON c.content_id = cooled.content_id
              WHERE cooled.t = $1",
        )
        .bind(first.memory_id.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            cooled_content_owner,
            dest.stored_owner_id(),
            "Content referenced only by a cooled version must follow it"
        );
        let personal_content_left: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.content WHERE content_id = $1",
        )
        .bind(personal_content_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            personal_content_left, 0,
            "exclusive cooled Content is GC'd after its destination remap"
        );

        let mut tx = pool.begin().await?;
        hydrate_memory(
            &mut tx,
            &core_pg_sidecars(),
            cold.as_ref(),
            first.memory_id.into_inner(),
        )
        .await?;
        tx.commit().await?;
        let hydrated_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            hydrated_owner,
            dest.stored_owner_id(),
            "the reminted cold record must hydrate under the destination"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("transfer cooled remint failed");
}

/// The transfer captures the old head, then blocks on the cooled-row UPDATE.
/// A same-handle ingest advances the head in that window. The transfer's CAS
/// must roll back that partial attempt, retry with the grown series, and move
/// every owner-scoped row together without adding a lock to normal ingest.
#[tokio::test]
async fn transfer_retries_when_ingest_advances_the_captured_head() {
    const PROBE_LOCK: i64 = 0x5052_4f58_5055_4238;

    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let first = ingest_fact_atomic(pool, &permit, &draft(), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, first.memory_id).await?;
        let mut second_draft = draft();
        second_draft.handle = Some(first.handle);
        second_draft.ingest_key = Some("k2".into());
        let second = ingest_fact_atomic(pool, &permit, &second_draft, None).await?;

        sqlx::raw_sql(
            "CREATE SEQUENCE public.transfer_race_probe;
             CREATE FUNCTION public.block_owner_transfer() RETURNS trigger
             LANGUAGE plpgsql AS $$
             BEGIN
                 PERFORM nextval('public.transfer_race_probe');
                 PERFORM pg_advisory_xact_lock(5787775711847989816);
                 RETURN NEW;
             END
             $$;
             CREATE TRIGGER block_owner_transfer
             BEFORE UPDATE OF owner_id ON proxima_core.cooled
             FOR EACH ROW
             WHEN (OLD.owner_id IS DISTINCT FROM NEW.owner_id)
             EXECUTE FUNCTION public.block_owner_transfer();",
        )
        .execute(pool)
        .await?;

        let mut blocker = pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(PROBE_LOCK)
            .execute(&mut *blocker)
            .await?;

        let transfer_pg = pg.clone();
        let transferred_id = second.memory_id;
        let transfer = tokio::spawn(async move {
            let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
            transfer_pg
                .transfer_to_owner(&permit, EntityId::Memory(transferred_id), dest)
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let reached: bool =
                    sqlx::query_scalar("SELECT is_called FROM public.transfer_race_probe")
                        .fetch_one(pool)
                        .await?;
                if reached {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;

        let mut third_draft = draft();
        third_draft.handle = Some(first.handle);
        third_draft.ingest_key = Some("k3".into());
        let third = ingest_fact_atomic(pool, &permit, &third_draft, None).await?;

        let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(PROBE_LOCK)
            .fetch_one(&mut *blocker)
            .await?;
        assert!(unlocked, "test must release the transfer probe lock");
        drop(blocker);

        let transferred = tokio::time::timeout(std::time::Duration::from_secs(5), transfer)
            .await??
            .map_err(|err| format!("transfer failed after retry: {err}"))?;
        assert!(transferred);

        let head: (Uuid, Uuid) =
            sqlx::query_as("SELECT t, owner_id FROM proxima_core.memory_head WHERE handle = $1")
                .bind(first.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head.0, third.memory_id.into_inner());
        assert_eq!(head.1, dest.stored_owner_id());
        let unmoved_versions: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM (
                     SELECT owner_id FROM proxima_core.memory WHERE handle = $1
                     UNION ALL
                     SELECT owner_id FROM proxima_core.cooled WHERE handle = $1
                    ) versions
              WHERE owner_id <> $2",
        )
        .bind(first.handle)
        .bind(dest.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(unmoved_versions, 0, "the retry must move the grown series");
        let stale_third_key: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys WHERE t = $1",
        )
        .bind(third.memory_id.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            stale_third_key, 0,
            "the late version's ingest key follows retry"
        );
        let third_sketch_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.sketch WHERE t = $1")
                .bind(third.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(third_sketch_owner, dest.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("transfer/ingest CAS retry failed");
}

/// A caller can resolve the Memory under its prior owner after the series head
/// has already moved elsewhere. Transfer must return clean `false` and leave
/// every hot/cold row and announce lane untouched. The head DELETE variant is
/// not statically constructible (`memory.handle` FK-references `memory_head`),
/// so the probe diverges the head owner, which `memory_head_t_only` admits.
#[tokio::test]
async fn transfer_is_unchanged_when_the_head_owner_is_already_lost() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        use proxima_storage_pg::verbs::forget::cold_object_key;

        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let first = ingest_fact_atomic(pool, &permit, &draft(), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, first.memory_id).await?;
        let mut later = draft();
        later.handle = Some(first.handle);
        later.ingest_key = Some("k2".into());
        let second = ingest_fact_atomic(pool, &permit, &later, None).await?;
        assert_eq!(second.handle, first.handle);
        let personal_key = cold_object_key(first.memory_id.into_inner());

        let usurper = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(usurper.stored_owner_id())
        .execute(pool)
        .await?;
        sqlx::query("UPDATE proxima_core.memory_head SET owner_id = $2 WHERE handle = $1")
            .bind(first.handle)
            .bind(usurper.stored_owner_id())
            .execute(pool)
            .await?;
        let announces_before: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.announce")
                .fetch_one(pool)
                .await?;

        let transferred = pg
            .transfer_to_owner(&permit, EntityId::Memory(second.memory_id), dest)
            .await?;
        assert!(
            !transferred,
            "a lost head is the clean not-transferred false, not an error"
        );

        let (cooled_owner, cooled_key): (Uuid, String) =
            sqlx::query_as("SELECT owner_id, object_key FROM proxima_core.cooled WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            cooled_owner,
            owner.stored_owner_id(),
            "the rolled-back transfer must not re-home the cooled row"
        );
        assert_eq!(
            cooled_key, personal_key,
            "the rolled-back transfer must keep the personal object key"
        );
        let hot_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(second.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(hot_owner, owner.stored_owner_id());
        let transfer_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.announce WHERE op = 'transfer'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(transfer_rows, 0, "no lane may announce a failed transfer");
        let announces_after: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.announce")
                .fetch_one(pool)
                .await?;
        assert_eq!(
            announces_after, announces_before,
            "a rolled-back transfer leaves the log exactly as it was"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("mid-transfer head loss rollback failed");
}

#[tokio::test]
async fn transfer_refuses_goal_entities() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let mut tx = pool.begin().await?;
        let out = write_goal(
            &mut tx,
            &owner,
            &GoalWriteCommand {
                handle: None,
                schema_id: "core/task-goal-v1".into(),
                title: "transfer me".into(),
                state: GoalState::Active,
                request_id: "pub-g".into(),
                close_fact_t: None,
                assignment_t: None,
                dependency_t: vec![],
                evidence_t: vec![],
                wake_id: None,
                mint_write_act: false,
                write_act_t: None,
            },
        )
        .await?;
        tx.commit().await?;

        let err = pg
            .transfer_to_owner(&permit, EntityId::Goal(GoalId::new(out.t)), dest)
            .await
            .expect_err("goals do not transfer");
        assert!(
            matches!(&err, StorageError::ConstraintViolation(msg)
                if msg.contains("goals do not transfer")),
            "refusal must be a ConstraintViolation naming the ruling: {err}"
        );
        let head_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.goal_head WHERE handle = $1")
                .bind(out.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head_owner, owner.stored_owner_id());
        let row_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.goal WHERE t = $1")
                .bind(out.t)
                .fetch_one(pool)
                .await?;
        assert_eq!(row_owner, owner.stored_owner_id());
        let transfer_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.announce WHERE op = 'transfer'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(transfer_rows, 0, "a refused transfer announces nothing");

        // DDL backstops behind the storage guard: goal rows refuse UPDATE
        // entirely, and `goal_head_t_only` freezes the head's owner_id.
        let frozen = sqlx::query("UPDATE proxima_core.goal SET owner_id = $2 WHERE t = $1")
            .bind(out.t)
            .bind(dest.stored_owner_id())
            .execute(pool)
            .await;
        assert!(frozen.is_err(), "goal must refuse UPDATE entirely");
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'group') ON CONFLICT DO NOTHING",
        )
        .bind(dest.stored_owner_id())
        .execute(pool)
        .await?;
        let head_frozen =
            sqlx::query("UPDATE proxima_core.goal_head SET owner_id = $2 WHERE handle = $1")
                .bind(out.handle)
                .bind(dest.stored_owner_id())
                .execute(pool)
                .await;
        assert!(
            head_frozen.is_err(),
            "goal_head_t_only must freeze the goal head's owner at the DDL layer"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("goal transfer refusal failed");
}

/// The erase-poison scenario: transferring an armed goal would strand its
/// `wake_config` row under the prior owner while the moved goal keeps arming
/// it, and that owner's compliance erase would abort forever on the
/// `ON DELETE RESTRICT` FK. Refusal keeps the wake row erasable.
#[tokio::test]
async fn transfer_refuses_armed_goal_and_owner_erase_still_succeeds() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Goal);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let mut tx = pool.begin().await?;
        let wake = insert_wake_config(
            &mut tx,
            &owner,
            &WakeConfigDraft {
                trigger_kind: WakeTriggerKind::FactSchema,
                trigger_schema_id: Some("core/test-fact-v1".into()),
                trigger_t: None,
                tool_ids: vec!["core.remember".into()],
                prompt: "private prompt".into(),
                hard_memory_t: vec![],
            },
        )
        .await?;
        let goal_handle =
            write_armed_goal(&mut tx, &owner, "armed goal", "pub-armed", wake).await?;
        tx.commit().await?;
        let goal_t: Uuid = sqlx::query_scalar("SELECT t FROM proxima_core.goal WHERE handle = $1")
            .bind(goal_handle)
            .fetch_one(pool)
            .await?;

        let err = pg
            .transfer_to_owner(&permit, EntityId::Goal(GoalId::new(goal_t)), dest)
            .await
            .expect_err("an armed goal is refused like any goal");
        assert!(matches!(err, StorageError::ConstraintViolation(_)));
        let wake_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.wake_config WHERE wake_id = $1")
                .bind(wake)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            wake_owner,
            owner.stored_owner_id(),
            "the wake row must stay with the goal's owner"
        );

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-armed-transfer".into(),
        });
        let outcome = pg
            .erase_personal_owner_if_drop_verified(&auth, user, false, &contract_sidecar_tables())
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("erase must complete after the refused transfer, got {outcome:?}");
        };
        assert_eq!(counts.wake_configs, 1, "the armed wake row is erased");
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.wake_config WHERE owner_id = $1",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(remaining, 0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("armed goal refusal / erase failed");
}

#[tokio::test]
async fn transfer_writes_announce_rows_under_both_lanes() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let written = ingest_fact_atomic(pool, &permit, &draft(), None).await?;
        let t = written.memory_id.into_inner();

        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
                .await?
        );

        // Raw rail: exactly one 'transfer' row per lane, same (handle, t).
        let rows: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT owner_id, handle, t FROM proxima_core.announce
              WHERE op = 'transfer' ORDER BY seq",
        )
        .fetch_all(pool)
        .await?;
        let lanes: Vec<Uuid> = rows.iter().map(|(owner_id, _, _)| *owner_id).collect();
        assert_eq!(
            rows.len(),
            2,
            "one row per lane: prior owner and destination owner"
        );
        assert!(lanes.contains(&owner.stored_owner_id()));
        assert!(lanes.contains(&dest.stored_owner_id()));
        for (_, handle, event_t) in &rows {
            assert_eq!(*handle, written.handle);
            assert_eq!(*event_t, t);
        }

        // The prior owner's earlier 'append' row is untouched.
        let (append_op, append_owner): (String, Uuid) =
            sqlx::query_as("SELECT op::text, owner_id FROM proxima_core.announce WHERE seq = $1")
                .bind(written.change_event_seq)
                .fetch_one(pool)
                .await?;
        assert_eq!(append_op, "append");
        assert_eq!(append_owner, owner.stored_owner_id());

        // The log stays append-only: the transfer wrote new rows, and no row
        // accepts UPDATE.
        let frozen =
            sqlx::query("UPDATE proxima_core.announce SET op = 'append' WHERE op = 'transfer'")
                .execute(pool)
                .await;
        assert!(frozen.is_err(), "announce rows must refuse UPDATE");

        // Hydrated rail, prior owner's lane: the transfer event is visible.
        let prior_lane = pg
            .list_change_events_after(std::slice::from_ref(&owner), Uuid::nil(), 100)
            .await?;
        let departed = prior_lane
            .iter()
            .find(|row| {
                matches!(
                    &row.event.kind,
                    ChangeEventKind::EntityTransfer { entity, .. }
                        if *entity == EntityRef::Memory(MemoryId::new(t))
                )
            })
            .expect("the prior owner's poll sees the transfer");
        assert_eq!(departed.event.owner, owner);

        // Destination lane: the arrival is visible under the new owner.
        let dest_lane = pg
            .list_change_events_after(&[dest], Uuid::nil(), 100)
            .await?;
        let arrived = dest_lane
            .iter()
            .find(|row| {
                matches!(
                    &row.event.kind,
                    ChangeEventKind::EntityTransfer { entity, .. }
                        if *entity == EntityRef::Memory(MemoryId::new(t))
                )
            })
            .expect("the destination's poll sees the arrival");
        assert_eq!(arrived.event.owner, dest);
        assert!(
            !dest_lane.iter().any(|row| matches!(
                row.event.kind,
                ChangeEventKind::EntityAppend { .. } | ChangeEventKind::EntityDelete { .. }
            )),
            "the append stays on the prior owner's lane"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("transfer announce rows failed");
}

#[tokio::test]
async fn transfer_moves_exclusive_blob_with_the_fact() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let seed = ingest_fact_atomic(pool, &permit, &draft(), None).await?;
        let _ = seed;
        let hash = vec![7u8; 32];
        let blob_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/bytes-v1', $2)
             RETURNING blob_id",
        )
        .bind(owner.stored_owner_id())
        .bind(&hash)
        .fetch_one(pool)
        .await?;

        let mut cited = draft();
        cited.ingest_key = Some("blob-k".into());
        cited.blob_id = Some(blob_id);
        let written = ingest_fact_atomic(pool, &permit, &cited, None).await?;
        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
                .await?
        );
        let blob_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(blob_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(blob_owner, dest.stored_owner_id());
        let stored_blob: Option<Uuid> =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(written.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(stored_blob, Some(blob_id));
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("blob transfer failed");
}

/// Retain-at-source for the audit sidecar, and what "retain" now means.
///
/// `mcp_call_logged_v1` describes the ACTOR of a tool call (`actor_upn`),
/// not the memory, and it carries its own `owner_id` stamped with the owner
/// that made the call. A transfer moves the memory and leaves the row
/// exactly where it is: it stays reachable by the source (history, export,
/// Art. 17 erase) and unreachable by the destination (its payload hydrate
/// joins the memory's owner to the row's). This replaced a DELETE, which
/// kept the destination out but destroyed history the source was entitled
/// to. Every other sidecar (here: `agent_note_v1`) still follows the memory.
#[tokio::test]
async fn transfer_leaves_the_actor_call_log_with_the_owner_that_made_the_call() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        // The Memory carries the mcp schema, so the payload hydrate really
        // dispatches to this sidecar. Written against `core/test-fact-v1`
        // it would dispatch elsewhere and the "destination sees nothing"
        // assertion below would hold vacuously.
        let written = ingest_mcp_call_fact(pool, &permit).await?;
        let t = written.memory_id.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
             VALUES ($1, $1, 'note', 'body')",
        )
        .bind(t)
        .execute(pool)
        .await?;

        let pinned_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.mcp_call_logged_v1 WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            pinned_owner,
            owner.stored_owner_id(),
            "the sidecar is stamped with the owner that made the call"
        );

        // The assertion that follows the transfer is only worth anything if
        // the payload hydrates in the first place.
        assert_eq!(
            hydrated_actor_payloads(&pg, owner, written.memory_id).await?,
            1,
            "the owner that made the call sees it in its own payload"
        );

        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
                .await?
        );

        let still_pinned: Vec<Uuid> =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.mcp_call_logged_v1 WHERE t = $1")
                .bind(t)
                .fetch_all(pool)
                .await?;
        assert_eq!(
            still_pinned,
            vec![owner.stored_owner_id()],
            "the transfer must neither destroy the audit row nor re-home it"
        );

        let notes: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.agent_note_v1 WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(notes, 1, "ordinary sidecars still follow the memory");
        let memory_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(t)
                .fetch_one(pool)
                .await?;
        assert_eq!(memory_owner, dest.stored_owner_id());

        // (a) The destination's payload hydrate excludes it. The sidecar
        // read joins the Memory's owner to the row's own, so the row is
        // invisible to whoever now holds the Memory.
        assert_eq!(
            hydrated_actor_payloads(&pg, dest, written.memory_id).await?,
            0,
            "the destination must not hydrate the prior owner's actor identity"
        );

        // (b) The source still answers "what did my agents do".
        let history = pg.read_mcp_call_history(&history_request(owner)).await?;
        assert_eq!(
            history.calls.len(),
            1,
            "the source keeps its own call history after giving the memory away"
        );
        assert_eq!(history.calls[0].tool_name, "core_remember");
        let destination_history = pg.read_mcp_call_history(&history_request(dest)).await?;
        assert!(
            destination_history.calls.is_empty(),
            "and the destination never inherits it"
        );

        // (d) Export follows the same owner: source in, destination out.
        let source_bundle = export_bundle(&pg, owner).await?;
        let exported = mcp_rows_in(&source_bundle);
        assert_eq!(
            exported.len(),
            1,
            "the source's Art. 15 bundle carries the calls it made"
        );
        // And carries them whole. A portability bundle that lists the right
        // number of rows and none of their fields is not a copy of anything;
        // `actor_upn` is the field this sidecar exists to retain, so it is
        // the one worth naming.
        assert_eq!(
            exported[0]
                .get("actor_upn")
                .and_then(serde_json::Value::as_str),
            Some("alice@example.test"),
            "the exported row is the row, not its primary key: {:?}",
            exported[0]
        );
        assert_eq!(
            exported[0]
                .get("tool_name")
                .and_then(serde_json::Value::as_str),
            Some("core_remember"),
        );
        let destination_bundle = export_bundle(&pg, dest).await?;
        assert!(
            mcp_rows_in(&destination_bundle).is_empty(),
            "the destination's bundle must not carry another owner's actor rows"
        );

        // (c) And the source can still erase them, which is the half a
        // de-registration would have lost: rows reachable by nobody are
        // rows Art. 17 cannot honour.
        let user_id = UserId::new(owner.stored_owner_id());
        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id,
            drop_event_id: "test-drop-retained-audit".into(),
        });
        let erased = pg
            .erase_personal_owner_if_drop_verified(
                &auth,
                user_id,
                false,
                &contract_sidecar_tables(),
            )
            .await?;
        assert!(
            matches!(erased, ComplianceEraseOutcome::Completed { .. }),
            "source erase completed: {erased:?}"
        );
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.mcp_call_logged_v1 WHERE t = $1",
        )
        .bind(t)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            left, 0,
            "the source's own erase reaches the rows it retained"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("audit sidecar retain-at-source failed");
}

/// The other half of "the row does not follow the Memory": nothing the
/// RECEIVING owner does to that Memory may reach the row.
///
/// Forget cools a Memory and deletes its sidecars into the cold object;
/// erase deletes the Memory outright. Both are the destination's to perform
/// after a transfer, and neither may destroy — or trip over — the source's
/// audit trail. Owner-pinned tables are therefore held out of the forget
/// dump entirely, and the row holds no foreign key into `memory`, so the
/// destination's erase does not fail on a child row it cannot see.
#[tokio::test]
async fn the_destination_can_forget_and_erase_without_touching_the_source_audit_trail() {
    let (db_name, pg, cold) = fresh_pg_with_cold().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();

        let first = ingest_mcp_call_fact(pool, &permit).await?;
        // A live head keeps the series transferable after the first version
        // is cooled.
        let mut later = mcp_draft();
        later.handle = Some(first.handle);
        later.ingest_key = Some("k2".into());
        let second = ingest_fact_atomic(pool, &permit, &later, None).await?;
        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(second.memory_id), dest)
                .await?
        );

        // The destination cools the version that carries the audit row.
        let dest_permit = OwnerWritePermit::new_for_tests(dest, AccessKind::Fact);
        MemoryAuthoringPort::forget_memory(&pg, &dest_permit, first.memory_id).await?;
        assert_eq!(
            pinned_owners(pool, first.memory_id.into_inner()).await?,
            vec![owner.stored_owner_id()],
            "a forget by the receiving owner must not cool away another owner's audit row"
        );

        // ...and hydrates it back. The row was never in the dump, so it is
        // not restored either — it never left.
        let mut tx = pool.begin().await?;
        hydrate_memory(
            &mut tx,
            &core_pg_sidecars(),
            cold.as_ref(),
            first.memory_id.into_inner(),
        )
        .await?;
        tx.commit().await?;
        assert_eq!(
            pinned_owners(pool, first.memory_id.into_inner()).await?,
            vec![owner.stored_owner_id()],
            "and the hydrate must not duplicate or re-home it"
        );
        assert_eq!(
            hydrated_actor_payloads(&pg, dest, first.memory_id).await?,
            0,
            "the destination still cannot see the prior owner's actor identity"
        );

        // Now the destination erases its whole owner. The Memory goes; the
        // source's audit row stays, and the erase does not fail on it.
        let group_id = match dest {
            OwnerRef::Group(group_id) => group_id,
            OwnerRef::Personal(_) => panic!("a transfer destination is a group"),
        };
        let auth =
            EraseAuthorization::new_for_tests(ComplianceEraseTarget::GroupOwner { group_id });
        let erased = pg
            .erase_group_owner_if_abandoned(&auth, group_id, false, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(erased, ComplianceEraseOutcome::Completed { .. }),
            "the destination's erase completes over a Memory whose audit row it does not own: {erased:?}"
        );
        let memories_left: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory WHERE handle = $1",
        )
        .bind(first.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(memories_left, 0, "the destination's memories are gone");
        assert_eq!(
            pinned_owners(pool, first.memory_id.into_inner()).await?,
            vec![owner.stored_owner_id()],
            "the source's audit row outlives the Memory it describes"
        );
        let history = pg.read_mcp_call_history(&history_request(owner)).await?;
        assert_eq!(
            history.calls.len(),
            1,
            "and the source can still read it back"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("destination lifecycle must not reach the source audit trail");
}

/// The FK safety that used to lean on World's migration-seeded `owners` row.
/// The destination here has never been written to, so the transfer transaction
/// must mint its `owners` row before any `owner_id` FK binds — including the
/// destination's own announce lane.
#[tokio::test]
async fn transfer_mints_the_destination_owner_row_in_the_same_transaction() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let written = ingest_fact_atomic(pool, &permit, &draft(), None).await?;

        let before: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.owners WHERE owner_id = $1",
        )
        .bind(dest.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(before, 0, "the destination owner row must not pre-exist");

        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
                .await?
        );

        let kind: String =
            sqlx::query_scalar("SELECT kind::text FROM proxima_core.owners WHERE owner_id = $1")
                .bind(dest.stored_owner_id())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            kind, "group",
            "the transfer mints the destination owner row"
        );
        let dest_announce: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.announce
              WHERE op = 'transfer' AND owner_id = $1",
        )
        .bind(dest.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(dest_announce, 1, "the destination lane's FK holds");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("destination owner row minting failed");
}

/// A transfer to the current owner is a no-op the storage layer refuses
/// outright rather than announcing a self-transfer.
#[tokio::test]
async fn transfer_refuses_the_current_owner_as_destination() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let written = ingest_fact_atomic(pool, &permit, &draft(), None).await?;

        let err = pg
            .transfer_to_owner(&permit, EntityId::Memory(written.memory_id), owner)
            .await
            .expect_err("a self-transfer is refused");
        assert!(matches!(err, StorageError::ConstraintViolation(_)));
        let transfer_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.announce WHERE op = 'transfer'",
        )
        .fetch_one(pool)
        .await?;
        assert_eq!(transfer_rows, 0, "a refused transfer announces nothing");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("self-transfer refusal failed");
}

/// Seed a completed cited blob under `owner`, returning `(blob_id, upload_id)`.
///
/// Written the way the upload lane writes it: the upload row's
/// `object_key` is the key derived from its own `upload_id`, because that
/// derivation is the whole read gate. A fixture that invented a key would
/// pass tests the production reader would refuse.
async fn seed_cited_blob(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    hash: &[u8],
) -> Result<(Uuid, Uuid), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
    )
    .bind(owner.stored_owner_id())
    .bind(proxima_core::OwnerRefKind::of(&owner).as_str())
    .execute(pool)
    .await?;
    let blob_id: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/uploaded-blob-v1', $2)
         RETURNING blob_id",
    )
    .bind(owner.stored_owner_id())
    .bind(hash)
    .fetch_one(pool)
    .await?;
    let upload_id: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob_uploads
             (owner_id, bucket, object_key, filename, mime, expected_byte_len,
              status, blob_id, sha256, expires_at, completed_at)
         VALUES ($1, 'bucket', 'placeholder', 'f.bin', 'application/octet-stream', 3,
                 'completed', $2, $3, now() + interval '1 day', now())
         RETURNING upload_id",
    )
    .bind(owner.stored_owner_id())
    .bind(blob_id)
    .bind(hash)
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "UPDATE proxima_core.blob_uploads
            SET object_key = 'objects/' || upload_id::text
          WHERE upload_id = $1",
    )
    .bind(upload_id)
    .execute(pool)
    .await?;
    Ok((blob_id, upload_id))
}

async fn cite(
    pool: &sqlx::PgPool,
    permit: &OwnerWritePermit,
    key: &str,
    blob_id: Uuid,
) -> Result<FactIngestOutcome, StorageError> {
    let mut cited = draft();
    cited.ingest_key = Some(key.into());
    cited.blob_id = Some(blob_id);
    ingest_fact_atomic(pool, permit, &cited, None).await
}

/// The refusal that the dedupe arm replaced.
///
/// One owner cites the same uploaded document from two series and then
/// transfers one of them away. Before the arm this was
/// `Conflict("cited blob is still referenced by another live series under
/// a different owner")` — and note what the predicate behind that message
/// actually asked: `owner_id <> <destination>`, which the SOURCE owner
/// satisfies. So the refusal fired for an owner citing its own document
/// twice, which is not an exotic case at all; it is what happens the
/// second time anyone cites a PDF they already uploaded.
///
/// Now the destination gets a `blob` row of its own and an upload row that
/// MOUNTS the source's object: same key, no bytes read or written. The
/// source keeps everything it had, because its other series still cites
/// it.
#[tokio::test]
async fn a_shared_blob_transfer_dedupes_instead_of_refusing() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();

        let hash = vec![11_u8; 32];
        let (blob_id, upload_id) = seed_cited_blob(pool, owner, &hash).await?;
        let source_key: String = sqlx::query_scalar(
            "SELECT object_key FROM proxima_core.blob_uploads WHERE upload_id = $1",
        )
        .bind(upload_id)
        .fetch_one(pool)
        .await?;

        let mine = cite(pool, &permit, "shared-mine", blob_id).await?;
        // A second series cites the very same blob row. This is what used
        // to make the transfer below impossible. It stays behind, which is
        // why the row cannot simply change hands.
        let _also_mine = cite(pool, &permit, "shared-also-mine", blob_id).await?;

        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(mine.memory_id), dest)
                .await?,
            "a shared cited blob must no longer refuse the transfer"
        );

        let moved_to: Uuid =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(mine.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_ne!(
            moved_to, blob_id,
            "the destination gets a row of its own, not the source's"
        );
        let (dest_owner, dest_schema, dest_hash): (Uuid, String, Vec<u8>) = sqlx::query_as(
            "SELECT owner_id, schema_id, content_hash FROM proxima_core.blob WHERE blob_id = $1",
        )
        .bind(moved_to)
        .fetch_one(pool)
        .await?;
        assert_eq!(dest_owner, dest.stored_owner_id());
        assert_eq!(dest_schema, "core/uploaded-blob-v1");
        assert_eq!(dest_hash, hash, "the dedupe key is the content hash");

        // The mount: one new upload row, naming the SOURCE's object.
        let (mounted_key, mounted_from, mounted_owner): (String, Option<Uuid>, Uuid) =
            sqlx::query_as(
                "SELECT object_key, mounted_from_upload_id, owner_id
                   FROM proxima_core.blob_uploads WHERE blob_id = $1",
            )
            .bind(moved_to)
            .fetch_one(pool)
            .await?;
        assert_eq!(mounted_owner, dest.stored_owner_id());
        assert_eq!(
            mounted_key, source_key,
            "the mount names the object that already exists; nothing is copied"
        );
        assert_eq!(
            mounted_from,
            Some(upload_id),
            "the mount records which row minted the object it reads"
        );

        // The source is untouched: the other owner still cites it.
        let source_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(blob_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            source_owner,
            owner.stored_owner_id(),
            "a still-cited source row does not change hands"
        );
        let orphaned: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory
              WHERE blob_id = $1 AND owner_id <> $2",
        )
        .bind(blob_id)
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            orphaned, 0,
            "no memory may be left citing a blob row owned by someone else \
             (Causa/Citations.lean: memory_cites m b -> memory_owner m = blob_owner b)"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("shared blob dedupe failed");
}

/// The common case does not pay for the uncommon one.
///
/// Nothing else cites the bytes, so the rows change hands in place: same
/// `blob_id`, same `upload_id`, no mount. The `blob_id` matters because it
/// is the `cited_object_id` a client reads by — minting a new one on every
/// transfer would invalidate citation ids for no reason.
#[tokio::test]
async fn an_unshared_blob_still_moves_in_place_with_no_mount() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();

        let (blob_id, upload_id) = seed_cited_blob(pool, owner, &[12_u8; 32]).await?;
        let written = cite(pool, &permit, "solo", blob_id).await?;
        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
                .await?
        );

        let (blob_owner, blob_count): (Uuid, i64) = sqlx::query_as(
            "SELECT owner_id, (SELECT count(*)::bigint FROM proxima_core.blob)
               FROM proxima_core.blob WHERE blob_id = $1",
        )
        .bind(blob_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(blob_owner, dest.stored_owner_id());
        assert_eq!(blob_count, 1, "moving in place mints no second row");

        let (upload_owner, mounted_from): (Uuid, Option<Uuid>) = sqlx::query_as(
            "SELECT owner_id, mounted_from_upload_id
               FROM proxima_core.blob_uploads WHERE upload_id = $1",
        )
        .bind(upload_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(upload_owner, dest.stored_owner_id());
        assert_eq!(mounted_from, None, "a move is not a mount");

        let cited: Option<Uuid> =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(written.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            cited,
            Some(blob_id),
            "the citation id a client already holds does not move"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("unshared blob move failed");
}

/// The UNIQUE violation the arm also closes.
///
/// `blob` is unique on `(owner_id, schema_id, content_hash)`. If the
/// destination already uploaded the same bytes, the old in-place move
/// `UPDATE blob SET owner_id = <dest>` collided with the row already
/// sitting there and the whole transfer failed on a constraint the caller
/// could do nothing about. Now the destination's own row wins and this
/// series' citation is repointed at it.
#[tokio::test]
async fn a_destination_that_already_holds_the_bytes_keeps_its_own_row() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();

        let hash = vec![13_u8; 32];
        let (source_blob, _) = seed_cited_blob(pool, owner, &hash).await?;
        let (dest_blob, dest_upload) = seed_cited_blob(pool, dest, &hash).await?;

        let written = cite(pool, &permit, "collide", source_blob).await?;
        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(written.memory_id), dest)
                .await?,
            "the destination already holding these bytes is not a conflict"
        );

        let cited: Option<Uuid> =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(written.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            cited,
            Some(dest_blob),
            "the citation follows the destination's existing row"
        );
        let mounted_from: Option<Uuid> = sqlx::query_scalar(
            "SELECT mounted_from_upload_id FROM proxima_core.blob_uploads WHERE upload_id = $1",
        )
        .bind(dest_upload)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            mounted_from, None,
            "the destination already had its own object; there is nothing to mount"
        );
        let source_still_there: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM proxima_core.blob WHERE blob_id = $1)",
        )
        .bind(source_blob)
        .fetch_one(pool)
        .await?;
        assert!(
            source_still_there,
            "the transfer does not decide the fate of the source's object; erase does"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("destination-holds-bytes transfer failed");
}

/// A mount of a mount still resolves to the row that uploaded bytes.
///
/// B mounts A's object. C then transfers from B. If C recorded B's
/// `upload_id` it would derive a key for an object that never existed —
/// B never uploaded anything. The mount column therefore always carries
/// the MINTING id, and the chain stays one hop deep however long it gets.
#[tokio::test]
async fn a_mount_of_a_mount_still_names_the_object_that_was_uploaded() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let first = destination();
        let second = destination();
        let pool = pg.pool_for_tests();

        let hash = vec![14_u8; 32];
        let (blob_id, minting_upload) = seed_cited_blob(pool, owner, &hash).await?;
        let mine = cite(pool, &permit, "chain-mine", blob_id).await?;
        let _also_mine = cite(pool, &permit, "chain-also-mine", blob_id).await?;

        // Hop one: shared, so the destination mounts.
        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(mine.memory_id), first)
                .await?
        );
        let hop_one: Uuid =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(mine.memory_id.into_inner())
                .fetch_one(pool)
                .await?;

        // Make hop one's row shared too, so hop two mounts rather than moves.
        let first_permit = OwnerWritePermit::new_for_tests(first, AccessKind::Fact);
        let shadow = cite(pool, &first_permit, "chain-shadow", hop_one).await?;
        let _ = shadow;
        assert!(
            pg.transfer_to_owner(&first_permit, EntityId::Memory(mine.memory_id), second)
                .await?
        );

        let hop_two: Uuid =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(mine.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        let (key, mounted_from): (String, Option<Uuid>) = sqlx::query_as(
            "SELECT object_key, mounted_from_upload_id
               FROM proxima_core.blob_uploads WHERE blob_id = $1",
        )
        .bind(hop_two)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            mounted_from,
            Some(minting_upload),
            "the second mount records the row that uploaded, not the row it copied from"
        );
        assert_eq!(
            key,
            format!("objects/{minting_upload}"),
            "and therefore names the one object that exists"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("chained mount failed");
}

/// Erasing one owner of a mounted object must not destroy the other's bytes.
///
/// The dedupe arm made the object many-to-one against the rows that name
/// it, which invalidated the invariant `delete_blobs` relied on: "the row
/// is going, so the object is going". This is that invalidation, tested
/// from both sides — the erase that must leave the bytes alone, and the
/// later erase that must destroy them.
///
/// It asserts on the OBJECT, not on `cold_purge_pending`. The pending row
/// is a crash-recovery mark that `finalize_cold_purge` clears as soon as
/// the destruction succeeds, so a test that counted queue rows would read
/// zero in both the "withheld" and the "destroyed" case and pass for the
/// wrong reason in one of them.
#[tokio::test]
async fn erasing_one_owner_of_a_mounted_object_does_not_destroy_the_bytes() {
    let (db_name, pg, cold) = fresh_pg_with_cold().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();

        let (blob_id, upload_id) = seed_cited_blob(pool, owner, &[15_u8; 32]).await?;
        let object_key = format!("objects/{upload_id}");
        cold.put(&object_key, b"the shared bytes").await?;

        let mine = cite(pool, &permit, "purge-mine", blob_id).await?;
        let _also_mine = cite(pool, &permit, "purge-also-mine", blob_id).await?;
        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(mine.memory_id), dest)
                .await?
        );
        let mounts: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.blob_uploads WHERE object_key = $1",
        )
        .bind(&object_key)
        .fetch_one(pool)
        .await?;
        assert_eq!(mounts, 2, "two rows now name one object");

        // The destination erases. Its row goes; the bytes must not,
        // because the original owner still reads them.
        let group_id = match dest {
            OwnerRef::Group(group_id) => group_id,
            OwnerRef::Personal(_) => panic!("a transfer destination is a group"),
        };
        let auth =
            EraseAuthorization::new_for_tests(ComplianceEraseTarget::GroupOwner { group_id });
        let erased = pg
            .erase_group_owner_if_abandoned(&auth, group_id, false, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(erased, ComplianceEraseOutcome::Completed { .. }),
            "the destination's erase completes: {erased:?}"
        );
        assert_eq!(
            cold.get(&object_key).await?,
            b"the shared bytes".to_vec(),
            "an object another owner still names survives that owner's erase"
        );
        let survivors: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.blob_uploads WHERE object_key = $1",
        )
        .bind(&object_key)
        .fetch_one(pool)
        .await?;
        assert_eq!(survivors, 1, "the source's row survives the other's erase");

        // Now the last owner goes, and the bytes go with it.
        let user_id = match owner {
            OwnerRef::Personal(user_id) => user_id,
            OwnerRef::Group(_) => panic!("seeded as a personal owner"),
        };
        let last = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id,
            drop_event_id: "drop-1".into(),
        });
        let erased = pg
            .erase_personal_owner_if_drop_verified(
                &last,
                user_id,
                false,
                &contract_sidecar_tables(),
            )
            .await?;
        assert!(
            matches!(erased, ComplianceEraseOutcome::Completed { .. }),
            "the last owner's erase completes: {erased:?}"
        );
        assert!(
            matches!(
                cold.get(&object_key).await,
                Err(proxima_core::StorageError::NotFound)
            ),
            "with no row left naming it, the object is finally destroyed"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("refcounted object purge failed");
}
