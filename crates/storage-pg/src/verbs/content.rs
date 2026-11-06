//! Owner-scoped Content upsert.

use proxiima_core::count_u64;
use proxiima_core::{SidecarPayload, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;

/// Blake3-32 of each schema's durable encoding, in `schema_id` order.
pub fn hash_sidecar_payloads(payloads: &[SidecarPayload]) -> Result<[u8; 32], StorageError> {
    if payloads.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "content hash requires at least one sidecar payload".into(),
        ));
    }
    let mut parts = Vec::with_capacity(payloads.len());
    for payload in payloads {
        let durable = payload
            .durable()
            .map_err(StorageError::ConstraintViolation)?;
        parts.push((
            payload.schema_id.as_str().to_owned(),
            durable.canonical_bytes(),
        ));
    }
    parts.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proxiima-content-v1\0");
    for (schema, bytes) in &parts {
        hasher.update(schema.as_bytes());
        hasher.update(b"\0");
        hasher.update(&(count_u64(bytes.len())).to_le_bytes());
        hasher.update(bytes);
        hasher.update(b"\0");
    }
    Ok(*hasher.finalize().as_bytes())
}

/// A derived admission's Content hash: the payload hash keyed by the
/// admission's kind.
///
/// Content is visible only through an admission that names it (0023), so an
/// Abstraction and a Perspective with the same payload get two rows: a writer
/// then meets only Content of a kind it reads, and reuse never looks past its
/// own limit.
#[must_use]
pub fn kind_scoped_hash(kind: &str, hash: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proxiima-content-kind-v1\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(hash);
    *hasher.finalize().as_bytes()
}

/// Insert or reuse `(owner, schema, hash)`.
///
/// Neither `RETURNING` nor a conflict target: each makes the insert read the
/// new row, and a Content row is visible only through an admission that
/// names it (0023), which the new row has not got yet. The fresh
/// `content_id` cannot conflict, so any conflict is `(owner, schema, hash)`.
pub async fn ensure_content(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    schema_id: &str,
    content_hash: &[u8; 32],
) -> Result<Uuid, StorageError> {
    let content_id = Uuid::now_v7();
    let inserted = sqlx::query(
        "INSERT INTO proxiima_core.content (content_id, owner_id, schema_id, content_hash)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING",
    )
    .bind(content_id)
    .bind(owner_id)
    .bind(schema_id)
    .bind(content_hash.as_slice())
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    if inserted.rows_affected() == 1 {
        return Ok(content_id);
    }
    sqlx::query_scalar(
        "SELECT content_id FROM proxiima_core.content
          WHERE owner_id = $1 AND schema_id = $2 AND content_hash = $3",
    )
    .bind(owner_id)
    .bind(schema_id)
    .bind(content_hash.as_slice())
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)
}

/// A/P without a hashed sidecar still get an owner-scoped Content row.
pub async fn ensure_text_content(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    kind: &str,
    schema_id: &str,
    text: &[u8],
) -> Result<Uuid, StorageError> {
    ensure_content(
        tx,
        owner_id,
        schema_id,
        &kind_scoped_hash(kind, &text_hash(schema_id, text)),
    )
    .await
}

