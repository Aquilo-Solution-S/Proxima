use crate::access::{EntityId, Relation};
use crate::authz::{AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome};
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::{GroupId, OwnerRef, UserId};

use super::Engine;

impl Engine {
    /// Add a user to a group with one relation.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller lacks admin on the group and
    /// `Internal` for storage failures.
    pub async fn add_member(
        &self,
        authz: &AuthzContext,
        group: GroupId,
        member: UserId,
        relation: Relation,
    ) -> Result<(), ProtocolError> {
        let group_owner = OwnerRef::Group(group);
        let permit = self
            .authorize_write(authz, &group_owner, Relation::Admin)
            .await?;
        let member_principal = OwnerRef::Personal(member);
        self.veto_and_observe_access_admin(
            authz,
            &group_owner,
            permit.owner(),
            Relation::Admin,
            AuthzOperation::Membership {
                group,
                member: member_principal,
                relation,
            },
        )?;
        self.storage()
            .access_admin
            .owner_membership_admin
            .add_group_member(group, member, relation, actor_uuid(authz))
            .await
            .map_err(|err| storage_error("add_group_member", &err))
    }

    /// Remove all membership rows for a user in one group.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller lacks admin on the group and
    /// `Internal` for storage failures.
    pub async fn remove_member(
        &self,
        authz: &AuthzContext,
        group: GroupId,
        member: UserId,
    ) -> Result<(), ProtocolError> {
        let group_owner = OwnerRef::Group(group);
        let permit = self
            .authorize_write(authz, &group_owner, Relation::Admin)
            .await?;
        let member_principal = OwnerRef::Personal(member);
        let current_relations = self
            .storage()
            .access_admin
            .owner_membership_admin
            .list_group_members(group)
            .await
            .map_err(|err| storage_error("list_group_members", &err))?
            .into_iter()
            .filter_map(|(candidate, relation)| (candidate == member).then_some(relation))
            .collect::<Vec<_>>();
        for relation in current_relations {
            self.veto_and_observe_access_admin(
                authz,
                &group_owner,
                permit.owner(),
                Relation::Admin,
                AuthzOperation::Membership {
                    group,
                    member: member_principal,
                    relation,
                },
            )?;
        }
        self.storage()
            .access_admin
            .owner_membership_admin
            .remove_group_member(group, member)
            .await
            .map_err(|err| storage_error("remove_group_member", &err))
    }

    /// List members for one group.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller lacks admin on the group and
    /// `Internal` for storage failures.
    pub async fn list_members(
        &self,
        authz: &AuthzContext,
        group: GroupId,
    ) -> Result<Vec<(UserId, Relation)>, ProtocolError> {
        let group_owner = OwnerRef::Group(group);
        self.authorize_write(authz, &group_owner, Relation::Admin)
            .await?;
        self.storage()
            .access_admin
            .owner_membership_admin
            .list_group_members(group)
            .await
            .map_err(|err| storage_error("list_group_members", &err))
    }

    /// Transfer one memory or goal's owner to `OwnerRef::World` — the
    /// kernel-law publish verb. This is an owner TRANSFER, not an ACL
    /// flag or a share row: World is universally readable and, per
    /// `authorize_write`'s `resolved == world()` short-circuit, never a
    /// write owner again afterward.
    ///
    /// Requires write/manage authority (`Relation::Admin`) on the entity's
    /// CURRENT owner — for a personal owner that is the subject's own
    /// `Role::personal()`; for a group owner it is a member holding
    /// `Role::admin()` (`manage = true`). Re-publishing an already-World
    /// entity fails closed: the current-owner lookup resolves to World,
    /// and `authorize_write` denies World as a write owner before any
    /// storage call.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` when the entity has no home owner (absent or,
    /// for a memory, tombstoned). Returns `Forbidden` when the caller
    /// lacks admin/manage authority on the current owner, or when the
    /// current owner is already World. Returns `Internal` for storage
    /// failures, and `NotFound` if the storage transfer finds no matching
    /// row (owner changed concurrently between the lookup and the write).
    pub async fn publish_to_world(
        &self,
        authz: &AuthzContext,
        entity: EntityId,
    ) -> Result<(), ProtocolError> {
        let current_owner = self
            .storage()
            .access_admin
            .owner_access_read
            .home_owner(entity)
            .await
            .map_err(|err| storage_error("home_owner", &err))?
            .ok_or_else(|| ProtocolError::not_found("entity not found"))?;

        let permit = self
            .authorize_write(authz, &current_owner, Relation::Admin)
            .await?;

        self.veto_and_observe_access_admin(
            authz,
            &current_owner,
            permit.owner(),
            Relation::Admin,
            AuthzOperation::EntityShare {
                entity,
                owner: OwnerRef::World,
            },
        )?;

        let transferred = self
            .storage()
            .access_admin
            .owner_transfer
            .transfer_to_world(entity, *permit.owner())
            .await
            .map_err(|err| storage_error("transfer_to_world", &err))?;

        if !transferred {
            return Err(ProtocolError::not_found(
                "entity already published or owner changed concurrently",
            ));
        }
        Ok(())
    }

    fn veto_and_observe_access_admin(
        &self,
        authz: &AuthzContext,
        requested: &OwnerRef,
        resolved: &OwnerRef,
        relation: Relation,
        operation: AuthzOperation,
    ) -> Result<(), ProtocolError> {
        let input = AuthzInput {
            authz,
            requested,
            resolved,
            relation,
            operation,
        };
        match self.registry.run_authorization_vetoes(&input) {
            Ok(()) => {
                self.registry
                    .run_authorization_observers(&input, AuthzOutcome::Allowed);
                Ok(())
            }
            Err(err) => {
                self.registry
                    .run_authorization_observers(&input, AuthzOutcome::DeniedVeto);
                Err(err)
            }
        }
    }
}

fn actor_uuid(authz: &AuthzContext) -> uuid::Uuid {
    authz
        .subject()
        .map_or_else(|| authz.principal().stable_key_uuid(), UserId::into_inner)
}

fn storage_error(context: &str, err: &StorageError) -> ProtocolError {
    ProtocolError::internal(format!("{context}: {err}"))
}
