//! `GoalWrite` verb — `write_goal` and `supersede_goal`.
//!
//! The two paths share most of their structure (replay check, body
//! comparison, then goal + parents + `change_event` insert). The shape
//! is kept co-located here so the symmetry is visible; the small
//! `supersedes` delta sits at the call sites rather than behind an
//! abstraction.

use std::collections::HashSet;

use proxima_core::verbs::goal_write::{GoalDraft, GoalState, GoalWriteOutcome};
use proxima_core::{GoalId, Principal, StorageError};
use sqlx::PgPool;

use crate::authorship::{authorship_columns, check_authorship_match};
use crate::error::map_err;

/// Replay-check tuple read from `proxima_core.goals`:
/// `(schema_id, schema_version, title, text, state, parent_goal_ids, supersedes, payload)`.
type GoalBodyRow = (
    String,
    i32,
    String,
    String,
    String,
    Vec<uuid::Uuid>,
    Option<uuid::Uuid>,
    Vec<u8>,
);

#[allow(clippy::too_many_lines)]
pub(crate) async fn write_goal_atomic(
    pool: &PgPool,
    draft: &GoalDraft,
) -> Result<GoalWriteOutcome, StorageError> {
    let (owner_kind, owner_principal_id) = match &draft.owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let owner_org_id = draft.owner.org_id.into_inner();

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    // Replay check by (owner, request_id).
    // We need to join with change_event to get the seq.
    let existing: Option<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT g.goal_id, ce.seq \
         FROM proxima_core.goals g \
         JOIN proxima_core.change_event ce ON ce.entity_goal_id = g.goal_id \
         WHERE (g.owner_principal_kind, g.owner_principal_id, g.owner_org_id, g.request_id) \
           = ($1, $2, $3, $4) \
         ORDER BY ce.seq ASC LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&draft.request_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;

    if let Some((existing_goal_id, existing_seq)) = existing {
        // Compare the existing body with the draft.
        let existing_row: GoalBodyRow = sqlx::query_as(
            "SELECT schema_id, schema_version, title, text, state, \
                     COALESCE((SELECT array_agg(parent_goal_id) FROM proxima_core.goal_parents WHERE goal_id = $1), '{}'::uuid[]), \
                     supersedes, payload \
             FROM proxima_core.goals WHERE goal_id = $1",
        )
        .bind(existing_goal_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_err)?;

        let existing_parents: HashSet<uuid::Uuid> = existing_row.5.into_iter().collect();
        let draft_parents: HashSet<uuid::Uuid> = draft
            .parent_goal_ids
            .iter()
            .map(|g| g.into_inner())
            .collect();

        let state_str = goal_state_str(draft.state);

        // Check if all fields match.
        let schema_id_match = existing_row.0 == draft.schema_id.as_str();
        let schema_version_match =
            existing_row.1 == draft.schema_version.into_inner().cast_signed();
        let title_match = existing_row.2 == draft.title;
        let text_match = existing_row.3 == draft.text;
        let state_match = existing_row.4 == state_str;
        let parents_match = existing_parents == draft_parents;
        let expected_supersedes = draft.supersedes_goal_id.map(GoalId::into_inner);
        let supersedes_match = existing_row.6 == expected_supersedes;
        let payload_match = existing_row.7 == draft.payload;

        // Also need to check authorship fields.
        let authorship_matches = check_authorship_match(&mut tx, existing_goal_id, draft).await?;

        let body_matches = schema_id_match
            && schema_version_match
            && title_match
            && text_match
            && state_match
            && parents_match
            && supersedes_match
            && payload_match;

        if body_matches && authorship_matches {
            tx.commit().await.map_err(map_err)?;
            return Ok(GoalWriteOutcome {
                goal_id: proxima_core::GoalId::new(existing_goal_id),
                change_event_seq: existing_seq,
                idempotent_replay: true,
            });
        }
        return Err(StorageError::ConstraintViolation(format!(
            "idempotency_conflict:{}",
            draft.request_id
        )));
    }

    let supersedes = draft.supersedes_goal_id.map(GoalId::into_inner);
    if let Some(prior_id) = supersedes {
        validate_prior_goal_owner(&mut tx, prior_id, owner_kind, owner_principal_id).await?;
    }

    // Generate ids inside the tx.
    let goal_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();

    insert_goal_row(&mut tx, draft, goal_id, supersedes).await?;
    insert_goal_parents(&mut tx, draft, goal_id).await?;
    insert_goal_change_event(&mut tx, draft, goal_id, change_seq, supersedes).await?;

    tx.commit().await.map_err(map_err)?;

    Ok(GoalWriteOutcome {
        goal_id: proxima_core::GoalId::new(goal_id),
        change_event_seq: change_seq,
        idempotent_replay: false,
    })
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn supersede_goal_atomic(
    pool: &PgPool,
    prior: proxima_core::GoalId,
    draft: &GoalDraft,
) -> Result<GoalWriteOutcome, StorageError> {
    if let Some(draft_prior) = draft.supersedes_goal_id
        && draft_prior != prior
    {
        return Err(StorageError::ConstraintViolation(
            "draft supersedes_goal_id does not match prior".to_string(),
        ));
    }

    let (owner_kind, owner_principal_id) = match &draft.owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let owner_org_id = draft.owner.org_id.into_inner();

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    // Replay check by (owner, request_id) — same as write_goal.
    let existing: Option<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT g.goal_id, ce.seq \
         FROM proxima_core.goals g \
         JOIN proxima_core.change_event ce ON ce.entity_goal_id = g.goal_id \
         WHERE (g.owner_principal_kind, g.owner_principal_id, g.owner_org_id, g.request_id) \
           = ($1, $2, $3, $4) \
         ORDER BY ce.seq ASC LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&draft.request_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;

    if let Some((existing_goal_id, existing_seq)) = existing {
        // Compare the existing body with the draft (including supersedes = prior).
        let existing_row: GoalBodyRow = sqlx::query_as(
            "SELECT schema_id, schema_version, title, text, state, \
                     COALESCE((SELECT array_agg(parent_goal_id) FROM proxima_core.goal_parents WHERE goal_id = $1), '{}'::uuid[]), \
                     supersedes, payload \
             FROM proxima_core.goals WHERE goal_id = $1",
        )
        .bind(existing_goal_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_err)?;

        let existing_parents: HashSet<uuid::Uuid> = existing_row.5.into_iter().collect();
        let draft_parents: HashSet<uuid::Uuid> = draft
            .parent_goal_ids
            .iter()
            .map(|g| g.into_inner())
            .collect();

        let state_str = goal_state_str(draft.state);

        // Check if all fields match (including supersedes = prior).
        let schema_id_match = existing_row.0 == draft.schema_id.as_str();
        let schema_version_match =
            existing_row.1 == draft.schema_version.into_inner().cast_signed();
        let title_match = existing_row.2 == draft.title;
        let text_match = existing_row.3 == draft.text;
        let state_match = existing_row.4 == state_str;
        let parents_match = existing_parents == draft_parents;
        let supersedes_match = existing_row.6 == Some(prior.into_inner());
        let payload_match = existing_row.7 == draft.payload;

        // Also need to check authorship fields.
        let authorship_matches = check_authorship_match(&mut tx, existing_goal_id, draft).await?;

        let body_matches = schema_id_match
            && schema_version_match
            && title_match
            && text_match
            && state_match
            && parents_match
            && supersedes_match
            && payload_match;

        if body_matches && authorship_matches {
            tx.commit().await.map_err(map_err)?;
            return Ok(GoalWriteOutcome {
                goal_id: proxima_core::GoalId::new(existing_goal_id),
                change_event_seq: existing_seq,
                idempotent_replay: true,
            });
        }
        return Err(StorageError::ConstraintViolation(format!(
            "idempotency_conflict:{}",
            draft.request_id
        )));
    }

    validate_prior_goal_owner(&mut tx, prior.into_inner(), owner_kind, owner_principal_id).await?;

    // Generate ids inside the tx.
    let goal_id = uuid::Uuid::now_v7();
    let change_seq = uuid::Uuid::now_v7();

    insert_goal_row(&mut tx, draft, goal_id, Some(prior.into_inner())).await?;
    insert_goal_parents(&mut tx, draft, goal_id).await?;
    insert_goal_change_event(
        &mut tx,
        draft,
        goal_id,
        change_seq,
        Some(prior.into_inner()),
    )
    .await?;

    tx.commit().await.map_err(map_err)?;

    Ok(GoalWriteOutcome {
        goal_id: proxima_core::GoalId::new(goal_id),
        change_event_seq: change_seq,
        idempotent_replay: false,
    })
}

fn goal_state_str(state: GoalState) -> &'static str {
    match state {
        GoalState::Proposed => "Proposed",
        GoalState::Active => "Active",
        GoalState::Paused => "Paused",
        GoalState::Achieved => "Achieved",
        GoalState::Abandoned => "Abandoned",
        GoalState::Rejected => "Rejected",
    }
}

