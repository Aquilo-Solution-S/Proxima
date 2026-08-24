//! Engine `UnitOfWork` — one-shot ingest, multi-write rollback, advisory lock.
#![allow(clippy::too_many_lines)]

use proxima::flavor::{FlavorBundle, NamedMigrator};
use proxima::{AppInfo, AuthPath, AuthzContext, FlavorApp, Proxima, ToolScope, company_owner};
use proxima_core::storage_ports::SidecarSessionRead;
use proxima_core::verbs::persist_mcp_call::McpCallLoggedV1;
use proxima_core::verbs::query::SidecarAtom;
use proxima_core::{
    AbstractionPayload, AgentDerivationV1, AgentNoteV1, AuthorDerivedRequestInput, EntityKind,
    FactPayload, InputContractId, MemoryId, MemoryOperatorKind, OperatorId, SchemaId,
    SchemaVersion, SidecarPayload, Speaker, UtteranceV1,
};
use proxima_core::{Role, UserId};
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
        model_id: "test",
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
        derived_from: origin,
        extra_refs: &[],
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
                model_id: "test",
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
                derived_from: std::slice::from_ref(&origin),
                extra_refs: &[],
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

/// Both owners log the SAME tool name into the same sidecar table, so the
/// test's predicate matches both rows and only the owner scope separates
/// them.
const TOOL: &str = "core_remember";

fn mcp_call(tool: &str, actor: &str) -> McpCallLoggedV1 {
    McpCallLoggedV1 {
        tool_name: tool.into(),
        actor_oid: actor.into(),
        actor_upn: format!("{actor}@example.test"),
        ok: true,
        error: None,
        latency_ms: 1,
        io_byte_len: 2,
        io_truncated: false,
        io_content_hash: [7_u8; 32],
    }
}

fn admin_authz_for(owner: proxima_core::Owner) -> AuthzContext {
    AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(owner, Role::admin())],
        AuthPath::HostBearer,
    )
    .narrowed_to_owner(owner)
    .expect("an admin on exactly this owner narrows to it")
}

/// The read half of `read → check → append`, inside the write transaction,
/// scoped to the owner the session authorized for.
///
/// Two claims, and the second is the load-bearing one. That a sidecar row can
/// be read is nothing — the read ports do that. What matters is that the read
/// sees THIS session's uncommitted write and is covered by the advisory lock
/// the session already holds, which is what a pool-scoped read cannot be; and
/// that the rows are the PERMIT'S owner's, stamped server-side, so a
/// predicate that matches another owner's row still does not return it.
#[tokio::test]
async fn unit_of_work_reads_its_own_sidecars_inside_the_transaction() {
    let db_name = unique_db_name("proxima_uow_read");
    create_db(&db_name).await.expect("PG required");
    let db_url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner_a = company_owner(Uuid::now_v7());
        let owner_b = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(db_url)
            .owner(owner_a)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let engine = built.engine();
        let authz_a = admin_authz_for(owner_a);
        let authz_b = admin_authz_for(owner_b);

        engine
            .ingest_typed_fact(&authz_b, "test/uow-read-b", &mcp_call(TOOL, "oid-b"))
            .await?;

        let note_read = SidecarSessionRead {
            table: "proxima_core.agent_note_v1",
            predicates: &[("title", SidecarAtom::Text("precondition".into()))],
            limit: None,
        };
        let call_read = SidecarSessionRead {
            table: "proxima_core.mcp_call_logged_v1",
            predicates: &[("tool_name", SidecarAtom::Text(TOOL.into()))],
            limit: None,
        };

        let mut uow = engine.unit_of_work(&authz_a).await?;
        uow.advisory_xact_lock(4242).await?;

        // A owns nothing yet: B's committed row is not A's to see, even
        // though it satisfies every predicate A wrote.
        let before = uow.read_own_sidecar(owner_a, &call_read).await?;
        assert!(
            before.is_empty(),
            "another owner's row is not in scope, predicate match or not: {before:?}"
        );

        let written = uow
            .ingest_fact("test/uow-read-a", &mcp_call(TOOL, "oid-a"))
            .await?;

        // Same transaction, uncommitted: A's own row IS visible, which is
        // what makes the precondition check binding.
        let rows = uow.read_own_sidecar(owner_a, &call_read).await?;
        assert_eq!(
            rows.len(),
            1,
            "the session sees its own write and only its own: {rows:?}"
        );
        assert_eq!(
            rows[0].get("actor_oid").and_then(serde_json::Value::as_str),
            Some("oid-a"),
            "and it is A's row, not B's: {rows:?}"
        );
        assert_eq!(
            rows[0].get("t").and_then(serde_json::Value::as_str),
            Some(written.memory_id.into_inner().to_string().as_str()),
            "keyed on the memory it belongs to: {rows:?}"
        );

        // Reading as B from A's session is refused at the write gate, not
        // served: the session's authz is what resolves the owner.
        let err = uow
            .read_own_sidecar(owner_b, &call_read)
            .await
            .expect_err("this session has no write authority for B");
        assert_eq!(
            err.code,
            proxima_core::ErrorCode::Forbidden,
            "a cross-owner read is refused at the gate, not served: {err:?}"
        );

        // The series-head lookup is owner-scoped through the memory join and
        // still answers for A's own schema.
        let note = AgentNoteV1 {
            note_id: Uuid::now_v7(),
            title: "precondition".into(),
            body: "precondition".into(),
            tags: Vec::new(),
            idempotency_key: Some("precondition".into()),
        };
        let note_written = uow.ingest_fact("test/uow-read-note", &note).await?;
        assert_eq!(
            uow.owned_series_head_memory_id(
                owner_a,
                &SchemaId::new(AgentNoteV1::SCHEMA_ID.into()),
                "proxima_core.agent_note_v1",
                &[("note_id", SidecarAtom::Uuid(note.note_id))],
            )
            .await?,
            Some(note_written.memory_id),
            "the series head is the row just appended"
        );

        // A surface that declares no owner column cannot be owner-scoped, so
        // it is refused rather than read wide.
        let err = uow
            .read_own_sidecar(owner_a, &note_read)
            .await
            .expect_err("a surface with no owner column cannot be scoped");
        assert!(
            format!("{err:?}").contains("declares no owner column"),
            "the refusal names why it cannot be scoped: {err:?}"
        );
        assert!(
            format!("{err:?}").contains("owned_series_head_memory_id"),
            "and names the fix: {err:?}"
        );

        // A table the frozen registry does not vouch for is refused.
        let err = uow
            .read_own_sidecar(
                owner_a,
                &SidecarSessionRead {
                    table: "public.not_a_registered_sidecar",
                    predicates: &[("tool_name", SidecarAtom::Text(TOOL.into()))],
                    limit: None,
                },
            )
            .await
            .expect_err("an unregistered table is not a declared surface");
        assert!(
            format!("{err:?}").contains("pg_sidecar!"),
            "the refusal names the fix: {err:?}"
        );

        // An unfiltered scan is a query, not a precondition check.
        let err = uow
            .read_own_sidecar(
                owner_a,
                &SidecarSessionRead {
                    table: "proxima_core.mcp_call_logged_v1",
                    predicates: &[],
                    limit: None,
                },
            )
            .await
            .expect_err("an unpredicated read is refused");
        assert!(
            format!("{err:?}").contains("at least one column predicate"),
            "the refusal says why: {err:?}"
        );

        uow.commit().await?;
        built.shutdown();
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("session sidecar read");
}
