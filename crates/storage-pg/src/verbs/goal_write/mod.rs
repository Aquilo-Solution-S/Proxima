//! Core Goal storage atoms.

use std::collections::HashSet;

use proxima_core::goal::payloads::{
    GoalAbandonedV1, GoalAchievedV1, GoalActivatedV1, GoalPausedV1,
};
use proxima_core::goal::relations::CORE_MOTIVATED_BY_RELATION;
use proxima_core::relation::{CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION};
use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, ChildGoalDraft, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, DecomposedGoalOutcome, GoalAtomicContext, GoalAuthorship, GoalDraft,
    GoalEvidenceRef, GoalLifecycleFact, GoalPayloadWrite, GoalState, GoalWakeConfigWrite,
    GoalWakeToolId, GoalWakeTrigger, GoalWriteOutcome, ModifyGoalAtomicRequest, SystemOrigin,
    TransitionGoalAtomicRequest,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    EdgeAuthorshipKind, EntityKind, FactPayload, GoalId, MemoryId, Owner, RegisteredRelation,
    SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::authorship::{AuthorshipColumns, authorship_columns};
use crate::error::{internal, map_err};
use crate::sidecars::{PgSidecarKey, PgSidecarRegistryFrozen};
use crate::verbs::edge_append::{EdgeDraft, append_edge_in_tx};
use crate::verbs::fact_ingest::ingest_fact_command_in_tx;

mod commands;
mod edges;
mod evidence;
mod insert;
mod lifecycle;
mod prior;
mod replay;
mod types;
mod wake;

use edges::{
    append_goal_to_self_edge, append_lifecycle_authored_edge, append_lifecycle_derived_from_edges,
    append_motivated_by_edges, edge_ids_for_goal_relations, edge_ids_for_lifecycle_memory,
    motivated_by_authorship_kind, resolve_relation,
};
use evidence::{
    outgoing_motivated_by_evidence, validate_evidence_in_owner, validate_operator_goal_evidence,
};
use insert::insert_or_replay_goal;
use lifecycle::{
    emit_lifecycle_fact, lifecycle_memory_for_goal, lifecycle_outcome, replay_goal_outcome,
};
use prior::{
    DraftFromPayload, child_draft, dependency_goal_ids, draft_from_payload, draft_from_stored,
    load_prior_goal, validate_active_head, validate_goal_achievement, validate_goal_transition,
};
use replay::{
    CreateGoalReplayExpectation, authorship_matches, ensure_create_goal_replay_side_effects_match,
    existing_goal_body_matches, goal_evidence_edges_match, idempotency_conflict,
};
use types::{
    AuthorshipRow, EvidenceRow, EvidenceTarget, ExistingGoalRow, GoalBodyRow, InsertedGoal,
    StoredGoal, StoredGoalRow, WakeConfigRow, WakeConfigShape, WakeWrite,
};
use wake::{goal_wake_matches, write_goal_wake_config};

pub(crate) use commands::{
    achieve_goal_atomic, create_goal_atomic, decompose_goal_atomic, modify_goal_atomic,
    transition_goal_atomic,
};
