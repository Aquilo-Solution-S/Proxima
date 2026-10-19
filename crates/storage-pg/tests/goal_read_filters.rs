//! Goal series selectors, mutation-safe paging, and Fact reference filters.
#![allow(clippy::missing_errors_doc, clippy::too_many_lines)]

use proxima_core::read_models::MemorySchemaSpec;
use proxima_core::storage_ports::{FactIngestPort, MemoryReadPort, OwnerWritePermit};
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::verbs::fact_ingest::{
    AuthorizedFactWrite, AuthorizedNodeLinks, FactWriteCommand,
};
use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{EntityKind, QueryRequest, SupersessionStatus};
use proxima_core::{
    AccessKind, EdgeEndpoint, GoalId, MemoryId, OwnerId, SchemaId, SchemaVersion, UserId,
};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::test_fixtures::fresh_pg;
use proxima_storage_pg::verbs::goal_timeseries::{GoalWriteCommand, GoalWriteOutcome, write_goal};
use uuid::Uuid;

const FACT_SCHEMA: &str = "core/test-fact-v1";
const OTHER_SCHEMA: &str = "core/other-test-fact-v1";

type TestError = Box<dyn std::error::Error>;

fn owner() -> OwnerId {
    OwnerId::from(UserId::new(Uuid::now_v7()))
}

fn schemas() -> Vec<MemorySchemaSpec> {
    [FACT_SCHEMA, OTHER_SCHEMA]
        .map(|schema| {
            MemorySchemaSpec::new(
                EntityKind::Fact,
                SchemaId::new(schema.into()),
                SchemaVersion::new(1),
                None,
            )
        })
        .into()
}

