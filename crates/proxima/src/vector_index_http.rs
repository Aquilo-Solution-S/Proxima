//! The network form of a store's vector index (#519): [`VectorIndexClient`]
//! is a [`VectorIndex`] a store calls over HTTP, and [`vector_index_router`]
//! serves any [`VectorIndex`] to the stores it names, so one host can back
//! several stores' indexes with the engine it chooses.
//!
//! The wire is JSON over `POST`, under the router's mount point:
//!
//! | Path | Body | Answer |
//! |---|---|---|
//! | `/v1/stores/{store}/put` | `{entity, owner, version, model_id, dim, vectors}` | `{}` |
//! | `/v1/stores/{store}/delete` | `{keys}` | `{}` |
//! | `/v1/stores/{store}/owner` | `{keys, owner}` | `{}` |
//! | `/v1/stores/{store}/nearest` | `{owners, model_id, dim, query, window}` | `{candidates: [{memory_id, score}]}` |
//! | `/v1/stores/{store}/entries` | `{after, limit}` | `{entries: [{entity_id, model_id, dim, owner, version}]}` |
//!
//! `entity` is `{"kind": "Fact" | "Abstraction" | "Perspective" | "Goal", "id": uuid}`;
//! a key is `{entity_id, model_id, dim}`; `vectors` and `query` are arrays of
//! numbers of the space's width. A refusal answers `400` with
//! `{"error": text}`, any other failure `503` without the index's own error,
//! and an unknown store `404`. A body over [`VECTOR_INDEX_BODY_LIMIT`] is
//! refused. The router authenticates no one: the host mounts it behind its
//! own middleware, and the client sends a bearer token when it has one.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use proxima_core::storage_ports::{VectorCandidate, VectorEntry, VectorIndex, VectorKey};
use proxima_core::{
    EmbeddableEntityRef, EmbeddingDim, EmbeddingSpace, EndpointUrlPolicy, EntityKind, GoalId,
    MemoryId, OwnerId, PlaintextHosts, SpaceVector, StorageError, validate_endpoint_url_for_hosts,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ProximaError;

/// Each index call's bound: a search waits on `nearest`, and a drain holds
/// its claim's transaction across `put`.
pub const VECTOR_INDEX_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest body [`vector_index_router`] reads: a `put` of one memory's
/// chunks at the widest space fits many times over, and the store sends at
/// most 500 keys per call.
pub const VECTOR_INDEX_BODY_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct WireEntity {
    kind: EntityKind,
    id: Uuid,
}

#[derive(Serialize, Deserialize)]
struct WireKey {
    entity_id: Uuid,
    model_id: String,
    dim: usize,
}

#[derive(Serialize, Deserialize)]
struct PutBody {
    entity: WireEntity,
    owner: Uuid,
    version: i32,
    model_id: String,
    dim: usize,
    vectors: Vec<Vec<f32>>,
}

#[derive(Serialize, Deserialize)]
struct DeleteBody {
    keys: Vec<WireKey>,
}

#[derive(Serialize, Deserialize)]
struct OwnerBody {
    keys: Vec<WireKey>,
    owner: Uuid,
}

#[derive(Serialize, Deserialize)]
struct NearestBody {
    owners: Vec<Uuid>,
    model_id: String,
    dim: usize,
    query: Vec<f32>,
    window: u32,
}

#[derive(Serialize, Deserialize)]
struct WireCandidate {
    memory_id: Uuid,
    score: f32,
}

#[derive(Serialize, Deserialize)]
struct NearestAnswer {
    candidates: Vec<WireCandidate>,
}

#[derive(Serialize, Deserialize)]
struct EntriesBody {
    after: Option<WireKey>,
    limit: u32,
}

/// An entry is its key's three fields beside its owner and version.
#[derive(Serialize, Deserialize)]
struct WireEntry {
    #[serde(flatten)]
    key: WireKey,
    owner: Uuid,
    version: i32,
}

#[derive(Serialize, Deserialize)]
struct EntriesAnswer {
    entries: Vec<WireEntry>,
}

#[derive(Serialize, Deserialize)]
struct Done {}

#[derive(Serialize, Deserialize)]
struct WireError {
    error: String,
}

fn invalid(message: impl Into<String>) -> StorageError {
    StorageError::ConstraintViolation(message.into())
}

fn space(model_id: String, dim: usize) -> Result<EmbeddingSpace, StorageError> {
    let dim = EmbeddingDim::from_width(dim)
        .ok_or_else(|| invalid(format!("width {dim} is not an index width")))?;
    Ok(EmbeddingSpace::new(model_id, dim))
}

fn space_vector(space: &EmbeddingSpace, values: Vec<f32>) -> Result<SpaceVector, StorageError> {
    SpaceVector::new(space.clone(), values).map_err(|error| invalid(error.to_string()))
}

fn key_to_wire(key: &VectorKey) -> WireKey {
    WireKey {
        entity_id: key.entity_id,
        model_id: key.space.model_id().to_owned(),
        dim: key.space.dim().width(),
    }
}

fn key_from_wire(key: WireKey) -> Result<VectorKey, StorageError> {
    Ok(VectorKey {
        entity_id: key.entity_id,
        space: space(key.model_id, key.dim)?,
    })
}

fn keys_to_wire(keys: &[VectorKey]) -> Vec<WireKey> {
    keys.iter().map(key_to_wire).collect()
}

fn keys_from_wire(keys: Vec<WireKey>) -> Result<Vec<VectorKey>, StorageError> {
    keys.into_iter().map(key_from_wire).collect()
}

fn entity_from_wire(entity: &WireEntity) -> EmbeddableEntityRef {
    match entity.kind {
        EntityKind::Goal => EmbeddableEntityRef::Goal(GoalId::new(entity.id)),
        kind @ (EntityKind::Fact | EntityKind::Abstraction | EntityKind::Perspective) => {
            EmbeddableEntityRef::Memory {
                kind,
                memory_id: MemoryId::new(entity.id),
            }
        }
    }
}

/// A store's [`VectorIndex`] at a remote host, through the network form.
pub struct VectorIndexClient {
    client: reqwest::Client,
    base: reqwest::Url,
    bearer: Option<String>,
}

impl std::fmt::Debug for VectorIndexClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorIndexClient")
            .field("origin", &self.base.origin().ascii_serialization())
            .field("has_bearer", &self.bearer.is_some())
            .finish_non_exhaustive()
    }
}

