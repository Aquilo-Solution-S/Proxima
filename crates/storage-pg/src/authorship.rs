//! Goal authorship → row-column projection + idempotency comparison.
//!
//! `goals` carries a flat set of nullable discriminator columns that
//! together encode `GoalAuthorship`. The (de)serialisation is shared by
//! the `goal_write` and `supersede_goal` verbs.

use proxima_core::StorageError;
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, OperatorKind, SystemOrigin};

use crate::error::map_err;

/// Flat authorship columns stored on `proxima_core.goals`.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct AuthorshipColumns {
    pub(crate) authorship_kind: String,
    pub(crate) authorship_origin: Option<String>,
    pub(crate) authorship_operator_id: Option<uuid::Uuid>,
    pub(crate) authorship_tool_id: Option<String>,
    pub(crate) operator_kind: Option<String>,
    pub(crate) model_id: Option<String>,
    pub(crate) prompt_version: Option<String>,
    pub(crate) personality_type_id: Option<String>,
    pub(crate) personality_instance_id: Option<uuid::Uuid>,
}

/// Project a `GoalAuthorship` into the flat column tuple.
pub(crate) fn authorship_columns(authorship: &GoalAuthorship) -> AuthorshipColumns {
    match authorship {
        GoalAuthorship::User => AuthorshipColumns {
            authorship_kind: "User".to_string(),
            authorship_origin: None,
            authorship_operator_id: None,
            authorship_tool_id: None,
            operator_kind: None,
            model_id: None,
            prompt_version: None,
            personality_type_id: None,
            personality_instance_id: None,
        },
        GoalAuthorship::System(SystemOrigin::Operator {
            operator_id,
            operator_kind,
            model_id,
            prompt_version,
            personality_instance_id,
        }) => AuthorshipColumns {
            authorship_kind: "System".to_string(),
            authorship_origin: Some("Operator".to_string()),
            authorship_operator_id: Some(operator_id.into_inner()),
            authorship_tool_id: None,
            operator_kind: Some(match operator_kind {
                OperatorKind::AtoGoal => "AtoGoal".to_string(),
            }),
            model_id: Some(model_id.as_str().to_string()),
            prompt_version: Some(prompt_version.as_str().to_string()),
            personality_type_id: None,
            personality_instance_id: Some(personality_instance_id.into_inner()),
        },
        GoalAuthorship::System(SystemOrigin::Tool { tool_id }) => AuthorshipColumns {
            authorship_kind: "System".to_string(),
            authorship_origin: Some("Tool".to_string()),
            authorship_operator_id: None,
            authorship_tool_id: Some(tool_id.as_str().to_string()),
            operator_kind: None,
            model_id: None,
            prompt_version: None,
            personality_type_id: None,
            personality_instance_id: None,
        },
        GoalAuthorship::External => AuthorshipColumns {
            authorship_kind: "External".to_string(),
            authorship_origin: None,
            authorship_operator_id: None,
            authorship_tool_id: None,
            operator_kind: None,
            model_id: None,
            prompt_version: None,
            personality_type_id: None,
            personality_instance_id: None,
        },
    }
}

/// Idempotency comparison: does the existing goal's authorship match the
/// one in `draft`? Used in the replay branch of `write_goal` /
/// `supersede_goal` to decide between idempotent-replay and conflict.
pub(crate) async fn check_authorship_match(
    tx: &mut sqlx::PgConnection,
    existing_goal_id: uuid::Uuid,
    draft: &GoalDraft,
) -> Result<bool, StorageError> {
    let existing_auth: AuthorshipColumns = sqlx::query_as(
        "SELECT authorship_kind, authorship_origin, authorship_operator_id, \
                     authorship_tool_id, operator_kind, model_id, \
                     prompt_version, personality_type_id, personality_instance_id \
             FROM proxima_core.goals WHERE goal_id = $1",
    )
    .bind(existing_goal_id)
    .fetch_one(tx)
    .await
    .map_err(map_err)?;

    Ok(existing_auth == authorship_columns(&draft.authorship))
}
