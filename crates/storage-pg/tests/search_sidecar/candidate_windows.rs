//! Candidate windows must honor the requested kind/head set and cursor depth.
//! Memory rows use ordinary authorized writes; only embedding vectors are
//! deterministic read fixtures, independent of an external model provider.

use std::sync::Arc;

use proxima_core::verbs::query::{MemorySearchPage, SearchCursor};
use proxima_core::{
    AgentDerivationV1, AgentNoteV1, AuthPath, AuthzContext, DerivationIdentity, DerivedMemory,
    Engine, FactWriteCommand, FlavorRegistry, InputContractId, MemoryId, MemoryTarget, OperatorId,
    Relation, SearchReadRequest, SeriesHandle, SidecarPayload,
};

use super::{
    EntityKind, MemorySearchRequest, OwnerRef, PgStorage, SchemaId, SearchMode, SearchOrder,
    SupersessionStatus, UserId, Uuid, create_db, db_url, drop_db, embed_literal, embed_literal_xy,
    search_req, seed_embedding,
};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;
const DERIVATION_SCHEMA: &str = "core/agent-derivation-v1";
const DECOYS: usize = 22;

struct Fixture {
    name: String,
    pg: PgStorage,
    engine: Engine,
    owner: OwnerRef,
    authz: AuthzContext,
}

impl Fixture {
    async fn new() -> TestResult<Self> {
        let name = format!("proxima_test_{}", Uuid::now_v7().simple());
        create_db(&name).await?;
        let pg = PgStorage::connect(&db_url(&name)).await?;
        pg.run_migrations().await?;
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());
        let user = UserId::new(Uuid::now_v7());
        Ok(Self {
            name,
            pg,
            engine,
            owner: OwnerRef::Personal(user),
            authz: AuthzContext::for_subject(user, AuthPath::HostBearer),
        })
    }

    async fn close(self) {
        self.pg.pool_for_tests().close().await;
        drop_db(&self.name).await.expect("drop isolated search DB");
    }

    async fn note(&self) -> TestResult<MemoryId> {
        let payload = AgentNoteV1 {
            note_id: Uuid::now_v7(),
            title: "candidate window needle".into(),
            body: "same lexical needle for every distinct admission".into(),
            tags: Vec::new(),
            idempotency_key: None,
        };
        let mut draft = FactWriteCommand::from_payload(
            "test/search-candidate-window",
            &payload,
            time::OffsetDateTime::now_utc(),
        );
        draft.lexical_language =
            Some(proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT.to_owned());
        let sidecars = [SidecarPayload::fact(payload)];
        let authorized = self
            .engine
            .authorize_fact_ingest(&self.authz, Relation::Ingest, draft, &sidecars)
            .await?;
        Ok(self
            .engine
            .ingest_fact_with_typed_sidecar(&authorized, &sidecars, None)
            .await?
            .memory_id)
    }

    async fn derive(
        &self,
        origin: MemoryId,
        kind: EntityKind,
        supersedes: Option<MemoryId>,
    ) -> TestResult<MemoryId> {
        let payload = AgentDerivationV1 {
            title: "candidate window derivation".into(),
            body: "ordinary typed grounding for a search fixture".into(),
            tags: Vec::new(),
            idempotency_key: None,
            source_memory_ids: vec![origin.into_inner()],
            model_id: "test-model".into(),
            client_name: "search-candidate-test".into(),
            client_version: "1".into(),
        };
        let target = match supersedes {
            Some(prior) => MemoryTarget::Revision(prior),
            None => MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
        };
        let text = "ordinary typed grounding for a search fixture";
        let identity = DerivationIdentity::new(
            OperatorId::new(Uuid::now_v7()),
            InputContractId::new(Uuid::now_v7()),
        );
        let memory = match kind {
            EntityKind::Abstraction => {
                DerivedMemory::abstraction(target, self.owner, text, payload, [origin], identity)?
            }
            EntityKind::Perspective => {
                DerivedMemory::perspective(target, self.owner, text, payload, [origin], identity)?
            }
            _ => unreachable!("only ordinary A/P derivations"),
        };
        Ok(self
            .engine
            .derive_memory(&self.authz, memory)
            .await?
            .memory_id)
    }

    async fn vector(&self, id: MemoryId, closer: bool) -> TestResult<()> {
        let vector = if closer {
            embed_literal()
        } else {
            embed_literal_xy("0.8", "0.6")
        };
        seed_embedding(
            self.pg.pool_for_tests(),
            self.owner,
            id.into_inner(),
            &vector,
        )
        .await?;
        Ok(())
    }

    async fn search(&self, request: MemorySearchRequest) -> TestResult<MemorySearchPage> {
        let response = self
            .engine
            .search(
                &self.authz,
                &SearchReadRequest {
                    search: request,
                    include_body: false,
                    include_neighbor_edges: false,
                },
            )
            .await?;
        Ok(MemorySearchPage {
            results: response.memories,
            has_more: response.has_more,
        })
    }

    fn semantic_request(&self, kind: EntityKind, mode: SearchMode) -> MemorySearchRequest {
        let mut request = search_req(self.owner, "unmatchedsearchtoken");
        request.kind = Some(kind);
        request.schema_id = Some(SchemaId::new(DERIVATION_SCHEMA.into()));
        request.mode = mode;
        request.limit = 1;
        request.semantic_weight = matches!(mode, SearchMode::Hybrid).then_some(1.0);
        request.embedding_model_id = Some("test-embed".into());
        let mut query = vec![0.0; 1024];
        query[0] = 1.0;
        request.query_embedding = Some(query);
        request
    }
}

