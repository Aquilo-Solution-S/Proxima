//! Owner scoping primitive.
//!
//! See docs/01-event-source.md §"Owner — scoping primitive" for semantics.

use crate::{GroupId, OrgId, UserId};

/// Owner carries principal (access scope) and org_id (billing unit).
/// org_id is NOT part of the access predicate (AGENTS.md invariant 4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Owner {
    pub principal: Principal,
    pub org_id: OrgId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Principal {
    User(UserId),
    Group(GroupId),
}
