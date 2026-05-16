//! `GoalWrite` verb — `write_goal` and `supersede_goal`.
//!
//! The two paths share most of their structure (replay check, body
//! comparison, then goal + parents + `change_event` insert). The shape
//! is kept co-located here so the symmetry is visible; the small
//! `supersedes` delta sits at the call sites rather than behind an
//! abstraction.

use std::collections::HashSet;

use proxima_core::verbs::goal_write::{
    GoalAuthorshipKind, GoalAuthorshipOrigin, GoalDraft, GoalState, GoalWriteOutcome, OperatorKind,
};
use proxima_core::{GoalId, OwnerPrincipalKind, Principal, StorageError};
use sqlx::PgPool;

use crate::authorship::{authorship_columns, check_authorship_match};
use crate::error::map_err;

#[allow(clippy::too_many_lines)]
pub(crate) async fn write_goal_atomic(
    pool: &PgPool,
    draft: &GoalDraft,
) -> Result<GoalWriteOutcome, StorageError> {
    let (owner_kind, owner_principal_id) = match &draft.owner.principal {
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    };
    let owner_org_id = draft.owner.org_id.into_inner();

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    // Replay check by (owner, request_id).
    // We need to join with change_event to get the seq.
    let existing = sqlx::query!(
        r#"SELECT g.goal_id, ce.seq
             FROM proxima_core.goals g
             JOIN proxima_core.change_event ce ON ce.entity_goal_id = g.goal_id
             WHERE (g.owner_principal_kind, g.owner_principal_id, g.owner_org_id, g.request_id)
               = ($1, $2, $3, $4)
             ORDER BY ce.seq ASC LIMIT 1"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        &draft.request_id,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;

    if let Some(existing) = existing {
        let existing_goal_id = existing.goal_id;
        let existing_seq = existing.seq;

        // Compare the existing body with the draft.
        let existing_row = sqlx::query!(
            r#"SELECT schema_id, schema_version, title, text,
                       state AS "state: GoalState",
                       COALESCE((SELECT array_agg(parent_goal_id) FROM proxima_core.goal_parents WHERE goal_id = $1), '{}'::uuid[]) AS "parents!",
                       supersedes, payload
                 FROM proxima_core.goals WHERE goal_id = $1"#,
            existing_goal_id,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(map_err)?;

        let existing_parents: HashSet<uuid::Uuid> = existing_row.parents.into_iter().collect();
        let draft_parents: HashSet<uuid::Uuid> = draft
            .parent_goal_ids
            .iter()
            .map(|g| g.into_inner())
            .collect();

        // Check if all fields match.
        let schema_id_match = existing_row.schema_id == draft.schema_id.as_str();
        let schema_version_match =
            existing_row.schema_version == draft.schema_version.into_inner().cast_signed();
        let title_match = existing_row.title == draft.title;
        let text_match = existing_row.text == draft.text;
        let state_match = existing_row.state == draft.state;
        let parents_match = existing_parents == draft_parents;
        let expected_supersedes = draft.supersedes_goal_id.map(GoalId::into_inner);
        let supersedes_match = existing_row.supersedes == expected_supersedes;
        let payload_match = existing_row.payload == draft.payload;

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
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    };
    let owner_org_id = draft.owner.org_id.into_inner();

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    // Replay check by (owner, request_id) — same as write_goal.
    let existing = sqlx::query!(
        r#"SELECT g.goal_id, ce.seq
             FROM proxima_core.goals g
             JOIN proxima_core.change_event ce ON ce.entity_goal_id = g.goal_id
             WHERE (g.owner_principal_kind, g.owner_principal_id, g.owner_org_id, g.request_id)
               = ($1, $2, $3, $4)
             ORDER BY ce.seq ASC LIMIT 1"#,
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        &draft.request_id,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_err)?;

    if let Some(existing) = existing {
        let existing_goal_id = existing.goal_id;
        let existing_seq = existing.seq;

        // Compare the existing body with the draft (including supersedes = prior).
        let existing_row = sqlx::query!(
            r#"SELECT schema_id, schema_version, title, text,
                       state AS "state: GoalState",
                       COALESCE((SELECT array_agg(parent_goal_id) FROM proxima_core.goal_parents WHERE goal_id = $1), '{}'::uuid[]) AS "parents!",
                       supersedes, payload
                 FROM proxima_core.goals WHERE goal_id = $1"#,
            existing_goal_id,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(map_err)?;

        let existing_parents: HashSet<uuid::Uuid> = existing_row.parents.into_iter().collect();
        let draft_parents: HashSet<uuid::Uuid> = draft
            .parent_goal_ids
            .iter()
            .map(|g| g.into_inner())
            .collect();

        // Check if all fields match (including supersedes = prior).
        let schema_id_match = existing_row.schema_id == draft.schema_id.as_str();
        let schema_version_match =
            existing_row.schema_version == draft.schema_version.into_inner().cast_signed();
        let title_match = existing_row.title == draft.title;
        let text_match = existing_row.text == draft.text;
        let state_match = existing_row.state == draft.state;
        let parents_match = existing_parents == draft_parents;
        let supersedes_match = existing_row.supersedes == Some(prior.into_inner());
        let payload_match = existing_row.payload == draft.payload;

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

async fn insert_goal_row(
    tx: &mut sqlx::PgConnection,
    draft: &GoalDraft,
    goal_id: uuid::Uuid,
    supersedes: Option<uuid::Uuid>,
) -> Result<(), StorageError> {
    let (owner_kind, owner_principal_id) = match &draft.owner.principal {
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    };
    let owner_org_id = draft.owner.org_id.into_inner();

    let authorship = authorship_columns(&draft.authorship);

    sqlx::query!(
        r#"INSERT INTO proxima_core.goals
            (goal_id, schema_id, schema_version, owner_principal_kind,
             owner_principal_id, owner_org_id, title, text, payload, state, supersedes,
             authorship_kind, authorship_origin, authorship_operator_id,
             authorship_tool_id, operator_kind, model_id, prompt_version,
             personality_instance_id, request_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                 $12, $13, $14, $15, $16, $17, $18, $19, $20)"#,
        goal_id,
        draft.schema_id.as_str(),
        draft.schema_version.into_inner().cast_signed(),
        owner_kind as OwnerPrincipalKind,
        owner_principal_id,
        owner_org_id,
        &draft.title,
        &draft.text,
        &draft.payload,
        draft.state as GoalState,
        supersedes,
        authorship.authorship_kind as GoalAuthorshipKind,
        authorship.authorship_origin as Option<GoalAuthorshipOrigin>,
        authorship.authorship_operator_id,
        authorship.authorship_tool_id,
        authorship.operator_kind as Option<OperatorKind>,
        authorship.model_id,
        authorship.prompt_version,
        authorship.personality_instance_id,
        &draft.request_id,
    )
    .execute(tx)
    .await
    .map_err(map_err)?;

    Ok(())
}

async fn validate_prior_goal_owner(
    tx: &mut sqlx::PgConnection,
    prior_goal_id: uuid::Uuid,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let prior_row = sqlx::query!(
        r#"SELECT owner_principal_kind AS "owner_principal_kind: OwnerPrincipalKind",
                  owner_principal_id
             FROM proxima_core.goals WHERE goal_id = $1"#,
        prior_goal_id,
    )
    .fetch_optional(tx)
    .await
    .map_err(map_err)?;

    match prior_row {
        None => Err(StorageError::NotFound),
        Some(row) => {
            if row.owner_principal_kind != owner_kind
                || row.owner_principal_id != owner_principal_id
            {
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
        sqlx::query!(
            "INSERT INTO proxima_core.goal_parents (goal_id, parent_goal_id)
             VALUES ($1, $2)",
            goal_id,
            parent_id.into_inner(),
        )
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
        Principal::User(u) => (OwnerPrincipalKind::User, u.into_inner()),
        Principal::Group(g) => (OwnerPrincipalKind::Group, g.into_inner()),
    };
    let owner_org_id = draft.owner.org_id.into_inner();

    match supersedes_goal_id {
        None => {
            sqlx::query!(
                r#"INSERT INTO proxima_core.change_event
                    (seq, owner_principal_kind, owner_principal_id, owner_org_id,
                     kind, entity_kind, entity_goal_id, entity_schema_id,
                     entity_schema_version)
                 VALUES ($1, $2, $3, $4, 'EntityAppend', 'Goal', $5, $6, $7)"#,
                change_seq,
                owner_kind as OwnerPrincipalKind,
                owner_principal_id,
                owner_org_id,
                goal_id,
                draft.schema_id.as_str(),
                draft.schema_version.into_inner().cast_signed(),
            )
            .execute(tx)
            .await
            .map_err(map_err)?;
        }
        Some(prior_id) => {
            sqlx::query!(
                r#"INSERT INTO proxima_core.change_event
                    (seq, owner_principal_kind, owner_principal_id, owner_org_id,
                     kind, entity_kind, entity_goal_id, entity_schema_id,
                     entity_schema_version, supersedes_goal_id)
                 VALUES ($1, $2, $3, $4, 'EntityAppend', 'Goal', $5, $6, $7, $8)"#,
                change_seq,
                owner_kind as OwnerPrincipalKind,
                owner_principal_id,
                owner_org_id,
                goal_id,
                draft.schema_id.as_str(),
                draft.schema_version.into_inner().cast_signed(),
                prior_id,
            )
            .execute(tx)
            .await
            .map_err(map_err)?;
        }
    }
    Ok(())
}
