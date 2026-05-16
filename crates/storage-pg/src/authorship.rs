//! Goal authorship → row-column projection + idempotency comparison.
//!
//! `goals` carries a flat set of nullable discriminator columns that
//! together encode `GoalAuthorship`. The (de)serialisation is shared by
//! the `goal_write` and `supersede_goal` verbs.

use proxima_core::StorageError;
use proxima_core::verbs::goal_write::{
    GoalAuthorship, GoalAuthorshipKind, GoalAuthorshipOrigin, GoalDraft, OperatorKind, SystemOrigin,
};

use crate::error::map_err;

/// Flat authorship columns stored on `proxima_core.goals`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorshipColumns {
    pub(crate) authorship_kind: GoalAuthorshipKind,
    pub(crate) authorship_origin: Option<GoalAuthorshipOrigin>,
    pub(crate) authorship_operator_id: Option<uuid::Uuid>,
    pub(crate) authorship_tool_id: Option<String>,
    pub(crate) operator_kind: Option<OperatorKind>,
    pub(crate) model_id: Option<String>,
    pub(crate) prompt_version: Option<String>,
    pub(crate) personality_instance_id: Option<uuid::Uuid>,
}

/// Project a `GoalAuthorship` into the flat column tuple.
pub(crate) fn authorship_columns(authorship: &GoalAuthorship) -> AuthorshipColumns {
    match authorship {
        GoalAuthorship::User => AuthorshipColumns {
            authorship_kind: GoalAuthorshipKind::User,
            authorship_origin: None,
            authorship_operator_id: None,
            authorship_tool_id: None,
            operator_kind: None,
            model_id: None,
            prompt_version: None,
            personality_instance_id: None,
        },
        GoalAuthorship::System(SystemOrigin::Operator {
            operator_id,
            operator_kind,
            model_id,
            prompt_version,
            personality_instance_id,
        }) => AuthorshipColumns {
            authorship_kind: GoalAuthorshipKind::System,
            authorship_origin: Some(GoalAuthorshipOrigin::Operator),
            authorship_operator_id: Some(operator_id.into_inner()),
            authorship_tool_id: None,
            operator_kind: Some(*operator_kind),
            model_id: Some(model_id.as_str().to_string()),
            prompt_version: Some(prompt_version.as_str().to_string()),
            personality_instance_id: Some(personality_instance_id.into_inner()),
        },
        GoalAuthorship::System(SystemOrigin::Tool { tool_id }) => AuthorshipColumns {
            authorship_kind: GoalAuthorshipKind::System,
            authorship_origin: Some(GoalAuthorshipOrigin::Tool),
            authorship_operator_id: None,
            authorship_tool_id: Some(tool_id.as_str().to_string()),
            operator_kind: None,
            model_id: None,
            prompt_version: None,
            personality_instance_id: None,
        },
        GoalAuthorship::External => AuthorshipColumns {
            authorship_kind: GoalAuthorshipKind::External,
            authorship_origin: None,
            authorship_operator_id: None,
            authorship_tool_id: None,
            operator_kind: None,
            model_id: None,
            prompt_version: None,
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
    let row = sqlx::query!(
        r#"SELECT authorship_kind AS "authorship_kind: GoalAuthorshipKind",
                  authorship_origin AS "authorship_origin: GoalAuthorshipOrigin",
                  authorship_operator_id,
                  authorship_tool_id,
                  operator_kind AS "operator_kind: OperatorKind",
                  model_id,
                  prompt_version,
                  personality_instance_id
             FROM proxima_core.goals WHERE goal_id = $1"#,
        existing_goal_id,
    )
    .fetch_one(tx)
    .await
    .map_err(map_err)?;

    let existing_auth = AuthorshipColumns {
        authorship_kind: row.authorship_kind,
        authorship_origin: row.authorship_origin,
        authorship_operator_id: row.authorship_operator_id,
        authorship_tool_id: row.authorship_tool_id,
        operator_kind: row.operator_kind,
        model_id: row.model_id,
        prompt_version: row.prompt_version,
        personality_instance_id: row.personality_instance_id,
    };

    Ok(existing_auth == authorship_columns(&draft.authorship))
}
