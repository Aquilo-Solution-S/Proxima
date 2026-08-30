//! Pins on the node, PK outbound, GIN inbound, lineage by t.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::read_models::MemorySchemaSpec;
use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::{InboundPinQuery, MemoryReadPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::{AuthorizedFactWrite, FactWriteCommand};
use proxima_core::verbs::query::{
    EntityKind, MemoryLineageCursor, MemoryLineageDirection, MemoryLineageRequest, QueryRequest,
};
use proxima_core::{
    AbstractionPayload, AccessKind, AgentDerivationV1, EdgeKind, EdgeTargetProjection, EntityRef,
    OwnerRef, SchemaId, SchemaVersion, SidecarPayload, UserId, project_listed_edge,
    project_window_edges,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn memory_schema_specs(registry: &proxima_core::FlavorRegistryFrozen) -> Vec<MemorySchemaSpec> {
    registry
        .schemas()
        .iter()
        .filter_map(|schema| {
            let kind = match schema.kind {
                proxima_core::verbs::schema::PayloadKind::Fact => EntityKind::Fact,
                proxima_core::verbs::schema::PayloadKind::Abstraction => EntityKind::Abstraction,
                proxima_core::verbs::schema::PayloadKind::Perspective => EntityKind::Perspective,
                _ => return None,
            };
            Some(MemorySchemaSpec {
                kind,
                schema_id: schema.schema_id.clone(),
                schema_version: schema.schema_version,
                sidecar_table: schema.sidecar_table.clone(),
            })
        })
        .collect()
}

fn draft(kind: &str, refs: Vec<Uuid>, origins: Vec<Uuid>) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("core/upload-v1".to_owned()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: origins
            .into_iter()
            .map(|t| {
                proxima_core::EdgeEndpoint::memory(EntityKind::Fact, proxima_core::MemoryId::new(t))
            })
            .collect(),
        refs,
        blob_id: None,
        kind: kind.into(),
    }
}

#[tokio::test]
async fn query_neighbors_edges_and_lineage_use_pins() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let specs = memory_schema_specs(&registry);
        let pg = PgStorage::connect(&url).await?.with_flavors(&registry);
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);

        let leaf = pg
            .ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let mut derived_cmd = draft("abstraction", vec![], vec![leaf.memory_id.into_inner()]);
        derived_cmd.schema_id = SchemaId::new(AgentDerivationV1::SCHEMA_ID.into());
        // `agent-derivation-v1` is `LanguagePolicy::PerRow`: the write names a
        // language.
        derived_cmd.lexical_language =
            Some(proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT.to_owned());
        // The typed write, not `ingest_fact_atomic` plus a hand-written row:
        // it stamps `sidecar_tables`, without which the derivation row is one no
        // forget, erase or export can reach.
        let authorized = AuthorizedFactWrite::new_for_tests(
            OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
            derived_cmd,
            Some(AgentDerivationV1::sidecar_table().to_owned()),
            Vec::new(),
        );
        let derived = pg
            .ingest_fact_with_typed_sidecar(
                &authorized,
                &[SidecarPayload::abstraction(AgentDerivationV1 {
                    title: "derived title".into(),
                    body: "made from leaf".into(),
                    tags: Vec::new(),
                    idempotency_key: None,
                    source_memory_ids: vec![leaf.memory_id.into_inner()],
                    model_id: "test".into(),
                    client_name: "test".into(),
                    client_version: "1".into(),
                })],
                None,
            )
            .await?;

        let mut q = QueryRequest::for_owner(owner);
        q.include_payloads = false;
        let page = pg.query_memories(&q, &specs).await?;
        let derived_row = page
            .memories
            .iter()
            .find(|row| row.id == derived.memory_id)
            .expect("derived row");
        assert_eq!(derived_row.origins, vec![leaf.memory_id]);
        assert!(
            page.edges.iter().any(|edge| {
                edge.kind == EdgeKind::Origin
                    && edge.source.memory_id() == Some(derived.memory_id)
                    && matches!(
                        &edge.target,
                        EdgeTargetProjection::Visible { target }
                            if target.memory_id() == Some(leaf.memory_id)
                    )
            }),
            "Query snapshot must project the window pin"
        );

        let inbound = pg
            .load_inbound_pin_nodes(
                &[owner],
                InboundPinQuery {
                    targets: &[leaf.memory_id],
                    goal_targets: false,
                    kind: None,
                    heads_only: true,
                    after: None,
                    limit: 50,
                },
            )
            .await?;
        assert!(
            inbound.iter().any(|node| {
                node.id == derived.memory_id && node.origins.contains(&leaf.memory_id)
            }),
            "GIN inbound returns the child row, not a reconstructed edge"
        );

        let outbound = pg.load_pin_nodes(&[owner], &[derived.memory_id]).await?;
        assert_eq!(outbound.len(), 1);
        assert_eq!(outbound[0].origins, vec![leaf.memory_id]);

        let down = pg
            .walk_memory_lineage(
                &[owner],
                &MemoryLineageRequest {
                    owner,
                    start_memory_id: leaf.memory_id,
                    direction: MemoryLineageDirection::Descendants,
                    depth: 4,
                    limit: 20,
                    after: None,
                },
            )
            .await?;
        assert!(
            down.edges.iter().any(|hop| {
                hop.edge.source.memory_id() == Some(derived.memory_id)
                    && hop.edge.created_at != time::OffsetDateTime::UNIX_EPOCH
            }),
            "descendants use origins @> and stamp created_at from t"
        );
        assert!(
            down.nodes
                .iter()
                .any(|n| n.memory_id == derived.memory_id && n.snippet.contains("derived title")),
            "lineage snippet comes from the sidecar"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("graph pin reads failed");
}