impl VectorIndexClient {
    /// The index of store `store` served at `base`: `https://`, or `http://`
    /// to loopback or a host named in `plaintext_hosts`, since every call
    /// carries memory vectors. Each call is bounded by
    /// [`VECTOR_INDEX_REQUEST_TIMEOUT`] and follows no redirect.
    ///
    /// # Errors
    ///
    /// [`ProximaError::Config`] for a URL that is not one of those.
    pub fn new(
        base: &str,
        store: u16,
        plaintext_hosts: &PlaintextHosts,
    ) -> Result<Self, ProximaError> {
        let mut base = reqwest::Url::parse(base)
            .map_err(|err| ProximaError::Config(format!("vector index URL: {err}")))?;
        match base.scheme() {
            "https" => {}
            "http" => validate_endpoint_url_for_hosts(
                base.as_str(),
                EndpointUrlPolicy::AllowLoopbackHttp,
                plaintext_hosts,
            )
            .map_err(|_| {
                ProximaError::Config(
                    "the vector index URL must be https:// unless its host is loopback or a \
                     named plaintext host: every call carries memory vectors"
                        .into(),
                )
            })?,
            other => {
                return Err(ProximaError::Config(format!(
                    "the vector index URL must be http(s), got scheme {other:?}"
                )));
            }
        }
        if base.host_str().is_none() {
            return Err(ProximaError::Config(
                "the vector index URL has no host".into(),
            ));
        }
        let path = format!("{}/v1/stores/{store}/", base.path().trim_end_matches('/'));
        base.set_path(&path);
        let client = reqwest::Client::builder()
            .timeout(VECTOR_INDEX_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| ProximaError::Config(format!("vector index client: {err}")))?;
        Ok(Self {
            client,
            base,
            bearer: None,
        })
    }

    /// Send `token` as `Authorization: Bearer <token>` on every call.
    #[must_use]
    pub fn with_bearer_token(mut self, token: String) -> Self {
        self.bearer = Some(token);
        self
    }

    async fn call<B: Serialize, A: for<'de> Deserialize<'de>>(
        &self,
        verb: &str,
        body: &B,
    ) -> Result<A, StorageError> {
        let url = self
            .base
            .join(verb)
            .map_err(|err| StorageError::Internal(format!("vector index URL: {err}")))?;
        let mut request = self.client.post(url).json(body);
        if let Some(token) = &self.bearer {
            request = request.bearer_auth(token);
        }
        let unavailable = |err: reqwest::Error| {
            StorageError::Unavailable(format!("vector index {verb}: {}", err.without_url()))
        };
        let response = request.send().await.map_err(unavailable)?;
        let status = response.status();
        if status.is_success() {
            return response.json().await.map_err(unavailable);
        }
        let reason = response
            .json::<WireError>()
            .await
            .map_or_else(|_| status.to_string(), |body| body.error);
        if status == StatusCode::BAD_REQUEST {
            return Err(invalid(format!("vector index {verb}: {reason}")));
        }
        Err(StorageError::Unavailable(format!(
            "vector index {verb}: {reason}"
        )))
    }
}

