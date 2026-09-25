//! Transactional publication of listenable Facts (issue #305).
//!
//! A schema declares itself listenable through [`crate::FactPayload::LISTENABLE`].
//! Every admitted Fact of such a schema captures ONE immutable `CloudEvents`
//! 1.0 record, keyed by the Fact's `t`, in the same transaction as the
//! `memory` row and its typed sidecars. This module owns the pieces of that
//! capture that are pure data:
//!
//! - [`PublicationSource`] — the configured producer identity, never
//!   caller-supplied.
//! - [`PublicationLimits`] / [`PublicationConfig`] — the deployment's export
//!   size cap and pending-queue bound.
//! - [`PublicationDraft`] — everything core resolves BEFORE storage is
//!   called. It is plain `Clone` data so the bounded write retry can re-run
//!   the whole transaction body without re-deriving it.
//! - [`PublicationExtensions`] — the host-bound `CloudEvents` extension
//!   attributes every captured event of this deployment carries.
//! - [`SealedPublication`] — the envelope bytes and their digest, sealed
//!   against the `t` storage minted.
//!
//! The port that drains the captured records lives in
//! [`crate::storage_ports::publication`]; it is host-only and is reachable
//! from no flavor-facing facade.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

use crate::mcp::handles::{PrefixedUuidClass, format_prefixed_uuid};
use crate::{Owner, SchemaId, SchemaVersion};

/// Default ceiling on records that have not been published yet.
pub const DEFAULT_MAX_PENDING: u64 = 100_000;

/// Default ceiling on one sealed envelope, in bytes.
pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 524_288;

/// `CloudEvents` `specversion` this substrate emits. 1.0.2 is a patch release
/// of the 1.0 line and the attribute stays `"1.0"`.
pub const CLOUDEVENTS_SPEC_VERSION: &str = "1.0";

/// `CloudEvents` `datacontenttype` this substrate emits. The `data` member is
/// the typed payload's serde JSON, always.
pub const CLOUDEVENTS_DATA_CONTENT_TYPE: &str = "application/json";

/// The configured producer identity bound to every event this deployment
/// captures.
///
/// `CloudEvents` requires `source` to be a non-empty URI-reference; this
/// newtype additionally refuses a relative reference, because a delivered
/// event has no base URI to resolve one against. Practically: an absolute
/// URI or a URN — something containing a scheme separator — with no
/// whitespace and no control characters.
///
/// It is deployment configuration and NEVER a caller-supplied value: a
/// producer that could assert its own source could impersonate another
/// installation to every consumer downstream.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicationSource(String);

impl PublicationSource {
    /// Validate and wrap a configured source identity.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationSourceError`] when the value is empty, carries
    /// whitespace or control characters, or is not an absolute URI/URN.
    pub fn new(raw: impl Into<String>) -> Result<Self, PublicationSourceError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(PublicationSourceError::Empty);
        }
        if raw
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"')
        {
            return Err(PublicationSourceError::IllegalCharacter { value: raw });
        }
        let Some((scheme, rest)) = raw.split_once(':') else {
            return Err(PublicationSourceError::NotAbsolute { value: raw });
        };
        if scheme.is_empty() || rest.is_empty() {
            return Err(PublicationSourceError::NotAbsolute { value: raw });
        }
        if !scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            || !scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            return Err(PublicationSourceError::NotAbsolute { value: raw });
        }
        Ok(Self(raw))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PublicationSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PublicationSource {
    type Err = PublicationSourceError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublicationSourceError {
    #[error("publication source must not be empty")]
    Empty,
    #[error("publication source {value:?} must not carry whitespace or control characters")]
    IllegalCharacter { value: String },
    #[error("publication source {value:?} must be an absolute URI or a URN (scheme:body)")]
    NotAbsolute { value: String },
}

/// Deployment bounds on capture. Both are refusals, not evictions: an
/// oversized export or an exhausted queue fails the Fact write rather than
/// dropping a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicationLimits {
    /// Ceiling on records in a non-`published` state. Reaching it refuses
    /// further listenable writes ([`PublicationError::CapacityExhausted`]).
    pub max_pending: u64,
    /// Ceiling on one sealed envelope's byte length
    /// ([`PublicationError::PayloadTooLarge`]).
    pub max_payload_bytes: usize,
}

