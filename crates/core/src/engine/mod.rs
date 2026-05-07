//! Engine composite — wires FlavorRegistryFrozen, MemoryStore, and
//! an AuthResolver behind the typed verb surfaces of
//! docs/14-protocol-surface.md.

pub mod agent_loop;
mod builder;
mod dispatcher;
mod goals;
mod ingest;
mod query;

use std::sync::Arc;

use crate::auth::AuthResolver;
use crate::error::ProtocolError;
use crate::llm::{AnthropicClient, EmbeddingClient};
use crate::storage::{StorageError, StorageHandle};
use crate::verbs::query::MemoryStore;
use crate::verbs::schema::FlavorRegistryFrozen;
use crate::{ModelTier, Owner};

pub struct Engine {
    registry: FlavorRegistryFrozen,
    // TODO(M3.B): remove MemoryStore
    memories: MemoryStore,
    auth: Box<dyn AuthResolver>,
    storage: StorageHandle,
    anthropic: Option<Arc<dyn AnthropicClient>>,
    embed: Option<Arc<dyn EmbeddingClient>>,
}

impl Engine {
    #[must_use]
    pub(crate) fn storage(&self) -> &StorageHandle {
        &self.storage
    }

    #[must_use]
    pub(crate) fn embed_client(&self) -> Option<&Arc<dyn EmbeddingClient>> {
        self.embed.as_ref()
    }

    #[must_use]
    pub(crate) fn anthropic(&self) -> Option<&Arc<dyn AnthropicClient>> {
        self.anthropic.as_ref()
    }

    /// Whether a wake for `(owner, tier)` has an executable LLM right
    /// now. The dispatcher gates on this *before* beginning a wake
    /// invocation so a missing client defers the wake instead of
    /// consuming it.
    ///
    /// v1 only has a single engine-wide Anthropic client, so the answer
    /// reduces to "is one wired". Once per-Principal model settings
    /// land (api keys, tier→provider routing), this resolves against
    /// the owner's configured providers and short-circuits per tier.
    #[must_use]
    pub(crate) fn llm_available_for_wake(&self, owner: &Owner, tier: ModelTier) -> bool {
        let _ = (owner, tier);
        self.anthropic.is_some()
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("registry", &self.registry)
            .field("memories", &self.memories)
            .field("auth", &"<dyn AuthResolver>")
            .field("storage", &"<dyn Storage>")
            .finish()
    }
}

pub(super) fn map_storage_err_for_goal_write(
    request_id: &str,
) -> impl FnOnce(StorageError) -> ProtocolError + '_ {
    move |e| match e {
        StorageError::ConstraintViolation(msg) if msg.starts_with("idempotency_conflict:") => {
            ProtocolError::idempotency_conflict(request_id)
        }
        StorageError::NotFound => ProtocolError::not_found("prior goal not found"),
        other => ProtocolError::internal(other.to_string()),
    }
}

#[cfg(test)]
mod tier_union_tests {
    use super::*;
    use crate::auth::NoAuth;
    use crate::ids::{OrgId, UserId};
    use crate::personality::{PersonalityFlavor, PersonalitySelfDraft, WakeFilter};
    use crate::verbs::query::MemoryStore;
    use crate::{FlavorRegistry, LlmCaps, ModelTier, Owner, Principal, SchemaId, SchemaVersion};
    use async_trait::async_trait;
    use uuid::Uuid;

    #[derive(Debug)]
    struct OpAt {
        tier: ModelTier,
        requires: LlmCaps,
    }
    #[async_trait]
    impl PersonalityFlavor for OpAt {
        fn personality_type_id(&self) -> &'static str {
            "test/personality"
        }

        fn self_schema(&self) -> SchemaId {
            SchemaId::new("test/self".into())
        }

        fn default_self_payload(
            &self,
            _owner: &Owner,
            _payload_overrides: Option<&serde_json::Value>,
        ) -> Result<PersonalitySelfDraft, crate::ProtocolError> {
            Ok(PersonalitySelfDraft {
                schema_id: self.self_schema(),
                schema_version: SchemaVersion::new(1),
                text: "test".into(),
                typed_payload: serde_json::json!({}),
            })
        }

        fn system_prompt(&self) -> &'static str {
            "test"
        }

        fn writeable_schemas(&self) -> &'static [&'static str] {
            &[]
        }

        fn writeable_relations(&self) -> &'static [&'static str] {
            &[]
        }

        fn default_wake_filters(&self) -> Vec<WakeFilter> {
            Vec::new()
        }

        fn tier(&self) -> ModelTier {
            self.tier
        }

        fn requires(&self) -> LlmCaps {
            self.requires
        }
    }

    fn engine_with_ops(ops: Vec<OpAt>) -> Engine {
        let mut reg = FlavorRegistry::new();
        reg.add_flavor(crate::FlavorDescriptor {
            flavor_id: "test".to_string(),
            display_name: "Test".to_string(),
            package_version: "0.0.0".to_string(),
            author: None,
            provenance: crate::FlavorProvenance::Builtin,
        });
        for op in ops {
            reg.add_personality(op);
        }
        let reg = reg.freeze();
        let principal = Principal::User(UserId::new(Uuid::now_v7()));
        let owner = Owner {
            principal: principal.clone(),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        Engine::new(
            reg,
            MemoryStore::new(),
            Box::new(NoAuth::new(principal, owner)),
        )
    }

    #[test]
    fn union_empty_when_no_ops_at_tier() {
        let eng = engine_with_ops(vec![OpAt {
            tier: ModelTier::Fast,
            requires: LlmCaps {
                tool_use: true,
                ..LlmCaps::none()
            },
        }]);
        assert_eq!(
            eng.tier_requires_union(ModelTier::Standard),
            LlmCaps::none()
        );
    }

    #[test]
    fn union_combines_caps_across_ops_at_same_tier() {
        let eng = engine_with_ops(vec![
            OpAt {
                tier: ModelTier::Standard,
                requires: LlmCaps {
                    tool_use: true,
                    ..LlmCaps::none()
                },
            },
            OpAt {
                tier: ModelTier::Standard,
                requires: LlmCaps {
                    json_mode: true,
                    ..LlmCaps::none()
                },
            },
            OpAt {
                tier: ModelTier::Deep,
                requires: LlmCaps {
                    vision: true,
                    ..LlmCaps::none()
                },
            },
        ]);
        let standard = eng.tier_requires_union(ModelTier::Standard);
        assert!(standard.tool_use);
        assert!(standard.json_mode);
        assert!(!standard.vision); // vision was on Deep, not Standard
    }
}
