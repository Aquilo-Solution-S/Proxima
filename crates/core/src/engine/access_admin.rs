use crate::access::{EntityId, Relation, world};
use crate::authz::{AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome};
use crate::error::ProtocolError;
use crate::personality::MemorySnapshot;
use crate::storage::StorageError;
use crate::{EntityOwnerRow, GroupId, Principal, RemoveOwnerOutcome, UserId};

use super::Engine;

impl Engine {
    /// Share one entity with another principal by adding a read-only
    /// `entity_owner` row.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the entity has no home owner,
    /// `Forbidden` when the caller lacks editor on that home owner, and
    /// `Internal` for storage failures.
    pub async fn share_entry(
        &self,
        authz: &AuthzContext,
        entity: EntityId,
        with: Principal,
    ) -> Result<(), ProtocolError> {
        let home = self.home_owner_for(entity).await?;
        let permit = self.authorize_write(authz, &home, Relation::Editor).await?;
        self.veto_and_observe_access_admin(
            authz,
            &home,
            permit.owner(),
            Relation::Editor,
            AuthzOperation::EntityShare {
                entity,
                owner: with.clone(),
            },
        )?;
        self.storage()
            .add_entity_owner_share(entity, &with, None)
            .await
            .map_err(|err| storage_error("add_entity_owner_share", &err))
    }

    /// Remove one read-only entity share. Home rows are refused by storage.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the entity has no home owner,
    /// `Forbidden` when the caller lacks editor on that home owner, and
    /// `Internal` for storage failures.
    pub async fn unshare_entry(
        &self,
        authz: &AuthzContext,
        entity: EntityId,
        with: Principal,
    ) -> Result<RemoveOwnerOutcome, ProtocolError> {
        let home = self.home_owner_for(entity).await?;
        let permit = self.authorize_write(authz, &home, Relation::Editor).await?;
        self.veto_and_observe_access_admin(
            authz,
            &home,
            permit.owner(),
            Relation::Editor,
            AuthzOperation::EntityShare {
                entity,
                owner: with.clone(),
            },
        )?;
        self.storage()
            .remove_entity_owner_share(entity, &with)
            .await
            .map_err(|err| storage_error("remove_entity_owner_share", &err))
    }

    /// List all home/share rows for one entity.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` when the entity has no home owner,
    /// `Forbidden` when the caller lacks editor on that home owner, and
    /// `Internal` for storage failures.
    pub async fn list_entry_shares(
        &self,
        authz: &AuthzContext,
        entity: EntityId,
    ) -> Result<Vec<EntityOwnerRow>, ProtocolError> {
        let home = self.home_owner_for(entity).await?;
        self.authorize_write(authz, &home, Relation::Editor).await?;
        self.storage()
            .list_entity_owners(entity)
            .await
            .map_err(|err| storage_error("list_entity_owners", &err))
    }

    /// Publish one entity by adding the World read row.
    ///
    /// # Errors
    ///
    /// Same as [`Self::share_entry`].
    pub async fn publish_entry(
        &self,
        authz: &AuthzContext,
        entity: EntityId,
    ) -> Result<(), ProtocolError> {
        self.share_entry(authz, entity, world()).await
    }

    /// Remove the World read row from one entity.
    ///
    /// # Errors
    ///
    /// Same as [`Self::unshare_entry`].
    pub async fn unpublish_entry(
        &self,
        authz: &AuthzContext,
        entity: EntityId,
    ) -> Result<RemoveOwnerOutcome, ProtocolError> {
        self.unshare_entry(authz, entity, world()).await
    }

    /// List public World-readable memory entities.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` for unauthenticated/denied readers and
    /// `Internal` for storage failures.
    pub async fn list_world_entities(
        &self,
        authz: &AuthzContext,
        limit: usize,
    ) -> Result<Vec<MemorySnapshot>, ProtocolError> {
        self.authorize_read(authz).await?;
        let sidecars = self.sidecar_specs();
        self.storage()
            .list_world_entities(limit, &sidecars)
            .await
            .map_err(|err| storage_error("list_world_entities", &err))
    }

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
        let group_owner = Principal::Group(group);
        let permit = self
            .authorize_write(authz, &group_owner, Relation::Admin)
            .await?;
        let member_principal = Principal::User(member);
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
        let group_owner = Principal::Group(group);
        let permit = self
            .authorize_write(authz, &group_owner, Relation::Admin)
            .await?;
        let member_principal = Principal::User(member);
        let current_relations = self
            .storage()
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
                    member: member_principal.clone(),
                    relation,
                },
            )?;
        }
        self.storage()
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
        let group_owner = Principal::Group(group);
        self.authorize_write(authz, &group_owner, Relation::Admin)
            .await?;
        self.storage()
            .list_group_members(group)
            .await
            .map_err(|err| storage_error("list_group_members", &err))
    }

    async fn home_owner_for(&self, entity: EntityId) -> Result<Principal, ProtocolError> {
        self.storage()
            .entity_home_owner(entity)
            .await
            .map_err(|err| storage_error("entity_home_owner", &err))?
            .ok_or_else(|| ProtocolError::invalid_argument("entity", "entry not found"))
    }

    fn veto_and_observe_access_admin(
        &self,
        authz: &AuthzContext,
        requested: &Principal,
        resolved: &Principal,
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
    authz.identity.principal.columns().1
}

fn storage_error(context: &str, err: &StorageError) -> ProtocolError {
    ProtocolError::internal(format!("{context}: {err}"))
}
