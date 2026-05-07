//! Goal authorship → row-column projection + idempotency comparison.
//!
//! `goals` carries a flat set of nullable discriminator columns that
//! together encode `GoalAuthorship`. The (de)serialisation is shared by
//! the `goal_write` and `supersede_goal` verbs.

use proxima_core::StorageError;
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, OperatorKind, SystemOrigin};

use crate::error::map_err;

/// Tuple of authorship columns for binding into an `INSERT` statement.
///
/// Order matches the `goals` table columns:
/// `(authorship_kind, authorship_origin, authorship_operator_id,
///   authorship_tool_id, operator_kind, model_id, prompt_version,
///   personality_type_id, personality_instance_id)`.
#[allow(clippy::type_complexity)]
pub(crate) type AuthorshipColumns = (
    String,
    Option<String>,
    Option<uuid::Uuid>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<uuid::Uuid>,
);

/// Project a `GoalAuthorship` into the flat column tuple.
pub(crate) fn authorship_columns(authorship: &GoalAuthorship) -> AuthorshipColumns {
    match authorship {
        GoalAuthorship::User => (
            "User".to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        GoalAuthorship::System(SystemOrigin::Operator {
            operator_id,
            operator_kind,
            model_id,
            prompt_version,
            personality_type_id,
            personality_instance_id,
        }) => (
            "System".to_string(),
            Some("Operator".to_string()),
            Some(operator_id.into_inner()),
            None,
            Some(match operator_kind {
                OperatorKind::AtoGoal => "AtoGoal".to_string(),
            }),
            Some(model_id.as_str().to_string()),
            Some(prompt_version.as_str().to_string()),
            Some(personality_type_id.clone()),
            Some(personality_instance_id.into_inner()),
        ),
        GoalAuthorship::System(SystemOrigin::Tool { tool_id }) => (
            "System".to_string(),
            Some("Tool".to_string()),
            None,
            Some(tool_id.as_str().to_string()),
            None,
            None,
            None,
            None,
            None,
        ),
        GoalAuthorship::External => (
            "External".to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
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
    #[allow(clippy::type_complexity)]
    let existing_auth: (
        String,
        Option<String>,
        Option<uuid::Uuid>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<uuid::Uuid>,
    ) = sqlx::query_as(
        "SELECT authorship_kind, authorship_origin, authorship_operator_id, \
                     authorship_tool_id, operator_kind, model_id, \
                     prompt_version, personality_type_id, personality_instance_id \
             FROM proxima_core.goals WHERE goal_id = $1",
    )
    .bind(existing_goal_id)
    .fetch_one(tx)
    .await
    .map_err(map_err)?;

    let (
        draft_kind,
        draft_origin,
        draft_op_id,
        draft_tool_id,
        draft_op_kind,
        draft_model,
        draft_prompt,
        draft_personality_type,
        draft_personality_instance,
    ) = authorship_columns(&draft.authorship);

    let kind_match = existing_auth.0 == draft_kind;
    let origin_match = existing_auth.1 == draft_origin;
    let op_id_match = existing_auth.2 == draft_op_id;
    let tool_id_match = existing_auth.3 == draft_tool_id;
    let op_kind_match = existing_auth.4 == draft_op_kind;
    let model_match = existing_auth.5 == draft_model;
    let prompt_match = existing_auth.6 == draft_prompt;
    let personality_type_match = existing_auth.7 == draft_personality_type;
    let personality_instance_match = existing_auth.8 == draft_personality_instance;

    Ok(kind_match
        && origin_match
        && op_id_match
        && tool_id_match
        && op_kind_match
        && model_match
        && prompt_match
        && personality_type_match
        && personality_instance_match)
}