#[async_trait::async_trait]
impl VectorIndex for VectorIndexClient {
    async fn put(
        &self,
        entity: EmbeddableEntityRef,
        owner: OwnerId,
        version: i32,
        vectors: &[SpaceVector],
    ) -> Result<(), StorageError> {
        let space = vectors
            .first()
            .map(|vector| vector.space().clone())
            .ok_or_else(|| invalid("embedding must contain at least one chunk"))?;
        let body = PutBody {
            entity: WireEntity {
                kind: entity.entity_kind(),
                id: entity.entity_id(),
            },
            owner: owner.into_inner(),
            version,
            model_id: space.model_id().to_owned(),
            dim: space.dim().width(),
            vectors: vectors.iter().map(|v| v.values().to_vec()).collect(),
        };
        self.call::<_, Done>("put", &body).await.map(drop)
    }

    async fn delete(&self, keys: &[VectorKey]) -> Result<(), StorageError> {
        let body = DeleteBody {
            keys: keys_to_wire(keys),
        };
        self.call::<_, Done>("delete", &body).await.map(drop)
    }

    async fn set_owner(&self, keys: &[VectorKey], owner: OwnerId) -> Result<(), StorageError> {
        let body = OwnerBody {
            keys: keys_to_wire(keys),
            owner: owner.into_inner(),
        };
        self.call::<_, Done>("owner", &body).await.map(drop)
    }

    async fn nearest(
        &self,
        owners: &[OwnerId],
        query: &SpaceVector,
        window: u32,
    ) -> Result<Vec<VectorCandidate>, StorageError> {
        let body = NearestBody {
            owners: owners.iter().copied().map(OwnerId::into_inner).collect(),
            model_id: query.space().model_id().to_owned(),
            dim: query.space().dim().width(),
            query: query.values().to_vec(),
            window,
        };
        let answer: NearestAnswer = self.call("nearest", &body).await?;
        if answer.candidates.len() > usize::try_from(window).unwrap_or(usize::MAX) {
            return Err(StorageError::Unavailable(
                "vector index nearest answered past its window".into(),
            ));
        }
        answer
            .candidates
            .into_iter()
            .map(|candidate| {
                if !(0.0..=1.0).contains(&candidate.score) {
                    return Err(StorageError::Unavailable(
                        "vector index nearest answered a score outside 0..=1".into(),
                    ));
                }
                Ok(VectorCandidate {
                    memory_id: MemoryId::new(candidate.memory_id),
                    similarity_score: candidate.score,
                })
            })
            .collect()
    }

    async fn entries(
        &self,
        after: Option<&VectorKey>,
        limit: u32,
    ) -> Result<Vec<VectorEntry>, StorageError> {
        let body = EntriesBody {
            after: after.map(key_to_wire),
            limit,
        };
        let answer: EntriesAnswer = self.call("entries", &body).await?;
        let entries = answer
            .entries
            .into_iter()
            .map(|entry| {
                Ok(VectorEntry {
                    key: key_from_wire(entry.key)
                        .map_err(|error| StorageError::Unavailable(error.to_string()))?,
                    owner: OwnerId::new(entry.owner),
                    version: entry.version,
                })
            })
            .collect::<Result<Vec<_>, StorageError>>()?;
        // The sweep pages by the last key: an unordered answer would skip.
        let ordered = entries
            .windows(2)
            .all(|pair| pair[0].key.order() < pair[1].key.order())
            && after.is_none_or(|after| {
                entries
                    .first()
                    .is_none_or(|first| after.order() < first.key.order())
            });
        if !ordered || entries.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
            return Err(StorageError::Unavailable(
                "vector index entries answered out of key order or past its limit".into(),
            ));
        }
        Ok(entries)
    }
}

/// The indexes a [`vector_index_router`] serves, by store number.
type Indexes = Arc<BTreeMap<u16, Arc<dyn VectorIndex>>>;

/// The server half: serve each store's index in `indexes` on the wire the
/// [module documentation](self) describes. Mount it behind the host's own
/// authentication.
pub fn vector_index_router(indexes: BTreeMap<u16, Arc<dyn VectorIndex>>) -> axum::Router {
    axum::Router::new()
        .route("/v1/stores/{store}/put", axum::routing::post(put))
        .route("/v1/stores/{store}/delete", axum::routing::post(delete))
        .route("/v1/stores/{store}/owner", axum::routing::post(set_owner))
        .route("/v1/stores/{store}/nearest", axum::routing::post(nearest))
        .route("/v1/stores/{store}/entries", axum::routing::post(entries))
        .layer(axum::extract::DefaultBodyLimit::max(
            VECTOR_INDEX_BODY_LIMIT,
        ))
        .with_state(Arc::new(indexes))
}

