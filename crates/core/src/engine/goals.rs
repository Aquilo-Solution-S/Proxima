use super::{Engine, map_storage_err_for_goal_write};
use crate::GoalId;
use crate::authz::{AuthzContext, Role};
use crate::error::ProtocolError;
use crate::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use crate::verbs::schema::PayloadKind;

impl Engine {
    /// docs/14 §"`GoalWrite`" — Owner-scoped write. Validates
    /// schema is registered as `PayloadKind::Goal` and delegates to
    /// storage.
    ///
    /// # Errors
    ///
    /// Returns `ProtocolError::Forbidden` when the context cannot access
    /// the owner or lacks the graph-write role, `ProtocolError::UnknownSchema`
    /// when the schema is not registered or not a Goal, or
    /// `ProtocolError::Internal` for storage failures.
    pub async fn write_goal(
        &self,
        authz: &AuthzContext,
        mut draft: GoalDraft,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        super::authorize(authz, &draft.principal, Role::GraphWrite)?;
        let owner = authz.scoped_owner(draft.principal.clone());
        draft.stamp_owner(owner);
        // Validate the schema is a registered Goal AND the payload decodes and
        // passes the registered validator — symmetric with EventIngest. Only
        // the schema *kind* was checked before, so an empty / non-object goal
        // payload was written unvalidated.
        self.validate_json_payload(
            &draft.schema_id,
            draft.schema_version,
            PayloadKind::Goal,
            &draft.payload,
            "payload",
        )?;
        self.storage
            .write_goal_atomic(&draft)
            .await
            .map_err(map_storage_err_for_goal_write(&draft.request_id))
    }

    /// docs/14 §"`GoalWrite`" — supersede path. Same auth and schema
    /// validation as `write_goal`, plus validates prior exists and
    /// belongs to the same owner.
    ///
    /// # Errors
    ///
    /// Returns `ProtocolError::Forbidden` when the context cannot access
    /// the owner or lacks the graph-write role, `ProtocolError::UnknownSchema`
    /// when the schema is not registered or not a Goal,
    /// `ProtocolError::NotFound` when the prior goal does not exist, or
    /// `ProtocolError::Internal` for storage failures.
    pub async fn supersede_goal(
        &self,
        authz: &AuthzContext,
        prior: GoalId,
        mut draft: GoalDraft,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        super::authorize(authz, &draft.principal, Role::GraphWrite)?;
        let owner = authz.scoped_owner(draft.principal.clone());
        draft.stamp_owner(owner);
        // Same schema + payload validation as `write_goal`.
        self.validate_json_payload(
            &draft.schema_id,
            draft.schema_version,
            PayloadKind::Goal,
            &draft.payload,
            "payload",
        )?;
        self.storage
            .supersede_goal_atomic(prior, &draft)
            .await
            .map_err(map_storage_err_for_goal_write(&draft.request_id))
    }
}
