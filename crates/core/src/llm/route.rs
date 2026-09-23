//! Which embedding endpoint serves each Owner.

use async_trait::async_trait;

use super::{BoundEmbeddingClient, EmbeddingClient, EmbeddingSpace, UnsupportedEmbeddingWidth};

/// Where one Owner's memories are embedded.
///
/// A route is the host's answer for the Owner whose memories are written or
/// searched — never the caller's. The engine embeds that Owner's texts and
/// queries only through the route's clients, so a route naming no client
/// means no jobs, no vectors and lexical-only search for the Owner.
///
/// A route has up to two clients. `current` embeds new memories inline and
/// serves search. `next`, set while the Owner moves to another model, is
/// queued for every new memory and filled by backfill; search stays on
/// `current` until the host flips the route to `current(next)`.
#[derive(Debug, Clone, Default)]
pub struct EmbeddingRoute {
    current: Option<BoundEmbeddingClient>,
    next: Option<BoundEmbeddingClient>,
}

impl EmbeddingRoute {
    /// No embeddings for this Owner.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            current: None,
            next: None,
        }
    }

    /// Embed and search this Owner's memories through `client`.
    #[must_use]
    pub const fn current(client: BoundEmbeddingClient) -> Self {
        Self {
            current: Some(client),
            next: None,
        }
    }

    /// Search through `current` while every memory is also embedded in
    /// `next`'s space. `current: None` starts an Owner that had no
    /// embeddings on `next` without serving semantic search yet.
    ///
    /// # Errors
    ///
    /// [`EmbeddingRouteError`] when both clients embed the same space: that
    /// is no move.
    pub fn moving(
        current: Option<BoundEmbeddingClient>,
        next: BoundEmbeddingClient,
    ) -> Result<Self, EmbeddingRouteError> {
        if current
            .as_ref()
            .is_some_and(|current| current.space() == next.space())
        {
            return Err(EmbeddingRouteError::new(format!(
                "a move needs a new embedding space; both clients embed {}",
                next.space()
            )));
        }
        Ok(Self {
            current,
            next: Some(next),
        })
    }

    /// The client that embeds new memories inline and search queries.
    #[must_use]
    pub const fn current_client(&self) -> Option<&BoundEmbeddingClient> {
        self.current.as_ref()
    }

    /// The client this Owner is moving to, if a move is under way.
    #[must_use]
    pub const fn next_client(&self) -> Option<&BoundEmbeddingClient> {
        self.next.as_ref()
    }

    /// Spaces a new memory of this Owner is queued for: `current`, then
    /// `next`.
    #[must_use]
    pub fn write_spaces(&self) -> Vec<EmbeddingSpace> {
        self.clients()
            .map(|client| client.space().clone())
            .collect()
    }

    /// Spaces a new memory of this Owner is queued for without an inline
    /// embed: `next`'s, while a move is under way.
    #[must_use]
    pub fn queued_spaces(&self) -> Vec<EmbeddingSpace> {
        self.next
            .iter()
            .map(|client| client.space().clone())
            .collect()
    }

    /// The client that embeds `space` for this Owner, or `None` when the
    /// route no longer names it: a job queued for such a space is stale.
    #[must_use]
    pub fn client_for(&self, space: &EmbeddingSpace) -> Option<&BoundEmbeddingClient> {
        self.clients().find(|client| client.space() == space)
    }

    fn clients(&self) -> impl Iterator<Item = &BoundEmbeddingClient> {
        self.current.iter().chain(self.next.iter())
    }

    /// The same route with every client wrapped by `wrap`, which is handed
    /// the client it wraps (the engine's request-timeout layer). Each
    /// binding keeps its space and origin, so wrapping cannot move a route.
    pub(crate) fn wrap_clients(
        self,
        wrap: impl Fn(std::sync::Arc<dyn EmbeddingClient>) -> std::sync::Arc<dyn EmbeddingClient>,
    ) -> Self {
        Self {
            current: self.current.map(|bound| bound.wrapped(&wrap)),
            next: self.next.map(|bound| bound.wrapped(&wrap)),
        }
    }
}

/// A route the host cannot resolve for an Owner.
///
/// Misconfiguration, not an outage: fetch credentials inside the client's
/// `embed`, where the drain retries. The engine never falls back to another
/// Owner's client or a default; it refuses the write, releases the Owner's
/// jobs, or drops the Owner's semantic arm.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct EmbeddingRouteError {
    message: String,
}

