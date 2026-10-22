//! One meaning for a tool `idempotency_key`: same key and content replays;
//! changed content under a known key is a conflict naming the existing handle.
//!
//! Every keyed MCP/REST write surface reaches a verdict only through
//! [`judge`]. Storage supplies the stored `(handle, fingerprint equal?)`.

use crate::canonical_json_bytes;
use crate::text_bounds::check_trimmed_len;
use uuid::Uuid;

/// Validated tool `idempotency_key` (1..=[`IdempotencyKey::MAX_CHARS`] after trim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub const MAX_CHARS: usize = 180;

    /// # Errors
    ///
    /// Returns the rejection reason when the trimmed key is empty or longer
    /// than `MAX_CHARS`.
    pub fn new(raw: impl Into<String>) -> Result<Self, String> {
        let raw = raw.into();
        match check_trimmed_len(&raw, Self::MAX_CHARS) {
            Ok(trimmed) => Ok(Self(trimmed.to_string())),
            Err(violation) => Err(violation.reason("idempotency_key")),
        }
    }

    /// [`Self::new`] under a tighter bound `max` (at most
    /// [`Self::MAX_CHARS`]), for a key that later keys are derived from.
    /// The reason has the same wording as [`Self::new`]'s, plus `hint`.
    ///
    /// # Errors
    ///
    /// Returns the rejection reason when the trimmed key is empty or longer
    /// than `max`.
    pub fn new_bounded(raw: impl Into<String>, max: usize, hint: &str) -> Result<Self, String> {
        let raw = raw.into();
        match check_trimmed_len(&raw, max.min(Self::MAX_CHARS)) {
            Ok(trimmed) => Ok(Self(trimmed.to_string())),
            Err(violation) => Err(format!("{} {hint}", violation.reason("idempotency_key"))),
        }
    }

    #[must_use]
    pub fn generated(prefix: &str) -> Self {
        Self(format!("{prefix}:{}", uuid::Uuid::now_v7()))
    }

    /// # Errors
    ///
    /// Returns an error when `raw` is present but fails [`Self::new`].
    pub fn optional_or_generated(prefix: &str, raw: Option<String>) -> Result<Self, String> {
        raw.map_or_else(|| Ok(Self::generated(prefix)), Self::new)
    }

    /// # Errors
    ///
    /// Returns an error when the child request key would exceed the
    /// common idempotency-key bound.
    pub fn child(&self, prefix: &str, index: usize) -> Result<Self, String> {
        Self::new(format!("{prefix}:{}:{index}", self.0))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// One kind of write = one key space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyScope {
    Note,
    Utterance,
    Abstraction,
    Perspective,
}

impl KeyScope {
    /// Source id segment used in the request ledger (`core/key/<scope>`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Utterance => "utterance",
            Self::Abstraction => "abstraction",
            Self::Perspective => "perspective",
        }
    }

    /// Full `ingest_keys.source_id` for this scope's request ledger.
    #[must_use]
    pub fn ledger_source(self) -> String {
        format!("core/key/{}", self.as_str())
    }
}

const FINGERPRINT_DOMAIN: &[u8] = b"proxima-request-v1";

/// What a keyed request asserts. Built only through [`ContentFingerprint::builder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentFingerprint([u8; 32]);

impl ContentFingerprint {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    #[must_use]
    pub fn builder(scope: KeyScope) -> ContentFingerprintBuilder {
        ContentFingerprintBuilder::new(scope)
    }
}

/// Tagged, length-prefixed field builder. An absent field is encoded
/// distinctly from an empty one.
#[derive(Debug)]
pub struct ContentFingerprintBuilder {
    hasher: blake3::Hasher,
}

