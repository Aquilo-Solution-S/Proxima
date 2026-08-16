//! Core Goal storage atoms.

use std::collections::HashSet;

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, DecomposedGoalOutcome, GoalAtomicContext, GoalAuthorship, GoalDraft,
    GoalEvidenceRef, GoalLifecycleFact, GoalPayloadWrite, GoalState, GoalWakeConfigWrite,
    GoalWakeToolId, GoalWakeTrigger, GoalWriteOutcome, ModifyGoalAtomicRequest, SystemOrigin,
    TransitionGoalAtomicRequest,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    EdgeEndpoint, EntityKind, GoalId, MemoryId, Owner, SchemaId, SchemaVersion,
    StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::{internal, map_err};
use crate::sidecars::{PgSidecarKey, PgSidecarRegistryFrozen};
mod commands;
mod edges;
mod evidence;
mod insert;
mod lifecycle;
mod prior;
mod replay;
mod types;
mod wake;

use edges::{assert_goal_topology_references, goal_topology_edge_count};
use evidence::{
    outgoing_motivated_by_evidence, validate_evidence_in_owner, validate_operator_goal_evidence,
};
use insert::insert_or_replay_goal;
use lifecycle::{LifecycleWrite, lifecycle_outcome};
use prior::{
    DraftFromPayload, child_draft, draft_from_payload, draft_from_stored, load_prior_goal,
    validate_active_head, validate_goal_achievement, validate_goal_transition,
};
use replay::{
    CreateGoalReplayExpectation, ensure_create_goal_replay_side_effects_match,
    goal_evidence_matches, idempotency_conflict,
};
use types::{
    EvidenceTarget, InsertedGoal, StoredGoal, WakeConfigShape, WakeWrite,
};
use wake::goal_wake_matches;

pub(crate) use commands::{
    achieve_goal_atomic, create_goal_atomic, decompose_goal_atomic, modify_goal_atomic,
    transition_goal_atomic,
};
