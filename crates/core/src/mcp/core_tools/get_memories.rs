//! `proxima://memories` — batch memory read by prefixed ids.
//!
//! Closes the wake-candidates hydration gap: `hard_memories` hands the
//! caller N prefixed ids, and until this surface each one cost a separate
//! `proxima://memory/{id}` round trip. One call returns the visible
//! subset plus an explicit `missing` list; not-exists and not-visible are
//! deliberately indistinguishable, mirroring the single get.

use std::collections::{BTreeSet, HashMap};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::MemoryId;
use crate::engine::GetMemoriesReadRequest;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::read_models::MemorySnapshot;

use super::get_memory::{GetMemoryOutput, project_memory_snapshot};

pub const MAX_BATCH_GET_MEMORIES: usize = 100;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetMemoriesArgs {
    /// Memory references (`F:`/`A:`/`P:` prefixed ids); at least one, at
    /// most 100 per call. On the resource, pass as the comma-separated
    /// `ids` query parameter.
    pub memories: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct GetMemoriesOutput {
    /// Found memories, in request order (duplicate ids collapse into the
    /// first occurrence).
    pub memories: Vec<GetMemoryOutput>,
    /// Requested ids that do not exist or are not visible to the caller
    /// (deliberately indistinguishable), in request order.
    pub missing: Vec<String>,
}

/// # Errors
///
/// Returns invalid memory references, an empty or over-cap id list,
/// authorization, or storage failures. Missing memories are NOT errors —
/// they land in [`GetMemoriesOutput::missing`].
pub async fn get_memories(
    ctx: McpToolCtx,
    args: GetMemoriesArgs,
) -> Result<GetMemoriesOutput, McpToolError> {
    if args.memories.is_empty() {
        return Err(McpToolError::InvalidInput(
            "provide at least one memory id".into(),
        ));
    }
    if args.memories.len() > MAX_BATCH_GET_MEMORIES {
        return Err(McpToolError::InvalidInput(format!(
            "at most {MAX_BATCH_GET_MEMORIES} memory ids per call; got {}",
            args.memories.len()
        )));
    }
    // Resolve every handle up front: a malformed reference fails the whole
    // call (caller bug), unlike a well-formed id that is merely missing.
    let mut requested: Vec<(MemoryId, &str)> = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in &args.memories {
        let id = ctx.resolve_memory(raw)?;
        if seen.insert(id) {
            requested.push((id, raw));
        }
    }
    let engine = ctx.require_engine()?;
    let response = engine
        .get_memories(
            &ctx.authz,
            &GetMemoriesReadRequest {
                memory_ids: requested.iter().map(|(id, _)| *id).collect(),
            },
        )
        .await?;
    let mut by_id: HashMap<MemoryId, MemorySnapshot> = response
        .memories
        .into_iter()
        .map(|snapshot| (snapshot.memory_id, snapshot))
        .collect();

    // One resolution for the whole batch: same caller, same owner, so the
    // reported key cannot differ per row. See `get_memory` for why this is
    // not the literal "entry".
    let output_space = super::memory_spaces::resolve_space_owner(
        &ctx,
        None,
        super::memory_spaces::SpaceDefault::Current,
    )?
    .key;

    let mut memories = Vec::with_capacity(requested.len());
    let mut missing = Vec::new();
    for (id, raw) in requested {
        match by_id.remove(&id) {
            Some(snapshot) => {
                memories.push(project_memory_snapshot(
                    &ctx,
                    snapshot,
                    output_space.clone(),
                    None,
                )?);
            }
            None => missing.push(raw.to_string()),
        }
    }
    Ok(GetMemoriesOutput { memories, missing })
}
