use super::Engine;
use crate::GoalPayload;
use crate::authz::{AuthzContext, MemoryAction, Role};
use crate::error::ProtocolError;
use crate::storage::StorageError;
use crate::verbs::goal_write::{
    CreateGoalAtomicRequest, GoalAtomicContext, GoalCreateRequest, GoalDraft, GoalPayloadWrite,
    GoalWriteBuildError, GoalWriteOutcome,
};
use crate::verbs::schema::PayloadKind;

impl Engine {
    /// Create an Active typed Goal for an embedded host or protocol
    /// caller without exposing `proxima_core.goals` storage shape.
    ///
    /// The request must name the target Self Perspective explicitly;
    /// current Proxima Goal assignment is `Goal --core/inspires--> Self`,
    /// not a detached owner-scoped Goal row.
    ///
    /// # Errors
    ///
    /// Returns `Forbidden` when `authz` cannot access the request Owner,
    /// lacks `graph_write`, or lacks a `memory.write` grant on the owner space;
    /// `UnknownSchema` when the typed [`GoalPayload`] schema is not registered
    /// as a Goal; `InvalidArgument` for malformed title/text/evidence/parent
    /// references; or `Internal` for storage failures.
    pub async fn create_goal<P>(
        &self,
        authz: &AuthzContext,
        request: GoalCreateRequest<P>,
    ) -> Result<GoalWriteOutcome, ProtocolError>
    where
        P: GoalPayload,
    {
        super::authorize_action(
            authz,
            &request.principal,
            Role::GraphWrite,
            MemoryAction::Write,
        )?;

        let GoalCreateRequest {
            principal,
            target_self_perspective_id,
            title,
            text,
            payload,
            request_id,
            evidence,
            parent_goal_ids,
            authorship,
            author_self_perspective_id,
        } = request;

        let mut payload_write =
            GoalPayloadWrite::from_payload(title, text, payload).map_err(map_goal_build_error)?;
        let schema = self
            .registry()
            .lookup_payload(
                &payload_write.schema_id,
                payload_write.schema_version,
                PayloadKind::Goal,
            )
            .ok_or_else(|| {
                ProtocolError::unknown_schema(
                    payload_write.schema_id.as_str(),
                    payload_write.schema_version.into_inner(),
                )
            })?;
        if schema.sidecar_table.is_none() {
            payload_write.sidecar_payload = None;
        }

        let embedding_client = self.embed_client();
        let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
        let draft = GoalDraft::active_from_payload_write(
            principal,
            payload_write,
            parent_goal_ids,
            authorship,
            request_id,
        );
        let outcome = self
            .storage()
            .create_goal_atomic(&CreateGoalAtomicRequest {
                draft,
                context: GoalAtomicContext {
                    registry: self.registry(),
                    embedding_model_id,
                    author_self_perspective_id,
                },
                target_self_perspective_id,
                evidence,
            })
            .await
            .map_err(map_goal_storage_error)?;
        Ok(outcome)
    }
}

fn map_goal_build_error(err: GoalWriteBuildError) -> ProtocolError {
    match err {
        GoalWriteBuildError::InvalidTitle => {
            ProtocolError::invalid_argument("title", err.to_string())
        }
        GoalWriteBuildError::InvalidText => {
            ProtocolError::invalid_argument("text", err.to_string())
        }
    }
}

fn map_goal_storage_error(err: StorageError) -> ProtocolError {
    match err {
        StorageError::NotFound => ProtocolError::not_found("goal write referenced row not found"),
        StorageError::ConstraintViolation(message)
            if message.starts_with("idempotency_conflict:") =>
        {
            let request_id = message
                .strip_prefix("idempotency_conflict:")
                .unwrap_or(message.as_str());
            ProtocolError::idempotency_conflict(request_id)
        }
        StorageError::ConstraintViolation(message) | StorageError::Conflict(message) => {
            ProtocolError::invalid_argument("goal", message)
        }
        StorageError::Unavailable(message) | StorageError::Internal(message) => {
            ProtocolError::internal(message)
        }
    }
}
