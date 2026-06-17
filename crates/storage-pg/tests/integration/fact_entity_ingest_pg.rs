//! Task 2 fact-entity derivation and head-pointer ingest coverage.

#![allow(clippy::too_many_lines)]

use std::sync::Arc;

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::engine::Engine;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::EventDraft;
use proxima_core::{
    AuthPath, AuthzContext, FactPayload, FlavorRegistry, FlavorRegistryFrozen, OrgId, Owner,
    Principal, Role, SchemaVersion, SourceBatchId, SourceId, StorageError, UserId,
    canonical_json_bytes,
};
use proxima_storage_pg::PgStorage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileRevisionV1 {
    repo_id: Uuid,
    file_path: String,
    language: Option<String>,
    content_sha256: String,
    size_bytes: i64,
    indexed_commit_sha: String,
    state: String,
}

impl FactPayload for FileRevisionV1 {
    const SCHEMA_ID: &'static str = "proxima-code/file-revision-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!("{} @ {}", self.file_path, self.indexed_commit_sha)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.file_revision_v1")
    }

    fn natural_key_columns() -> &'static [&'static str] {
        &["repo_id", "file_path"]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileRevisionV2 {
    repo_id: Uuid,
    file_path: String,
    language: Option<String>,
    content_sha256: String,
    size_bytes: i64,
    indexed_commit_sha: String,
    state: String,
}

impl FactPayload for FileRevisionV2 {
    const SCHEMA_ID: &'static str = FileRevisionV1::SCHEMA_ID;
    const SCHEMA_VERSION: u32 = 2;

    fn render(&self) -> String {
        format!("{} @ {}", self.file_path, self.indexed_commit_sha)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.file_revision_v1")
    }

    fn natural_key_columns() -> &'static [&'static str] {
        &["repo_id", "file_path"]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CodeChunkV1 {
    repo_id: Uuid,
    file_path: String,
    chunk_index: i32,
    text: String,
    language: Option<String>,
    chunk_type: String,
    byte_range_start: i64,
    byte_range_end: i64,
    line_range_start: i64,
    line_range_end: i64,
    state: String,
}

impl FactPayload for CodeChunkV1 {
    const SCHEMA_ID: &'static str = "proxima-code/code-chunk-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!(
            "{}:{}-{}",
            self.file_path, self.line_range_start, self.line_range_end
        )
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.code_chunk_v1")
    }

    fn natural_key_columns() -> &'static [&'static str] {
        &["repo_id", "file_path", "chunk_index"]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CommitV1 {
    repo_id: Uuid,
    sha: String,
    message: String,
}

impl FactPayload for CommitV1 {
    const SCHEMA_ID: &'static str = "proxima-code/commit-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!("{} {}", self.sha, self.message)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.commit_v1")
    }
}

fn registry_for_test() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema::<FileRevisionV1>();
    registry.add_fact_schema::<FileRevisionV2>();
    registry.add_fact_schema::<CodeChunkV1>();
    registry.add_fact_schema::<CommitV1>();
    registry.freeze()
}

async fn create_code_sidecars(pg: &PgStorage) -> Result<(), sqlx::Error> {
    for sql in [
        "CREATE SCHEMA proxima_code",
        "CREATE TABLE proxima_code.file_revision_v1 (
            memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
            repo_id uuid NOT NULL,
            file_path text NOT NULL,
            language text,
            content_sha256 text NOT NULL,
            size_bytes bigint NOT NULL,
            indexed_commit_sha text NOT NULL,
            state text NOT NULL
        )",
        "CREATE TABLE proxima_code.code_chunk_v1 (
            memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
            repo_id uuid NOT NULL,
            file_path text NOT NULL,
            chunk_index integer NOT NULL,
            text text NOT NULL,
            language text,
            chunk_type text NOT NULL,
            byte_range_start bigint NOT NULL,
            byte_range_end bigint NOT NULL,
            line_range_start bigint NOT NULL,
            line_range_end bigint NOT NULL,
            state text NOT NULL
        )",
        "CREATE TABLE proxima_code.commit_v1 (
            memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
            repo_id uuid NOT NULL,
            sha text NOT NULL,
            message text NOT NULL
        )",
    ] {
        sqlx::query(sql).execute(pg.pool()).await?;
    }
    Ok(())
}

fn engine_for(pg: &PgStorage) -> Engine {
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    Engine::new(registry_for_test()).with_storage(storage)
}

fn owner_with(user: UserId, org_id: OrgId) -> Owner {
    Owner {
        principal: Principal::User(user),
        org_id,
    }
}

fn draft_for<P: FactPayload>(owner: &Owner, payload_value: &Value) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new(format!("test/fact-entity/{}", Uuid::now_v7())),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
        author_personality_instance_id: None,
        schema_id: P::schema_id(),
        schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
        payload: canonical_json_bytes(payload_value),
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: None,
    }
}

