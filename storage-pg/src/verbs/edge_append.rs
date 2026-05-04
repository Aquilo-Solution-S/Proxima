#![allow(clippy::doc_markdown)]
//! Atomic edge + typed sidecar write.
//!
//! `append_edge_in_tx` inserts one `proxima_core.edges` row plus an
//! optional typed sidecar row (keyed on `edge_id`) plus the
//! `EdgeAppend` change_event row, all in a single transaction.
//!
//! Used by M5.5 typed F-layer edges (e.g. `proxima-code/calls`).

use proxima_core::{Owner, RelationClass, StorageError};

use crate::error::map_err;

/// Draft of an edge to be written. All fields map directly to
/// `proxima_core.edges` columns except `edge_id`, which is generated
/// by the caller (UUIDv7 per AGENTS.md invariant 17).
#[derive(Debug, Clone)]
pub struct EdgeDraft<'a> {
    pub edge_id: uuid::Uuid,
    pub relation: &'a str,
    pub class: RelationClass,
    pub source_kind: &'a str,
    pub source_memory_id: Option<uuid::Uuid>,
    pub source_goal_id: Option<uuid::Uuid>,
    pub target_kind: &'a str,
    pub target_memory_id: Option<uuid::Uuid>,
    pub target_goal_id: Option<uuid::Uuid>,
    pub authorship_kind: &'a str,
    pub authorship_owner_memory_id: Option<uuid::Uuid>,
    pub owner: &'a Owner,
}

/// Write an edge row + (optional) typed sidecar + the EdgeAppend
/// change_event in one transaction. The sidecar is written iff
/// `payload` is `Some` and `sidecar_table` is `Some`.
///
/// If `sidecar_table` is `None`, `payload` MUST be `None` (debug
/// assert). This mirrors the `RelationDescriptor.payload_schema =
/// None` substrate-only path.
///
/// # Errors
///
/// Returns `ConstraintViolation` on FK / check failures; `Internal`
/// on sqlx failure.
pub async fn append_edge_in_tx(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
    payload: Option<&serde_json::Value>,
    sidecar_table: Option<&str>,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(draft.owner);

    // Debug-assert: sidecar_table None implies payload None.
    debug_assert!(
        sidecar_table.is_some() || payload.is_none(),
        "sidecar_table is None but payload is Some"
    );

    // 1. Insert the edge row.
    sqlx::query(
        "INSERT INTO proxima_core.edges \
            (edge_id, relation, relation_class, \
             source_kind, source_memory_id, source_goal_id, \
             target_kind, target_memory_id, target_goal_id, \
             authorship_kind, authorship_owner_memory_id, \
             owner_principal_kind, owner_principal_id, owner_org_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(draft.edge_id)
    .bind(draft.relation)
    .bind(draft.class.as_str())
    .bind(draft.source_kind)
    .bind(draft.source_memory_id)
    .bind(draft.source_goal_id)
    .bind(draft.target_kind)
    .bind(draft.target_memory_id)
    .bind(draft.target_goal_id)
    .bind(draft.authorship_kind)
    .bind(draft.authorship_owner_memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    // 2. Insert typed sidecar if provided.
    if let (Some(payload_json), Some(table)) = (payload, sidecar_table) {
        // Validate table identifier (same guard as in consolidate.rs).
        validate_table_ident(table)?;

        let sidecar_sql = format!(
            "INSERT INTO {table} \
             SELECT * FROM jsonb_populate_record( \
                 NULL::{table}, \
                 ($1::jsonb || jsonb_build_object('edge_id', $2::uuid)) \
             )"
        );
        sqlx::query(&sidecar_sql)
            .bind(payload_json)
            .bind(draft.edge_id)
            .execute(&mut *tx)
            .await
            .map_err(map_err)?;
    }

    // 3. Insert EdgeAppend change_event.
    let seq = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event \
            (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind, \
             edge_id, edge_relation, \
             edge_source_kind, edge_source_memory_id, edge_source_goal_id, \
             edge_target_kind, edge_target_memory_id, edge_target_goal_id) \
         VALUES ($1, $2, $3, $4, 'EdgeAppend', $5, $6, \
                 $7, $8, $9, $10, $11, $12)",
    )
    .bind(seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(draft.edge_id)
    .bind(draft.relation)
    .bind(draft.source_kind)
    .bind(draft.source_memory_id)
    .bind(draft.source_goal_id)
    .bind(draft.target_kind)
    .bind(draft.target_memory_id)
    .bind(draft.target_goal_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    Ok(())
}

/// Reject identifiers that aren't a sane `schema.table` literal.
fn validate_table_ident(ident: &str) -> Result<(), StorageError> {
    let ok = !ident.is_empty()
        && ident
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if ok {
        Ok(())
    } else {
        Err(StorageError::ConstraintViolation(format!(
            "invalid sidecar table identifier: {ident}"
        )))
    }
}

fn owner_columns(owner: &Owner) -> (&'static str, uuid::Uuid, uuid::Uuid) {
    let (kind, principal_id) = match &owner.principal {
        proxima_core::Principal::User(u) => ("User", u.into_inner()),
        proxima_core::Principal::Group(g) => ("Group", g.into_inner()),
    };
    (kind, principal_id, owner.org_id.into_inner())
}
