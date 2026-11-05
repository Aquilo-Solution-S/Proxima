use super::Engine;
use crate::authz::hooks::GoalCollaborationChange;
use crate::authz::{AuthzContext, AuthzInput, AuthzOperation, AuthzOutcome};
use crate::error::ProtocolError;
use crate::{
    EntityId, GoalAudienceGrantInput, GoalCommandPermit, GoalHandle, GoalPublicationInput,
    GoalWorkBindingInput, MemoryId, OwnerId, UserId,
};

impl Engine {
    /// Start an independent A/P series from one exact readable version.
    /// Cross-owner use also needs explicit source-owner fork consent.
    ///
    /// # Errors
    /// Refuses immutable Facts, restricted authentication, missing source-use
    /// or destination authority, unsupported copy schemas and stale consent.
    pub async fn fork_memory(
        &self,
        authz: &AuthzContext,
        request: &crate::ForkMemoryRequest,
    ) -> Result<crate::ForkMemoryOutcome, ProtocolError> {
        self.operation_authority(authz)?;
        let scope = authz
            .owner_scope()
            .filter(|scope| scope.goal_principal().is_some())
            .ok_or_else(|| {
                ProtocolError::forbidden("direct authenticated fork authority required")
            })?;
        let source = self
            .authorize_entry_read(authz, EntityId::Memory(request.source))
            .await?;
        let kind = source.kind();
        if !matches!(
            kind,
            crate::AccessKind::Abstraction | crate::AccessKind::Perspective
        ) {
            return Err(ProtocolError::invalid_argument(
                "source",
                "only A/P versions can be forked",
            ));
        }
        let destination = match request.destination {
            crate::MemoryDestination::Owner(owner) => crate::ForkDestinationPermit::Owner(
                self.authorize_write(authz, &owner, kind)
                    .await?
                    .into_owner_write(),
            ),
            crate::MemoryDestination::WorkingGoal(goal) => {
                crate::ForkDestinationPermit::WorkingGoal(
                    self.goal_command_permit(authz, goal, GoalCollaborationChange::Fork)
                        .await?,
                )
            }
        };
        let owner = match &destination {
            crate::ForkDestinationPermit::Owner(write) => *write.owner(),
            crate::ForkDestinationPermit::WorkingGoal(command) => command.controller(),
        };
        let schema = source.schema_id().ok_or_else(|| {
            ProtocolError::invalid_argument("source", "source schema metadata is unavailable")
        })?;
        let queued_spaces = self
            .embedding_spaces_for_write(&owner, schema.as_str())
            .await?;
        let permit = crate::ForkCommandPermit::new(
            scope.clone(),
            request.source,
            *source.owner(),
            destination,
            queued_spaces,
        );
        self.storage()
            .goal_command
            .goal_collaboration
            .fork_memory(request, &permit)
            .await
            .map_err(super::goal_write::map_goal_storage_error)
    }

    pub(super) async fn goal_command_permit(
        &self,
        authz: &AuthzContext,
        goal: GoalHandle,
        change: GoalCollaborationChange,
    ) -> Result<GoalCommandPermit, ProtocolError> {
        self.operation_authority(authz)?;
        let scope = authz
            .owner_scope()
            .filter(|scope| scope.goal_principal().is_some())
            .ok_or_else(|| {
                ProtocolError::forbidden("direct authenticated Goal authority required")
            })?;
        let controller = self
            .storage()
            .goal_command
            .goal_collaboration
            .controller_owner(scope, goal)
            .await
            .map_err(super::goal_write::map_goal_storage_error)?
            .ok_or_else(|| ProtocolError::not_found("Goal not found"))?;
        let input = AuthzInput {
            authz,
            requested: &controller,
            resolved: &controller,
            operation: AuthzOperation::GoalCollaboration { goal, change },
        };
        if let Err(error) = self.registry.run_authorization_vetoes(&input) {
            self.registry
                .run_authorization_observers(&input, AuthzOutcome::DeniedVeto);
            return Err(error);
        }
        self.registry
            .run_authorization_observers(&input, AuthzOutcome::Allowed);
        Ok(GoalCommandPermit::new(scope.clone(), goal, controller))
    }

    /// Grant a person bounded access to one Goal's publications and work.
    ///
    /// # Errors
    /// Refuses restricted authentication, host vetoes, stale authority, and
    /// grants exceeding the caller's current authority at commit.
    pub async fn grant_goal_access(
        &self,
        authz: &AuthzContext,
        input: &GoalAudienceGrantInput,
    ) -> Result<(), ProtocolError> {
        let permit = self
            .goal_command_permit(authz, input.goal, GoalCollaborationChange::GrantAccess)
            .await?;
        self.storage()
            .goal_command
            .goal_collaboration
            .grant_access(input, &permit)
            .await
            .map_err(super::goal_write::map_goal_storage_error)
    }

