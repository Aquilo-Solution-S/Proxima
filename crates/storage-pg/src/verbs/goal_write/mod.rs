//! Core Goal storage atoms.

use std::collections::HashSet;

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, DecomposedGoalOutcome, GoalAtomicContext, GoalAuthorship, GoalDraft,
    GoalEvidenceRef, GoalPayloadWrite, GoalReplayOutcome, GoalReplayRequest, GoalState,
    GoalWakeConfigWrite, GoalWakeToolId, GoalWakeTrigger, GoalWriteOutcome,
    ModifyGoalAtomicRequest, SystemOrigin, TransitionGoalAtomicRequest,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    EdgeEndpoint, EntityKind, GoalId, MemoryId, Owner, SchemaId, SchemaVersion, StorageError,
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

use edges::assert_goal_topology_references;
use evidence::{
    load_goal_evidence_exact, validate_evidence_in_owner, validate_operator_goal_evidence,
};
use insert::{
    PreparedGoalInsert, insert_or_replay_goal, persist_prepared_goal_insert, prepare_goal_insert,
};
use lifecycle::{LifecycleWrite, lifecycle_outcome};
use prior::{
    DraftFromPayload, child_draft, draft_from_payload, draft_from_stored, load_prior_goal,
    validate_active_head, validate_goal_achievement, validate_goal_transition,
};
use replay::{
    achieve_replay_declaration, create_replay_declaration, decompose_replay_declarations,
    idempotency_conflict, modify_replay_declaration, record_goal_replay_declaration,
    require_goal_replay, resolve_decompose_replay_set, resolve_goal_replay,
    transition_replay_declaration,
};
use types::{EvidenceTarget, InsertedGoal, StoredGoal, WakeWrite};

pub(crate) use crate::verbs::goal_timeseries::{
    GoalWritePreparation, lock_prepared_goal_write, lock_prepared_goal_writes,
    persist_prepared_goal_write, prepare_goal_write,
};

pub(crate) use commands::{
    achieve_goal_atomic, create_goal_atomic, create_goal_in_tx, decompose_goal_atomic,
    modify_goal_atomic, transition_goal_atomic,
};
pub(crate) use replay::{resolve_goal_command_replay, resolve_goal_command_replay_in_tx};