impl Default for PublicationLimits {
    fn default() -> Self {
        Self {
            max_pending: DEFAULT_MAX_PENDING,
            max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
        }
    }
}

/// The publication half of the engine/storage configuration.
///
/// `source` is `None` in a deployment that registers no listenable schema.
/// A frozen registry that DOES register one and leaves it `None` is a build
/// error — see [`PublicationConfig::validate_against`] — and a listenable
/// write refuses with [`PublicationError::SourceUnbound`] regardless, so an
/// unvalidated composition cannot leak an unsourced event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicationConfig {
    pub source: Option<PublicationSource>,
    pub limits: PublicationLimits,
}

impl PublicationConfig {
    #[must_use]
    pub fn new(source: PublicationSource) -> Self {
        Self {
            source: Some(source),
            limits: PublicationLimits::default(),
        }
    }

    #[must_use]
    pub const fn with_limits(mut self, limits: PublicationLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Refuse a configuration that binds no source while `listenable`
    /// schemas are registered.
    ///
    /// `listenable` is the frozen registry's listenable schema ids. The
    /// error names them, because "which schema made this required" is the
    /// only question an operator has when the boot fails.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError::SourceUnbound`] naming the registered
    /// listenable schema ids.
    pub fn validate_against<'a>(
        &self,
        listenable: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), PublicationError> {
        if self.source.is_some() {
            return Ok(());
        }
        let mut ids: Vec<&str> = listenable.into_iter().collect();
        if ids.is_empty() {
            return Ok(());
        }
        ids.sort_unstable();
        ids.dedup();
        Err(PublicationError::SourceUnbound {
            schema_id: ids.join(", "),
        })
    }
}

/// Ceiling on host-bound extension attributes carried by one event.
///
/// Small on purpose: these attributes travel on EVERY captured Fact of a
/// listenable schema, and a broker header set that grows with the host's
/// imagination is a cost every consumer pays forever.
pub const MAX_PUBLICATION_EXTENSIONS: usize = 8;

/// `CloudEvents` 1.0.2 §3.1 ceiling on an extension attribute NAME.
pub const MAX_EXTENSION_NAME_CHARS: usize = 20;

/// Ceiling on one string-valued extension attribute, in bytes.
pub const MAX_EXTENSION_VALUE_BYTES: usize = 256;

/// The `CloudEvents` 1.0.2 core context attribute names, which an extension
/// may never shadow. `data_base64` is included although this substrate only
/// emits structured JSON: a consumer that re-encodes into binary mode would
/// otherwise find two things claiming that name.
const CLOUDEVENTS_CORE_ATTRIBUTES: [&str; 10] = [
    "specversion",
    "id",
    "source",
    "type",
    "datacontenttype",
    "dataschema",
    "subject",
    "time",
    "data",
    "data_base64",
];

/// Extension-name prefix this substrate keeps for itself (`proximaowner`,
/// `proximamodel`, and whatever a later release adds).
const PROXIMA_EXTENSION_PREFIX: &str = "proxima";

/// One extension attribute's value.
///
/// The three types the `CloudEvents` 1.0.2 JSON format serializes without a
/// canonicalization question: a JSON string, a JSON number that is a 32-bit
/// signed integer (the spec's `Integer` is `int32`), and a JSON boolean.
/// `Timestamp`, `URI` and `Binary` are deliberately absent — each has a
/// string rendering the host can produce itself, and each would otherwise
/// give the substrate a second chance to format it differently.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(untagged)]
pub enum ExtensionValue {
    String(String),
    Integer(i32),
    Boolean(bool),
}

impl From<String> for ExtensionValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for ExtensionValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<i32> for ExtensionValue {
    fn from(value: i32) -> Self {
        Self::Integer(value)
    }
}

