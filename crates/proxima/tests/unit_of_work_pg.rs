//! Engine `UnitOfWork` — one-shot ingest, multi-write rollback, advisory lock.
#![allow(clippy::too_many_lines)]

#[path = "fixtures/split_core_db.rs"]
mod split_core_db;

use proxima::flavor::{FlavorBundle, NamedMigrator};
use proxima::{AppInfo, AuthPath, AuthzContext, FlavorApp, Proxima, ServedSurface, company_owner};
use proxima_core::verbs::goal_write::{
    GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest, GoalEvidenceRef, GoalTopologyWrite,
    IdempotencyKey,
};
use proxima_core::{
    AgentNoteV1, DerivedMemory, InterpretationSubjectKind, InterpretationV1, MemoryId,
    MemoryTarget, SeriesHandle, SimpleTextGoalV1,
};
use proxima_core::{ErrorCode, Role, UserId};
use proxima_pg_testkit::db_url;
use split_core_db::clone_split_core_db;
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

fn typed_goal_request(
    owner: proxima_core::OwnerId,
    assignment: MemoryId,
    evidence: Vec<GoalEvidenceRef>,
    request_id: &str,
    author_self_perspective_id: Option<MemoryId>,
) -> GoalCreateRequest<SimpleTextGoalV1> {
    GoalCreateRequest {
        owner,
        topology: GoalTopologyWrite::new(
            Some(GoalAssignmentTarget::perspective(assignment)),
            Vec::new(),
            evidence,
        )
        .expect("fixture topology is valid"),
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
    let split_db = clone_split_core_db("proxima_typed_goal")
        .await
        .expect("PG required");
    let db_name = split_db.name().to_owned();
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let admin_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner)
            .allow_insecure_single_owner()
            .served_surface(ServedSurface::All)
            .build()
            .await?;
        let authz = built.single_owner_authz().expect("single owner");
        let engine = built.host().engine();
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
        .fetch_one(&admin_pool)
        .await?;
        assert_eq!(missing, 0, "dropped UoW must roll back pending memories");
        let missing_goal: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal WHERE t = $1")
                .bind(pending_goal_id)
                .fetch_one(&admin_pool)
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
                .fetch_one(&admin_pool)
                .await?;
        assert_eq!(
            topology.0,
            Some(committed_perspective.memory_id.into_inner())
        );
        assert_eq!(topology.1, vec![committed_fact.memory_id.into_inner()]);

        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("typed Goal UoW test failed");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn typed_goal_pending_foreign_perspective_rejects_cross_owner_assignment() {
    let split_db = clone_split_core_db("proxima_typed_goal_foreign_pending")
        .await
        .expect("PG required");
    let db_name = split_db.name().to_owned();
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let admin_pool = sqlx::PgPool::connect(&db_url(&db_name)).await?;
        let owner_a = company_owner(Uuid::now_v7());
        let owner_b = company_owner(Uuid::now_v7());
        let built = Proxima::<EmptyApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(owner_a)
            .allow_insecure_single_owner()
            .served_surface(ServedSurface::All)
            .build()
            .await?;
        let authz = proxima_core::test_fixtures::authenticated_context(
            AuthzContext::for_subject_with_role(
                UserId::new(Uuid::now_v7()),
                [(owner_a, Role::admin()), (owner_b, Role::admin())],
                AuthPath::HostBearer,
            ),
        );
        let engine = built.host().engine();
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
        .fetch_one(&admin_pool)
        .await?;
        assert_eq!(missing_memory, 0, "dropped UoW must roll back foreign rows");
        built.shutdown().await;
        Ok(())
    }
    .await;
    result.expect("cross-owner pending typed Goal regression failed");
}
