//! The goal-change Fact: what a goal write did, as an ordinary core Fact.
//!
//! A goal write is an action, and an action is a Fact (docs/05). Every write
//! that inserts a goal row also writes one `core/goal-change-v1` Fact in the
//! same transaction. The Fact is listenable, so a host that drains the
//! outbox sees goal writes on the same feed as every other Fact (docs/18
//! §Goal changes).
//!
//! Storage composes the payload inside the goal transaction, because the new
//! version's `t` and the series handle are minted there.

use crate::mcp::handles::{PrefixedUuidClass, format_prefixed_uuid, parse_prefixed_uuid};
use crate::verbs::goal_write::GoalState;
use crate::{FactPayload, GoalId, PayloadKeyBuilder};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// What a pause does with the work a scheduler already queued for the goal.
///
/// Proxima records the decision and does not interpret it. Running work
/// finishes either way.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_core.goal_pending_work",
    rename_all = "snake_case"
)]
pub enum PendingWork {
    /// The scheduler holds the queued work and runs it after a resume.
    Keep,
    /// The scheduler drops the queued work, ready or waiting to retry.
    Discard,
}

impl PendingWork {
    /// The wire spelling, as `core_goal` takes it and the payload carries it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Discard => "discard",
        }
    }
}

/// What a goal write did. Closed: `pending` exists only on [`Self::Pause`]
/// and `parent` only on [`Self::DecomposeChild`], so the other variants
/// cannot carry them.
///
/// Serialized as `{"kind": "...", ...}`. A non-pause change says nothing
/// about pending work: on abandon or achieve the goal is terminal, and a
/// scheduler drops what it queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoalChange {
    /// A new goal series.
    Set,
    /// A replacement of an Active goal's content.
    Modify,
    /// A child created by a decompose. The child starts its own series.
    DecomposeChild {
        #[serde(with = "goal_handle")]
        parent: GoalId,
    },
    Pause {
        pending: PendingWork,
    },
    Resume,
    Abandon,
    Achieve,
}

/// The discriminant of [`GoalChange`], as the sidecar's `change_kind`
/// column stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "proxima_core.goal_change_kind", rename_all = "snake_case")]
pub enum GoalChangeKind {
    Set,
    Modify,
    DecomposeChild,
    Pause,
    Resume,
    Abandon,
    Achieve,
}

/// A stored change row that no [`GoalChange`] can be built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GoalChangeColumnsError {
    #[error("a {kind:?} change row needs `parent_t`")]
    MissingParent { kind: GoalChangeKind },
    #[error("a {kind:?} change row needs `pending`")]
    MissingPending { kind: GoalChangeKind },
    #[error("a {kind:?} change row must not carry `parent_t`")]
    UnexpectedParent { kind: GoalChangeKind },
    #[error("a {kind:?} change row must not carry `pending`")]
    UnexpectedPending { kind: GoalChangeKind },
}

impl GoalChange {
    #[must_use]
    pub const fn kind(self) -> GoalChangeKind {
        match self {
            Self::Set => GoalChangeKind::Set,
            Self::Modify => GoalChangeKind::Modify,
            Self::DecomposeChild { .. } => GoalChangeKind::DecomposeChild,
            Self::Pause { .. } => GoalChangeKind::Pause,
            Self::Resume => GoalChangeKind::Resume,
            Self::Abandon => GoalChangeKind::Abandon,
            Self::Achieve => GoalChangeKind::Achieve,
        }
    }

    /// The parent of a decomposed child.
    #[must_use]
    pub const fn parent(self) -> Option<GoalId> {
        match self {
            Self::DecomposeChild { parent } => Some(parent),
            Self::Set
            | Self::Modify
            | Self::Pause { .. }
            | Self::Resume
            | Self::Abandon
            | Self::Achieve => None,
        }
    }

    /// What a pause does with queued work.
    #[must_use]
    pub const fn pending(self) -> Option<PendingWork> {
        match self {
            Self::Pause { pending } => Some(pending),
            Self::Set
            | Self::Modify
            | Self::DecomposeChild { .. }
            | Self::Resume
            | Self::Abandon
            | Self::Achieve => None,
        }
    }

