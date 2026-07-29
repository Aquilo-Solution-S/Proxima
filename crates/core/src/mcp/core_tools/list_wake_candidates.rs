//! `proxima://wake-candidates` — wake-candidate admission read.
//!
//! Given one readable trigger Fact, returns armed Active Goal heads whose
//! wake trigger matches it (04 §Execution model). This is the pull half of
//! the harness wake loop: poll `proxima://change-events`, then ask which
//! Goals wake on each appended Fact. Read model only — no scheduler,
//! executor, or invocation ledger exists behind it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{ListWakeCandidatesReadRequest, MAX_WAKE_CANDIDATE_LIMIT};
use crate::mcp::{McpToolCtx, McpToolError};
use crate::read_models::GoalWakeCandidate;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListWakeCandidatesArgs {
    /// Trigger Fact reference (`F:<uuid>`), exactly as emitted by
    /// `proxima://change-events`. Non-Fact references are rejected at parse.
    pub fact: String,
    /// Max candidates; values above 200 are clamped, 0 is rejected,
    /// default 50.
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ListWakeCandidatesOutput {
    pub candidates: Vec<WakeCandidateItem>,
    /// More admitted candidates exist past `limit`; re-poll with a
    /// higher limit (max 200) or narrow the trigger.
    pub has_more: bool,
}

#[derive(Debug, Serialize)]
pub struct WakeCandidateItem {
    /// Armed Active Goal head admitted for wake planning.
    pub goal: String,
    /// Stored wake prompt for the external harness.
    pub prompt: String,
    /// Configured wake toolset; already narrowed to the caller's tool scope.
    pub tool_ids: Vec<String>,
    /// Pinned wake-context memory references (`F:`/`A:`/`P:` prefixed ids);
    /// hydrate via `proxima://memory/{id}`.
    pub hard_memories: Vec<String>,
    /// Owner keys (`personal:<uuid>`/`group:<uuid>`) the caller may write to.
    pub actor_write_owners: Vec<String>,
}

/// # Errors
///
/// Returns invalid trigger reference, authorization, or storage failures.
pub async fn list_wake_candidates(
    ctx: McpToolCtx,
    args: ListWakeCandidatesArgs,
) -> Result<ListWakeCandidatesOutput, McpToolError> {
    let trigger_fact_id = ctx.resolve_fact_memory(&args.fact)?;
    let limit = super::resolve_page_limit(args.limit)? as usize;
    debug_assert!(limit <= MAX_WAKE_CANDIDATE_LIMIT);
    let engine = ctx.require_engine()?;
    let response = engine
        .list_goal_wake_candidates(
            &ctx.authz,
            &ListWakeCandidatesReadRequest {
                trigger_fact_id,
                limit,
            },
        )
        .await?;
    Ok(ListWakeCandidatesOutput {
        has_more: response.has_more,
        candidates: response
            .candidates
            .into_iter()
            .map(|candidate| candidate_item(&ctx, candidate))
            .collect(),
    })
}

fn candidate_item(ctx: &McpToolCtx, candidate: GoalWakeCandidate) -> WakeCandidateItem {
    WakeCandidateItem {
        goal: ctx.format_goal(candidate.goal_id),
        prompt: candidate.prompt,
        tool_ids: candidate.tool_ids,
        hard_memories: candidate
            .hard_memories
            .into_iter()
            .map(|hard| {
                super::wire_ref::format_memory_by_kind(ctx, hard.memory_id, Some(hard.kind))
            })
            .collect(),
        actor_write_owners: candidate
            .actor_write_owners
            .into_iter()
            .map(crate::OwnerRef::external_key)
            .collect(),
    }
}
