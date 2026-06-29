//! Auth-gated Fact ingest with typed inline citation sidecars.

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::CitationPort;
use proxima_core::verbs::fact_ingest::{
    FactReceiptDraft, FactWriteCommand, InlineCitationMappingDraft, InlineCitedObjectDraft,
};
use proxima_core::{
    AuthPath, AuthzContext, CitationMappingPayload, CitedObjectPayload, Engine, FactPayload,
    FlavorRegistry, FlavorRegistryFrozen, GroupId, MemoryId, Owner, OwnerRef, PayloadKeyBuilder,
    Relation, Role, SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError, UserId,
    canonical_json_bytes,
};
use proxima_storage_pg::sidecars::{
    PgCitationMappingSidecar, PgCitedObjectSidecar, PgMemoryPayload, PgMemoryPayloadFuture,
    PgSidecarFuture,
};
use proxima_storage_pg::verbs::fact_ingest::{
    FactIngestSidecarFuture, PgFactSidecar, attach_citation_in_tx, ingest_fact_in_tx,
    ingest_fact_with_citation_atomic, ingest_fact_with_citation_in_tx,
};
use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, PgStorage, register_core_pg_sidecars,
};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct TestFact {
    note: String,
}

impl FactPayload for TestFact {
    const SCHEMA_ID: &'static str = "test/inline-cited-fact";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("note", &self.note);
        key.finish()
    }

    fn render(&self) -> String {
        self.note.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("public.inline_cited_fact_sidecar")
    }
}

impl PgFactSidecar for TestFact {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> FactIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.inline_cited_fact_sidecar (memory_id, note)
                 VALUES ($1, $2)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.note)
            .execute(tx.as_mut())
            .await
            .map_err(|e| StorageError::Internal(e.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for TestFact {
    fn load_memory_payload(
        _pool: &sqlx::PgPool,
        _memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async { Ok(None) })
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
}

impl PgCitedObjectSidecar for TestCitedObject {
    fn insert_cited_object_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::PgConnection,
        cited_object_id: Uuid,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.inline_cited_object_sidecar (cited_object_id, body)
                 VALUES ($1, $2)
                 ON CONFLICT (cited_object_id) DO NOTHING",
            )
            .bind(cited_object_id)
            .bind(&self.body)
            .execute(tx)
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
}

impl PgCitationMappingSidecar for TestCitationMapping {
    fn insert_citation_mapping_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::PgConnection,
        citation_mapping_id: Uuid,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.inline_citation_mapping_sidecar
                    (citation_mapping_id, byte_start, byte_end)
                 VALUES ($1, $2, $3)",
            )
            .bind(citation_mapping_id)
            .bind(self.byte_start)
            .bind(self.byte_end)
            .execute(tx)
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

fn registry() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema::<TestFact>();
    registry.add_cited_object_schema::<TestCitedObject>();
    registry.add_citation_mapping_schema::<TestCitationMapping>();
    registry.freeze()
}

fn engine() -> Engine {
    Engine::new(registry())
}

fn pg_sidecars() -> PgSidecarRegistryFrozen {
    let registry = registry();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<TestFact>();
    sidecars.add_cited_object::<TestCitedObject>();
    sidecars.add_citation_mapping::<TestCitationMapping>();
    sidecars
        .freeze_against(registry.schemas())
        .expect("inline citation test sidecars match schemas")
}

async fn fresh_pg_with_sidecars() -> (PgStorage, String) {
    let (pg, db_name) = fresh_pg().await;
    (pg.with_sidecars(pg_sidecars()), db_name)
}

fn draft(_owner: &Owner, note: &str) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: TestFact::schema_id(),
        schema_version: SchemaVersion::new(TestFact::SCHEMA_VERSION),
        payload: json(&TestFact {
            note: note.to_string(),
        }),
        rendered_text: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/inline-cited-source"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
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

async fn seed_membership(
    pool: &sqlx::PgPool,
    group: &OwnerRef,
    member: &OwnerRef,
) -> Result<(), sqlx::Error> {
    let OwnerRef::Group(group_id) = group else {
        panic!("group principal required");
    };
    let OwnerRef::Personal(member_id) = member else {
        panic!("user principal required");
    };
    sqlx::query(
        "INSERT INTO proxima_core.group_memberships
            (group_id, member_user_id, relation)
         VALUES ($1, $2, 'viewer')",
    )
    .bind(group_id.into_inner())
    .bind(member_id.into_inner())
    .execute(pool)
    .await?;
    Ok(())
}