async fn insert_goal_row(
    tx: &mut sqlx::PgConnection,
    draft: &GoalDraft,
    goal_id: uuid::Uuid,
    supersedes: Option<uuid::Uuid>,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id) = match &draft.owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let owner_org_id = draft.owner.org_id.into_inner();

    let state_str = goal_state_str(draft.state);

    let (
        authorship_kind,
        authorship_origin,
        authorship_operator_id,
        authorship_tool_id,
        operator_kind,
        model_id,
        prompt_version,
        personality_type_id,
        personality_instance_id,
    ) = authorship_columns(&draft.authorship);

    sqlx::query(
        "INSERT INTO proxima_core.goals \
            (goal_id, schema_id, schema_version, owner_principal_kind, \
             owner_principal_id, owner_org_id, title, text, payload, state, supersedes, \
             authorship_kind, authorship_origin, authorship_operator_id, \
             authorship_tool_id, operator_kind, model_id, prompt_version, \
             personality_type_id, personality_instance_id, request_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21)",
    )
    .bind(goal_id)
    .bind(draft.schema_id.as_str())
    .bind(draft.schema_version.into_inner().cast_signed())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&draft.title)
    .bind(&draft.text)
    .bind(&draft.payload)
    .bind(state_str)
    .bind(supersedes)
    .bind(authorship_kind)
    .bind(authorship_origin)
    .bind(authorship_operator_id)
    .bind(authorship_tool_id)
    .bind(operator_kind)
    .bind(model_id)
    .bind(prompt_version)
    .bind(personality_type_id)
    .bind(personality_instance_id)
    .bind(&draft.request_id)
    .execute(tx)
    .await
    .map_err(map_err)?;

    Ok(())
}

