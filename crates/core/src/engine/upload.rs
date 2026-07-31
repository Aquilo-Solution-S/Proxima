//! Completing an upload: the artefact and the record of its arrival, in
//! one write.
//!
//! [`Engine::complete_upload_as_fact`] is the whole completion verb. There
//! is no other: [`CitedBlobPort`] stages bytes and is told what they were
//! recorded as, but it no longer persists anything, so a caller reaching
//! past this verb would leave the corpus with nothing in it.
//!
//! The shape follows from where the transaction has to be. Persisting a
//! cited object is a Fact write with an inline citation — storage already
//! upserts the object on `(owner, schema, content_hash)` and inserts its
//! typed row through the registered cited-object sidecar — so once the
//! Fact carries the artefact as its citation, the two cannot come apart.
//! What could not be in that transaction is the object-store work:
//! streaming, hashing, and copying an S3 object is not a database
//! statement and must not hold a transaction open while it runs. Hence the
//! split: [`CitedBlobPort::stage_upload`] before, one transaction, then
//! [`CitedBlobPort::finish_upload`] after.
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
use crate::citations::{
    UPLOADED_BLOB_SCHEMA_ID, UPLOADED_BLOB_WHOLE_SCHEMA_ID, UploadedBlobPayload,
    UploadedBlobWholeV1,
};
use crate::error::ProtocolError;
use crate::storage_ports::{CitedBlobPort, CitedBlobUploadCompleted};
use crate::verbs::fact_ingest::{
    FactWriteCommand, InlineCitationMappingDraft, InlineCitedObjectDraft,
};
use crate::{
    CitationMappingPayload, CitedObjectPayload, FactIngestOutcome, OwnerRef, SchemaId,
    SchemaVersion, SidecarPayload, SourceBatchId, UploadV1,
};

/// Provenance of every upload Fact. Constant, not per-call: the source is
/// the upload lane itself, and the identity of the individual file lives in
/// the receipt key (see [`UploadV1::receipt_key`]). A per-call source id
/// would make every completion a distinct receipt and defeat replay.
const UPLOAD_SOURCE_ID: &str = "core/upload";

/// A finished upload: the stored artefact, and the Fact that records it.
/// Both were written together, so neither exists without the other.
#[derive(Debug, Clone)]
pub struct UploadCompleted {
    /// The artefact as the corpus now holds it.
    pub blob: CitedBlobUploadCompleted,
    /// The `core/upload-v1` Fact citing that artefact.
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
    /// # What is atomic
    ///
    /// The cited object, its typed `cited_uploaded_blob_v1` row, the
    /// citation, the Fact, its receipt, its embedding job, and every
    /// extension row are one transaction. Nothing a reader can observe
    /// survives a failure partway: an artefact with no recorded arrival is
    /// not a state this verb can leave behind.
    ///
    /// Two things sit outside it, both deliberately, and neither is corpus
    /// content. Staging streams and copies an S3 object — no transaction
    /// may be held open across that. Finishing marks the upload row and
    /// deletes the redundant pending object. A crash before finishing
    /// leaves an upload row still saying `pending` whose artefact is
    /// already recorded; completing the same upload again resolves it.
    ///
    /// # Idempotency
    ///
    /// Safe to call again with the same `upload_id`. Staging is
    /// content-addressed, the Fact replays on its receipt and returns the
    /// same `memory_id` and cited object, and finishing tolerates an
    /// upload already completed against the same artefact.
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
        // Staging does the object-store half and stops: bytes verified,
        // moved to their canonical content-addressed key, nothing
        // recorded. Everything below this line is one transaction.
        let staged = blobs
            .stage_upload(authz, owner, upload_id)
            .await
            .map_err(|err| map_write_storage_error(err, "upload_id", "upload not found"))?;

        let content_hash = hex::encode(staged.payload.content_hash);
        let payload = UploadV1 {
            filename: staged.payload.filename.clone(),
            mime: staged.payload.mime.clone(),
            byte_len: staged.payload.byte_len,
            content_hash: content_hash.clone(),
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

        // Inline, carrying the artefact's full typed payload. Storage
        // upserts the cited object on `(owner, schema, content_hash)` —
        // the same key the blob lane deduplicates on — inserts its
        // `cited_uploaded_blob_v1` row through the registered cited-object
        // sidecar, and writes the citation and the Fact, all in the one
        // transaction. That is the point of the split: the artefact and
        // the record of its arrival are now the same write.
        let cited_object = InlineCitedObjectDraft {
            schema_id: SchemaId::new(UPLOADED_BLOB_SCHEMA_ID.to_string()),
            schema_version: SchemaVersion::new(UploadedBlobPayload::SCHEMA_VERSION),
            payload_bytes: serde_json::to_vec(&staged.payload).map_err(|err| {
                ProtocolError::internal(format!("serialize staged blob payload: {err}"))
            })?,
        };
        let authorized = self
            .authorize_fact_with_citation(
                authz,
                Relation::Editor,
                draft,
                cited_object,
                whole_blob_mapping(),
            )
            .await?;

        let resolved = *authorized.owner_write_permit().owner();
        if resolved != owner {
            return Err(ProtocolError::internal(format!(
                "upload was staged under {owner:?} but the Fact write resolved to {resolved:?}; \
                 refusing to record an upload against an owner that does not hold the artefact",
            )));
        }

        let embedding_client = self.embed_client();
        let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
        let fact = self
            .ingest_fact_with_citation_and_typed_sidecar(
                &authorized,
                extensions,
                embedding_model_id,
            )
            .await?;

        // Present on every citation-bearing write, including a replay,
        // where it is read back from the Fact that already exists.
        let cited_object_id = fact.cited_object_id.ok_or_else(|| {
            ProtocolError::internal(
                "upload Fact was written with a citation but storage reported no cited object",
            )
        })?;

        // Outside the transaction, and necessarily so: it marks the
        // upload row and deletes the now-redundant pending object, one of
        // which is not a database write at all. A crash before this leaves
        // an upload row still saying `pending` while its artefact and the
        // Fact are committed — bookkeeping for the transfer protocol,
        // invisible in the corpus, and repaired by completing again.
        blobs
            .finish_upload(authz, owner, upload_id, cited_object_id)
            .await
            .map_err(|err| map_write_storage_error(err, "upload_id", "upload not found"))?;

        let blob = CitedBlobUploadCompleted {
            cited_object_id: cited_object_id.to_string(),
            schema: UPLOADED_BLOB_SCHEMA_ID.to_string(),
            content_hash,
            sha256: hex::encode(staged.payload.sha256),
            byte_len: staged.payload.byte_len,
            mime: staged.payload.mime,
            filename: staged.payload.filename,
            // "You added nothing new." True when this owner's upload of
            // these bytes was already recorded, and when the upload id
            // itself had already been completed.
            idempotent_replay: fact.idempotent_replay || staged.already_completed.is_some(),
        };
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
