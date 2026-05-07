//! Engine composite — wires FlavorRegistryFrozen, MemoryStore, and
//! an AuthResolver behind the typed verb surfaces of
//! docs/14-protocol-surface.md.

mod builder;
mod dispatcher;
mod goals;
mod ingest;
mod query;

use std::path::PathBuf;
use std::sync::Arc;

use crate::auth::AuthResolver;
use crate::auth::Credentials;
use crate::error::ProtocolError;
use crate::llm::{AnthropicClient, EmbeddingClient};
use crate::storage::{StorageError, StorageHandle};
use crate::verbs::query::MemoryStore;
use crate::verbs::schema::FlavorRegistryFrozen;
use crate::{
    BindInferenceTierRequest, BindInferenceTierResponse, InferenceTargetRow,
    InferenceTierBindingRow, Owner, Principal, RegisterInferenceTargetRequest,
    RegisterInferenceTargetResponse, RemoveInferenceTargetRequest, RemoveInferenceTargetResponse,
    SetWakeEntriesRequest, SetWakeEntriesResponse,
};
pub struct Engine {
    registry: FlavorRegistryFrozen,
    // TODO(M3.B): remove MemoryStore
    memories: MemoryStore,
    auth: Box<dyn AuthResolver>,
    storage: StorageHandle,
    recipes_root: PathBuf,
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

    fn authorize_owner(&self, creds: &Credentials, owner: &Owner) -> Result<(), ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if resolved.can_access_owner(owner) {
            Ok(())
        } else {
            Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ))
        }
    }

    pub async fn register_inference_target(
        &self,
        creds: &Credentials,
        req: &RegisterInferenceTargetRequest,
    ) -> Result<RegisterInferenceTargetResponse, ProtocolError> {
        self.authorize_owner(creds, &req.owner)?;
        crate::inference::register_inference_target::register_inference_target(
            self.storage.as_ref(),
            req,
        )
        .await
    }

    pub async fn list_inference_targets(
        &self,
        creds: &Credentials,
        owner: &Owner,
    ) -> Result<Vec<InferenceTargetRow>, ProtocolError> {
        self.authorize_owner(creds, owner)?;
        crate::inference::list_inference_targets::list_inference_targets(
            self.storage.as_ref(),
            owner,
        )
        .await
    }

    pub async fn remove_inference_target(
        &self,
        creds: &Credentials,
        req: &RemoveInferenceTargetRequest,
    ) -> Result<RemoveInferenceTargetResponse, ProtocolError> {
        self.authorize_owner(creds, &req.owner)?;
        crate::inference::remove_inference_target::remove_inference_target(
            self.storage.as_ref(),
            req,
        )
        .await
    }

    pub async fn bind_inference_tier(
        &self,
        creds: &Credentials,
        req: &BindInferenceTierRequest,
    ) -> Result<BindInferenceTierResponse, ProtocolError> {
        self.authorize_owner(creds, &req.owner)?;
        crate::inference::bind_inference_tier::bind_inference_tier(self.storage.as_ref(), req).await
    }

    pub async fn list_inference_tier_bindings(
        &self,
        creds: &Credentials,
        owner: &Owner,
    ) -> Result<Vec<InferenceTierBindingRow>, ProtocolError> {
        self.authorize_owner(creds, owner)?;
        crate::inference::list_inference_tier_bindings::list_inference_tier_bindings(
            self.storage.as_ref(),
            owner,
        )
        .await
    }

    pub async fn set_wake_entries(
        &self,
        creds: &Credentials,
        req: &SetWakeEntriesRequest,
    ) -> Result<SetWakeEntriesResponse, ProtocolError> {
        self.authorize_owner(creds, &req.owner)?;
        let ctx = crate::inference::set_wake_entries::SetWakeEntriesContext {
            storage: self.storage.as_ref(),
            registry: self.registry(),
            owner_recipes_root: self.owner_recipes_root(&req.owner),
        };
        crate::inference::set_wake_entries::set_wake_entries(&ctx, req).await
    }

    #[must_use]
    pub fn owner_recipes_root(&self, owner: &Owner) -> PathBuf {
        let principal_id = match &owner.principal {
            Principal::User(user) => user.into_inner(),
            Principal::Group(group) => group.into_inner(),
        };
        self.recipes_root.join(principal_id.to_string())
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("registry", &self.registry)
            .field("memories", &self.memories)
            .field("auth", &"<dyn AuthResolver>")
            .field("storage", &"<dyn Storage>")
            .field("recipes_root", &self.recipes_root)
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
    use crate::personality::{PersonalityFlavor, PersonalitySelfDraft};
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