    /// Rebuild a change from the sidecar's three columns: the one place a
    /// stored row becomes a [`GoalChange`]. The table's CHECKs refuse the
    /// rows this refuses; the second look is for a row that skipped them.
    ///
    /// # Errors
    ///
    /// [`GoalChangeColumnsError`] when `parent` or `pending` is present on a
    /// kind that cannot carry it, or missing on the kind that needs it.
    pub const fn from_columns(
        kind: GoalChangeKind,
        parent: Option<GoalId>,
        pending: Option<PendingWork>,
    ) -> Result<Self, GoalChangeColumnsError> {
        match kind {
            GoalChangeKind::DecomposeChild => match (parent, pending) {
                (Some(parent), None) => Ok(Self::DecomposeChild { parent }),
                (None, _) => Err(GoalChangeColumnsError::MissingParent { kind }),
                (Some(_), Some(_)) => Err(GoalChangeColumnsError::UnexpectedPending { kind }),
            },
            GoalChangeKind::Pause => match (parent, pending) {
                (None, Some(pending)) => Ok(Self::Pause { pending }),
                (_, None) => Err(GoalChangeColumnsError::MissingPending { kind }),
                (Some(_), Some(_)) => Err(GoalChangeColumnsError::UnexpectedParent { kind }),
            },
            GoalChangeKind::Set => Self::bare(kind, Self::Set, parent, pending),
            GoalChangeKind::Modify => Self::bare(kind, Self::Modify, parent, pending),
            GoalChangeKind::Resume => Self::bare(kind, Self::Resume, parent, pending),
            GoalChangeKind::Abandon => Self::bare(kind, Self::Abandon, parent, pending),
            GoalChangeKind::Achieve => Self::bare(kind, Self::Achieve, parent, pending),
        }
    }

    /// A kind that carries neither `parent` nor `pending`.
    const fn bare(
        kind: GoalChangeKind,
        change: Self,
        parent: Option<GoalId>,
        pending: Option<PendingWork>,
    ) -> Result<Self, GoalChangeColumnsError> {
        match (parent, pending) {
            (None, None) => Ok(change),
            (Some(_), _) => Err(GoalChangeColumnsError::UnexpectedParent { kind }),
            (None, Some(_)) => Err(GoalChangeColumnsError::UnexpectedPending { kind }),
        }
    }
}

/// `core/goal-change-v1`: one goal write, as the event's `data`.
///
/// `series` is the goal's series handle, the same on every version and
/// opaque (not a `G:` handle). `goal` is the version this write created and
/// `prior` the one it replaced, `None` when the write starts a series (set,
/// a decomposed child). `state` is the state after the write.
///
/// `revision` is the version's position in its series: 1 for a set and for a
/// decomposed child, one more for each later version.
///
/// Ordering is the consumer's: a failed delivery is retried after later
/// records, so a resume can arrive before the pause it follows. Key by
/// `series` and apply an event only if its `revision` is greater than the
/// last one applied for that series. `goal` versions are identifiers, not an
/// order: a version's `t` comes from the database clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalChangeV1 {
    pub series: uuid::Uuid,
    #[serde(with = "goal_handle")]
    pub goal: GoalId,
    #[serde(with = "goal_handle_option")]
    pub prior: Option<GoalId>,
    pub revision: u32,
    pub state: GoalState,
    pub change: GoalChange,
}

impl FactPayload for GoalChangeV1 {
    const SCHEMA_ID: &'static str = "core/goal-change-v1";
    const SCHEMA_VERSION: u32 = 1;
    // Core always registers the schema, so a stored Fact is readable. A
    // runtime that drains the outbox turns this on before freeze
    // (`FlavorRegistry::publish_goal_changes`): without an intake there is
    // nobody to tell, and a goal write stays as it was.
    const LISTENABLE: bool = false;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_uuid("goal", self.goal.into_inner());
        key.finish()
    }

    fn render(&self) -> String {
        format!(
            "goal-change {} {}",
            self.change.kind().as_str(),
            format_prefixed_uuid(self.goal.into_inner(), PrefixedUuidClass::Goal)
        )
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_core.goal_change_v1")
    }

    /// Written by hand: the `G:<uuid>` spelling of `goal`, `prior` and
    /// `parent` is a serde adapter a derive cannot see.
    fn json_schema() -> Option<serde_json::Value> {
        let handle = serde_json::json!({
            "type": "string",
            "pattern": "^G:[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
        });
        let kind_only = |kind: &str| {
            serde_json::json!({
                "type": "object",
                "required": ["kind"],
                "properties": { "kind": { "const": kind } },
                "additionalProperties": false
            })
        };
        Some(serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "GoalChangeV1",
            "type": "object",
            "required": ["series", "goal", "prior", "revision", "state", "change"],
            "properties": {
                "series": { "type": "string", "format": "uuid" },
                "goal": handle,
                "prior": { "oneOf": [handle, { "type": "null" }] },
                "revision": { "type": "integer", "minimum": 1 },
                "state": { "enum": ["Active", "Paused", "Achieved", "Abandoned"] },
                "change": {
                    "oneOf": [
                        kind_only("set"),
                        kind_only("modify"),
                        {
                            "type": "object",
                            "required": ["kind", "parent"],
                            "properties": {
                                "kind": { "const": "decompose_child" },
                                "parent": handle
                            },
                            "additionalProperties": false
                        },
                        {
                            "type": "object",
                            "required": ["kind", "pending"],
                            "properties": {
                                "kind": { "const": "pause" },
                                "pending": { "enum": ["keep", "discard"] }
                            },
                            "additionalProperties": false
                        },
                        kind_only("resume"),
                        kind_only("abandon"),
                        kind_only("achieve")
                    ]
                }
            },
            "additionalProperties": false
        }))
    }
}