impl From<bool> for ExtensionValue {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

/// A validated, name-ordered, bounded set of `CloudEvents` extension
/// attributes a HOST binds to every Fact it captures.
///
/// Name-ordered because the envelope bytes are the stored artifact: the
/// digest, the broker's dedup key and every consumer's signature check are
/// over them, so the attribute order cannot depend on the order a host
/// happened to bind things in. A `BTreeMap` makes "sorted by name" a
/// property of the type rather than of the caller.
///
/// Validation is at binding time, not at seal time: an attribute a
/// consumer would refuse is a host misconfiguration, and the refusal
/// belongs at the boundary that produced it rather than on the first Fact
/// write hours later.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicationExtensions(BTreeMap<String, ExtensionValue>);

impl PublicationExtensions {
    /// The empty set. A deployment that binds nothing emits no extension
    /// attributes at all.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind one more attribute.
    ///
    /// # Errors
    ///
    /// [`PublicationExtensionsError`], one variant per rule: an illegal or
    /// over-long name, a core or reserved name, an empty/over-long/control
    /// character string value, a name already bound, or more than
    /// [`MAX_PUBLICATION_EXTENSIONS`] attributes.
    pub fn with(
        mut self,
        name: impl Into<String>,
        value: impl Into<ExtensionValue>,
    ) -> Result<Self, PublicationExtensionsError> {
        self.bind(name.into(), value.into())?;
        Ok(self)
    }

    /// Fold `other` in, keeping the set additive: a name bound on either
    /// side is an error rather than an overwrite. Nothing can replace or
    /// clear provenance a host already bound.
    ///
    /// # Errors
    ///
    /// [`PublicationExtensionsError::DuplicateName`] for a name both sets
    /// carry, [`PublicationExtensionsError::TooMany`] when the union is
    /// over the ceiling.
    pub fn merged(mut self, other: Self) -> Result<Self, PublicationExtensionsError> {
        for (name, value) in other.0 {
            self.bind(name, value)?;
        }
        Ok(self)
    }

    fn bind(
        &mut self,
        name: String,
        value: ExtensionValue,
    ) -> Result<(), PublicationExtensionsError> {
        validate_extension_name(&name)?;
        validate_extension_value(&name, &value)?;
        if self.0.contains_key(&name) {
            return Err(PublicationExtensionsError::DuplicateName { name });
        }
        if self.0.len() >= MAX_PUBLICATION_EXTENSIONS {
            return Err(PublicationExtensionsError::TooMany {
                name,
                max: MAX_PUBLICATION_EXTENSIONS,
            });
        }
        self.0.insert(name, value);
        Ok(())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ExtensionValue> {
        self.0.get(name)
    }

    /// The bound attributes in name order — the order they serialize in.
    pub fn iter(&self) -> ExtensionEntries<'_> {
        self.0.iter().map(borrow_entry as fn(_) -> _)
    }
}

/// One bound attribute, borrowed.
pub type ExtensionEntry<'a> = (&'a str, &'a ExtensionValue);

/// The iterator [`PublicationExtensions::iter`] returns. Named rather than
/// `impl Iterator` so `&PublicationExtensions` can also be `IntoIterator`.
pub type ExtensionEntries<'a> = std::iter::Map<
    std::collections::btree_map::Iter<'a, String, ExtensionValue>,
    fn((&'a String, &'a ExtensionValue)) -> ExtensionEntry<'a>,
>;

fn borrow_entry<'a>((name, value): (&'a String, &'a ExtensionValue)) -> ExtensionEntry<'a> {
    (name.as_str(), value)
}

impl<'a> IntoIterator for &'a PublicationExtensions {
    type Item = ExtensionEntry<'a>;
    type IntoIter = ExtensionEntries<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl serde::Serialize for PublicationExtensions {
    /// Serializes as a MAP so the envelope can flatten it inline, in name
    /// order, between the substrate's own attributes and `data`.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter())
    }
}

/// Why a host-bound extension attribute was refused.
///
/// A configuration fault in the host, never a caller fault: nothing on the
/// wire can reach the binder.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublicationExtensionsError {
    #[error("extension attribute name must not be empty")]
    EmptyName,
    #[error(
        "extension attribute name {name:?} must use only lowercase letters and digits \
         (CloudEvents 1.0.2 §3.1)"
    )]
    IllegalNameCharacter { name: String },
    #[error(
        "extension attribute name {name:?} is {chars} characters, \
         over the {max}-character limit"
    )]
    NameTooLong {
        name: String,
        chars: usize,
        max: usize,
    },
    #[error("extension attribute name {name:?} is a CloudEvents core context attribute")]
    CoreAttributeName { name: String },
    #[error("extension attribute name {name:?} uses the reserved {prefix:?} prefix")]
    ReservedName { name: String, prefix: &'static str },
    #[error("extension attribute {name:?} must not carry an empty string value")]
    EmptyValue { name: String },
    #[error("extension attribute {name:?} must not carry control characters")]
    IllegalValueCharacter { name: String },
    #[error("extension attribute {name:?} is {bytes} bytes, over the {max}-byte limit")]
    ValueTooLong {
        name: String,
        bytes: usize,
        max: usize,
    },
    #[error("extension attribute {name:?} is already bound; a bound attribute is never replaced")]
    DuplicateName { name: String },
    #[error("extension attribute {name:?} is over the {max}-attribute limit")]
    TooMany { name: String, max: usize },
}

