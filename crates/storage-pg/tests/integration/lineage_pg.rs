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

    let owner_read = vec![owner];
    let ancestors = pg
        .walk_memory_lineage(
            &owner_read,
            &MemoryLineageRequest {
                principal: owner,
                start_memory_id: MemoryId::new(perspective),
                direction: MemoryLineageDirection::Ancestors,
                depth: 3,
                limit: 10,
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
                principal: owner,
                start_memory_id: MemoryId::new(old),
                direction: MemoryLineageDirection::Descendants,
                depth: 3,
                limit: 10,
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

    drop(pg);
    drop_db(&db_name).await?;
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
             operator_kind, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/lineage-v1', 1, 'Abstraction',
                 $4, 'Wake', 'test-model', 'test-v1')",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(text)
    .execute(pg.pool())
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
    .execute(pg.pool())
    .await?;
    Ok(edge_id)
}
