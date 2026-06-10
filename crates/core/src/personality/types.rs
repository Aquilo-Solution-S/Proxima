//! Personality type definitions.
//!
//! This module contains enum types used throughout the personality system:
//! - `WakeChainDepth` - Depth tracking for wake chains
//! - `WakeEntryTriggerKind` - What triggers a wake entry
//! - `WakeEntryGoalScope` - Goal scoping for wake entries
//! - `WakeExecutionMode` - Execution mode for wake entries
//! - `WakeEntryExecutionMode` - Execution mode variant
//! - `WakeEntryAuthoredBy` - Who authored a wake entry
//! - `WakeInvocationStatus` - Status of a wake invocation
//! - `PersonalityMemoryKind` - Kind of personality memory

use crate::outbox::EntityKind;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
)]
pub struct WakeChainDepth(u16);

impl WakeChainDepth {
    #[must_use]
    pub const fn new(inner: u16) -> Self {
        Self(inner)
    }

    #[must_use]
    pub const fn zero() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn into_inner(self) -> u16 {
        self.0
    }

    #[must_use]
    pub fn next_after(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
    schemars::JsonSchema,
    sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_core.wake_trigger_kind",
    rename_all = "snake_case"
)]
pub enum WakeEntryTriggerKind {
    OnMemory,
    OnEdge,
}

impl WakeEntryTriggerKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OnMemory => "on_memory",
            Self::OnEdge => "on_edge",
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
    schemars::JsonSchema,
    sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "proxima_core.wake_goal_scope", rename_all = "snake_case")]
pub enum WakeEntryGoalScope {
    #[default]
    None,
    TriggerGoalAssigned,
}

impl WakeEntryGoalScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::TriggerGoalAssigned => "trigger_goal_assigned",
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_core.wake_execution_mode",
    rename_all = "snake_case"
)]
pub enum WakeExecutionMode {
    SubstrateOnly,
}

impl WakeExecutionMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SubstrateOnly => "substrate_only",
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
    schemars::JsonSchema,
    sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_core.wake_execution_mode",
    rename_all = "snake_case"
)]
pub enum WakeEntryExecutionMode {
    SubstrateOnly,
}

impl WakeEntryExecutionMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SubstrateOnly => "substrate_only",
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    specta::Type,
    Default,
    schemars::JsonSchema,
    sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "proxima_core.wake_authored_by", rename_all = "snake_case")]
pub enum WakeEntryAuthoredBy {
    #[default]
    Any,
    // serde keeps `self_author` (existing JSON contract); SQL enum
    // value is `self` (the historical CHECK/text discriminator the
    // ENUM migration preserved).
    #[sqlx(rename = "self")]
    SelfAuthor,
    Other,
}

impl WakeEntryAuthoredBy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::SelfAuthor => "self",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(
    type_name = "proxima_core.wake_invocation_status",
    rename_all = "lowercase"
)]
pub enum WakeInvocationStatus {
    Running,
    Succeeded,
    Truncated,
    Failed,
}

impl WakeInvocationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Truncated => "truncated",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonalityMemoryKind {
    Abstraction,
    Perspective,
}

impl PersonalityMemoryKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Abstraction => "Abstraction",
            Self::Perspective => "Perspective",
        }
    }

    #[must_use]
    pub const fn entity_kind(self) -> EntityKind {
        match self {
            Self::Abstraction => EntityKind::Abstraction,
            Self::Perspective => EntityKind::Perspective,
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_core.personality_status",
    rename_all = "snake_case"
)]
pub enum PersonalityStatus {
    Active,
    NeedsRepair,
    Tombstoned,
}

impl PersonalityStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::NeedsRepair => "needs_repair",
            Self::Tombstoned => "tombstoned",
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type,
)]
#[serde(rename_all = "lowercase")]
#[sqlx(
    type_name = "proxima_core.wake_invocation_log_status",
    rename_all = "lowercase"
)]
pub enum WakeInvocationLogStatus {
    Started,
    Succeeded,
    Failed,
}

impl WakeInvocationLogStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    sqlx::Type,
)]
#[serde(rename_all = "lowercase")]
#[sqlx(
    type_name = "proxima_core.wake_trace_outcome_kind",
    rename_all = "lowercase"
)]
pub enum WakeTraceOutcomeKind {
    Succeeded,
    Truncated,
    Failed,
}

impl WakeTraceOutcomeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Truncated => "truncated",
            Self::Failed => "failed",
        }
    }
}