async fn fact(pg: &PgStorage, owner: OwnerId, schema: &str, targets: &[Uuid]) -> MemoryId {
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let draft = FactWriteCommand {
        schema_id: SchemaId::new(schema.into()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    };
    let links = AuthorizedNodeLinks::new_for_tests(
        Vec::new(),
        targets
            .iter()
            .map(|t| EdgeEndpoint::goal(GoalId::new(*t)))
            .collect(),
    );
    let authorized =
        AuthorizedFactWrite::new_with_links_for_tests(permit, draft, None, Vec::new(), links);
    pg.ingest_authorized_fact_atomic(&authorized, &[])
        .await
        .unwrap()
        .memory_id
}

async fn goal(
    pg: &PgStorage,
    owner: OwnerId,
    handle: Option<Uuid>,
    state: GoalState,
) -> GoalWriteOutcome {
    let close_fact_t = if matches!(state, GoalState::Achieved | GoalState::Abandoned) {
        Some(fact(pg, owner, FACT_SCHEMA, &[]).await.into_inner())
    } else {
        None
    };
    let mut tx = pg.pool_for_tests().begin().await.unwrap();
    let result = write_goal(
        &mut tx,
        &owner,
        proxima_storage_pg::test_fixtures::writer(),
        &GoalWriteCommand {
            handle,
            schema_id: "core/task-v1".into(),
            title: "goal read fixture".into(),
            text: None,
            state,
            request_id: Uuid::now_v7().to_string(),
            close_fact_t,
            assignment_t: None,
            dependency_t: Vec::new(),
            evidence_t: Vec::new(),
            mint_write_act: false,
            write_act_t: None,
            parent_handle: None,
        },
        1,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    result
}

#[tokio::test]
async fn goal_handles_find_latest_and_open_pages_survive_lifecycle_writes() -> Result<(), TestError>
{
    let (pg, _db) = fresh_pg("proxima_goal_reads").await;
    let reader = owner();
    let reader_scope = owner_scope_for_tests(&[reader]);
    let a = goal(&pg, reader, None, GoalState::Active).await;
    let a_paused = goal(&pg, reader, Some(a.handle), GoalState::Paused).await;
    let a_resumed = goal(&pg, reader, Some(a.handle), GoalState::Active).await;
    let b = goal(&pg, reader, None, GoalState::Active).await;
    let c = goal(&pg, reader, None, GoalState::Active).await;
    let terminal = goal(&pg, reader, None, GoalState::Abandoned).await;
    let foreign = goal(&pg, owner(), None, GoalState::Active).await;
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_handles = vec![a.handle, b.handle, foreign.handle, Uuid::now_v7()];
    let heads = pg
        .query_memories(Some(&reader_scope), &[reader], &req, &[])
        .await?;
    assert_eq!(
        heads
            .goals
            .iter()
            .map(|g| g.id.into_inner())
            .collect::<Vec<_>>(),
        [b.t, a_resumed.t]
    );
    for absent in [foreign.handle, Uuid::now_v7()] {
        req.goal_handles = vec![absent];
        assert!(
            pg.query_memories(Some(&reader_scope), &[reader], &req, &[])
                .await?
                .goals
                .is_empty()
        );
    }
    req.goal_handles = vec![a.handle];
    req.schema_id = Some(SchemaId::new("core/task-v1".into()));
    assert_eq!(
        pg.query_memories(Some(&reader_scope), &[reader], &req, &[])
            .await?
            .goals[0]
            .id
            .into_inner(),
        a_resumed.t
    );
    req.schema_id = Some(SchemaId::new("core/other-goal-v1".into()));
    assert!(
        pg.query_memories(Some(&reader_scope), &[reader], &req, &[])
            .await?
            .goals
            .is_empty()
    );

    req.schema_id = None;
    req.goal_handles.clear();
    req.goal_states = [GoalState::Active, GoalState::Paused].into();
    req.limit = 1;
    let page = pg
        .query_memories(Some(&reader_scope), &[reader], &req, &[])
        .await?;
    assert_eq!(page.goals[0].handle, c.handle);
    let c_paused = goal(&pg, reader, Some(c.handle), GoalState::Paused).await;
    let a_paused_again = goal(&pg, reader, Some(a.handle), GoalState::Paused).await;
    let mut seen = vec![page.goals[0].handle];
    req.page.after = page.next_cursor;
    loop {
        let page = pg
            .query_memories(Some(&reader_scope), &[reader], &req, &[])
            .await?;
        seen.extend(page.goals.iter().map(|g| g.handle));
        assert!(
            seen.len() <= 3,
            "no duplicate series after lifecycle changes"
        );
        let Some(cursor) = page.next_cursor else {
            break;
        };
        req.page.after = Some(cursor);
    }
    assert_eq!(seen, [c.handle, b.handle, a.handle]);

    req.supersession = SupersessionStatus::IncludeSuperseded;
    req.goal_states.clear();
    req.page.after = None;
    let mut history = Vec::new();
    loop {
        let page = pg
            .query_memories(Some(&reader_scope), &[reader], &req, &[])
            .await?;
        history.extend(page.goals.iter().map(|g| (g.handle, g.id.into_inner())));
        assert!(history.len() <= 8);
        let Some(cursor) = page.next_cursor else {
            break;
        };
        req.page.after = Some(cursor);
    }
    let mut expected = [
        a,
        a_paused,
        a_resumed,
        b,
        c,
        terminal,
        c_paused,
        a_paused_again,
    ]
    .iter()
    .map(|g| (g.handle, g.t))
    .collect::<Vec<_>>();
    expected.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(history, expected);
    Ok(())
}

#[tokio::test]
async fn fact_goal_series_filters_match_all_versions_before_paging_and_respect_scope()
-> Result<(), TestError> {
    let (pg, _db) = fresh_pg("proxima_goal_fact_reads").await;
    let reader = owner();
    let reader_scope = owner_scope_for_tests(&[reader]);
    let foreign_owner = owner();
    let a = goal(&pg, reader, None, GoalState::Active).await;
    let a_paused = goal(&pg, reader, Some(a.handle), GoalState::Paused).await;
    let b = goal(&pg, reader, None, GoalState::Active).await;
    let hidden = goal(&pg, foreign_owner, None, GoalState::Active).await;
    let old = fact(&pg, reader, FACT_SCHEMA, &[a.t]).await;
    let latest = fact(&pg, reader, FACT_SCHEMA, &[a_paused.t]).await;
    let both = fact(&pg, reader, FACT_SCHEMA, &[a.t, b.t]).await;
    let b_only = fact(&pg, reader, FACT_SCHEMA, &[b.t]).await;
    let other_schema = fact(&pg, reader, OTHER_SCHEMA, &[a.t]).await;
    fact(&pg, foreign_owner, FACT_SCHEMA, &[a.t]).await;
    fact(&pg, reader, FACT_SCHEMA, &[hidden.t]).await;
    // Newer unrelated rows must not consume any slot in the filtered pages.
    for _ in 0..8 {
        fact(&pg, reader, FACT_SCHEMA, &[]).await;
    }
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Fact);
    req.schema_id = Some(SchemaId::new(FACT_SCHEMA.into()));
    req.referenced_goal_handles = vec![a.handle];
    req.limit = 1;
    for supersession in [
        SupersessionStatus::HeadsOnly,
        SupersessionStatus::IncludeSuperseded,
    ] {
        req.supersession = supersession;
        req.page.after = None;
        let mut seen = Vec::new();
        loop {
            let page = pg
                .query_memories(Some(&reader_scope), &[reader], &req, &schemas())
                .await?;
            seen.extend(page.memories.iter().map(|m| m.id));
            assert!(seen.len() <= 3);
            let Some(cursor) = page.next_cursor else {
                break;
            };
            req.page.after = Some(cursor);
        }
        assert_eq!(seen, [both, latest, old]);
    }
    req.page.after = None;
    req.limit = 100;
    req.referenced_goal_handles.push(b.handle);
    let rows = pg
        .query_memories(Some(&reader_scope), &[reader], &req, &schemas())
        .await?;
    assert_eq!(
        rows.memories.iter().map(|m| m.id).collect::<Vec<_>>(),
        [b_only, both, latest, old]
    );
    req.schema_id = Some(SchemaId::new(OTHER_SCHEMA.into()));
    assert_eq!(
        pg.query_memories(Some(&reader_scope), &[reader], &req, &schemas())
            .await?
            .memories[0]
            .id,
        other_schema
    );
    req.schema_id = None;
    for handle in [hidden.handle, Uuid::now_v7()] {
        req.referenced_goal_handles = vec![handle];
        assert!(
            pg.query_memories(Some(&reader_scope), &[reader], &req, &schemas())
                .await?
                .memories
                .is_empty()
        );
    }
    req.referenced_goal_handles = vec![a.handle];
    req.entity_kind = Some(EntityKind::Abstraction);
    assert!(
        pg.query_memories(Some(&reader_scope), &[reader], &req, &schemas())
            .await?
            .memories
            .is_empty()
    );
    Ok(())
}

struct Verifier(proxima_core::OwnerRoles);

#[async_trait::async_trait]
impl proxima_core::Authenticator for Verifier {
    async fn authenticate(
        &self,
        _: &proxima_core::Credentials,
    ) -> Result<proxima_core::AuthzContext, proxima_core::AuthError> {
        Ok(proxima_core::AuthzContext::server_resolved(
            self.0.clone(),
            proxima_core::AuthPath::HostBearer,
        ))
    }
}

#[tokio::test]
async fn goal_reference_filters_preserve_kind_and_owner_nondisclosure_under_rls()
-> Result<(), TestError> {
    use proxima_core::{Credentials, GroupId, OwnerRoles, Role};
    use proxima_pg_testkit::SplitRoleDb;
    use proxima_storage_pg::{PgPoolConfig, PgTuning};

    let db = SplitRoleDb::create("proxima_goal_rls", &[]).await?;
    let platform = proxima_storage_pg::PgMigrationStorage::connect_with_config(
        db.platform_url(),
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await?;
    platform.run_migrations().await?;
    let admin = sqlx::PgPool::connect(&db.admin_url()).await?;
    let owners = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
    let mut handles = Vec::new();
    let mut versions = Vec::new();
    for owner in owners {
        let handle = Uuid::now_v7();
        let t = Uuid::now_v7();
        let mut tx = admin.begin().await?;
        sqlx::query("INSERT INTO proxima_core.owners(owner_id,kind) VALUES ($1,'group')")
            .bind(owner)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO proxima_core.goal_head(handle,t,owner_id,schema_id) VALUES ($1,$2,$3,'core/task-v1')")
            .bind(handle).bind(t).bind(owner).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO proxima_core.goal(handle,t,owner_id,title,state,request_id, \"by\", via) VALUES ($1,$2,$3,'RLS goal','Active',$4, '00000000-0000-0000-0000-000000000509'::uuid, NULL)")
            .bind(handle).bind(t).bind(owner).bind(t.to_string()).execute(&mut *tx).await?;
        tx.commit().await?;
        handles.push(handle);
        versions.push(t);
    }
    let mut facts = Vec::new();
    for (source, target) in [
        (owners[0], versions[0]),
        (owners[1], versions[1]),
        (owners[2], versions[0]),
        (owners[0], versions[2]),
    ] {
        let t = Uuid::now_v7();
        let mut tx = admin.begin().await?;
        sqlx::query("INSERT INTO proxima_core.memory_head(handle,t,owner_id,schema_id,kind) VALUES ($1,$1,$2,$3,'fact')")
            .bind(t).bind(source).bind(FACT_SCHEMA).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO proxima_core.memory(handle,t,owner_id,schema_id,kind,goal_refs,occurred_at, \"by\", via) VALUES ($1,$1,$2,$3,'fact',ARRAY[$4]::uuid[],uuid_extract_timestamp($1), '00000000-0000-0000-0000-000000000509'::uuid, NULL)")
            .bind(t).bind(source).bind(FACT_SCHEMA).bind(target).execute(&mut *tx).await?;
        tx.commit().await?;
        facts.push(MemoryId::new(t));
    }
    let owner_ids = owners.map(|id| OwnerId::from(GroupId::new(id)));
    let authz = proxima_core::authenticate(
        &Verifier(OwnerRoles::for_subject(
            UserId::new(Uuid::now_v7()),
            [
                (owner_ids[0], Role::viewer()),
                (owner_ids[1], Role::ingest()),
            ],
        )?),
        &Credentials::Bearer("goal-rls-fixture".into()),
    )
    .await?;
    let runtime = PgStorage::connect_for_tests(db.runtime_url().to_owned()).await?;
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Goal);
    req.goal_handles = handles.clone();
    let goals = runtime
        .query_memories(authz.owner_scope(), &owner_ids, &req, &[])
        .await?;
    assert_eq!(
        goals.goals.iter().map(|g| g.handle).collect::<Vec<_>>(),
        [handles[0]]
    );

    req.entity_kind = Some(EntityKind::Fact);
    req.goal_handles.clear();
    req.referenced_goal_handles = handles.clone();
    for supersession in [
        SupersessionStatus::HeadsOnly,
        SupersessionStatus::IncludeSuperseded,
    ] {
        req.supersession = supersession;
        let visible = runtime
            .query_memories(authz.owner_scope(), &owner_ids, &req, &schemas())
            .await?;
        assert_eq!(
            visible.memories.iter().map(|m| m.id).collect::<Vec<_>>(),
            [facts[0]]
        );
        for handle in [handles[1], handles[2], Uuid::now_v7()] {
            req.referenced_goal_handles = vec![handle];
            assert!(
                runtime
                    .query_memories(authz.owner_scope(), &owner_ids, &req, &schemas())
                    .await?
                    .memories
                    .is_empty()
            );
        }
        req.referenced_goal_handles = handles.clone();
    }
    runtime.pool_for_tests().close().await;
    platform.clone_pool_for_backend().close().await;
    admin.close().await;
    Ok(())
}
