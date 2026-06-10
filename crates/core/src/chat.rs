use std::collections::{HashMap, HashSet};

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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
use crate::{
    AbstractionPayload, EdgeAuthorshipKind, EdgeId, Engine, EntityKind, FactPayload, GoalId,
    MemoryId, Owner, OwnerPrincipalKind, Principal, Storage, StorageError,
};

mod coordination;
mod payloads;
mod store;
mod targets;
mod thread;
mod tools;
mod validation;
mod write;

pub use coordination::*;
pub use payloads::*;
pub use store::*;
pub use thread::*;
pub use tools::*;

use targets::{
    is_enabled_chat_message_wake, list_chat_targets, load_chat_parent_thread_key, load_end_request,
    load_message, load_started_target, resolve_chat_target, resolve_end_request_target,
    resolve_personality_for_self, thread_is_ended,
};
use thread::{
    entity_kind_for_class, entity_kind_for_class_map, load_thread_ended, load_thread_started,
    resolve_thread_key_arg,
};
use validation::{
    load_existing_end_by_request, load_memory_handle_classes, load_summary_source_memory_ids,
    resolve_context_goals, resolve_context_memories, validate_chat_source_memories,
};
use write::{
    chat_compaction_memory_id, chat_storage, chat_summary_memory_id, edge_authorship_for_ctx,
    normalize_text,
};
