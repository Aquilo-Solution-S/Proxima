//! An owner-to-owner transfer is an in-place series transfer.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use std::sync::Arc;

use proxima_core::FactPayload;
use proxima_core::owner_inverse::{
    EraseAuthorization, ExportAuthorization, OwnerEraseOutcome, OwnerEraseTarget,
    OwnerExportTarget, OwnerSurfaces,
};
use proxima_core::read_models::MemorySchemaSpec;
use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::MemoryAuthoringPort;
use proxima_core::storage_ports::{
    ChangeEventPort, McpCallReadPort, MemoryInspectPort, MemoryReadPort, OwnerInversePort,
    OwnerTransferPort, OwnerWritePermit,
};
use proxima_core::verbs::fact_ingest::{
    AuthorizedFactWrite, CitationSpec, FactIngestOutcome, FactWriteCommand,
};
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::mcp_call_history::McpCallHistoryRequest;
use proxima_core::verbs::persist_mcp_call::McpCallLoggedV1;
use proxima_core::verbs::query::QueryRequest;
use proxima_core::{
    AccessKind, AgentNoteV1, AuthPath, AuthzContext, ChangeEventKind, ColdObjectStore, Engine,
    EntityId, EntityRef, FlavorRegistry, GoalId, GroupId, MemoryId, OwnerRef, SchemaId,
    SchemaVersion, SidecarPayload, StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::verbs::forget::{MemoryColdStore, erase_memory_series};
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, write_goal};
use proxima_storage_pg::verbs::wake_timeseries::{
    WakeConfigDraft, WakeTriggerKind, insert_wake_config, write_armed_goal,
};
use proxima_storage_pg::{PgStorage, core_pg_sidecars};
use uuid::Uuid;

fn memory_schema_specs() -> Vec<MemorySchemaSpec> {
    proxima_core::FlavorRegistry::new()
        .freeze_or_panic_for_tests()
        .schemas()
        .iter()
        .filter_map(|schema| {
            let kind = match schema.kind {
                proxima_core::verbs::schema::PayloadKind::Fact => proxima_core::EntityKind::Fact,
                proxima_core::verbs::schema::PayloadKind::Abstraction => {
                    proxima_core::EntityKind::Abstraction
                }
                proxima_core::verbs::schema::PayloadKind::Perspective => {
                    proxima_core::EntityKind::Perspective
                }
                _ => return None,
            };
            Some(MemorySchemaSpec {
                kind,
                schema_id: schema.schema_id.clone(),
                schema_version: schema.schema_version,
                sidecar_table: schema.sidecar_table.clone(),
            })
        })
        .collect()
}

/// The five sidecar legs exactly as the engine assembles them: from the
/// frozen flavor registry. Passing empty slices here would silently skip
/// the owner-pinned leg, which is the difference these tests exist to
/// measure.
fn contract_sidecar_tables() -> OwnerSurfaces {
    OwnerSurfaces::for_registry(&proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests())
}

/// Observe one specific advisory waiter/holder, including its mode and lock
/// key. Counting all advisory waiters is racy when a test has more than one
/// transaction in flight; matching PostgreSQL's two-word advisory-key view
/// makes the rendezvous prove the lock vocabulary under test.
async fn owner_fence_lock_state(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    mode: &str,
    granted: bool,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_locks
              WHERE locktype = 'advisory'
                AND granted = $3
                AND mode = $4
                AND classid::bigint = ((hashtextextended(
                    'proxima-owner-fence:' || $1 || ':' || $2::text, 0
                ) >> 32) & 4294967295)
                AND objid::bigint = (hashtextextended(
                    'proxima-owner-fence:' || $1 || ':' || $2::text, 0
                ) & 4294967295)
         )",
    )
    .bind(match owner {
        OwnerRef::Personal(_) => "personal",
        OwnerRef::Group(_) => "group",
    })
    .bind(owner.stored_owner_id())
    .bind(granted)
    .bind(mode)
    .fetch_one(pool)
    .await
}

async fn handle_lock_held(pool: &sqlx::PgPool, handle: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_locks
              WHERE locktype = 'advisory'
                AND granted
                AND mode = 'ExclusiveLock'
                AND classid::bigint = ((hashtextextended(
                    'proxima-memory-handle:' || $1::text, 0
                ) >> 32) & 4294967295)
                AND objid::bigint = (hashtextextended(
                    'proxima-memory-handle:' || $1::text, 0
                ) & 4294967295)
         )",
    )
    .bind(handle)
    .fetch_one(pool)
    .await
}

async fn owner_fence_waiter_count(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    mode: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM pg_locks
          WHERE locktype = 'advisory'
            AND NOT granted
            AND mode = $3
            AND classid::bigint = ((hashtextextended(
                'proxima-owner-fence:' || $1 || ':' || $2::text, 0
            ) >> 32) & 4294967295)
            AND objid::bigint = (hashtextextended(
                'proxima-owner-fence:' || $1 || ':' || $2::text, 0
            ) & 4294967295)",
    )
    .bind(match owner {
        OwnerRef::Personal(_) => "personal",
        OwnerRef::Group(_) => "group",
    })
    .bind(owner.stored_owner_id())
    .bind(mode)
    .fetch_one(pool)
    .await
}

async fn lifecycle_waiter_count(pool: &sqlx::PgPool, t: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM pg_locks
          WHERE locktype = 'advisory'
            AND NOT granted
            AND mode = 'ExclusiveLock'
            AND classid::bigint = ((hashtextextended(
                'proxima-forget:' || $1::text, 0
            ) >> 32) & 4294967295)
            AND objid::bigint = (hashtextextended(
                'proxima-forget:' || $1::text, 0
            ) & 4294967295)",
    )
    .bind(t)
    .fetch_one(pool)
    .await
}

/// Sessions queued behind one object key's lifecycle lock.
///
/// The refcount fence: every taker that is about to decide whether an upload
/// object's bytes may go queues here first, so the decision reads a set of
/// referencing rows nobody else is still changing.
async fn object_key_waiter_count(
    pool: &sqlx::PgPool,
    object_key: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM pg_locks
          WHERE locktype = 'advisory'
            AND NOT granted
            AND mode = 'ExclusiveLock'
            AND classid::bigint = ((hashtextextended(
                'proxima-object-key:' || $1, 0
            ) >> 32) & 4294967295)
            AND objid::bigint = (hashtextextended(
                'proxima-object-key:' || $1, 0
            ) & 4294967295)",
    )
    .bind(object_key)
    .fetch_one(pool)
    .await
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
fn note_payload(title: &str, body: &str) -> SidecarPayload {
    SidecarPayload::fact(AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: title.to_owned(),
        body: body.to_owned(),
        tags: Vec::new(),
        idempotency_key: None,
    })
}

async fn ingest_mcp_call_fact(
    pg: &PgStorage,
    permit: &OwnerWritePermit,
    extra: &[SidecarPayload],
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
    let mut draft =
        FactWriteCommand::from_payload("proxima/fact", &payload, time::OffsetDateTime::now_utc());
    // `extra` may carry a `LanguagePolicy::PerRow` schema; the mcp sidecar
    // itself pins its configuration and reads no language bind.
    draft.lexical_language =
        Some(proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT.to_owned());
    let authorized = AuthorizedFactWrite::new_for_tests(
        OwnerWritePermit::new_for_tests(*permit.owner(), AccessKind::Fact),
        draft,
        McpCallLoggedV1::sidecar_table().map(str::to_owned),
        Vec::new(),
    );
    let mut payloads = vec![SidecarPayload::fact(payload)];
    payloads.extend_from_slice(extra);
    pg.ingest_fact_with_typed_sidecar(&authorized, &payloads, None)
        .await
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
    let schemas = memory_schema_specs();
    let response = pg
        .query_memories(
            &QueryRequest {
                memory_ids: vec![memory_id],
                include_payloads: true,
                ..QueryRequest::for_owner(owner)
            },
            &schemas,
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

async fn assert_owner_pinned_payload_redacted(
    pg: &PgStorage,
    owner: OwnerRef,
    memory_id: MemoryId,
) -> Result<(), StorageError> {
    let snapshots = pg
        .load_memories_by_ids(&[owner], &[memory_id], &memory_schema_specs())
        .await?;
    assert_eq!(
        snapshots.len(),
        1,
        "the transferred Memory remains visible when its owner-pinned payload stays behind"
    );
    let snapshot = &snapshots[0];
    assert_eq!(snapshot.memory_id, memory_id);
    assert_eq!(snapshot.owner, owner);
    assert_eq!(
        snapshot.schema_version.into_inner(),
        McpCallLoggedV1::SCHEMA_VERSION
    );
    assert!(snapshot.payload.is_none());
    assert!(snapshot.text.is_none());
    Ok(())
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
) -> Result<proxima_core::owner_inverse::OwnerExportBundle, StorageError> {
    let target = match owner {
        OwnerRef::Personal(user_id) => OwnerExportTarget::PersonalOwner { user_id },
        OwnerRef::Group(group_id) => OwnerExportTarget::GroupOwner { group_id },
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
/// tell a row from a scalar: `to_jsonb(t)` resolves to the column named `t`,
/// not the range table, when only one table is in scope, so an export emitting
/// the primary key where the row belongs has the right cardinality and a bundle
/// empty of content.
fn mcp_rows_in(bundle: &proxima_core::owner_inverse::OwnerExportBundle) -> Vec<&serde_json::Value> {
    bundle
        .table("proxima_core.mcp_call_logged_v1")
        .iter()
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

/// A cold store that records every destruction it is asked for.
///
/// The count is the only witness that separates "both owners withheld" from
/// "one owner destroyed": `cold_purge_pending` reads empty in both cases,
/// because a discharged debt deletes its own row.
#[derive(Debug, Default)]
struct CountingColdStore {
    inner: MemoryColdStore,
    deletes: std::sync::Mutex<Vec<String>>,
}

impl CountingColdStore {
    fn deletes_of(&self, key: &str) -> usize {
        self.deletes
            .lock()
            .expect("delete log")
            .iter()
            .filter(|logged| logged.as_str() == key)
            .count()
    }
}

#[async_trait::async_trait]
impl ColdObjectStore for CountingColdStore {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        self.inner.put(key, bytes).await
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.inner.get(key).await
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.deletes
            .lock()
            .expect("delete log")
            .push(key.to_owned());
        self.inner.delete(key).await
    }
}

async fn fresh_pg_with_counting_cold() -> (String, PgStorage, Arc<CountingColdStore>) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let cold = Arc::new(CountingColdStore::default());
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect")
        .with_cold(cold.clone());
    pg.run_migrations().await.expect("migrate");
    (db_name, pg, cold)
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
        // The typed write, not `ingest_fact_atomic` plus a hand-written row:
        // only a write that DECLARES `agent_note_v1` leaves a note row the
        // transfer's sidecar leg can reach.
        let mut fact = draft();
        // `agent-note-v1` is `LanguagePolicy::PerRow`: the write names a
        // language.
        fact.lexical_language =
            Some(proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT.to_owned());
        let authorized = AuthorizedFactWrite::new_for_tests(
            OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            fact,
            AgentNoteV1::sidecar_table().map(str::to_owned),
            Vec::new(),
        );
        let written = pg
            .ingest_fact_with_typed_sidecar(&authorized, &[note_payload("pub", "body")], None)
            .await?;
        let t = written.memory_id.into_inner();
        let witness_before: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(t)
        .fetch_optional(pool)
        .await?;
        assert_eq!(
            witness_before, None,
            "a transfer must not create a hard-erase witness"
        );
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
            .transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                dest,
                &contract_sidecar_tables(),
            )
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
        let witness_after: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(t)
        .fetch_optional(pool)
        .await?;
        assert_eq!(
            witness_after, None,
            "a committed transfer must not create a hard-erase witness"
        );
        let replay = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                dest,
                &contract_sidecar_tables(),
            )
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
    let (db_name, pg, _cold) = fresh_pg_with_cold().await;
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
        let first = pg.ingest_fact_atomic(&permit, &first_draft, None).await?;
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
        // The cold dump's `embed_models` list is read off
        // `proxima_core.embeddings` at cooling time, so this series has to
        // carry an embedding BEFORE it is forgotten, or the hydrate below
        // files no job at all and the owner assertion on it is vacuous.
        sqlx::query(
            "INSERT INTO proxima_core.embeddings (entity_id, model_id, vec, owner_id)
             VALUES ($1, 'transfer-hydrate-model',
                     ('[' || array_to_string(array_fill(0::real, ARRAY[1024]), ',') || ']')::vector,
                     $2)",
        )
        .bind(first.memory_id.into_inner())
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, first.memory_id).await?;
        let mut later = draft();
        later.handle = Some(first.handle);
        later.ingest_key = Some("k2".into());
        let second = pg.ingest_fact_atomic(&permit, &later, None).await?;
        assert_eq!(second.handle, first.handle);

        let transferred = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(second.memory_id),
                dest,
                &contract_sidecar_tables(),
            )
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
        for witnessed_t in [first.memory_id.into_inner(), second.memory_id.into_inner()] {
            let witness: Option<String> = sqlx::query_scalar(
                "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
            )
            .bind(witnessed_t)
            .fetch_optional(pool)
            .await?;
            assert_eq!(
                witness, None,
                "transferring a cooled series must not create an erase witness"
            );
        }
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

        // Announce is append-only and the earlier ingest already wrote rows
        // for this `t` under the giver, so the assertion has to be scoped to
        // what THIS hydrate appends. `seq` is a uuidv7 PK; the pre-hydrate
        // set is the watermark.
        let announce_before: Vec<Uuid> =
            sqlx::query_scalar("SELECT seq FROM proxima_core.announce")
                .fetch_all(pool)
                .await?;

        let dest_permit = OwnerWritePermit::new_for_tests(dest, AccessKind::Fact);
        let hydrated =
            MemoryAuthoringPort::hydrate_memories(&pg, &dest_permit, &[first.memory_id]).await?;
        assert_eq!(
            hydrated.outcomes[0].status,
            proxima_core::MemoryHydrationStatus::Hydrated
        );
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

        // The `memory` row is right because the INSERT reads the owner off
        // `cooled`. Everything ELSE the hydrate writes must too: the cold
        // record's embedded `row.owner_id` is never rewritten by a transfer, so
        // the bytes keep naming the giver forever. Each of these three surfaces
        // is owner-scoped on read, so a giver-owned row here is a memory the
        // giver gave away and can still reach.
        let sketch_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.sketch WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            sketch_owner,
            dest.stored_owner_id(),
            "the sketch is read owner-scoped: hydrating it under the giver \
             hands a transferred memory back to the owner that gave it away"
        );

        let hydrate_announce_owners: Vec<Uuid> = sqlx::query_scalar(
            "SELECT owner_id FROM proxima_core.announce
              WHERE t = $1 AND seq <> ALL($2::uuid[])",
        )
        .bind(first.memory_id.into_inner())
        .bind(&announce_before)
        .fetch_all(pool)
        .await?;
        assert!(
            !hydrate_announce_owners.is_empty(),
            "the hydrate announces, or this assertion proves nothing"
        );
        for announced in &hydrate_announce_owners {
            assert_eq!(
                *announced,
                dest.stored_owner_id(),
                "the hydrate announces to the owner that HAS the memory now"
            );
        }

        let embed_job_owners: Vec<Uuid> = sqlx::query_scalar(
            "SELECT owner_id FROM proxima_core.embedding_jobs WHERE entity_id = $1",
        )
        .bind(first.memory_id.into_inner())
        .fetch_all(pool)
        .await?;
        assert_eq!(
            embed_job_owners,
            vec![dest.stored_owner_id()],
            "the re-embed job is the destination's work; filing it under the \
             giver also breaks hydrate outright once the giver is erased, \
             because the owner-kind lookup is a fetch_one"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("transfer cooled remint failed");
}

