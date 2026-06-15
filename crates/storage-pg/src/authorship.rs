//! Goal authorship → row-column projection + idempotency comparison.
//!
//! `goals` carries a flat set of nullable discriminator columns that
//! together encode `GoalAuthorship`. The (de)serialisation is shared by
//! the goal-write storage atoms (create / transition / achieve / modify
//! / decompose).

use proxima_core::verbs::goal_write::{
    GoalAuthorship, GoalAuthorshipKind, GoalAuthorshipOrigin, OperatorKind, SystemOrigin,
};

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
