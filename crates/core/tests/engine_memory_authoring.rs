mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::verbs::event_ingest::EventDraft;
use proxima_core::verbs::query::MemoryStore;
use proxima_core::{
    AbstractionPayload, AuthPath, AuthorshipKindMask, AuthzContext, EdgeAuthorshipKind, EntityKind,
    EntityKindMask, FactPayload, FlavorRegistry, MemoryId, MemoryOperatorKind, Owner,
    OwnerPrincipalKind, PersonalityInstanceId, Principal, RelationClass, RelationDescriptor, Role,
    SchemaId, SchemaVersion, SourceBatchId, SourceId, Storage, canonical_json_bytes,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug)]
struct FixedEmbeddingClient;

#[async_trait]
impl EmbeddingClient for FixedEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![text.len() as f32, 1.0, 2.0])
    }

    fn model_id(&self) -> &str {
        "test-embed"
    }

    fn dim(&self) -> usize {
        3
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentNoteV1 {
    note_id: Uuid,
    title: String,
    body: String,
    tags: Vec<String>,
    idempotency_key: Option<String>,
}

impl FactPayload for AgentNoteV1 {
    const SCHEMA_ID: &'static str = "proxima-agent-memory/agent-note-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!("{}\n\n{}", self.title, self.body)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_agent_memory.agent_note_v1")
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentDerivationV1 {
    title: String,
    body: String,
    tags: Vec<String>,
    idempotency_key: Option<String>,
    source_memory_ids: Vec<Uuid>,
    model_id: String,
    client_name: String,
    client_version: String,
}

impl AbstractionPayload for AgentDerivationV1 {
    const SCHEMA_ID: &'static str = "proxima-agent-memory/agent-derivation-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_agent_memory.agent_derivation_v1"
    }
}

#[tokio::test]
async fn engine_author_derived_writes_memory_edge_and_embedding()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    apply_agent_memory_migration(&pg).await?;

    let owner = owner_fixture();
    let source_abstraction = insert_source_abstraction(&pg, &owner).await?;
    let author_personality = PersonalityInstanceId::new(Uuid::now_v7());
    let mut registry = FlavorRegistry::new();
    registry.add_abstraction_schema::<AgentDerivationV1>();
    registry.add_relation(RelationDescriptor::substrate(
        "test/derived-from-abstraction",
        RelationClass::Provenance,
        EntityKindMask::abstraction(),
        EntityKindMask::abstraction(),
        AuthorshipKindMask::external_agent(),
    ));
    let engine = proxima_core::Engine::new(registry.freeze(), MemoryStore::new())
        .with_storage(pg.clone().into_handle())
        .with_embed(Arc::new(FixedEmbeddingClient));
    let relation = engine
        .registry()
        .resolve_relation("test/derived-from-abstraction")
        .expect("test relation registered");
    let sidecar_payload = serde_json::to_value(AgentDerivationV1 {
        title: "Derived".into(),
        body: "derived body".into(),
        tags: vec!["memory".into()],
        idempotency_key: Some("derive-1".into()),
        source_memory_ids: vec![source_abstraction.into_inner()],
        model_id: "agent-model".into(),
        client_name: "test-client".into(),
        client_version: "1".into(),
    })?;
    let derived_memory_id = MemoryId::new(Uuid::now_v7());
    let edges = [proxima_core::AuthorDerivedEdgeInput {
        relation,
        source_kind: EntityKind::Abstraction,
        source_memory_id: derived_memory_id,
        target_kind: EntityKind::Abstraction,
        target_memory_id: source_abstraction,
        authorship_kind: EdgeAuthorshipKind::ExternalAgent,
        authorship_owner_memory_id: Some(source_abstraction),
    }];

    let outcome = engine
        .author_derived(proxima_core::AuthorDerivedRequestInput {
            memory_id: derived_memory_id,
            owner: owner.clone(),
            kind: EntityKind::Abstraction,
            text: "derived body".into(),
            schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
            operator_kind: MemoryOperatorKind::ExternalAgent,
            model_id: "agent-model",
            prompt_version: "test-prompt",
            author_personality_instance_id: Some(author_personality),
            sidecar_table: AgentDerivationV1::sidecar_table(),
            sidecar_payload,
            edges: &edges,
        })
        .await?;

    assert!(!outcome.idempotent_replay);
    assert_eq!(outcome.edge_count, 1);
    let memory_id = outcome.memory_id.into_inner();
    let memory_row: (EntityKind, String, Uuid) = sqlx::query_as(
        "SELECT kind, text, personality_instance_id
           FROM proxima_core.memories
          WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(memory_row.0, EntityKind::Abstraction);
    assert_eq!(memory_row.1, "derived body");
    assert_eq!(memory_row.2, author_personality.into_inner());

    let sidecar_title: String = sqlx::query_scalar(
        "SELECT title FROM proxima_agent_memory.agent_derivation_v1 WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(sidecar_title, "Derived");

    let edge_row: (String, Uuid, Uuid, Option<Uuid>) = sqlx::query_as(
        "SELECT relation, source_memory_id, target_memory_id, authorship_owner_memory_id
           FROM proxima_core.edges
          WHERE source_memory_id = $1 AND target_memory_id = $2",
    )
    .bind(memory_id)
    .bind(source_abstraction.into_inner())
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(edge_row.0, "test/derived-from-abstraction");
    assert_eq!(edge_row.1, memory_id);
    assert_eq!(edge_row.2, source_abstraction.into_inner());
    assert_eq!(edge_row.3, Some(source_abstraction.into_inner()));

    let embedding_row: (String, Vec<f32>, i32) = sqlx::query_as(
        "SELECT model_id, vec, dim
           FROM proxima_core.embeddings
          WHERE entity_kind = 'Abstraction' AND entity_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(embedding_row.0, "test-embed");
    assert_eq!(embedding_row.1, vec![12.0, 1.0, 2.0]);
    assert_eq!(embedding_row.2, 3);

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn ingest_event_with_sidecar_writes_fact_and_note_sidecar()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    apply_agent_memory_migration(&pg).await?;

    let owner = owner_fixture();
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema::<AgentNoteV1>();
    let engine = proxima_core::Engine::new(registry.freeze(), MemoryStore::new());
    let authz = AuthzContext::single_owner(&owner, AuthPath::System);
    let note = AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: "Note title".into(),
        body: "Note body".into(),
        tags: vec!["tag".into()],
        idempotency_key: Some("note-1".into()),
    };
    let payload = serde_json::to_value(&note)?;
    let draft = EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner.principal.clone(),
        org_id: None,
        author_personality_instance_id: None,
        schema_id: AgentNoteV1::schema_id(),
        schema_version: SchemaVersion::new(AgentNoteV1::SCHEMA_VERSION),
        payload: canonical_json_bytes(&payload),
        rendered_text: None,
        observed_at: time::OffsetDateTime::now_utc(),
        occurred_at: time::OffsetDateTime::now_utc(),
        citation: None,
    };
    let authorized = engine.authorize_event_ingest(&authz, Role::SourceIngest, draft)?;
    let outcome = pg
        .ingest_event_with_sidecar(
            &authorized,
            AgentNoteV1::sidecar_table().expect("agent note has a sidecar table"),
            &payload,
        )
        .await?;

    let memory_row: (Option<EntityKind>, String) =
        sqlx::query_as("SELECT kind, text FROM proxima_core.memories WHERE memory_id = $1")
            .bind(outcome.memory_id.into_inner())
            .fetch_one(pg.pool())
            .await?;
    assert_eq!(memory_row.0, None);
    assert_eq!(memory_row.1, "Note title\n\nNote body");

    let sidecar_row: (Uuid, String, String) = sqlx::query_as(
        "SELECT note_id, title, body
           FROM proxima_agent_memory.agent_note_v1
          WHERE memory_id = $1",
    )
    .bind(outcome.memory_id.into_inner())
    .fetch_one(pg.pool())
    .await?;
    assert_eq!(sidecar_row.0, note.note_id);
    assert_eq!(sidecar_row.1, "Note title");
    assert_eq!(sidecar_row.2, "Note body");

    drop(engine);
    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn apply_agent_memory_migration(
    pg: &proxima_storage_pg::PgStorage,
) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(include_str!(
        "../../../flavors/agent-memory/migrations/20260516000030_baseline.sql"
    ))
    .execute(pg.pool())
    .await
    .map(|_| ())
}

async fn insert_source_abstraction(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<MemoryId, sqlx::Error> {
    let memory_id = Uuid::now_v7();
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, $5, 1, 'Abstraction',
                 'source abstraction', 'ExternalAgent', 'source-model',
                 'source-prompt', $6, 0)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(AgentDerivationV1::SCHEMA_ID)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await?;
    Ok(MemoryId::new(memory_id))
}