/// A caller may retain only an older cooled admission id while the series is
/// still live. The cooled row supplies the handle, but the transfer must still
/// move the hot head through its normal owner compare-and-set.
#[tokio::test]
async fn transfer_accepts_a_cooled_input_for_a_live_series() {
    let (db_name, pg, _cold) = fresh_pg_with_cold().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let first = pg.ingest_fact_atomic(&permit, &draft(), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, first.memory_id).await?;

        let mut later = draft();
        later.handle = Some(first.handle);
        later.ingest_key = Some("cooled-input-live-head".into());
        let second = pg.ingest_fact_atomic(&permit, &later, None).await?;

        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(first.memory_id),
                dest,
                &contract_sidecar_tables(),
            )
            .await?
        );

        let cooled_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.cooled WHERE t = $1")
                .bind(first.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(cooled_owner, dest.stored_owner_id());
        let hot_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(second.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(hot_owner, dest.stored_owner_id());
        let head: (Uuid, Uuid) =
            sqlx::query_as("SELECT t, owner_id FROM proxima_core.memory_head WHERE handle = $1")
                .bind(first.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            head,
            (second.memory_id.into_inner(), dest.stored_owner_id())
        );

        let announced_t: Vec<Uuid> = sqlx::query_scalar(
            "SELECT t FROM proxima_core.announce
              WHERE op = 'transfer' ORDER BY seq",
        )
        .fetch_all(pool)
        .await?;
        assert_eq!(announced_t, vec![second.memory_id.into_inner(); 2]);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cooled input live-series transfer failed");
}

/// A series can be entirely cooled, leaving no `memory` or `memory_head` row.
/// The cooled identity still names the series boundary, and every cooled stub
/// moves atomically with the transfer.
#[tokio::test]
async fn transfer_moves_a_fully_cooled_headless_series() {
    let (db_name, pg, _cold) = fresh_pg_with_cold().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let first = pg.ingest_fact_atomic(&permit, &draft(), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, first.memory_id).await?;

        let mut later = draft();
        later.handle = Some(first.handle);
        later.ingest_key = Some("cooled-input-headless".into());
        let second = pg.ingest_fact_atomic(&permit, &later, None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, second.memory_id).await?;

        let before: (i64, i64, i64) = sqlx::query_as(
            "SELECT
                 (SELECT count(*) FROM proxima_core.memory WHERE handle = $1),
                 (SELECT count(*) FROM proxima_core.memory_head WHERE handle = $1),
                 (SELECT count(*) FROM proxima_core.cooled WHERE handle = $1)",
        )
        .bind(first.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(before, (0, 0, 2));

        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(first.memory_id),
                dest,
                &contract_sidecar_tables(),
            )
            .await?
        );

        let owners: Vec<Uuid> = sqlx::query_scalar(
            "SELECT owner_id FROM proxima_core.cooled
              WHERE handle = $1 ORDER BY t",
        )
        .bind(first.handle)
        .fetch_all(pool)
        .await?;
        assert_eq!(owners, vec![dest.stored_owner_id(); 2]);
        let after: (i64, i64, i64) = sqlx::query_as(
            "SELECT
                 (SELECT count(*) FROM proxima_core.memory WHERE handle = $1),
                 (SELECT count(*) FROM proxima_core.memory_head WHERE handle = $1),
                 (SELECT count(*) FROM proxima_core.cooled WHERE handle = $1)",
        )
        .bind(first.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(after, (0, 0, 2));

        let announced: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT owner_id, handle, t FROM proxima_core.announce
              WHERE op = 'transfer' ORDER BY seq",
        )
        .fetch_all(pool)
        .await?;
        assert_eq!(announced.len(), 2);
        assert!(announced.iter().all(|(owner_id, announced_handle, t)| {
            *announced_handle == first.handle
                && *t == second.memory_id.into_inner()
                && (*owner_id == owner.stored_owner_id() || *owner_id == dest.stored_owner_id())
        }));
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("fully cooled headless transfer failed");
}

/// The public Engine must use the cooled identity for authorization too. A
/// direct storage transfer can prove the move, but an Engine call first asks
/// `visible_home_owner`; omitting `cooled` there turns a valid headless Memory
/// into a false public NotFound.
#[tokio::test]
async fn engine_transfer_accepts_a_fully_cooled_headless_memory() {
    let (db_name, pg, _cold) = fresh_pg_with_cold().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let destination = destination();
        let pool = pg.pool_for_tests();
        let written = pg.ingest_fact_atomic(&permit, &draft(), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id).await?;

        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'group'::proxima_core.owner_kind)",
        )
        .bind(destination.stored_owner_id())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.group_memberships
                (group_id, member_user_id, relation)
             VALUES ($1, $2, 'admin'::proxima_core.membership_relation)",
        )
        .bind(destination.stored_owner_id())
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;

        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let authz =
            AuthzContext::for_subject(UserId::new(owner.stored_owner_id()), AuthPath::HostBearer);
        engine
            .transfer_to_owner(&authz, EntityId::Memory(written.memory_id), destination)
            .await?;

        let cooled_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.cooled WHERE t = $1")
                .bind(written.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(cooled_owner, destination.stored_owner_id());
        let head_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory_head WHERE handle = $1",
        )
        .bind(written.handle)
        .fetch_one(pool)
        .await?;
        assert_eq!(head_count, 0, "headless transfer must not invent a head");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("Engine headless transfer failed");
}

/// The transfer captures and locks the complete series handle before its
/// cooled-row update. A same-handle source-owner ingest that arrives in that
/// window must wait for the transfer, then fail cleanly once the series has
/// moved rather than advancing a head behind the transfer's back.
#[tokio::test]
async fn transfer_serializes_same_handle_ingest_after_handle_lock() {
    const PROBE_LOCK: i64 = 0x5052_4f58_5055_4238;

    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let first = pg.ingest_fact_atomic(&permit, &draft(), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, first.memory_id).await?;
        let mut second_draft = draft();
        second_draft.handle = Some(first.handle);
        second_draft.ingest_key = Some("k2".into());
        let second = pg.ingest_fact_atomic(&permit, &second_draft, None).await?;

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
                .transfer_to_owner(
                    &permit,
                    EntityId::Memory(transferred_id),
                    dest,
                    &contract_sidecar_tables(),
                )
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

        assert!(
            handle_lock_held(pool, first.handle).await?,
            "transfer must hold the exact series-handle advisory lock before moving rows"
        );

        let mut third_draft = draft();
        third_draft.handle = Some(first.handle);
        third_draft.ingest_key = Some("k3".into());
        let third_pg = pg.clone();
        let third = tokio::spawn(async move {
            third_pg
                .ingest_fact_atomic(&permit, &third_draft, None)
                .await
        });

        let mut owner_waiting = false;
        for _ in 0..100 {
            if owner_fence_lock_state(pool, owner, "ShareLock", false).await? {
                owner_waiting = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            owner_waiting && !third.is_finished(),
            "same-handle append must queue on the source owner fence held by transfer"
        );

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
        let third_error = tokio::time::timeout(std::time::Duration::from_secs(5), third)
            .await??
            .expect_err("the source-owner append must lose to the completed transfer");
        assert!(
            !third_error.to_string().contains("40P01"),
            "the serialized append must not deadlock: {third_error}"
        );

        let head: (Uuid, Uuid) =
            sqlx::query_as("SELECT t, owner_id FROM proxima_core.memory_head WHERE handle = $1")
                .bind(first.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head.0, second.memory_id.into_inner());
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
        assert_eq!(
            unmoved_versions, 0,
            "the transfer must move the complete series"
        );
        let stale_third_key: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.ingest_keys
              WHERE owner_id = $1 AND ingest_key = 'k3'",
        )
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            stale_third_key, 0,
            "the losing append must not leave an ingest key"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("transfer/ingest lifecycle serialization failed");
}

