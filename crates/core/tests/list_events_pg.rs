//! End-to-end MCP change-event list tool against transient PG storage.

use std::{future::Future, pin::Pin, sync::Arc};

mod common;

use common::{ConstantEmbedding, drop_db, fresh_pg, owner_fixture};
use proxima_core::engine::Engine;
use proxima_core::mcp::core_tools::list_events::{ListEventsArgs, ListEventsOutput, list_events};
use proxima_core::mcp::core_tools::memory::derive::{DeriveArgs, DerivedKind};
use proxima_core::mcp::core_tools::memory::link::LinkArgs;
use proxima_core::mcp::core_tools::memory::remember::RememberArgs;
use proxima_core::mcp::core_tools::{DeriveTool, LinkTool, RememberTool};
use proxima_core::mcp::{HandleTable, McpAuthorContext, McpToolCtx, McpToolExtensions, OutputMode};
use proxima_core::{
    AuthPath, AuthzContext, FlavorRegistry, FlavorRegistryFrozen, McpTool, McpToolError, Owner,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type BoxTestFuture<'a> = Pin<Box<dyn Future<Output = TestResult> + 'a>>;

#[tokio::test]
async fn list_events_returns_entity_and_edge_events_in_seq_order() -> TestResult {
    with_harness(|harness| {
        Box::pin(async move {
            let produced = harness.seed_abstraction_fact_edge().await?;
            let page = harness
                .list_events(ListEventsArgs {
                    since: None,
                    limit: Some(1000),
                })
                .await?;

            assert!(
                page.events.len() >= 3,
                "expected at least two entity events and one edge event, got {:?}",
                page.events
            );
            assert!(!page.has_more);
            assert_eq!(
                page.next_since,
                page.events.last().map(|event| event.seq.clone())
            );

            let seqs = page
                .events
                .iter()
                .map(|event| {
                    uuid::Uuid::parse_str(&event.seq).expect("seq must be a parseable UUID")
                })
                .collect::<Vec<_>>();
            assert!(
                seqs.windows(2).all(|pair| pair[0] < pair[1]),
                "events must be strictly seq-ascending: {seqs:?}"
            );

            let entity = page
                .events
                .iter()
                .find(|event| {
                    event.kind == "entity_append" && event.entity_kind.as_deref() == Some("Fact")
                })
                .expect("fact entity_append event");
            assert_non_empty_handle(entity.entity.as_deref(), 'F');
            assert_eq!(entity.entity_kind.as_deref(), Some("Fact"));
            assert!(entity.schema_id.as_deref().is_some_and(|id| !id.is_empty()));
            assert_eq!(entity.schema_version, Some(1));

            // Derivation emits provenance edges too; match the explicit link
            // edge by its handle. Its source is the Abstraction, target the Fact.
            let edge = page
                .events
                .iter()
                .find(|event| {
                    event.kind == "edge_append"
                        && event.edge.as_deref() == Some(produced.edge_handle.as_str())
                })
                .expect("explicit link edge_append event");
            assert_non_empty_handle(edge.edge.as_deref(), 'E');
            assert_non_empty_handle(edge.source.as_deref(), 'A');
            assert_non_empty_handle(edge.target.as_deref(), 'F');
            assert!(edge.relation.as_deref().is_some_and(|id| !id.is_empty()));
            assert_eq!(edge.source.as_deref(), Some(produced.source.as_str()));
            assert_eq!(edge.target.as_deref(), Some(produced.target.as_str()));
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn list_events_pages_with_strict_cursor_and_empty_tail() -> TestResult {
    with_harness(|harness| {
        Box::pin(async move {
            harness.seed_abstraction_fact_edge().await?;

            let first = harness
                .list_events(ListEventsArgs {
                    since: None,
                    limit: Some(1),
                })
                .await?;
            assert_eq!(first.events.len(), 1);
            assert!(first.has_more);
            let mut seen = vec![first.events[0].seq.clone()];
            let mut cursor = first.next_since.expect("first page next_since");
            assert_eq!(cursor, seen[0]);

            loop {
                let page = harness
                    .list_events(ListEventsArgs {
                        since: Some(cursor.clone()),
                        limit: Some(1),
                    })
                    .await?;
                if page.events.is_empty() {
                    assert!(!page.has_more);
                    assert_eq!(page.next_since, Some(cursor));
                    break;
                }

                assert_eq!(page.events.len(), 1);
                let next = page.events[0].seq.clone();
                assert!(
                    uuid::Uuid::parse_str(&cursor)? < uuid::Uuid::parse_str(&next)?,
                    "cursor must advance strictly"
                );
                assert!(!seen.contains(&next), "page overlapped on seq {next}");
                seen.push(next.clone());
                cursor = page.next_since.expect("non-empty page next_since");
                assert_eq!(cursor, next);
            }

            assert!(
                seen.len() >= 3,
                "expected at least two entity events and one edge event, got {seen:?}"
            );
            Ok(())
        })
    })
    .await
}

async fn with_harness<F>(test: F) -> TestResult
where
    F: for<'a> FnOnce(&'a ToolHarness) -> BoxTestFuture<'a>,
{
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let harness = ToolHarness::new(pg);
        test(&harness).await
    }
    .await;
    drop_db(&db_name).await?;
    result
}

struct ToolHarness {
    pg: proxima_storage_pg::PgStorage,
    owner: Owner,
    handles: Arc<HandleTable>,
    registry: Arc<FlavorRegistryFrozen>,
    author: McpAuthorContext,
    engine: Arc<Engine>,
}

struct ProducedGraph {
    source: String,
    target: String,
    edge_handle: String,
}

impl ToolHarness {
    fn new(pg: proxima_storage_pg::PgStorage) -> Self {
        let owner = owner_fixture();
        let registry = Arc::new(FlavorRegistry::new().freeze());
        let handles = Arc::new(HandleTable::new());
        let author = author_ctx();
        let engine = Arc::new(
            Engine::new((*registry).clone())
                .with_storage_ports(Arc::new(pg.clone()).storage_ports())
                .with_embed(Arc::new(ConstantEmbedding::prefixed(
                    "test-embed",
                    &[1.0, 2.0, 3.0],
                ))),
        );
        Self {
            pg,
            owner,
            handles,
            registry,
            author,
            engine,
        }
    }

    async fn call<T: McpTool>(&self, args: T::Args) -> Result<T::Output, McpToolError> {
        T::call(self.ctx(), args).await
    }

    async fn list_events(&self, args: ListEventsArgs) -> Result<ListEventsOutput, McpToolError> {
        list_events(self.ctx(), args).await
    }

    async fn seed_abstraction_fact_edge(&self) -> Result<ProducedGraph, McpToolError> {
        // `core/agent-link-refers-to` requires an Abstraction/Perspective
        // source and a memory target, so derive an Abstraction from a Fact
        // and link it back to that Fact.
        let fact = self
            .call::<RememberTool>(remember_args(
                "List events target",
                "Target fact for the list-events edge.",
                "list-events-target",
            ))
            .await?;
        let abstraction = self
            .call::<DeriveTool>(DeriveArgs {
                kind: DerivedKind::Abstraction,
                title: "List events source".into(),
                body: "Abstraction that refers to the target fact.".into(),
                tags: Vec::new(),
                source_handles: vec![fact.handle.clone()],
                model_id: "codex-test".into(),
                idempotency_key: Some("list-events-source".into()),
                space: None,
            })
            .await?;
        let edge = self
            .call::<LinkTool>(LinkArgs {
                source: abstraction.handle.clone(),
                target: fact.handle.clone(),
                reason: "The abstraction refers to the target fact.".into(),
                confidence: 80,
                space: None,
            })
            .await?;
        Ok(ProducedGraph {
            source: abstraction.handle,
            target: fact.handle,
            edge_handle: edge.edge_handle,
        })
    }

    fn ctx(&self) -> McpToolCtx {
        McpToolCtx {
            owner: self.owner,
            authz: AuthzContext::single_owner(&self.owner, AuthPath::System),
            handles: Some(self.handles.clone()),
            mode: OutputMode::Handles,
            registry: self.registry.clone(),
            author: self.author.clone(),
            caller_self_perspective: None,
            master_token_id: None,
            extensions: McpToolExtensions::with(self.pg.pool().clone()),
            engine: Some(self.engine.clone()),
        }
    }
}

fn remember_args(title: &str, body: &str, idempotency_key: &str) -> RememberArgs {
    RememberArgs {
        title: title.into(),
        body: body.into(),
        tags: Vec::new(),
        idempotency_key: Some(idempotency_key.into()),
        citation: None,
        space: None,
    }
}

fn author_ctx() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "codex-test".into(),
        client_name: "codex".into(),
        client_version: "1".into(),
        personality_instance_id: None,
        caller_self_perspective: None,
    }
}

fn assert_non_empty_handle(value: Option<&str>, prefix: char) {
    let handle = value.expect("expected handle");
    assert!(
        handle.starts_with(prefix) && handle.len() > 1,
        "expected {prefix} handle, got {handle}"
    );
}
