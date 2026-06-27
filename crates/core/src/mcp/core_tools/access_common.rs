use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::access::{AccessGrantRow, Relation, Visibility};
use crate::mcp::McpToolError;
use crate::{GroupId, OwnerPrincipalKind, Principal, UserId};

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationArg {
    Owner,
    Admin,
    Editor,
    Viewer,
    Ingest,
    Member,
}

impl From<RelationArg> for Relation {
    fn from(value: RelationArg) -> Self {
        match value {
            RelationArg::Owner => Self::Owner,
            RelationArg::Admin => Self::Admin,
            RelationArg::Editor => Self::Editor,
            RelationArg::Viewer => Self::Viewer,
            RelationArg::Ingest => Self::Ingest,
            RelationArg::Member => Self::Member,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityArg {
    Private,
    Shared,
    Public,
}

impl From<VisibilityArg> for Visibility {
    fn from(value: VisibilityArg) -> Self {
        match value {
            VisibilityArg::Private => Self::Private,
            VisibilityArg::Shared => Self::Shared,
            VisibilityArg::Public => Self::Public,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StatusOutput {
    pub ok: bool,
}

#[derive(Debug, Serialize)]
pub struct GrantOutput {
    pub relation: String,
    pub subject: String,
    pub subject_is_group: bool,
}

pub(super) fn parse_principal(raw: &str) -> Result<Principal, McpToolError> {
    let Some((kind, id)) = raw.split_once(':') else {
        return raw
            .parse::<uuid::Uuid>()
            .map(|id| Principal::User(UserId::new(id)))
            .map_err(|err| {
                McpToolError::InvalidInput(format!(
                    "principal must be user:<uuid>, group:<uuid>, or bare user uuid: {err}"
                ))
            });
    };
    let id = id
        .parse::<uuid::Uuid>()
        .map_err(|err| McpToolError::InvalidInput(format!("principal id must be a uuid: {err}")))?;
    match kind {
        "user" => Ok(Principal::User(UserId::new(id))),
        "group" => Ok(Principal::Group(GroupId::new(id))),
        other => Err(McpToolError::InvalidInput(format!(
            "principal kind must be user or group, got {other}"
        ))),
    }
}

pub(super) fn format_principal(principal: &Principal) -> String {
    let (kind, id) = principal.columns();
    match kind {
        OwnerPrincipalKind::User => format!("user:{id}"),
        OwnerPrincipalKind::Group => format!("group:{id}"),
    }
}

pub(super) fn format_relation(relation: Relation) -> String {
    match relation {
        Relation::Owner => "owner",
        Relation::Admin => "admin",
        Relation::Editor => "editor",
        Relation::Viewer => "viewer",
        Relation::Ingest => "ingest",
        Relation::Member => "member",
    }
    .to_string()
}

pub(super) fn format_grant(row: &AccessGrantRow) -> GrantOutput {
    GrantOutput {
        relation: format_relation(row.relation),
        subject: format_principal(&row.subject),
        subject_is_group: row.subject_is_group,
    }
}
