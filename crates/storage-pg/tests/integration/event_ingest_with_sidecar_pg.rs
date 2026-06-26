//! Auth-gated `EventIngest` plus caller-owned sidecar transaction tests.

use std::collections::HashSet;
use std::sync::Arc;

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{
    AuthPath, AuthzContext, CapabilitySet, Engine, ErrorCode, FactPayload, FlavorRegistryFrozen,
    Identity, MemoryActionSet, MemorySpaceGrant, MemorySpaceGrants, Owner, PayloadKeyBuilder, Role,
    RoleSet, SchemaId, SchemaVersion, SourceBatchId, SourceId, Storage, StorageError, ToolScope,
};
use proxima_storage_pg::verbs::event_ingest::{event_ingest_with_sidecar_atomic, ingest_fact};
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct UncitedFactPayload {
    note: String,
}

impl FactPayload for UncitedFactPayload {
    const SCHEMA_ID: &'static str = "test/uncited_fact";
    const SCHEMA_VERSION: u32 = 1;

    fn event_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("note", &self.note);
        key.finish()
    }

    fn render(&self) -> String {
        self.note.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("public.uncited_fact_sidecar")
    }
}

fn schemas_for_test() -> Vec<SchemaInfo> {
    vec![
        SchemaInfo::opaque(
            SchemaId::new("test/sidecar_fact".into()),
            SchemaVersion::new(1),
            PayloadKind::Fact,
        ),
        SchemaInfo::opaque(
            SchemaId::new(UncitedFactPayload::SCHEMA_ID.into()),
            SchemaVersion::new(UncitedFactPayload::SCHEMA_VERSION),
            PayloadKind::Fact,
        ),
        SchemaInfo::opaque(
            SchemaId::new("test/sidecar_cited".into()),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        ),
        SchemaInfo::opaque(
            SchemaId::new("test/sidecar_citation".into()),
            SchemaVersion::new(1),
            PayloadKind::CitationMapping,
        ),
    ]
}

fn fresh_draft(owner: &Owner) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    let payload = format!("sidecar gated ingest {}", Uuid::now_v7()).into_bytes();
    let content_hash = blake3::hash(&payload);
    EventDraft {
        source_id: SourceId::new("test/sidecar-source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner.clone(),
        author_personality_instance_id: None,
        schema_id: SchemaId::new("test/sidecar_fact".into()),
        schema_version: SchemaVersion::new(1),
        payload,
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("test/sidecar_cited".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: *content_hash.as_bytes(),
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("test/sidecar_citation".into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    }
}

fn reduced_authz(owner: &Owner) -> AuthzContext {
    let mut accessible_principals = HashSet::with_capacity(1);
    accessible_principals.insert(owner.clone());
    AuthzContext {
        identity: Identity {
            principal: owner.clone(),
            accessible_principals,
            expires_at: None,
            auth_epoch: 0,
        },
        capabilities: CapabilitySet {
            tool_scope: ToolScope::All,
            roles: RoleSet {
                graph_read: true,
                graph_write: true,
                source_ingest: false,
                admin: false,
            },
            memory_spaces: MemorySpaceGrants::unrestricted(),
        },
        auth_path: AuthPath::System,
    }
}

/// A pure source-ingest identity: `source_ingest` only, no `graph_write`.
/// `write_grant` toggles the owner-space `memory.write` grant.
fn source_ingest_only_authz(owner: &Owner, write_grant: bool) -> AuthzContext {
    let mut accessible_principals = HashSet::with_capacity(1);
    accessible_principals.insert(owner.clone());
    let actions = if write_grant {
        MemoryActionSet {
            search: false,
            read: false,
            write: true,
            publish: false,
            admin: false,
        }
    } else {
        MemoryActionSet::read_only()
    };
    AuthzContext {
        identity: Identity {
            principal: owner.clone(),
            accessible_principals,
            expires_at: None,
            auth_epoch: 0,
        },
        capabilities: CapabilitySet {
            tool_scope: ToolScope::All,
            roles: RoleSet {
                graph_read: false,
                graph_write: false,
                source_ingest: true,
                admin: false,
            },
            memory_spaces: MemorySpaceGrants::explicit(vec![MemorySpaceGrant {
                key: "personal".into(),
                label: "Personal".into(),
                owner: owner.clone(),
                actions,
            }]),
        },
        auth_path: AuthPath::HostBearer,
    }
}

fn engine_for(pg: &proxima_storage_pg::PgStorage) -> Engine {
    let storage: Arc<dyn Storage> = Arc::new(pg.clone());
    Engine::new(FlavorRegistryFrozen::with_schemas(schemas_for_test())).with_storage(storage)
}

async fn event_row_counts(
    pool: &sqlx::PgPool,
    event_id: proxima_core::EventId,
) -> Result<(i64, i64), sqlx::Error> {
    let event_id_bytes = event_id.into_inner();
    let memories = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM proxima_core.memories WHERE event_id = $1",
    )
    .bind(event_id_bytes.as_slice())
    .fetch_one(pool)
    .await?;
    let events = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM proxima_core.events WHERE event_id = $1",
    )
    .bind(event_id_bytes.as_slice())
    .fetch_one(pool)
    .await?;
    Ok((memories, events))
}

