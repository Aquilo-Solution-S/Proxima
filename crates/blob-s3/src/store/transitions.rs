//! What a status-changing `UPDATE` that changed no row should conclude.
//!
//! `finish_upload` and `abort_upload` race each other and the expiry
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
    AbortedOrExpired,
}

/// What a `finish_upload` that changed no row should do, given the status
/// the upload actually holds now.
///
/// `finish_upload` runs AFTER the transaction that recorded the artefact and
/// its Fact committed, so an error here reports a failure for a write that
/// succeeded — and leaves an `upload_id` the caller can never retry. The
/// lost race is therefore classified, not assumed: only a genuinely
/// contradictory state is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FinishTransitionDecision {
    /// This call performed the transition.
    WonPending,
    /// Someone else completed it against the same artefact. Idempotent —
    /// the same guarantee the `Completed` arm already grants a replay.
    AlreadyFinished,
    /// An abort or an expiry reached the row first, but the artefact and
    /// its Fact are already committed. Record the completion over the
    /// terminal status: the transfer's outcome IS completed, and an abort
    /// that lost this race did not undo a committed write.
    OvertookTerminal,
}

pub(super) fn finish_transition_decision(
    observed_status: UploadStatus,
    observed_cited_object_id: Option<Uuid>,
    cited_object_id: Uuid,
    rows_affected: u64,
) -> Result<FinishTransitionDecision, BlobError> {
    match rows_affected {
        1 => Ok(FinishTransitionDecision::WonPending),
        0 => match observed_status {
            UploadStatus::Completed if observed_cited_object_id == Some(cited_object_id) => {
                Ok(FinishTransitionDecision::AlreadyFinished)
            }
            // The only genuinely contradictory state: something else was
            // staged under this upload id.
            UploadStatus::Completed => Err(BlobError::State(
                "upload already completed against a different cited object".into(),
            )),
            UploadStatus::Aborted | UploadStatus::Expired => {
                Ok(FinishTransitionDecision::OvertookTerminal)
            }
            UploadStatus::Pending => Err(BlobError::State(
                "upload finish did not transition pending row".into(),
            )),
        },
        other => Err(BlobError::State(format!(
            "upload finish affected {other} rows"
        ))),
    }
}

pub(super) fn abort_transition_decision(
    observed_status: UploadStatus,
    rows_affected: u64,
) -> Result<AbortTransitionDecision, BlobError> {
    match rows_affected {
        1 => Ok(AbortTransitionDecision::WonPending),
        0 => match observed_status {
            UploadStatus::Completed => Ok(AbortTransitionDecision::Completed),
            UploadStatus::Aborted | UploadStatus::Expired => {
                Ok(AbortTransitionDecision::AbortedOrExpired)
            }
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
        // The case that used to be reported as "aborted/expired": another
        // caller finished this upload against the same artefact. The write
        // the caller is being told about has already committed.
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
            AbortTransitionDecision::AbortedOrExpired
        );
        assert_eq!(
            abort_transition_decision(UploadStatus::Expired, 0)
                .expect("expired replay is idempotent"),
            AbortTransitionDecision::AbortedOrExpired
        );
        assert!(matches!(
            abort_transition_decision(UploadStatus::Pending, 0),
            Err(BlobError::State(message))
                if message == "upload abort did not transition pending row"
        ));
    }
}
