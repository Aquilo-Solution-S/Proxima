use std::sync::Arc;

use super::Engine;
use crate::auth::AuthResolver;
use crate::operators::{EmbeddingClient, LlmClient, OperatorRegistry};
use crate::storage::{NoopStorage, StorageHandle};
use crate::verbs::query::MemoryStore;
use crate::verbs::schema::SchemaRegistry;
use crate::{LlmCaps, ModelTier};

impl Engine {
    pub fn new(
        registry: SchemaRegistry,
        memories: MemoryStore,
        auth: Box<dyn AuthResolver>,
    ) -> Self {
        Self {
            registry,
            memories,
            auth,
            storage: Arc::new(NoopStorage),
            operators: OperatorRegistry::new(),
            llm: None,
            embed: None,
        }
    }

    /// Get a reference to the schema registry.
    #[must_use]
    pub fn registry(&self) -> &SchemaRegistry {
        &self.registry
    }

    #[must_use]
    pub fn with_storage(mut self, storage: StorageHandle) -> Self {
        self.storage = storage;
        self
    }

    /// Register operators (M5: F→A only). Bare-Engine without
    /// operators behaves identically to M4 — `close_batch` flips
    /// `closed_at` and returns. With operators registered AND an
    /// LLM + embed client wired in, `close_batch` also runs F→A
    /// consolidation inline (docs/04 §"F→A").
    #[must_use]
    pub fn with_operators(mut self, registry: OperatorRegistry) -> Self {
        self.operators = registry;
        self
    }

    /// Union of `requires()` over all F→A operators bound to `tier`.
    /// Returns `LlmCaps::none()` if no operator uses that tier — the
    /// caller (runtime credential-write validation) treats that as
    /// "any model satisfies".
    ///
    /// Walks `self.operators.f2a_operators()`. As A→P / A→Goal /
    /// Edge operator slots land, this method extends to walk those
    /// too — for now, F→A is the only populated slot.
    #[must_use]
    pub fn tier_requires_union(&self, tier: ModelTier) -> LlmCaps {
        let mut acc = LlmCaps::none();
        for op in self.operators.f2a_operators() {
            if op.tier() == tier {
                let r = op.requires();
                acc.tool_use |= r.tool_use;
                acc.json_mode |= r.json_mode;
                acc.long_context |= r.long_context;
                acc.vision |= r.vision;
            }
        }
        acc
    }

    #[must_use]
    pub fn with_llm(mut self, llm: Arc<dyn LlmClient>) -> Self {
        self.llm = Some(llm);
        self
    }

    #[must_use]
    pub fn with_embed(mut self, embed: Arc<dyn EmbeddingClient>) -> Self {
        self.embed = Some(embed);
        self
    }
}