fn json_response<A: Serialize>(status: StatusCode, body: &A) -> Response {
    match serde_json::to_vec(body) {
        Ok(bytes) => (
            status,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn refusal(status: StatusCode, error: String) -> Response {
    json_response(status, &WireError { error })
}

/// One call: the store's index, the body parsed where it enters, the
/// index's answer or failure on the wire.
async fn serve<B, A, F, Fut>(indexes: &Indexes, store: u16, body: &Bytes, call: F) -> Response
where
    B: for<'de> Deserialize<'de>,
    A: Serialize,
    F: FnOnce(Arc<dyn VectorIndex>, B) -> Fut,
    Fut: std::future::Future<Output = Result<A, StorageError>>,
{
    let Some(index) = indexes.get(&store).cloned() else {
        return refusal(StatusCode::NOT_FOUND, format!("no index for store {store}"));
    };
    let body = match serde_json::from_slice::<B>(body) {
        Ok(body) => body,
        Err(error) => return refusal(StatusCode::BAD_REQUEST, error.to_string()),
    };
    match call(index, body).await {
        Ok(answer) => json_response(StatusCode::OK, &answer),
        Err(StorageError::ConstraintViolation(error)) => refusal(StatusCode::BAD_REQUEST, error),
        Err(other) => {
            tracing::warn!(store, error = %other, "vector index call failed");
            refusal(
                StatusCode::SERVICE_UNAVAILABLE,
                "the index is unavailable".into(),
            )
        }
    }
}

async fn put(State(indexes): State<Indexes>, Path(store): Path<u16>, body: Bytes) -> Response {
    serve(&indexes, store, &body, |index, body: PutBody| async move {
        let space = space(body.model_id, body.dim)?;
        let vectors = body
            .vectors
            .into_iter()
            .map(|values| space_vector(&space, values))
            .collect::<Result<Vec<_>, _>>()?;
        index
            .put(
                entity_from_wire(&body.entity),
                OwnerId::new(body.owner),
                body.version,
                &vectors,
            )
            .await
            .map(|()| Done {})
    })
    .await
}

async fn delete(State(indexes): State<Indexes>, Path(store): Path<u16>, body: Bytes) -> Response {
    serve(
        &indexes,
        store,
        &body,
        |index, body: DeleteBody| async move {
            let keys = keys_from_wire(body.keys)?;
            index.delete(&keys).await.map(|()| Done {})
        },
    )
    .await
}

async fn set_owner(
    State(indexes): State<Indexes>,
    Path(store): Path<u16>,
    body: Bytes,
) -> Response {
    serve(
        &indexes,
        store,
        &body,
        |index, body: OwnerBody| async move {
            let keys = keys_from_wire(body.keys)?;
            index
                .set_owner(&keys, OwnerId::new(body.owner))
                .await
                .map(|()| Done {})
        },
    )
    .await
}

async fn nearest(State(indexes): State<Indexes>, Path(store): Path<u16>, body: Bytes) -> Response {
    serve(
        &indexes,
        store,
        &body,
        |index, body: NearestBody| async move {
            let query = space_vector(&space(body.model_id, body.dim)?, body.query)?;
            let owners: Vec<OwnerId> = body.owners.into_iter().map(OwnerId::new).collect();
            let candidates = index.nearest(&owners, &query, body.window).await?;
            Ok(NearestAnswer {
                candidates: candidates
                    .into_iter()
                    .map(|candidate| WireCandidate {
                        memory_id: candidate.memory_id.into_inner(),
                        score: candidate.similarity_score,
                    })
                    .collect(),
            })
        },
    )
    .await
}

async fn entries(State(indexes): State<Indexes>, Path(store): Path<u16>, body: Bytes) -> Response {
    serve(
        &indexes,
        store,
        &body,
        |index, body: EntriesBody| async move {
            let after = body.after.map(key_from_wire).transpose()?;
            let entries = index.entries(after.as_ref(), body.limit).await?;
            Ok(EntriesAnswer {
                entries: entries
                    .into_iter()
                    .map(|entry| WireEntry {
                        key: key_to_wire(&entry.key),
                        owner: entry.owner.into_inner(),
                        version: entry.version,
                    })
                    .collect(),
            })
        },
    )
    .await
}