/// A citation-bearing source admission holds the source owner fence while it
/// materializes a Memory. The transfer's exclusive source fence must wait for
/// that admission, then observe the committed citation and mount the object
/// instead of moving the source row out from under it.
#[tokio::test]
async fn transfer_serializes_source_citation_before_moving_a_blob() {
    const PROBE_LOCK: i64 = 0x4349_5445_4f57_4e52;

    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let destination = destination();
        let pool = pg.pool_for_tests();

        let mut target_draft = draft();
        target_draft.citation = Some(
            CitationSpec::v1(
                "core/test-cited-object-v1",
                [17_u8; 32],
                "core/test-citation-mapping-v1",
            )
            .into(),
        );
        let target = pg.ingest_fact_atomic(&permit, &target_draft, None).await?;
        let blob_id = target
            .cited_object_id
            .ok_or("citation-bearing target did not return its blob")?;

        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE SEQUENCE public.transfer_citation_probe;
             CREATE FUNCTION public.block_source_citation_insert() RETURNS trigger
             LANGUAGE plpgsql AS $$
             BEGIN
                 PERFORM nextval('public.transfer_citation_probe');
                 PERFORM pg_advisory_xact_lock({PROBE_LOCK});
                 RETURN NEW;
             END
             $$;
             CREATE TRIGGER block_source_citation_insert
             BEFORE INSERT ON proxima_core.memory
             FOR EACH ROW
             EXECUTE FUNCTION public.block_source_citation_insert();",
        )))
        .execute(pool)
        .await?;

        let mut blocker = pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(PROBE_LOCK)
            .execute(&mut *blocker)
            .await?;

        let append_pg = pg.clone();
        let append_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let mut append_draft = draft();
        append_draft.ingest_key = Some("source-citation".into());
        append_draft.blob_id = Some(blob_id);
        let append = tokio::spawn(async move {
            append_pg
                .ingest_fact_atomic(&append_permit, &append_draft, None)
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let called: bool =
                    sqlx::query_scalar("SELECT is_called FROM public.transfer_citation_probe")
                        .fetch_one(pool)
                        .await?;
                if called {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;

        let transfer_pg = pg.clone();
        let transfer_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let transfer = tokio::spawn(async move {
            transfer_pg
                .transfer_to_owner(
                    &transfer_permit,
                    EntityId::Memory(target.memory_id),
                    destination,
                    &contract_sidecar_tables(),
                )
                .await
        });

        // The append is paused while holding the source shared fence. Once
        // the transfer has queued for the exclusive fence, the blob must
        // still be source-owned: source citation and the row move are
        // serialized.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if owner_fence_lock_state(pool, owner, "ExclusiveLock", false).await? {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        let owner_before_release: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(blob_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(owner_before_release, owner.stored_owner_id());

        let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(PROBE_LOCK)
            .fetch_one(&mut *blocker)
            .await?;
        assert!(unlocked);
        drop(blocker);

        let appended = tokio::time::timeout(std::time::Duration::from_secs(10), append).await??;
        let appended = appended?;
        let transferred =
            tokio::time::timeout(std::time::Duration::from_secs(10), transfer).await??;
        assert!(transferred?);

        let source_blob_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(blob_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(source_blob_owner, owner.stored_owner_id());
        let source_citation_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(appended.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(source_citation_owner, owner.stored_owner_id());
        let source_citation_blob: Uuid =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(appended.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(source_citation_blob, blob_id);

        let target_owner_and_blob: (Uuid, Uuid) = sqlx::query_as(
            "SELECT m.owner_id, m.blob_id
               FROM proxima_core.memory m
              WHERE m.t = $1",
        )
        .bind(target.memory_id.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(target_owner_and_blob.0, destination.stored_owner_id());
        assert_ne!(
            target_owner_and_blob.1, blob_id,
            "the transferred citation must use the destination mount"
        );
        let mounted_blob_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(target_owner_and_blob.1)
                .fetch_one(pool)
                .await?;
        assert_eq!(mounted_blob_owner, destination.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("transfer/source citation serialization failed");
}

/// Destination citation admission holds the destination owner fence in
/// shared mode while it creates the unique `(owner, schema, content_hash)`
/// blob row. A transfer must wait for the exclusive destination fence, then
/// reread the dedupe key and remap to the row the citation just created.
#[tokio::test]
async fn transfer_serializes_destination_citation_dedupe() {
    const PROBE_LOCK: i64 = 0x0044_4553_5442_4c4f;

    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let source_permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let destination = destination();
        let pool = pg.pool_for_tests();

        let mut source_draft = draft();
        source_draft.citation = Some(
            CitationSpec::v1(
                "core/test-cited-object-v1",
                [29_u8; 32],
                "core/test-citation-mapping-v1",
            )
            .into(),
        );
        let source_row = pg
            .ingest_fact_atomic(&source_permit, &source_draft, None)
            .await?;
        let source_blob = source_row.cited_object_id.ok_or("source blob missing")?;

        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE SEQUENCE public.destination_blob_probe;
             CREATE FUNCTION public.block_destination_blob_insert() RETURNS trigger
             LANGUAGE plpgsql AS $$
             BEGIN
                 IF NEW.owner_id = '{destination}'::uuid THEN
                     PERFORM nextval('public.destination_blob_probe');
                     PERFORM pg_advisory_xact_lock({PROBE_LOCK});
                 END IF;
                 RETURN NEW;
             END
             $$;
             CREATE TRIGGER block_destination_blob_insert
             BEFORE INSERT ON proxima_core.blob
             FOR EACH ROW
             EXECUTE FUNCTION public.block_destination_blob_insert();",
            destination = destination.stored_owner_id(),
        )))
        .execute(pool)
        .await?;

        let mut blocker = pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(PROBE_LOCK)
            .execute(&mut *blocker)
            .await?;

        let destination_pg = pg.clone();
        let destination_citation = tokio::spawn(async move {
            let permit = OwnerWritePermit::new_for_tests(destination, AccessKind::Fact);
            let mut citation = draft();
            citation.citation = Some(
                CitationSpec::v1(
                    "core/test-cited-object-v1",
                    [29_u8; 32],
                    "core/test-citation-mapping-v1",
                )
                .into(),
            );
            destination_pg
                .ingest_fact_atomic(&permit, &citation, None)
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let called: bool =
                    sqlx::query_scalar("SELECT is_called FROM public.destination_blob_probe")
                        .fetch_one(pool)
                        .await?;
                if called {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;

        let transfer_pg = pg.clone();
        let transfer_permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let transfer = tokio::spawn(async move {
            transfer_pg
                .transfer_to_owner(
                    &transfer_permit,
                    EntityId::Memory(source_row.memory_id),
                    destination,
                    &contract_sidecar_tables(),
                )
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let waiting = owner_fence_waiter_count(pool, destination, "ExclusiveLock").await?;
                if waiting >= 1 {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        let owner_before_release: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(source_blob)
                .fetch_one(pool)
                .await?;
        assert_eq!(owner_before_release, source.stored_owner_id());

        let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(PROBE_LOCK)
            .fetch_one(&mut *blocker)
            .await?;
        assert!(unlocked);
        drop(blocker);

        let destination_result =
            tokio::time::timeout(std::time::Duration::from_secs(10), destination_citation)
                .await??;
        let destination_row = destination_result?;
        let transferred =
            tokio::time::timeout(std::time::Duration::from_secs(10), transfer).await??;
        assert!(
            transferred?,
            "transfer must reread the destination dedupe row"
        );

        let (target_owner, target_blob): (Uuid, Uuid) =
            sqlx::query_as("SELECT owner_id, blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(source_row.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(target_owner, destination.stored_owner_id());
        assert_eq!(target_blob, destination_row.cited_object_id.unwrap());
        assert_ne!(target_blob, source_blob);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("transfer/destination citation serialization failed");
}

/// A committed cited Fact can precede the upload bookkeeping transition. An
/// unresolved upload row must stop transfer from moving its blob until finish
/// (or an explicit terminal cleanup) publishes the row; otherwise a later
/// source-owner finish could create a cross-owner upload locator.
#[tokio::test]
async fn transfer_refuses_unpublished_upload_until_finish_publishes() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let destination = destination();
        let pool = pg.pool_for_tests();
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, $2::proxima_core.owner_kind) ON CONFLICT DO NOTHING",
        )
        .bind(source.stored_owner_id())
        .bind(proxima_core::OwnerRefKind::of(&source).as_str())
        .execute(pool)
        .await?;
        let content_hash = vec![37_u8; 32];
        let blob_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/uploaded-blob-v1', $2)
             RETURNING blob_id",
        )
        .bind(source.stored_owner_id())
        .bind(&content_hash)
        .fetch_one(pool)
        .await?;
        let upload_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                (owner_id, upload_id, bucket, object_key, filename, mime,
                 expected_byte_len, status, content_hash, expires_at)
             VALUES ($1, $2, 'test', 'pending/test', 'test.pdf',
                     'application/pdf', 1, 'pending', $3,
                     now() + interval '1 hour')",
        )
        .bind(source.stored_owner_id())
        .bind(upload_id)
        .bind(&content_hash)
        .execute(pool)
        .await?;
        let written = cite(&pg, &permit, "unpublished-upload", blob_id).await?;

        let error = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                destination,
                &contract_sidecar_tables(),
            )
            .await
            .expect_err("transfer must wait for unresolved upload publication");
        assert!(
            matches!(error, StorageError::Conflict(ref message) if message.contains("publication")),
            "unpublished upload refusal must be a caller-retryable conflict: {error}"
        );
        let source_blob_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(blob_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(source_blob_owner, source.stored_owner_id());

        // Model finish's database half after the Fact committed. The real
        // upload service performs this transition under the same owner fence;
        // stage has already pinned the digest and canonical object key.
        sqlx::query(
            "UPDATE proxima_core.blob_uploads
                SET status = 'completed', blob_id = $2, completed_at = now(),
                    object_key = 'objects/' || upload_id::text, sha256 = $4
              WHERE owner_id = $1 AND upload_id = $3",
        )
        .bind(source.stored_owner_id())
        .bind(blob_id)
        .bind(upload_id)
        .bind(&content_hash)
        .execute(pool)
        .await?;
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                destination,
                &contract_sidecar_tables()
            )
            .await?,
            "transfer proceeds once finish published the upload row"
        );
        let (blob_owner, upload_owner): (Uuid, Uuid) = sqlx::query_as(
            "SELECT b.owner_id, u.owner_id
               FROM proxima_core.blob b
               JOIN proxima_core.blob_uploads u ON u.blob_id = b.blob_id
              WHERE b.blob_id = $1 AND u.upload_id = $2",
        )
        .bind(blob_id)
        .bind(upload_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(blob_owner, destination.stored_owner_id());
        assert_eq!(upload_owner, destination.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("unpublished upload transfer fence failed");
}

/// 0007 cannot infer BLAKE3 for a row staged under the previous schema.
/// Its canonical locator and SHA-256 prove staging happened, but do not say
/// which cited blob the later Fact committed. Transfer therefore fences the
/// owner until the ordinary completion retry re-hashes and finishes the row.
#[tokio::test]
async fn legacy_staged_upload_without_blake3_fences_transfer_until_retry() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let destination = destination();
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, $2::proxima_core.owner_kind)",
        )
        .bind(source.stored_owner_id())
        .bind(proxima_core::OwnerRefKind::of(&source).as_str())
        .execute(pool)
        .await?;
        let content_hash = vec![38_u8; 32];
        let blob_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/uploaded-blob-v1', $2)
             RETURNING blob_id",
        )
        .bind(source.stored_owner_id())
        .bind(&content_hash)
        .fetch_one(pool)
        .await?;
        let upload_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                (upload_id, owner_id, bucket, object_key, filename, mime,
                 expected_byte_len, status, sha256, expires_at)
             VALUES ($1, $2, 'test', 'objects/' || $1::text, 'legacy.pdf',
                     'application/pdf', 1, 'pending', $3,
                     now() + interval '1 hour')",
        )
        .bind(upload_id)
        .bind(source.stored_owner_id())
        .bind(&content_hash)
        .execute(pool)
        .await?;
        let written = cite(&pg, &permit, "legacy-staged-upload", blob_id).await?;

        let error = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                destination,
                &contract_sidecar_tables(),
            )
            .await
            .expect_err("an identity-ambiguous legacy stage must fence transfer");
        assert!(
            matches!(error, StorageError::Conflict(ref message) if message.contains("publication")),
            "legacy staged upload refusal must be retryable: {error}"
        );
        let owner_before_retry: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(blob_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(owner_before_retry, source.stored_owner_id());

        // Model the retry's restage and finish database transitions after it
        // re-hashed the retained object under the 0007 schema.
        sqlx::query(
            "UPDATE proxima_core.blob_uploads
                SET content_hash = $4, status = 'completed', blob_id = $2,
                    completed_at = now()
              WHERE owner_id = $1 AND upload_id = $3",
        )
        .bind(source.stored_owner_id())
        .bind(blob_id)
        .bind(upload_id)
        .bind(&content_hash)
        .execute(pool)
        .await?;
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                destination,
                &contract_sidecar_tables(),
            )
            .await?,
            "transfer proceeds after the retry publishes exact identity"
        );
        let (blob_owner, upload_owner): (Uuid, Uuid) = sqlx::query_as(
            "SELECT b.owner_id, u.owner_id
               FROM proxima_core.blob b
               JOIN proxima_core.blob_uploads u ON u.blob_id = b.blob_id
              WHERE b.blob_id = $1 AND u.upload_id = $2",
        )
        .bind(blob_id)
        .bind(upload_id)
        .fetch_one(pool)
        .await?;
        assert_eq!(blob_owner, destination.stored_owner_id());
        assert_eq!(upload_owner, destination.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("legacy staged upload transfer fence failed");
}

