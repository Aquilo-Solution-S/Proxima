//! The lineage walk traverses `origin` only. Supersession is a row pointer;
//! following it would answer a different question.

use proxima_core::storage_ports::*;
use proxima_core::verbs::query::{MemoryLineageDirection, MemoryLineageRequest};
use proxima_core::{
    EdgeKind, EdgeTargetProjection, EntityKind, EntityRef, GroupId, MemoryId, OwnerRef, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use crate::common::{drop_db, fresh_pg, seed_memory, seed_memory_edge};

struct Chain {
    perspective: MemoryId,
    abstraction: MemoryId,
    fact: MemoryId,
}

/// P ← A ← F, all origins, all one owner.
async fn seed_chain(pg: &PgStorage, owner: OwnerRef) -> Result<Chain, Box<dyn std::error::Error>> {
    let fact = seed_memory(pg, &owner, EntityKind::Fact, "the observation").await?;
    let abstraction = seed_memory(pg, &owner, EntityKind::Abstraction, "the summary").await?;
    let perspective = seed_memory(pg, &owner, EntityKind::Perspective, "the judgment").await?;
    seed_memory_edge(
        pg,
        &owner,
        (EntityKind::Abstraction, abstraction),
        (EntityKind::Fact, fact),
        EdgeKind::Origin,
    )
    .await?;
    seed_memory_edge(
        pg,
        &owner,
        (EntityKind::Perspective, perspective),
        (EntityKind::Abstraction, abstraction),
        EdgeKind::Origin,
    )
    .await?;
    Ok(Chain {
        perspective,
        abstraction,
        fact,
    })
}

fn request(
    owner: OwnerRef,
    start: MemoryId,
    direction: MemoryLineageDirection,
) -> MemoryLineageRequest {
    MemoryLineageRequest {
        owner,
        start_memory_id: start,
        direction,
        depth: 4,
        limit: 50,
        after: None,
    }
}

#[tokio::test]
async fn ancestors_follow_origin_upstream() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let chain = seed_chain(&pg, owner).await?;

        let response = pg
            .walk_memory_lineage(
                &[owner],
                &request(owner, chain.perspective, MemoryLineageDirection::Ancestors),
            )
            .await?;
        assert_eq!(response.edges.len(), 2, "P→A and A→F");
        assert!(
            response
                .edges
                .iter()
                .all(|edge| edge.edge.kind == EdgeKind::Origin)
        );
        let distances: Vec<_> = response.edges.iter().map(|edge| edge.distance).collect();
        assert!(distances.contains(&1) && distances.contains(&2));
        let node_ids: Vec<_> = response
            .nodes
            .iter()
            .map(|node| node.memory_id)
            .collect::<Vec<_>>();
        assert!(node_ids.contains(&chain.abstraction));
        assert!(node_ids.contains(&chain.fact));
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

#[tokio::test]
async fn descendants_follow_origin_downstream() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let chain = seed_chain(&pg, owner).await?;

        let response = pg
            .walk_memory_lineage(
                &[owner],
                &request(owner, chain.fact, MemoryLineageDirection::Descendants),
            )
            .await?;
        assert_eq!(response.edges.len(), 2);
        assert!(
            response
                .nodes
                .iter()
                .any(|node| node.memory_id == chain.perspective)
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// A `reference` row between the same nodes is not lineage: the walk does not
/// see it, because it is not a claim about what the node was made from.
#[tokio::test]
async fn a_reference_is_not_lineage() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let subject = seed_memory(&pg, &owner, EntityKind::Fact, "subject").await?;
        let claim = seed_memory(&pg, &owner, EntityKind::Perspective, "a claim about it").await?;
        seed_memory_edge(
            &pg,
            &owner,
            (EntityKind::Perspective, claim),
            (EntityKind::Fact, subject),
            EdgeKind::Reference,
        )
        .await?;

        let response = pg
            .walk_memory_lineage(
                &[owner],
                &request(owner, claim, MemoryLineageDirection::Ancestors),
            )
            .await?;
        assert!(
            response.edges.is_empty(),
            "an interpretation grounds through references, which lineage does not walk"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// An unreadable next endpoint is withheld, and the walk stops there rather
/// than crossing into what the reader may not see.
#[tokio::test]
async fn an_unreadable_hop_is_redacted_and_terminal() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));

        let hidden = seed_memory(&pg, &group, EntityKind::Fact, "not yours").await?;
        let mine = seed_memory(&pg, &owner, EntityKind::Abstraction, "mine").await?;
        seed_memory_edge(
            &pg,
            &owner,
            (EntityKind::Abstraction, mine),
            (EntityKind::Fact, hidden),
            EdgeKind::Origin,
        )
        .await?;

        let response = pg
            .walk_memory_lineage(
                &[owner],
                &request(owner, mine, MemoryLineageDirection::Ancestors),
            )
            .await?;
        assert_eq!(response.edges.len(), 1);
        assert_eq!(
            response.edges[0].edge.target,
            EdgeTargetProjection::Redacted
        );
        assert!(
            !response.nodes.iter().any(|node| node.memory_id == hidden),
            "a redacted endpoint contributes no node"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// The cursor is the edge, because an edge has no id.
#[tokio::test]
async fn the_lineage_cursor_is_the_edge() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let source = seed_memory(&pg, &owner, EntityKind::Abstraction, "source").await?;
        for index in 0..4 {
            let target =
                seed_memory(&pg, &owner, EntityKind::Fact, &format!("input {index}")).await?;
            seed_memory_edge(
                &pg,
                &owner,
                (EntityKind::Abstraction, source),
                (EntityKind::Fact, target),
                EdgeKind::Origin,
            )
            .await?;
        }

        let mut seen: Vec<EntityRef> = Vec::new();
        let mut after = None;
        loop {
            let response = pg
                .walk_memory_lineage(
                    &[owner],
                    &MemoryLineageRequest {
                        owner,
                        start_memory_id: source,
                        direction: MemoryLineageDirection::Ancestors,
                        depth: 1,
                        limit: 2,
                        after,
                    },
                )
                .await?;
            seen.extend(
                response
                    .edges
                    .iter()
                    .filter_map(|edge| edge.edge.target.endpoint().map(|target| target.entity)),
            );
            match response.next_cursor {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }
        assert_eq!(seen.len(), 4);
        let mut unique = seen.clone();
        unique.sort_by_key(|entity| match entity {
            EntityRef::Memory(id) => id.into_inner(),
            EntityRef::Goal(id) => id.into_inner(),
            EntityRef::FactEntity(id) => id.into_inner(),
        });
        unique.dedup();
        assert_eq!(unique.len(), 4, "paging visits each edge exactly once");
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}
