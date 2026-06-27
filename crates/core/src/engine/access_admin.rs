//! Owner-gated entry-access grant management and public marketplace browse.
//!
//! Grant-management is owner-only by design: it closes admin self-escalation.
//! Flavors layer restrictive policy via the [`AuthzOperation`] deny-hook
//! (review-gated publish, restricted sharing). Delegation of sharing is
//! co-ownership through `add_owner`; `owner` dominates grant-management and
//! transfer. A finer non-owner sharing-admin relation is a future extension.
//! Core deliberately has no allow/widening hook; a hook that can grant access
//! would reopen the escalation surface this model closes.

use crate::access::{
    AccessGrantRow, AccessScope, EntryVisibilityTarget, GrantResource, GrantSelector, GrantSubject,
    NewAccessGrant, Relation, RelationSelector, RemoveOwnerOutcome, Visibility,
};
use crate::authz::{AuthPath, AuthzContext, AuthzInput, AuthzOperation};
use crate::error::ProtocolError;
use crate::personality::{MemorySnapshot, PersonalityInstanceId};
use crate::storage::StorageError;
use crate::{MemoryId, Owner, Principal};

use super::{Engine, MemoryPermit};

struct ShareEntryGrant {
    memory_id: MemoryId,
    current_visibility: Visibility,
    subject: GrantSubject,
    relation: Relation,
}

impl Engine {
    /// Share one entry with a principal or group subject.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when the entry is absent/tombstoned, `Forbidden`
    /// when the caller is not an owner of the resolved entry owner-space,
    /// `InvalidArgument` when `relation` is not grantable, or `Internal`
    /// for storage failures.
    pub async fn share_entry(
        &self,
        authz: &AuthzContext,
        memory_id: MemoryId,
        subject: GrantSubject,
        relation: Relation,
    ) -> Result<(), ProtocolError> {
        let facts = self.resolve_entry_access_facts(memory_id).await?;
        let permit = self
            .authorize_request(authz, &facts.owner, Relation::Owner)
            .await?;
        self.share_entry_authorized(
            authz,
            &permit,
            ShareEntryGrant {
                memory_id,
                current_visibility: facts.visibility,
                subject,
                relation,
            },
        )
        .await
    }

    async fn share_entry_authorized(
        &self,
        authz: &AuthzContext,
        permit: &MemoryPermit,
        grant: ShareEntryGrant,
    ) -> Result<(), ProtocolError> {
        reject_entry_grant_relation(grant.relation)?;
        self.run_access_admin_hooks(
            authz,
            permit.owner(),
            AuthzOperation::ShareEntry {
                memory_id: grant.memory_id,
                subject: grant.subject.clone(),
                relation: grant.relation,
            },
        )?;
        let granted_by = self.grant_author_personality(permit.owner(), authz).await?;
        self.storage
            .share_entry_atomic(
                &NewAccessGrant {
                    space_owner: permit.owner().clone(),
                    resource: GrantResource::Memory(grant.memory_id),
                    relation: grant.relation,
                    subject: grant.subject,
                    granted_by,
                },
                grant.current_visibility == Visibility::Private,
            )
            .await
            .map_err(|err| storage_error("share_entry_atomic", &err))
    }

    /// Revoke all entry-level grants for one subject on one entry.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when the entry is absent/tombstoned, `Forbidden`
    /// when the caller is not an owner of the resolved entry owner-space, or
    /// `Internal` for storage failures.
    pub async fn unshare_entry(
        &self,
        authz: &AuthzContext,
        memory_id: MemoryId,
        subject: GrantSubject,
    ) -> Result<(), ProtocolError> {
        let facts = self.resolve_entry_access_facts(memory_id).await?;
        let permit = self
            .authorize_request(authz, &facts.owner, Relation::Owner)
            .await?;
        self.unshare_entry_authorized(&permit, memory_id, subject)
            .await
    }

    async fn unshare_entry_authorized(
        &self,
        permit: &MemoryPermit,
        memory_id: MemoryId,
        subject: GrantSubject,
    ) -> Result<(), ProtocolError> {
        self.storage
            .unshare_entry_atomic(&GrantSelector {
                space_owner: permit.owner().clone(),
                resource: GrantResource::Memory(memory_id),
                relation: RelationSelector::AllGrantable,
                subject,
            })
            .await
            .map_err(|err| storage_error("unshare_entry_atomic", &err))?;
        Ok(())
    }

    /// Set one entry's visibility. This is the only owner-gated verb that can
    /// move an entry to or from `Public`.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when the entry is absent/tombstoned, `Forbidden`
    /// when the caller is not an owner of the resolved entry owner-space, or
    /// `Internal` for storage failures.
    pub async fn set_entry_visibility(
        &self,
        authz: &AuthzContext,
        memory_id: MemoryId,
        target: EntryVisibilityTarget,
    ) -> Result<(), ProtocolError> {
        let facts = self.resolve_entry_access_facts(memory_id).await?;
        let permit = self
            .authorize_request(authz, &facts.owner, Relation::Owner)
            .await?;
        self.set_entry_visibility_authorized(authz, &permit, memory_id, target)
            .await
    }

