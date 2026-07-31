//! Completing an upload, and recording that it happened.
//!
//! [`Engine::complete_upload_as_fact`] is the whole upload-completion verb:
//! it finishes the blob-store side and mints the `core/upload-v1` Fact that
//! cites the resulting artefact. Callers that want the Fact — which is all
//! of them, since "a file entered the corpus" is a substrate guarantee —
//! use this instead of reaching for [`CitedBlobPort::complete_upload`]
//! directly.
//!
//! The blob port arrives as an argument rather than as an `Engine` field.
//! The store is host-wired and optional (a host without S3 has none), while
//! the `Engine` is not; making it a field would push that optionality into
//! every construction site to serve one verb.

use uuid::Uuid;

use super::Engine;
use super::errors::map_write_storage_error;
use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::citations::{UPLOADED_BLOB_WHOLE_SCHEMA_ID, UploadedBlobWholeV1};
use crate::error::ProtocolError;
use crate::storage_ports::{CitedBlobPort, CitedBlobUploadCompleted};
use crate::verbs::fact_ingest::{FactWriteCommand, InlineCitationMappingDraft};
use crate::{
    CitationMappingPayload, FactIngestOutcome, OwnerRef, SchemaId, SchemaVersion, SidecarPayload,
    SourceBatchId, UploadV1,
};

/// Provenance of every upload Fact. Constant, not per-call: the source is
/// the upload lane itself, and the identity of the individual file lives in
/// the receipt key (see [`UploadV1::receipt_key`]). A per-call source id
/// would make every completion a distinct receipt and defeat replay.
const UPLOAD_SOURCE_ID: &str = "core/upload";

/// A finished upload: the stored artefact, and the Fact that records it.
#[derive(Debug, Clone)]
pub struct UploadCompleted {
    /// What the blob store holds. `idempotent_replay` here is about the
    /// BYTES: true when these bytes were already in the corpus.
    pub blob: CitedBlobUploadCompleted,
    /// The `core/upload-v1` Fact citing that artefact.
    /// `idempotent_replay` here is about the EVENT: true when this owner's
    /// upload of these bytes was already recorded.
    pub fact: FactIngestOutcome,
}

impl Engine {
    /// Complete a pending upload and record it as a `core/upload-v1` Fact
    /// citing the stored artefact.
    ///
    /// `extensions` are extra typed sidecar rows a flavor wants against
    /// this Fact — extra columns on an event the substrate defines. They
    /// land in the Fact's own transaction or not at all; pass `&[]` for
    /// none. A flavor may only ADD rows against schemas it has registered,
    /// and cannot alter the Fact itself.
    ///
    /// # Idempotency
    ///
    /// Safe to call again with the same `upload_id`. The blob side is
    /// content-addressed and returns the stored artefact unchanged; the
    /// Fact side replays on its receipt and returns the same `memory_id`.
    /// Retrying is therefore also the repair path for the gap below.
    ///
    /// # The two-transaction gap
    ///
    /// The artefact is committed by the blob store before the Fact write
    /// begins — two transactions, because [`CitedBlobPort`] owns the first
    /// and this crate cannot reach inside it. A crash in between leaves a
    /// cited object that nothing cites: recoverable (complete again) but
    /// not atomic, and until it is completed again the corpus holds a file
    /// with no record of its arrival. Closing that is a separate change to
    /// the boundary itself, not something a caller can arrange.
    ///
    /// # Errors
    ///
    /// Returns the blob store's failures mapped like any write (a missing,
    /// expired, aborted, or length-mismatched upload is `InvalidArgument`);
    /// `Forbidden` when `authz` resolves no single writable owner;
    /// `InvalidArgument` when the Fact write is rejected; and `Internal`
    /// when the resolved write owner is not the owner the artefact was
    /// stored under.
    pub async fn complete_upload_as_fact(
        &self,
        blobs: &dyn CitedBlobPort,
        authz: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
        extensions: &[SidecarPayload],
    ) -> Result<UploadCompleted, ProtocolError> {
        let blob = blobs
            .complete_upload(authz, owner, upload_id)
            .await
            .map_err(|err| map_write_storage_error(err, "upload_id", "upload not found"))?;

        let cited_object_id = Uuid::parse_str(&blob.cited_object_id).map_err(|err| {
            ProtocolError::internal(format!(
                "blob store returned a malformed cited_object_id {}: {err}",
                blob.cited_object_id,
            ))
        })?;

        let payload = UploadV1 {
            filename: blob.filename.clone(),
            mime: blob.mime.clone(),
            byte_len: blob.byte_len,
            content_hash: blob.content_hash.clone(),
        };
        // No lexical language is stamped: a filename is not prose, and
        // guessing a text-search configuration from one would be a guess
        // about the document, which nobody has read yet.
        let draft = FactWriteCommand::from_payload(
            UPLOAD_SOURCE_ID,
            SourceBatchId::new(Uuid::now_v7()),
            &payload,
            time::OffsetDateTime::now_utc(),
        );

        // By reference, not inline: the cited object already exists with
        // its full typed payload. The inline path would upsert an opaque
        // object on the same content hash; the by-ref path makes storage
        // verify existence, owner, and schema in the transaction that
        // writes the citation.
        let authorized = self
            .authorize_fact_with_citation_by_ref(
                authz,
                Relation::Editor,
                draft,
                cited_object_id,
                whole_blob_mapping(),
            )
            .await?;

        let resolved = *authorized.owner_write_permit().owner();
        if resolved != owner {
            return Err(ProtocolError::internal(format!(
                "upload was stored under {owner:?} but the Fact write resolved to {resolved:?}; \
                 refusing to record an upload against an owner that does not hold the artefact",
            )));
        }

        let embedding_client = self.embed_client();
        let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
        let fact = self
            .ingest_fact_with_citation_ref_and_typed_sidecar(
                &authorized,
                extensions,
                embedding_model_id,
            )
            .await?;

        Ok(UploadCompleted { blob, fact })
    }
}

/// "This Fact came from that artefact", with no locator — the mapping is
/// the whole citation. `UploadedBlobWholeV1` is a braced empty struct, so
/// its wire form is `{}`; it is serialized rather than hardcoded so the
/// two cannot drift.
fn whole_blob_mapping() -> InlineCitationMappingDraft {
    InlineCitationMappingDraft {
        schema_id: SchemaId::new(UPLOADED_BLOB_WHOLE_SCHEMA_ID.to_string()),
        schema_version: SchemaVersion::new(UploadedBlobWholeV1::SCHEMA_VERSION),
        payload_bytes: serde_json::to_vec(&UploadedBlobWholeV1 {})
            .expect("UploadedBlobWholeV1 is an empty struct and always serializes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mapping this verb sends must be the one core registered for
    /// whole-artefact citations, and must target the uploaded-blob
    /// cited-object schema — otherwise `authorize_fact_with_citation_by_ref`
    /// would refuse it, at runtime, on every upload.
    #[test]
    fn the_mapping_targets_the_uploaded_blob_schema() {
        let mapping = whole_blob_mapping();

        assert_eq!(mapping.schema_id.as_str(), UPLOADED_BLOB_WHOLE_SCHEMA_ID);
        assert_eq!(
            UploadedBlobWholeV1::cited_object_schema().as_str(),
            crate::citations::UPLOADED_BLOB_SCHEMA_ID
        );
    }

    /// The typed ingest boundary requires every payload to be a JSON
    /// object; `{}` is what an empty mapping must look like on the wire.
    #[test]
    fn the_mapping_payload_is_a_json_object() {
        assert_eq!(whole_blob_mapping().payload_bytes, b"{}");
    }
}
