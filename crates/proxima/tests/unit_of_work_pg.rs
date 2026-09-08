//! Engine `UnitOfWork` — one-shot ingest, multi-write rollback, advisory lock.
#![allow(clippy::too_many_lines)]

use proxima::flavor::{FlavorBundle, NamedMigrator};
use proxima::{AppInfo, AuthPath, AuthzContext, FlavorApp, Proxima, ToolScope, company_owner};
use proxima_core::storage_ports::SidecarSessionRead;
use proxima_core::verbs::goal_write::{
    GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest, GoalEvidenceRef, GoalTopologyWrite,
    IdempotencyKey,
};
use proxima_core::verbs::persist_mcp_call::McpCallLoggedV1;
use proxima_core::verbs::query::SidecarAtom;
use proxima_core::{
    AgentDerivationV1, AgentNoteV1, DerivationIdentity, DerivedMemory, FactPayload,
    InputContractId, InterpretationSubjectKind, InterpretationV1, MemoryId, MemoryTarget,
    OperatorId, SchemaId, SeriesHandle, SimpleTextGoalV1, Speaker, UtteranceV1,
};
use proxima_core::{ErrorCode, Role, UserId};
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

fn derived_abstraction(
    owner: proxima_core::Owner,
    origin: MemoryId,
    title: &str,
) -> Result<DerivedMemory, proxima_core::ProtocolError> {
    DerivedMemory::abstraction(
        MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
        owner,
        title,
        AgentDerivationV1 {
            title: title.into(),
            body: title.into(),
            tags: Vec::new(),
            idempotency_key: None,
            source_memory_ids: vec![origin.into_inner()],
            model_id: "test".into(),
            client_name: "test".into(),
            client_version: "1".into(),
        },
        [origin],
        DerivationIdentity::new(
            OperatorId::new(Uuid::now_v7()),
            InputContractId::new(Uuid::now_v7()),
        ),
    )
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
            .ingest_fact(
                &authz,
                proxima::FactWrite::new(owner, "test/uow-one-shot", &note("one-shot")),
            )
            .await?;
        assert!(!one.memory_id.into_inner().is_nil());

        {
            let mut uow = engine.unit_of_work(&authz).await?;
            uow.ingest_fact(proxima::FactWrite::new(
                owner,
                "test/uow-a",
                &note("rollback-a"),
            ))
            .await?;
            uow.ingest_fact(proxima::FactWrite::new(
                owner,
                "test/uow-b",
                &note("rollback-b"),
            ))
            .await?;
            uow.derive_memory(derived_abstraction(owner, one.memory_id, "derived in uow")?)
                .await?;
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
            .ingest_fact(
                &authz,
                proxima::FactWrite::new(owner, "test/uow-cite", &note("cited")).citation(
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
        let first = uow
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "test/uow-session",
                &note("first"),
            ))
            .await?;
        let second = uow
            .ingest_fact(
                proxima::FactWrite::new(owner, "test/uow-session", &note("second"))
                    .refs([first.memory_id]),
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
async fn unit_of_work_derive_memories_is_atomic() {
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
            .ingest_fact(
                &authz,
                proxima::FactWrite::new(owner, "test/uow-derived-all", &note("source")),
            )
            .await?;
        {
            let mut uow = engine.unit_of_work(&authz).await?;
            let written = uow
                .derive_memories([
                    derived_abstraction(owner, source.memory_id, "batch-a")?,
                    derived_abstraction(owner, source.memory_id, "batch-b")?,
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
            .derive_memories([
                derived_abstraction(owner, source.memory_id, "commit-a")?,
                derived_abstraction(owner, source.memory_id, "commit-b")?,
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
    result.expect("unit of work derive_memories pg test failed");
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

fn typed_goal_request(
    owner: proxima_core::Owner,
    assignment: MemoryId,
    evidence: Vec<GoalEvidenceRef>,
    request_id: &str,
    author_self_perspective_id: Option<MemoryId>,
) -> GoalCreateRequest<SimpleTextGoalV1> {
    GoalCreateRequest {
        owner,
        topology: GoalTopologyWrite::new(
            GoalAssignmentTarget::perspective(assignment),
            Vec::new(),
            evidence,
        )
        .expect("fixture topology is valid"),
        wake: None,
        title: "typed goal".to_owned(),
        text: "typed goal text".to_owned(),
        payload: SimpleTextGoalV1 {},
        request_id: IdempotencyKey::new(request_id).expect("fixture request id is valid"),
        authorship: GoalAuthorship::User,
        author_self_perspective_id,
    }
}

#[tokio::test]
async fn typed_goal_standalone_and_uow_validate_pending_kinds() {
    let db_name = unique_db_name("proxima_typed_goal");
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
        let fact = engine
            .ingest_fact(
                &authz,
                proxima::FactWrite::new(owner, "typed-goal-fact", &note("evidence")),
            )
            .await?;
        let perspective = engine
            .derive_memory(
                &authz,
                DerivedMemory::interpretation(
                    MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                    owner,
                    "pending perspective",
                    InterpretationV1 {
                        claim: "claim".to_owned(),
                        confidence: 80,
                        subject_memory_ids: vec![fact.memory_id.into_inner()],
                        subject_kinds: vec![InterpretationSubjectKind::Fact],
                        model_id: "test".to_owned(),
                        client_name: "test".to_owned(),
                        client_version: "1".to_owned(),
                    },
                ),
            )
            .await?;
        let standalone = engine
            .create_goal(
                &authz,
                typed_goal_request(
                    owner,
                    perspective.memory_id,
                    vec![GoalEvidenceRef::new(fact.memory_id)],
                    "typed-goal-standalone",
                    None,
                ),
            )
            .await?;
        let mut uow = engine.unit_of_work(&authz).await?;
        let replay = uow
            .create_goal(typed_goal_request(
                owner,
                perspective.memory_id,
                vec![GoalEvidenceRef::new(fact.memory_id)],
                "typed-goal-standalone",
                None,
            ))
            .await?;
        assert_eq!(replay.goal_id, standalone.goal_id);
        assert!(replay.idempotent_replay);
        uow.commit().await?;

        // These three rows are all uncommitted in one UoW. Goal validation
        // must use the actual kinds recorded by the preceding writes.
        let mut pending = engine.unit_of_work(&authz).await?;
        let pending_fact = pending
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "typed-goal-pending-fact",
                &note("pending evidence"),
            ))
            .await?;
        let pending_perspective = pending
            .derive_memory(DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                owner,
                "pending perspective",
                InterpretationV1 {
                    claim: "pending claim".to_owned(),
                    confidence: 90,
                    subject_memory_ids: vec![pending_fact.memory_id.into_inner()],
                    subject_kinds: vec![InterpretationSubjectKind::Fact],
                    model_id: "test".to_owned(),
                    client_name: "test".to_owned(),
                    client_version: "1".to_owned(),
                },
            ))
            .await?;
        let pending_goal = pending
            .create_goal(typed_goal_request(
                owner,
                pending_perspective.memory_id,
                vec![GoalEvidenceRef::new(pending_fact.memory_id)],
                "typed-goal-pending-valid",
                Some(pending_perspective.memory_id),
            ))
            .await?;
        assert!(!pending_goal.idempotent_replay);

        let error = pending
            .create_goal(typed_goal_request(
                owner,
                pending_fact.memory_id,
                vec![GoalEvidenceRef::new(pending_fact.memory_id)],
                "typed-goal-wrong-assignment",
                None,
            ))
            .await
            .expect_err("Fact assignment must be rejected");
        assert!(error.message.contains("assignment"));

        let error = pending
            .create_goal(typed_goal_request(
                owner,
                pending_perspective.memory_id,
                vec![GoalEvidenceRef::new(pending_perspective.memory_id)],
                "typed-goal-wrong-evidence",
                None,
            ))
            .await
            .expect_err("Perspective evidence must be rejected");
        assert!(error.message.contains("evidence"));

        let error = pending
            .create_goal(typed_goal_request(
                owner,
                pending_perspective.memory_id,
                vec![GoalEvidenceRef::new(pending_fact.memory_id)],
                "typed-goal-wrong-self",
                Some(pending_fact.memory_id),
            ))
            .await
            .expect_err("Fact author-self must be rejected");
        assert!(error.message.contains("author_self_perspective_id"));

        let pending_fact_id = pending_fact.memory_id.into_inner();
        let pending_perspective_id = pending_perspective.memory_id.into_inner();
        let pending_goal_id = pending_goal.goal_id.into_inner();
        drop(pending);
        let missing: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory WHERE t = ANY($1)",
        )
        .bind(vec![pending_fact_id, pending_perspective_id])
        .fetch_one(built.pool_for_tests())
        .await?;
        assert_eq!(missing, 0, "dropped UoW must roll back pending memories");
        let missing_goal: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal WHERE t = $1")
                .bind(pending_goal_id)
                .fetch_one(built.pool_for_tests())
                .await?;
        assert_eq!(missing_goal, 0, "dropped UoW must roll back pending goals");

        let mut committed = engine.unit_of_work(&authz).await?;
        let committed_fact = committed
            .ingest_fact(proxima::FactWrite::new(
                owner,
                "typed-goal-committed-fact",
                &note("committed evidence"),
            ))
            .await?;
        let committed_perspective = committed
            .derive_memory(DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                owner,
                "committed perspective",
                InterpretationV1 {
                    claim: "committed claim".to_owned(),
                    confidence: 90,
                    subject_memory_ids: vec![committed_fact.memory_id.into_inner()],
                    subject_kinds: vec![InterpretationSubjectKind::Fact],
                    model_id: "test".to_owned(),
                    client_name: "test".to_owned(),
                    client_version: "1".to_owned(),
                },
            ))
            .await?;
        let committed_goal = committed
            .create_goal(typed_goal_request(
                owner,
                committed_perspective.memory_id,
                vec![GoalEvidenceRef::new(committed_fact.memory_id)],
                "typed-goal-committed",
                Some(committed_perspective.memory_id),
            ))
            .await?;
        committed.commit().await?;
        let topology: (Option<Uuid>, Vec<Uuid>) =
            sqlx::query_as("SELECT assignment_t, evidence_t FROM proxima_core.goal WHERE t = $1")
                .bind(committed_goal.goal_id.into_inner())
                .fetch_one(built.pool_for_tests())
                .await?;
        assert_eq!(
            topology.0,
            Some(committed_perspective.memory_id.into_inner())
        );
        assert_eq!(topology.1, vec![committed_fact.memory_id.into_inner()]);

        built.shutdown();
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("typed Goal UoW test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn typed_goal_pending_foreign_perspective_rejects_cross_owner_assignment() {
    let db_name = unique_db_name("proxima_typed_goal_foreign_pending");
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
        let authz = AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner_a, Role::admin()), (owner_b, Role::admin())],
            AuthPath::HostBearer,
        );
        let engine = built.engine();
        let mut uow = engine.unit_of_work(&authz).await?;
        let foreign_fact = uow
            .ingest_fact(proxima::FactWrite::new(
                owner_b,
                "typed-goal-foreign-pending-fact",
                &note("foreign pending evidence"),
            ))
            .await?;
        let foreign_perspective = uow
            .derive_memory(DerivedMemory::interpretation(
                MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
                owner_b,
                "foreign pending perspective",
                InterpretationV1 {
                    claim: "foreign claim".to_owned(),
                    confidence: 90,
                    subject_memory_ids: vec![foreign_fact.memory_id.into_inner()],
                    subject_kinds: vec![InterpretationSubjectKind::Fact],
                    model_id: "test".to_owned(),
                    client_name: "test".to_owned(),
                    client_version: "1".to_owned(),
                },
            ))
            .await?;
        let foreign_fact_id = foreign_fact.memory_id.into_inner();
        let foreign_perspective_id = foreign_perspective.memory_id.into_inner();
        let error = uow
            .create_goal(typed_goal_request(
                owner_a,
                foreign_perspective.memory_id,
                Vec::new(),
                "typed-goal-cross-owner-pending",
                None,
            ))
            .await
            .expect_err("pending foreign Perspective must be rejected");
        assert_eq!(error.code, ErrorCode::Forbidden);
        assert_eq!(error.message, "entry not found");
        drop(uow);

        let missing_memory: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memory WHERE t = ANY($1)",
        )
        .bind(vec![foreign_fact_id, foreign_perspective_id])
        .fetch_one(built.pool_for_tests())
        .await?;
        assert_eq!(missing_memory, 0, "dropped UoW must roll back foreign rows");
        built.shutdown();
        Ok(())
    }
    .await;
    drop_db(&db_name).await.expect("drop fixture");
    result.expect("cross-owner pending typed Goal regression failed");
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
            .ingest_fact(
                &authz_b,
                proxima::FactWrite::new(owner_b, "test/uow-read-b", &mcp_call(TOOL, "oid-b")),
            )
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
            .ingest_fact(proxima::FactWrite::new(
                owner_a,
                "test/uow-read-a",
                &mcp_call(TOOL, "oid-a"),
            ))
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
        let note_written = uow
            .ingest_fact(proxima::FactWrite::new(
                owner_a,
                "test/uow-read-note",
                &note,
            ))
            .await?;
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