async fn validate_prior_goal_owner(
    tx: &mut sqlx::PgConnection,
    prior_goal_id: uuid::Uuid,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let prior_row: Option<(String, uuid::Uuid)> = sqlx::query_as(
        "SELECT owner_principal_kind, owner_principal_id \
         FROM proxima_core.goals WHERE goal_id = $1",
    )
    .bind(prior_goal_id)
    .fetch_optional(tx)
    .await
    .map_err(map_err)?;

    match prior_row {
        None => Err(StorageError::NotFound),
        Some((p_kind, p_principal_id)) => {
            if p_kind != owner_kind || p_principal_id != owner_principal_id {
                Err(StorageError::ConstraintViolation(
                    "supersede crosses Owner boundary".to_string(),
                ))
            } else {
                Ok(())
            }
        }
    }
}

async fn insert_goal_parents(
    tx: &mut sqlx::PgConnection,
    draft: &GoalDraft,
    goal_id: uuid::Uuid,
) -> Result<(), StorageError> {
    for parent_id in &draft.parent_goal_ids {
        sqlx::query(
            "INSERT INTO proxima_core.goal_parents (goal_id, parent_goal_id) \
             VALUES ($1, $2)",
        )
        .bind(goal_id)
        .bind(parent_id.into_inner())
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    }
    Ok(())
}

async fn insert_goal_change_event(
    tx: &mut sqlx::PgConnection,
    draft: &GoalDraft,
    goal_id: uuid::Uuid,
    change_seq: uuid::Uuid,
    supersedes_goal_id: Option<uuid::Uuid>,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id) = match &draft.owner.principal {
        Principal::User(u) => ("User", u.into_inner()),
        Principal::Group(g) => ("Group", g.into_inner()),
    };
    let owner_org_id = draft.owner.org_id.into_inner();

    match supersedes_goal_id {
        None => {
            sqlx::query(
                "INSERT INTO proxima_core.change_event \
                    (seq, owner_principal_kind, owner_principal_id, owner_org_id, \
                     kind, entity_kind, entity_goal_id, entity_schema_id, \
                     entity_schema_version) \
                 VALUES ($1, $2, $3, $4, 'EntityAppend', 'Goal', $5, $6, $7)",
            )
            .bind(change_seq)
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(owner_org_id)
            .bind(goal_id)
            .bind(draft.schema_id.as_str())
            .bind(draft.schema_version.into_inner().cast_signed())
            .execute(tx)
            .await
            .map_err(map_err)?;
        }
        Some(prior_id) => {
            sqlx::query(
                "INSERT INTO proxima_core.change_event \
                    (seq, owner_principal_kind, owner_principal_id, owner_org_id, \
                     kind, entity_kind, entity_goal_id, entity_schema_id, \
                     entity_schema_version, supersedes_goal_id) \
                 VALUES ($1, $2, $3, $4, 'EntityAppend', 'Goal', $5, $6, $7, $8)",
            )
            .bind(change_seq)
            .bind(owner_kind)
            .bind(owner_principal_id)
            .bind(owner_org_id)
            .bind(goal_id)
            .bind(draft.schema_id.as_str())
            .bind(draft.schema_version.into_inner().cast_signed())
            .bind(prior_id)
            .execute(tx)
            .await
            .map_err(map_err)?;
        }
    }
    Ok(())
}
