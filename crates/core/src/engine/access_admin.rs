use crate::access::{EntityId, Relation};
use crate::authz::{
    AuthPath, AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome, MembershipChange,
};
use crate::compliance::ComplianceEraseTarget;
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::{GroupId, OwnerRef, UserId};

use super::Engine;

impl Engine {
    /// Seed the first Admin membership for a fresh group.
    ///
    /// Host-only bootstrap: this verb is not registered as an MCP tool. It
    /// admits exactly one Admin row only when the group currently has no Admin;
    /// later membership mutations must use [`Self::add_member`].
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller lacks compliance-controller
    /// authority or an authorization veto denies the bootstrap,
    /// `InvalidArgument` when the group already has an Admin, and `Internal`
    /// for storage failures.
    pub async fn bootstrap_group_admin(
        &self,
        authz: &AuthzContext,
        group: GroupId,
        first_admin: UserId,
    ) -> Result<(), ProtocolError> {
        let target = ComplianceEraseTarget::GroupOwner { group_id: group };
        if !self.compliance_controller_authorized(authz, &target).await {
            return Err(ProtocolError::forbidden(
                "compliance controller authorization required",
            ));
        }

        let bootstrap_member_authz = AuthzContext::for_subject(first_admin, AuthPath::System);
        let group_owner = OwnerRef::Group(group);
        let member_principal = OwnerRef::Personal(first_admin);
        self.veto_and_observe_access_admin(
            &bootstrap_member_authz,
            &group_owner,
            &group_owner,
            Relation::Admin,
            AuthzOperation::Membership {
                change: MembershipChange::Add,
                group,
                member: member_principal,
                relation: Relation::Admin,
            },
        )?;
        self.storage()
            .access_admin
            .owner_membership_admin
            .bootstrap_group_admin(group, first_admin, first_admin.into_inner())
            .await
            .map_err(bootstrap_storage_error)
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
                change: MembershipChange::Add,
                group,
                member: member_principal,
                relation,
            },
        )?;
        self.storage()
            .access_admin
            .owner_membership_admin
            .add_group_member(
                permit.owner_write_permit(),
                group,
                member,
                relation,
                actor_uuid(authz),
            )
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
                    change: MembershipChange::Remove,
                    group,
                    member: member_principal,
                    relation,
                },
            )?;
        }
        self.storage()
            .access_admin
            .owner_membership_admin
            .remove_group_member(permit.owner_write_permit(), group, member)
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
            .transfer_to_world(permit.owner_write_permit(), entity)
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

fn bootstrap_storage_error(err: StorageError) -> ProtocolError {
    match err {
        StorageError::ConstraintViolation(message) | StorageError::Conflict(message) => {
            ProtocolError::invalid_argument("group", message)
        }
        StorageError::Suppressed(message) => ProtocolError::suppressed(message),
        StorageError::NotFound => ProtocolError::not_found("group not found"),
        StorageError::Unavailable(message) | StorageError::Internal(message) => {
            ProtocolError::internal(message)
        }
        StorageError::V004ResetRequired { details } => ProtocolError::internal(details),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::super::access_sets::tests::MembershipStorage;
    use crate::access::{Relation, Role};
    use crate::authz::{
        AuthPath, AuthorizationHook, AuthzContext, AuthzInput, AuthzOperation, MembershipChange,
    };
    use crate::{ErrorCode, FlavorRegistry, GroupId, OwnerRef, UserId};
    use uuid::Uuid;

    #[test]
    fn access_admin_bootstrap_group_admin_is_not_mcp_exposed() {
        let frozen = FlavorRegistry::default().freeze_or_panic_for_tests();

        for tool in frozen.list_mcp_tools() {
            assert_ne!(
                tool.name, "bootstrap_group_admin",
                "bootstrap_group_admin must remain host-only"
            );
            for spec in tool.action_arg_specs {
                assert_ne!(
                    spec.action, "bootstrap_group_admin",
                    "bootstrap_group_admin must not be exposed as an MCP action"
                );
            }
            assert!(
                !tool
                    .args_schema
                    .to_string()
                    .contains("bootstrap_group_admin"),
                "bootstrap_group_admin must not appear in MCP schemas"
            );
        }
    }

    #[tokio::test]
    async fn access_admin_bootstrap_group_admin_requires_operator_authority() {
        let engine = crate::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
        let group = GroupId::new(Uuid::now_v7());
        let first_admin = UserId::new(Uuid::now_v7());
        let authz = AuthzContext::for_subject(first_admin, AuthPath::HostBearer);

        let err = engine
            .bootstrap_group_admin(&authz, group, first_admin)
            .await
            .expect_err("non-operator caller must not bootstrap group admin");
        assert_eq!(err.code, ErrorCode::Forbidden);
    }

    #[derive(Debug)]
    struct RecordingMembershipHook {
        changes: Arc<Mutex<Vec<MembershipChange>>>,
    }

    impl AuthorizationHook for RecordingMembershipHook {
        fn observe(&self, input: &AuthzInput<'_>, _outcome: crate::authz::AuthzOutcome) {
            if let AuthzOperation::Membership { change, .. } = &input.operation {
                self.changes.lock().expect("changes lock").push(*change);
            }
        }
    }

    #[tokio::test]
    async fn access_admin_membership_hook_records_add_and_remove_direction() {
        let group = GroupId::new(Uuid::now_v7());
        let admin = UserId::new(Uuid::now_v7());
        let member = UserId::new(Uuid::now_v7());
        let changes = Arc::new(Mutex::new(Vec::new()));
        let mut registry = FlavorRegistry::new();
        registry.add_authorization_hook(Arc::new(RecordingMembershipHook {
            changes: changes.clone(),
        }));
        let engine = crate::Engine::new(registry.freeze_or_panic_for_tests()).with_storage_ports(
            MembershipStorage {
                member: OwnerRef::Personal(member),
                group,
                membership_relation: Relation::Viewer,
                home_owner: None,
                entity_readable: false,
                memory_kind: None,
            }
            .storage_ports(),
        );
        let authz = AuthzContext::for_subject_with_role(
            admin,
            [(OwnerRef::Group(group), Role::admin())],
            AuthPath::HostBearer,
        );

        let add_err = engine
            .add_member(&authz, group, member, Relation::Viewer)
            .await
            .expect_err("test storage rejects membership writes after hook observation");
        assert_eq!(add_err.code, ErrorCode::Internal);

        let remove_err = engine
            .remove_member(&authz, group, member)
            .await
            .expect_err("test storage rejects membership writes after hook observation");
        assert_eq!(remove_err.code, ErrorCode::Internal);

        assert_eq!(
            *changes.lock().expect("changes lock"),
            vec![MembershipChange::Add, MembershipChange::Remove]
        );
    }
}
