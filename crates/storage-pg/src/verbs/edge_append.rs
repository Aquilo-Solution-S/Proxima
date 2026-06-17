#![allow(clippy::doc_markdown)]
//! Atomic edge + typed sidecar write.
//!
//! `append_edge_in_tx` inserts one `proxima_core.edges` row plus an
//! optional typed sidecar row (keyed on `edge_id`) plus the
//! `EdgeAppend` change_event row, all in a single transaction.
//!
//! Used by M5.5 typed F-layer edges (e.g. `proxima-code/calls`).

use proxima_core::{
    EdgeAuthorshipKind, EndpointBinding, EntityKind, Owner, RegisteredRelation, StorageError,
};

use crate::error::map_err;
use crate::pg_ident::PgIdent;

/// Draft of an edge to be written.
#[derive(Debug, Clone)]
pub struct EdgeDraft<'a> {
    pub edge_id: uuid::Uuid,
    pub relation: RegisteredRelation<'a>,
    pub source_kind: EntityKind,
    pub source_memory_id: Option<uuid::Uuid>,
    pub source_goal_id: Option<uuid::Uuid>,
    pub source_fact_entity_id: Option<uuid::Uuid>,
    pub target_kind: EntityKind,
    pub target_memory_id: Option<uuid::Uuid>,
    pub target_goal_id: Option<uuid::Uuid>,
    pub target_fact_entity_id: Option<uuid::Uuid>,
    pub authorship_kind: EdgeAuthorshipKind,
    pub authorship_owner_memory_id: Option<uuid::Uuid>,
    pub owner: &'a Owner,
}

/// Write an edge row + (optional) typed sidecar + the EdgeAppend
/// change_event in one transaction.
///
/// # Errors
///
/// Returns `ConstraintViolation` on FK / check failures; `Internal`
/// on sqlx failure.
pub async fn append_edge_in_tx(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
    payload: Option<&serde_json::Value>,
) -> Result<(), StorageError> {
    let sidecar_table = draft.relation.payload_sidecar_table;
    validate_edge_draft(draft, payload)?;

    if !insert_edge_row(tx, draft).await? {
        return Ok(());
    }

    // Sidecar SQL composed from validated identifier; can't be a macro.
    if let (Some(payload_json), Some(table)) = (payload, sidecar_table) {
        let table = PgIdent::table(table)?;

        let sidecar_sql = format!(
            "INSERT INTO {table} \
             SELECT * FROM jsonb_populate_record( \
                 NULL::{table}, \
                 ($1::jsonb || jsonb_build_object('edge_id', $2::uuid)) \
             )",
            table = table.as_str(),
        );
        sqlx::query(&sidecar_sql)
            .bind(payload_json)
            .bind(draft.edge_id)
            .execute(&mut *tx)
            .await
            .map_err(map_err)?;
    }

    append_edge_change_event(tx, draft).await
}

fn validate_edge_draft(
    draft: &EdgeDraft<'_>,
    payload: Option<&serde_json::Value>,
) -> Result<(), StorageError> {
    let descriptor = draft.relation.descriptor;
    let sidecar_table = draft.relation.payload_sidecar_table;
    match (sidecar_table, payload) {
        (Some(_), Some(_)) | (None, None) => {}
        (Some(_), None) => {
            return Err(StorageError::ConstraintViolation(format!(
                "missing EdgePayload for typed relation {}",
                descriptor.relation
            )));
        }
        (None, Some(_)) => {
            return Err(StorageError::ConstraintViolation(format!(
                "payload supplied for substrate relation {}",
                descriptor.relation
            )));
        }
    }
    if !exactly_one_endpoint(
        draft.source_memory_id,
        draft.source_goal_id,
        draft.source_fact_entity_id,
    ) {
        return Err(StorageError::ConstraintViolation(
            "source endpoint columns violate exactly-one invariant".into(),
        ));
    }
    if !exactly_one_endpoint(
        draft.target_memory_id,
        draft.target_goal_id,
        draft.target_fact_entity_id,
    ) {
        return Err(StorageError::ConstraintViolation(
            "target endpoint columns violate exactly-one invariant".into(),
        ));
    }
    descriptor
        .validate_edge_shape(
            draft.source_kind.as_str(),
            endpoint_binding(
                draft.source_memory_id,
                draft.source_goal_id,
                draft.source_fact_entity_id,
            ),
            draft.target_kind.as_str(),
            endpoint_binding(
                draft.target_memory_id,
                draft.target_goal_id,
                draft.target_fact_entity_id,
            ),
            draft.authorship_kind.as_str(),
        )
        .map_err(StorageError::ConstraintViolation)?;
    Ok(())
}

async fn insert_edge_row(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
) -> Result<bool, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = draft.owner.columns();
    let descriptor = draft.relation.descriptor;
    let inserted: Option<(uuid::Uuid,)> = sqlx::query_as(
        "INSERT INTO proxima_core.edges \
            (edge_id, relation, relation_class, \
             source_kind, source_memory_id, source_goal_id, source_fact_entity_id, \
             target_kind, target_memory_id, target_goal_id, target_fact_entity_id, \
             authorship_kind, authorship_owner_memory_id, \
             owner_principal_kind, owner_principal_id, owner_org_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
         ON CONFLICT (edge_id) DO NOTHING \
         RETURNING edge_id",
    )
    .bind(draft.edge_id)
    .bind(descriptor.relation.as_str())
    .bind(descriptor.class)
    .bind(draft.source_kind)
    .bind(draft.source_memory_id)
    .bind(draft.source_goal_id)
    .bind(draft.source_fact_entity_id)
    .bind(draft.target_kind)
    .bind(draft.target_memory_id)
    .bind(draft.target_goal_id)
    .bind(draft.target_fact_entity_id)
    .bind(draft.authorship_kind)
    .bind(draft.authorship_owner_memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;
    Ok(inserted.is_some())
}

async fn append_edge_change_event(
    tx: &mut sqlx::PgConnection,
    draft: &EdgeDraft<'_>,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = draft.owner.columns();
    let descriptor = draft.relation.descriptor;
    let seq = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event \
            (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind, \
             edge_id, edge_relation, \
             edge_source_memory_id, edge_source_goal_id, edge_source_fact_entity_id, \
             edge_target_memory_id, edge_target_goal_id, edge_target_fact_entity_id) \
         VALUES ($1, $2, $3, $4, 'EdgeAppend', $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(draft.edge_id)
    .bind(descriptor.relation.as_str())
    .bind(draft.source_memory_id)
    .bind(draft.source_goal_id)
    .bind(draft.source_fact_entity_id)
    .bind(draft.target_memory_id)
    .bind(draft.target_goal_id)
    .bind(draft.target_fact_entity_id)
    .execute(&mut *tx)
    .await
    .map_err(map_err)?;

    Ok(())
}

fn exactly_one_endpoint(
    memory: Option<uuid::Uuid>,
    goal: Option<uuid::Uuid>,
    fact_entity: Option<uuid::Uuid>,
) -> bool {
    usize::from(memory.is_some()) + usize::from(goal.is_some()) + usize::from(fact_entity.is_some())
        == 1
}

fn endpoint_binding(
    memory: Option<uuid::Uuid>,
    goal: Option<uuid::Uuid>,
    fact_entity: Option<uuid::Uuid>,
) -> EndpointBinding {
    if fact_entity.is_some() {
        EndpointBinding::FollowHead
    } else {
        debug_assert!(memory.is_some() || goal.is_some());
        EndpointBinding::Pin
    }
}