    /// Remove one person's Goal route; independent routes remain effective.
    ///
    /// # Errors
    /// Refuses absent Goals, host vetoes, and callers without fresh Goal
    /// audience-management authority.
    pub async fn revoke_goal_access(
        &self,
        authz: &AuthzContext,
        goal: GoalHandle,
        subject: UserId,
    ) -> Result<(), ProtocolError> {
        let permit = self
            .goal_command_permit(authz, goal, GoalCollaborationChange::RevokeAccess)
            .await?;
        self.storage()
            .goal_command
            .goal_collaboration
            .revoke_access(goal, subject, &permit)
            .await
            .map_err(super::goal_write::map_goal_storage_error)
    }

    /// Publish an exact admission with the source owner's explicit consent.
    /// Consent covers the Goal's current and future managed audience.
    ///
    /// # Errors
    /// Refuses unreadable sources, missing source administration, host
    /// vetoes, and stale source or Goal authority.
    pub async fn publish_to_goal(
        &self,
        authz: &AuthzContext,
        input: &GoalPublicationInput,
    ) -> Result<(), ProtocolError> {
        let source_entry = self
            .authorize_entry_read(authz, EntityId::Memory(input.memory))
            .await?;
        let source = self
            .authorize_goal_source(authz, *source_entry.owner())
            .await?;
        let permit = self
            .goal_command_permit(authz, input.goal, GoalCollaborationChange::Publish)
            .await?;
        self.storage()
            .goal_command
            .goal_collaboration
            .publish(input, &permit, &source)
            .await
            .map_err(super::goal_write::map_goal_storage_error)
    }

    /// Withdraw one exact publication without deleting the owner's memory.
    ///
    /// # Errors
    /// Refuses host vetoes and callers lacking current Goal-management or
    /// source-owner authority.
    pub async fn unpublish_from_goal(
        &self,
        authz: &AuthzContext,
        goal: GoalHandle,
        memory: MemoryId,
    ) -> Result<(), ProtocolError> {
        let permit = self
            .goal_command_permit(authz, goal, GoalCollaborationChange::Unpublish)
            .await?;
        self.storage()
            .goal_command
            .goal_collaboration
            .unpublish(goal, memory, &permit)
            .await
            .map_err(super::goal_write::map_goal_storage_error)
    }

    /// Delegate editing of one series through a same-owner working Goal.
    ///
    /// # Errors
    /// Refuses missing source administration, host vetoes, cross-owner
    /// bindings, and stale source or Goal authority.
    pub async fn bind_goal_work(
        &self,
        authz: &AuthzContext,
        input: &GoalWorkBindingInput,
    ) -> Result<(), ProtocolError> {
        let permit = self
            .goal_command_permit(authz, input.goal, GoalCollaborationChange::BindWork)
            .await?;
        let source = self
            .authorize_goal_source(authz, permit.controller())
            .await?;
        self.storage()
            .goal_command
            .goal_collaboration
            .bind_work(input, &permit, &source)
            .await
            .map_err(super::goal_write::map_goal_storage_error)
    }

    /// Remove a work binding; this does not erase its independent series.
    ///
    /// # Errors
    /// Refuses host vetoes and callers without fresh Goal-management or
    /// source-owner authority.
    pub async fn unbind_goal_work(
        &self,
        authz: &AuthzContext,
        input: &GoalWorkBindingInput,
    ) -> Result<(), ProtocolError> {
        let permit = self
            .goal_command_permit(authz, input.goal, GoalCollaborationChange::UnbindWork)
            .await?;
        self.storage()
            .goal_command
            .goal_collaboration
            .unbind_work(input, &permit)
            .await
            .map_err(super::goal_write::map_goal_storage_error)
    }

    async fn authorize_goal_source(
        &self,
        authz: &AuthzContext,
        owner: OwnerId,
    ) -> Result<crate::storage_ports::OwnerWritePermit, ProtocolError> {
        let permit = self.authorize_owner_admin(authz, &owner).await?;
        if *permit.owner() != owner
            || !(OwnerId::from(
                authz
                    .subject()
                    .ok_or_else(|| ProtocolError::forbidden("authenticated subject required"))?,
            ) == owner
                || authz.may_manage(&owner))
        {
            return Err(ProtocolError::forbidden(
                "source-owner sharing authority required",
            ));
        }
        Ok(permit.into_owner_write())
    }
}