impl GoalChangeKind {
    /// The `kind` the payload carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Modify => "modify",
            Self::DecomposeChild => "decompose_child",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Abandon => "abandon",
            Self::Achieve => "achieve",
        }
    }
}

/// `G:<uuid>` on the wire, [`GoalId`] in Rust. `GoalId` itself serializes as
/// a bare uuid, which is the form storage columns take.
mod goal_handle {
    use super::{
        Deserialize, Deserializer, GoalId, PrefixedUuidClass, Serializer, format_prefixed_uuid,
        parse_prefixed_uuid,
    };

    pub(super) fn serialize<S: Serializer>(id: &GoalId, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format_prefixed_uuid(
            id.into_inner(),
            PrefixedUuidClass::Goal,
        ))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<GoalId, D::Error> {
        let raw = String::deserialize(deserializer)?;
        parse_prefixed_uuid(&raw, PrefixedUuidClass::Goal)
            .map(GoalId::new)
            .map_err(serde::de::Error::custom)
    }
}

/// [`goal_handle`] for a nullable field: `null` when absent, never skipped.
mod goal_handle_option {
    use super::{
        Deserialize, Deserializer, GoalId, PrefixedUuidClass, Serializer, format_prefixed_uuid,
        parse_prefixed_uuid,
    };

    #[expect(clippy::ref_option, reason = "the signature serde's `with` calls")]
    pub(super) fn serialize<S: Serializer>(
        id: &Option<GoalId>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match id {
            Some(id) => serializer.serialize_str(&format_prefixed_uuid(
                id.into_inner(),
                PrefixedUuidClass::Goal,
            )),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<GoalId>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|raw| {
                parse_prefixed_uuid(&raw, PrefixedUuidClass::Goal)
                    .map(GoalId::new)
                    .map_err(serde::de::Error::custom)
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::{GoalChange as C, GoalChangeKind as K};
    use super::{GoalChangeV1, GoalId, GoalState, PendingWork};
    use serde_json::json;

    fn id(n: u128) -> GoalId {
        GoalId::new(uuid::Uuid::from_u128(n))
    }

    #[test]
    fn the_payload_is_the_documented_json() {
        let parent = "G:00000000-0000-0000-0000-000000000009";
        let pending = PendingWork::Discard;
        let changes = [
            (C::Set, json!({ "kind": "set" })),
            (C::Modify, json!({ "kind": "modify" })),
            (
                C::DecomposeChild { parent: id(9) },
                json!({ "kind": "decompose_child", "parent": parent }),
            ),
            (
                C::Pause { pending },
                json!({ "kind": "pause", "pending": "discard" }),
            ),
            (C::Resume, json!({ "kind": "resume" })),
            (C::Abandon, json!({ "kind": "abandon" })),
            (C::Achieve, json!({ "kind": "achieve" })),
        ];
        for (change, wire) in changes {
            let payload = GoalChangeV1 {
                series: uuid::Uuid::from_u128(1),
                goal: id(3),
                prior: Some(id(2)),
                revision: 2,
                state: GoalState::Paused,
                change,
            };
            let value = json!({
                "series": "00000000-0000-0000-0000-000000000001", "revision": 2, "state": "Paused",
                "goal": "G:00000000-0000-0000-0000-000000000003",
                "prior": "G:00000000-0000-0000-0000-000000000002", "change": wire,
            });
            assert_eq!(serde_json::to_value(&payload).expect("serializes"), value);
            let parsed: GoalChangeV1 = serde_json::from_value(value).expect("parses");
            assert_eq!(parsed, payload);
        }
    }

    #[test]
    fn columns_rebuild_exactly_the_change_they_were_cut_from() {
        let pause = C::Pause {
            pending: PendingWork::Discard,
        };
        for change in [C::Set, C::DecomposeChild { parent: id(9) }, pause] {
            let (kind, parent, pending) = (change.kind(), change.parent(), change.pending());
            assert_eq!(C::from_columns(kind, parent, pending), Ok(change));
        }
        let keep = Some(PendingWork::Keep);
        let bad = [
            (K::Resume, None, keep),
            (K::Set, Some(id(1)), None),
            (K::Pause, None, None),
            (K::DecomposeChild, None, None),
        ];
        for (kind, parent, pending) in bad {
            assert!(C::from_columns(kind, parent, pending).is_err(), "{kind:?}");
        }
    }
}