impl ContentFingerprintBuilder {
    fn new(scope: KeyScope) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(FINGERPRINT_DOMAIN);
        hasher.update(&[0]);
        hasher.update(scope.as_str().as_bytes());
        Self { hasher }
    }

    fn tag(&mut self, name: &[u8]) {
        self.hasher.update(&[0]);
        self.hasher.update(&(name.len() as u64).to_be_bytes());
        self.hasher.update(name);
    }

    /// A field's name and the byte saying a value follows.
    fn present(&mut self, field: &str) {
        self.tag(field.as_bytes());
        self.hasher.update(&[1]);
    }

    /// A field's name and the byte saying no value follows.
    fn absent(mut self, field: &str) -> Self {
        self.tag(field.as_bytes());
        self.hasher.update(&[0]);
        self
    }

    /// Present trimmed text field.
    #[must_use]
    pub fn text(self, field: &str, value: &str) -> Self {
        self.bytes(field, value.trim().as_bytes())
    }

    /// Explicitly absent text field (distinct from empty).
    #[must_use]
    pub fn absent_text(self, field: &str) -> Self {
        self.absent(field)
    }

    /// Present byte payload (schema id, version, canonical JSON, …).
    #[must_use]
    pub fn bytes(mut self, field: &str, value: &[u8]) -> Self {
        self.present(field);
        self.hasher.update(&(value.len() as u64).to_be_bytes());
        self.hasher.update(value);
        self
    }

    #[must_use]
    pub fn absent_bytes(self, field: &str) -> Self {
        self.absent(field)
    }

    /// Schema-identified payload: schema id + version + canonical JSON bytes.
    #[must_use]
    pub fn schema_payload(
        self,
        field: &str,
        schema_id: &str,
        schema_version: u32,
        payload: &serde_json::Value,
    ) -> Self {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(schema_id.len() as u64).to_be_bytes());
        buf.extend_from_slice(schema_id.as_bytes());
        buf.extend_from_slice(&schema_version.to_be_bytes());
        buf.extend_from_slice(&canonical_json_bytes(payload));
        self.bytes(field, &buf)
    }

    /// Sorted, deduplicated UUID set (origins, refs, …).
    #[must_use]
    pub fn uuid_set(mut self, field: &str, ids: impl IntoIterator<Item = Uuid>) -> Self {
        let mut ids: Vec<Uuid> = ids.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        self.present(field);
        self.hasher.update(&(ids.len() as u64).to_be_bytes());
        for id in ids {
            self.hasher.update(id.as_bytes());
        }
        self
    }

    #[must_use]
    pub fn absent_uuid_set(self, field: &str) -> Self {
        self.absent(field)
    }

    /// Normalized lowercase tags, sorted and deduplicated.
    #[must_use]
    pub fn tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut tags: Vec<String> = tags
            .into_iter()
            .map(|t| t.as_ref().trim().to_ascii_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        tags.sort_unstable();
        tags.dedup();
        self.present("tags");
        self.hasher.update(&(tags.len() as u64).to_be_bytes());
        for tag in tags {
            self.hasher.update(&(tag.len() as u64).to_be_bytes());
            self.hasher.update(tag.as_bytes());
        }
        self
    }

    #[must_use]
    pub fn build(self) -> ContentFingerprint {
        ContentFingerprint(*self.hasher.finalize().as_bytes())
    }
}

/// A keyed request: scope + key + content fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestKey {
    scope: KeyScope,
    key: IdempotencyKey,
    fingerprint: ContentFingerprint,
}

impl RequestKey {
    #[must_use]
    pub fn new(scope: KeyScope, key: IdempotencyKey, fingerprint: ContentFingerprint) -> Self {
        Self {
            scope,
            key,
            fingerprint,
        }
    }

    #[must_use]
    pub fn scope(&self) -> KeyScope {
        self.scope
    }

    #[must_use]
    pub fn key(&self) -> &IdempotencyKey {
        &self.key
    }

    #[must_use]
    pub fn fingerprint(&self) -> ContentFingerprint {
        self.fingerprint
    }

    /// `ingest_keys.ingest_key` for the request-id row: `r:<hex blake3(key)>`.
    #[must_use]
    pub fn request_row_key(&self) -> String {
        let digest = blake3::hash(self.key.as_str().as_bytes());
        format!("r:{}", hex::encode(digest.as_bytes()))
    }

    /// `ingest_keys.ingest_key` for the fingerprint row:
    /// `f:<hex blake3(len‖key‖fingerprint)>`.
    #[must_use]
    pub fn fingerprint_row_key(&self) -> String {
        let key = self.key.as_str().as_bytes();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&(key.len() as u64).to_be_bytes());
        hasher.update(key);
        hasher.update(self.fingerprint.as_bytes());
        format!("f:{}", hex::encode(hasher.finalize().as_bytes()))
    }
}

/// Verdict when a keyed request may proceed or replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyVerdict<H> {
    /// No prior record: admit.
    Fresh,
    /// Prior record with equal fingerprint: return the existing handle.
    Replay(H),
}