/// A prepare that never staged bytes has no content identity and therefore
/// cannot claim every series under its owner. A staged upload is equally
/// irrelevant to a different series: the owner fence serializes publication,
/// while the content/handle join supplies the exact transfer boundary.
#[tokio::test]
async fn transfer_ignores_unstaged_and_unrelated_pending_uploads() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let destination = destination();
        let pool = pg.pool_for_tests();
        let moving = pg.ingest_fact_atomic(&permit, &draft(), None).await?;

        let unrelated_hash = vec![91_u8; 32];
        let unrelated_blob: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/uploaded-blob-v1', $2)
             RETURNING blob_id",
        )
        .bind(source.stored_owner_id())
        .bind(&unrelated_hash)
        .fetch_one(pool)
        .await?;
        let unrelated = cite(&pg, &permit, "unrelated-upload", unrelated_blob).await?;

        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                (owner_id, bucket, object_key, filename, mime,
                 expected_byte_len, status, content_hash, expires_at)
             VALUES
                ($1, 'test', 'pending/unstaged', 'lost.bin',
                 'application/octet-stream', 1, 'pending', NULL,
                 now() + interval '1 hour'),
                ($1, 'test', 'objects/unrelated', 'other.bin',
                 'application/octet-stream', 1, 'pending', $2,
                 now() + interval '1 hour')",
        )
        .bind(source.stored_owner_id())
        .bind(&unrelated_hash)
        .execute(pool)
        .await?;

        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(moving.memory_id),
                destination,
                &contract_sidecar_tables(),
            )
            .await?,
            "neither an abandoned prepare nor another series' staged upload may pin this series"
        );
        let unrelated_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(unrelated.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            unrelated_owner,
            source.stored_owner_id(),
            "the series whose staged upload stayed pending remains at the source"
        );
        let pending_source_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM proxima_core.blob_uploads
              WHERE owner_id = $1 AND status = 'pending'",
        )
        .bind(source.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(pending_source_rows, 2, "transfer does not consume uploads");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("unrelated pending upload transfer failed");
}

/// Terminal upload cleanup has abandoned publication and must not pin an
/// owner's transfer boundary forever. The terminal row has no blob id, so it
/// remains in the source scope while the cited blob itself moves; a later
/// completed publication is handled by the upload service against the blob's
/// current owner.
#[tokio::test]
async fn transfer_allows_aborted_and_expired_upload_cleanup() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pool = pg.pool_for_tests();
        for (ordinal, status) in [(0_u8, "aborted"), (1_u8, "expired")] {
            let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
            let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
            let destination = destination();
            let mut cited = draft();
            cited.citation = Some(
                CitationSpec::v1(
                    "core/test-cited-object-v1",
                    [71_u8 + ordinal; 32],
                    "core/test-citation-mapping-v1",
                )
                .into(),
            );
            let written = pg.ingest_fact_atomic(&permit, &cited, None).await?;
            let blob_id = written.cited_object_id.ok_or("cited blob missing")?;
            let upload_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO proxima_core.blob_uploads
                    (owner_id, upload_id, bucket, object_key, filename, mime,
                     expected_byte_len, status, expires_at)
                 VALUES ($1, $2, 'test', 'pending/test', 'test.pdf',
                     'application/pdf', 1, $3::proxima_core.blob_upload_status,
                         now() + interval '1 hour')",
            )
            .bind(source.stored_owner_id())
            .bind(upload_id)
            .bind(status)
            .execute(pool)
            .await?;

            assert!(
                pg.transfer_to_owner(
                    &permit,
                    EntityId::Memory(written.memory_id),
                    destination,
                    &contract_sidecar_tables(),
                )
                .await?,
                "transfer must proceed after {status} cleanup"
            );
            let moved_blob_owner: Uuid =
                sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                    .bind(blob_id)
                    .fetch_one(pool)
                    .await?;
            assert_eq!(moved_blob_owner, destination.stored_owner_id());
            let (terminal_owner, observed_status): (Uuid, String) = sqlx::query_as(
                "SELECT owner_id, status::text
                   FROM proxima_core.blob_uploads
                  WHERE upload_id = $1",
            )
            .bind(upload_id)
            .fetch_one(pool)
            .await?;
            assert_eq!(terminal_owner, source.stored_owner_id());
            assert_eq!(observed_status, status);
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("terminal upload cleanup must not block transfer");
}

/// A terminal row is ordinarily abandoned cleanup, but not when this exact
/// series already carries its canonical uploaded-blob cited object. That
/// shape means the Fact committed before finish; moving the blob would make
/// the source-owned finish publish across an owner boundary.
#[tokio::test]
async fn transfer_waits_for_terminal_uploaded_blob_publication() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let destination = destination();
        let pool = pg.pool_for_tests();
        let digest = vec![81_u8; 32];
        let (blob_id, upload_id) = seed_cited_blob(pool, source, &digest).await?;
        sqlx::query(
            "UPDATE proxima_core.blob_uploads
                SET status = 'aborted', blob_id = NULL, completed_at = NULL
              WHERE upload_id = $1",
        )
        .bind(upload_id)
        .execute(pool)
        .await?;
        let written = cite(&pg, &permit, "terminal-publication", blob_id).await?;

        let error = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                destination,
                &contract_sidecar_tables(),
            )
            .await
            .expect_err("a committed uploaded-blob Fact must finish before transfer");
        assert!(
            matches!(error, StorageError::Conflict(ref message) if message.contains("publication")),
            "terminal publication refusal must be retryable: {error}"
        );

        sqlx::query(
            "UPDATE proxima_core.blob_uploads
                SET status = 'completed', blob_id = $2, completed_at = now()
              WHERE upload_id = $1",
        )
        .bind(upload_id)
        .bind(blob_id)
        .execute(pool)
        .await?;
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                destination,
                &contract_sidecar_tables(),
            )
            .await?,
            "transfer proceeds after terminal publication is resolved"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("terminal uploaded-blob publication transfer fence failed");
}

/// A failed attempt at bytes that were later published successfully is only
/// cleanup. The exact completed row is the readable publication; letting an
/// older same-hash terminal row fence forever would make abort-and-retry a
/// permanent transfer denial.
#[tokio::test]
async fn terminal_same_hash_retry_does_not_pin_an_exactly_published_blob() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let destination = destination();
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let hash = vec![82_u8; 32];
        let (blob_id, completed_upload) = seed_cited_blob(pool, source, &hash).await?;
        let terminal_upload = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                (upload_id, owner_id, bucket, object_key, filename, mime,
                 expected_byte_len, status, sha256, content_hash, expires_at,
                 aborted_at)
             VALUES ($1, $2, 'test', 'objects/' || $1::text, 'first.pdf',
                     'application/pdf', 3, 'aborted', $3, $3,
                     now() + interval '1 hour', now())",
        )
        .bind(terminal_upload)
        .bind(source.stored_owner_id())
        .bind(&hash)
        .execute(pool)
        .await?;
        let written = cite(&pg, &permit, "same-hash-retry", blob_id).await?;

        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                destination,
                &contract_sidecar_tables(),
            )
            .await?,
            "a superseded terminal attempt must not pin an exact publication"
        );
        let (memory_owner, blob_owner, completed_owner): (Uuid, Uuid, Uuid) = sqlx::query_as(
            "SELECT m.owner_id, b.owner_id, u.owner_id
               FROM proxima_core.memory m
               JOIN proxima_core.blob b ON b.blob_id = m.blob_id
               JOIN proxima_core.blob_uploads u ON u.upload_id = $2
              WHERE m.t = $1",
        )
        .bind(written.memory_id.into_inner())
        .bind(completed_upload)
        .fetch_one(pool)
        .await?;
        assert_eq!(memory_owner, destination.stored_owner_id());
        assert_eq!(blob_owner, destination.stored_owner_id());
        assert_eq!(completed_owner, destination.stored_owner_id());
        let (terminal_owner, terminal_status): (Uuid, String) = sqlx::query_as(
            "SELECT owner_id, status::text
               FROM proxima_core.blob_uploads
              WHERE upload_id = $1",
        )
        .bind(terminal_upload)
        .fetch_one(pool)
        .await?;
        assert_eq!(terminal_owner, source.stored_owner_id());
        assert_eq!(terminal_status, "aborted");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("same-hash terminal cleanup transfer failed");
}

/// A transfer locks the series handle before its lifecycle set. A row that
/// crosses the lifecycle seam is therefore rejected before any transfer leg
/// runs, and the atomic port retries from a fresh snapshot rather than
/// transferring only the rows it happened to lock first.
#[tokio::test]
async fn transfer_retries_when_series_membership_drifts_before_mutation() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let first = pg.ingest_fact_atomic(&permit, &draft(), None).await?;
        let first_t = first.memory_id.into_inner();

        // Hold the first lifecycle key so the transfer has a deterministic
        // window after its membership snapshot and before the exact reread.
        let mut blocker = pool.begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(
                 hashtextextended('proxima-forget:' || $1::text, 0)
             )",
        )
        .bind(first_t)
        .execute(&mut *blocker)
        .await?;

        let transfer_pg = pg.clone();
        let transfer = tokio::spawn(async move {
            let transfer_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
            transfer_pg
                .transfer_to_owner(
                    &transfer_permit,
                    EntityId::Memory(first.memory_id),
                    dest,
                    &contract_sidecar_tables(),
                )
                .await
        });

        // The handle is not blocked, so a waiter here means the transfer has
        // taken its complete membership snapshot and is waiting for the
        // lifecycle set. This avoids sleeping for a race with the test row.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if lifecycle_waiter_count(pool, first_t).await? >= 1 {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;

        let drift_t = Uuid::now_v7();
        // This direct insert is the deterministic seam probe. Production
        // appenders take the handle lock; bypassing the advisory lock here
        // lets the test prove that the exact reread is still a safety net if
        // a writer crosses the boundary.
        sqlx::query(
            "INSERT INTO proxima_core.memory
                (handle, t, kind, owner_id, schema_id, origins, refs,
                 goal_refs, sidecar_tables)
             VALUES ($1, $2, 'fact', $3, 'core/test-fact-v1', '{}', '{}',
                     '{}', '{}')",
        )
        .bind(first.handle)
        .bind(drift_t)
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
            .bind(first.handle)
            .bind(drift_t)
            .execute(pool)
            .await?;
        // Keep the seam row a valid series append: the head advances with
        // the membership, exactly as the ordinary append path does.

        blocker.rollback().await?;
        let moved = tokio::time::timeout(std::time::Duration::from_secs(10), transfer).await??;
        assert!(moved?, "the retry must transfer the complete series");

        let source_rows: i64 = sqlx::query_scalar::<_, i64>(
            "SELECT count(*)::bigint
               FROM proxima_core.memory
              WHERE handle = $1 AND owner_id = $2
             UNION ALL
             SELECT count(*)::bigint
               FROM proxima_core.cooled
              WHERE handle = $1 AND owner_id = $2",
        )
        .bind(first.handle)
        .bind(owner.stored_owner_id())
        .fetch_all(pool)
        .await?
        .into_iter()
        .sum();
        assert_eq!(
            source_rows, 0,
            "no source-owned version may escape the retry"
        );
        let destination_rows: i64 = sqlx::query_scalar::<_, i64>(
            "SELECT count(*)::bigint
               FROM proxima_core.memory
              WHERE handle = $1 AND owner_id = $2
             UNION ALL
             SELECT count(*)::bigint
               FROM proxima_core.cooled
              WHERE handle = $1 AND owner_id = $2",
        )
        .bind(first.handle)
        .bind(dest.stored_owner_id())
        .fetch_all(pool)
        .await?
        .into_iter()
        .sum();
        assert_eq!(destination_rows, 2, "the retry must move both versions");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("transfer membership drift retry failed");
}

