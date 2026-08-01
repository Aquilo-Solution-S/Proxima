//! Edge reads: source-owned visibility, target redaction, kind filtering, and
//! a keyset whose position is the row itself.

use proxima_core::access::world;
use proxima_core::storage_ports::*;
use proxima_core::verbs::query::{EdgeExistsRequest, EdgeFilter, EdgeReadRequest};
use proxima_core::{
    EdgeKind, EdgeTargetProjection, EntityKind, EntityRef, GroupId, MemoryId, OwnerRef, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use crate::common::{drop_db, fresh_pg, seed_memory, seed_memory_edge};

/// The persisted result of an owner transfer: the node becomes World-owned
/// while the edge keeps the owner that wrote it.
async fn publish_to_world(pg: &PgStorage, memory_id: MemoryId) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE proxima_core.memories
            SET owner_kind = 'world', owner_id = NULL
          WHERE memory_id = $1",
    )
    .bind(memory_id.into_inner())
    .execute(pg.pool_for_tests())
    .await
    .map(|_| ())
}

struct Fixture {
    p: OwnerRef,
    q: OwnerRef,
    p_read: Vec<OwnerRef>,
    q_read: Vec<OwnerRef>,
    p_without_g1_read: Vec<OwnerRef>,
    world_read: Vec<OwnerRef>,
    a: MemoryId,
    f1: MemoryId,
    public: MemoryId,
}

/// P owns an Abstraction; group G1 owns the Fact it was made from; a
/// World-owned Abstraction points at a private Fact of Q.
async fn seed(pg: &PgStorage) -> Result<Fixture, Box<dyn std::error::Error>> {
    let p = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let q = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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

    // World ownership is the persisted result of publish-to-World, so the
    // edge is written under the authoring owner and the NODE moves afterwards.
    // World is never a write owner, on an edge or anywhere else.
    let public = seed_memory(pg, &p, EntityKind::Abstraction, "published").await?;
    let private = seed_memory(pg, &q, EntityKind::Fact, "private").await?;
    seed_memory_edge(
        pg,
        &p,
        (EntityKind::Abstraction, public),
        (EntityKind::Fact, private),
        EdgeKind::Origin,
    )
    .await?;
    publish_to_world(pg, public).await?;

    Ok(Fixture {
        p,
        q,
        p_read: vec![p, g1],
        q_read: vec![q],
        p_without_g1_read: vec![p],
        world_read: vec![q, world()],
        a,
        f1,
        public,
    })
}

async fn read(
    pg: &PgStorage,
    read_owners: &[OwnerRef],
    owner: OwnerRef,
    filter: EdgeFilter,
) -> Result<Vec<proxima_core::Edge>, Box<dyn std::error::Error>> {
    let response = pg
        .read_edges(
            read_owners,
            &EdgeReadRequest {
                owner,
                filter,
                limit: 50,
                cursor: None,
            },
        )
        .await?;
    Ok(response.edges)
}

fn source_filter(memory_id: MemoryId) -> EdgeFilter {
    EdgeFilter {
        kind: None,
        source: Some(EntityRef::Memory(memory_id)),
        target: None,
    }
}

