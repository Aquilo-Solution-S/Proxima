use std::sync::Arc;

use super::Engine;
use crate::auth::AuthResolver;
use crate::llm::{AnthropicClient, EmbeddingClient};
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
            anthropic: None,
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

    /// Union of `requires()` over all personalities bound to `tier`.
    /// Returns `LlmCaps::none()` if no operator uses that tier — the
    /// caller (runtime credential-write validation) treats that as
    /// "any model satisfies".
    ///
    #[must_use]
    pub fn tier_requires_union(&self, tier: ModelTier) -> LlmCaps {
        let mut acc = LlmCaps::none();
        for personality in self.registry.list_personalities() {
            if personality.tier() == tier {
                let r = personality.requires();
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
            .list_personalities()
            .iter()
            .any(|personality| personality.tier() == tier)
    }

    #[must_use]
    pub fn with_embed(mut self, embed: Arc<dyn EmbeddingClient>) -> Self {
        self.embed = Some(embed);
        self
    }

    #[must_use]
    pub fn with_anthropic(mut self, anthropic: Arc<dyn AnthropicClient>) -> Self {
        self.anthropic = Some(anthropic);
        self
    }
}