#[tokio::test]
async fn pin_node_loads_are_owner_scoped_and_redact_in_memory() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);

        let a = pg
            .ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let b = pg
            .ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let c = pg
            .ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let other_permit = OwnerWritePermit::new_for_tests(other, AccessKind::Fact);
        let foreign = pg
            .ingest_fact_atomic(&other_permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let hub = pg
            .ingest_fact_atomic(
                &permit,
                &draft(
                    "abstraction",
                    vec![foreign.memory_id.into_inner()],
                    vec![
                        a.memory_id.into_inner(),
                        b.memory_id.into_inner(),
                        c.memory_id.into_inner(),
                    ],
                ),
                None,
            )
            .await?;

        let hubs = pg.load_pin_nodes(&[owner], &[hub.memory_id]).await?;
        assert_eq!(hubs.len(), 1);
        let hub_node = &hubs[0];
        assert_eq!(hub_node.origins.len(), 3);
        assert_eq!(hub_node.refs, vec![foreign.memory_id]);

        let mut pin_ids = hub_node.origins.clone();
        pin_ids.extend(hub_node.refs.iter().copied());
        let visible_nodes = pg.load_pin_nodes(&[owner], &pin_ids).await?;
        assert_eq!(visible_nodes.len(), 3, "foreign pin is not an owned node");
        let visible: std::collections::HashMap<_, _> = visible_nodes
            .iter()
            .map(|node| (node.id, node.kind))
            .collect();

        let window = project_window_edges(&hubs, 50);
        assert_eq!(window.len(), 0, "targets are not in the hub-only window");

        let mut listed = Vec::new();
        for (target, kind) in hub_node.pins() {
            listed.push(project_listed_edge(
                hub_node.kind,
                hub_node.id,
                target,
                kind,
                &visible,
            ));
        }
        assert_eq!(listed.len(), 4);
        assert_eq!(
            listed
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Origin)
                .count(),
            3
        );
        let redacted = listed
            .iter()
            .find(|edge| edge.kind == EdgeKind::Reference)
            .expect("foreign ref is projected");
        assert!(
            matches!(redacted.target, EdgeTargetProjection::Redacted),
            "unauthorized pin target is redacted, not loaded"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner-scoped pin loads failed");
}