/// A keyed request that already names another content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyConflict {
    pub key: String,
    pub existing: Uuid,
}

/// The rule: no record → Fresh; equal fingerprint → Replay; other fingerprint → Conflict.
///
/// # Errors
///
/// Returns [`KeyConflict`] when a prior record exists with a different fingerprint.
pub fn judge<H: ExistingHandle>(
    stored: Option<(H, bool /* fingerprint equal */)>,
    key: &IdempotencyKey,
) -> Result<KeyVerdict<H>, KeyConflict> {
    match stored {
        None => Ok(KeyVerdict::Fresh),
        Some((handle, true)) => Ok(KeyVerdict::Replay(handle)),
        Some((handle, false)) => Err(KeyConflict {
            key: key.as_str().to_string(),
            existing: handle.as_existing_uuid(),
        }),
    }
}

/// Types that can name the existing handle in a conflict.
pub trait ExistingHandle {
    fn as_existing_uuid(&self) -> Uuid;
}

impl ExistingHandle for Uuid {
    fn as_existing_uuid(&self) -> Uuid {
        *self
    }
}

impl ExistingHandle for crate::MemoryId {
    fn as_existing_uuid(&self) -> Uuid {
        (*self).into_inner()
    }
}

impl ExistingHandle for crate::GoalId {
    fn as_existing_uuid(&self) -> Uuid {
        (*self).into_inner()
    }
}

/// Replay handle plus whether the named memory/goal is forgotten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayHandle {
    pub id: Uuid,
    pub forgotten: bool,
}

impl ExistingHandle for ReplayHandle {
    fn as_existing_uuid(&self) -> Uuid {
        self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn judge_fresh_replay_conflict() {
        let key = IdempotencyKey::new("k").unwrap();
        assert!(matches!(judge::<Uuid>(None, &key), Ok(KeyVerdict::Fresh)));
        let id = Uuid::nil();
        assert!(matches!(
            judge(Some((id, true)), &key),
            Ok(KeyVerdict::Replay(_))
        ));
        let err = judge(Some((id, false)), &key).unwrap_err();
        assert_eq!(err.key, "k");
        assert_eq!(err.existing, id);
    }

    /// The encoding is frozen: this digest, taken before the framing was shared, must not move.
    #[test]
    fn the_fingerprint_encoding_does_not_move() {
        let id = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        let fingerprint = ContentFingerprint::builder(KeyScope::Perspective)
            .text("text", "  padded  ")
            .absent_text("no_text")
            .bytes("bytes", b"\x00\x01")
            .absent_bytes("no_bytes")
            .schema_payload(
                "payload",
                "schema/v1",
                3,
                &serde_json::json!({"b": 1, "a": [2]}),
            )
            .uuid_set("ids", [id, Uuid::nil(), id])
            .absent_uuid_set("no_ids")
            .tags([" B ", "a", "b", ""])
            .build();
        assert_eq!(
            fingerprint.to_hex(),
            "939efb5d99a8cdcf34c60842cd71d988e2ec3d2c8b832b1b15d60967d8db5a26"
        );
    }

    #[test]
    fn fingerprint_absent_differs_from_empty() {
        let a = ContentFingerprint::builder(KeyScope::Note)
            .text("title", "")
            .build();
        let b = ContentFingerprint::builder(KeyScope::Note)
            .absent_text("title")
            .build();
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_tags_order_independent() {
        let a = ContentFingerprint::builder(KeyScope::Note)
            .tags(["b", "a"])
            .build();
        let b = ContentFingerprint::builder(KeyScope::Note)
            .tags(["a", "b", "a"])
            .build();
        assert_eq!(a, b);
    }

    #[test]
    fn request_row_keys_are_stable() {
        let key = IdempotencyKey::new("stable").unwrap();
        let fp = ContentFingerprint::builder(KeyScope::Note)
            .text("title", "t")
            .text("body", "b")
            .tags(std::iter::empty::<&str>())
            .build();
        let rk = RequestKey::new(KeyScope::Note, key, fp);
        assert!(rk.request_row_key().starts_with("r:"));
        assert!(rk.fingerprint_row_key().starts_with("f:"));
        assert_eq!(rk.request_row_key(), rk.request_row_key());
    }
}
