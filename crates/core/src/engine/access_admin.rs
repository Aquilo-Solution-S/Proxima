use crate::access::{EntityId, Relation};
use crate::authz::{
    AuthPath, AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome, MembershipChange,
};
use crate::compliance::ComplianceEraseTarget;
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::{GroupId, OwnerRef, UserId};

use super::Engine;

/// One page of group members from [`Engine::list_members`], in the
/// keyset total order `(member_user_id, relation)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMemberPage {
    pub members: Vec<(UserId, Relation)>,
    /// More members exist past this page; resume from the last returned
    /// `(member, relation)` pair.
    pub has_more: bool,
}

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
        require_group_manage(authz, &group_owner)?;
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
        require_group_manage(authz, &group_owner)?;
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

    /// List one page of members for one group, in the keyset total order
    /// `(member_user_id, relation)`.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when the caller lacks admin on the group and
    /// `Internal` for storage failures.
    pub async fn list_members(
        &self,
        authz: &AuthzContext,
        group: GroupId,
        limit: u32,
        after: Option<(UserId, Relation)>,
    ) -> Result<GroupMemberPage, ProtocolError> {
        let group_owner = OwnerRef::Group(group);
        self.authorize_write(authz, &group_owner, Relation::Admin)
            .await?;
        let fetch = i64::from(limit).saturating_add(1);
        let mut members = self
            .storage()
            .access_admin
            .owner_membership_admin
            .list_group_members_page(group, after, fetch)
            .await
            .map_err(|err| storage_error("list_group_members_page", &err))?;
        let has_more = members.len() > usize::try_from(limit).unwrap_or(usize::MAX);
        members.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(GroupMemberPage { members, has_more })
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
        if matches!(current_owner, OwnerRef::Group(_)) {
            require_group_manage(authz, &current_owner)?;
        }

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

/// Membership-admin and group publish require the `manage` bit, not merely
/// `Relation::Admin` write authority. `authorize_write(.., Admin)` is satisfied
/// by any role that can write Goals (`write >= Goal`), so a custom
/// `OwnerAccessPort` could hand out `Role::new(Goal, Goal, false)` — write-Goal
/// but `manage = false` — and pass the write gate. Consult `may_manage`
/// explicitly so only roles with `manage = true` (e.g. `Role::admin`) mutate
/// membership or publish a group-owned entity.
fn require_group_manage(authz: &AuthzContext, group_owner: &OwnerRef) -> Result<(), ProtocolError> {
    if authz.may_manage(group_owner) {
        Ok(())
    } else {
        Err(ProtocolError::forbidden("requires manage on this owner"))
    }
}

fn actor_uuid(authz: &AuthzContext) -> uuid::Uuid {
    authz
        .subject()
        .map_or_else(|| authz.principal().stable_key_uuid(), UserId::into_inner)
}

fn storage_error(context: &str, err: &StorageError) -> ProtocolError {
    super::errors::internal_storage_error(context, err)
}

fn bootstrap_storage_error(err: StorageError) -> ProtocolError {
    super::errors::map_write_storage_error(err, "group", "group not found")
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

    fn membership_storage_with_home(
        member: UserId,
        group: GroupId,
        home_owner: Option<OwnerRef>,
    ) -> MembershipStorage {
        MembershipStorage {
            member: OwnerRef::Personal(member),
            group,
            membership_relation: Relation::Viewer,
            home_owner,
            entity_readable: true,
            memory_kind: None,
        }
    }

    #[tokio::test]
    async fn access_admin_membership_and_publish_require_manage_not_just_write() {
        use crate::access::AccessCeiling;

        let group = GroupId::new(Uuid::now_v7());
        let group_owner = OwnerRef::Group(group);
        let caller = UserId::new(Uuid::now_v7());
        let member = UserId::new(Uuid::now_v7());
        let entity = crate::EntityId::Memory(crate::MemoryId::new(Uuid::now_v7()));

        // write == Goal (satisfies `authorize_write(.., Admin)`) but manage == false.
        let write_only = Role::new(AccessCeiling::Goal, AccessCeiling::Goal, false)
            .expect("write==read is a valid role");
        let write_only_authz = AuthzContext::for_subject_with_role(
            caller,
            [(group_owner, write_only)],
            AuthPath::HostBearer,
        );
        let admin_authz = AuthzContext::for_subject_with_role(
            caller,
            [(group_owner, Role::admin())],
            AuthPath::HostBearer,
        );

        // add_member: write-without-manage is denied before touching storage.
        let engine = crate::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(
                membership_storage_with_home(member, group, Some(group_owner)).storage_ports(),
            );
        let denied = engine
            .add_member(&write_only_authz, group, member, Relation::Viewer)
            .await
            .expect_err("write-without-manage must not add members");
        assert_eq!(denied.code, ErrorCode::Forbidden);

        // remove_member: same gate, fails closed before enumerating members.
        let denied = engine
            .remove_member(&write_only_authz, group, member)
            .await
            .expect_err("write-without-manage must not remove members");
        assert_eq!(denied.code, ErrorCode::Forbidden);

        // publish of a group-owned entity: same manage gate.
        let denied = engine
            .publish_to_world(&write_only_authz, entity)
            .await
            .expect_err("write-without-manage must not publish a group entity");
        assert_eq!(denied.code, ErrorCode::Forbidden);

        // manage == true (Role::admin) passes the manage gate and reaches storage,
        // which this stub rejects with an Internal error — proving the gate opened.
        let past_gate = engine
            .add_member(&admin_authz, group, member, Relation::Viewer)
            .await
            .expect_err("stub storage rejects the write after the gate opens");
        assert_eq!(past_gate.code, ErrorCode::Internal);

        let past_gate = engine
            .publish_to_world(&admin_authz, entity)
            .await
            .expect_err("stub storage rejects the transfer after the gate opens");
        assert_eq!(past_gate.code, ErrorCode::Internal);
    }
}
