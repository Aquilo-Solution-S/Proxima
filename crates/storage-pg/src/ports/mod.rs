use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::{DerivedEdgeSpec, MemoryId, Owner, StorageError};

use crate::verbs;

mod compliance;
mod embeddings;
mod goals;
mod ingest;
mod memory;
mod owners;
mod read;
mod registry;

fn edge_draft_from_spec<'a>(edge: &'a DerivedEdgeSpec<'a>) -> verbs::edge_append::EdgeDraft<'a> {
    verbs::edge_append::EdgeDraft {
        edge_id: uuid::Uuid::now_v7(),
        relation: edge.relation,
        source_kind: edge.source_kind,
        source_memory_id: Some(edge.source_memory_id.into_inner()),
        source_goal_id: None,
        source_fact_entity_id: None,
        target_kind: edge.target_kind,
        target_memory_id: Some(edge.target_memory_id.into_inner()),
        target_goal_id: None,
        target_fact_entity_id: None,
        authorship_kind: edge.authorship_kind,
        authorship_owner_memory_id: edge.authorship_owner_memory_id.map(MemoryId::into_inner),
        owner: edge.owner,
    }
}

fn validate_permit_owner(permit: &OwnerWritePermit, owner: &Owner) -> Result<(), StorageError> {
    if permit.owner() == owner {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(
            "request owner does not match owner write permit".into(),
        ))
    }
}