/// The Content hash of a derived admission without a hashed sidecar.
pub fn text_hash(schema_id: &str, text: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proxiima-content-text-v1\0");
    hasher.update(schema_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(text);
    *hasher.finalize().as_bytes()
}

/// Content for an admission's typed payloads; a derived kind's hash is
/// [`kind_scoped_hash`]ed, a Fact's is not.
pub async fn ensure_content_from_payloads(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    kind: &str,
    schema_id: &str,
    payloads: &[SidecarPayload],
) -> Result<Option<Uuid>, StorageError> {
    if payloads.is_empty() {
        return Ok(None);
    }
    let mut hash = hash_sidecar_payloads(payloads)?;
    if kind != "fact" {
        hash = kind_scoped_hash(kind, &hash);
    }
    Ok(Some(ensure_content(tx, owner_id, schema_id, &hash).await?))
}

/// Drop Content rows no admission, live or forgotten, names.
pub async fn gc_unreferenced_content(
    tx: &mut Transaction<'_, Postgres>,
    content_id: Uuid,
) -> Result<(), StorageError> {
    sqlx::query(
        "DELETE FROM proxiima_core.content c
          WHERE c.content_id = $1
            AND NOT EXISTS (
                    SELECT 1 FROM proxiima_core.memory m WHERE m.content_id = c.content_id /* FORGET-POLICY: includes-forgotten (a forgotten memory still names its content) */
                )",
    )
    .bind(content_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}

/// The same orphan anti-join over a whole set of Content ids, in one
/// statement.
///
/// Owner-scope erase calls this, not [`gc_unreferenced_content`] in a loop:
/// one round trip per distinct `content_id` is an N+1 leg inside a path whose
/// whole argument is that it is set-based. `GC_UNREFERENCED_BLOBS_SQL` is the
/// same anti-join over an array.
pub async fn gc_unreferenced_content_batch(
    tx: &mut Transaction<'_, Postgres>,
    content_ids: &[Uuid],
) -> Result<u64, StorageError> {
    if content_ids.is_empty() {
        return Ok(0);
    }
    Ok(sqlx::query(
        "DELETE FROM proxiima_core.content c
          WHERE c.content_id = ANY($1::uuid[])
            AND NOT EXISTS (
                    SELECT 1 FROM proxiima_core.memory m WHERE m.content_id = c.content_id /* FORGET-POLICY: includes-forgotten (a forgotten memory still names its content) */
                )",
    )
    .bind(content_ids)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?
    .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::hash_sidecar_payloads;
    use proxiima_core::{
        DurablePayload, DurablePayloadBuilder, FactPayload, KeyField, PayloadDisplay,
        PayloadProjection, SidecarPayload,
    };

    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct AliasedFact {
        #[serde(rename = "wire_body")]
        body: String,
    }

    impl FactPayload for AliasedFact {
        const SCHEMA_ID: &'static str = "test/content-alias";
        const SCHEMA_VERSION: u32 = 1;

        fn receipt_key(&self) -> Vec<u8> {
            self.body.as_bytes().to_vec()
        }

        fn render(&self) -> String {
            self.body.clone()
        }
    }

    impl PayloadProjection for AliasedFact {
        const KEY_FIELDS: &'static [KeyField<Self>] = &[];

        fn display(&self) -> PayloadDisplay {
            PayloadDisplay {
                body: Some(self.body.clone()),
                ..PayloadDisplay::default()
            }
        }

        fn durable(&self) -> Result<DurablePayload, String> {
            let mut fields = DurablePayloadBuilder::new();
            fields.field("body", &self.body)?;
            Ok(fields.finish())
        }
    }

    #[test]
    fn content_identity_keeps_v1_bytes_when_protocol_fields_are_renamed() {
        let payload = SidecarPayload::fact(AliasedFact { body: "x".into() });
        assert_eq!(payload.to_protocol_json().unwrap()["wire_body"], "x");
        let durable = payload.durable().expect("durable");
        assert_eq!(durable.canonical_bytes(), br#"{"body":"x"}"#);

        // The pre-rename persisted v1 frame: canonical {"body":"x"}.
        let bytes = br#"{"body":"x"}"#;
        let mut legacy = blake3::Hasher::new();
        legacy.update(b"proxiima-content-v1\0");
        legacy.update(b"test/content-alias\0");
        legacy.update(&12_u64.to_le_bytes());
        legacy.update(bytes);
        legacy.update(b"\0");
        assert_eq!(
            hash_sidecar_payloads(&[payload]).unwrap(),
            *legacy.finalize().as_bytes()
        );
    }
}
