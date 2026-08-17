//! S2: Engine `UnitOfWork` — one-shot ingest, multi-write rollback, advisory lock.
#![allow(clippy::too_many_lines)]

use proxima::flavor::{FlavorBundle, NamedMigrator};
use proxima::{AppInfo, FlavorApp, Proxima, ToolScope, company_owner};
use proxima_core::{
    AbstractionPayload, AgentDerivationV1, AgentNoteV1, AuthorDerivedRequestInput, EntityKind,
    InputContractId, MemoryId, MemoryOperatorKind, OperatorId, SchemaId, SchemaVersion,
    SidecarPayload, Speaker, UtteranceV1,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use uuid::Uuid;

struct EmptyApp;

impl FlavorBundle for EmptyApp {
    fn register(
        _: &mut proxima_core::FlavorRegistry,
    ) -> Result<(), proxima_core::FlavorRegistryError> {
        Ok(())
    }
    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for EmptyApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "uow-test",
            title: "uow-test",
            version: "0",
        }
    }
}

fn note(title: &str) -> AgentNoteV1 {
    AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: title.into(),
        body: title.into(),
        tags: Vec::new(),
        idempotency_key: Some(title.into()),
    }
}

fn derived_abstraction<'a>(
    owner: proxima_core::Owner,
    origin: &'a [proxima_core::EdgeEndpoint],
    title: &str,
) -> AuthorDerivedRequestInput<'a> {
    AuthorDerivedRequestInput {
        memory_id: MemoryId::new(Uuid::now_v7()),
        owner,
        kind: EntityKind::Abstraction,
        text: title.into(),
        schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
        operator_kind: MemoryOperatorKind::FtoA,
        operator_id: OperatorId::new(Uuid::now_v7()),
        input_contract_id: InputContractId::new(Uuid::now_v7()),
        source_batch_id: None,
        model_id: "test",
        prompt_version: "1",
        sidecar_payload: SidecarPayload::abstraction(AgentDerivationV1 {
            title: title.into(),
            body: title.into(),
            tags: Vec::new(),
            idempotency_key: None,
            source_memory_ids: vec![
                origin[0]
                    .memory_id()
                    .map_or_else(Uuid::nil, MemoryId::into_inner),
            ],
            model_id: "test".into(),
            client_name: "test".into(),
            client_version: "1".into(),
        }),
        authoring_perspective_id: None,
        derived_from: origin,
        supersedes: None,
        lexical_language: None,
    }
}

