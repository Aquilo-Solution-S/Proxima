//! P1: pins on the node, PK outbound, GIN inbound, lineage by t.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::storage_ports::{MemoryReadPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::query::{
    EntityKind, MemoryLineageDirection, MemoryLineageRequest, QueryRequest,
};
use proxima_core::verbs::schema::{
    MemorySearchProjection, MemorySearchProjectionField, PayloadKind,
};
use proxima_core::{
    AccessKind, EdgeKind, EdgeTargetProjection, OwnerRef, SchemaId, SchemaVersion,
    SearchProjectionColumnKind, UserId, project_listed_edge, project_window_edges,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use uuid::Uuid;

fn note_projection() -> MemorySearchProjection {
    MemorySearchProjection {
        schema_id: SchemaId::new("core/agent-note-v1".to_string()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Fact,
        sidecar_table: "proxima_core.agent_note_v1".into(),
        fields: vec![
            MemorySearchProjectionField {
                column: "title".into(),
                kind: SearchProjectionColumnKind::Text,
            },
            MemorySearchProjectionField {
                column: "body".into(),
                kind: SearchProjectionColumnKind::Text,
            },
        ],
        tag_column: Some("tags".into()),
        tsv_column: Some("search_tsv".into()),
        embed_text_column: Some("embed_text".into()),
        language_column: Some("lexical_language".into()),
    }
}

fn draft(kind: &str, refs: Vec<Uuid>, origins: Vec<Uuid>) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(format!("graph/{kind}")),
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
        let pg = PgStorage::connect(&url)
            .await?
            .with_search_projections(vec![note_projection()]);
        pg.run_migrations().await?;
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();

        let leaf = ingest_fact_atomic(pool, &permit, &draft("fact", vec![], vec![]), None).await?;
        let mut derived_cmd = draft("abstraction", vec![], vec![leaf.memory_id.into_inner()]);
        derived_cmd.schema_id = SchemaId::new("core/agent-note-v1".into());
        let derived = ingest_fact_atomic(pool, &permit, &derived_cmd, None).await?;
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $2, 'derived title', 'made from leaf', '{}')",
        )
        .bind(derived.memory_id.into_inner())
        .bind(Uuid::now_v7())
        .execute(pool)
        .await?;

        let mut q = QueryRequest::for_owner(owner);
        q.include_payloads = false;
        let page = pg.query_memories(&q, &[]).await?;
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
            .load_inbound_pin_nodes(&[owner], &[leaf.memory_id])
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
        let pool = pg.pool_for_tests();

        let a = ingest_fact_atomic(pool, &permit, &draft("fact", vec![], vec![]), None).await?;
        let b = ingest_fact_atomic(pool, &permit, &draft("fact", vec![], vec![]), None).await?;
        let c = ingest_fact_atomic(pool, &permit, &draft("fact", vec![], vec![]), None).await?;
        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let other_permit = OwnerWritePermit::new_for_tests(other, AccessKind::Fact);
        let foreign =
            ingest_fact_atomic(pool, &other_permit, &draft("fact", vec![], vec![]), None).await?;
        let hub = ingest_fact_atomic(
            pool,
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

/// Old SQL `JOIN tgt AND tgt.owner_id = ANY` dropped the hop. The pin
/// is on the source; a foreign origin must redact.
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
        let pool = pg.pool_for_tests();

        let own = ingest_fact_atomic(pool, &permit, &draft("fact", vec![], vec![]), None).await?;
        let foreign =
            ingest_fact_atomic(pool, &other_permit, &draft("fact", vec![], vec![]), None).await?;
        let derived = ingest_fact_atomic(
            pool,
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
    result.expect("D8 lineage redaction failed");
}