/// The pin is on the source; a foreign origin must redact.
#[tokio::test]
async fn lineage_redacts_foreign_origin_instead_of_dropping() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let other_permit = OwnerWritePermit::new_for_tests(other, AccessKind::Fact);

        let own = pg
            .ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let foreign = pg
            .ingest_fact_atomic(&other_permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let derived = pg
            .ingest_fact_atomic(
                &permit,
                &draft(
                    "abstraction",
                    vec![],
                    vec![own.memory_id.into_inner(), foreign.memory_id.into_inner()],
                ),
                None,
            )
            .await?;

        let walked = pg
            .walk_memory_lineage(
                &[owner],
                &MemoryLineageRequest {
                    owner,
                    start_memory_id: derived.memory_id,
                    direction: MemoryLineageDirection::Ancestors,
                    depth: 2,
                    limit: 20,
                    after: None,
                },
            )
            .await?;
        assert!(
            walked
                .nodes
                .iter()
                .any(|node| node.memory_id == derived.memory_id),
            "start node is admitted"
        );
        assert!(
            walked
                .nodes
                .iter()
                .any(|node| node.memory_id == own.memory_id),
            "same-owner origin is a node"
        );
        assert!(
            walked
                .nodes
                .iter()
                .all(|node| node.memory_id != foreign.memory_id),
            "foreign origin must not appear as a node"
        );
        assert!(
            walked.edges.iter().any(|hop| {
                hop.edge.source.memory_id() == Some(derived.memory_id)
                    && matches!(
                        hop.edge.target,
                        EdgeTargetProjection::Visible { target }
                            if target.memory_id() == Some(own.memory_id)
                    )
            }),
            "same-owner origin stays visible"
        );
        assert!(
            walked.edges.iter().any(|hop| {
                hop.edge.source.memory_id() == Some(derived.memory_id)
                    && matches!(hop.edge.target, EdgeTargetProjection::Redacted)
            }),
            "foreign origin hop is redacted, not dropped"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("lineage redaction failed");
}

#[tokio::test]
async fn inbound_pin_page_is_newest_heads_and_keyset() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let hub = pg
            .ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let mut ids = Vec::new();
        for _ in 0..300 {
            let child = pg
                .ingest_fact_atomic(
                    &permit,
                    &draft("abstraction", vec![], vec![hub.memory_id.into_inner()]),
                    None,
                )
                .await?;
            ids.push(child.memory_id);
        }
        let newest = &ids[ids.len() - 200..];
        let oldest = &ids[..100];

        let page = pg
            .load_inbound_pin_nodes(
                &[owner],
                InboundPinQuery {
                    targets: &[hub.memory_id],
                    goal_targets: false,
                    kind: Some(EdgeKind::Origin),
                    heads_only: true,
                    after: None,
                    limit: 200,
                },
            )
            .await?;
        let got: Vec<_> = page.iter().map(|n| n.id).collect();
        assert_eq!(got.len(), 200);
        for id in newest {
            assert!(got.contains(id), "newest head {id:?} missing from sample");
        }
        for id in oldest {
            assert!(!got.contains(id), "oldest head {id:?} leaked into sample");
        }

        let rest = pg
            .load_inbound_pin_nodes(
                &[owner],
                InboundPinQuery {
                    targets: &[hub.memory_id],
                    goal_targets: false,
                    kind: Some(EdgeKind::Origin),
                    heads_only: true,
                    after: page.last().map(|n| n.id),
                    limit: 200,
                },
            )
            .await?;
        let rest_ids: Vec<_> = rest.iter().map(|n| n.id).collect();
        assert_eq!(rest_ids.len(), 100);
        for id in &got {
            assert!(!rest_ids.contains(id), "keyset overlapped {id:?}");
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("inbound pin page test failed");
}

#[tokio::test]
async fn inbound_heads_only_drops_superseded_pin() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let hub = pg
            .ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let other = pg
            .ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let old = pg
            .ingest_fact_atomic(
                &permit,
                &draft("abstraction", vec![], vec![hub.memory_id.into_inner()]),
                None,
            )
            .await?;
        let mut next = draft("abstraction", vec![], vec![other.memory_id.into_inner()]);
        next.handle = Some(old.handle);
        let new = pg.ingest_fact_atomic(&permit, &next, None).await?;
        assert_ne!(old.memory_id, new.memory_id);

        let heads = pg
            .load_inbound_pin_nodes(
                &[owner],
                InboundPinQuery {
                    targets: &[hub.memory_id],
                    goal_targets: false,
                    kind: Some(EdgeKind::Origin),
                    heads_only: true,
                    after: None,
                    limit: 50,
                },
            )
            .await?;
        assert!(
            heads
                .iter()
                .all(|n| n.id != old.memory_id && n.id != new.memory_id),
            "rewritten series no longer pins hub at head"
        );

        let all_hot = pg
            .load_inbound_pin_nodes(
                &[owner],
                InboundPinQuery {
                    targets: &[hub.memory_id],
                    goal_targets: false,
                    kind: Some(EdgeKind::Origin),
                    heads_only: false,
                    after: None,
                    limit: 50,
                },
            )
            .await?;
        assert!(
            all_hot.iter().any(|n| n.id == old.memory_id),
            "superseded t still pins hub on the complete path"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("inbound heads-only rewrite test failed");
}

/// A←{B,C}←D←{E1,E2}←F. D must be expanded once (two origin edges, not four).
#[tokio::test]
async fn lineage_diamond_visits_shared_node_once() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);

        let leaf = pg
            .ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let e1 = pg
            .ingest_fact_atomic(
                &permit,
                &draft("abstraction", vec![], vec![leaf.memory_id.into_inner()]),
                None,
            )
            .await?;
        let e2 = pg
            .ingest_fact_atomic(
                &permit,
                &draft("abstraction", vec![], vec![leaf.memory_id.into_inner()]),
                None,
            )
            .await?;
        let meet = pg
            .ingest_fact_atomic(
                &permit,
                &draft(
                    "abstraction",
                    vec![],
                    vec![e1.memory_id.into_inner(), e2.memory_id.into_inner()],
                ),
                None,
            )
            .await?;
        let left = pg
            .ingest_fact_atomic(
                &permit,
                &draft("abstraction", vec![], vec![meet.memory_id.into_inner()]),
                None,
            )
            .await?;
        let right = pg
            .ingest_fact_atomic(
                &permit,
                &draft("abstraction", vec![], vec![meet.memory_id.into_inner()]),
                None,
            )
            .await?;
        let tip = pg
            .ingest_fact_atomic(
                &permit,
                &draft(
                    "abstraction",
                    vec![],
                    vec![left.memory_id.into_inner(), right.memory_id.into_inner()],
                ),
                None,
            )
            .await?;

        let up = pg
            .walk_memory_lineage(
                &[owner],
                &MemoryLineageRequest {
                    owner,
                    start_memory_id: tip.memory_id,
                    direction: MemoryLineageDirection::Ancestors,
                    depth: 8,
                    limit: 50,
                    after: None,
                },
            )
            .await?;
        let meet_origins: Vec<_> = up
            .edges
            .iter()
            .filter(|hop| hop.edge.source.memory_id() == Some(meet.memory_id))
            .collect();
        assert_eq!(
            meet_origins.len(),
            2,
            "meet origin edges must not duplicate: {meet_origins:?}"
        );
        assert_eq!(
            up.edges.len(),
            8,
            "diamond is 8 origin edges, got {}",
            up.edges.len()
        );

        let down = pg
            .walk_memory_lineage(
                &[owner],
                &MemoryLineageRequest {
                    owner,
                    start_memory_id: leaf.memory_id,
                    direction: MemoryLineageDirection::Descendants,
                    depth: 8,
                    limit: 50,
                    after: None,
                },
            )
            .await?;
        let tip_hops: Vec<_> = down
            .edges
            .iter()
            .filter(|hop| hop.edge.source.memory_id() == Some(tip.memory_id))
            .collect();
        assert_eq!(
            tip_hops.len(),
            2,
            "tip must enter the descendant frontier once"
        );
        assert_eq!(down.edges.len(), 8, "descendants match the same 8 edges");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("diamond lineage test failed");
}

