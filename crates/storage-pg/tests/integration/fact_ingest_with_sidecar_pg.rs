//! Auth-gated `FactIngest` plus caller-owned sidecar transaction tests.

use proxima_core::storage_ports::*;
use std::sync::Arc;

use crate::common::{drop_db, fresh_pg, owner_fixture, owner_write_permit};
use proxima_core::verbs::fact_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{
    AuthPath, AuthzContext, Engine, ErrorCode, FactPayload, FlavorRegistryFrozen, GroupId, Owner,
    OwnerRef, PayloadKeyBuilder, Relation, Role, SchemaId, SchemaVersion, SourceBatchId, SourceId,
    StorageError, UserId,
};
use proxima_storage_pg::verbs::fact_ingest::{
    fact_ingest_with_sidecar_atomic, ingest_fact, ingest_fact_for_owner_plain,
};
use uuid::Uuid;

type ResolvedAuthz = AuthzContext;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct UncitedFactPayload {
    note: String,
}

impl FactPayload for UncitedFactPayload {
    const SCHEMA_ID: &'static str = "test/uncited_fact";
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

fn fresh_draft(_owner: &Owner) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    let payload = format!("sidecar gated ingest {}", Uuid::now_v7()).into_bytes();
    let content_hash = blake3::hash(&payload);
    FactWriteCommand {
        schema_id: SchemaId::new("test/sidecar_fact".into()),
        schema_version: SchemaVersion::new(1),
        payload,
        rendered_text: None,
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/sidecar-source"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
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
        derived_from: Vec::new(),
    }
}