async fn ingest_payload<P>(
    pg: &PgStorage,
    engine: &Engine,
    owner: &Owner,
    payload: &P,
) -> Result<proxima_core::EventIngestOutcome, StorageError>
where
    P: FactPayload,
{
    let payload_value =
        serde_json::to_value(payload).map_err(|err| StorageError::Internal(err.to_string()))?;
    let draft = draft_for::<P>(owner, &payload_value);
    let authz = AuthzContext::single_owner(owner, AuthPath::System);
    let authorized = engine
        .authorize_event_ingest(&authz, Role::SourceIngest, draft)
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    pg.ingest_event_with_sidecar(
        &authorized,
        P::sidecar_table().expect("test payloads have sidecars"),
        &payload_value,
        None,
    )
    .await
}

fn file_revision(repo_id: Uuid, file_path: &str, version: &str) -> FileRevisionV1 {
    FileRevisionV1 {
        repo_id,
        file_path: file_path.to_string(),
        language: Some("rust".to_string()),
        content_sha256: format!("{version:0<64}"),
        size_bytes: 42,
        indexed_commit_sha: version.to_string(),
        state: "Present".to_string(),
    }
}

fn file_revision_v2(repo_id: Uuid, file_path: &str, version: &str) -> FileRevisionV2 {
    FileRevisionV2 {
        repo_id,
        file_path: file_path.to_string(),
        language: Some("rust".to_string()),
        content_sha256: format!("{version:0<64}"),
        size_bytes: 42,
        indexed_commit_sha: version.to_string(),
        state: "Present".to_string(),
    }
}

fn code_chunk(repo_id: Uuid, file_path: &str, chunk_index: i32, text: &str) -> CodeChunkV1 {
    CodeChunkV1 {
        repo_id,
        file_path: file_path.to_string(),
        chunk_index,
        text: text.to_string(),
        language: Some("rust".to_string()),
        chunk_type: "function".to_string(),
        byte_range_start: 0,
        byte_range_end: 10,
        line_range_start: 1,
        line_range_end: 2,
        state: "Present".to_string(),
    }
}

fn commit(repo_id: Uuid, sha: &str) -> CommitV1 {
    CommitV1 {
        repo_id,
        sha: sha.to_string(),
        message: format!("commit {sha}"),
    }
}

fn file_revision_natural_key(repo_id: Uuid, file_path: &str) -> Value {
    json!([repo_id.to_string(), file_path])
}

fn code_chunk_natural_key(repo_id: Uuid, file_path: &str, chunk_index: i32) -> Value {
    json!([repo_id.to_string(), file_path, chunk_index])
}

async fn memory_fact_entity_id(
    pg: &PgStorage,
    memory_id: proxima_core::MemoryId,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT fact_entity_id
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pg.pool())
    .await
}

async fn entity_head(pg: &PgStorage, fact_entity_id: Uuid) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT current_memory_id
           FROM proxima_core.fact_entities
          WHERE fact_entity_id = $1",
    )
    .bind(fact_entity_id)
    .fetch_one(pg.pool())
    .await
}

async fn entity_count(pg: &PgStorage) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.fact_entities")
        .fetch_one(pg.pool())
        .await
}

