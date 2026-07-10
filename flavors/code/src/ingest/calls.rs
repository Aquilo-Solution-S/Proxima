use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::{EdgeAuthorshipKind, EdgeId, MemoryId, Owner};
use proxima_storage_pg::verbs::edge_write::{
    MemoryEndpoint, append_owner_checked_typed_memory_edge,
};
use sqlx::PgPool;

use super::IngestError;
use super::schemas::schema_registry;
use crate::payloads::EdgeCallsV1;

/// Typed payload for one derived `code/calls` edge: caller + callee
/// code-slice Abstractions, callsite byte range, and callee identifier
/// metadata.
///
/// `callsite_byte_start_in_source_chunk` is the offset of the call
/// expression *within the source chunk's text* (not file-level). It
/// participates in the deterministic `edge_id` derivation so the same
/// caller→callee call site collapses to a single edge across commits
/// where the chunk's content is unchanged but its file-level byte
/// position has shifted (a sibling above changed). The file-level
/// `callsite_byte_start` / `callsite_byte_end` are stored in the
/// sidecar for first-observation context but do not contribute to
/// edge identity.
#[derive(Debug, Clone)]
pub struct CallEdgeDraft {
    pub source_memory_id: uuid::Uuid,
    pub target_memory_id: uuid::Uuid,
    pub callsite_byte_start: u32,
    pub callsite_byte_end: u32,
    pub callsite_byte_start_in_source_chunk: u32,
    pub callee_name: String,
    pub is_dynamic: bool,
}

/// Stable namespace UUID for deterministic `proxima-code` edge_ids.
/// Combined with the natural-key bytes via `Uuid::new_v5`, this
/// produces an `edge_id` that's identical across re-ingests of the
/// same call site — the substrate's `ON CONFLICT (edge_id) DO
/// NOTHING` then drops the duplicate without firing a duplicate
/// `EdgeAppend` change_event.
const PROXIMA_CODE_EDGE_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0xb8, 0xe7, 0xf8, 0xd2, 0x7c, 0x4f, 0x4f, 0x5a, 0x9e, 0x3a, 0x4d, 0x2b, 0x1e, 0x9f, 0x0a, 0x3c,
]);

/// Derive the natural-key bytes for a `proxima-code/calls` edge.
/// Components: owner kind / stable key id, the relation string, both
/// endpoint memory ids, and the **chunk-relative** callsite byte
/// start. File-level offsets are deliberately omitted so the key is
/// stable when chunk content is stable but the chunk has shifted in
/// the file.
fn calls_edge_natural_key(
    owner: &Owner,
    source_memory_id: uuid::Uuid,
    target_memory_id: uuid::Uuid,
    callsite_byte_start_in_source_chunk: u32,
) -> Vec<u8> {
    let mut k = Vec::with_capacity(128);
    let kind = proxima_core::OwnerRefKind::of(owner);
    let owner_key_id = owner.stable_key_uuid();
    k.extend_from_slice(kind.as_str().as_bytes());
    k.push(0);
    k.extend_from_slice(owner_key_id.as_bytes());
    k.push(0);
    k.extend_from_slice(b"proxima-code/calls");
    k.push(0);
    k.extend_from_slice(source_memory_id.as_bytes());
    k.push(0);
    k.extend_from_slice(target_memory_id.as_bytes());
    k.push(0);
    k.extend_from_slice(&callsite_byte_start_in_source_chunk.to_be_bytes());
    k
}

/// Deterministic `edge_id` for a `proxima-code/calls` edge: the v5 of
/// the natural key under [`PROXIMA_CODE_EDGE_NAMESPACE`]. Org-free
/// Org-free: the key folds the owner *principal* only.
fn calls_edge_id(
    owner: &Owner,
    source_memory_id: uuid::Uuid,
    target_memory_id: uuid::Uuid,
    callsite_byte_start_in_source_chunk: u32,
) -> uuid::Uuid {
    let key = calls_edge_natural_key(
        owner,
        source_memory_id,
        target_memory_id,
        callsite_byte_start_in_source_chunk,
    );
    uuid::Uuid::new_v5(&PROXIMA_CODE_EDGE_NAMESPACE, &key)
}

/// Atomic Engine-authored edge + typed sidecar write for derived
/// `code/calls` edges.
///
/// `edge_id` is derived deterministically from the natural key
/// (owner ‖ relation ‖ source_memory_id ‖ target_memory_id ‖
/// chunk-relative callsite offset). Replays over the same source file
/// revision produce the same derived code-slice ids and therefore the
/// same call edge id.
pub async fn ingest_calls_edge(
    pool: &PgPool,
    permit: &OwnerWritePermit,
    edge: &CallEdgeDraft,
) -> Result<(), IngestError> {
    let owner = permit.owner();
    let registry = schema_registry();
    let relation = registry
        .resolve_relation("proxima-code/calls")
        .ok_or_else(|| {
            IngestError::Storage("missing registered relation proxima-code/calls".into())
        })?;

    let edge_id = calls_edge_id(
        owner,
        edge.source_memory_id,
        edge.target_memory_id,
        edge.callsite_byte_start_in_source_chunk,
    );

    let payload = EdgeCallsV1 {
        callsite_byte_start: edge.callsite_byte_start,
        callsite_byte_end: edge.callsite_byte_end,
        callee_name: edge.callee_name.clone(),
        is_dynamic: edge.is_dynamic,
    };

    let mut tx = pool.begin().await?;
    append_owner_checked_typed_memory_edge(
        tx.as_mut(),
        permit,
        EdgeId::new(edge_id),
        relation,
        MemoryEndpoint::abstraction(MemoryId::new(edge.source_memory_id)),
        MemoryEndpoint::abstraction(MemoryId::new(edge.target_memory_id)),
        EdgeAuthorshipKind::Engine,
        Some(MemoryId::new(edge.source_memory_id)),
        &payload,
    )
    .await?;
    tx.commit().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::calls_edge_id;
    use proxima_core::{OwnerRef, UserId};
    use uuid::Uuid;

    /// Pins the org-free call-edge `edge_id` against drift. The natural key folds
    /// the owner *principal* ‖ relation ‖ endpoints
    /// ‖ chunk-relative callsite — no org. A fixed input must reproduce
    /// exactly this uuid so re-ingested call sites dedup by `edge_id`.
    #[test]
    fn calls_edge_id_golden_is_org_free() {
        let owner = OwnerRef::Personal(UserId::new(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid literal"),
        ));
        let source = Uuid::parse_str("00000000-0000-0000-0000-0000000000aa").expect("uuid literal");
        let target = Uuid::parse_str("00000000-0000-0000-0000-0000000000bb").expect("uuid literal");
        let id = calls_edge_id(&owner, source, target, 7);
        assert_eq!(
            id,
            Uuid::parse_str("e375387a-61f6-5ae2-b852-0c14ab4741f4").expect("uuid literal")
        );
    }
}
