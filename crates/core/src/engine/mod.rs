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