#[tokio::test]
async fn unit_of_work_one_shot_and_rollback_and_lock() {
    let db_name = unique_db_name("proxima_uow");
    create_db(&db_name).await.expect("PG required");
    let db_url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(db_url)
            .owner(owner)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let authz = built.single_owner_authz().expect("single owner");
        let engine = built.engine();

        let one = engine
            .ingest_typed_fact(&authz, "test/uow-one-shot", &note("one-shot"))
            .await?;
        assert!(!one.memory_id.into_inner().is_nil());

        {
            let mut uow = engine.unit_of_work(&authz).await?;
            uow.ingest_fact("test/uow-a", &note("rollback-a")).await?;
            uow.ingest_fact("test/uow-b", &note("rollback-b")).await?;
            let origin = proxima_core::EdgeEndpoint::memory(EntityKind::Fact, one.memory_id);
            let derived = AuthorDerivedRequestInput {
                memory_id: MemoryId::new(Uuid::now_v7()),
                owner,
                kind: EntityKind::Abstraction,
                text: "derived in uow".into(),
                schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
                operator_kind: MemoryOperatorKind::FtoA,
                operator_id: OperatorId::new(Uuid::now_v7()),
                input_contract_id: InputContractId::new(Uuid::now_v7()),
                source_batch_id: None,
                model_id: "test",
                prompt_version: "1",
                sidecar_payload: SidecarPayload::abstraction(AgentDerivationV1 {
                    title: "uow".into(),
                    body: "body".into(),
                    tags: Vec::new(),
                    idempotency_key: None,
                    source_memory_ids: vec![one.memory_id.into_inner()],
                    model_id: "test".into(),
                    client_name: "test".into(),
                    client_version: "1".into(),
                }),
                authoring_perspective_id: None,
                derived_from: std::slice::from_ref(&origin),
                supersedes: None,
                lexical_language: None,
            };
            uow.author_derived(derived).await?;
        }
        let rolled: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t <> $1")
                .bind(one.memory_id.into_inner())
                .fetch_one(built.pool_for_tests())
                .await?;
        assert_eq!(
            rolled, 0,
            "drop without commit must leave only the one-shot"
        );

        let mut held = engine.unit_of_work(&authz).await?;
        held.advisory_xact_lock(42).await?;
        let started = std::time::Instant::now();
        let engine2 = engine.clone();
        let authz2 = authz.clone();
        let waiter = tokio::spawn(async move {
            let mut other = engine2.unit_of_work(&authz2).await.unwrap();
            other.advisory_xact_lock(42).await.unwrap();
            other.commit().await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        held.commit().await?;
        waiter.await?;
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(100),
            "second UoW must wait on the advisory lock"
        );

        let _ = UtteranceV1 {
            speaker: Speaker::User,
            conversation_id: "x".into(),
            text: "x".into(),
        };
        built.shutdown();
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("unit of work pg test failed");
}

#[tokio::test]
async fn unit_of_work_citation_spec_lands_cited_object() {
    let db_name = unique_db_name("proxima_uow_cite");
    create_db(&db_name).await.expect("PG required");
    let db_url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(db_url)
            .owner(owner)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let authz = built.single_owner_authz().expect("single owner");
        let engine = built.engine();
        let hash = [0x11u8; 32];
        let outcome = engine
            .ingest_typed_fact_with(
                &authz,
                proxima::TypedFactIngest::new("test/uow-cite", &note("cited")).citation(
                    proxima::CitationSpec::v1(
                        proxima::UPLOADED_BLOB_SCHEMA_ID,
                        hash,
                        proxima::UPLOADED_BLOB_WHOLE_SCHEMA_ID,
                    ),
                ),
            )
            .await?;
        let cited = outcome
            .cited_object_id
            .expect("DraftHint citation must persist a blob");
        let blob_hash: Vec<u8> =
            sqlx::query_scalar("SELECT content_hash FROM proxima_core.blob WHERE blob_id = $1")
                .bind(cited)
                .fetch_one(built.pool_for_tests())
                .await?;
        assert_eq!(blob_hash, hash);
        built.shutdown();
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("unit of work citation pg test failed");
}

#[tokio::test]
async fn unit_of_work_later_write_may_cite_earlier_uncommitted_fact() {
    let db_name = unique_db_name("proxima_uow_session");
    create_db(&db_name).await.expect("PG required");
    let db_url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(db_url)
            .owner(owner)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let authz = built.single_owner_authz().expect("single owner");
        let engine = built.engine();
        let mut uow = engine.unit_of_work(&authz).await?;
        let first = uow.ingest_fact("test/uow-session", &note("first")).await?;
        let origin = proxima_core::EdgeEndpoint::memory(EntityKind::Fact, first.memory_id);
        let second = uow
            .ingest_typed(
                proxima::TypedFactIngest::new("test/uow-session", &note("second"))
                    .derived_from([origin]),
            )
            .await?;
        uow.commit().await?;
        assert_ne!(first.memory_id, second.memory_id);
        let refs: Vec<Uuid> =
            sqlx::query_scalar("SELECT unnest(refs) FROM proxima_core.memory WHERE t = $1")
                .bind(second.memory_id.into_inner())
                .fetch_all(built.pool_for_tests())
                .await?;
        assert!(
            refs.contains(&first.memory_id.into_inner()),
            "second Fact must pin the uncommitted first; got {refs:?}"
        );
        built.shutdown();
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("unit of work session-visible cite failed");
}

#[tokio::test]
async fn unit_of_work_author_derived_all_is_atomic() {
    let db_name = unique_db_name("proxima_uow_derived_all");
    create_db(&db_name).await.expect("PG required");
    let db_url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(db_url)
            .owner(owner)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let authz = built.single_owner_authz().expect("single owner");
        let engine = built.engine();
        let source = engine
            .ingest_typed_fact(&authz, "test/uow-derived-all", &note("source"))
            .await?;
        let origin = proxima_core::EdgeEndpoint::memory(EntityKind::Fact, source.memory_id);
        let origins = [origin];
        {
            let mut uow = engine.unit_of_work(&authz).await?;
            let written = uow
                .author_derived_all([
                    derived_abstraction(owner, &origins, "batch-a"),
                    derived_abstraction(owner, &origins, "batch-b"),
                ])
                .await?;
            assert_eq!(written.len(), 2);
        }
        let rolled: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t <> $1")
                .bind(source.memory_id.into_inner())
                .fetch_one(built.pool_for_tests())
                .await?;
        assert_eq!(rolled, 0, "drop without commit must roll the whole batch");

        let mut uow = engine.unit_of_work(&authz).await?;
        let written = uow
            .author_derived_all([
                derived_abstraction(owner, &origins, "commit-a"),
                derived_abstraction(owner, &origins, "commit-b"),
            ])
            .await?;
        uow.commit().await?;
        assert_eq!(written.len(), 2);
        let landed: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory WHERE t = ANY($1)",
        )
        .bind(
            written
                .iter()
                .map(|row| row.memory_id.into_inner())
                .collect::<Vec<_>>(),
        )
        .fetch_one(built.pool_for_tests())
        .await?;
        assert_eq!(landed, 2);

        built.shutdown();
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("unit of work author_derived_all pg test failed");
}
