//! Auth-gated Fact ingest with typed inline citation sidecars.

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::verbs::event_ingest::{
    EventDraft, InlineCitationMappingDraft, InlineCitedObjectDraft,
};
use proxima_core::{
    AuthPath, AuthzContext, CitationMappingPayload, CitedObjectPayload, Engine, FactPayload,
    FlavorRegistry, Owner, PersonalityInstanceId, Role, SchemaId, SchemaVersion, SourceBatchId,
    SourceId, StorageError, canonical_json_bytes,
};
use proxima_storage_pg::verbs::event_ingest::{
    ingest_fact_with_citation_atomic, ingest_fact_with_citation_in_tx,
};
use std::future::Future;
use std::pin::Pin;
use uuid::Uuid;

type SidecarFuture<'t> = Pin<Box<dyn Future<Output = Result<(), StorageError>> + Send + 't>>;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct TestFact {
    note: String,
}

impl FactPayload for TestFact {
    const SCHEMA_ID: &'static str = "test/inline-cited-fact";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        self.note.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("public.inline_cited_fact_sidecar")
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct TestCitedObject {
    body: String,
}

impl CitedObjectPayload for TestCitedObject {
    const SCHEMA_ID: &'static str = "test/inline-cited-object";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "public.inline_cited_object_sidecar"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        *blake3::hash(self.body.as_bytes()).as_bytes()
    }

    fn sidecar_insert<'t>(
        &'t self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        sidecar_row_id: Uuid,
    ) -> SidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.inline_cited_object_sidecar (cited_object_id, body)
                 VALUES ($1, $2)
                 ON CONFLICT (cited_object_id) DO NOTHING",
            )
            .bind(sidecar_row_id)
            .bind(&self.body)
            .execute(&mut **tx)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
            Ok(())
        })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct TestCitationMapping {
    byte_start: i32,
    byte_end: i32,
}

impl CitationMappingPayload for TestCitationMapping {
    const SCHEMA_ID: &'static str = "test/inline-citation-mapping";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("public.inline_citation_mapping_sidecar")
    }

    fn cited_object_schema() -> SchemaId {
        TestCitedObject::schema_id()
    }

    fn sidecar_insert<'t>(
        &'t self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        sidecar_row_id: Uuid,
    ) -> SidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.inline_citation_mapping_sidecar
                    (citation_mapping_id, byte_start, byte_end)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (citation_mapping_id) DO NOTHING",
            )
            .bind(sidecar_row_id)
            .bind(self.byte_start)
            .bind(self.byte_end)
            .execute(&mut **tx)
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
            Ok(())
        })
    }
}

fn json<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let value = serde_json::to_value(value).expect("test payload serializes as JSON");
    canonical_json_bytes(&value)
}

fn engine() -> Engine {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema::<TestFact>();
    registry.add_cited_object_schema::<TestCitedObject>();
    registry.add_citation_mapping_schema::<TestCitationMapping>();
    Engine::new(registry.freeze())
}

fn draft(owner: &Owner, note: &str, author: Option<PersonalityInstanceId>) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/inline-cited-source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner.principal.clone(),
        org_id: None,
        author_personality_instance_id: author,
        schema_id: TestFact::schema_id(),
        schema_version: SchemaVersion::new(TestFact::SCHEMA_VERSION),
        payload: json(&TestFact {
            note: note.to_string(),
        }),
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: None,
    }
}

fn cited_object() -> InlineCitedObjectDraft {
    let payload = TestCitedObject {
        body: "same cited body".to_string(),
    };
    InlineCitedObjectDraft {
        schema_id: TestCitedObject::schema_id(),
        schema_version: SchemaVersion::new(TestCitedObject::SCHEMA_VERSION),
        payload_bytes: json(&payload),
    }
}

fn citation_mapping(byte_start: i32, byte_end: i32) -> InlineCitationMappingDraft {
    InlineCitationMappingDraft {
        schema_id: TestCitationMapping::schema_id(),
        schema_version: SchemaVersion::new(TestCitationMapping::SCHEMA_VERSION),
        payload_bytes: json(&TestCitationMapping {
            byte_start,
            byte_end,
        }),
    }
}