#[tokio::test]
async fn stateful_ingest_derives_entity_and_sets_memory_fk() {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await.expect("migrate core");
    create_code_sidecars(&pg).await.expect("create sidecars");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(&pg);
        let repo_id = Uuid::now_v7();
        let payload = file_revision(repo_id, "src/lib.rs", "v1");
        let outcome = ingest_payload(&pg, &engine, &owner, &payload).await?;

        let fact_entity_id = memory_fact_entity_id(&pg, outcome.memory_id)
            .await?
            .expect("stateful memory has fact_entity_id");
        let row: (Value, Uuid) = sqlx::query_as(
            "SELECT natural_key, current_memory_id
               FROM proxima_core.fact_entities
              WHERE fact_entity_id = $1",
        )
        .bind(fact_entity_id)
        .fetch_one(pg.pool())
        .await?;
        let natural_key = file_revision_natural_key(repo_id, "src/lib.rs");
        assert_eq!(row.0, natural_key);
        assert_eq!(row.1, outcome.memory_id.into_inner());

        let lookup = pg
            .fact_entity_id_for(
                &owner,
                &FileRevisionV1::schema_id(),
                SchemaVersion::new(FileRevisionV1::SCHEMA_VERSION),
                &natural_key,
            )
            .await?;
        assert_eq!(
            lookup.map(proxima_core::FactEntityId::into_inner),
            Some(fact_entity_id)
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("stateful_ingest_derives_entity_and_sets_memory_fk failed");
}

#[tokio::test]
async fn stateless_fact_skips_entity_derivation() {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await.expect("migrate core");
    create_code_sidecars(&pg).await.expect("create sidecars");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(&pg);
        let outcome = ingest_payload(&pg, &engine, &owner, &commit(Uuid::now_v7(), "abc")).await?;
        assert_eq!(memory_fact_entity_id(&pg, outcome.memory_id).await?, None);
        assert_eq!(entity_count(&pg).await?, 0);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("stateless_fact_skips_entity_derivation failed");
}

#[tokio::test]
async fn stateful_head_advances_to_newer_version() {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await.expect("migrate core");
    create_code_sidecars(&pg).await.expect("create sidecars");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(&pg);
        let repo_id = Uuid::now_v7();
        let first = ingest_payload(
            &pg,
            &engine,
            &owner,
            &file_revision(repo_id, "src/lib.rs", "v1"),
        )
        .await?;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let second = ingest_payload(
            &pg,
            &engine,
            &owner,
            &file_revision(repo_id, "src/lib.rs", "v2"),
        )
        .await?;

        let first_entity = memory_fact_entity_id(&pg, first.memory_id)
            .await?
            .expect("first row entity");
        let second_entity = memory_fact_entity_id(&pg, second.memory_id)
            .await?
            .expect("second row entity");
        assert_eq!(first_entity, second_entity);
        assert_eq!(
            entity_head(&pg, first_entity).await?,
            second.memory_id.into_inner()
        );
        assert_eq!(entity_count(&pg).await?, 1);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("stateful_head_advances_to_newer_version failed");
}

#[tokio::test]
async fn guarded_upsert_does_not_regress_to_older_created_at() {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await.expect("migrate core");
    create_code_sidecars(&pg).await.expect("create sidecars");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(&pg);
        let repo_id = Uuid::now_v7();
        let first = ingest_payload(
            &pg,
            &engine,
            &owner,
            &file_revision(repo_id, "src/lib.rs", "future"),
        )
        .await?;
        let fact_entity_id = memory_fact_entity_id(&pg, first.memory_id)
            .await?
            .expect("first row entity");
        let future = time::OffsetDateTime::now_utc() + Duration::hours(1);
        sqlx::query("UPDATE proxima_core.memories SET created_at = $2 WHERE memory_id = $1")
            .bind(first.memory_id.into_inner())
            .bind(future)
            .execute(pg.pool())
            .await?;
        sqlx::query(
            "UPDATE proxima_core.fact_entities
                SET current_created_at = $2
              WHERE fact_entity_id = $1",
        )
        .bind(fact_entity_id)
        .bind(future)
        .execute(pg.pool())
        .await?;

        let older = ingest_payload(
            &pg,
            &engine,
            &owner,
            &file_revision(repo_id, "src/lib.rs", "older"),
        )
        .await?;
        assert_eq!(
            memory_fact_entity_id(&pg, older.memory_id).await?,
            Some(fact_entity_id)
        );
        assert_eq!(
            entity_head(&pg, fact_entity_id).await?,
            first.memory_id.into_inner()
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("guarded_upsert_does_not_regress_to_older_created_at failed");
}

#[tokio::test]
async fn replay_is_idempotent_and_does_not_mint_or_move_entity() {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await.expect("migrate core");
    create_code_sidecars(&pg).await.expect("create sidecars");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(&pg);
        let repo_id = Uuid::now_v7();
        let payload = file_revision(repo_id, "src/lib.rs", "v1");
        let payload_value = serde_json::to_value(&payload)?;
        let draft = draft_for::<FileRevisionV1>(&owner, &payload_value);
        let authorized = engine.authorize_event_ingest(
            &AuthzContext::single_owner(&owner, AuthPath::System),
            Role::SourceIngest,
            draft,
        )?;

        let first = pg
            .ingest_event_with_sidecar(
                &authorized,
                FileRevisionV1::sidecar_table().expect("sidecar table"),
                &payload_value,
                None,
            )
            .await?;
        let fact_entity_id = memory_fact_entity_id(&pg, first.memory_id)
            .await?
            .expect("first row entity");
        let replay = pg
            .ingest_event_with_sidecar(
                &authorized,
                FileRevisionV1::sidecar_table().expect("sidecar table"),
                &payload_value,
                None,
            )
            .await?;

        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, first.memory_id);
        assert_eq!(entity_count(&pg).await?, 1);
        assert_eq!(
            entity_head(&pg, fact_entity_id).await?,
            first.memory_id.into_inner()
        );
        let memories =
            sqlx::query_scalar::<_, i64>("SELECT count(*)::bigint FROM proxima_core.memories")
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(memories, 1);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("replay_is_idempotent_and_does_not_mint_or_move_entity failed");
}

#[tokio::test]
async fn full_owner_triple_participates_in_entity_identity() {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await.expect("migrate core");
    create_code_sidecars(&pg).await.expect("create sidecars");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let user = UserId::new(Uuid::now_v7());
        let owner_a = owner_with(user, OrgId::new(Uuid::now_v7()));
        let owner_b = owner_with(user, OrgId::new(Uuid::now_v7()));
        let engine = engine_for(&pg);
        let repo_id = Uuid::now_v7();
        let payload = file_revision(repo_id, "src/lib.rs", "same");

        let first = ingest_payload(&pg, &engine, &owner_a, &payload).await?;
        let second = ingest_payload(&pg, &engine, &owner_b, &payload).await?;
        let first_entity = memory_fact_entity_id(&pg, first.memory_id)
            .await?
            .expect("first owner entity");
        let second_entity = memory_fact_entity_id(&pg, second.memory_id)
            .await?
            .expect("second owner entity");
        assert_ne!(first_entity, second_entity);
        assert_eq!(entity_count(&pg).await?, 2);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("full_owner_triple_participates_in_entity_identity failed");
}

#[tokio::test]
async fn schema_version_participates_in_entity_identity() {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await.expect("migrate core");
    create_code_sidecars(&pg).await.expect("create sidecars");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(&pg);
        let repo_id = Uuid::now_v7();
        let v1 = ingest_payload(
            &pg,
            &engine,
            &owner,
            &file_revision(repo_id, "src/lib.rs", "v1"),
        )
        .await?;
        let v2 = ingest_payload(
            &pg,
            &engine,
            &owner,
            &file_revision_v2(repo_id, "src/lib.rs", "v2"),
        )
        .await?;
        let v1_entity = memory_fact_entity_id(&pg, v1.memory_id)
            .await?
            .expect("v1 entity");
        let v2_entity = memory_fact_entity_id(&pg, v2.memory_id)
            .await?
            .expect("v2 entity");
        assert_ne!(v1_entity, v2_entity);
        assert_eq!(entity_count(&pg).await?, 2);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("schema_version_participates_in_entity_identity failed");
}

#[tokio::test]
async fn natural_key_values_split_entities() {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await.expect("migrate core");
    create_code_sidecars(&pg).await.expect("create sidecars");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(&pg);
        let repo_id = Uuid::now_v7();
        let chunk_0 = ingest_payload(
            &pg,
            &engine,
            &owner,
            &code_chunk(repo_id, "src/lib.rs", 0, "fn a() {}"),
        )
        .await?;
        let chunk_1 = ingest_payload(
            &pg,
            &engine,
            &owner,
            &code_chunk(repo_id, "src/lib.rs", 1, "fn b() {}"),
        )
        .await?;
        let entity_0 = memory_fact_entity_id(&pg, chunk_0.memory_id)
            .await?
            .expect("chunk 0 entity");
        let entity_1 = memory_fact_entity_id(&pg, chunk_1.memory_id)
            .await?
            .expect("chunk 1 entity");
        assert_ne!(entity_0, entity_1);

        let lookup_1 = pg
            .fact_entity_id_for(
                &owner,
                &CodeChunkV1::schema_id(),
                SchemaVersion::new(CodeChunkV1::SCHEMA_VERSION),
                &code_chunk_natural_key(repo_id, "src/lib.rs", 1),
            )
            .await?;
        assert_eq!(
            lookup_1.map(proxima_core::FactEntityId::into_inner),
            Some(entity_1)
        );
        assert_eq!(entity_count(&pg).await?, 2);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("natural_key_values_split_entities failed");
}

#[tokio::test]
async fn unique_natural_key_guard_rejects_duplicate_entity_row() {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await.expect("migrate core");
    create_code_sidecars(&pg).await.expect("create sidecars");

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(&pg);
        let repo_id = Uuid::now_v7();
        let outcome = ingest_payload(
            &pg,
            &engine,
            &owner,
            &file_revision(repo_id, "src/lib.rs", "v1"),
        )
        .await?;
        let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
        let err = sqlx::query(
            "INSERT INTO proxima_core.fact_entities
                (fact_entity_id, owner_principal_kind, owner_principal_id, owner_org_id,
                 schema_id, schema_version, natural_key, current_memory_id, current_created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())",
        )
        .bind(Uuid::now_v7())
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id)
        .bind(FileRevisionV1::SCHEMA_ID)
        .bind(1_i32)
        .bind(file_revision_natural_key(repo_id, "src/lib.rs"))
        .bind(outcome.memory_id.into_inner())
        .execute(pg.pool())
        .await
        .expect_err("duplicate natural key must hit unique guard");
        let Some(db_err) = err.as_database_error() else {
            panic!("expected database error, got {err}");
        };
        assert!(db_err.is_unique_violation());
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("unique_natural_key_guard_rejects_duplicate_entity_row failed");
}
