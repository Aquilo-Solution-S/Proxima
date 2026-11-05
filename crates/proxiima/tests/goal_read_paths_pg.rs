//! Goal-route count regression: a published old version counts once while a
//! private successor remains invisible to the audience.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "fixture helpers are shared by this focused test")]
mod example_engine;

use example_engine::ExampleEngine;
use proxiima::spiice::{SidecarAtom, SidecarHeadsScope, sidecar_heads_by_key};
use proxiima_core::engine::{GetMemoriesReadRequest, GetMemoryReadRequest, SearchReadRequest};
use proxiima_core::test_fixtures::authenticated_context;
use proxiima_core::verbs::goal_write::{
    GoalAuthorship, GoalCreateRequest, GoalTopologyWrite, IdempotencyKey,
};
use proxiima_core::verbs::query::{
    EntityKind, MemorySearchRequest, MemorySearchScope, QueryRequest, SearchMode, SearchOrder,
    SupersessionStatus, TagMatch,
};
use proxiima_core::{
    AuthPath, AuthzContext, DerivedMemory, FactPayload, FactWrite, GoalAudienceGrantInput,
    GoalHandle, GoalPublicationInput, InterpretationSubjectKind, InterpretationV1, MemoryId,
    MemoryTarget, PublicationUse, Role, SeriesHandle, SimpleTextGoalV1, UserId,
};
use proxiima_example::NoteV1;
use proxiima_example::StampV1;
use uuid::Uuid;

fn auth(user: UserId) -> AuthzContext {
    authenticated_context(AuthzContext::for_subject(user, AuthPath::HostBearer))
}

