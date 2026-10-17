use super::Engine;
use crate::access::AccessKind;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::read_models::MemorySchemaSpec;
use crate::storage_ports::QueryStoragePorts;
use crate::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeReadRequest, EdgeReadResponse, MemoryLineageRequest,
    MemoryLineageResponse, QueryCursor, QueryRequest, QueryResponse, SidecarAtom,
};
use crate::verbs::schema::{SchemaRequest, SchemaResponse};
use crate::{OwnerId, OwnerScope, SchemaId};

impl Engine {
    /// docs/protocol.md
    /// default. Owner is not consulted.
    #[must_use]
    pub fn schema(&self, req: &SchemaRequest) -> SchemaResponse {
        self.registry.handle(req)
    }

    /// docs/protocol.md
    /// set (`S_read`). Caller passes the transport-extracted authorization
    /// context; the engine resolves the readable owners from it and filters
    /// results to `row owner ∈ S_read`.
    /// The request carries filters only; the engine supplies the complete
    /// readable owner set resolved from `authz` to storage.
    ///
    /// Heads-only requests need no per-schema natural-key filter: a head
    /// is the latest `t` on a `handle`, and `FactPayload`
    /// natural-key columns already decide which `handle` an ingest lands
    /// on (docs/build-a-flavor.md).
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context authorizes no reads (a denied
    /// context authorizes nothing), `InvalidArgument` when `req.limit == 0`,
    /// or `Internal` when the storage query fails. A read set the host vetoes
    /// down to nothing answers an empty page, not an error.
    pub async fn query(
        &self,
        authz: &AuthzContext,
        req: &QueryRequest,
    ) -> Result<QueryResponse, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        let schemas = self.memory_schema_specs();
        query_authorized(
            &self.storage.query,
            authz.owner_scope(),
            &schemas,
            &read_owners,
            req,
        )
        .await
    }

    /// The read access set (`S_read`) this context resolves to, after the
    /// host read vetoes: a vetoed owner is not in it. The set can be empty
    /// when every readable owner is vetoed; a pushdown scan over an empty
    /// set reads nothing.
    ///
    /// Every read verb above narrows to this set before it touches storage.
    /// A flavor that runs its own candidate scan needs the same set as a SQL
    /// predicate rather than as a post-filter: `proxima_*.projection` carries
    /// a composite `gin(owner_id, search_tsv)`, and only an owner predicate on
    /// the projection itself lets that index serve the scan. Handing the set
    /// out is not a widening — it is the identical set the engine would have
    /// applied afterwards, applied earlier. The flavor still admits its
    /// candidates through [`Engine::query`]; pushing the owner down narrows
    /// what the scan reads, never what the caller may see.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context authorizes no reads (a denied
    /// context authorizes nothing). An all-vetoed set is `Ok` and empty.
    pub async fn authorized_read_owners(
        &self,
        authz: &AuthzContext,
    ) -> Result<Vec<OwnerId>, ProtocolError> {
        self.authorize_read(authz).await
    }