fn validate_extension_name(name: &str) -> Result<(), PublicationExtensionsError> {
    if name.is_empty() {
        return Err(PublicationExtensionsError::EmptyName);
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        return Err(PublicationExtensionsError::IllegalNameCharacter {
            name: name.to_owned(),
        });
    }
    if name.len() > MAX_EXTENSION_NAME_CHARS {
        return Err(PublicationExtensionsError::NameTooLong {
            name: name.to_owned(),
            chars: name.len(),
            max: MAX_EXTENSION_NAME_CHARS,
        });
    }
    if CLOUDEVENTS_CORE_ATTRIBUTES.contains(&name) {
        return Err(PublicationExtensionsError::CoreAttributeName {
            name: name.to_owned(),
        });
    }
    if name.starts_with(PROXIMA_EXTENSION_PREFIX) {
        return Err(PublicationExtensionsError::ReservedName {
            name: name.to_owned(),
            prefix: PROXIMA_EXTENSION_PREFIX,
        });
    }
    Ok(())
}

fn validate_extension_value(
    name: &str,
    value: &ExtensionValue,
) -> Result<(), PublicationExtensionsError> {
    let ExtensionValue::String(text) = value else {
        return Ok(());
    };
    if text.is_empty() {
        return Err(PublicationExtensionsError::EmptyValue {
            name: name.to_owned(),
        });
    }
    if text.chars().any(char::is_control) {
        return Err(PublicationExtensionsError::IllegalValueCharacter {
            name: name.to_owned(),
        });
    }
    if text.len() > MAX_EXTENSION_VALUE_BYTES {
        return Err(PublicationExtensionsError::ValueTooLong {
            name: name.to_owned(),
            bytes: text.len(),
            max: MAX_EXTENSION_VALUE_BYTES,
        });
    }
    Ok(())
}

/// Everything core resolves about one listenable Fact before storage opens
/// its transaction.
///
/// Plain data, deliberately: the bounded retry around begin→body→commit
/// re-runs the transaction body, and a draft that had to be re-derived from
/// an authorization context would be a second chance to derive it
/// differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationDraft {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    /// `CloudEvents` `type`. The registered schema id, verbatim: the version
    /// travels in `dataschema`, and a type token that folded it in would
    /// make every consumer subscription version-pinned.
    pub event_type: String,
    /// `CloudEvents` `dataschema`. Resolves against this release's own
    /// catalog (`proxima://schema/{schema_id}/{schema_version}`); no HTTP
    /// fetch is required to validate an event.
    pub data_schema: String,
    /// The configured producer identity. Bound here, at authorization time,
    /// so an event cannot pick up a later installation's identity.
    pub source: PublicationSource,
    /// Model/runner identity certified by the authenticating token, when the
    /// deployment binds one. NEVER the caller-supplied `model_id` label.
    pub model_id: Option<String>,
    /// The server-resolved owner of the Fact being admitted.
    pub owner: Owner,
    /// Extension attributes the HOST bound to this write's authorization
    /// context. Never assembled by the code producing the payload, and
    /// never reachable from anything on the wire.
    pub extensions: PublicationExtensions,
    /// The typed payload's serde JSON — the export snapshot.
    pub data: serde_json::Value,
}