/// Opposite transfers acquire both endpoint fences in one canonical order.
/// Canonical ordering deliberately means both transactions contend on the
/// same minimum endpoint; they cannot each hold a distinct first endpoint.
/// This test holds that exact first fence, waits for both transfer requests
/// in `ExclusiveLock` mode, and then releases it to prove the actual wait
/// order rather than relying on scheduler timing. It calls the storage
/// transfer primitive directly with personal endpoints; the Engine's public
/// port restricts destinations to groups, but this lock-order regression is
/// about the storage boundary itself.
#[tokio::test]
async fn crossed_transfers_do_not_deadlock_on_owner_fences() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let left = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let right = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let left_permit = OwnerWritePermit::new_for_tests(left, AccessKind::Fact);
        let right_permit = OwnerWritePermit::new_for_tests(right, AccessKind::Fact);
        let left_row = pg.ingest_fact_atomic(&left_permit, &draft(), None).await?;
        let right_row = pg.ingest_fact_atomic(&right_permit, &draft(), None).await?;

        let pool = pg.pool_for_tests();
        let first_fenced_owner = if left.stored_owner_id() < right.stored_owner_id() {
            left
        } else {
            right
        };
        let mut fence_blocker = pool.begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(
                 hashtextextended('proxima-owner-fence:' || $1 || ':' || $2::text, 0)
             )",
        )
        .bind("personal")
        .bind(first_fenced_owner.stored_owner_id())
        .execute(&mut *fence_blocker)
        .await?;

        let left_pg = pg.clone();
        let right_pg = pg.clone();
        let left_transfer = tokio::spawn(async move {
            left_pg
                .transfer_to_owner(
                    &left_permit,
                    EntityId::Memory(left_row.memory_id),
                    right,
                    &contract_sidecar_tables(),
                )
                .await
        });
        let right_transfer = tokio::spawn(async move {
            right_pg
                .transfer_to_owner(
                    &right_permit,
                    EntityId::Memory(right_row.memory_id),
                    left,
                    &contract_sidecar_tables(),
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let waiting =
                    owner_fence_waiter_count(pool, first_fenced_owner, "ExclusiveLock").await?;
                if waiting >= 2 {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        fence_blocker.rollback().await?;
        let (left_result, right_result) =
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                tokio::join!(left_transfer, right_transfer)
            })
            .await?;
        assert!(left_result??, "left-to-right transfer must complete");
        assert!(right_result??, "right-to-left transfer must complete");

        let left_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE handle = $1")
                .bind(left_row.handle)
                .fetch_one(pool)
                .await?;
        let right_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE handle = $1")
                .bind(right_row.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(left_owner, right.stored_owner_id());
        assert_eq!(right_owner, left.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("crossed transfer fence ordering failed");
}

/// A completed erase removes the original admission and leaves the handle
/// available for a new series. A later transfer request for the erased `t`
/// must be a no-op and must not move the replacement series that reuses the
/// handle.
#[tokio::test]
async fn transfer_does_not_move_a_handle_reused_after_complete_erase() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let destination = destination();
        let pool = pg.pool_for_tests();
        let original = pg.ingest_fact_atomic(&permit, &draft(), None).await?;

        let mut erase = pool.begin().await?;
        let (erased, plan) = erase_memory_series(
            &mut erase,
            &core_pg_sidecars(),
            &contract_sidecar_tables(),
            &owner,
            &[original.memory_id.into_inner()],
        )
        .await?;
        assert_eq!(erased, 1);
        assert!(plan.is_empty());
        erase.commit().await?;

        let mut replacement_draft = draft();
        replacement_draft.handle = Some(original.handle);
        replacement_draft.ingest_key = Some("replacement".into());
        let replacement = pg
            .ingest_fact_atomic(&permit, &replacement_draft, None)
            .await?;
        assert_eq!(replacement.handle, original.handle);

        let moved = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(original.memory_id),
                destination,
                &contract_sidecar_tables(),
            )
            .await?;
        assert!(
            !moved,
            "an erased admission cannot transfer its replacement"
        );

        let replacement_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(replacement.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(replacement_owner, owner.stored_owner_id());
        let head: Uuid =
            sqlx::query_scalar("SELECT t FROM proxima_core.memory_head WHERE handle = $1")
                .bind(original.handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head, replacement.memory_id.into_inner());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("handle reuse after complete erase was not fenced");
}

/// A caller can resolve a cooled Memory under its prior owner after the series
/// head has already moved elsewhere. Transfer must return clean `false` and
/// leave every hot/cold row and announce lane untouched. The head DELETE
/// variant is not statically constructible (`memory.handle` FK-references
/// `memory_head`), so the probe diverges the head owner, which
/// `memory_head_t_only` admits.
#[tokio::test]
async fn transfer_is_unchanged_when_the_head_owner_is_already_lost() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        use proxima_storage_pg::verbs::forget::cold_object_key;

        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();
        let first = pg.ingest_fact_atomic(&permit, &draft(), None).await?;
        MemoryAuthoringPort::forget_memory(&pg, &permit, first.memory_id).await?;
        let mut later = draft();
        later.handle = Some(first.handle);
        later.ingest_key = Some("k2".into());
        let second = pg.ingest_fact_atomic(&permit, &later, None).await?;
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
        let destination_owner_before: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.owners WHERE owner_id = $1",
        )
        .bind(dest.stored_owner_id())
        .fetch_one(pool)
        .await?;
        let announces_before: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.announce")
                .fetch_one(pool)
                .await?;

        let transferred = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(first.memory_id),
                dest,
                &contract_sidecar_tables(),
            )
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
        let destination_owner_after: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.owners WHERE owner_id = $1",
        )
        .bind(dest.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            destination_owner_after, destination_owner_before,
            "a rejected transfer must not leave a destination owner row"
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
            .transfer_to_owner(
                &permit,
                EntityId::Goal(GoalId::new(out.t)),
                dest,
                &contract_sidecar_tables(),
            )
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
/// it, and that owner's owner erase would abort forever on the
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
            .transfer_to_owner(
                &permit,
                EntityId::Goal(GoalId::new(goal_t)),
                dest,
                &contract_sidecar_tables(),
            )
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

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "test-drop-armed-transfer".into(),
        });
        let outcome = pg
            .erase_personal_owner(&auth, user, &contract_sidecar_tables())
            .await?;
        let OwnerEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("erase must complete after the refused transfer, got {outcome:?}");
        };
        assert_eq!(
            counts.get("wake_configs"),
            1,
            "the armed wake row is erased"
        );
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
        let written = pg.ingest_fact_atomic(&permit, &draft(), None).await?;
        let t = written.memory_id.into_inner();

        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                dest,
                &contract_sidecar_tables()
            )
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
        let seed = pg.ingest_fact_atomic(&permit, &draft(), None).await?;
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
        let written = pg.ingest_fact_atomic(&permit, &cited, None).await?;
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                dest,
                &contract_sidecar_tables()
            )
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

/// Retain-at-source for the audit sidecar, and what "retain" means.
///
/// `mcp_call_logged_v1` describes the ACTOR of a tool call (`actor_upn`),
/// not the memory, and it carries its own `owner_id` stamped with the owner
/// that made the call. A transfer moves the memory and leaves the row
/// exactly where it is: it stays reachable by the source (history, export,
/// Art. 17 erase) and unreachable by the destination (its payload hydrate
/// joins the memory's owner to the row's). Deleting the row instead would keep
/// the destination out at the cost of history the source is entitled to. Every
/// other sidecar (here: `agent_note_v1`) follows the memory.
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
        // Both sidecars ride one write, so the memory declares both: an
        // ordinary sidecar the transfer moves, and the retained audit row.
        let written = ingest_mcp_call_fact(&pg, &permit, &[note_payload("note", "body")]).await?;
        let t = written.memory_id.into_inner();

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
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                dest,
                &contract_sidecar_tables()
            )
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
        assert_owner_pinned_payload_redacted(&pg, dest, written.memory_id).await?;

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
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id,
            drop_event_id: "test-drop-retained-audit".into(),
        });
        let erased = pg
            .erase_personal_owner(&auth, user_id, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(erased, OwnerEraseOutcome::Completed { .. }),
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

/// A retained audit row belongs to its actor even after its Memory moves, but
/// it still keeps the Memory's source attribution. Source-scope erasure must
/// therefore use the moved Memory only to identify the source, not to
/// re-decide the independently stored owner of the audit row.
#[tokio::test]
async fn source_scope_erase_reaches_a_retained_actor_log_after_transfer() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let user_id = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user_id);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let destination = destination();
        let pool = pg.pool_for_tests();
        let written = ingest_mcp_call_fact(&pg, &permit, &[]).await?;
        let t = written.memory_id.into_inner();
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                destination,
                &contract_sidecar_tables(),
            )
            .await?
        );

        let source_id = proxima_core::SourceId::new("proxima/fact");
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
            user_id,
            source_id: source_id.clone(),
            drop_event_id: "erase-transferred-audit-source".into(),
        });
        let erased = pg
            .erase_personal_source_scope(&auth, user_id, &source_id, &contract_sidecar_tables())
            .await?;
        assert!(matches!(erased, OwnerEraseOutcome::Completed { .. }));
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint
                   FROM proxima_core.mcp_call_logged_v1
                  WHERE t = $1 AND owner_id = $2",
            )
            .bind(t)
            .bind(owner.stored_owner_id())
            .fetch_one(pool)
            .await?,
            0,
            "the source owner can erase its retained row through source attribution"
        );
        assert_eq!(
            sqlx::query_scalar::<_, Uuid>("SELECT owner_id FROM proxima_core.memory WHERE t = $1",)
                .bind(t)
                .fetch_one(pool)
                .await?,
            destination.stored_owner_id(),
            "source-scope erase does not reach the transferred Memory"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("source-scope retained audit erase failed");
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
    let (db_name, pg, _cold) = fresh_pg_with_cold().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();

        let first = ingest_mcp_call_fact(&pg, &permit, &[]).await?;
        // A live head keeps the series transferable after the first version
        // is cooled.
        let mut later = mcp_draft();
        later.handle = Some(first.handle);
        later.ingest_key = Some("k2".into());
        let second = pg.ingest_fact_atomic(&permit, &later, None).await?;
        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(second.memory_id), dest, &contract_sidecar_tables())
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
        let hydrated = MemoryAuthoringPort::hydrate_memories(&pg, &dest_permit, &[first.memory_id])
            .await?;
        assert_eq!(
            hydrated.outcomes[0].status,
            proxima_core::MemoryHydrationStatus::Hydrated
        );
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
        assert_owner_pinned_payload_redacted(&pg, dest, first.memory_id).await?;

        // Now the destination erases its whole owner. The Memory goes; the
        // source's audit row stays, and the erase does not fail on it.
        let group_id = match dest {
            OwnerRef::Group(group_id) => group_id,
            OwnerRef::Personal(_) => panic!("a transfer destination is a group"),
        };
        let auth =
            EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id });
        let erased = pg
            .erase_group_owner(&auth, group_id, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(erased, OwnerEraseOutcome::Completed { .. }),
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

/// FK safety with no migration-seeded `owners` row to lean on. The destination
/// here has never been written to, so the transfer transaction
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
        let written = pg.ingest_fact_atomic(&permit, &draft(), None).await?;

        let before: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.owners WHERE owner_id = $1",
        )
        .bind(dest.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(before, 0, "the destination owner row must not pre-exist");

        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                dest,
                &contract_sidecar_tables()
            )
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

/// An absent destination must have one owner-row/fence order. Hold the
/// destination fence first, then start a real append: the append must wait on
/// its shared fence before attempting the owner INSERT. The inverse order —
/// INSERT first, then shared fence — would leave an uncommitted owner row
/// between the two transactions and deadlock transfer against append.
#[tokio::test]
async fn absent_destination_append_waits_before_owner_insert() {
    const PROBE_LOCK: i64 = 0x4142_5345_4e54_4f57;

    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let destination = destination();
        let source_permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let destination_permit = OwnerWritePermit::new_for_tests(destination, AccessKind::Fact);
        let written = pg
            .ingest_fact_atomic(&source_permit, &draft(), None)
            .await?;
        let pool = pg.pool_for_tests().clone();

        // The transfer is deliberately held at the destination's first-use
        // owner fence. This creates the inverse-order window in which the old
        // append path inserted the owner before waiting on that fence.
        let mut fence_blocker = pool.begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(
                 hashtextextended('proxima-owner-fence:' || $1 || ':' || $2::text, 0)
             )",
        )
        .bind("group")
        .bind(destination.stored_owner_id())
        .execute(&mut *fence_blocker)
        .await?;

        let transfer_pg = pg.clone();
        let transfer = tokio::spawn(async move {
            transfer_pg
                .transfer_to_owner(
                    &source_permit,
                    EntityId::Memory(written.memory_id),
                    destination,
                    &contract_sidecar_tables(),
                )
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if owner_fence_waiter_count(&pool, destination, "ExclusiveLock").await? >= 1 {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;

        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE SEQUENCE public.absent_owner_insert_probe;
             CREATE FUNCTION public.note_absent_owner_insert() RETURNS trigger
             LANGUAGE plpgsql AS $$
             BEGIN
                 PERFORM nextval('public.absent_owner_insert_probe');
                 PERFORM pg_advisory_xact_lock({PROBE_LOCK});
                 RETURN NEW;
             END
             $$;
             CREATE TRIGGER note_absent_owner_insert
             AFTER INSERT ON proxima_core.owners
             FOR EACH ROW
             EXECUTE FUNCTION public.note_absent_owner_insert();"
        )))
        .execute(&pool)
        .await?;

        let mut probe = pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(PROBE_LOCK)
            .execute(&mut *probe)
            .await?;
        let append_pg = pg.clone();
        let append = tokio::spawn(async move {
            append_pg
                .ingest_fact_atomic(&destination_permit, &draft(), None)
                .await
        });

        // The append's shared owner-fence waiter is the required rendezvous.
        // If it inserted first, the trigger sequence is called and the test
        // fails before releasing the blocker into the deadlock shape.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let inserted: bool =
                    sqlx::query_scalar("SELECT is_called FROM public.absent_owner_insert_probe")
                        .fetch_one(&pool)
                        .await?;
                assert!(
                    !inserted,
                    "append inserted an owner before its shared fence"
                );
                if owner_fence_waiter_count(&pool, destination, "ShareLock").await? >= 1 {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;

        fence_blocker.rollback().await?;
        sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1)")
            .bind(PROBE_LOCK)
            .fetch_one(&mut *probe)
            .await?;
        let (append_result, transfer_result) =
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                tokio::join!(append, transfer)
            })
            .await?;
        append_result??;
        assert!(
            transfer_result??,
            "transfer completes after the append owner row"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("absent destination owner/fence order deadlocked");
}