    async fn set_entry_visibility_authorized(
        &self,
        authz: &AuthzContext,
        permit: &MemoryPermit,
        memory_id: MemoryId,
        target: EntryVisibilityTarget,
    ) -> Result<(), ProtocolError> {
        self.run_access_admin_hooks(
            authz,
            permit.owner(),
            AuthzOperation::SetEntryVisibility { memory_id, target },
        )?;
        self.storage
            .set_memory_visibility(permit.owner(), memory_id, target.into())
            .await
            .map_err(|err| storage_error("set_memory_visibility", &err))
    }

    /// Grant a relation on a space to a principal or group subject.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller is not an owner of `space`,
    /// `InvalidArgument` when `relation` is not grantable, or `Internal`
    /// for storage failures.
    pub async fn set_space_binding(
        &self,
        authz: &AuthzContext,
        space: Owner,
        subject: GrantSubject,
        relation: Relation,
    ) -> Result<(), ProtocolError> {
        let permit = self
            .authorize_request(authz, &space, Relation::Owner)
            .await?;
        self.set_space_binding_authorized(authz, &permit, subject, relation)
            .await
    }

    async fn set_space_binding_authorized(
        &self,
        authz: &AuthzContext,
        permit: &MemoryPermit,
        subject: GrantSubject,
        relation: Relation,
    ) -> Result<(), ProtocolError> {
        reject_space_grant_relation(relation)?;
        self.run_access_admin_hooks(
            authz,
            permit.owner(),
            AuthzOperation::SetSpaceBinding {
                subject: subject.clone(),
                relation,
            },
        )?;
        let granted_by = self.grant_author_personality(permit.owner(), authz).await?;
        self.storage
            .insert_space_binding(&NewAccessGrant {
                space_owner: permit.owner().clone(),
                resource: GrantResource::Space,
                relation,
                subject,
                granted_by,
            })
            .await
            .map_err(|err| storage_error("insert_space_binding", &err))
    }

    /// Revoke all space-level grants for one subject.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller is not an owner of `space`, or
    /// `Internal` for storage failures.
    pub async fn revoke_space_binding(
        &self,
        authz: &AuthzContext,
        space: Owner,
        subject: GrantSubject,
    ) -> Result<(), ProtocolError> {
        let permit = self
            .authorize_request(authz, &space, Relation::Owner)
            .await?;
        self.revoke_space_binding_authorized(&permit, subject).await
    }

    async fn revoke_space_binding_authorized(
        &self,
        permit: &MemoryPermit,
        subject: GrantSubject,
    ) -> Result<(), ProtocolError> {
        self.storage
            .revoke_access_grants(&GrantSelector {
                space_owner: permit.owner().clone(),
                resource: GrantResource::Space,
                relation: RelationSelector::AllGrantable,
                subject,
            })
            .await
            .map_err(|err| storage_error("revoke_access_grants", &err))?;
        Ok(())
    }

    /// List active grants on a space or memory resource.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller is not an owner of
    /// `resource_owner`, or `Internal` for storage failures.
    pub async fn list_grants(
        &self,
        authz: &AuthzContext,
        resource_owner: Owner,
        resource: GrantResource,
    ) -> Result<Vec<AccessGrantRow>, ProtocolError> {
        let permit = self
            .authorize_request(authz, &resource_owner, Relation::Owner)
            .await?;
        self.list_grants_authorized(&permit, resource).await
    }

    async fn list_grants_authorized(
        &self,
        permit: &MemoryPermit,
        resource: GrantResource,
    ) -> Result<Vec<AccessGrantRow>, ProtocolError> {
        self.storage
            .list_access_grants(permit.owner(), resource)
            .await
            .map_err(|err| storage_error("list_access_grants", &err))
    }

    /// Add a co-owner to a space.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller is not an owner of `space`, or
    /// `Internal` for storage failures.
    pub async fn add_owner(
        &self,
        authz: &AuthzContext,
        space: Owner,
        new_owner: Principal,
    ) -> Result<(), ProtocolError> {
        let permit = self
            .authorize_request(authz, &space, Relation::Owner)
            .await?;
        self.add_owner_authorized(authz, &permit, &new_owner).await
    }

    async fn add_owner_authorized(
        &self,
        authz: &AuthzContext,
        permit: &MemoryPermit,
        new_owner: &Principal,
    ) -> Result<(), ProtocolError> {
        let granted_by = self.grant_author_personality(permit.owner(), authz).await?;
        self.storage
            .add_space_owner(permit.owner(), new_owner, granted_by)
            .await
            .map_err(|err| storage_error("add_space_owner", &err))
    }

    /// Remove a co-owner from a space. Removing the last owner is refused.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller is not an owner of `space`,
    /// `InvalidArgument` when this would remove the last owner, or
    /// `Internal` for storage failures.
    pub async fn remove_owner(
        &self,
        authz: &AuthzContext,
        space: Owner,
        owner_principal: Principal,
    ) -> Result<(), ProtocolError> {
        let permit = self
            .authorize_request(authz, &space, Relation::Owner)
            .await?;
        self.remove_owner_authorized(&permit, &owner_principal)
            .await
    }

