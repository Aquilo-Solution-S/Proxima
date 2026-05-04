//! Engine composite — wires SchemaRegistry, MemoryStore, and
//! an AuthResolver behind the typed verb surfaces of
//! docs/14-protocol-surface.md.

use crate::auth::{AuthResolver, Credentials};
use crate::error::ProtocolError;
use crate::verbs::query::{MemoryStore, QueryRequest, QueryResponse};
use crate::verbs::schema::{SchemaRegistry, SchemaRequest, SchemaResponse};

pub struct Engine {
    registry: SchemaRegistry,
    memories: MemoryStore,
    auth: Box<dyn AuthResolver>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("registry", &self.registry)
            .field("memories", &self.memories)
            .field("auth", &"<dyn AuthResolver>")
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
        }
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
}
