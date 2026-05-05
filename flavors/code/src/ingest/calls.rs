use proxima_core::Owner;
use sqlx::PgPool;

use super::IngestError;
use super::schemas::schema_registry;

/// Typed payload for one `code/calls` edge: caller + callee chunk
/// memories, callsite byte range, and callee identifier metadata.
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
/// Components: owner principal kind / id / org-id, the relation
/// string, both endpoint memory ids, and the **chunk-relative**
/// callsite byte start. File-level offsets are deliberately omitted
/// so the key is stable when chunk content is stable but the chunk
/// has shifted in the file.
fn calls_edge_natural_key(
    owner: &Owner,
    source_memory_id: uuid::Uuid,
    target_memory_id: uuid::Uuid,
    callsite_byte_start_in_source_chunk: u32,
) -> Vec<u8> {
    use proxima_core::Principal;
    let mut k = Vec::with_capacity(128);
    let (kind, principal_id) = match &owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    k.extend_from_slice(kind.as_bytes());
    k.push(0);
    k.extend_from_slice(principal_id.as_bytes());
    k.push(0);
    k.extend_from_slice(owner.org_id.into_inner().as_bytes());
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

/// Atomic edge + typed sidecar write for `code/calls` edges.
/// Wraps `proxima_storage_pg::verbs::edge_append::append_edge_in_tx`.
///
/// `edge_id` is derived deterministically from the natural key
/// (owner ‖ relation ‖ source_memory_id ‖ target_memory_id ‖
/// chunk-relative callsite offset). Re-ingests of the same call site
/// produce the same `edge_id` and are dropped by the `ON CONFLICT`
/// guard inside `append_edge_in_tx` — sidecar + change_event are
/// gated on the edge insert actually returning a row.
pub async fn ingest_calls_edge(
    pool: &PgPool,
    owner: &Owner,
    edge: &CallEdgeDraft,
) -> Result<(), IngestError> {
    use proxima_storage_pg::verbs::edge_append::{EdgeDraft, append_edge_in_tx};

    let registry = schema_registry();
    let relation = registry
        .resolve_relation("proxima-code/calls")
        .ok_or_else(|| {
            IngestError::Storage("missing registered relation proxima-code/calls".into())
        })?;

    let key = calls_edge_natural_key(
        owner,
        edge.source_memory_id,
        edge.target_memory_id,
        edge.callsite_byte_start_in_source_chunk,
    );
    let edge_id = uuid::Uuid::new_v5(&PROXIMA_CODE_EDGE_NAMESPACE, &key);

    let payload = serde_json::json!({
        "callsite_byte_start": edge.callsite_byte_start,
        "callsite_byte_end": edge.callsite_byte_end,
        "callee_name": edge.callee_name,
        "is_dynamic": edge.is_dynamic,
    });

    let draft = EdgeDraft {
        edge_id,
        relation,
        source_kind: "Fact",
        source_memory_id: Some(edge.source_memory_id),
        source_goal_id: None,
        target_kind: "Fact",
        target_memory_id: Some(edge.target_memory_id),
        target_goal_id: None,
        authorship_kind: "EventSource",
        authorship_owner_memory_id: Some(edge.source_memory_id),
        owner,
    };

    let mut tx = pool.begin().await?;
    append_edge_in_tx(&mut tx, &draft, Some(&payload)).await?;
    tx.commit().await?;

    Ok(())
}
