//! Neighbor windows redact, they do not rewrite.
//!
//! A neighbor read answers "what touches these memories". An endpoint the
//! reader may not see comes back withheld rather than removed, so the shape of
//! the graph is the same for every reader and only the contents differ.

use proxima_core::storage_ports::*;
use proxima_core::{
    EdgeKind, EdgeTargetProjection, EntityKind, EntityRef, GroupId, MemoryId, OwnerRef, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use crate::common::{drop_db, fresh_pg, seed_memory, seed_memory_edge};

struct Fixture {
    p: OwnerRef,
    g1: OwnerRef,
    a: MemoryId,
    f1: MemoryId,
}

async fn seed(pg: &PgStorage) -> Result<Fixture, Box<dyn std::error::Error>> {
    let p = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let g1 = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let a = seed_memory(pg, &p, EntityKind::Abstraction, "abstraction").await?;
    let f1 = seed_memory(pg, &g1, EntityKind::Fact, "grounding fact").await?;
    seed_memory_edge(
        pg,
        &p,
        (EntityKind::Abstraction, a),
        (EntityKind::Fact, f1),
        EdgeKind::Origin,
    )
    .await?;
    Ok(Fixture { p, g1, a, f1 })
}

#[tokio::test]
async fn a_readable_edge_with_an_unreadable_target_keeps_its_row()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let fx = seed(&pg).await?;

        let full = pg
            .load_neighbor_memory_edges(&[fx.p, fx.g1], &[fx.a], 50)
            .await?;
        assert_eq!(full.len(), 1);
        assert_eq!(
            full[0].target.endpoint().map(|target| target.entity),
            Some(EntityRef::Memory(fx.f1))
        );
        assert_eq!(full[0].kind, EdgeKind::Origin);

        let redacted = pg.load_neighbor_memory_edges(&[fx.p], &[fx.a], 50).await?;
        assert_eq!(redacted.len(), 1, "the row survives; only the target goes");
        assert_eq!(redacted[0].target, EdgeTargetProjection::Redacted);
        assert_eq!(redacted[0].source.kind, EntityKind::Abstraction);
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

#[tokio::test]
async fn an_unreadable_source_hides_the_edge() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let fx = seed(&pg).await?;
        let stranger = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let edges = pg
            .load_neighbor_memory_edges(&[stranger, fx.g1], &[fx.f1], 50)
            .await?;
        assert!(
            edges.is_empty(),
            "the edge is owned by its source; an unreadable source means no row"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// A neighbor window is over memories in either direction: an edge is returned
/// whether the requested id is its source or its target.
#[tokio::test]
async fn the_window_looks_both_ways() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let fx = seed(&pg).await?;
        let from_target = pg
            .load_neighbor_memory_edges(&[fx.p, fx.g1], &[fx.f1], 50)
            .await?;
        assert_eq!(from_target.len(), 1);
        assert_eq!(
            from_target[0].source.entity,
            EntityRef::Memory(fx.a),
            "the requested id is the target here"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

#[tokio::test]
async fn an_empty_read_set_returns_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let fx = seed(&pg).await?;
        assert!(
            pg.load_neighbor_memory_edges(&[], &[fx.a], 50)
                .await?
                .is_empty()
        );
        assert!(
            pg.load_neighbor_memory_edges(&[fx.p], &[], 50)
                .await?
                .is_empty()
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}