async fn create_sidecar_tables(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE public.inline_cited_fact_sidecar (
            memory_id uuid PRIMARY KEY,
            note text NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE public.inline_cited_object_sidecar (
            cited_object_id uuid PRIMARY KEY,
            body text NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE public.inline_citation_mapping_sidecar (
            citation_mapping_id uuid PRIMARY KEY,
            byte_start integer NOT NULL,
            byte_end integer NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
async fn fact_with_inline_citation_writes_rows_and_reuses_cited_object()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        create_sidecar_tables(pg.pool()).await?;

        let engine = engine();
        let owner = owner_fixture();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let personality = PersonalityInstanceId::new(Uuid::now_v7());
        let first = engine.authorize_fact_with_citation(
            &authz,
            Role::SourceIngest,
            draft(&owner, "first fact", None),
            cited_object(),
            citation_mapping(0, 4),
        )?;
        let second = engine.authorize_fact_with_citation(
            &authz,
            Role::SourceIngest,
            draft(&owner, "second fact", Some(personality)),
            cited_object(),
            citation_mapping(5, 9),
        )?;
        let expected_content_hash = TestCitedObject {
            body: "same cited body".to_string(),
        }
        .idempotency_key();
        assert_eq!(first.cited_object().content_hash(), &expected_content_hash);
        assert_eq!(second.cited_object().content_hash(), &expected_content_hash);

        let first_note = "first fact".to_string();
        let first_outcome =
            ingest_fact_with_citation_atomic(pg.pool(), &first, None, move |tx, outcome| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO public.inline_cited_fact_sidecar (memory_id, note)
                         VALUES ($1, $2)",
                    )
                    .bind(outcome.memory_id.into_inner())
                    .bind(first_note)
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| StorageError::Internal(e.to_string()))?;
                    Ok(())
                })
            })
            .await?;
        let second_note = "second fact".to_string();
        let second_outcome =
            ingest_fact_with_citation_atomic(pg.pool(), &second, None, move |tx, outcome| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO public.inline_cited_fact_sidecar (memory_id, note)
                         VALUES ($1, $2)",
                    )
                    .bind(outcome.memory_id.into_inner())
                    .bind(second_note)
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| StorageError::Internal(e.to_string()))?;
                    Ok(())
                })
            })
            .await?;

        assert!(!first_outcome.idempotent_replay);
        assert!(!second_outcome.idempotent_replay);
        assert_ne!(first_outcome.memory_id, second_outcome.memory_id);
        assert_written_rows_and_personality(pg.pool(), &second_outcome, personality).await?;

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn fact_sidecar_failure_rolls_back_whole_inline_citation_ingest()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        create_sidecar_tables(pg.pool()).await?;

        let engine = engine();
        let owner = owner_fixture();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let authorized = engine.authorize_fact_with_citation(
            &authz,
            Role::SourceIngest,
            draft(&owner, "rollback fact", None),
            cited_object(),
            citation_mapping(0, 4),
        )?;
        let event_id = authorized.draft().event_id();

        let mut tx = pg.pool().begin().await?;
        let err = ingest_fact_with_citation_in_tx(&mut tx, &authorized, None, |_tx, _outcome| {
            Box::pin(async move { Err(StorageError::Internal("fact sidecar failed".into())) })
        })
        .await
        .expect_err("failing Fact sidecar must abort the verb");
        drop(tx);

        assert!(err.to_string().contains("fact sidecar failed"));
        let event_id_bytes = event_id.into_inner();
        let memories = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM proxima_core.memories WHERE event_id = $1",
        )
        .bind(event_id_bytes.as_slice())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(memories, 0);
        assert_eq!(count(pg.pool(), "proxima_core.cited_objects").await?, 0);
        assert_eq!(
            count(pg.pool(), "public.inline_cited_object_sidecar").await?,
            0
        );
        assert_eq!(count(pg.pool(), "proxima_core.events").await?, 0);
        assert_eq!(count(pg.pool(), "proxima_core.citation_mappings").await?, 0);
        assert_eq!(
            count(pg.pool(), "public.inline_citation_mapping_sidecar").await?,
            0
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

async fn count(pool: &sqlx::PgPool, table: &str) -> Result<i64, sqlx::Error> {
    let sql = format!("SELECT count(*) FROM {table}");
    sqlx::query_scalar(&sql).fetch_one(pool).await
}

async fn assert_written_rows_and_personality(
    pool: &sqlx::PgPool,
    second_outcome: &proxima_core::EventIngestOutcome,
    personality: PersonalityInstanceId,
) -> Result<(), sqlx::Error> {
    assert_eq!(
        count(pool, "proxima_core.cited_objects").await?,
        1,
        "same content hash must reuse the cited object"
    );
    assert_eq!(count(pool, "public.inline_cited_object_sidecar").await?, 1);
    assert_eq!(count(pool, "proxima_core.events").await?, 2);
    assert_eq!(count(pool, "proxima_core.memories").await?, 2);
    assert_eq!(count(pool, "public.inline_cited_fact_sidecar").await?, 2);
    assert_eq!(count(pool, "proxima_core.citation_mappings").await?, 2);
    assert_eq!(
        count(pool, "public.inline_citation_mapping_sidecar").await?,
        2
    );

    let cited_object_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT cited_object_id FROM proxima_core.citation_mappings")
            .fetch_all(pool)
            .await?;
    assert_eq!(cited_object_ids.len(), 2);
    assert_eq!(cited_object_ids[0], cited_object_ids[1]);

    let stamped: Uuid = sqlx::query_scalar(
        "SELECT personality_instance_id
         FROM proxima_core.memories
         WHERE memory_id = $1",
    )
    .bind(second_outcome.memory_id.into_inner())
    .fetch_one(pool)
    .await?;
    assert_eq!(stamped, personality.into_inner());

    let change_stamped: Uuid = sqlx::query_scalar(
        "SELECT entity_personality_instance_id
         FROM proxima_core.change_event
         WHERE seq = $1",
    )
    .bind(second_outcome.change_event_seq)
    .fetch_one(pool)
    .await?;
    assert_eq!(change_stamped, personality.into_inner());
    Ok(())
}