fn perspective(text: &str, fact: MemoryId, note: MemoryId) -> InterpretationV1 {
    InterpretationV1 {
        claim: text.to_owned(),
        confidence: 90,
        subject_memory_ids: vec![fact.into_inner(), note.into_inner()],
        subject_kinds: vec![
            InterpretationSubjectKind::Fact,
            InterpretationSubjectKind::Fact,
        ],
        model_id: "test".into(),
        client_name: "test".into(),
        client_version: "1".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "read-route acceptance keeps latest-series and audience visibility evidence together"
)]
async fn goal_route_counts_latest_visible_series_once() {
    let host = ExampleEngine::new("proxiima_goal_read_paths", |_| {}).await;
    let owner = UserId::new(Uuid::now_v7());
    let guest = UserId::new(Uuid::now_v7());
    let owner_id = owner.into();
    let owner_auth = auth(owner);
    let guest_auth = auth(guest);
    let notebook = host.notebook(&owner_id).await;
    let mut unit = host.engine.unit_of_work(&owner_auth).await.expect("unit");
    let fact = unit
        .ingest_fact(FactWrite::new(
            owner_id,
            "goal-read-paths",
            &NoteV1 {
                notebook_id: notebook,
                key: "grounding".into(),
                body: "private grounding".into(),
                written_at: time::OffsetDateTime::now_utc(),
            },
        ))
        .await
        .expect("fact")
        .memory_id;
    let stamp = unit
        .ingest_fact(FactWrite::new(
            owner_id,
            StampV1::SCHEMA_ID,
            &StampV1 {
                mark: "published stamp".into(),
            },
        ))
        .await
        .expect("published stamp")
        .memory_id;
    let hidden_stamp = unit
        .ingest_fact(FactWrite::new(
            owner_id,
            StampV1::SCHEMA_ID,
            &StampV1 {
                mark: "hidden stamp".into(),
            },
        ))
        .await
        .expect("hidden stamp")
        .memory_id;
    let published_note = unit
        .ingest_fact(FactWrite::new(
            owner_id,
            NoteV1::SCHEMA_ID,
            &NoteV1 {
                notebook_id: notebook,
                key: "published".into(),
                body: "published note".into(),
                written_at: time::OffsetDateTime::now_utc(),
            },
        ))
        .await
        .expect("published note")
        .memory_id;
    let hidden_note = unit
        .ingest_fact(FactWrite::new(
            owner_id,
            NoteV1::SCHEMA_ID,
            &NoteV1 {
                notebook_id: notebook,
                key: "hidden".into(),
                body: "hidden note".into(),
                written_at: time::OffsetDateTime::now_utc(),
            },
        ))
        .await
        .expect("hidden note")
        .memory_id;
    let first = unit
        .derive_memory(DerivedMemory::interpretation(
            MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
            owner_id,
            "published",
            perspective("published", fact, published_note),
        ))
        .await
        .expect("perspective")
        .memory_id;
    let goal = unit
        .create_goal(GoalCreateRequest {
            owner: owner_id,
            topology: GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new()).expect("topology"),
            title: "Customer".into(),
            text: "Customer audience".into(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new("goal-read-paths").expect("key"),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .expect("goal");
    unit.commit().await.expect("commit");
    let handle: Uuid = sqlx::query_scalar("SELECT handle FROM proxiima_core.goal WHERE t=$1")
        .bind(goal.goal_id.into_inner())
        .fetch_one(&host.pool)
        .await
        .expect("goal handle");
    let goal = GoalHandle::new(handle);
    host.engine
        .publish_to_goal(
            &owner_auth,
            &GoalPublicationInput {
                goal,
                memory: first,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("publish");
    host.engine
        .publish_to_goal(
            &owner_auth,
            &GoalPublicationInput {
                goal,
                memory: stamp,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("publish stamp");
    host.engine
        .publish_to_goal(
            &owner_auth,
            &GoalPublicationInput {
                goal,
                memory: published_note,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("publish note");
    let private = host
        .engine
        .derive_memory(
            &owner_auth,
            DerivedMemory::interpretation(
                MemoryTarget::Revision(first),
                owner_id,
                "private successor",
                perspective("private successor", fact, published_note),
            ),
        )
        .await
        .expect("private successor");
    host.engine
        .grant_goal_access(
            &owner_auth,
            &GoalAudienceGrantInput {
                goal,
                subject: guest,
                role: Role::viewer(),
                expires_at: None,
            },
        )
        .await
        .expect("grant");
    let head = host
        .engine
        .get_memory(
            &guest_auth,
            &GetMemoryReadRequest {
                memory_id: first,
                include_neighbor_edges: false,
                at: proxiima::host::ReadAt::SeriesHead,
            },
        )
        .await
        .expect("published visible head");
    assert_eq!(head.memory.map(|memory| memory.memory_id), Some(first));
    let note_neighbors = host
        .engine
        .get_memory(
            &guest_auth,
            &GetMemoryReadRequest {
                memory_id: published_note,
                include_neighbor_edges: true,
                at: proxiima::host::ReadAt::Version,
            },
        )
        .await
        .expect("published Note neighbors");
    assert!(
        note_neighbors
            .neighbor_edges
            .iter()
            .any(|edge| edge.source.entity == proxiima_core::EntityRef::Memory(first))
    );
    assert!(
        note_neighbors
            .neighbor_edges
            .iter()
            .all(|edge| edge.source.entity != proxiima_core::EntityRef::Memory(private.memory_id))
    );
    host.engine
        .publish_to_goal(
            &owner_auth,
            &GoalPublicationInput {
                goal,
                memory: private.memory_id,
                permitted_use: PublicationUse::Read,
            },
        )
        .await
        .expect("publish private successor");
    let search = host
        .engine
        .search(
            &guest_auth,
            &SearchReadRequest {
                search: MemorySearchRequest {
                    scope: MemorySearchScope::Readable,
                    read_owners: Vec::new(),
                    query: "private successor".into(),
                    mode: SearchMode::Lexical,
                    supersession: SupersessionStatus::HeadsOnly,
                    limit: 1,
                    kind: Some(EntityKind::Perspective),
                    schema_id: None,
                    tags: Vec::new(),
                    tag_match: TagMatch::default(),
                    since: None,
                    until: None,
                    order: SearchOrder::Relevance,
                    min_score: None,
                    semantic_weight: None,
                    after: None,
                    semantic: None,
                },
                include_body: false,
                include_neighbor_edges: false,
            },
        )
        .await
        .expect("Goal-readable search");
    assert_eq!(search.memories.len(), 1);
    assert_eq!(search.memories[0].memory_id, private.memory_id);
    let owner_search = host
        .engine
        .search(
            &guest_auth,
            &SearchReadRequest {
                search: MemorySearchRequest {
                    scope: MemorySearchScope::Owner(owner_id),
                    read_owners: Vec::new(),
                    query: "published".into(),
                    mode: SearchMode::Lexical,
                    supersession: SupersessionStatus::HeadsOnly,
                    limit: 1,
                    kind: Some(EntityKind::Perspective),
                    schema_id: None,
                    tags: Vec::new(),
                    tag_match: TagMatch::default(),
                    since: None,
                    until: None,
                    order: SearchOrder::Relevance,
                    min_score: None,
                    semantic_weight: None,
                    after: None,
                    semantic: None,
                },
                include_body: false,
                include_neighbor_edges: false,
            },
        )
        .await;
    assert!(
        owner_search.is_err(),
        "explicit Owner scope must remain owner-only"
    );
    let counts = host
        .engine
        .memory_counts(&guest_auth)
        .await
        .expect("counts");
    assert_eq!(counts.perspective, 1, "published series counts once");
    assert_eq!(counts.fact, 2, "only the published Facts are counted");
    let mut request = QueryRequest::readable();
    request.entity_kind = Some(EntityKind::Perspective);
    let rows = host
        .engine
        .query(&guest_auth, &request)
        .await
        .expect("query");
    assert_eq!(rows.memories.len(), 1);
    assert_eq!(rows.memories[0].id, private.memory_id);
    let exact = host
        .engine
        .get_memory(
            &guest_auth,
            &GetMemoryReadRequest {
                memory_id: first,
                include_neighbor_edges: false,
                at: proxiima::host::ReadAt::Version,
            },
        )
        .await
        .expect("published exact version read");
    assert_eq!(exact.memory.map(|memory| memory.memory_id), Some(first));
    let batch = host
        .engine
        .get_memories(
            &guest_auth,
            &GetMemoriesReadRequest {
                memory_ids: vec![
                    first,
                    private.memory_id,
                    fact,
                    stamp,
                    hidden_stamp,
                    published_note,
                    hidden_note,
                ],
            },
        )
        .await
        .expect("published batch read");
    assert_eq!(
        batch
            .memories
            .iter()
            .map(|memory| memory.memory_id)
            .collect::<std::collections::BTreeSet<_>>(),
        [first, private.memory_id, stamp, published_note]
            .into_iter()
            .collect(),
        "batch returns the four published admissions and omits hidden sources"
    );
    let sketches = host
        .engine
        .load_sketches(&guest_auth, &[first, private.memory_id, fact])
        .await
        .expect("Goal sketches");
    assert_eq!(
        sketches
            .iter()
            .map(|row| row.id)
            .collect::<std::collections::BTreeSet<_>>(),
        [first, private.memory_id].into_iter().collect()
    );
    let nodes = host
        .engine
        .lineage_nodes(&guest_auth, &[first, private.memory_id, fact])
        .await
        .expect("Goal lineage nodes");
    assert_eq!(
        nodes
            .iter()
            .map(|row| row.memory_id)
            .collect::<std::collections::BTreeSet<_>>(),
        [first, private.memory_id].into_iter().collect()
    );
    request.supersession = SupersessionStatus::IncludeSuperseded;
    let history = host
        .engine
        .query(&guest_auth, &request)
        .await
        .expect("published history");
    assert_eq!(history.memories.len(), 2);
    assert!(
        history
            .memories
            .iter()
            .all(|memory| [first, private.memory_id].contains(&memory.id))
    );
    let stamp_read = host
        .engine
        .get_memory(
            &guest_auth,
            &GetMemoryReadRequest {
                memory_id: stamp,
                include_neighbor_edges: false,
                at: proxiima::host::ReadAt::Version,
            },
        )
        .await
        .expect("published stamp read");
    assert_eq!(
        stamp_read.memory.as_ref().map(|memory| memory.memory_id),
        Some(stamp)
    );
    assert!(
        stamp_read
            .memory
            .as_ref()
            .and_then(|memory| memory.payload.as_ref())
            .and_then(|payload| payload.downcast_ref::<StampV1>())
            .is_some()
    );
    let hidden_read = host
        .engine
        .get_memory(
            &guest_auth,
            &GetMemoryReadRequest {
                memory_id: hidden_stamp,
                include_neighbor_edges: false,
                at: proxiima::host::ReadAt::Version,
            },
        )
        .await;
    assert!(hidden_read.is_err(), "unpublished Stamp must remain hidden");
    let note_heads = sidecar_heads_by_key::<NoteV1>(
        &host.engine,
        &guest_auth,
        SidecarHeadsScope::Readable,
        &[],
        None,
        100,
    )
    .await
    .expect("published note sidecar head");
    assert_eq!(
        note_heads
            .heads
            .iter()
            .map(|head| head.memory_id)
            .collect::<Vec<_>>(),
        vec![published_note]
    );
    assert_eq!(
        note_heads.heads[0].natural_key[0].1,
        SidecarAtom::Uuid(notebook)
    );
    assert_eq!(
        note_heads.heads[0].natural_key[1].1,
        SidecarAtom::Text("published".into())
    );
    assert!(
        !note_heads
            .heads
            .iter()
            .any(|head| head.memory_id == hidden_note)
    );
    host.pool.close().await;
}