    async fn remove_owner_authorized(
        &self,
        permit: &MemoryPermit,
        owner_principal: &Principal,
    ) -> Result<(), ProtocolError> {
        match self
            .storage
            .remove_space_owner(permit.owner(), owner_principal)
            .await
            .map_err(|err| storage_error("remove_space_owner", &err))?
        {
            RemoveOwnerOutcome::Removed => Ok(()),
            RemoveOwnerOutcome::RefusedLastOwner => Err(ProtocolError::invalid_argument(
                "owner_principal",
                "cannot remove the last owner",
            )),
            RemoveOwnerOutcome::NotFound => Err(ProtocolError::not_found("owner grant not found")),
        }
    }

    /// Bootstrap the first owner row for a space. This provisioning verb does
    /// not use owner resolution because no owner row may exist yet.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` unless the caller has unrestricted access and can
    /// access `space`, or `Internal` for storage failures.
    pub async fn init_space_owner(
        &self,
        authz: &AuthzContext,
        space: Owner,
        owner_principal: Principal,
    ) -> Result<(), ProtocolError> {
        if authz.capabilities.access != AccessScope::Unrestricted
            || !authz.identity.can_access_principal(&space)
        {
            return Err(ProtocolError::forbidden(
                "requires unrestricted access to provision space owner",
            ));
        }
        self.init_space_owner_authorized(authz, &space, &owner_principal)
            .await
    }

    async fn init_space_owner_authorized(
        &self,
        authz: &AuthzContext,
        space: &Owner,
        owner_principal: &Principal,
    ) -> Result<(), ProtocolError> {
        let granted_by = self.grant_author_personality(space, authz).await?;
        self.storage
            .init_space_owner(space, owner_principal, granted_by)
            .await
            .map_err(|err| storage_error("init_space_owner", &err))
    }

    /// Browse public marketplace entries. This is intentionally not
    /// owner-scoped; any authenticated context may read public entries.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` for denied contexts, `InvalidArgument` for a
    /// non-positive limit, or `Internal` for storage failures.
    pub async fn browse_marketplace(
        &self,
        authz: &AuthzContext,
        limit: i64,
    ) -> Result<Vec<MemorySnapshot>, ProtocolError> {
        if authz.auth_path == AuthPath::Denied {
            return Err(ProtocolError::forbidden(
                "denied context cannot browse marketplace",
            ));
        }
        if limit <= 0 {
            return Err(ProtocolError::invalid_argument(
                "limit",
                "must be greater than zero",
            ));
        }
        self.storage
            .list_public_memories(limit)
            .await
            .map_err(|err| storage_error("list_public_memories", &err))
    }

    async fn resolve_entry_access_facts(
        &self,
        memory_id: MemoryId,
    ) -> Result<crate::EntryAccessFacts, ProtocolError> {
        self.storage
            .resolve_entry_owner(memory_id)
            .await
            .map_err(|err| storage_error("resolve_entry_owner", &err))?
            .ok_or_else(|| ProtocolError::not_found("memory not found"))
    }

    async fn grant_author_personality(
        &self,
        owner: &Owner,
        authz: &AuthzContext,
    ) -> Result<PersonalityInstanceId, ProtocolError> {
        self.ensure_subject_personality(owner, &authz.identity.principal)
            .await
            .map(|personality| personality.instance_id)
            .map_err(|err| storage_error("ensure_subject_personality", &err))
    }

    fn run_access_admin_hooks(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
        operation: AuthzOperation,
    ) -> Result<(), ProtocolError> {
        let input = AuthzInput {
            authz,
            requested: owner,
            resolved: owner,
            relation: Relation::Owner,
            operation,
        };
        let (result, outcome) = match self.registry.run_authorization_vetoes(&input) {
            Ok(()) => (Ok(()), crate::authz::AuthzOutcome::Allowed),
            Err(err) => (Err(err), crate::authz::AuthzOutcome::DeniedVeto),
        };
        self.registry.run_authorization_observers(&input, outcome);
        result
    }
}

fn reject_entry_grant_relation(relation: Relation) -> Result<(), ProtocolError> {
    if relation == Relation::Owner {
        return Err(ProtocolError::invalid_argument(
            "relation",
            "owner relation is reserved for owner bootstrap/transfer verbs",
        ));
    }
    if !relation.is_entry_grantable() {
        return Err(ProtocolError::invalid_argument(
            "relation",
            "entry grants support only editor or viewer",
        ));
    }
    Ok(())
}

fn reject_space_grant_relation(relation: Relation) -> Result<(), ProtocolError> {
    if !relation.is_space_grantable() {
        return Err(ProtocolError::invalid_argument(
            "relation",
            "owner relation is reserved for owner bootstrap/transfer verbs",
        ));
    }
    Ok(())
}

fn storage_error(context: &str, err: &StorageError) -> ProtocolError {
    match err {
        StorageError::NotFound => ProtocolError::not_found(context),
        StorageError::ConstraintViolation(msg) => ProtocolError::invalid_argument(context, msg),
        other => ProtocolError::internal(format!("{context}: {other}")),
    }
}