/// A first-use destination owner is fenced before a write takes any lifecycle
/// target lock. The trigger pauses the real transfer immediately after its
/// owner INSERT, while it still holds the destination fence exclusively; the
/// destination admission waits on that fence in shared mode, and a probe
/// proves T is still freely lockable. Releasing the transfer lets both
/// production transactions finish without an owner/advisory cycle.
#[tokio::test]
async fn destination_owner_admission_waits_before_target_lifecycle_lock() {
    const PROBE_LOCK: i64 = 0x4445_5354_4f57_4e52;

    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let destination = destination();
        let pool = pg.pool_for_tests().clone();
        let written = pg.ingest_fact_atomic(&permit, &draft(), None).await?;
        let target_t = written.memory_id.into_inner();

        sqlx::raw_sql(
            "CREATE SEQUENCE public.transfer_owner_insert_probe;
             CREATE FUNCTION public.block_destination_owner_insert() RETURNS trigger
             LANGUAGE plpgsql AS $$
             BEGIN
                 PERFORM nextval('public.transfer_owner_insert_probe');
                 PERFORM pg_advisory_xact_lock(4919429789545614930);
                 RETURN NEW;
             END
             $$;
             CREATE TRIGGER block_destination_owner_insert
             AFTER INSERT ON proxima_core.owners
             FOR EACH ROW
             EXECUTE FUNCTION public.block_destination_owner_insert();",
        )
        .execute(&pool)
        .await?;
        let mut blocker = pool.acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(PROBE_LOCK)
            .execute(&mut *blocker)
            .await?;

        let transfer_pg = pg.clone();
        let transfer = tokio::spawn(async move {
            let transfer_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
            transfer_pg
                .transfer_to_owner(
                    &transfer_permit,
                    EntityId::Memory(written.memory_id),
                    destination,
                    &contract_sidecar_tables(),
                )
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let called: bool =
                    sqlx::query_scalar("SELECT is_called FROM public.transfer_owner_insert_probe")
                        .fetch_one(&pool)
                        .await?;
                if called {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;

        let admission_pg = pg.clone();
        let admission = tokio::spawn(async move {
            let admission_permit = OwnerWritePermit::new_for_tests(destination, AccessKind::Fact);
            let mut admission_draft = draft();
            admission_draft.refs = vec![target_t];
            admission_pg
                .ingest_fact_atomic(&admission_permit, &admission_draft, None)
                .await
        });
        let mut owner_waiting = false;
        for _ in 0..100 {
            if admission.is_finished() {
                break;
            }
            if owner_fence_waiter_count(&pool, destination, "ShareLock").await? >= 1 {
                owner_waiting = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            owner_waiting && !admission.is_finished(),
            "destination admission must wait on transfer's exclusive owner fence"
        );

        let mut target_probe = pool.begin().await?;
        let target_available: bool = sqlx::query_scalar(
            "SELECT pg_try_advisory_xact_lock(
                 hashtextextended('proxima-forget:' || $1::text, 0)
             )",
        )
        .bind(target_t)
        .fetch_one(&mut *target_probe)
        .await?;
        assert!(
            target_available,
            "owner fencing must precede the destination lifecycle lock"
        );
        target_probe.rollback().await?;

        let unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(PROBE_LOCK)
            .fetch_one(&mut *blocker)
            .await?;
        assert!(unlocked, "the trigger gate must be released");
        drop(blocker);

        let (transfer_result, admission_result) =
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                tokio::join!(transfer, admission)
            })
            .await?;
        assert!(transfer_result??, "the target transfer must commit");
        let admitted = admission_result??;
        assert!(!admitted.idempotent_replay);

        let moved_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(target_t)
                .fetch_one(&pool)
                .await?;
        assert_eq!(moved_owner, destination.stored_owner_id());
        let admitted_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(admitted.memory_id.into_inner())
                .fetch_one(&pool)
                .await?;
        assert_eq!(admitted_owner, destination.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("destination owner/admission ordering test failed");
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
        let written = pg.ingest_fact_atomic(&permit, &draft(), None).await?;

        let err = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                owner,
                &contract_sidecar_tables(),
            )
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
              status, blob_id, sha256, content_hash, expires_at, completed_at)
         VALUES ($1, 'bucket', 'placeholder', 'f.bin', 'application/octet-stream', 3,
                 'completed', $2, $3, $3, now() + interval '1 day', now())
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

/// Add a canonical, same-owner completed row whose blank filename makes the
/// publication metadata unreadable. Every other witness is exact so tests
/// cannot pass because a different validation predicate happened to fail.
async fn seed_malformed_completed_upload(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    blob_id: Uuid,
    hash: &[u8],
) -> Result<Uuid, sqlx::Error> {
    let upload_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
             (upload_id, owner_id, bucket, object_key, filename, mime,
              expected_byte_len, status, blob_id, sha256, content_hash,
              expires_at, completed_at)
         VALUES ($1, $2, 'bucket', 'objects/' || $1::text, '',
                 'application/octet-stream', 3, 'completed', $3, $4, $4,
                 now() + interval '1 day', now() + interval '1 minute')",
    )
    .bind(upload_id)
    .bind(owner.stored_owner_id())
    .bind(blob_id)
    .bind(hash)
    .execute(pool)
    .await?;
    Ok(upload_id)
}

async fn cite(
    pg: &PgStorage,
    permit: &OwnerWritePermit,
    key: &str,
    blob_id: Uuid,
) -> Result<FactIngestOutcome, StorageError> {
    let mut cited = draft();
    cited.ingest_key = Some(key.into());
    cited.blob_id = Some(blob_id);
    pg.ingest_fact_atomic(permit, &cited, None).await
}

/// [`cite`], attributed to a named source so a source-scope erase can
/// select it. `draft()` attributes everything to `src`, which is one scope.
async fn cite_from(
    pg: &PgStorage,
    permit: &OwnerWritePermit,
    source: &str,
    key: &str,
    blob_id: Uuid,
) -> Result<FactIngestOutcome, StorageError> {
    let mut cited = draft();
    cited.source_id = Some(source.into());
    cited.ingest_key = Some(key.into());
    cited.blob_id = Some(blob_id);
    pg.ingest_fact_atomic(permit, &cited, None).await
}

#[tokio::test]
async fn transfer_refuses_a_series_citing_another_owners_blob() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let foreign = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        for owner in [source, foreign] {
            sqlx::query(
                "INSERT INTO proxima_core.owners (owner_id, kind)
                 VALUES ($1, $2::proxima_core.owner_kind)",
            )
            .bind(owner.stored_owner_id())
            .bind(proxima_core::OwnerRefKind::of(&owner).as_str())
            .execute(pool)
            .await?;
        }
        let content_hash = vec![43_u8; 32];
        let source_blob: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/bytes-v1', $2) RETURNING blob_id",
        )
        .bind(source.stored_owner_id())
        .bind(&content_hash)
        .fetch_one(pool)
        .await?;
        let foreign_blob: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/bytes-v1', $2) RETURNING blob_id",
        )
        .bind(foreign.stored_owner_id())
        .bind(&content_hash)
        .fetch_one(pool)
        .await?;
        let written = cite(&pg, &permit, "foreign-blob", source_blob).await?;
        sqlx::query("UPDATE proxima_core.memory SET blob_id = $2 WHERE t = $1")
            .bind(written.memory_id.into_inner())
            .bind(foreign_blob)
            .execute(pool)
            .await?;

        let error = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                destination(),
                &contract_sidecar_tables(),
            )
            .await
            .expect_err("a source series cannot move another owner's blob");
        assert!(
            matches!(error, StorageError::ConstraintViolation(ref message)
                if message.contains("outside its source owner")),
            "the malformed cross-owner citation must fail closed: {error}"
        );
        let (memory_owner, blob_owner): (Uuid, Uuid) = sqlx::query_as(
            "SELECT m.owner_id, b.owner_id
               FROM proxima_core.memory m
               JOIN proxima_core.blob b ON b.blob_id = m.blob_id
              WHERE m.t = $1",
        )
        .bind(written.memory_id.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(memory_owner, source.stored_owner_id());
        assert_eq!(blob_owner, foreign.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cross-owner cited blob transfer refusal failed");
}

#[tokio::test]
async fn transfer_refuses_an_uploaded_blob_with_a_noncanonical_source_locator() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let hash = vec![44_u8; 32];
        let (blob_id, upload_id) = seed_cited_blob(pool, source, &hash).await?;
        sqlx::query(
            "UPDATE proxima_core.blob_uploads
                SET object_key = 'objects/not-the-minting-id'
              WHERE upload_id = $1",
        )
        .bind(upload_id)
        .execute(pool)
        .await?;
        let moving = cite(&pg, &permit, "noncanonical-source-locator", blob_id).await?;

        let error = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(moving.memory_id),
                destination(),
                &contract_sidecar_tables(),
            )
            .await
            .expect_err("a locator not minted by its upload lineage must not transfer");
        assert!(
            matches!(error, StorageError::ConstraintViolation(ref message)
                if message.contains("exact-only completed source-owner publication set")),
            "the malformed source publication must fail closed: {error}"
        );
        let (memory_owner, blob_owner): (Uuid, Uuid) = sqlx::query_as(
            "SELECT m.owner_id, b.owner_id
               FROM proxima_core.memory m
               JOIN proxima_core.blob b ON b.blob_id = m.blob_id
              WHERE m.t = $1",
        )
        .bind(moving.memory_id.into_inner())
        .fetch_one(pool)
        .await?;
        assert_eq!(memory_owner, source.stored_owner_id());
        assert_eq!(blob_owner, source.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("noncanonical source upload transfer refusal failed");
}

#[tokio::test]
async fn transfer_refuses_a_mixed_valid_and_malformed_source_publication_set() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let hash = vec![45_u8; 32];
        let (blob_id, _) = seed_cited_blob(pool, source, &hash).await?;
        let malformed = seed_malformed_completed_upload(pool, source, blob_id, &hash).await?;
        let moving = cite(&pg, &permit, "mixed-source-publications", blob_id).await?;

        let error = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(moving.memory_id),
                destination(),
                &contract_sidecar_tables(),
            )
            .await
            .expect_err("one valid upload row must not mask a malformed completed row");
        assert!(
            matches!(error, StorageError::ConstraintViolation(ref message)
                if message.contains("exact-only completed source-owner publication set")),
            "the mixed source publication set must fail closed: {error}"
        );
        let (memory_owner, malformed_owner): (Uuid, Uuid) = sqlx::query_as(
            "SELECT m.owner_id, u.owner_id
               FROM proxima_core.memory m
               JOIN proxima_core.blob_uploads u ON u.upload_id = $2
              WHERE m.t = $1",
        )
        .bind(moving.memory_id.into_inner())
        .bind(malformed)
        .fetch_one(pool)
        .await?;
        assert_eq!(memory_owner, source.stored_owner_id());
        assert_eq!(malformed_owner, source.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("mixed source publication transfer refusal failed");
}

