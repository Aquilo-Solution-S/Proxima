//! Engine composite — wires SchemaRegistry, MemoryStore, and
//! an AuthResolver behind the typed verb surfaces of
//! docs/14-protocol-surface.md.

use std::sync::Arc;

use crate::auth::{AuthResolver, Credentials};
use crate::error::ProtocolError;
use crate::storage::{NoopStorage, StorageHandle};
use crate::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use crate::verbs::query::{MemoryStore, QueryRequest, QueryResponse};
use crate::verbs::schema::{SchemaRegistry, SchemaRequest, SchemaResponse};

pub struct Engine {
    registry: SchemaRegistry,
    memories: MemoryStore,
    auth: Box<dyn AuthResolver>,
    storage: StorageHandle,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("registry", &self.registry)
            .field("memories", &self.memories)
            .field("auth", &"<dyn AuthResolver>")
            .field("storage", &"<dyn Storage>")
            .finish()
    }
}

impl Engine {
    pub fn new(
        registry: SchemaRegistry,
        memories: MemoryStore,
        auth: Box<dyn AuthResolver>,
    ) -> Self {
        Self {
            registry,
            memories,
            auth,
            storage: Arc::new(NoopStorage),
        }
    }

    #[must_use]
    pub fn with_storage(mut self, storage: StorageHandle) -> Self {
        self.storage = storage;
        self
    }

    /// docs/14 §"Schema" — binary-scoped, unauthenticated by
    /// default. Owner is not consulted.
    pub fn schema(&self, req: &SchemaRequest) -> SchemaResponse {
        self.registry.handle(req)
    }

    /// docs/14 §"Query" — Owner-scoped. Caller passes the
    /// transport-extracted credentials; engine resolves and
    /// gates `req.owner ∈ resolved.accessible_owners`.
    pub fn query(
        &self,
        creds: &Credentials,
        req: &QueryRequest,
    ) -> Result<QueryResponse, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.accessible_owners.contains(&req.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        Ok(self.memories.query(req))
    }

    /// docs/14 §"EventIngest" — Owner-scoped write. Validates
    /// schemas and delegates to storage.
    pub async fn event_ingest(
        &self,
        creds: &Credentials,
        draft: EventDraft,
    ) -> Result<EventIngestOutcome, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.accessible_owners.contains(&draft.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        // Three schema validations: fact, cited_object, citation_mapping.
        for (sid, ver) in [
            (&draft.schema_id, draft.schema_version),
            (
                &draft.cited_object.schema_id,
                draft.cited_object.schema_version,
            ),
            (
                &draft.citation_mapping.schema_id,
                draft.citation_mapping.schema_version,
            ),
        ] {
            if self.registry.lookup(sid, ver).is_none() {
                return Err(ProtocolError::unknown_schema(
                    sid.as_str(),
                    ver.into_inner(),
                ));
            }
        }
        self.storage
            .ingest_event_atomic(&draft)
            .await
            .map_err(|e| ProtocolError::internal(e.to_string()))
    }
}