#[tokio::test]
async fn lineage_pages_finish_a_distance_before_the_next() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);

        let leaf = pg
            .ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let mut mids = Vec::new();
        for _ in 0..5 {
            let mid = pg
                .ingest_fact_atomic(
                    &permit,
                    &draft("abstraction", vec![], vec![leaf.memory_id.into_inner()]),
                    None,
                )
                .await?;
            mids.push(mid.memory_id);
        }
        let root = pg
            .ingest_fact_atomic(
                &permit,
                &draft(
                    "abstraction",
                    vec![],
                    mids.iter().map(|id| id.into_inner()).collect(),
                ),
                None,
            )
            .await?;

        let req = |limit: u32, after: Option<MemoryLineageCursor>| MemoryLineageRequest {
            owner,
            start_memory_id: root.memory_id,
            direction: MemoryLineageDirection::Ancestors,
            depth: 3,
            limit,
            after,
        };
        let page1 = pg.walk_memory_lineage(&[owner], &req(2, None)).await?;
        assert_eq!(page1.edges.len(), 2);
        assert!(page1.truncated);
        assert!(page1.edges.iter().all(|hop| hop.distance == 1));
        assert_eq!(
            page1.edges[0]
                .edge
                .target
                .endpoint()
                .and_then(proxima_core::EdgeEndpoint::memory_id),
            Some(mids[4])
        );
        assert_eq!(
            page1.edges[1]
                .edge
                .target
                .endpoint()
                .and_then(proxima_core::EdgeEndpoint::memory_id),
            Some(mids[3])
        );

        let page2 = pg
            .walk_memory_lineage(&[owner], &req(2, page1.next_cursor))
            .await?;
        assert_eq!(page2.edges.len(), 2);
        assert!(page2.truncated);
        assert!(page2.edges.iter().all(|hop| hop.distance == 1));

        let page3 = pg
            .walk_memory_lineage(&[owner], &req(2, page2.next_cursor))
            .await?;
        assert_eq!(page3.edges.len(), 2);
        assert_eq!(page3.edges[0].distance, 1);
        assert_eq!(page3.edges[1].distance, 2);
        assert!(page3.truncated);

        let from_last_dist1 = MemoryLineageCursor {
            distance: 1,
            source: EntityRef::Memory(root.memory_id),
            target: EntityRef::Memory(mids[0]),
        };
        let dist2 = pg
            .walk_memory_lineage(&[owner], &req(10, Some(from_last_dist1)))
            .await?;
        assert!(dist2.edges.iter().all(|hop| hop.distance == 2));
        assert_eq!(dist2.edges.len(), 5);
        assert!(!dist2.truncated);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("lineage keyset test failed");
}