impl PublicationDraft {
    /// Build the draft for one registered listenable schema.
    #[must_use]
    pub fn new(
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        source: PublicationSource,
        owner: Owner,
        model_id: Option<String>,
        extensions: PublicationExtensions,
        data: serde_json::Value,
    ) -> Self {
        let event_type = schema_id.as_str().to_owned();
        let data_schema = data_schema_uri(&schema_id, schema_version);
        Self {
            schema_id,
            schema_version,
            event_type,
            data_schema,
            source,
            model_id,
            owner,
            extensions,
            data,
        }
    }
}

/// One listenable admission's whole capture instruction: what to seal, and
/// the bounds this deployment seals it under.
///
/// The two travel together because they are enforced together and there is
/// exactly ONE authority for the limits — the engine's
/// [`PublicationConfig`]. An earlier shape let the storage backend hold its
/// own copy, which meant a deployment could raise
/// `PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES` in one place and enforce the old
/// number in the other, with no way to notice. Storage now reads the limits
/// off the witness it is already given and cannot have a second opinion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationPlan {
    pub draft: PublicationDraft,
    pub limits: PublicationLimits,
}

impl PublicationPlan {
    #[must_use]
    pub const fn new(draft: PublicationDraft, limits: PublicationLimits) -> Self {
        Self { draft, limits }
    }
}

/// The local `dataschema` target for a registered schema.
#[must_use]
pub fn data_schema_uri(schema_id: &SchemaId, schema_version: SchemaVersion) -> String {
    format!(
        "proxima://schema/{}/{}",
        schema_id.as_str(),
        schema_version.into_inner()
    )
}

/// The `CloudEvents` 1.0 structured-JSON envelope, in a FIXED attribute
/// order.
///
/// A struct rather than a `serde_json::Map`, because the bytes are the
/// stored artifact: the digest, the broker's dedup key and every consumer's
/// signature check are over these bytes, and an attribute order that
/// depended on a map's iteration order would make the same event serialize
/// two ways.
#[derive(Debug, serde::Serialize)]
struct CloudEventEnvelope<'a> {
    specversion: &'static str,
    id: &'a str,
    source: &'a str,
    #[serde(rename = "type")]
    event_type: &'a str,
    datacontenttype: &'static str,
    dataschema: &'a str,
    time: &'a str,
    proximaowner: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    proximamodel: Option<&'a str>,
    /// Host-bound extension attributes, inlined in name order. Serialized
    /// AFTER the substrate's own attributes and BEFORE `data`, so a host
    /// binding a new attribute never reorders the ones already shipping.
    /// An empty set emits nothing.
    #[serde(flatten)]
    extensions: &'a PublicationExtensions,
    data: &'a serde_json::Value,
}

/// A captured event, sealed against the `t` storage minted for the Fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedPublication {
    /// The Fact's `t`. Also the outbox row's primary key: one Fact, one
    /// record, enforced by the database rather than by the writer.
    pub id: Uuid,
    /// `CloudEvents` `id`: the canonical wire form of `t` (`F:<uuid>`).
    pub event_id: String,
    /// The envelope, exactly as it will be delivered.
    pub bytes: Vec<u8>,
    /// blake3 of [`Self::bytes`]. The integrity witness a republication is
    /// checked against.
    pub digest: [u8; 32],
    pub event_type: String,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
}

impl SealedPublication {
    /// Seal `plan.draft` against the `t` the admission minted, under
    /// `plan.limits`.
    ///
    /// The limits arrive WITH the draft rather than beside it so that no
    /// caller can seal against a different ceiling than the engine
    /// configured — see [`PublicationPlan`].
    ///
    /// # Errors
    ///
    /// - [`PublicationError::ExportFailed`] when `t` carries no `UUIDv7`
    ///   timestamp or the envelope cannot be serialized.
    /// - [`PublicationError::PayloadTooLarge`] when the sealed bytes exceed
    ///   `plan.limits.max_payload_bytes`.
    pub fn seal(plan: &PublicationPlan, t: Uuid) -> Result<Self, PublicationError> {
        let PublicationPlan { draft, limits } = plan;
        let event_id = format_prefixed_uuid(t, PrefixedUuidClass::Fact);
        let time = uuid_v7_rfc3339_millis(t)?;
        let owner = draft.owner.external_key();
        let envelope = CloudEventEnvelope {
            specversion: CLOUDEVENTS_SPEC_VERSION,
            id: &event_id,
            source: draft.source.as_str(),
            event_type: &draft.event_type,
            datacontenttype: CLOUDEVENTS_DATA_CONTENT_TYPE,
            dataschema: &draft.data_schema,
            time: &time,
            proximaowner: &owner,
            proximamodel: draft.model_id.as_deref(),
            extensions: &draft.extensions,
            data: &draft.data,
        };
        let bytes = serde_json::to_vec(&envelope)
            .map_err(|err| PublicationError::ExportFailed(err.to_string()))?;
        if bytes.len() > limits.max_payload_bytes {
            return Err(PublicationError::PayloadTooLarge {
                bytes: bytes.len(),
                max: limits.max_payload_bytes,
            });
        }
        let digest = *blake3::hash(&bytes).as_bytes();
        Ok(Self {
            id: t,
            event_id,
            bytes,
            digest,
            event_type: draft.event_type.clone(),
            schema_id: draft.schema_id.clone(),
            schema_version: draft.schema_version,
        })
    }
}

