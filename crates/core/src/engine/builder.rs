use std::sync::Arc;

use super::Engine;
use crate::auth::AuthResolver;
use crate::operators::{EmbeddingClient, LlmClient};
use crate::storage::{NoopStorage, StorageHandle};
use crate::verbs::query::MemoryStore;
use crate::verbs::schema::FlavorRegistryFrozen;
use crate::{LlmCaps, ModelTier};

impl Engine {
    pub fn new(
        registry: FlavorRegistryFrozen,
        memories: MemoryStore,
        auth: Box<dyn AuthResolver>,
    ) -> Self {
        Self {
            registry,
            memories,
            auth,
            storage: Arc::new(NoopStorage),
            llms: std::collections::HashMap::new(),
            embed: None,
        }
    }

    /// Get a reference to the schema registry.
    #[must_use]
    pub fn registry(&self) -> &FlavorRegistryFrozen {
        &self.registry
    }

    #[must_use]
    pub fn with_storage(mut self, storage: StorageHandle) -> Self {
        self.storage = storage;
        self
    }

    /// Union of `requires()` over all operators bound to `tier`.
    /// Returns `LlmCaps::none()` if no operator uses that tier — the
    /// caller (runtime credential-write validation) treats that as
    /// "any model satisfies".
    ///
    #[must_use]
    pub fn tier_requires_union(&self, tier: ModelTier) -> LlmCaps {
        let mut acc = LlmCaps::none();
        for op in self.registry.list_f2a_operators() {
            if op.tier() == tier {
                let r = op.requires();
                acc.tool_use |= r.tool_use;
                acc.json_mode |= r.json_mode;
                acc.long_context |= r.long_context;
                acc.vision |= r.vision;
            }
        }
        for op in self.registry.list_a2p_operators() {
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
    pub fn uses_llm_tier(&self, tier: ModelTier) -> bool {
        self.registry
            .list_f2a_operators()
            .iter()
            .any(|op| op.tier() == tier)
            || self
                .registry
                .list_a2p_operators()
                .iter()
                .any(|op| op.tier() == tier)
    }

    #[must_use]
    pub fn with_llm(mut self, llm: Arc<dyn LlmClient>) -> Self {
        for tier in [ModelTier::Fast, ModelTier::Standard, ModelTier::Deep] {
            self.llms.insert(tier, llm.clone());
        }
        self
    }

    #[must_use]
    pub fn with_llm_for_tier(mut self, tier: ModelTier, llm: Arc<dyn LlmClient>) -> Self {
        self.llms.insert(tier, llm);
        self
    }

    pub(crate) fn llm_for_tier(&self, tier: ModelTier) -> Option<&Arc<dyn LlmClient>> {
        self.llms.get(&tier)
    }

    #[must_use]
    pub fn with_embed(mut self, embed: Arc<dyn EmbeddingClient>) -> Self {
        self.embed = Some(embed);
        self
    }
}
