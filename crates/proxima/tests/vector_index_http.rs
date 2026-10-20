//! The network form of a store's vector index: a client and the router
//! between them behave as the index behind the router, and each side refuses
//! what the other must not send.

use std::collections::BTreeMap;
use std::sync::Arc;

use proxima::vector_index_http::{VectorIndexClient, vector_index_router};
use proxima_core::storage_ports::{
    InMemoryVectorIndex, VectorCandidate, VectorEntry, VectorIndex, VectorKey,
};
use proxima_core::{
    EmbeddableEntityRef, EmbeddingDim, EmbeddingSpace, EntityKind, GoalId, MemoryId, OwnerId,
    PlaintextHosts, SpaceVector, StorageError, UserId,
};
use uuid::Uuid;

/// A router on a loopback port, stopped when dropped.
struct Served {
    base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(indexes: BTreeMap<u16, Arc<dyn VectorIndex>>) -> Served {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = vector_index_router(indexes);
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    Served { base, task }
}

fn client(served: &Served, store: u16) -> VectorIndexClient {
    VectorIndexClient::new(&served.base, store, &PlaintextHosts::none()).unwrap()
}

fn space() -> EmbeddingSpace {
    EmbeddingSpace::new("wire", EmbeddingDim::D384)
}

fn vector(head: [f32; 2]) -> SpaceVector {
    let mut values = vec![0.0; 384];
    values[..2].copy_from_slice(&head);
    SpaceVector::new(space(), values).unwrap()
}

fn memory(id: u128) -> EmbeddableEntityRef {
    EmbeddableEntityRef::Memory {
        kind: EntityKind::Perspective,
        memory_id: MemoryId::new(Uuid::from_u128(id)),
    }
}

fn key(id: u128) -> VectorKey {
    VectorKey {
        entity_id: Uuid::from_u128(id),
        space: space(),
    }
}

fn owner(id: u128) -> OwnerId {
    OwnerId::from(UserId::new(Uuid::from_u128(id)))
}

/// Every call through the wire lands on the store's own index and answers
/// what that index answers; another store's index stays untouched.
#[tokio::test]
async fn the_wire_carries_every_call_to_its_stores_index() {
    let (mine, other) = (
        Arc::new(InMemoryVectorIndex::default()),
        Arc::new(InMemoryVectorIndex::default()),
    );
    let served = serve(BTreeMap::from([
        (3, mine.clone() as Arc<dyn VectorIndex>),
        (4, other.clone() as Arc<dyn VectorIndex>),
    ]))
    .await;
    let remote = client(&served, 3);
    let (me, you) = (owner(10), owner(11));
    remote
        .put(memory(1), me, 2, &[vector([0.0, 1.0]), vector([1.0, 0.0])])
        .await
        .unwrap();
    remote
        .put(memory(2), me, 1, &[vector([1.0, 1.0])])
        .await
        .unwrap();
    remote
        .put(
            EmbeddableEntityRef::Goal(GoalId::new(Uuid::from_u128(3))),
            me,
            1,
            &[vector([1.0, 0.0])],
        )
        .await
        .unwrap();

    let query = vector([1.0, 0.0]);
    let wire = remote.nearest(&[me], &query, 10).await.unwrap();
    assert_eq!(wire, mine.nearest(&[me], &query, 10).await.unwrap());
    assert_eq!(wire.len(), 2, "a goal is no candidate");
    assert_eq!(
        remote.entries(None, 10).await.unwrap(),
        mine.entries(None, 10).await.unwrap()
    );
    let page = remote.entries(None, 1).await.unwrap();
    let rest = remote.entries(Some(&page[0].key), 10).await.unwrap();
    assert_eq!(page.len() + rest.len(), 3);

    remote.set_owner(&[key(2)], you).await.unwrap();
    remote.delete(&[key(1), key(99)]).await.unwrap();
    assert!(remote.nearest(&[me], &query, 10).await.unwrap().is_empty());
    let moved = remote.nearest(&[you], &query, 10).await.unwrap();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].memory_id, MemoryId::new(Uuid::from_u128(2)));
    let left: Vec<VectorEntry> = mine.entries(None, 10).await.unwrap();
    assert_eq!(left.len(), 2);
    assert_eq!(left[0].version, 1);
    assert!(other.entries(None, 10).await.unwrap().is_empty());
}

