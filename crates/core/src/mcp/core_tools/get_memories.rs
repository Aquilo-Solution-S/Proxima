//! `proxima://memories` — batch memory read by UUIDs.
//!
//! Hydrates a bounded set of memory UUIDs in one call. Returns the visible
//! subset plus an explicit `missing` list; not-exists and not-visible are
//! deliberately indistinguishable, mirroring the single get. Ids on another
//! store that did not answer (failed, timed out, or answered for a different
//! memory) are listed in `unanswered` instead: retry them, they may exist.

use std::collections::{BTreeSet, HashMap};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::MemoryId;
use crate::engine::GetMemoriesReadRequest;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::read_models::MemorySnapshot;
use crate::storage_ports::other_stores::{OtherStoresError, RemoteMemory, RemoteRead};

use super::get_memory::{GetMemoryOutput, project_memory_snapshot, project_remote_memory};

pub const MAX_BATCH_GET_MEMORIES: usize = 100;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetMemoriesArgs {
    /// Memory references (bare UUIDs); at least one, at
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
    /// (deliberately indistinguishable), in request order. Includes ids on
    /// an unknown store (no existence leak).
    pub missing: Vec<String>,
    /// Requested ids on another store that did not answer in time (failed,
    /// timed out, or answered for a different memory), in request order.
    /// Retryable: nothing is known about whether they exist.
    pub unanswered: Vec<String>,
}

/// # Errors
///
/// Returns invalid memory references, an empty or over-cap id list,
/// authorization, or storage failures. Missing memories are NOT errors —
/// they land in [`GetMemoriesOutput::missing`]; neither are other stores that
/// did not answer — those land in [`GetMemoriesOutput::unanswered`].
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
                memory_ids: requested
                    .iter()
                    .map(|(id, _)| *id)
                    .filter(|id| {
                        crate::memory_store_number(id.into_inner()) == engine.store_number()
                    })
                    .collect(),
            },
        )
        .await?;
    let mut by_id: HashMap<MemoryId, MemorySnapshot> = response
        .memories
        .into_iter()
        .map(|snapshot| (snapshot.memory_id, snapshot))
        .collect();
    let mut remote = read_remote_memories(&ctx, &requested, engine.store_number()).await;

    let mut memories = Vec::with_capacity(requested.len());
    let mut missing = Vec::new();
    let mut unanswered = Vec::new();
    for (id, raw) in requested {
        if crate::memory_store_number(id.into_inner()) != engine.store_number() {
            match remote.remove(&id) {
                Some(RemoteAnswer::Found(memory)) => {
                    memories.push(project_remote_memory(*memory));
                }
                Some(RemoteAnswer::Unanswered) => unanswered.push(raw.to_string()),
                None => missing.push(raw.to_string()),
            }
            continue;
        }
        match by_id.remove(&id) {
            Some(snapshot) => {
                // Each row reports its own memory's space.
                memories.push(project_memory_snapshot(&ctx, snapshot, None)?);
            }
            None => missing.push(raw.to_string()),
        }
    }
    Ok(GetMemoriesOutput {
        memories,
        missing,
        unanswered,
    })
}

/// What another store said about one requested id; absent = missing.
enum RemoteAnswer {
    Found(Box<RemoteMemory>),
    Unanswered,
}

/// One bounded remote batch. Not-found, erased and unknown-store ids are
/// absent (missing); every other failure is [`RemoteAnswer::Unanswered`].
async fn read_remote_memories(
    ctx: &McpToolCtx,
    requested: &[(MemoryId, &str)],
    local_number: u16,
) -> HashMap<MemoryId, RemoteAnswer> {
    let Some(service) = super::other_stores::service(ctx) else {
        return HashMap::new();
    };
    let ids = requested
        .iter()
        .map(|(id, _)| id.into_inner())
        .filter(|id| crate::memory_store_number(*id) != local_number)
        .collect::<Vec<_>>();
    let reads = service
        .read_many_bounded(super::other_stores::caller(ctx), &ids)
        .await;
    ids.iter()
        .zip(reads)
        .filter_map(|(id, read)| match read {
            Ok(RemoteRead::Found(memory)) => {
                Some((MemoryId::new(*id), RemoteAnswer::Found(memory)))
            }
            Ok(RemoteRead::NotFound | RemoteRead::Erased)
            | Err(OtherStoresError::UnknownStore(_)) => None,
            Err(_) => Some((MemoryId::new(*id), RemoteAnswer::Unanswered)),
        })
        .collect()
}

#[cfg(test)]
mod tests;