fn ids(page: &MemorySearchPage) -> Vec<MemoryId> {
    page.results.iter().map(|row| row.memory_id).collect()
}

fn relevance_cursor(page: &MemorySearchPage, seen: u32) -> SearchCursor {
    let last = page.results.last().expect("nonempty previous page");
    SearchCursor::Relevance {
        score_bits: last.score.to_bits(),
        memory_id: last.memory_id,
        seen,
    }
}

async fn one_at_a_time(fixture: &Fixture) -> TestResult<Vec<MemorySearchPage>> {
    let mut request = search_req(fixture.owner, "needle");
    request.schema_id = Some(SchemaId::new("core/agent-note-v1".into()));
    request.limit = 1;
    let mut pages = Vec::new();
    // Bounded so a broken non-advancing cursor cannot hang the regression.
    for seen in 1..=23 {
        let page = fixture.search(request.clone()).await?;
        let done = !page.has_more || page.results.is_empty();
        if !done {
            request.after = Some(relevance_cursor(&page, seen));
        }
        pages.push(page);
        if done {
            break;
        }
    }
    Ok(pages)
}

#[tokio::test]
async fn semantic_kind_filter_precedes_the_candidate_limit() {
    let fixture = Fixture::new().await.expect("isolated fixture");
    let result: TestResult<_> = async {
        let anchor = fixture.note().await?;
        let mut abstractions = Vec::new();
        for _ in 0..DECOYS {
            let id = fixture
                .derive(anchor, EntityKind::Abstraction, None)
                .await?;
            fixture.vector(id, true).await?;
            abstractions.push(id);
        }
        let perspective = fixture
            .derive(abstractions[0], EntityKind::Perspective, None)
            .await?;
        fixture.vector(perspective, false).await?;
        let mut observations = Vec::new();
        for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
            let request = fixture.semantic_request(EntityKind::Perspective, mode);
            let small = fixture.search(request.clone()).await?;
            let mut wide = request;
            wide.limit = 8;
            let control = fixture.search(wide).await?;
            observations.push((mode, small, control));
        }
        let all_kinds = fixture
            .search(fixture.semantic_request(EntityKind::Abstraction, SearchMode::Semantic))
            .await?;
        Ok((perspective, abstractions, observations, all_kinds))
    }
    .await;
    fixture.close().await;
    let (perspective, abstractions, observations, all_kinds) = result.expect("typed kind probe");
    assert_eq!(all_kinds.results.len(), 1);
    assert!(abstractions.contains(&all_kinds.results[0].memory_id));
    for (mode, small, control) in observations {
        eprintln!("kind mode={mode:?} target={perspective:?} small={small:?} wide={control:?}");
        assert_eq!(ids(&control), vec![perspective], "wider control finds P");
        assert_eq!(control.results[0].kind, EntityKind::Perspective);
        assert!(!control.has_more);
        assert_eq!(ids(&small), vec![perspective], "{mode:?} kind before LIMIT");
        assert_eq!(small.results, control.results);
        assert!(!small.has_more);
    }
}