impl EmbeddingRouteError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Host-invoked maintenance reports a route it cannot resolve as an
/// internal storage failure; nothing was read or written.
impl From<EmbeddingRouteError> for crate::storage::StorageError {
    fn from(err: EmbeddingRouteError) -> Self {
        Self::Internal(format!("embedding route: {err}"))
    }
}

impl From<UnsupportedEmbeddingWidth> for EmbeddingRouteError {
    fn from(err: UnsupportedEmbeddingWidth) -> Self {
        Self::new(err.to_string())
    }
}

/// Host policy: which embedding endpoint serves each Owner.
///
/// Called for the Owner of the data on every write, drain batch and search,
/// so it must be a cheap lookup of host configuration.
#[async_trait]
pub trait EmbeddingRouter: Send + Sync + std::fmt::Debug {
    /// The route for memories `owner` owns.
    ///
    /// # Errors
    ///
    /// [`EmbeddingRouteError`] when the host cannot say, for this Owner.
    async fn route(&self, owner: &crate::Owner) -> Result<EmbeddingRoute, EmbeddingRouteError>;
}

/// One client for every Owner: the single-endpoint host.
#[derive(Debug, Clone)]
pub struct SingleClientRouter {
    client: BoundEmbeddingClient,
}

impl SingleClientRouter {
    #[must_use]
    pub const fn new(client: BoundEmbeddingClient) -> Self {
        Self { client }
    }

    /// Route every Owner to `client`, bound to its space.
    ///
    /// # Errors
    ///
    /// [`UnsupportedEmbeddingWidth`] when no lane indexes the client's width.
    pub fn bind(
        client: std::sync::Arc<dyn EmbeddingClient>,
    ) -> Result<Self, UnsupportedEmbeddingWidth> {
        BoundEmbeddingClient::bind(client).map(Self::new)
    }

    #[must_use]
    pub const fn client(&self) -> &BoundEmbeddingClient {
        &self.client
    }
}

#[async_trait]
impl EmbeddingRouter for SingleClientRouter {
    async fn route(&self, _owner: &crate::Owner) -> Result<EmbeddingRoute, EmbeddingRouteError> {
        Ok(EmbeddingRoute::current(self.client.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::super::LlmError;
    use super::EmbeddingClient;
    use async_trait::async_trait;

    #[derive(Debug)]
    struct Named(&'static str, usize);

    #[async_trait]
    impl EmbeddingClient for Named {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            Ok(vec![0.0; self.1])
        }

        fn model_id(&self) -> &str {
            self.0
        }

        fn dim(&self) -> usize {
            self.1
        }
    }

    fn named(model: &'static str, dim: usize) -> super::BoundEmbeddingClient {
        super::BoundEmbeddingClient::bind(std::sync::Arc::new(Named(model, dim))).expect("a lane")
    }

    #[test]
    fn a_moving_route_writes_both_spaces_and_searches_current() {
        let old = named("old", 768);
        let new = named("new", 1024);
        let route = super::EmbeddingRoute::moving(Some(old.clone()), new.clone()).expect("a move");
        assert_eq!(
            route.write_spaces(),
            vec![old.space().clone(), new.space().clone()]
        );
        assert!(route.current_client().is_some_and(|c| c.same_client(&old)));
        assert!(route.next_client().is_some_and(|c| c.same_client(&new)));
        assert!(
            route
                .client_for(new.space())
                .is_some_and(|c| c.same_client(&new))
        );
        assert!(
            route
                .client_for(old.space())
                .is_some_and(|c| c.same_client(&old))
        );

        let first = super::EmbeddingRoute::moving(None, new.clone()).expect("a first route");
        assert!(first.current_client().is_none(), "nothing to search yet");
        assert_eq!(first.write_spaces(), vec![new.space().clone()]);
    }

    #[test]
    fn a_move_needs_a_new_space() {
        // Same model at a new width is a move; the same space twice is not.
        let rewidth = super::EmbeddingRoute::moving(Some(named("m", 1024)), named("m", 768));
        assert!(rewidth.is_ok());
        let err = super::EmbeddingRoute::moving(Some(named("m", 1024)), named("m", 1024))
            .expect_err("no move");
        assert!(err.to_string().contains("new embedding space"), "{err}");
    }
}
