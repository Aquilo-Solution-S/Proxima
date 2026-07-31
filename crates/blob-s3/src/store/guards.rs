//! What every verb checks before it touches Postgres or S3.
//!
//! Read and write are gated separately and asymmetrically — see
//! [`ensure_owner_access`] and [`ensure_owner_write_access`] — so the two
//! sit next to each other rather than at opposite ends of a long file.

use std::time::Duration;

use aws_sdk_s3::presigning::PresigningConfig;
use proxima_core::{AccessKind, AuthzContext, Owner, OwnerRef};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::dto::CitedBlobUploadPrepareTs;
use crate::error::BlobError;

pub(super) fn presign_config(ttl_seconds: u64) -> Result<PresigningConfig, BlobError> {
    PresigningConfig::expires_in(Duration::from_secs(ttl_seconds))
        .map_err(|e| BlobError::Config(format!("invalid presign TTL: {e}")))
}

pub(super) fn validate_prepare(
    req: &CitedBlobUploadPrepareTs,
    max_blob_bytes: Option<u64>,
) -> Result<(), BlobError> {
    if req.filename.trim().is_empty() {
        return Err(BlobError::State("filename is required".into()));
    }
    if req.mime.trim().is_empty() {
        return Err(BlobError::State("mime is required".into()));
    }
    if req.byte_len > i64::MAX as u64 {
        return Err(BlobError::State("byte_len exceeds Postgres bigint".into()));
    }
    if let Some(max) = max_blob_bytes
        && req.byte_len > max
    {
        return Err(BlobError::State(format!(
            "byte_len {} exceeds max blob size {max}",
            req.byte_len
        )));
    }
    Ok(())
}

/// Gate a blob READ on host-resolved Fact-read authority for `owner`, rather
/// than trusting the client-supplied `owner` field alone. Symmetric with
/// [`ensure_owner_write_access`]: a cited blob is a Fact-attached payload, so
/// read access is `may_read(owner, Fact)` — the same per-kind role ceiling the
/// write gate uses, not a coarser "any accessible principal" check that a
/// Goal-only-read role could slip through.
pub(super) fn ensure_owner_access(ctx: &AuthzContext, owner: &Owner) -> Result<(), BlobError> {
    if ctx.may_read(owner, AccessKind::Fact) {
        Ok(())
    } else {
        Err(BlobError::Denied(
            "owner is not readable for this authorization context".into(),
        ))
    }
}

/// Gate a blob WRITE (prepare/complete/abort) on host-resolved write authority,
/// not mere read access. A cited blob is a Fact-attached payload, so the caller
/// must hold Fact-write (Ingest/Editor/Admin) on `owner`: a read-only group
/// Viewer, though it can *read* the group, must not be able to mint pending rows
/// or canonical cited-blob rows in the group's namespace. Also rejects World, which never owns cited blobs.
pub(super) fn ensure_owner_write_access(
    ctx: &AuthzContext,
    owner: &Owner,
) -> Result<(), BlobError> {
    ensure_write_owner(owner)?;
    if ctx.may_write(owner, AccessKind::Fact) {
        Ok(())
    } else {
        Err(BlobError::Denied(
            "owner is not writable for this authorization context".into(),
        ))
    }
}

pub(super) fn parse_uuid(value: &str) -> Result<Uuid, BlobError> {
    Uuid::parse_str(value).map_err(|_| BlobError::State(format!("invalid uuid: {value}")))
}

pub(super) fn ensure_write_owner(owner: &Owner) -> Result<(), BlobError> {
    if matches!(owner, OwnerRef::World) {
        return Err(BlobError::State(
            "World is read-only and cannot own cited blobs".into(),
        ));
    }
    Ok(())
}

pub(super) fn format_time(value: OffsetDateTime) -> Result<String, BlobError> {
    value
        .format(&Rfc3339)
        .map_err(|e| BlobError::State(e.to_string()))
}

#[cfg(test)]
mod tests {
    use proxima_core::UserId;

    use super::super::testkit::prepare_req;
    use super::*;

    #[test]
    fn world_cannot_prepare_cited_blob_write() {
        let err = ensure_write_owner(&OwnerRef::World).expect_err("world write rejected");
        assert!(err.to_string().contains("World is read-only"));
    }

    #[test]
    fn validate_prepare_rejects_byte_len_over_cap() {
        let err = validate_prepare(&prepare_req(1_025), Some(1_024))
            .expect_err("over-cap byte_len rejected");
        assert!(err.to_string().contains("exceeds max blob size"));
    }

    #[test]
    fn validate_prepare_allows_within_cap_and_when_uncapped() {
        validate_prepare(&prepare_req(1_024), Some(1_024)).expect("at-cap accepted");
        validate_prepare(&prepare_req(u64::from(u32::MAX)), None).expect("uncapped accepted");
    }

    #[test]
    fn owner_access_gate_allows_accessible_owner() {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let ctx = AuthzContext::single_owner(&owner, proxima_core::AuthPath::System);
        ensure_owner_access(&ctx, &owner).expect("accessible owner passes");
    }

    #[test]
    fn owner_access_gate_denies_foreign_owner() {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let ctx = AuthzContext::single_owner(&owner, proxima_core::AuthPath::System);
        let foreign = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let err = ensure_owner_access(&ctx, &foreign).expect_err("foreign owner denied");
        assert!(matches!(err, BlobError::Denied(_)));
    }

    // Blob writes require WRITE authority, not mere read access. A group
    // Viewer can read the group (owner_access gate passes) but must not be able
    // to create cited blobs in it (owner_write gate denies).
    #[test]
    fn owner_write_gate_denies_read_only_group_viewer() {
        use proxima_core::{GroupId, Role};
        let subject = UserId::new(Uuid::now_v7());
        let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let ctx = AuthzContext::for_subject_with_role(
            subject,
            [(group, Role::viewer())],
            proxima_core::AuthPath::HostBearer,
        );
        // Read gate passes (the viewer can see the group)…
        ensure_owner_access(&ctx, &group).expect("viewer can read the group");
        // …but the write gate denies.
        let err = ensure_owner_write_access(&ctx, &group).expect_err("viewer cannot write");
        assert!(matches!(err, BlobError::Denied(_)));
    }

    #[test]
    fn owner_write_gate_allows_editor_group_and_self() {
        use proxima_core::{GroupId, Role};
        let subject = UserId::new(Uuid::now_v7());
        let group = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let ctx = AuthzContext::for_subject_with_role(
            subject,
            [(group, Role::editor())],
            proxima_core::AuthPath::HostBearer,
        );
        ensure_owner_write_access(&ctx, &group).expect("editor can write the group");
        ensure_owner_write_access(&ctx, &OwnerRef::Personal(subject)).expect("self is writable");
    }
}
