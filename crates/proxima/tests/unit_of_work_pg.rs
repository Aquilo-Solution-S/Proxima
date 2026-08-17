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
