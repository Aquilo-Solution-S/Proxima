//! Personality tool context.
//!
//! This module contains the context type passed to personality tools:
//! - `PersonalityToolContext` - Context for tool invocations

use std::sync::Arc;

use crate::mcp::HandleTable;
use crate::personality::tool::PersonalityTool;
use crate::personality::types::WakeChainDepth;
use crate::{Engine, MemoryId, Owner};

use super::personality::PersonalityInstanceId;

/// Context passed to personality tool invocations.
#[derive(Debug, Clone)]
pub struct PersonalityToolContext<'a> {
    pub engine: &'a Engine,
    pub owner: &'a Owner,
    pub type_id: &'a str,
    pub instance_id: PersonalityInstanceId,
    pub current_root_perspective_memory_id: MemoryId,
    pub triggering_event_memory_id: MemoryId,
    pub triggering_event_depth: WakeChainDepth,
    pub writeable_schemas: Vec<String>,
    pub writeable_relations: Vec<String>,
    pub palette: &'a [Arc<dyn PersonalityTool>],
    /// Wake invocation bound by the MCP substrate handler after
    /// extracting the wake token from request extensions. Provenance-
    /// stamping substrate tools read `model_id` from here so the row
    /// reflects the InferenceTarget that drove the wake instead of a
    /// static `Standard`-tier guess. `None` only in unit tests.
    pub wake_invocation: Option<&'a crate::wake::token_store::WakeTokenContext>,
    read_log: Arc<tokio::sync::Mutex<Vec<(MemoryId, WakeChainDepth)>>>,
    /// Per-wake handle table. In production this is
    /// `wake.handles.clone()`. In unit tests without a wake it is a
    /// fresh empty table; tests exercising handle behavior pre-seed
    /// it manually.
    pub handles: Arc<HandleTable>,
}

impl<'a> PersonalityToolContext<'a> {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        engine: &'a Engine,
        owner: &'a Owner,
        type_id: &'a str,
        instance_id: PersonalityInstanceId,
        current_root_perspective_memory_id: MemoryId,
        triggering_event_memory_id: MemoryId,
        triggering_event_depth: WakeChainDepth,
        writeable_schemas: Vec<String>,
        writeable_relations: Vec<String>,
        palette: &'a [Arc<dyn PersonalityTool>],
        handles: Arc<HandleTable>,
    ) -> Self {
        Self {
            engine,
            owner,
            type_id,
            instance_id,
            current_root_perspective_memory_id,
            triggering_event_memory_id,
            triggering_event_depth,
            writeable_schemas,
            writeable_relations,
            palette,
            wake_invocation: None,
            read_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            handles,
        }
    }

    /// Bind the active `WakeTokenContext` for the duration of this tool
    /// dispatch. The MCP handler calls this after extracting the wake
    /// token from request extensions.
    #[must_use]
    pub fn with_wake_invocation(
        mut self,
        wake_invocation: &'a crate::wake::token_store::WakeTokenContext,
    ) -> Self {
        self.wake_invocation = Some(wake_invocation);
        self
    }

    #[must_use]
    pub fn with_read_log(
        mut self,
        read_log: Arc<tokio::sync::Mutex<Vec<(MemoryId, WakeChainDepth)>>>,
    ) -> Self {
        self.read_log = read_log;
        self
    }

    pub(crate) async fn record_read(
        &self,
        ids: impl IntoIterator<Item = (MemoryId, WakeChainDepth)>,
    ) {
        let mut log = self.read_log.lock().await;
        log.extend(ids);
    }

    pub(crate) async fn snapshot_provenance(&self) -> (Vec<MemoryId>, WakeChainDepth) {
        let log = self.read_log.lock().await;
        let mut provenance = Vec::with_capacity(log.len() + 1);
        provenance.push(self.triggering_event_memory_id);
        let mut depth = self.triggering_event_depth;
        for (memory_id, memory_depth) in log.iter().copied() {
            if !provenance.contains(&memory_id) {
                provenance.push(memory_id);
            }
            depth = depth.max(memory_depth);
        }
        (provenance, depth.next_after())
    }
}