#[tokio::test]
async fn transfer_refuses_an_unpublished_destination_dedupe_blob() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let destination = destination();
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let hash = vec![45_u8; 32];
        let (source_blob, _) = seed_cited_blob(pool, source, &hash).await?;
        let moving = cite(&pg, &permit, "unpublished-destination-dedupe", source_blob).await?;

        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, $2::proxima_core.owner_kind)",
        )
        .bind(destination.stored_owner_id())
        .bind(proxima_core::OwnerRefKind::of(&destination).as_str())
        .execute(pool)
        .await?;
        let destination_blob: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
             VALUES ($1, 'core/uploaded-blob-v1', $2)
             RETURNING blob_id",
        )
        .bind(destination.stored_owner_id())
        .bind(&hash)
        .fetch_one(pool)
        .await?;

        let error = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(moving.memory_id),
                destination,
                &contract_sidecar_tables(),
            )
            .await
            .expect_err("dedupe must not remap onto a blob without a readable publication");
        assert!(
            matches!(error, StorageError::ConstraintViolation(ref message)
                if message.contains("destination uploaded blob does not have an exact-only")),
            "the unreadable destination dedupe row must fail closed: {error}"
        );
        let (memory_owner, cited_blob): (Uuid, Uuid) =
            sqlx::query_as("SELECT owner_id, blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(moving.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(memory_owner, source.stored_owner_id());
        assert_eq!(cited_blob, source_blob);
        let destination_blob_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.blob WHERE blob_id = $1")
                .bind(destination_blob)
                .fetch_one(pool)
                .await?;
        assert_eq!(destination_blob_owner, destination.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("unpublished destination dedupe refusal failed");
}

#[tokio::test]
async fn transfer_refuses_a_mixed_valid_and_malformed_destination_publication_set() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let destination = destination();
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let hash = vec![46_u8; 32];
        let (source_blob, _) = seed_cited_blob(pool, source, &hash).await?;
        let moving = cite(&pg, &permit, "mixed-destination-publications", source_blob).await?;
        let (destination_blob, _) = seed_cited_blob(pool, destination, &hash).await?;
        let malformed =
            seed_malformed_completed_upload(pool, destination, destination_blob, &hash).await?;

        let error = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(moving.memory_id),
                destination,
                &contract_sidecar_tables(),
            )
            .await
            .expect_err("destination dedupe must reject a mixed publication set");
        assert!(
            matches!(error, StorageError::ConstraintViolation(ref message)
                if message.contains("destination uploaded blob does not have an exact-only")),
            "the mixed destination publication set must fail closed: {error}"
        );
        let (memory_owner, cited_blob): (Uuid, Uuid) =
            sqlx::query_as("SELECT owner_id, blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(moving.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(memory_owner, source.stored_owner_id());
        assert_eq!(cited_blob, source_blob);
        let malformed_owner: Uuid = sqlx::query_scalar(
            "SELECT owner_id FROM proxima_core.blob_uploads WHERE upload_id = $1",
        )
        .bind(malformed)
        .fetch_one(pool)
        .await?;
        assert_eq!(malformed_owner, destination.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("mixed destination publication transfer refusal failed");
}

#[tokio::test]
async fn a_blob_mount_copies_only_source_owned_upload_rows() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let foreign = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let destination = destination();
        let pool = pg.pool_for_tests();
        let hash = vec![47_u8; 32];
        let (blob_id, _) = seed_cited_blob(pool, source, &hash).await?;
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, $2::proxima_core.owner_kind)",
        )
        .bind(foreign.stored_owner_id())
        .bind(proxima_core::OwnerRefKind::of(&foreign).as_str())
        .execute(pool)
        .await?;
        let foreign_upload = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                (upload_id, owner_id, bucket, object_key, filename, mime,
                 expected_byte_len, status, blob_id, sha256, content_hash,
                 expires_at, completed_at)
             VALUES ($1, $2, 'foreign', 'objects/foreign', 'foreign.bin',
                     'application/octet-stream', 3, 'completed', $3, $4, $4,
                     now() + interval '1 day', now())",
        )
        .bind(foreign_upload)
        .bind(foreign.stored_owner_id())
        .bind(blob_id)
        .bind(&hash)
        .execute(pool)
        .await?;

        let moving = cite(&pg, &permit, "mount-owned", blob_id).await?;
        let _source_shadow = cite(&pg, &permit, "mount-owned-shadow", blob_id).await?;
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(moving.memory_id),
                destination,
                &contract_sidecar_tables(),
            )
            .await?
        );
        let moved_blob: Uuid =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(moving.memory_id.into_inner())
                .fetch_one(pool)
                .await?;
        let destination_uploads: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM proxima_core.blob_uploads
              WHERE blob_id = $1 AND owner_id = $2",
        )
        .bind(moved_blob)
        .bind(destination.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(
            destination_uploads, 1,
            "only the source owner's exact publication may be mounted"
        );
        let foreign_owner: Uuid = sqlx::query_scalar(
            "SELECT owner_id FROM proxima_core.blob_uploads WHERE upload_id = $1",
        )
        .bind(foreign_upload)
        .fetch_one(pool)
        .await?;
        assert_eq!(foreign_owner, foreign.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("source-owned upload mount failed");
}

#[tokio::test]
async fn an_in_place_blob_move_refuses_a_foreign_upload_pointer() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let foreign = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(source, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let hash = vec![53_u8; 32];
        let (blob_id, _) = seed_cited_blob(pool, source, &hash).await?;
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, $2::proxima_core.owner_kind)",
        )
        .bind(foreign.stored_owner_id())
        .bind(proxima_core::OwnerRefKind::of(&foreign).as_str())
        .execute(pool)
        .await?;
        let foreign_upload = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                (upload_id, owner_id, bucket, object_key, filename, mime,
                 expected_byte_len, status, blob_id, sha256, content_hash,
                 expires_at, completed_at)
             VALUES ($1, $2, 'foreign', 'objects/foreign-in-place', 'foreign.bin',
                     'application/octet-stream', 3, 'completed', $3, $4, $4,
                     now() + interval '1 day', now())",
        )
        .bind(foreign_upload)
        .bind(foreign.stored_owner_id())
        .bind(blob_id)
        .bind(&hash)
        .execute(pool)
        .await?;
        let moving = cite(&pg, &permit, "foreign-upload-in-place", blob_id).await?;

        let error = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(moving.memory_id),
                destination(),
                &contract_sidecar_tables(),
            )
            .await
            .expect_err("a malformed foreign upload pointer must stop an in-place move");
        assert!(
            matches!(error, StorageError::ConstraintViolation(ref message)
                if message.contains("upload row outside its source owner")),
            "the move must fail closed rather than strand the foreign row: {error}"
        );
        let (memory_owner, blob_owner, foreign_upload_owner): (Uuid, Uuid, Uuid) = sqlx::query_as(
            "SELECT m.owner_id, b.owner_id, u.owner_id
                   FROM proxima_core.memory m
                   JOIN proxima_core.blob b ON b.blob_id = m.blob_id
                   JOIN proxima_core.blob_uploads u ON u.upload_id = $2
                  WHERE m.t = $1",
        )
        .bind(moving.memory_id.into_inner())
        .bind(foreign_upload)
        .fetch_one(pool)
        .await?;
        assert_eq!(memory_owner, source.stored_owner_id());
        assert_eq!(blob_owner, source.stored_owner_id());
        assert_eq!(foreign_upload_owner, foreign.stored_owner_id());
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("foreign upload in-place move refusal failed");
}

/// The dedupe arm, in place of a refusal.
///
/// One owner cites the same uploaded document from two series and then
/// transfers one of them away. A refcount predicate spelled
/// `owner_id <> <destination>` is satisfied by the SOURCE owner, so it refuses
/// an owner citing its own document twice — which is what happens the second
/// time anyone cites a PDF they already uploaded.
///
/// Instead the destination gets a `blob` row of its own and an upload row that
/// MOUNTS the source's object: same key, no bytes read or written. The source
/// keeps everything it had, because its other series still cites it.
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

        let mine = cite(&pg, &permit, "shared-mine", blob_id).await?;
        // A second series cites the very same blob row. This is what used
        // to make the transfer below impossible. It stays behind, which is
        // why the row cannot simply change hands.
        let _also_mine = cite(&pg, &permit, "shared-also-mine", blob_id).await?;

        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(mine.memory_id),
                dest,
                &contract_sidecar_tables()
            )
            .await?,
            "a shared cited blob must not refuse the transfer"
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
        let written = cite(&pg, &permit, "solo", blob_id).await?;
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                dest,
                &contract_sidecar_tables()
            )
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
/// destination already uploaded the same bytes, an in-place
/// `UPDATE blob SET owner_id = <dest>` collides with the row already sitting
/// there and fails the whole transfer on a constraint the caller can do nothing
/// about. The destination's own row wins instead, and this series' citation is
/// repointed at it.
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

        let written = cite(&pg, &permit, "collide", source_blob).await?;
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(written.memory_id),
                dest,
                &contract_sidecar_tables()
            )
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
        let mine = cite(&pg, &permit, "chain-mine", blob_id).await?;
        let _also_mine = cite(&pg, &permit, "chain-also-mine", blob_id).await?;

        // Hop one: shared, so the destination mounts.
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(mine.memory_id),
                first,
                &contract_sidecar_tables()
            )
            .await?
        );
        let hop_one: Uuid =
            sqlx::query_scalar("SELECT blob_id FROM proxima_core.memory WHERE t = $1")
                .bind(mine.memory_id.into_inner())
                .fetch_one(pool)
                .await?;

        // Make hop one's row shared too, so hop two mounts rather than moves.
        let first_permit = OwnerWritePermit::new_for_tests(first, AccessKind::Fact);
        let shadow = cite(&pg, &first_permit, "chain-shadow", hop_one).await?;
        let _ = shadow;
        assert!(
            pg.transfer_to_owner(
                &first_permit,
                EntityId::Memory(mine.memory_id),
                second,
                &contract_sidecar_tables()
            )
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

        let mine = cite(&pg, &permit, "purge-mine", blob_id).await?;
        let _also_mine = cite(&pg, &permit, "purge-also-mine", blob_id).await?;
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(mine.memory_id),
                dest,
                &contract_sidecar_tables()
            )
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
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id });
        let erased = pg
            .erase_group_owner(&auth, group_id, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(erased, OwnerEraseOutcome::Completed { .. }),
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
        let last = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id,
            drop_event_id: "drop-1".into(),
        });
        let erased = pg
            .erase_personal_owner(&last, user_id, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(erased, OwnerEraseOutcome::Completed { .. }),
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

/// The same object, two owners, both erasing at once: the bytes go exactly
/// once.
///
/// The sequential arm above passes on a refcount read against a stale
/// snapshot. Under READ COMMITTED two concurrent erases each see the other's
/// `blob_uploads` row, each concludes "someone else still names it", and both
/// withhold — the object is destroyed zero times and nothing is left pointing
/// at it. The object-key lock is what makes the anti-join a decision rather
/// than an observation: the second erase runs its refcount after the first
/// has committed its deletion.
///
/// The rendezvous is the proof that the lock is the one being taken. A third
/// session holds `proxima-object-key:<key>` and both erases must queue behind
/// it; drop that lock from the erase path and the wait never happens, so this
/// test fails at the barrier rather than on the byte count.
#[tokio::test]
async fn concurrent_erases_of_a_mounted_object_destroy_its_bytes_exactly_once() {
    let (db_name, pg, cold) = fresh_pg_with_counting_cold().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();

        let (blob_id, upload_id) = seed_cited_blob(pool, owner, &[17_u8; 32]).await?;
        let object_key = format!("objects/{upload_id}");
        cold.put(&object_key, b"the contested bytes").await?;

        let mine = cite(&pg, &permit, "race-mine", blob_id).await?;
        let _also_mine = cite(&pg, &permit, "race-also-mine", blob_id).await?;
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(mine.memory_id),
                dest,
                &contract_sidecar_tables()
            )
            .await?
        );
        let mounts: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.blob_uploads WHERE object_key = $1",
        )
        .bind(&object_key)
        .fetch_one(pool)
        .await?;
        assert_eq!(mounts, 2, "two owners now name one object");

        // Hold the object's key so neither erase can decide until both are
        // in flight and past their own owner fences.
        let mut barrier = pool.begin().await?;
        sqlx::query(
            "SELECT pg_advisory_xact_lock(
                 hashtextextended('proxima-object-key:' || $1, 0)
             )",
        )
        .bind(&object_key)
        .execute(&mut *barrier)
        .await?;

        let group_id = match dest {
            OwnerRef::Group(group_id) => group_id,
            OwnerRef::Personal(_) => panic!("a transfer destination is a group"),
        };
        let user_id = match owner {
            OwnerRef::Personal(user_id) => user_id,
            OwnerRef::Group(_) => panic!("seeded as a personal owner"),
        };
        let group_pg = pg.clone();
        let group_erase = tokio::spawn(async move {
            let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id });
            group_pg
                .erase_group_owner(&auth, group_id, &contract_sidecar_tables())
                .await
        });
        let personal_pg = pg.clone();
        let personal_erase = tokio::spawn(async move {
            let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
                user_id,
                drop_event_id: "race-drop".into(),
            });
            personal_pg
                .erase_personal_owner(&auth, user_id, &contract_sidecar_tables())
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if object_key_waiter_count(pool, &object_key).await? >= 2 {
                    return Ok::<(), sqlx::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        barrier.rollback().await?;

        let group_outcome = group_erase.await??;
        let personal_outcome = personal_erase.await??;
        assert!(
            matches!(group_outcome, OwnerEraseOutcome::Completed { .. }),
            "the group's erase completes: {group_outcome:?}"
        );
        assert!(
            matches!(personal_outcome, OwnerEraseOutcome::Completed { .. }),
            "the personal owner's erase completes: {personal_outcome:?}"
        );

        let survivors: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.blob_uploads WHERE object_key = $1",
        )
        .bind(&object_key)
        .fetch_one(pool)
        .await?;
        assert_eq!(survivors, 0, "no row is left naming the object");
        assert_eq!(
            cold.deletes_of(&object_key),
            1,
            "exactly one of the two erases owed the bytes, and paid"
        );
        assert!(
            matches!(
                cold.get(&object_key).await,
                Err(proxima_core::StorageError::NotFound)
            ),
            "the object is destroyed, not orphaned"
        );
        let pending: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cold_purge_pending")
                .fetch_one(pool)
                .await?;
        assert_eq!(pending, 0, "the discharged debt leaves no queue row");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("concurrent refcounted object purge failed");
}

