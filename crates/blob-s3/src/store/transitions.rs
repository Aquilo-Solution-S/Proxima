//! What a status-changing `UPDATE` that changed no row should conclude.
//!
//! Transactional upload publication and `abort_upload` race the expiry
//! sweep, and both re-read the row rather than assume why they lost. The
//! decision is a pure function of the observed status, so it is assertable
//! without a database — the races themselves are pinned in
//! `blob_roundtrip_pg`.

use uuid::Uuid;

use super::rows::UploadStatus;
use crate::error::BlobError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AbortTransitionDecision {
    WonPending,
    Completed,
    AlreadyAborted,
    AlreadyExpired,
}

/// Result of the guarded locator write performed after a pending object has
/// been staged.  A zero-row update is not success: the row may have become a
/// terminal state while the provider write was in flight and must be repaired
/// before the caller is given a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StageLocatorDecision {
    Staged,
    Replay(Uuid),
    RepairTerminal(UploadStatus),
}

pub(super) fn stage_locator_decision(
    observed_status: UploadStatus,
    observed_blob_id: Option<Uuid>,
    rows_affected: u64,
) -> Result<StageLocatorDecision, BlobError> {
    match rows_affected {
        1 if observed_status == UploadStatus::Pending => Ok(StageLocatorDecision::Staged),
        1 => Err(BlobError::State(
            "upload locator update affected a non-pending row".into(),
        )),
        0 => match observed_status {
            UploadStatus::Completed => observed_blob_id
                .map(StageLocatorDecision::Replay)
                .ok_or_else(|| BlobError::State("completed upload is missing blob_id".into())),
            UploadStatus::Aborted | UploadStatus::Expired => {
                Ok(StageLocatorDecision::RepairTerminal(observed_status))
            }
            UploadStatus::Pending => Err(BlobError::State(
                "upload locator update did not transition pending row".into(),
            )),
        },
        other => Err(BlobError::State(format!(
            "upload locator update affected {other} rows"
        ))),
    }
}

#[cfg(test)]
pub(super) use proxima_storage_pg::access::owner_columns::FinishTransitionDecision;

#[cfg(test)]
/// Shared publication classification, mapped into the blob error vocabulary.
pub(super) fn finish_transition_decision(
    observed_status: UploadStatus,
    observed_cited_object_id: Option<Uuid>,
    cited_object_id: Uuid,
    rows_affected: u64,
) -> Result<FinishTransitionDecision, BlobError> {
    proxima_storage_pg::access::owner_columns::finish_transition_decision(
        observed_status,
        observed_cited_object_id,
        cited_object_id,
        rows_affected,
    )
    .map_err(Into::into)
}

pub(super) fn abort_transition_decision(
    observed_status: UploadStatus,
    rows_affected: u64,
) -> Result<AbortTransitionDecision, BlobError> {
    match rows_affected {
        1 => Ok(AbortTransitionDecision::WonPending),
        0 => match observed_status {
            UploadStatus::Completed => Ok(AbortTransitionDecision::Completed),
            UploadStatus::Aborted => Ok(AbortTransitionDecision::AlreadyAborted),
            UploadStatus::Expired => Ok(AbortTransitionDecision::AlreadyExpired),
            UploadStatus::Pending => Err(BlobError::State(
                "upload abort did not transition pending row".into(),
            )),
        },
        other => Err(BlobError::State(format!(
            "upload abort affected {other} rows"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_transition_decision_is_race_idempotent() {
        let object = Uuid::now_v7();
        let other = Uuid::now_v7();

        assert_eq!(
            finish_transition_decision(UploadStatus::Pending, None, object, 1)
                .expect("pending transition wins"),
            FinishTransitionDecision::WonPending
        );
        // Concurrent finish against the same artefact: the write has
        // already committed.
        assert_eq!(
            finish_transition_decision(UploadStatus::Completed, Some(object), object, 0)
                .expect("a concurrent finish against the same artefact is idempotent"),
            FinishTransitionDecision::AlreadyFinished
        );
        // Genuinely contradictory: something else was staged under this id.
        assert!(matches!(
            finish_transition_decision(UploadStatus::Completed, Some(other), object, 0),
            Err(BlobError::State(message))
                if message.contains("different cited object")
        ));
        // An abort or an expiry that reached the row first does NOT fail the
        // completion: `cited_object_id` exists only because the corpus
        // transaction committed, so the write being reported on succeeded.
        assert_eq!(
            finish_transition_decision(UploadStatus::Aborted, None, object, 0)
                .expect("an abort that lost the race cannot fail a committed write"),
            FinishTransitionDecision::OvertookTerminal
        );
        assert_eq!(
            finish_transition_decision(UploadStatus::Expired, None, object, 0)
                .expect("an expiry sweep cannot fail a committed write"),
            FinishTransitionDecision::OvertookTerminal
        );
        assert!(matches!(
            finish_transition_decision(UploadStatus::Pending, None, object, 0),
            Err(BlobError::State(message)) if message.contains("did not transition")
        ));
    }

    #[test]
    fn abort_transition_decision_is_race_idempotent() {
        assert_eq!(
            abort_transition_decision(UploadStatus::Pending, 1).expect("pending transition wins"),
            AbortTransitionDecision::WonPending
        );
        assert_eq!(
            abort_transition_decision(UploadStatus::Completed, 0)
                .expect("completed race loss is idempotent"),
            AbortTransitionDecision::Completed
        );
        assert_eq!(
            abort_transition_decision(UploadStatus::Aborted, 0)
                .expect("aborted replay is idempotent"),
            AbortTransitionDecision::AlreadyAborted
        );
        assert_eq!(
            abort_transition_decision(UploadStatus::Expired, 0)
                .expect("expired replay is idempotent"),
            AbortTransitionDecision::AlreadyExpired
        );
        assert!(matches!(
            abort_transition_decision(UploadStatus::Pending, 0),
            Err(BlobError::State(message))
                if message == "upload abort did not transition pending row"
        ));
    }

    #[test]
    fn stage_locator_decision_is_fail_closed_and_repairable() {
        let blob = Uuid::now_v7();
        assert_eq!(
            stage_locator_decision(UploadStatus::Pending, None, 1)
                .expect("pending locator update wins"),
            StageLocatorDecision::Staged
        );
        assert_eq!(
            stage_locator_decision(UploadStatus::Completed, Some(blob), 0)
                .expect("same completed blob replays"),
            StageLocatorDecision::Replay(blob)
        );
        assert_eq!(
            stage_locator_decision(UploadStatus::Aborted, None, 0)
                .expect("aborted row needs repair"),
            StageLocatorDecision::RepairTerminal(UploadStatus::Aborted)
        );
        assert_eq!(
            stage_locator_decision(UploadStatus::Expired, None, 0)
                .expect("expired row needs repair"),
            StageLocatorDecision::RepairTerminal(UploadStatus::Expired)
        );
        assert!(stage_locator_decision(UploadStatus::Pending, None, 0).is_err());
        assert!(stage_locator_decision(UploadStatus::Completed, None, 0).is_err());
        assert!(stage_locator_decision(UploadStatus::Pending, None, 2).is_err());
    }
}