/// The recording time of a Fact, read out of its own identity.
///
/// `memory` carries no `recorded_at` column — the time IS the `UUIDv7`
/// timestamp inside `t` — so the event's `time` is derived from the id
/// rather than stamped a second time. Millisecond precision, which is all a
/// v7 timestamp carries.
fn uuid_v7_rfc3339_millis(t: Uuid) -> Result<String, PublicationError> {
    let timestamp = t.get_timestamp().ok_or_else(|| {
        PublicationError::ExportFailed(format!("memory id {t} carries no UUIDv7 timestamp"))
    })?;
    let (seconds, nanos) = timestamp.to_unix();
    let millis = i128::from(seconds)
        .checked_mul(1_000)
        .and_then(|s| s.checked_add(i128::from(nanos / 1_000_000)))
        .ok_or_else(|| {
            PublicationError::ExportFailed(format!("memory id {t} timestamp overflow"))
        })?;
    let moment = time::OffsetDateTime::from_unix_timestamp_nanos(millis * 1_000_000)
        .map_err(|err| PublicationError::ExportFailed(format!("memory id {t} timestamp: {err}")))?;
    moment
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|err| PublicationError::ExportFailed(format!("memory id {t} timestamp: {err}")))
}

/// Why a listenable Fact write was refused.
///
/// Every variant fails the WHOLE write. Capture is not best-effort: a
/// deployment that accepted the Fact and dropped its event would have no way
/// to notice, which is the failure mode the outbox exists to remove.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublicationError {
    #[error("publication envelope is {bytes} bytes, over the {max}-byte limit")]
    PayloadTooLarge { bytes: usize, max: usize },
    #[error("publication outbox is at capacity ({pending} pending, limit {max})")]
    CapacityExhausted { pending: u64, max: u64 },
    #[error("publication export failed: {0}")]
    ExportFailed(String),
    #[error(
        "schema {schema_id} is listenable but no publication source is configured; \
         set one before admitting listenable Facts"
    )]
    SourceUnbound { schema_id: String },
    #[error(
        "schema {schema_id} is listenable and must be written through the typed API; \
         the receipt-only route carries no payload to export"
    )]
    UntypedListenableWrite { schema_id: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SchemaId, SchemaVersion, UserId};

    fn draft() -> PublicationDraft {
        PublicationDraft::new(
            SchemaId::new("test/listenable-v1".into()),
            SchemaVersion::new(1),
            PublicationSource::new("urn:proxima:test").expect("source"),
            Owner::Personal(UserId::new(Uuid::nil())),
            Some("model-x".into()),
            PublicationExtensions::new(),
            serde_json::json!({ "body": "hello" }),
        )
    }

    #[test]
    fn a_source_must_be_an_absolute_uri_or_urn() {
        assert!(PublicationSource::new("https://proxima.example/prod").is_ok());
        assert!(PublicationSource::new("urn:proxima:prod").is_ok());
        assert!(matches!(
            PublicationSource::new(""),
            Err(PublicationSourceError::Empty)
        ));
        assert!(matches!(
            PublicationSource::new("proxima prod"),
            Err(PublicationSourceError::IllegalCharacter { .. })
        ));
        assert!(matches!(
            PublicationSource::new("/relative/reference"),
            Err(PublicationSourceError::NotAbsolute { .. })
        ));
        assert!(matches!(
            PublicationSource::new("scheme:"),
            Err(PublicationSourceError::NotAbsolute { .. })
        ));
        assert_eq!(
            "urn:x:y".parse::<PublicationSource>().expect("parse"),
            PublicationSource::new("urn:x:y").expect("new")
        );
    }

    /// Bound in a deliberately non-alphabetical order, so the ordering
    /// assertion below is about the type and not about the caller.
    fn extensions() -> PublicationExtensions {
        PublicationExtensions::new()
            .with("stepid", "step-7")
            .expect("a step id is a legal attribute")
            .with("runid", "run-3")
            .expect("a run id is a legal attribute")
            .with("attempt", 2)
            .expect("an integer is a legal value")
            .with("replay", false)
            .expect("a boolean is a legal value")
    }

    #[test]
    fn the_envelope_key_order_is_fixed_and_the_time_comes_from_the_v7_id() {
        let t = Uuid::parse_str("01930000-0000-7000-8000-000000000001").expect("v7 id");
        let mut draft = draft();
        draft.extensions = extensions();
        let sealed = SealedPublication::seal(
            &PublicationPlan::new(draft, PublicationLimits::default()),
            t,
        )
        .expect("seal");
        let text = String::from_utf8(sealed.bytes.clone()).expect("utf8");
        // The ORDER assertion reads the serialized text: a parsed `Map` is a
        // container whose iteration order is a build-feature detail, and the
        // bytes are what the digest and the consumer see.
        let mut cursor = 0usize;
        for key in [
            "\"specversion\"",
            "\"id\"",
            "\"source\"",
            "\"type\"",
            "\"datacontenttype\"",
            "\"dataschema\"",
            "\"time\"",
            "\"proximaowner\"",
            "\"proximamodel\"",
            // Host-bound extensions: after the substrate's own attributes,
            // before `data`, in NAME order rather than binding order.
            "\"attempt\":2",
            "\"replay\":false",
            "\"runid\":\"run-3\"",
            "\"stepid\":\"step-7\"",
            "\"data\"",
        ] {
            let at = text[cursor..]
                .find(key)
                .unwrap_or_else(|| panic!("{key} missing or out of order in {text}"));
            cursor += at + key.len();
        }
        assert_eq!(sealed.event_id, format!("F:{t}"));

        let (seconds, nanos) = t.get_timestamp().expect("v7 timestamp").to_unix();
        let expected = time::OffsetDateTime::from_unix_timestamp_nanos(
            i128::from(seconds) * 1_000_000_000 + i128::from(nanos / 1_000_000) * 1_000_000,
        )
        .expect("moment")
        .format(&time::format_description::well_known::Rfc3339)
        .expect("rfc3339");
        assert!(
            text.contains(&format!("\"time\":\"{expected}\"")),
            "{text} should carry the v7 time {expected}"
        );
        assert!(expected.ends_with('Z'), "{expected} must be UTC-normalized");
        assert_eq!(sealed.digest, *blake3::hash(&sealed.bytes).as_bytes());
    }

    #[test]
    fn an_oversized_export_is_refused_rather_than_truncated() {
        let limits = PublicationLimits {
            max_payload_bytes: 16,
            ..PublicationLimits::default()
        };
        let err = SealedPublication::seal(&PublicationPlan::new(draft(), limits), Uuid::now_v7())
            .expect_err("too big");
        assert!(matches!(
            err,
            PublicationError::PayloadTooLarge { max: 16, .. }
        ));
    }

    #[test]
    fn an_unbound_source_names_every_listenable_schema() {
        let config = PublicationConfig::default();
        assert!(config.validate_against(Vec::<&str>::new()).is_ok());
        let err = config
            .validate_against(vec!["b/two-v1", "a/one-v1", "b/two-v1"])
            .expect_err("unbound");
        assert_eq!(
            err,
            PublicationError::SourceUnbound {
                schema_id: "a/one-v1, b/two-v1".into()
            }
        );
        assert!(
            PublicationConfig::new(PublicationSource::new("urn:x:y").expect("source"))
                .validate_against(vec!["a/one-v1"])
                .is_ok()
        );
    }

    #[test]
    fn sealing_the_same_bindings_twice_produces_the_same_bytes() {
        let t = Uuid::parse_str("01930000-0000-7000-8000-000000000002").expect("v7 id");
        let seal_once = |ext: PublicationExtensions| {
            let mut draft = draft();
            draft.extensions = ext;
            SealedPublication::seal(
                &PublicationPlan::new(draft, PublicationLimits::default()),
                t,
            )
            .expect("seal")
        };
        // Same attributes, opposite binding order.
        let forwards = seal_once(
            PublicationExtensions::new()
                .with("runid", "r")
                .expect("bind")
                .with("stepid", "s")
                .expect("bind"),
        );
        let backwards = seal_once(
            PublicationExtensions::new()
                .with("stepid", "s")
                .expect("bind")
                .with("runid", "r")
                .expect("bind"),
        );
        assert_eq!(forwards.bytes, backwards.bytes);
        assert_eq!(forwards.digest, backwards.digest);
    }

    #[test]
    fn an_extension_name_follows_the_cloudevents_naming_rule() {
        let empty = PublicationExtensions::new();
        assert!(matches!(
            empty.clone().with("", "v"),
            Err(PublicationExtensionsError::EmptyName)
        ));
        assert!(matches!(
            empty.clone().with("run_id", "v"),
            Err(PublicationExtensionsError::IllegalNameCharacter { .. })
        ));
        assert!(matches!(
            empty.clone().with("RunId", "v"),
            Err(PublicationExtensionsError::IllegalNameCharacter { .. })
        ));
        assert!(matches!(
            empty.clone().with("a".repeat(21), "v"),
            Err(PublicationExtensionsError::NameTooLong { max: 20, .. })
        ));
        assert!(empty.clone().with("a".repeat(20), "v").is_ok());
        assert!(matches!(
            empty.clone().with("time", "v"),
            Err(PublicationExtensionsError::CoreAttributeName { .. })
        ));
        assert!(matches!(
            empty.clone().with("data", "v"),
            Err(PublicationExtensionsError::CoreAttributeName { .. })
        ));
        assert!(matches!(
            empty.with("proximaowner", "v"),
            Err(PublicationExtensionsError::ReservedName {
                prefix: "proxima",
                ..
            })
        ));
    }

    #[test]
    fn a_string_value_is_bounded_and_printable() {
        let empty = PublicationExtensions::new();
        assert!(matches!(
            empty.clone().with("runid", ""),
            Err(PublicationExtensionsError::EmptyValue { .. })
        ));
        assert!(matches!(
            empty.clone().with("runid", "a\nb"),
            Err(PublicationExtensionsError::IllegalValueCharacter { .. })
        ));
        assert!(matches!(
            empty.clone().with("runid", "x".repeat(257)),
            Err(PublicationExtensionsError::ValueTooLong { max: 256, .. })
        ));
        assert!(empty.with("runid", "x".repeat(256)).is_ok());
    }

    #[test]
    fn the_set_is_bounded_and_a_bound_name_is_never_replaced() {
        let mut set = PublicationExtensions::new();
        for index in 0..8 {
            set = set.with(format!("a{index}"), index).expect("within bounds");
        }
        assert_eq!(set.len(), 8);
        assert!(matches!(
            set.clone().with("a9", 9),
            Err(PublicationExtensionsError::TooMany { max: 8, .. })
        ));
        assert!(matches!(
            set.clone().with("a0", "other"),
            Err(PublicationExtensionsError::DuplicateName { .. })
        ));
        assert_eq!(set.get("a0"), Some(&ExtensionValue::Integer(0)));
        assert!(PublicationExtensions::new().is_empty());
    }

    #[test]
    fn merging_is_additive_and_refuses_a_name_both_sides_bind() {
        let host = PublicationExtensions::new()
            .with("runid", "r")
            .expect("bind");
        let more = PublicationExtensions::new()
            .with("stepid", "s")
            .expect("bind");
        let merged = host.clone().merged(more).expect("disjoint sets merge");
        assert_eq!(
            merged.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            vec!["runid", "stepid"]
        );
        assert!(matches!(
            host.merged(
                PublicationExtensions::new()
                    .with("runid", "other")
                    .expect("bind")
            ),
            Err(PublicationExtensionsError::DuplicateName { .. })
        ));
    }
}