async fn embedding_job_count(pool: &sqlx::PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.embedding_jobs")
        .fetch_one(pool)
        .await
}

#[tokio::test]
async fn authz_rejection_writes_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let engine = engine_for(&pg);
    let draft = fresh_draft(&owner);
    let event_id = draft.event_id();
    let err = engine
        .authorize_event_ingest(&reduced_authz(&owner), Role::SourceIngest, draft)
        .expect_err("missing source_ingest role must reject before storage");

    assert_eq!(err.code, ErrorCode::Forbidden);
    assert!(err.message.contains("requires source_ingest role"));
    assert_eq!(event_row_counts(pg.pool(), event_id).await?, (0, 0));

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn source_ingest_only_authorizes_event_ingest_with_write_grant()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let engine = engine_for(&pg);

    // A pure source-ingest identity (no graph_write) WITH a memory.write grant
    // authorizes: source ingestion gates on its SourceIngest role plus the
    // owner-space write grant, and must NOT additionally require graph_write.
    let authorized = engine.authorize_event_ingest(
        &source_ingest_only_authz(&owner, true),
        Role::SourceIngest,
        fresh_draft(&owner),
    )?;
    assert_eq!(authorized.draft().principal, owner);

    // The same identity WITHOUT a write grant is rejected at the owner-space
    // grant gate (read-only grant), not the role gate.
    let err = engine
        .authorize_event_ingest(
            &source_ingest_only_authz(&owner, false),
            Role::SourceIngest,
            fresh_draft(&owner),
        )
        .expect_err("missing memory.write grant must reject");
    assert_eq!(err.code, ErrorCode::Forbidden);
    assert!(err.message.contains("requires memory.write on owner"));

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn sidecar_failure_rolls_back_fact() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();
    let engine = engine_for(&pg);
    let draft = fresh_draft(&owner);
    let authorized = engine.authorize_event_ingest(
        &AuthzContext::single_owner(&owner, AuthPath::System),
        Role::SourceIngest,
        draft,
    )?;
    let event_id = authorized.draft().event_id();

    let err = event_ingest_with_sidecar_atomic(
        pg.pool(),
        &authorized,
        Some("rollback-test-embed"),
        |_tx, _outcome| Box::pin(async move { Err(StorageError::Internal("boom".into())) }),
    )
    .await
    .expect_err("sidecar failure must surface");

    assert!(err.to_string().contains("boom"));
    assert_eq!(event_row_counts(pg.pool(), event_id).await?, (0, 0));
    assert_eq!(embedding_job_count(pg.pool()).await?, 0);

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn ingest_fact_writes_uncited_fact_and_sidecar() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    sqlx::query(
        "CREATE TABLE public.uncited_fact_sidecar (
            memory_id uuid PRIMARY KEY,
            note text NOT NULL
        )",
    )
    .execute(pg.pool())
    .await?;

    let owner = owner_fixture();
    let engine = engine_for(&pg);
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let payload = UncitedFactPayload {
        note: format!("uncited fact {}", Uuid::now_v7()),
    };
    let sidecar_note = payload.note.clone();

    let outcome = ingest_fact(
        pg.pool(),
        &engine,
        &authz,
        Role::SourceIngest,
        &payload,
        move |tx, outcome| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO public.uncited_fact_sidecar (memory_id, note)
                     VALUES ($1, $2)",
                )
                .bind(outcome.memory_id.into_inner())
                .bind(sidecar_note)
                .execute(&mut **tx)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;
                Ok(())
            })
        },
    )
    .await?;

    let citation_mapping_id = sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT citation_mapping_id
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(outcome.memory_id.into_inner())
    .fetch_one(pg.pool())
    .await?;
    assert!(citation_mapping_id.is_none());

    let cited_objects =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxima_core.cited_objects")
            .fetch_one(pg.pool())
            .await?;
    let citation_mappings = sqlx::query_scalar::<_, i64>(
        "SELECT count(*)
           FROM proxima_core.citation_mappings
          WHERE memory_id = $1",
    )
    .bind(outcome.memory_id.into_inner())
    .fetch_one(pg.pool())
    .await?;
    let sidecars = sqlx::query_scalar::<_, i64>(
        "SELECT count(*)
           FROM public.uncited_fact_sidecar
          WHERE memory_id = $1
            AND note = $2",
    )
    .bind(outcome.memory_id.into_inner())
    .bind(&payload.note)
    .fetch_one(pg.pool())
    .await?;

    assert_eq!(cited_objects, 0);
    assert_eq!(citation_mappings, 0);
    assert_eq!(sidecars, 1);

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
