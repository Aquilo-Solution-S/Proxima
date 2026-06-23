use super::Engine;
use crate::storage::{AuthorDerivedOutcome, AuthorDerivedRequest, DerivedEdgeSpec, StorageError};
use crate::{
    CORE_SUPERSEDES_RELATION, EdgeAuthorshipKind, EntityKind, MemoryId, MemoryOperatorKind, Owner,
    PersonalityInstanceId, RegisteredRelation, SchemaId, SchemaVersion, SidecarPayload,
};

#[derive(Debug, Clone)]
pub struct AuthorDerivedEdgeInput<'a> {
    pub relation: RegisteredRelation<'a>,
    pub source_kind: EntityKind,
    pub source_memory_id: MemoryId,
    pub target_kind: EntityKind,
    pub target_memory_id: MemoryId,
    pub authorship_kind: EdgeAuthorshipKind,
    pub authorship_owner_memory_id: Option<MemoryId>,
}

#[derive(Debug)]
pub struct AuthorDerivedRequestInput<'a> {
    pub memory_id: MemoryId,
    pub owner: Owner,
    pub kind: EntityKind,
    pub text: String,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub operator_kind: MemoryOperatorKind,
    pub model_id: &'a str,
    pub prompt_version: &'a str,
    pub author_personality_instance_id: Option<PersonalityInstanceId>,
    pub sidecar_payload: SidecarPayload,
    /// Prior A/P memory superseded by this derived memory. The engine
    /// records both `memories.supersedes` and a same-transaction
    /// `core/supersedes` edge.
    pub supersedes: Option<MemoryId>,
    pub edges: &'a [AuthorDerivedEdgeInput<'a>],
}

impl Engine {
    /// Author one derived Memory and its already-resolved edges. When an
    /// embedding client is configured, the Engine embeds before storage;
    /// otherwise storage receives `None` and persists no embedding row.
    ///
    /// # Errors
    ///
    /// Returns `Internal` when the embedding client fails,
    /// `ConstraintViolation` on embedding dimension mismatch, and storage
    /// errors from the atomic write.
    pub async fn author_derived(
        &self,
        req: AuthorDerivedRequestInput<'_>,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        let (embedding, embedding_model_id) = if let Some(client) = self.embed_client() {
            let embedding = client
                .embed(&req.text)
                .await
                .map_err(|e| StorageError::Internal(format!("embed derived memory text: {e}")))?;
            if embedding.len() != client.dim() {
                return Err(StorageError::ConstraintViolation(format!(
                    "embedding dim mismatch: client dim {} but vector len {}",
                    client.dim(),
                    embedding.len(),
                )));
            }
            (Some(embedding), Some(client.model_id().to_string()))
        } else {
            (None, None)
        };

        let supersedes_relation = if req.supersedes.is_some() {
            Some(
                self.registry()
                    .resolve_relation(CORE_SUPERSEDES_RELATION)
                    .ok_or_else(|| {
                        StorageError::ConstraintViolation(format!(
                            "relation {CORE_SUPERSEDES_RELATION} not registered"
                        ))
                    })?,
            )
        } else {
            None
        };

        let owner = req.owner;
        let mut edges: Vec<DerivedEdgeSpec<'_>> = req
            .edges
            .iter()
            .map(|edge| DerivedEdgeSpec {
                owner: &owner,
                relation: edge.relation,
                source_kind: edge.source_kind,
                source_memory_id: edge.source_memory_id,
                target_kind: edge.target_kind,
                target_memory_id: edge.target_memory_id,
                authorship_kind: edge.authorship_kind,
                authorship_owner_memory_id: edge.authorship_owner_memory_id,
                sidecar_payload: None,
            })
            .collect();
        if let (Some(prior), Some(relation)) = (req.supersedes, supersedes_relation) {
            edges.push(DerivedEdgeSpec {
                owner: &owner,
                relation,
                source_kind: req.kind,
                source_memory_id: req.memory_id,
                target_kind: req.kind,
                target_memory_id: prior,
                authorship_kind: EdgeAuthorshipKind::Engine,
                authorship_owner_memory_id: None,
                sidecar_payload: None,
            });
        }
        let storage_req = AuthorDerivedRequest {
            memory_id: req.memory_id,
            owner: owner.clone(),
            kind: req.kind,
            text: req.text,
            schema_id: req.schema_id,
            schema_version: req.schema_version,
            operator_kind: req.operator_kind,
            model_id: req.model_id,
            prompt_version: req.prompt_version,
            author_personality_instance_id: req.author_personality_instance_id,
            sidecar_payload: req.sidecar_payload,
            supersedes: req.supersedes,
            embedding,
            embedding_model_id: embedding_model_id.as_deref(),
            edges: &edges,
        };
        self.storage().author_derived(&storage_req).await
    }
}