    /// Edge read scoped to the context's read access set (`S_read`). Same
    /// auth shape as `Query`; callers narrow by kind and/or endpoint.
    /// Edges are source-owned: an edge is visible iff its source is
    /// readable; an unreadable target is rendered as
    /// `EdgeTargetProjection::Redacted` without id/kind leakage.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context authorizes no reads,
    /// `InvalidArgument` when `req.limit == 0`, or `Internal` when storage
    /// fails. An all-vetoed read set answers no edges, not an error.
    pub async fn read_edges(
        &self,
        authz: &AuthzContext,
        req: &EdgeReadRequest,
    ) -> Result<EdgeReadResponse, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        read_edges_authorized(&self.storage.query, authz.owner_scope(), &read_owners, req).await
    }

    /// Edge existence probe scoped to the context's read set (`S_read`), same
    /// source-owned visibility as `read_edges`: existence is disclosed only for
    /// edges whose source is readable.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context authorizes no reads, or `Internal`
    /// when storage fails. An all-vetoed read set answers `false`.
    pub async fn edge_exists(
        &self,
        authz: &AuthzContext,
        req: &EdgeExistsRequest,
    ) -> Result<EdgeExistsResponse, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        edge_exists_authorized(&self.storage.query, authz.owner_scope(), &read_owners, req).await
    }

    /// Provenance/Supersession lineage walk from one memory.
    /// Same auth shape as `Query`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context authorizes no reads, or
    /// `Internal` when storage fails. An all-vetoed read set walks nothing.
    pub async fn walk_memory_lineage(
        &self,
        authz: &AuthzContext,
        req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, ProtocolError> {
        let read_owners = self.authorize_read(authz).await?;
        walk_memory_lineage_authorized(&self.storage.query, authz.owner_scope(), &read_owners, req)
            .await
    }

    /// Current owned series handle whose sidecar matches `columns`.
    ///
    /// Owner-only. A transferred series is a miss for the prior
    /// owner. Flavor code must not JOIN `memory_head` to answer this.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the context reads nothing on `owner`,
    /// `InvalidArgument` when the column list is empty or an identifier
    /// is invalid, and `Internal` on storage failure.
    pub async fn owned_series_handle(
        &self,
        authz: &AuthzContext,
        owner: OwnerId,
        schema_id: &SchemaId,
        sidecar_table: &str,
        columns: &[(&str, SidecarAtom)],
    ) -> Result<Option<uuid::Uuid>, ProtocolError> {
        self.authorize_owner_read(authz, &owner, AccessKind::Fact)
            .await?;
        self.storage
            .query
            .memory_read
            .owned_series_handle(
                authz.owner_scope(),
                owner,
                schema_id,
                sidecar_table,
                columns,
            )
            .await
            .map_err(|err| match err {
                crate::StorageError::ConstraintViolation(message) => {
                    crate::error::ProtocolError::invalid_argument("columns", message)
                }
                other => super::errors::internal_storage_error("owned_series_handle", &other),
            })
    }
}

pub(in crate::engine) async fn query_authorized(
    ports: &QueryStoragePorts,
    owner_scope: Option<&OwnerScope>,
    schemas: &[MemorySchemaSpec],
    read_owners: &[OwnerId],
    req: &QueryRequest,
) -> Result<QueryResponse, ProtocolError> {
    if req.limit == 0 {
        return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
    }
    validate_query_cursor(req)?;
    validate_goal_selectors(req)?;
    if read_owners.is_empty() {
        return Ok(QueryResponse {
            memories: Vec::new(),
            goals: Vec::new(),
            edges: Vec::new(),
            next_cursor: None,
        });
    }
    ports
        .memory_read
        .query_memories(owner_scope, read_owners, req, schemas)
        .await
        .map_err(|e| ProtocolError::internal(e.to_string()))
}

fn validate_query_cursor(req: &QueryRequest) -> Result<(), ProtocolError> {
    let Some(cursor) = &req.page.after else {
        return Ok(());
    };

    match req.entity_kind {
        None => Err(ProtocolError::invalid_argument(
            "page.after",
            "cursor requires a single entity_kind",
        )),
        Some(crate::EntityKind::Goal) if matches!(cursor, QueryCursor::Goal { .. }) => Ok(()),
        Some(
            crate::EntityKind::Fact
            | crate::EntityKind::Abstraction
            | crate::EntityKind::Perspective,
        ) if matches!(cursor, QueryCursor::Memory { .. }) => Ok(()),
        Some(_) => Err(ProtocolError::invalid_argument(
            "page.after",
            "cursor kind does not match entity_kind",
        )),
    }
}

/// A Goal selector filters one stream. Anywhere else the store would ignore
/// it and return a page wider than the caller asked for.
fn validate_goal_selectors(req: &QueryRequest) -> Result<(), ProtocolError> {
    if !req.goal_handles.is_empty() && req.entity_kind != Some(crate::EntityKind::Goal) {
        return Err(ProtocolError::invalid_argument(
            "goal_handles",
            "requires entity_kind Goal",
        ));
    }
    if !req.referenced_goal_handles.is_empty() && req.entity_kind != Some(crate::EntityKind::Fact) {
        return Err(ProtocolError::invalid_argument(
            "referenced_goal_handles",
            "requires entity_kind Fact",
        ));
    }
    Ok(())
}