/// The router parses each body where it enters: an unknown store, a body
/// that is not the call's, and a width the store does not index are refused,
/// and the client reads a refusal as the index's, anything else as down.
#[tokio::test]
async fn the_router_refuses_unknown_stores_and_malformed_calls() {
    let served = serve(BTreeMap::from([(
        0,
        Arc::new(InMemoryVectorIndex::default()) as Arc<dyn VectorIndex>,
    )]))
    .await;
    let unknown = client(&served, 9)
        .nearest(&[owner(1)], &vector([1.0, 0.0]), 5)
        .await;
    assert!(
        matches!(unknown, Err(StorageError::Unavailable(_))),
        "{unknown:?}"
    );

    let http = reqwest::Client::new();
    let post = |path: &str, body: &'static str| {
        http.post(format!("{}/v1/stores/0/{path}", served.base))
            .header("content-type", "application/json")
            .body(body)
            .send()
    };
    let status = |response: reqwest::Response| response.status().as_u16();
    assert_eq!(
        status(post("nearest", "{\"owners\":[]}").await.unwrap()),
        400
    );
    assert_eq!(
        status(
            post(
                "delete",
                "{\"keys\":[{\"entity_id\":\"00000000-0000-0000-0000-000000000001\",\"model_id\":\"m\",\"dim\":385}]}"
            )
            .await
            .unwrap()
        ),
        400
    );
    assert_eq!(
        status(
            post(
                "nearest",
                "{\"owners\":[],\"model_id\":\"m\",\"dim\":384,\"query\":[1.0],\"window\":1}"
            )
            .await
            .unwrap()
        ),
        400,
        "a query not of its space's width"
    );
    let empty = client(&served, 0).put(memory(1), owner(1), 1, &[]).await;
    assert!(
        matches!(empty, Err(StorageError::ConstraintViolation(_))),
        "{empty:?}"
    );
}

/// An index whose answers break the port's promises.
struct Lying {
    candidates: Vec<VectorCandidate>,
    entries: Vec<VectorEntry>,
}

#[async_trait::async_trait]
impl VectorIndex for Lying {
    async fn put(
        &self,
        _: EmbeddableEntityRef,
        _: OwnerId,
        _: i32,
        _: &[SpaceVector],
    ) -> Result<(), StorageError> {
        Err(StorageError::Internal("disk full".into()))
    }

    async fn delete(&self, _: &[VectorKey]) -> Result<(), StorageError> {
        Ok(())
    }

    async fn set_owner(&self, _: &[VectorKey], _: OwnerId) -> Result<(), StorageError> {
        Ok(())
    }

    async fn nearest(
        &self,
        _: &[OwnerId],
        _: &SpaceVector,
        _: u32,
    ) -> Result<Vec<VectorCandidate>, StorageError> {
        Ok(self.candidates.clone())
    }

    async fn entries(
        &self,
        _: Option<&VectorKey>,
        _: u32,
    ) -> Result<Vec<VectorEntry>, StorageError> {
        Ok(self.entries.clone())
    }
}

/// The client parses each answer where it enters the store: more candidates
/// than the window, a score outside `0..=1`, and entries out of key order or
/// past the limit are the index failing, not results.
#[tokio::test]
async fn the_client_refuses_answers_that_break_the_port() {
    let candidate = |score| VectorCandidate {
        memory_id: MemoryId::new(Uuid::now_v7()),
        similarity_score: score,
    };
    let entry = |id| VectorEntry {
        key: key(id),
        owner: owner(1),
        version: 1,
    };
    let lying = |candidates, entries| {
        Arc::new(Lying {
            candidates,
            entries,
        }) as Arc<dyn VectorIndex>
    };
    let served = serve(BTreeMap::from([
        (
            1,
            lying(
                vec![candidate(0.5), candidate(0.4)],
                vec![entry(2), entry(1)],
            ),
        ),
        (2, lying(vec![candidate(1.5)], vec![entry(1), entry(2)])),
    ]))
    .await;
    let query = vector([1.0, 0.0]);
    let (first, second) = (client(&served, 1), client(&served, 2));
    let unavailable =
        |result: Result<_, StorageError>| matches!(result, Err(StorageError::Unavailable(_)));
    assert!(
        unavailable(first.nearest(&[owner(1)], &query, 1).await.map(drop)),
        "past the window"
    );
    assert!(first.nearest(&[owner(1)], &query, 2).await.is_ok());
    assert!(
        unavailable(second.nearest(&[owner(1)], &query, 5).await.map(drop)),
        "score above 1"
    );
    assert!(
        unavailable(first.entries(None, 5).await.map(drop)),
        "out of key order"
    );
    assert!(
        unavailable(second.entries(None, 1).await.map(drop)),
        "past the limit"
    );
    assert!(
        unavailable(second.entries(Some(&key(1)), 5).await.map(drop)),
        "not after the cursor"
    );
    assert!(second.entries(None, 5).await.is_ok());
    assert!(
        unavailable(first.put(memory(1), owner(1), 1, &[query]).await),
        "an index failure is the index down"
    );
}

/// Every call carries memory vectors, so the client takes plaintext only to
/// loopback or a named host.
#[test]
fn the_client_takes_plaintext_only_to_loopback_or_a_named_host() {
    let none = PlaintextHosts::none();
    assert!(VectorIndexClient::new("https://index.example", 0, &none).is_ok());
    assert!(VectorIndexClient::new("http://127.0.0.1:9", 0, &none).is_ok());
    assert!(VectorIndexClient::new("http://index.example", 0, &none).is_err());
    assert!(VectorIndexClient::new("ftp://index.example", 0, &none).is_err());
    assert!(VectorIndexClient::new("not a url", 0, &none).is_err());
    let named = PlaintextHosts::parse("index.internal").unwrap();
    assert!(VectorIndexClient::new("http://index.internal:8080/base", 0, &named).is_ok());
}
