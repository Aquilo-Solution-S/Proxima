use std::path::PathBuf;
use std::sync::Arc;

use super::Engine;
use crate::auth::AuthResolver;
use crate::llm::{AnthropicClient, EmbeddingClient};
use crate::storage::{NoopStorage, StorageHandle};
use crate::verbs::query::MemoryStore;
use crate::verbs::schema::FlavorRegistryFrozen;

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
            recipes_root: default_recipes_root(),
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

    #[must_use]
    pub fn with_recipes_root(mut self, recipes_root: PathBuf) -> Self {
        self.recipes_root = recipes_root;
        self
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

fn default_recipes_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".proxima/recipes")
}
