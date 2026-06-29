use crate::access::Relation;
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
            .list_group_members(group)
            .await
            .map_err(|err| storage_error("list_group_members", &err))
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