/// The same guarantee, on the source-scope arm.
///
/// `enqueue_blob_object_keys` has two arms and they refcount differently: the
/// owner arm asks whether another OWNER names the key, the source arm asks
/// whether another upload row outside the selected blob set does. The source
/// arm needs its own pin — without one, dropping its whole `NOT EXISTS` leaves
/// the suite green while a source-scope erase enqueues the object key of a blob
/// another owner has mounted, and the retention lane then destroys bytes that
/// owner still reads. This is that arm.
///
/// The shape is the one a mount actually produces: one owner cites an
/// upload from two sources, hands one series to a group (which mounts
/// rather than copies), then erases the source scope it kept. The blob the
/// erase selects is the last thing IT has naming the object — and the mount
/// is the thing that must stop the object going.
#[tokio::test]
async fn erasing_one_source_scope_of_a_mounted_object_does_not_destroy_the_bytes() {
    let (db_name, pg, cold) = fresh_pg_with_cold().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let user_id = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user_id);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();

        let (blob_id, upload_id) = seed_cited_blob(pool, owner, &[23_u8; 32]).await?;
        let object_key = format!("objects/{upload_id}");
        cold.put(&object_key, b"bytes two owners read").await?;

        let dropped = cite_from(&pg, &permit, "src-drop", "scope-drop", blob_id).await?;
        let handed_over = cite_from(&pg, &permit, "src-keep", "scope-keep", blob_id).await?;
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(handed_over.memory_id),
                dest,
                &contract_sidecar_tables()
            )
            .await?
        );
        let mounts: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.blob_uploads WHERE object_key = $1",
        )
        .bind(&object_key)
        .fetch_one(pool)
        .await?;
        assert_eq!(mounts, 2, "the transfer mounted rather than copied");
        let _ = dropped;

        // The source owner erases the scope that holds its only remaining
        // citation. Its blob row and upload row go; the object must not.
        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
            user_id,
            source_id: proxima_core::SourceId::new("src-drop"),
            drop_event_id: "drop-source-mounted".into(),
        });
        let outcome = pg
            .erase_personal_source_scope(
                &auth,
                user_id,
                &proxima_core::SourceId::new("src-drop"),
                &contract_sidecar_tables(),
            )
            .await?;
        assert!(
            matches!(outcome, OwnerEraseOutcome::Completed { .. }),
            "the source-scope erase completes: {outcome:?}"
        );
        let source_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.blob_uploads
              WHERE object_key = $1 AND owner_id = $2",
        )
        .bind(&object_key)
        .bind(owner.stored_owner_id())
        .fetch_one(pool)
        .await?;
        assert_eq!(source_rows, 0, "the erased scope's own upload row goes");
        assert_eq!(
            cold.get(&object_key).await?,
            b"bytes two owners read".to_vec(),
            "the object the destination mounted survives the source's scope erase"
        );

        // The mount is now the only thing naming the object. When it goes,
        // the object goes: the anti-join withholds keys, it does not leak
        // them.
        let group_id = match dest {
            OwnerRef::Group(group_id) => group_id,
            OwnerRef::Personal(_) => panic!("a transfer destination is a group"),
        };
        let last = EraseAuthorization::new_for_tests(OwnerEraseTarget::GroupOwner { group_id });
        let outcome = pg
            .erase_group_owner(&last, group_id, &contract_sidecar_tables())
            .await?;
        assert!(
            matches!(outcome, OwnerEraseOutcome::Completed { .. }),
            "the destination's erase completes: {outcome:?}"
        );
        assert!(
            matches!(
                cold.get(&object_key).await,
                Err(proxima_core::StorageError::NotFound)
            ),
            "with the last row naming it gone, the object is destroyed"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("source-scope refcounted object purge failed");
}

/// A flavor-scope erase must take the blobs its admissions cited.
///
/// `erase_memory_series` is presented as the complete inverse a flavor gets
/// for free, and the code flavor's entire citation story is blobs. It read
/// `blob_id` into its hot row and used it for nothing: erasing a repository
/// left the `blob` row, the `blob_uploads` row and the S3 object behind,
/// with no admission left anywhere to reach them from. Nothing would ever
/// have collected them — owner erase is the only other thing that
/// deletes blobs, and it works by owner or by source, not by flavor scope.
///
/// Both directions, because a refcount that only ever says "delete" is not
/// a refcount: the bytes go when this was the last reference, and stay when
/// another owner mounted the same object.
#[tokio::test]
async fn erasing_the_last_admission_citing_a_blob_takes_the_blob_and_owes_its_bytes() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();

        let (blob_id, upload_id) = seed_cited_blob(pool, owner, &[31_u8; 32]).await?;
        let object_key = format!("objects/{upload_id}");
        let mine = cite(&pg, &permit, "series-erase", blob_id).await?;
        let mine_again = cite(&pg, &permit, "series-erase-two", blob_id).await?;

        let mut tx = pool.begin().await?;
        let (erased, plan) = erase_memory_series(
            &mut tx,
            &core_pg_sidecars(),
            &contract_sidecar_tables(),
            &owner,
            &[
                mine.memory_id.into_inner(),
                mine_again.memory_id.into_inner(),
            ],
        )
        .await?;
        tx.commit().await?;
        assert_eq!(erased, 2);
        for erased_t in [
            mine.memory_id.into_inner(),
            mine_again.memory_id.into_inner(),
        ] {
            let witness: Option<String> = sqlx::query_scalar(
                "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
            )
            .bind(erased_t)
            .fetch_optional(pool)
            .await?;
            assert_eq!(
                witness.as_deref(),
                Some("fact"),
                "bulk series erase records every erased Memory kind"
            );
        }
        assert_eq!(
            plan.object_keys(),
            std::slice::from_ref(&object_key),
            "the erase owes the object store the bytes of the blob it just orphaned"
        );

        let leftovers: Vec<String> = sqlx::query_scalar(
            "SELECT 'blob' AS relation FROM proxima_core.blob WHERE blob_id = $1
             UNION ALL
             SELECT 'blob_uploads' FROM proxima_core.blob_uploads WHERE blob_id = $1
             ORDER BY relation",
        )
        .bind(blob_id)
        .fetch_all(pool)
        .await?;
        assert!(
            leftovers.is_empty(),
            "no row of a blob nothing cites survives the erase: {leftovers:?}"
        );
        let queued: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.cold_purge_pending WHERE object_key = $1",
        )
        .bind(&object_key)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            queued, 1,
            "and the durable purge record is there for the retention lane to drain"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("series erase blob leg failed");
}

/// The other direction: the object another owner mounted stays.
#[tokio::test]
async fn a_series_erase_does_not_owe_bytes_another_owner_mounted() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let dest = destination();
        let pool = pg.pool_for_tests();

        let (blob_id, upload_id) = seed_cited_blob(pool, owner, &[37_u8; 32]).await?;
        let object_key = format!("objects/{upload_id}");
        let mine = cite(&pg, &permit, "series-mine", blob_id).await?;
        let handed_over = cite(&pg, &permit, "series-handed-over", blob_id).await?;
        assert!(
            pg.transfer_to_owner(
                &permit,
                EntityId::Memory(handed_over.memory_id),
                dest,
                &contract_sidecar_tables()
            )
            .await?
        );

        let mut tx = pool.begin().await?;
        let (erased, plan) = erase_memory_series(
            &mut tx,
            &core_pg_sidecars(),
            &contract_sidecar_tables(),
            &owner,
            &[mine.memory_id.into_inner()],
        )
        .await?;
        tx.commit().await?;
        assert_eq!(erased, 1);
        let witness: Option<String> = sqlx::query_scalar(
            "SELECT kind::text FROM proxima_core.erased_pin_target WHERE t = $1",
        )
        .bind(mine.memory_id.into_inner())
        .fetch_optional(pool)
        .await?;
        assert_eq!(
            witness.as_deref(),
            Some("fact"),
            "series erase records the erased hot Memory kind"
        );
        assert!(
            plan.object_keys().is_empty(),
            "the destination mounted this object; the erase owes nothing: {:?}",
            plan.object_keys()
        );

        let source_rows: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.blob WHERE blob_id = $1")
                .bind(blob_id)
                .fetch_one(pool)
                .await?;
        assert_eq!(
            source_rows, 0,
            "the source's own blob row still goes — it is the OBJECT that is shared"
        );
        let mount_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.blob_uploads WHERE object_key = $1",
        )
        .bind(&object_key)
        .fetch_one(pool)
        .await?;
        assert_eq!(mount_rows, 1, "the destination's mount is untouched");
        let queued: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.cold_purge_pending WHERE object_key = $1",
        )
        .bind(&object_key)
        .fetch_one(pool)
        .await?;
        assert_eq!(
            queued, 0,
            "nothing enqueued the bytes a live owner still reads"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("series erase mount refcount failed");
}

/// The blob leg's lock, from a second session.
///
/// `gc_unreferenced_blobs` asks "does any admission still cite this blob"
/// against its own snapshot and then deletes; the delete is enforced
/// against the latest one. A citation committed between the two makes the
/// delete wait on the citer's `FOR KEY SHARE` and then raise `23503`,
/// which aborts the entire erase — a repo teardown lost to someone
/// uploading a document at the wrong moment.
///
/// `cited_blobs_locked_for_erase` closes it by taking `FOR UPDATE` on every
/// candidate before the erase loop runs, so the citer waits and then fails
/// its own foreign key in its own transaction. This is what says the lock
/// is really there: the same citation lands immediately when nothing holds
/// the blob and is still waiting at the statement timeout when the erase
/// does.
///
/// `FOR NO KEY UPDATE` would NOT pass this test, which is the point of
/// spelling the mode out: it permits concurrent key-share holders, so the
/// citation below would land and the race would be exactly where it was.
#[tokio::test]
async fn a_cited_blob_is_held_against_a_concurrent_citation() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();

        let (blob_id, _upload) = seed_cited_blob(pool, owner, &[31_u8; 32]).await?;
        let cited = cite(&pg, &permit, "held-blob", blob_id).await?;

        // A second session whose statements give up rather than hang. Same
        // storage, its own pool: the per-connection statement timeout is
        // pool policy, so a second `PgStorage` IS the second session.
        let writer = PgStorage::connect_with_config(
            &db_url(&db_name),
            proxima_storage_pg::PgPoolConfig {
                statement_timeout: std::time::Duration::from_millis(2500),
                ..proxima_storage_pg::PgPoolConfig::default()
            },
            proxima_storage_pg::PgTuning::default(),
        )
        .await?;

        // Nothing held: citing the same blob is an ordinary write.
        cite(&writer, &permit, "before-the-lock", blob_id)
            .await
            .expect("with no erase in flight a second citation is an ordinary write");

        let mut tx = pool.begin().await?;
        let held = proxima_storage_pg::verbs::forget::cited_blobs_locked_for_erase(
            &mut tx,
            &owner,
            &[cited.memory_id.into_inner()],
        )
        .await?;
        assert_eq!(
            held,
            vec![blob_id],
            "the erase holds exactly the blob its admissions cite"
        );

        let err = cite(&writer, &permit, "during-the-lock", blob_id)
            .await
            .expect_err("a citation of a held blob must wait, not land");
        assert!(
            err.to_string().contains("statement timeout"),
            "the citation should have blocked on the erase's row lock until the \
             statement timeout cancelled it; instead it failed with {err}"
        );

        tx.rollback().await?;
        cite(&writer, &permit, "after-the-lock", blob_id)
            .await
            .expect("once the erase is gone the citation lands");
        writer.pool_for_tests().close().await;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("a_cited_blob_is_held_against_a_concurrent_citation failed");
}