fn bot_principal() -> OwnerRef {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

fn group_owner() -> Owner {
    OwnerRef::Group(GroupId::new(Uuid::now_v7()))
}

fn bot_authz_with_role(bot: &OwnerRef, owner: Owner, role: Role) -> ResolvedAuthz {
    let OwnerRef::Personal(user) = *bot else {
        panic!("bot test principal must be a user");
    };
    AuthzContext::for_subject_with_role(user, [(owner, role)], AuthPath::HostBearer)
}

async fn seed_group_membership(
    pg: &proxima_storage_pg::PgStorage,
    space_owner: &OwnerRef,
    relation: Relation,
    subject: &OwnerRef,
) {
    let OwnerRef::Group(group) = space_owner else {
        panic!("group membership can only seed group-owned spaces");
    };
    let OwnerRef::Personal(user) = subject else {
        panic!("group membership can only seed user members");
    };
    let permit = owner_write_permit(space_owner, proxima_core::AccessKind::Goal)
        .await
        .expect("seed group membership permit");
    pg.add_group_member(&permit, *group, *user, relation, Uuid::now_v7())
        .await
        .expect("seed group membership");
}

fn engine_for(pg: &proxima_storage_pg::PgStorage) -> Engine {
    Engine::new(FlavorRegistryFrozen::with_schemas(schemas_for_test()))
        .with_storage_ports(Arc::new(pg.clone()).storage_ports())
}

async fn event_row_counts(
    pool: &sqlx::PgPool,
    receipt_id: proxima_core::FactReceiptId,
) -> Result<(i64, i64), sqlx::Error> {
    let receipt_id_bytes = receipt_id.into_inner();
    let memories = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM proxima_core.memories WHERE receipt_id = $1",
    )
    .bind(receipt_id_bytes.as_slice())
    .fetch_one(pool)
    .await?;
    let events = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM proxima_core.fact_receipts WHERE receipt_id = $1",
    )
    .bind(receipt_id_bytes.as_slice())
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
    let owner = group_owner();
    let engine = engine_for(&pg);
    let draft = fresh_draft(&owner);
    let receipt_id = draft.receipt_id_for_owner(owner).expect("receipt id");
    let bot = bot_principal();
    let authz = bot_authz_with_role(&bot, owner, Role::viewer())
        .narrowed_to_owner(owner)
        .expect("viewer role narrows to target owner");
    let err = engine
        .authorize_fact_ingest(&authz, Relation::Ingest, draft, &[])
        .await
        .expect_err("missing fact_ingest role must reject before storage");

    assert_eq!(err.code, ErrorCode::Forbidden);
    assert!(err.message.contains("requires ingest on this owner"));
    assert_eq!(
        event_row_counts(pg.pool_for_tests(), receipt_id).await?,
        (0, 0)
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn source_ingest_only_authorizes_fact_ingest_with_write_grant()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = group_owner();
    let engine = engine_for(&pg);
    let bot = bot_principal();
    let authz = bot_authz_with_role(&bot, owner, Role::viewer())
        .narrowed_to_owner(owner)
        .expect("viewer role narrows to target owner");

    let err = engine
        .authorize_fact_ingest(&authz, Relation::Ingest, fresh_draft(&owner), &[])
        .await
        .expect_err("missing ingest grant must reject");
    assert_eq!(err.code, ErrorCode::Forbidden);
    assert!(err.message.contains("requires ingest on this owner"));

    seed_group_membership(&pg, &owner, Relation::Ingest, &bot).await;
    let authz = bot_authz_with_role(&bot, owner, Role::ingest())
        .narrowed_to_owner(owner)
        .expect("ingest role narrows to target owner");

    let authorized = engine
        .authorize_fact_ingest(&authz, Relation::Ingest, fresh_draft(&owner), &[])
        .await?;
    assert_eq!(*authorized.permit().owner(), owner);

    let err = engine
        .authorize_fact_ingest(&authz, Relation::Editor, fresh_draft(&owner), &[])
        .await
        .expect_err("ingest grant must not authorize editor writes");
    assert_eq!(err.code, ErrorCode::Forbidden);
    assert!(err.message.contains("requires editor on this owner"));

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
    let authorized = engine
        .authorize_fact_ingest(
            &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            Relation::Ingest,
            draft,
            &[],
        )
        .await?;
    let receipt_id = authorized
        .draft()
        .receipt_id_for_owner(*authorized.permit().owner())
        .expect("receipt id");

    let err = fact_ingest_with_sidecar_atomic(
        pg.pool_for_tests(),
        &authorized,
        Some("rollback-test-embed"),
        |_tx, _outcome| Box::pin(async move { Err(StorageError::Internal("boom".into())) }),
    )
    .await
    .expect_err("sidecar failure must surface");

    assert!(err.to_string().contains("boom"));
    assert_eq!(
        event_row_counts(pg.pool_for_tests(), receipt_id).await?,
        (0, 0)
    );
    assert_eq!(embedding_job_count(pg.pool_for_tests()).await?, 0);

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
    .execute(pg.pool_for_tests())
    .await?;

    let owner = owner_fixture();
    let permit = owner_write_permit(&owner, proxima_core::AccessKind::Fact).await?;
    let payload = UncitedFactPayload {
        note: format!("uncited fact {}", Uuid::now_v7()),
    };
    let sidecar_note = payload.note.clone();

    let outcome = ingest_fact(
        pg.pool_for_tests(),
        &permit,
        &payload,
        None,
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
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert!(citation_mapping_id.is_none());

    let cited_objects =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxima_core.cited_objects")
            .fetch_one(pg.pool_for_tests())
            .await?;
    let citation_mappings = sqlx::query_scalar::<_, i64>(
        "SELECT count(*)
           FROM proxima_core.citation_mappings
          WHERE memory_id = $1",
    )
    .bind(outcome.memory_id.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await?;
    let sidecars = sqlx::query_scalar::<_, i64>(
        "SELECT count(*)
           FROM public.uncited_fact_sidecar
          WHERE memory_id = $1
            AND note = $2",
    )
    .bind(outcome.memory_id.into_inner())
    .bind(&payload.note)
    .fetch_one(pg.pool_for_tests())
    .await?;

    assert_eq!(cited_objects, 0);
    assert_eq!(citation_mappings, 0);
    assert_eq!(sidecars, 1);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn ingest_fact_for_owner_plain_replays_closure_backed_fact()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let owner = owner_fixture();
    let permit = owner_write_permit(&owner, proxima_core::AccessKind::Fact).await?;
    let payload = UncitedFactPayload {
        note: format!("plain fact {}", Uuid::now_v7()),
    };

    let first = ingest_fact(
        pg.pool_for_tests(),
        &permit,
        &payload,
        None,
        |_tx, _outcome| Box::pin(async { Ok(()) }),
    )
    .await?;
    let replay = ingest_fact_for_owner_plain(pg.pool_for_tests(), &permit, &payload, None).await?;

    assert!(!first.idempotent_replay);
    assert!(replay.idempotent_replay);
    assert_eq!(replay.memory_id, first.memory_id);
    assert_eq!(replay.change_event_seq, first.change_event_seq);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