#[tokio::test]
async fn an_edge_is_visible_to_whoever_can_read_its_source()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let fx = seed(&pg).await?;

        let unreadable = read(&pg, &fx.q_read, fx.q, source_filter(fx.a)).await?;
        assert!(
            unreadable.is_empty(),
            "an unreadable source hides the edge entirely"
        );

        let visible = read(&pg, &fx.p_read, fx.p, source_filter(fx.a)).await?;
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].kind, EdgeKind::Origin);
        assert_eq!(visible[0].source.kind, EntityKind::Abstraction);
        assert_eq!(
            visible[0].target.endpoint().map(|target| target.entity),
            Some(EntityRef::Memory(fx.f1))
        );
        assert_eq!(
            visible[0].target.endpoint().map(|target| target.kind),
            Some(EntityKind::Fact)
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// A readable edge with an unreadable target keeps its shape: the row comes
/// back with the endpoint withheld, disclosing neither id nor kind.
#[tokio::test]
async fn an_unreadable_target_is_redacted_not_suppressed() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let fx = seed(&pg).await?;

        let redacted = read(&pg, &fx.p_without_g1_read, fx.p, source_filter(fx.a)).await?;
        assert_eq!(redacted.len(), 1);
        assert_eq!(redacted[0].target, EdgeTargetProjection::Redacted);
        assert!(redacted[0].target.endpoint().is_none());

        // A target-id filter must not become an oracle for the same fact.
        let probe = read(
            &pg,
            &fx.p_without_g1_read,
            fx.p,
            EdgeFilter {
                kind: None,
                source: Some(EntityRef::Memory(fx.a)),
                target: Some(EntityRef::Memory(fx.f1)),
            },
        )
        .await?;
        assert!(
            probe.is_empty(),
            "a target filter must not confirm an unreadable target"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// Publishing a node must not publish what it points at: a World-readable
/// source whose target the reader cannot see drops out of the page entirely.
#[tokio::test]
async fn a_world_readable_source_never_leaks_a_private_target()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let fx = seed(&pg).await?;
        let stranger = vec![OwnerRef::Personal(UserId::new(Uuid::now_v7())), world()];
        let edges = read(&pg, &stranger, world(), source_filter(fx.public)).await?;
        assert!(edges.is_empty(), "public-source guard omits private target");

        // The owner of the private target still sees it.
        let owner_view = read(&pg, &fx.world_read, world(), source_filter(fx.public)).await?;
        assert_eq!(owner_view.len(), 1);
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

#[tokio::test]
async fn the_kind_filter_narrows_to_one_kind() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let fx = seed(&pg).await?;
        seed_memory_edge(
            &pg,
            &fx.p,
            (EntityKind::Abstraction, fx.a),
            (EntityKind::Fact, fx.f1),
            EdgeKind::Reference,
        )
        .await?;

        let all = read(&pg, &fx.p_read, fx.p, source_filter(fx.a)).await?;
        assert_eq!(all.len(), 2, "one origin and one reference, same endpoints");

        let origins = read(
            &pg,
            &fx.p_read,
            fx.p,
            EdgeFilter {
                kind: Some(EdgeKind::Origin),
                source: Some(EntityRef::Memory(fx.a)),
                target: None,
            },
        )
        .await?;
        assert_eq!(origins.len(), 1);
        assert_eq!(origins[0].kind, EdgeKind::Origin);
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

/// The keyset position is the edge itself — there is no id to resume from —
/// and a paged walk visits every row exactly once.
#[tokio::test]
async fn the_keyset_pages_without_skipping_or_repeating() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let source = seed_memory(&pg, &owner, EntityKind::Abstraction, "source").await?;
        let mut targets = Vec::new();
        for index in 0..5 {
            let target =
                seed_memory(&pg, &owner, EntityKind::Fact, &format!("fact {index}")).await?;
            seed_memory_edge(
                &pg,
                &owner,
                (EntityKind::Abstraction, source),
                (EntityKind::Fact, target),
                EdgeKind::Origin,
            )
            .await?;
            targets.push(target);
        }

        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = pg
                .read_edges(
                    std::slice::from_ref(&owner),
                    &EdgeReadRequest {
                        owner,
                        filter: source_filter(source),
                        limit: 2,
                        cursor,
                    },
                )
                .await?;
            seen.extend(
                page.edges
                    .iter()
                    .filter_map(|edge| edge.target.endpoint().map(|target| target.entity)),
            );
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }

        assert_eq!(seen.len(), 5, "every row is visited");
        let mut unique = seen.clone();
        unique.sort_by_key(|entity| match entity {
            EntityRef::Memory(id) => id.into_inner(),
            EntityRef::Goal(id) => id.into_inner(),
            EntityRef::FactEntity(id) => id.into_inner(),
        });
        unique.dedup();
        assert_eq!(unique.len(), 5, "no row is visited twice");
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}

#[tokio::test]
async fn existence_is_disclosed_only_for_readable_sources() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        let fx = seed(&pg).await?;
        let request = EdgeExistsRequest {
            owner: fx.p,
            filter: source_filter(fx.a),
        };
        assert!(pg.edge_exists(&fx.p_read, &request).await?.exists);
        assert!(!pg.edge_exists(&fx.q_read, &request).await?.exists);
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    drop_db(&db_name).await.ok();
    result
}