#[tokio::test]
async fn semantic_head_filter_precedes_the_candidate_limit() {
    let fixture = Fixture::new().await.expect("isolated fixture");
    let result: TestResult<_> = async {
        let anchor = fixture.note().await?;
        let mut old = Vec::new();
        let mut heads = Vec::new();
        for _ in 0..DECOYS {
            let prior = fixture
                .derive(anchor, EntityKind::Abstraction, None)
                .await?;
            fixture.vector(prior, true).await?;
            let head = fixture
                .derive(anchor, EntityKind::Abstraction, Some(prior))
                .await?;
            // A unique best current head avoids depending on a distance tie
            // at the small semantic candidate window's boundary.
            if heads.is_empty() {
                seed_embedding(
                    fixture.pg.pool_for_tests(),
                    fixture.owner,
                    head.into_inner(),
                    &embed_literal_xy("0.9", "0.435889894"),
                )
                .await?;
            } else {
                fixture.vector(head, false).await?;
            }
            old.push(prior);
            heads.push(head);
        }
        let mut observations = Vec::new();
        for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
            let request = fixture.semantic_request(EntityKind::Abstraction, mode);
            let small = fixture.search(request.clone()).await?;
            let mut wide = request.clone();
            wide.limit = 8;
            let control = fixture.search(wide).await?;
            let mut historical = request;
            historical.supersession = SupersessionStatus::IncludeSuperseded;
            let historical = fixture.search(historical).await?;
            observations.push((mode, small, control, historical));
        }
        Ok((old, heads, observations))
    }
    .await;
    fixture.close().await;
    let (old, heads, observations) = result.expect("typed supersession probe");
    for (mode, small, control, historical) in observations {
        eprintln!("head mode={mode:?} small={small:?} wide={control:?} history={historical:?}");
        assert_eq!(control.results.len(), 8);
        assert!(ids(&control).iter().all(|id| heads.contains(id)));
        assert_eq!(control.results[0].memory_id, heads[0]);
        assert!(control.has_more);
        assert_eq!(historical.results.len(), 1);
        assert!(old.contains(&historical.results[0].memory_id));
        assert!(historical.has_more);
        assert_eq!(ids(&small), vec![heads[0]], "{mode:?} heads before LIMIT");
        assert_eq!(small.results[0], control.results[0]);
        assert!(small.has_more);
    }
}

#[tokio::test]
async fn relevance_cursor_survives_a_smaller_page_limit() {
    let fixture = Fixture::new().await.expect("isolated fixture");
    let result: TestResult<_> = async {
        let mut admitted = Vec::new();
        for _ in 0..22 {
            admitted.push(fixture.note().await?);
        }
        let mut request = search_req(fixture.owner, "needle");
        request.schema_id = Some(SchemaId::new("core/agent-note-v1".into()));
        request.limit = 21;
        let first = fixture.search(request.clone()).await?;
        request.after = Some(relevance_cursor(&first, 21));
        request.limit = 1;
        let small = fixture.search(request.clone()).await?;
        request.limit = 2;
        let control = fixture.search(request.clone()).await?;
        request.order = SearchOrder::Recency;
        request.after = None;
        request.limit = 21;
        let recent_first = fixture.search(request.clone()).await?;
        let last = recent_first.results.last().expect("nonempty recency page");
        request.after = Some(SearchCursor::Recency {
            created_at: last.created_at,
            memory_id: last.memory_id,
            seen: 21,
        });
        request.limit = 1;
        let recent_last = fixture.search(request).await?;
        let singles = one_at_a_time(&fixture).await?;
        Ok((
            admitted,
            first,
            small,
            control,
            recent_first,
            recent_last,
            singles,
        ))
    }
    .await;
    fixture.close().await;
    let (mut admitted, first, small, control, recent_first, recent_last, singles) =
        result.expect("real lexical pagination probe");
    admitted.sort_by_key(|id| std::cmp::Reverse(id.into_inner()));
    eprintln!("paging admitted={admitted:?} first={first:?} small={small:?} control={control:?}");
    assert_eq!(ids(&first), admitted[..21]);
    assert!(first.has_more);
    assert_eq!(ids(&control), admitted[21..]);
    assert!(!control.has_more);
    assert_eq!(ids(&recent_first), admitted[..21]);
    assert!(recent_first.has_more);
    assert_eq!(ids(&recent_last), admitted[21..]);
    assert!(!recent_last.has_more);
    assert_eq!(
        singles.len(),
        admitted.len(),
        "constant limit=1 traverses every row"
    );
    for (index, page) in singles.iter().enumerate() {
        assert_eq!(ids(page), vec![admitted[index]]);
        assert_eq!(
            page.has_more,
            index + 1 < admitted.len(),
            "page {index} lookahead"
        );
    }
    assert_eq!(
        ids(&small),
        admitted[21..],
        "shrinking limit must retain the next row"
    );
    assert_eq!(small.results, control.results);
    assert!(!small.has_more);
}
