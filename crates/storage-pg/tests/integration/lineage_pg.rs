use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::storage_ports::*;

use proxima_core::verbs::query::{MemoryLineageDirection, MemoryLineageRequest};
use proxima_core::{MemoryId, Owner, OwnerRef, RelationClass, UserId};
use uuid::Uuid;

#[tokio::test]
async fn walk_memory_lineage_follows_provenance_and_supersession_by_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let other_owner = OwnerRef::Personal(UserId::new(Uuid::from_u128(99)));

    let old = insert_memory(&pg, &owner, "old abstraction").await?;
    let new = insert_memory(&pg, &owner, "new abstraction").await?;
    let perspective = insert_memory(&pg, &owner, "perspective").await?;
    let other_old = insert_memory(&pg, &other_owner, "other owner old").await?;
    let other = insert_memory(&pg, &other_owner, "other owner").await?;
    let world = OwnerRef::World;
    let world_old = insert_memory(&pg, &world, "world old").await?;
    let world_ref = insert_memory(&pg, &owner, "world ref").await?;

    insert_edge(
        &pg,
        &owner,
        new,
        old,
        "core/supersedes",
        RelationClass::Supersession,
    )
    .await?;
    insert_edge(
        &pg,
        &owner,
        perspective,
        new,
        "core/derived-from",
        RelationClass::Provenance,
    )
    .await?;
    insert_edge(
        &pg,
        &other_owner,
        other,
        other_old,
        "other/derived-from",
        RelationClass::Provenance,
    )
    .await?;
    insert_edge(
        &pg,
        &owner,
        world_ref,
        world_old,
        "world/derived-from",
        RelationClass::Provenance,
    )
    .await?;

    assert_owner_lineage(&pg, owner, old, perspective, other).await?;
    assert_world_lineage(&pg, owner, world, world_old, world_ref).await?;

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Diamond fan-out: the same edge reachable through two equal-length
/// paths must project once (the recursive walk used to emit one row per
/// path), and keyset pages must cover the whole walk exactly once.
#[tokio::test]
async fn walk_memory_lineage_pages_and_deduplicates_diamonds()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let start = insert_memory(&pg, &owner, "start").await?;
    let a1 = insert_memory(&pg, &owner, "arm one").await?;
    let a2 = insert_memory(&pg, &owner, "arm two").await?;
    let joint = insert_memory(&pg, &owner, "joint").await?;
    let origin = insert_memory(&pg, &owner, "origin").await?;

    let mut expected_edges = Vec::new();
    for (source, target) in [(start, a1), (start, a2), (a1, joint), (a2, joint)] {
        expected_edges.push(
            insert_edge(
                &pg,
                &owner,
                source,
                target,
                "core/derived-from",
                RelationClass::Provenance,
            )
            .await?,
        );
    }
    // The diamond joint's outgoing edge is reachable through both arms at
    // the same distance — the duplicate-projection case.
    expected_edges.push(
        insert_edge(
            &pg,
            &owner,
            joint,
            origin,
            "core/derived-from",
            RelationClass::Provenance,
        )
        .await?,
    );

    let owner_read = vec![owner];
    let request = |after| MemoryLineageRequest {
        owner,
        start_memory_id: MemoryId::new(start),
        direction: MemoryLineageDirection::Ancestors,
        depth: 4,
        limit: 2,
        after,
    };

    // Unpaged full walk projects each edge exactly once despite the
    // two equal-length paths through the diamond.
    let full = pg
        .walk_memory_lineage(
            &owner_read,
            &MemoryLineageRequest {
                limit: 20,
                after: None,
                ..request(None)
            },
        )
        .await?;
    assert_eq!(full.edges.len(), expected_edges.len());
    assert!(!full.truncated);
    assert!(full.next_cursor.is_none());

    // Keyset pages: 2 + 2 + 1, no duplicates, exact coverage.
    let mut walked = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = pg.walk_memory_lineage(&owner_read, &request(after)).await?;
        pages += 1;
        assert!(pages <= 3, "walk must exhaust in three pages");
        assert!(
            page.nodes
                .iter()
                .any(|node| node.memory_id.into_inner() == start && node.distance == 0),
            "every page carries the start node"
        );
        walked.extend(page.edges.iter().map(|edge| edge.edge_id));
        assert_eq!(page.truncated, page.next_cursor.is_some());
        match page.next_cursor {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    let mut expected = expected_edges.clone();
    expected.sort_unstable();
    let mut walked_sorted = walked.clone();
    walked_sorted.sort_unstable();
    assert_eq!(
        walked_sorted, expected,
        "pages must cover every lineage edge exactly once"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn assert_owner_lineage(
    pg: &proxima_storage_pg::PgStorage,
    owner: Owner,
    old: Uuid,
    perspective: Uuid,
    other: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let owner_read = vec![owner];
    let ancestors = pg
        .walk_memory_lineage(
            &owner_read,
            &MemoryLineageRequest {
                owner,
                start_memory_id: MemoryId::new(perspective),
                direction: MemoryLineageDirection::Ancestors,
                depth: 3,
                limit: 10,
                after: None,
            },
        )
        .await?;
    assert_eq!(ancestors.nodes.len(), 3);
    assert_eq!(ancestors.edges.len(), 2);
    assert!(
        ancestors
            .nodes
            .iter()
            .any(|node| node.memory_id.into_inner() == old && node.distance == 2)
    );
    assert!(
        !ancestors
            .nodes
            .iter()
            .any(|node| node.memory_id.into_inner() == other)
    );

    let descendants = pg
        .walk_memory_lineage(
            &owner_read,
            &MemoryLineageRequest {
                owner,
                start_memory_id: MemoryId::new(old),
                direction: MemoryLineageDirection::Descendants,
                depth: 3,
                limit: 10,
                after: None,
            },
        )
        .await?;
    assert_eq!(descendants.nodes.len(), 3);
    assert!(
        descendants
            .nodes
            .iter()
            .any(|node| node.memory_id.into_inner() == perspective && node.distance == 2)
    );
    Ok(())
}

async fn assert_world_lineage(
    pg: &proxima_storage_pg::PgStorage,
    owner: Owner,
    world: Owner,
    world_old: Uuid,
    world_ref: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let world_read = vec![owner, world];
    let world_ancestors = pg
        .walk_memory_lineage(
            &world_read,
            &MemoryLineageRequest {
                owner: world,
                start_memory_id: MemoryId::new(world_old),
                direction: MemoryLineageDirection::Descendants,
                depth: 2,
                limit: 10,
                after: None,
            },
        )
        .await?;
    assert_eq!(world_ancestors.nodes.len(), 2);
    assert!(
        world_ancestors
            .nodes
            .iter()
            .any(|node| node.memory_id.into_inner() == world_ref && node.distance == 1),
        "World-owned lineage must match read_set rows with NULL owner_id"
    );
    Ok(())
}

async fn insert_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/lineage-v1', 1, 'Abstraction',
                 $4, 'AtoA', '00000000-0000-0000-0000-000000000311'::uuid,
                 '00000000-0000-0000-0000-000000000312'::uuid, NULL,
                 'test-model', 'test-v1')",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(memory_id)
}

async fn insert_edge(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    source: Uuid,
    target: Uuid,
    relation: &str,
    relation_class: RelationClass,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let edge_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, owner_kind, owner_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id)
         VALUES ($1, $2, $3, $4, $5,
                 'Abstraction', $6, NULL,
                 'Abstraction', $7, NULL,
                 'Engine', NULL)",
    )
    .bind(edge_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(relation)
    .bind(relation_class)
    .bind(source)
    .bind(target)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(edge_id)
}
