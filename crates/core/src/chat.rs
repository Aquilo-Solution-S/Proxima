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

use targets::*;
use thread::{
    entity_kind_for_class, entity_kind_for_class_map, load_thread_ended, load_thread_started,
    resolve_thread_key_arg,
};
use validation::*;
use write::*;