pub(in crate::engine) async fn read_edges_authorized(
    ports: &QueryStoragePorts,
    owner_scope: Option<&OwnerScope>,
    read_owners: &[OwnerId],
    req: &EdgeReadRequest,
) -> Result<EdgeReadResponse, ProtocolError> {
    if req.limit == 0 {
        return Err(ProtocolError::invalid_argument("limit", "must be > 0"));
    }
    if read_owners.is_empty() {
        return Ok(EdgeReadResponse {
            edges: Vec::new(),
            next_cursor: None,
        });
    }
    super::pin_read::read_edges_from_nodes(&ports.memory_read, owner_scope, read_owners, req).await
}

pub(in crate::engine) async fn edge_exists_authorized(
    ports: &QueryStoragePorts,
    owner_scope: Option<&OwnerScope>,
    read_owners: &[OwnerId],
    req: &EdgeExistsRequest,
) -> Result<EdgeExistsResponse, ProtocolError> {
    if read_owners.is_empty() {
        return Ok(EdgeExistsResponse { exists: false });
    }
    super::pin_read::edge_exists_from_nodes(&ports.memory_read, owner_scope, read_owners, req).await
}

pub(in crate::engine) async fn walk_memory_lineage_authorized(
    ports: &QueryStoragePorts,
    owner_scope: Option<&OwnerScope>,
    read_owners: &[OwnerId],
    req: &MemoryLineageRequest,
) -> Result<MemoryLineageResponse, ProtocolError> {
    if read_owners.is_empty() {
        return Ok(MemoryLineageResponse {
            nodes: Vec::new(),
            edges: Vec::new(),
            truncated: false,
            next_cursor: None,
            external_edges: Vec::new(),
        });
    }
    ports
        .memory_read
        .walk_memory_lineage(owner_scope, read_owners, req)
        .await
        .map_err(|e| ProtocolError::internal(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::validate_goal_selectors;
    use crate::verbs::query::QueryRequest;
    use crate::{EntityKind, ErrorCode};

    fn request(entity_kind: Option<EntityKind>) -> QueryRequest {
        let mut req = QueryRequest::readable();
        req.entity_kind = entity_kind;
        req
    }

    #[test]
    fn goal_handles_select_only_the_goal_stream() {
        for kind in [
            None,
            Some(EntityKind::Fact),
            Some(EntityKind::Abstraction),
            Some(EntityKind::Perspective),
        ] {
            let mut req = request(kind);
            req.goal_handles = vec![uuid::Uuid::now_v7()];
            let err = validate_goal_selectors(&req).expect_err("refused outside Goal");
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{kind:?}");
            assert!(err.message.contains("goal_handles"), "{}", err.message);
        }
        let mut req = request(Some(EntityKind::Goal));
        req.goal_handles = vec![uuid::Uuid::now_v7()];
        validate_goal_selectors(&req).expect("accepted on the Goal stream");
    }

    #[test]
    fn referenced_goal_handles_select_only_the_fact_stream() {
        for kind in [
            None,
            Some(EntityKind::Goal),
            Some(EntityKind::Abstraction),
            Some(EntityKind::Perspective),
        ] {
            let mut req = request(kind);
            req.referenced_goal_handles = vec![uuid::Uuid::now_v7()];
            let err = validate_goal_selectors(&req).expect_err("refused outside Fact");
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{kind:?}");
            assert!(
                err.message.contains("referenced_goal_handles"),
                "{}",
                err.message
            );
        }
        let mut req = request(Some(EntityKind::Fact));
        req.referenced_goal_handles = vec![uuid::Uuid::now_v7()];
        validate_goal_selectors(&req).expect("accepted on the Fact stream");
    }

    #[test]
    fn requests_without_goal_selectors_pass() {
        validate_goal_selectors(&request(None)).expect("no selector, no constraint");
    }
}
