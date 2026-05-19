mod common;

use common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::verbs::query::{MemoryLineageDirection, MemoryLineageRequest};
use proxima_core::{
    MemoryId, OrgId, Owner, OwnerPrincipalKind, Principal, RelationClass, Storage, UserId,
};
use uuid::Uuid;

#[tokio::test]
async fn walk_memory_lineage_follows_provenance_and_supersession_by_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let other_owner = Owner {
        principal: Principal::User(UserId::new(Uuid::from_u128(99))),
        org_id: OrgId::new(Uuid::nil()),
    };

    let old = insert_memory(&pg, &owner, "old abstraction", 1).await?;
    let new = insert_memory(&pg, &owner, "new abstraction", 2).await?;
    let perspective = insert_memory(&pg, &owner, "perspective", 3).await?;
    let other = insert_memory(&pg, &other_owner, "other owner", 4).await?;

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
        old,
        "other/derived-from",
        RelationClass::Provenance,
    )
    .await?;

    let ancestors = pg
        .walk_memory_lineage(&MemoryLineageRequest {
            owner: owner.clone(),
            start_memory_id: MemoryId::new(perspective),
            direction: MemoryLineageDirection::Ancestors,
            depth: 3,
            limit: 10,
            reader_personality_instance_id: None,
        })
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
        .walk_memory_lineage(&MemoryLineageRequest {
            owner,
            start_memory_id: MemoryId::new(old),
            direction: MemoryLineageDirection::Descendants,
            depth: 3,
            limit: 10,
            reader_personality_instance_id: None,
        })
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
    wake_chain_depth: i16,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, 'test/lineage-v1', 1, 'Abstraction',
                 $5, 'Wake', 'test-model', 'test-v1',
                 '00000000-0000-0000-0000-000000000000'::uuid, $6)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(text)
    .bind(wake_chain_depth)
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
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ($1, $2, $3,
                 'Abstraction', $4, NULL,
                 'Abstraction', $5, NULL,
                 'Engine', NULL,
                 $6, $7, $8)",
    )
    .bind(edge_id)
    .bind(relation)
    .bind(relation_class)
    .bind(source)
    .bind(target)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pg.pool())
    .await?;
    Ok(edge_id)
}
