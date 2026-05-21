use std::collections::{HashMap, HashSet};

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;

use crate::approval::{
    ApprovalDecision, ApprovalEligibleVoter, ApprovalRequirement, ApprovalTargetKind,
    ApprovalVoteVerdict, ApprovalVoterKind,
};
use crate::mcp::{McpTool, McpToolCtx, McpToolError, MemoryHandleClass};
use crate::personality::{
    PersonalityInstanceId, PersonalityStatus, WakeEntryRow, WakeEntryTriggerKind,
    writeable_schemas_for_palette,
};
use crate::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use crate::{
    AbstractionPayload, CORE_DERIVED_FROM_RELATION, CORE_HAS_APPROVAL_DECISION_RELATION,
    CORE_HAS_APPROVAL_POLICY_RELATION, CORE_RECEIVES_CHAT_END_REQUEST_RELATION,
    CORE_RECEIVES_CHAT_MESSAGE_RELATION, CORE_REPLIES_TO_MESSAGE_RELATION, CORE_VOTES_ON_RELATION,
    EdgeAuthorshipKind, EdgeId, Engine, EntityKind, FactPayload, GoalId, MemoryId,
    MemoryOperatorKind, Owner, OwnerPrincipalKind, Principal, SchemaId, SchemaVersion,
    SourceBatchId, SourceId, StorageError,
};

mod coordination;
mod payloads;
mod targets;
mod thread;
mod tools;
mod validation;
mod write;

pub use coordination::*;
pub use payloads::*;
pub use thread::*;
pub use tools::*;

use targets::*;
use thread::{
    entity_kind_for_class_map, load_thread_ended, load_thread_started, resolve_thread_key_arg,
};
use validation::*;
use write::*;
