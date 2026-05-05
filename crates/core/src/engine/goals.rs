use super::{Engine, map_storage_err_for_goal_write};
use crate::GoalId;
use crate::auth::Credentials;
use crate::error::ProtocolError;
use crate::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use crate::verbs::schema::PayloadKind;

impl Engine {
    /// docs/14 §"GoalWrite" — Owner-scoped write. Validates
    /// schema is registered as PayloadKind::Goal and delegates to
    /// storage.
    pub async fn write_goal(
        &self,
        creds: &Credentials,
        draft: GoalDraft,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&draft.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        // Validate goal schema is registered AND has PayloadKind::Goal.
        match self.registry.lookup(&draft.schema_id, draft.schema_version) {
            Some(info) if info.kind == PayloadKind::Goal => {}
            _ => {
                return Err(ProtocolError::unknown_schema(
                    draft.schema_id.as_str(),
                    draft.schema_version.into_inner(),
                ));
            }
        }
        self.storage
            .write_goal_atomic(&draft)
            .await
            .map_err(map_storage_err_for_goal_write(&draft.request_id))
    }

    /// docs/14 §"GoalWrite" — supersede path. Same auth and schema
    /// validation as write_goal, plus validates prior exists and
    /// belongs to the same owner.
    pub async fn supersede_goal(
        &self,
        creds: &Credentials,
        prior: GoalId,
        draft: GoalDraft,
    ) -> Result<GoalWriteOutcome, ProtocolError> {
        let resolved = self
            .auth
            .resolve(creds)
            .map_err(|_| ProtocolError::auth_required())?;
        if !resolved.can_access_owner(&draft.owner) {
            return Err(ProtocolError::forbidden(
                "principal cannot access requested owner",
            ));
        }
        // Validate goal schema is registered AND has PayloadKind::Goal.
        match self.registry.lookup(&draft.schema_id, draft.schema_version) {
            Some(info) if info.kind == PayloadKind::Goal => {}
            _ => {
                return Err(ProtocolError::unknown_schema(
                    draft.schema_id.as_str(),
                    draft.schema_version.into_inner(),
                ));
            }
        }
        self.storage
            .supersede_goal_atomic(prior, &draft)
            .await
            .map_err(map_storage_err_for_goal_write(&draft.request_id))
    }
}
