use super::Engine;
use crate::storage::{AuthorDerivedOutcome, AuthorDerivedRequest, DerivedEdgeSpec, StorageError};
use crate::{
    EdgeAuthorshipKind, EntityKind, MemoryId, MemoryOperatorKind, Owner, PersonalityInstanceId,
    RegisteredRelation, SchemaId, SchemaVersion,
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
    pub sidecar_table: &'a str,
    pub sidecar_payload: serde_json::Value,
    pub edges: &'a [AuthorDerivedEdgeInput<'a>],
}

impl Engine {
    /// Author one derived Memory and its already-resolved edges. The
    /// Engine owns the embedding step so storage receives concrete data.
    ///
    /// # Errors
    ///
    /// Returns `Internal` when no embedding client is configured or the
    /// client fails, `ConstraintViolation` on embedding dimension
    /// mismatch, and storage errors from the atomic write.
    pub async fn author_derived(
        &self,
        req: AuthorDerivedRequestInput<'_>,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        let client = self.embed_client().ok_or_else(|| {
            StorageError::Internal("embedding client required for derived memory authoring".into())
        })?;
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

        let owner = req.owner;
        let edges: Vec<DerivedEdgeSpec<'_>> = req
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
                edge_payload: None,
            })
            .collect();
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
            sidecar_table: req.sidecar_table,
            sidecar_payload: req.sidecar_payload,
            embedding: Some(embedding),
            embedding_model_id: Some(client.model_id()),
            edges: &edges,
        };
        self.storage().author_derived(&storage_req).await
    }
}