async fn ingest_plain_fact_for_attach(
    pg: &proxima_storage_pg::PgStorage,
    engine: &Engine,
    authz: &AuthzContext,
) -> Result<proxima_core::FactIngestOutcome, Box<dyn std::error::Error>> {
    let fact = TestFact {
        note: "plain fact".to_string(),
    };
    let note = fact.note.clone();
    let mut tx = pg.pool().begin().await?;
    let fact_outcome = ingest_fact_in_tx(
        &mut tx,
        engine,
        authz,
        Relation::Ingest,
        &fact,
        move |tx, outcome| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO public.inline_cited_fact_sidecar (memory_id, note)
                         VALUES ($1, $2)",
                )
                .bind(outcome.memory_id.into_inner())
                .bind(note)
                .execute(&mut **tx)
                .await
                .map_err(|e| StorageError::Internal(e.to_string()))?;
                Ok(())
            })
        },
    )
    .await?;
    tx.commit().await?;
    Ok(fact_outcome)
}

async fn stored_fact_citation_mapping_id(
    pool: &sqlx::PgPool,
    memory_id: MemoryId,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT citation_mapping_id
         FROM proxima_core.memories
         WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pool)
    .await
}

#[tokio::test]
async fn fact_with_inline_citation_writes_rows_and_reuses_cited_object()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        create_sidecar_tables(pg.pool()).await?;

        let engine = engine();
        let owner = owner_fixture();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let first = engine
            .authorize_fact_with_citation(
                &authz,
                Relation::Ingest,
                draft(&owner, "first fact"),
                cited_object(),
                citation_mapping(0, 4),
            )
            .await?;
        let second = engine
            .authorize_fact_with_citation(
                &authz,
                Relation::Ingest,
                draft(&owner, "second fact"),
                cited_object(),
                citation_mapping(5, 9),
            )
            .await?;
        let expected_content_hash = TestCitedObject {
            body: "same cited body".to_string(),
        }
        .idempotency_key();
        assert_eq!(first.cited_object().content_hash(), &expected_content_hash);
        assert_eq!(second.cited_object().content_hash(), &expected_content_hash);

        let first_note = "first fact".to_string();
        let first_outcome = ingest_fact_with_citation_atomic(
            pg.pool(),
            pg.sidecars(),
            &first,
            None,
            move |tx, outcome| {
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
            },
        )
        .await?;
        let second_note = "second fact".to_string();
        let second_outcome = ingest_fact_with_citation_atomic(
            pg.pool(),
            pg.sidecars(),
            &second,
            None,
            move |tx, outcome| {
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
            },
        )
        .await?;

        assert!(!first_outcome.idempotent_replay);
        assert!(!second_outcome.idempotent_replay);
        assert_ne!(first_outcome.memory_id, second_outcome.memory_id);
        assert_written_rows(pg.pool()).await?;

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn attach_citation_adds_readback_and_is_idempotent() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        create_sidecar_tables(pg.pool()).await?;

        let engine = engine();
        let owner = owner_fixture();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);

        let fact_outcome = ingest_plain_fact_for_attach(&pg, &engine, &authz).await?;
        assert!(
            pg.citation_of_fact(fact_outcome.memory_id).await?.is_none(),
            "plain Fact starts uncited"
        );

        let authorized = engine
            .authorize_citation_attachment(
                &authz,
                Relation::Ingest,
                owner,
                fact_outcome.memory_id,
                cited_object(),
                citation_mapping(1, 5),
            )
            .await?;

        let mut tx = pg.pool().begin().await?;
        let first_attach = attach_citation_in_tx(&mut tx, pg.sidecars(), &authorized).await?;
        tx.commit().await?;

        assert!(first_attach.attached);
        assert!(!first_attach.idempotent);
        assert_eq!(first_attach.memory_id, fact_outcome.memory_id);

        let readback = pg
            .citation_of_fact(fact_outcome.memory_id)
            .await?
            .expect("attached citation must be readable");
        assert_eq!(readback.cited_object_id, first_attach.cited_object_id);
        assert_eq!(readback.mapping_schema_id, TestCitationMapping::schema_id());
        assert_eq!(
            readback.cited_object_schema_id,
            TestCitedObject::schema_id()
        );

        let stored_mapping_id =
            stored_fact_citation_mapping_id(pg.pool(), fact_outcome.memory_id).await?;
        assert_eq!(stored_mapping_id, Some(readback.citation_mapping_id));
        assert_eq!(count(pg.pool(), "proxima_core.citation_mappings").await?, 1);
        assert_eq!(
            count(pg.pool(), "public.inline_citation_mapping_sidecar").await?,
            1
        );

        let mut tx = pg.pool().begin().await?;
        let second_attach = attach_citation_in_tx(&mut tx, pg.sidecars(), &authorized).await?;
        tx.commit().await?;

        assert!(!second_attach.attached);
        assert!(second_attach.idempotent);
        assert_eq!(second_attach.memory_id, fact_outcome.memory_id);
        assert_eq!(second_attach.cited_object_id, first_attach.cited_object_id);
        assert_eq!(count(pg.pool(), "proxima_core.citation_mappings").await?, 1);
        assert_eq!(
            count(pg.pool(), "public.inline_citation_mapping_sidecar").await?,
            1
        );

        let missing = engine
            .authorize_citation_attachment(
                &authz,
                Relation::Ingest,
                owner,
                MemoryId::new(Uuid::now_v7()),
                cited_object(),
                citation_mapping(1, 5),
            )
            .await?;
        let mut tx = pg.pool().begin().await?;
        let err = attach_citation_in_tx(&mut tx, pg.sidecars(), &missing)
            .await
            .expect_err("missing target Fact must return NotFound");
        drop(tx);
        assert!(matches!(err, StorageError::NotFound));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn facts_citing_object_filters_by_read_owners() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        create_sidecar_tables(pg.pool()).await?;

        let engine = engine();
        let p = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let q = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let g1 = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        seed_membership(pg.pool(), &g1, &q).await?;

        let group_authz = AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(g1, Role::admin())],
            AuthPath::System,
        )
        .narrowed_to_owner(g1)
        .expect("group admin narrows to target owner");
        let group_authorized = engine
            .authorize_fact_with_citation(
                &group_authz,
                Relation::Ingest,
                draft(&g1, "group fact"),
                cited_object(),
                citation_mapping(0, 4),
            )
            .await?;
        let p_authorized = engine
            .authorize_fact_with_citation(
                &AuthzContext::single_owner(&p, AuthPath::System),
                Relation::Ingest,
                draft(&p, "p fact"),
                cited_object(),
                citation_mapping(0, 4),
            )
            .await?;

        let group_outcome = ingest_fact_with_citation_atomic(
            pg.pool(),
            pg.sidecars(),
            &group_authorized,
            None,
            |tx, outcome| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO public.inline_cited_fact_sidecar (memory_id, note)
                         VALUES ($1, 'group fact')",
                    )
                    .bind(outcome.memory_id.into_inner())
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| StorageError::Internal(e.to_string()))?;
                    Ok(())
                })
            },
        )
        .await?;
        let group_citation = pg
            .citation_of_fact(group_outcome.memory_id)
            .await?
            .expect("group fact citation");
        let p_outcome = ingest_fact_with_citation_atomic(
            pg.pool(),
            pg.sidecars(),
            &p_authorized,
            None,
            |tx, outcome| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO public.inline_cited_fact_sidecar (memory_id, note)
                         VALUES ($1, 'p fact')",
                    )
                    .bind(outcome.memory_id.into_inner())
                    .execute(&mut **tx)
                    .await
                    .map_err(|e| StorageError::Internal(e.to_string()))?;
                    Ok(())
                })
            },
        )
        .await?;
        let p_citation = pg
            .citation_of_fact(p_outcome.memory_id)
            .await?
            .expect("p fact citation");

        let q_read_owners = vec![q, g1];
        let group_facts = pg
            .facts_citing_object(&q_read_owners, group_citation.cited_object_id, &[])
            .await?;
        assert_eq!(group_facts.len(), 1);
        assert_eq!(group_facts[0].memory_id, group_outcome.memory_id);

        let p_facts = pg
            .facts_citing_object(&q_read_owners, p_citation.cited_object_id, &[])
            .await?;
        assert!(p_facts.is_empty(), "Q must not see P's personal citation");
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn fact_sidecar_failure_rolls_back_whole_inline_citation_ingest()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg_with_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        create_sidecar_tables(pg.pool()).await?;

        let engine = engine();
        let owner = owner_fixture();
        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let authorized = engine
            .authorize_fact_with_citation(
                &authz,
                Relation::Ingest,
                draft(&owner, "rollback fact"),
                cited_object(),
                citation_mapping(0, 4),
            )
            .await?;
        let receipt_id = authorized
            .draft()
            .receipt_id_for_owner(*authorized.permit().owner())
            .expect("receipt id");

        let mut tx = pg.pool().begin().await?;
        let err = ingest_fact_with_citation_in_tx(
            &mut tx,
            pg.sidecars(),
            &authorized,
            None,
            |_tx, _outcome| {
                Box::pin(async move { Err(StorageError::Internal("fact sidecar failed".into())) })
            },
        )
        .await
        .expect_err("failing Fact sidecar must abort the verb");
        drop(tx);

        assert!(err.to_string().contains("fact sidecar failed"));
        let receipt_id_bytes = receipt_id.into_inner();
        let memories = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM proxima_core.memories WHERE receipt_id = $1",
        )
        .bind(receipt_id_bytes.as_slice())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(memories, 0);
        assert_eq!(count(pg.pool(), "proxima_core.cited_objects").await?, 0);
        assert_eq!(
            count(pg.pool(), "public.inline_cited_object_sidecar").await?,
            0
        );
        assert_eq!(count(pg.pool(), "proxima_core.fact_receipts").await?, 0);
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

async fn assert_written_rows(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    assert_eq!(
        count(pool, "proxima_core.cited_objects").await?,
        1,
        "same content hash must reuse the cited object"
    );
    assert_eq!(count(pool, "public.inline_cited_object_sidecar").await?, 1);
    assert_eq!(count(pool, "proxima_core.fact_receipts").await?, 2);
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

    Ok(())
}
