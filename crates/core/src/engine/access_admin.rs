use crate::access::{AccessKind, EntityId, Relation};
use crate::authz::{
    AuthPath, AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome, MembershipChange,
};
use crate::error::ProtocolError;
use crate::owner_inverse::OwnerEraseTarget;
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
    /// Returns `Forbidden` when the caller lacks owner-erase authority
    /// authority or an authorization veto denies the bootstrap,
    /// `InvalidArgument` when the group already has an Admin, and `Internal`
    /// for storage failures.
    pub async fn bootstrap_group_admin(
        &self,
        authz: &AuthzContext,
        group: GroupId,
        first_admin: UserId,
    ) -> Result<(), ProtocolError> {
        let target = OwnerEraseTarget::GroupOwner { group_id: group };
        if !self.erase_authority_grants(authz, &target).await {
            return Err(ProtocolError::forbidden(
                "owner-erase authority authorization required",
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

    /// Transfer one memory's owner to `to_owner`. This is an owner
    /// TRANSFER, not an ACL flag or a share row: the series moves in place
    /// (`MemoryHeadAligned`) and leaves the prior owner's view entirely.
    ///
    /// Goals do not transfer, so a Goal entity is refused here, before any
    /// owner lookup or storage call. The `goal_head_t_only` trigger is the
    /// DDL backstop for the same rule.
    ///
    /// Authorization is **admin on both sides**:
    /// * `Relation::Admin` on the entity's CURRENT owner — for a personal
    ///   owner that is the subject's own `Role::personal()`; for a group
    ///   owner it is a member holding `Role::admin()` (`manage = true`),
    ///   re-checked through `require_group_manage`. This is the side the
    ///   request is scoped to: the write permit that carries the transfer
    ///   into storage is the SOURCE owner's, so a caller gives a memory
    ///   away while acting AS its owner.
    /// * Write-Goal plus group-manage on `to_owner`, resolved by
    ///   `authorize_transfer_destination`. The destination must
    ///   therefore be a **Group**: `may_manage` is false for every personal
    ///   owner by construction, so there is no personal-owner spelling of
    ///   receiving-side consent. A personal destination is refused with
    ///   `InvalidArgument`.
    ///
    /// The destination's `owners` row is minted inside the storage
    /// transaction (`ensure_owner_row`), so the paired announce rows'
    /// `owner_id` FKs hold without any migration-seeded owner.
    ///
    /// The ownership oracle is closed by ordering: the `to_owner ==
    /// current_owner` refusal fires only AFTER both sides authorize, so a
    /// caller with no authority cannot tell `InvalidArgument` (the entity
    /// already lives at the owner they named) from `Forbidden` (it does
    /// not) and probe entity→owner mappings.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` for a Goal entity and for a non-Group
    /// destination. Returns `NotFound` when the entity has no home owner
    /// (absent or tombstoned). Returns `Forbidden` when the caller lacks
    /// admin/manage authority on either side. Returns `InvalidArgument`
    /// when an authorized caller names the current owner as the
    /// destination. Returns `Internal` for storage failures, and
    /// `NotFound` if the storage transfer finds no matching row (owner
    /// changed concurrently between the lookup and the write).
    pub async fn transfer_to_owner(
        &self,
        authz: &AuthzContext,
        entity: EntityId,
        to_owner: OwnerRef,
    ) -> Result<(), ProtocolError> {
        self.operation_authority(authz)?;
        if matches!(entity, EntityId::Goal(_)) {
            return Err(ProtocolError::invalid_argument(
                "entity",
                "goals do not transfer",
            ));
        }
        if !matches!(to_owner, OwnerRef::Group(_)) {
            return Err(ProtocolError::invalid_argument(
                "to_owner",
                "transfer destination must be a group owner: receiving-side consent is \
                 group-manage authority, which no personal owner can grant",
            ));
        }
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
        // Receiving side: the destination consents by the caller holding
        // manage on it. A transfer needs consent from both owners.
        self.authorize_transfer_destination(authz, &to_owner)
            .await?;

        // Only now, with both sides authorized, is it safe to say that the
        // destination is where the entity already lives. Answering that
        // earlier told an unauthorized caller where any entity is homed.
        if current_owner == to_owner {
            return Err(ProtocolError::invalid_argument(
                "to_owner",
                "transfer destination is already the current owner",
            ));
        }

        self.veto_and_observe_access_admin(
            authz,
            &current_owner,
            permit.owner(),
            Relation::Admin,
            AuthzOperation::EntityTransfer { entity, to_owner },
        )?;

        let transferred = self
            .storage()
            .access_admin
            .owner_transfer
            .transfer_to_owner(
                permit.owner_write_permit(),
                entity,
                to_owner,
                &self.owner_surfaces(),
            )
            .await
            .map_err(|err| storage_error("transfer_to_owner", &err))?;

        if !transferred {
            return Err(ProtocolError::not_found(
                "entity owner changed concurrently",
            ));
        }
        Ok(())
    }

    /// Receiving-side consent for [`Self::transfer_to_owner`].
    ///
    /// A transfer is the one verb that needs authority on TWO owners at
    /// once, and the served surface cannot hand it both. `mcp_auth`
    /// narrows every authenticated request through
    /// `AuthzContext::narrowed_to_owner(selected_owner)`, whose role map
    /// holds exactly the one owner the caller selected — so whichever side
    /// the caller selects, the request context is silent about the other.
    /// The narrowing is a security boundary and stays; the destination's
    /// consent is resolved out of band instead, the way
    /// [`Self::bootstrap_group_admin`] steps outside the request context
    /// rather than widening it.
    ///
    /// Two paths, in priority order:
    /// * The request context already carries a resolved role for
    ///   `to_owner` (an embedded host that never narrowed, or a caller who
    ///   selected the destination). That role is authoritative and is
    ///   gated exactly as before — `authorize_write(.., Admin)` plus
    ///   [`require_group_manage`], hooks and owner resolution included. A
    ///   role that is present but insufficient is a refusal, never a
    ///   fall-through to storage: a host that deliberately hands out a
    ///   narrowed role must not be overruled by a membership row.
    /// * The context is silent about `to_owner` — the narrowed served
    ///   surface. Then, and only then, the authenticated subject's
    ///   membership is re-read from storage. Those rows are
    ///   engine-authoritative: [`Self::add_member`] and
    ///   [`Self::bootstrap_group_admin`] are what write them, and every
    ///   write is itself manage-gated.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when neither path establishes write-Goal plus
    /// manage on `to_owner`, and `Internal` when the membership re-read
    /// fails.
    async fn authorize_transfer_destination(
        &self,
        authz: &AuthzContext,
        to_owner: &OwnerRef,
    ) -> Result<(), ProtocolError> {
        if authz.role_for_owner(to_owner).is_some() {
            self.authorize_write(authz, to_owner, Relation::Admin)
                .await?;
            return require_group_manage(authz, to_owner);
        }
        self.authorize_transfer_destination_out_of_band(authz, to_owner)
            .await
    }

    /// Re-read the authenticated subject's membership on `to_owner` and
    /// require the same authority the in-context gate requires: a role
    /// that may write Goals AND carries `manage`. Only `Relation::Admin`
    /// maps to such a role, so this is `Relation::Admin` on the
    /// destination group — spelled through [`Relation::role`] so the rule
    /// tracks the role table rather than restating it.
    ///
    /// Refused before the lookup for every context that does not name a
    /// subject whose membership may stand in for its own authority:
    ///
    /// * `Delegated` — a delegated grant decodes `manage = false` by
    ///   construction (`storage-pg/src/delegated_authority.rs`). Its
    ///   subject is a real user who may well hold Admin on the
    ///   destination, so re-reading membership would hand the grant an
    ///   authority the grant does not carry. `operation_authority` already
    ///   refuses raw delegated contexts and `transfer_to_owner` takes no
    ///   `DelegatedPhase`, so this is the second lock on a shut door —
    ///   worth keeping, because the first one lives in another module.
    /// * `Denied` and any context that is not server-resolved — fail
    ///   closed; a denied context authorizes nothing.
    async fn authorize_transfer_destination_out_of_band(
        &self,
        authz: &AuthzContext,
        to_owner: &OwnerRef,
    ) -> Result<(), ProtocolError> {
        let refused = || ProtocolError::forbidden("requires manage on this owner");
        if matches!(authz.auth_path(), AuthPath::Delegated | AuthPath::Denied)
            || !authz.is_server_resolved()
        {
            return Err(refused());
        }
        let Some(subject) = authz.subject() else {
            return Err(refused());
        };
        let memberships = self
            .storage()
            .access_admin
            .owner_access_read
            .resolve_membership(&OwnerRef::Personal(subject))
            .await
            .map_err(|err| storage_error("resolve_membership", &err))?;
        let consents = memberships.iter().any(|row| {
            if OwnerRef::Group(row.group) != *to_owner {
                return false;
            }
            let role = row.relation.role();
            role.may_write(AccessKind::Goal) && role.manages()
        });
        if consents { Ok(()) } else { Err(refused()) }
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

/// Membership-admin and group transfer require the `manage` bit, not merely
/// `Relation::Admin` write authority. `authorize_write(.., Admin)` is satisfied
/// by any role that can write Goals (`write >= Goal`), so a custom
/// `OwnerAccessPort` could hand out `Role::new(Goal, Goal, false)` — write-Goal
/// but `manage = false` — and pass the write gate. Consult `may_manage`
/// explicitly so only roles with `manage = true` (e.g. `Role::admin`) mutate
/// membership or move a group-owned entity to another owner.
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
    use crate::{EntityId, ErrorCode, FlavorRegistry, GroupId, MemoryId, OwnerRef, UserId};
    use uuid::Uuid;

    /// The destination gate has two paths and they must never merge.
    ///
    /// When the request context already carries a resolved role for the
    /// destination, that role is the answer — including when the answer is
    /// no. Falling through to the membership re-read on a refusal would let
    /// a stored Admin row overrule a host that deliberately handed out a
    /// narrowed role, and the narrowed served surface is the only reason
    /// resolving the destination out of band is safe at all.
    ///
    /// The control is what gives the refusals meaning. The SAME storage,
    /// holding a real Admin membership on the destination the whole time,
    /// is consulted when the context is silent — consent is granted, the
    /// transfer gets past the gate, and it dies further down on the
    /// double's rejecting write with `Internal`. So `Forbidden` below can
    /// only mean the lookup never ran.
    #[tokio::test]
    async fn a_carried_role_that_cannot_manage_never_falls_through_to_the_membership_row() {
        let subject = UserId::new(Uuid::now_v7());
        let destination_group = GroupId::new(Uuid::now_v7());
        let destination = OwnerRef::Group(destination_group);
        let entity = EntityId::Memory(MemoryId::new(Uuid::now_v7()));

        let engine = || {
            crate::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
                .with_storage_ports(
                    MembershipStorage {
                        member: OwnerRef::Personal(subject),
                        group: destination_group,
                        // Real, sufficient, receiving-side consent, sitting
                        // in storage for every case in this test.
                        membership_relation: Relation::Admin,
                        home_owner: Some(OwnerRef::Personal(subject)),
                        entity_readable: true,
                        memory_kind: None,
                        goal_evidence: None,
                        observed_modify_evidence: std::sync::Arc::new(std::sync::Mutex::new(None)),
                        observed_goal_authorship: std::sync::Arc::new(std::sync::Mutex::new(
                            Vec::new(),
                        )),
                    }
                    .storage_ports(),
                )
        };

        let silent = AuthzContext::for_subject(subject, AuthPath::HostBearer);
        let control = engine()
            .transfer_to_owner(&silent, entity, destination)
            .await
            .expect_err("the storage double rejects the transfer write itself");
        assert_eq!(
            control.code,
            ErrorCode::Internal,
            "a context silent about the destination must reach the membership row and be \
             consented — otherwise the refusals below prove nothing"
        );

        // Present but insufficient. `Role::personal()` is the sharp one: it
        // may write Goals, so only `manages()` separates it from Admin.
        for role in [
            Role::viewer(),
            Role::ingest(),
            Role::editor(),
            Role::personal(),
        ] {
            let carried = AuthzContext::for_subject_with_role(
                subject,
                [(destination, role)],
                AuthPath::HostBearer,
            );
            let err = engine()
                .transfer_to_owner(&carried, entity, destination)
                .await
                .expect_err("a carried role that cannot manage must refuse");
            assert_eq!(
                err.code,
                ErrorCode::Forbidden,
                "carried role {role:?} must refuse outright; Internal here would mean the \
                 gate fell through to the membership lookup and the stored Admin row \
                 overruled the narrowed role"
            );
        }
    }

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
                goal_evidence: None,
                observed_modify_evidence: std::sync::Arc::new(std::sync::Mutex::new(None)),
                observed_goal_authorship: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
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
            goal_evidence: None,
            observed_modify_evidence: std::sync::Arc::new(std::sync::Mutex::new(None)),
            observed_goal_authorship: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    #[tokio::test]
    async fn access_admin_transfer_refuses_goal_entities_before_owner_lookup() {
        let engine = crate::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
        let caller = UserId::new(Uuid::now_v7());
        let authz = AuthzContext::for_subject(caller, AuthPath::HostBearer);
        let goal = crate::EntityId::Goal(crate::GoalId::new(Uuid::now_v7()));
        let destination = OwnerRef::Group(GroupId::new(Uuid::now_v7()));

        // The default engine has no storage: reaching the owner lookup would
        // surface Internal, so InvalidArgument proves the gate fired first.
        let err = engine
            .transfer_to_owner(&authz, goal, destination)
            .await
            .expect_err("goals do not transfer");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(
            err.message.contains("goals do not transfer"),
            "refusal must state the ruling: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn access_admin_transfer_refuses_a_personal_destination() {
        let engine = crate::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
        let caller = UserId::new(Uuid::now_v7());
        let authz = AuthzContext::for_subject(caller, AuthPath::HostBearer);
        let entity = crate::EntityId::Memory(crate::MemoryId::new(Uuid::now_v7()));

        let err = engine
            .transfer_to_owner(&authz, entity, OwnerRef::Personal(caller))
            .await
            .expect_err("a personal owner cannot grant receiving-side consent");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(
            err.message.contains("must be a group owner"),
            "refusal must name the destination rule: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn access_admin_membership_and_transfer_require_manage_not_just_write() {
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
        let destination = GroupId::new(Uuid::now_v7());
        let destination_owner = OwnerRef::Group(destination);
        let admin_authz = AuthzContext::for_subject_with_role(
            caller,
            [
                (group_owner, Role::admin()),
                (destination_owner, Role::admin()),
            ],
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

        // transfer OUT of a group-owned entity: same manage gate on the
        // source side.
        let denied = engine
            .transfer_to_owner(&write_only_authz, entity, destination_owner)
            .await
            .expect_err("write-without-manage must not transfer a group entity");
        assert_eq!(denied.code, ErrorCode::Forbidden);

        // Admin on the source but nothing at all on the destination: the
        // receiving side refuses. `MembershipStorage` reports no membership
        // for this caller, so the out-of-band destination lookup finds
        // nothing either — neither path consents.
        let source_only_authz = AuthzContext::for_subject_with_role(
            caller,
            [(group_owner, Role::admin())],
            AuthPath::HostBearer,
        );
        let denied = engine
            .transfer_to_owner(&source_only_authz, entity, destination_owner)
            .await
            .expect_err("no authority on the destination must refuse the transfer");
        assert_eq!(denied.code, ErrorCode::Forbidden);

        // Full admin on the SOURCE, write-without-manage on the
        // DESTINATION. `authorize_write(.., Admin)` is satisfied on both
        // sides — write == Goal is all it asks — so the destination's
        // `require_group_manage` is the only thing standing between this
        // caller and a transfer into a group that never consented. Delete
        // that one call and this assertion sees `Internal` (the stub
        // storage refusing the write) instead of `Forbidden`.
        let write_only_destination_authz = AuthzContext::for_subject_with_role(
            caller,
            [
                (group_owner, Role::admin()),
                (destination_owner, write_only),
            ],
            AuthPath::HostBearer,
        );
        let denied = engine
            .transfer_to_owner(&write_only_destination_authz, entity, destination_owner)
            .await
            .expect_err("write-without-manage on the destination is not receiving-side consent");
        assert_eq!(
            denied.code,
            ErrorCode::Forbidden,
            "the destination manage gate must refuse before storage is touched: {}",
            denied.message
        );
        assert!(
            denied.message.contains("requires manage on this owner"),
            "refusal must name the manage bit: {}",
            denied.message
        );

        // manage == true (Role::admin) passes the manage gate and reaches storage,
        // which this stub rejects with an Internal error — proving the gate opened.
        let past_gate = engine
            .add_member(&admin_authz, group, member, Relation::Viewer)
            .await
            .expect_err("stub storage rejects the write after the gate opens");
        assert_eq!(past_gate.code, ErrorCode::Internal);

        let past_gate = engine
            .transfer_to_owner(&admin_authz, entity, destination_owner)
            .await
            .expect_err("stub storage rejects the transfer after the gate opens");
        assert_eq!(past_gate.code, ErrorCode::Internal);
    }

    /// The served surface narrows every request to ONE owner, so a
    /// transfer's context can never name both sides. This is that context:
    /// `Role::admin()` on the source group and literally nothing on the
    /// destination — the exact shape
    /// `AuthzContext::narrowed_to_owner(source)` produces. The destination's
    /// consent comes from the membership rows instead, so an Admin row for
    /// this caller on the destination group opens the gate and a
    /// non-manage row does not.
    #[tokio::test]
    async fn access_admin_transfer_resolves_a_narrowed_destination_out_of_band() {
        let source = GroupId::new(Uuid::now_v7());
        let source_owner = OwnerRef::Group(source);
        let destination = GroupId::new(Uuid::now_v7());
        let destination_owner = OwnerRef::Group(destination);
        let caller = UserId::new(Uuid::now_v7());
        let entity = crate::EntityId::Memory(crate::MemoryId::new(Uuid::now_v7()));

        // Exactly what the middleware hands the engine: one owner, one role.
        let narrowed = AuthzContext::for_subject_with_role(
            caller,
            [(source_owner, Role::admin())],
            AuthPath::HostBearer,
        );
        assert!(
            narrowed.role_for_owner(&destination_owner).is_none(),
            "a narrowed context must carry no role for the destination"
        );

        let storage_with_relation = |relation: Relation| {
            crate::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
                .with_storage_ports(
                    MembershipStorage {
                        member: OwnerRef::Personal(caller),
                        group: destination,
                        membership_relation: relation,
                        home_owner: Some(source_owner),
                        entity_readable: true,
                        memory_kind: None,
                        goal_evidence: None,
                        observed_modify_evidence: std::sync::Arc::new(std::sync::Mutex::new(None)),
                        observed_goal_authorship: std::sync::Arc::new(std::sync::Mutex::new(
                            Vec::new(),
                        )),
                    }
                    .storage_ports(),
                )
        };

        // An Admin row on the destination is the receiving side's consent:
        // the gate opens and the stub storage refuses the write instead.
        let past_gate = storage_with_relation(Relation::Admin)
            .transfer_to_owner(&narrowed, entity, destination_owner)
            .await
            .expect_err("stub storage rejects the transfer after the gate opens");
        assert_eq!(
            past_gate.code,
            ErrorCode::Internal,
            "an Admin membership row on the destination must open the gate: {}",
            past_gate.message
        );

        // Every other relation maps to a role without `manage`, so none of
        // them is consent.
        for relation in [Relation::Editor, Relation::Viewer, Relation::Ingest] {
            let denied = storage_with_relation(relation)
                .transfer_to_owner(&narrowed, entity, destination_owner)
                .await
                .unwrap_err();
            assert_eq!(
                denied.code,
                ErrorCode::Forbidden,
                "{relation:?} on the destination is not receiving-side consent: {}",
                denied.message
            );
        }
    }

    /// A delegated grant decodes `manage = false`, so it must not reach the
    /// out-of-band lookup and borrow its own subject's Admin membership.
    #[tokio::test]
    async fn access_admin_transfer_refuses_a_delegated_context_before_the_membership_lookup() {
        let source = GroupId::new(Uuid::now_v7());
        let source_owner = OwnerRef::Group(source);
        let destination = GroupId::new(Uuid::now_v7());
        let caller = UserId::new(Uuid::now_v7());
        let entity = crate::EntityId::Memory(crate::MemoryId::new(Uuid::now_v7()));

        // Admin on the destination in storage — the authority the grant
        // must NOT be able to borrow.
        let engine = crate::Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
            .with_storage_ports(
                MembershipStorage {
                    member: OwnerRef::Personal(caller),
                    group: destination,
                    membership_relation: Relation::Admin,
                    home_owner: Some(source_owner),
                    entity_readable: true,
                    memory_kind: None,
                    goal_evidence: None,
                    observed_modify_evidence: std::sync::Arc::new(std::sync::Mutex::new(None)),
                    observed_goal_authorship: std::sync::Arc::new(
                        std::sync::Mutex::new(Vec::new()),
                    ),
                }
                .storage_ports(),
            );
        let delegated = AuthzContext::for_subject_with_role(
            caller,
            [(source_owner, Role::admin())],
            AuthPath::Delegated,
        );

        let denied = engine
            .transfer_to_owner(&delegated, entity, OwnerRef::Group(destination))
            .await
            .expect_err("a raw delegated context is not Engine authority");
        assert_eq!(denied.code, ErrorCode::Forbidden);
    }
}
