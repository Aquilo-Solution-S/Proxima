//! Payload traits per docs/03 — typing layer required for
//! every Memory kind.
//!
//! `SCHEMA_ID` is `&'static str` (not `SchemaId`) so it can be a
//! `const`. The runtime `SchemaId` (a String wrapper) is built
//! by the `schema_id()` helper at registration time. This is a
//! deliberate divergence from the doc-illustrative
//! `const SCHEMA_ID: SchemaId = ...` shape: that requires
//! const-construction of `String`, which Rust does not allow.

use crate::edge::EdgeEndpoint;
use crate::{EntityKind, GoalId, MemoryId, SchemaId};

/// How a schema-declared reference field addresses the node it points
/// at. A binding is a property of the *field*, decided by the schema
/// author once, not a policy row consulted per write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ReferenceBinding {
    /// Point at the exact memory or goal row named by the field.
    Pin,
}

impl ReferenceBinding {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pin => "Pin",
        }
    }
}

/// One node reference read out of a payload field.
///
/// A payload that declares these is saying: *this field points at
/// another node, and I am the home of that statement*. Ingest turns each
/// one into a `Reference` index entry in the node write's own
/// transaction (docs/16 §The Model). The index answers "is there a
/// connection"; the payload answers "what is it" — ten call sites from
/// chunk A to chunk B are one index row and ten payload entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PayloadReference {
    /// Payload field the reference was read from. Diagnostics only — it
    /// is deliberately not persisted, because a field name is schema
    /// detail and the index carries no content.
    pub field: &'static str,
    pub binding: ReferenceBinding,
    pub target: EdgeEndpoint,
}

impl PayloadReference {
    /// Reference a pinned memory row.
    #[must_use]
    pub const fn memory(field: &'static str, kind: EntityKind, memory_id: MemoryId) -> Self {
        Self {
            field,
            binding: ReferenceBinding::Pin,
            target: EdgeEndpoint::memory(kind, memory_id),
        }
    }

    /// Reference a Goal row.
    #[must_use]
    pub const fn goal(field: &'static str, goal_id: GoalId) -> Self {
        Self {
            field,
            binding: ReferenceBinding::Pin,
            target: EdgeEndpoint::goal(goal_id),
        }
    }

    /// Pins always address a memory or Goal row.
    ///
    /// # Errors
    ///
    /// Never fails: the only remaining binding is [`ReferenceBinding::Pin`].
    pub fn validate(&self) -> Result<(), String> {
        let _ = self;
        Ok(())
    }
}

/// Builds the schema-owned identity bytes a payload's `receipt_key` (or
/// `goal_key`) returns: the canonical spelling of *these are the values
/// this payload declares*.
///
/// # Why the encoding is frozen
///
/// A receipt key is identity. `receipt_id` folds these bytes, so two
/// writes that build the same bytes replay onto one Fact and two that
/// differ mint two. That makes every method here a compatibility
/// surface: change what an existing one emits and every Fact whose
/// schema uses it re-mints on next registration, silently and
/// retroactively. **No existing method's emission may change, byte for
/// byte.** A new spelling arrives as a new method with a new tag byte,
/// and a schema adopts it by bumping its `SCHEMA_VERSION` — which is
/// itself part of the key, so the change is deliberate and dated.
///
/// # Encoding
///
/// The header, written by [`PayloadKeyBuilder::new`]:
///
/// ```text
/// b"PKEY1\0" ‖ u64_be(schema_id.len()) ‖ schema_id ‖ u32_be(schema_version)
/// ```
///
/// Then one frame per declared field, in call order:
///
/// ```text
/// u64_be(name.len()) ‖ name ‖ tag ‖ value
/// ```
///
/// There are no separators. Every value is either fixed-width or
/// length-prefixed, so the concatenation reads back unambiguously and no
/// field's bytes can be mistaken for the start of the next field's.
///
/// # Tags
///
/// | Tag | Method | Value bytes |
/// |---|---|---|
/// | `s` | [`field_str`](Self::field_str) | `u64_be(len) ‖ utf8` |
/// | `b` | [`field_bool`](Self::field_bool) | 1 byte, `0` or `1` |
/// | `1` | [`field_u8`](Self::field_u8) | 1 byte |
/// | `4` | [`field_u32`](Self::field_u32) | 4 bytes, big-endian |
/// | `I` | [`field_i32`](Self::field_i32) | 4 bytes, big-endian |
/// | `L` | [`field_i64`](Self::field_i64) | 8 bytes, big-endian |
/// | `8` | [`field_u64`](Self::field_u64), [`field_usize`](Self::field_usize) | 8 bytes, big-endian |
/// | `u` | [`field_uuid`](Self::field_uuid) | 16 raw bytes |
/// | `B` | [`field_bytes`](Self::field_bytes) | `u64_be(len) ‖ bytes` |
/// | `t` | [`field_time`](Self::field_time) | 16 bytes, big-endian `unix_timestamp_nanos` |
/// | `S` | [`field_option_str`](Self::field_option_str) | presence ‖ `s`'s value |
/// | `O` | [`field_option_bool`](Self::field_option_bool) | presence ‖ `b`'s value |
/// | `2` | [`field_option_u8`](Self::field_option_u8) | presence ‖ `1`'s value |
/// | `5` | [`field_option_u32`](Self::field_option_u32) | presence ‖ `4`'s value |
/// | `i` | [`field_option_i32`](Self::field_option_i32) | presence ‖ `I`'s value |
/// | `l` | [`field_option_i64`](Self::field_option_i64) | presence ‖ `L`'s value |
/// | `9` | [`field_option_u64`](Self::field_option_u64), [`field_option_usize`](Self::field_option_usize) | presence ‖ `8`'s value |
/// | `U` | [`field_option_uuid`](Self::field_option_uuid) | presence ‖ `u`'s value |
/// | `C` | [`field_option_bytes`](Self::field_option_bytes) | presence ‖ `B`'s value |
/// | `T` | [`field_option_time`](Self::field_option_time) | presence ‖ `t`'s value |
/// | `[` | [`field_str_list`](Self::field_str_list), [`field_uuid_list`](Self::field_uuid_list), [`list`](Self::list) | `u64_be(count)` ‖ elements |
///
/// A presence byte is `1` followed by the value's own bytes, or the
/// single byte `0` and nothing after it.
///
/// # The invariant
///
/// Two payloads produce the same key **iff** they declare the same
/// values. The builder exists so every schema inherits that property
/// from one place instead of re-deriving it, and so the property can be
/// tested once.
///
/// Optional fields are where it is easiest to lose, so every option kind
/// has a blessed spelling and three distinct byte sequences:
///
/// - the field is **never declared** — no frame at all;
/// - the field is declared **`None`** — the frame, then presence `0`;
/// - the field is declared **`Some(default)`** — the frame, then
///   presence `1`, then the default value's own bytes.
///
/// Those are three different statements about the world and they are
/// three different identities. A hand-rolled absence encoding collapses
/// at least two of them: spelling a missing string as `""` makes `None`
/// and `Some("")` one Fact; skipping the field when it is `None` makes
/// *not asked* and *known to be absent* one Fact, and leaves the key of
/// a payload with two optional fields depending on which of them was
/// absent. Use the `field_option_*` methods; replacing those inventions
/// is what they are for.
#[derive(Debug, Clone)]
pub struct PayloadKeyBuilder {
    bytes: Vec<u8>,
}

const TAG_STR: u8 = b's';
const TAG_BOOL: u8 = b'b';
const TAG_U8: u8 = b'1';
const TAG_U32: u8 = b'4';
const TAG_I32: u8 = b'I';
const TAG_I64: u8 = b'L';
const TAG_U64: u8 = b'8';
const TAG_UUID: u8 = b'u';
const TAG_BYTES: u8 = b'B';
const TAG_TIME: u8 = b't';
const TAG_OPTION_STR: u8 = b'S';
const TAG_OPTION_BOOL: u8 = b'O';
const TAG_OPTION_U8: u8 = b'2';
const TAG_OPTION_U32: u8 = b'5';
const TAG_OPTION_I32: u8 = b'i';
const TAG_OPTION_I64: u8 = b'l';
const TAG_OPTION_U64: u8 = b'9';
const TAG_OPTION_UUID: u8 = b'U';
const TAG_OPTION_BYTES: u8 = b'C';
const TAG_OPTION_TIME: u8 = b'T';
const TAG_LIST: u8 = b'[';

/// Every tag byte above, paired with the field kind that claims it.
///
/// Tags must be pairwise distinct: two kinds sharing a byte would let
/// two different declarations frame to the same bytes, which is the one
/// way this encoding can lose identity without anyone editing a method.
/// The table is what `every_tag_byte_is_distinct` iterates, so claiming
/// a byte and fencing it are one line apart. `field_usize` and
/// `field_option_usize` are absent by design — they delegate to the u64
/// methods and claim no byte of their own — as are the two typed list
/// helpers, which share `list`'s.
#[cfg(test)]
const TAG_TABLE: &[(&str, u8)] = &[
    ("field_str", TAG_STR),
    ("field_bool", TAG_BOOL),
    ("field_u8", TAG_U8),
    ("field_u32", TAG_U32),
    ("field_i32", TAG_I32),
    ("field_i64", TAG_I64),
    ("field_u64", TAG_U64),
    ("field_uuid", TAG_UUID),
    ("field_bytes", TAG_BYTES),
    ("field_time", TAG_TIME),
    ("field_option_str", TAG_OPTION_STR),
    ("field_option_bool", TAG_OPTION_BOOL),
    ("field_option_u8", TAG_OPTION_U8),
    ("field_option_u32", TAG_OPTION_U32),
    ("field_option_i32", TAG_OPTION_I32),
    ("field_option_i64", TAG_OPTION_I64),
    ("field_option_u64", TAG_OPTION_U64),
    ("field_option_uuid", TAG_OPTION_UUID),
    ("field_option_bytes", TAG_OPTION_BYTES),
    ("field_option_time", TAG_OPTION_TIME),
    ("list", TAG_LIST),
];

impl PayloadKeyBuilder {
    #[must_use]
    pub fn new(schema_id: &str, schema_version: u32) -> Self {
        let mut this = Self {
            bytes: b"PKEY1\0".to_vec(),
        };
        this.raw_str(schema_id);
        this.bytes.extend_from_slice(&schema_version.to_be_bytes());
        this
    }

    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }

    pub fn field_str(&mut self, name: &str, value: &str) {
        self.field(name, TAG_STR);
        self.raw_str(value);
    }

    pub fn field_bool(&mut self, name: &str, value: bool) {
        self.field(name, TAG_BOOL);
        self.bytes.push(u8::from(value));
    }

    pub fn field_u8(&mut self, name: &str, value: u8) {
        self.field(name, TAG_U8);
        self.bytes.push(value);
    }

    pub fn field_u32(&mut self, name: &str, value: u32) {
        self.field(name, TAG_U32);
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn field_i32(&mut self, name: &str, value: i32) {
        self.field(name, TAG_I32);
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn field_i64(&mut self, name: &str, value: i64) {
        self.field(name, TAG_I64);
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn field_u64(&mut self, name: &str, value: u64) {
        self.field(name, TAG_U64);
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn field_usize(&mut self, name: &str, value: usize) {
        self.field_u64(name, u64::try_from(value).unwrap_or(u64::MAX));
    }

    pub fn field_uuid(&mut self, name: &str, value: uuid::Uuid) {
        self.field(name, TAG_UUID);
        self.bytes.extend_from_slice(value.as_bytes());
    }

    pub fn field_bytes(&mut self, name: &str, value: &[u8]) {
        self.field(name, TAG_BYTES);
        self.raw_bytes(value);
    }

    pub fn field_time(&mut self, name: &str, value: time::OffsetDateTime) {
        self.field(name, TAG_TIME);
        self.bytes
            .extend_from_slice(&value.unix_timestamp_nanos().to_be_bytes());
    }

    pub fn field_option_str(&mut self, name: &str, value: Option<&str>) {
        self.field(name, TAG_OPTION_STR);
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.raw_str(value);
            }
            None => self.bytes.push(0),
        }
    }

    pub fn field_option_uuid(&mut self, name: &str, value: Option<uuid::Uuid>) {
        self.field(name, TAG_OPTION_UUID);
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.bytes.extend_from_slice(value.as_bytes());
            }
            None => self.bytes.push(0),
        }
    }

    pub fn field_option_bool(&mut self, name: &str, value: Option<bool>) {
        self.field(name, TAG_OPTION_BOOL);
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.bytes.push(u8::from(value));
            }
            None => self.bytes.push(0),
        }
    }

    pub fn field_option_time(&mut self, name: &str, value: Option<time::OffsetDateTime>) {
        self.field(name, TAG_OPTION_TIME);
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.bytes
                    .extend_from_slice(&value.unix_timestamp_nanos().to_be_bytes());
            }
            None => self.bytes.push(0),
        }
    }

    /// A `u8` that may not have been observed. `None`, `Some(0)` and not
    /// declaring the field at all are three different keys.
    pub fn field_option_u8(&mut self, name: &str, value: Option<u8>) {
        self.field(name, TAG_OPTION_U8);
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.bytes.push(value);
            }
            None => self.bytes.push(0),
        }
    }

    /// A `u32` that may not have been observed. `None`, `Some(0)` and not
    /// declaring the field at all are three different keys.
    pub fn field_option_u32(&mut self, name: &str, value: Option<u32>) {
        self.field(name, TAG_OPTION_U32);
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.bytes.extend_from_slice(&value.to_be_bytes());
            }
            None => self.bytes.push(0),
        }
    }

    /// An `i32` that may not have been observed. `None`, `Some(0)` and not
    /// declaring the field at all are three different keys.
    pub fn field_option_i32(&mut self, name: &str, value: Option<i32>) {
        self.field(name, TAG_OPTION_I32);
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.bytes.extend_from_slice(&value.to_be_bytes());
            }
            None => self.bytes.push(0),
        }
    }

    /// An `i64` that may not have been observed. `None`, `Some(0)` and not
    /// declaring the field at all are three different keys.
    pub fn field_option_i64(&mut self, name: &str, value: Option<i64>) {
        self.field(name, TAG_OPTION_I64);
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.bytes.extend_from_slice(&value.to_be_bytes());
            }
            None => self.bytes.push(0),
        }
    }

    /// A `u64` that may not have been observed. `None`, `Some(0)` and not
    /// declaring the field at all are three different keys.
    pub fn field_option_u64(&mut self, name: &str, value: Option<u64>) {
        self.field(name, TAG_OPTION_U64);
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.bytes.extend_from_slice(&value.to_be_bytes());
            }
            None => self.bytes.push(0),
        }
    }

    /// A `usize` that may not have been observed. Shares
    /// [`field_option_u64`](Self::field_option_u64)'s tag and
    /// [`field_usize`](Self::field_usize)'s saturating conversion, so a
    /// count spells the same whichever width the caller holds it in.
    pub fn field_option_usize(&mut self, name: &str, value: Option<usize>) {
        self.field_option_u64(
            name,
            value.map(|value| u64::try_from(value).unwrap_or(u64::MAX)),
        );
    }

    /// A byte string that may not have been observed. `None`,
    /// `Some(&[])` and not declaring the field at all are three
    /// different keys — which is why an empty slice is not a spelling
    /// for absence.
    pub fn field_option_bytes(&mut self, name: &str, value: Option<&[u8]>) {
        self.field(name, TAG_OPTION_BYTES);
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.raw_bytes(value);
            }
            None => self.bytes.push(0),
        }
    }

    pub fn field_str_list(&mut self, name: &str, values: &[String]) {
        self.field(name, TAG_LIST);
        self.raw_len(values.len());
        for value in values {
            self.raw_str(value);
        }
    }

    pub fn field_uuid_list(&mut self, name: &str, values: &[uuid::Uuid]) {
        self.field(name, TAG_LIST);
        self.raw_len(values.len());
        for value in values {
            self.bytes.extend_from_slice(value.as_bytes());
        }
    }

    pub fn list(&mut self, name: &str, len: usize) {
        self.field(name, TAG_LIST);
        self.raw_len(len);
    }

    fn field(&mut self, name: &str, tag: u8) {
        self.raw_str(name);
        self.bytes.push(tag);
    }

    fn raw_str(&mut self, value: &str) {
        self.raw_bytes(value.as_bytes());
    }

    fn raw_bytes(&mut self, value: &[u8]) {
        self.raw_len(value.len());
        self.bytes.extend_from_slice(value);
    }

    fn raw_len(&mut self, len: usize) {
        self.bytes
            .extend_from_slice(&u64::try_from(len).unwrap_or(u64::MAX).to_be_bytes());
    }
}

#[must_use]
pub fn schema_only_key(schema_id: &str, schema_version: u32) -> Vec<u8> {
    PayloadKeyBuilder::new(schema_id, schema_version).finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchProjectionColumnKind {
    Text,
    TextArray,
    /// Not a sidecar column: the owning memory's rendered text (the
    /// string the memory was embedded from).
    ///
    /// A sidecar usually declares a projection to contribute *retrieval
    /// structure* rather than new content — above all a `tag_column`,
    /// which is the only predicate that can scope a search to a subset
    /// of a corpus. The unscoped branch has no tags to offer
    /// (`push_tag_filter` gets the literal `NULL::text[]` there), so a
    /// tag-filtered query is served by projection branches alone.
    ///
    /// Construct it as [`SearchProjectionField::MEMORY_TEXT`].
    MemoryText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SearchProjectionField {
    pub column: &'static str,
    pub kind: SearchProjectionColumnKind,
}

impl SearchProjectionField {
    /// The owning memory row's `text`. `column` is unused — the value
    /// does not come from the sidecar table.
    ///
    /// A projection of exactly this one field, with no `language_column`,
    /// is the whole reason the kind exists: the branch then projects the
    /// same string as the unscoped search path, so a tag-scoped search
    /// and an unscoped one cannot return different text for the same
    /// memory. Combine it with sidecar fields when the sidecar genuinely
    /// adds searchable content the render does not carry.
    pub const MEMORY_TEXT: Self = Self {
        column: "",
        kind: SearchProjectionColumnKind::MemoryText,
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchProjection {
    pub fields: &'static [SearchProjectionField],
    pub tag_column: Option<String>,
    /// Column holding the row's pre-computed lexical vector, for sidecar
    /// tables whose migration adds one (see `proxima_core.lexical_tsv`).
    /// Declaring it lets search read the stored vector instead of
    /// tokenising the projected text on every candidate row.
    pub tsv_column: Option<&'static str>,
    /// Column holding the row's lexical language (`regconfig`), for
    /// sidecar tables whose migration adds one. Search ranks each
    /// candidate with its own language's tsquery; declared, it reads the
    /// sidecar's column (which may be pinned, as the code flavor pins
    /// `english`), absent it falls back to the owning memory row's
    /// `lexical_language`. A sidecar declaring `tsv_column` over a
    /// per-row-language vector should declare this too, or ranking uses
    /// the wrong configuration for pinned rows.
    pub language_column: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactTombstone {
    pub column: &'static str,
    pub value: &'static str,
}

pub trait FactPayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// GDPR Art. 9 (and analogous regimes') special-category flag.
    /// Defaults to `false`; controllers handling health, biometric,
    /// political, or other heightened-protection categories must
    /// override to `true`. See docs/03 §Special-category declaration
    /// and docs/13 §Compliance vocabulary.
    const SPECIAL_CATEGORY: bool = false;
    /// Whether this schema's rendered text earns a VECTOR. Defaults to
    /// `true`; a schema whose render is a template rather than prose
    /// should override to `false`.
    ///
    /// GATES THE VECTOR ONLY — never the text, and never lexical search.
    /// A non-embeddable Fact still writes [`Self::render`], so it is
    /// still readable and still matched by full-text search. That
    /// distinction is the whole point: a filename
    /// is often the ONLY handle a person has on a file they are looking
    /// for, which is a lexical need, while `"uploaded page-00042.png\n
    /// image/png, 18332 bytes"` has no semantic neighbourhood worth
    /// having. The alternative already available — setting
    /// `rendered_text` to `None` — buys the same saving by making the
    /// Fact unfindable, which is not the same trade.
    ///
    /// The cost this exists to avoid is not primarily money. Tens of
    /// thousands of renders off one template differ only in a filename
    /// and an integer, so their vectors are mutual near-neighbours, and
    /// a dense cluster of them in the index is a retrieval problem
    /// before it is a bill.
    ///
    /// A PROPERTY OF THE SCHEMA, READ FROM THE SCHEMA. It is deliberately
    /// not stamped on the row: flip this declaration and the next
    /// reconcile picks the rows up (or drops them), because the registry
    /// is the single place the answer lives. A per-row copy would freeze
    /// today's decision into history and drift from the type that owns
    /// it.
    const EMBEDDABLE: bool = true;
    /// Schema-owned receipt replay key material. This is not a payload
    /// serialization format; the typed sidecar remains the payload.
    #[must_use]
    fn receipt_key(&self) -> Vec<u8>;
    fn render(&self) -> String;
    /// Node references this payload's fields carry (docs/16
    /// §Reference). Ingest derives one `Reference` index entry per
    /// declaration, in the node write's own transaction, so the edge set
    /// stays a function of node content and re-deriving it from payloads
    /// reproduces it exactly. Default: none.
    #[must_use]
    fn references(&self) -> Vec<PayloadReference> {
        Vec::new()
    }
    /// Per-schema typed Fact sidecar table, or `None` when the Fact
    /// carries no sidecar of its own (its typed payload lives elsewhere,
    /// e.g. in a citation cited-object). Defaults to `None`; mirrors the
    /// optional-sidecar treatment of `CitationMappingPayload`.
    #[must_use]
    fn sidecar_table() -> Option<&'static str> {
        None
    }
    /// Natural-key columns on the sidecar table for stateful Fact
    /// schemas. Default empty = stateless (every observation is a
    /// distinct head). When non-empty, the schema participates in
    /// head-by-natural-key queries (docs/03 §Stateful Fact schemas).
    #[must_use]
    fn natural_key_columns() -> &'static [&'static str] {
        &[]
    }
    /// Optional discriminator for stateful Fact deletion observations.
    /// Storage uses this build-time metadata for `PresentOnly` queries.
    #[must_use]
    fn tombstone() -> Option<FactTombstone> {
        None
    }
    /// Build-time lexical search projection. Only human-meaningful
    /// text columns belong here; raw JSON, code bodies, logs, and
    /// opaque ids stay out of `core/search_memories`.
    #[must_use]
    fn search_projection() -> Option<SearchProjection> {
        None
    }
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}

pub trait AbstractionPayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    fn sidecar_table() -> &'static str;
    /// Node references this payload's fields carry (docs/16
    /// §Reference). Ingest derives one `Reference` index entry per
    /// declaration, in the node write's own transaction, so the edge set
    /// stays a function of node content and re-deriving it from payloads
    /// reproduces it exactly. Default: none.
    #[must_use]
    fn references(&self) -> Vec<PayloadReference> {
        Vec::new()
    }
    #[must_use]
    fn search_projection() -> Option<SearchProjection> {
        None
    }
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}

pub trait PerspectivePayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    fn sidecar_table() -> &'static str;
    /// Node references this payload's fields carry (docs/16
    /// §Reference). Ingest derives one `Reference` index entry per
    /// declaration, in the node write's own transaction, so the edge set
    /// stays a function of node content and re-deriving it from payloads
    /// reproduces it exactly. Default: none.
    #[must_use]
    fn references(&self) -> Vec<PayloadReference> {
        Vec::new()
    }
    #[must_use]
    fn search_projection() -> Option<SearchProjection> {
        None
    }
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}

/// Typed payload for a Goal row in `proxima_core.goal`.
/// Mirrors `FactPayload` / `AbstractionPayload` for the Goal layer.
///
/// See docs/06 §Goal entity and docs/03 §Sidecar tables.
pub trait GoalPayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    /// Schema-owned body key used for `GoalWrite` idempotency conflict
    /// checks. Title/text and authorship are compared separately.
    #[must_use]
    fn goal_key(&self) -> Vec<u8>;
    /// Node references this payload's fields carry (docs/16
    /// §Reference). Ingest derives one `Reference` index entry per
    /// declaration, in the node write's own transaction, so the edge set
    /// stays a function of node content and re-deriving it from payloads
    /// reproduces it exactly. Default: none.
    #[must_use]
    fn references(&self) -> Vec<PayloadReference> {
        Vec::new()
    }
    /// Per-schema typed Goal sidecar table, or `None` when the Goal's
    /// typed payload has no schema-specific storage beyond
    /// `proxima_core.goal` sidecar payload.
    #[must_use]
    fn sidecar_table() -> Option<&'static str> {
        None
    }
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}

/// Typed payload for a cited blob, keyed on `blob_id`. Cited blobs
/// do not participate in F/A/P queries; the artifact body is the
/// blob bytes, while `blob` stores ownership and a content-addressed
/// hash. See docs/11 §"Trait families".
pub trait CitedObjectPayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    fn sidecar_table() -> &'static str;
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }

    /// Stable BLAKE3-32 hash of the artifact content. Re-ingesting
    /// the same artifact for the same Owner deduplicates the
    /// `blob` row via `(owner_id, schema_id, content_hash)`.
    fn idempotency_key(&self) -> [u8; 32];
}

/// Typed locator for a Fact→blob citation. The link is `memory.blob_id`
/// (0..1). A sidecar is **optional**, needed only when the citation
/// carries schema-specific metadata such as byte ranges. A fieldless
/// mapping (a pure link, the common case) returns `None`. See docs/11
/// §"Trait families".
pub trait CitationMappingPayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    /// Sidecar table for this mapping's schema-specific metadata, or
    /// `None` when the mapping is a pure link with no extra columns.
    /// Returning `None` means no sidecar row is written and no table is
    /// required — don't mint an empty table just to satisfy the trait.
    #[must_use]
    fn sidecar_table() -> Option<&'static str> {
        None
    }
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }

    /// Schema id of the `CitedObjectPayload` this mapping is allowed
    /// to annotate.
    fn cited_object_schema() -> SchemaId;
}

#[cfg(test)]
mod tests {
    use super::{PayloadKeyBuilder, TAG_TABLE, schema_only_key};

    const SCHEMA: &str = "test/payload-key";
    const VERSION: u32 = 7;

    fn builder() -> PayloadKeyBuilder {
        PayloadKeyBuilder::new(SCHEMA, VERSION)
    }

    /// The header, spelled by hand rather than by the code under test:
    /// magic, length-prefixed schema id, big-endian schema version. Every
    /// golden below is this plus the frames it declares.
    fn header() -> Vec<u8> {
        let mut bytes = b"PKEY1\0".to_vec();
        bytes.extend_from_slice(&len(SCHEMA.len()));
        bytes.extend_from_slice(SCHEMA.as_bytes());
        bytes.extend_from_slice(&VERSION.to_be_bytes());
        bytes
    }

    /// `u64_be(name.len()) ‖ name ‖ tag` — the frame that opens a field.
    fn frame(bytes: &mut Vec<u8>, name: &str, tag: u8) {
        bytes.extend_from_slice(&len(name.len()));
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(tag);
    }

    fn len(value: usize) -> [u8; 8] {
        u64::try_from(value)
            .expect("test lengths fit u64")
            .to_be_bytes()
    }

    fn uuid(last: u8) -> uuid::Uuid {
        uuid::Uuid::from_bytes([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, last])
    }

    fn timestamp() -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp_nanos(1_712_345_678_901_234_567)
            .expect("timestamp in range")
    }

    #[test]
    fn the_header_is_the_magic_the_schema_id_and_the_schema_version() {
        assert_eq!(schema_only_key(SCHEMA, VERSION), header());
        assert_eq!(builder().finish(), header());
    }

    #[test]
    fn field_str_emits_a_length_prefixed_string_after_tag_s() {
        let mut key = builder();
        key.field_str("title", "Fassade");

        let mut expected = header();
        frame(&mut expected, "title", b's');
        expected.extend_from_slice(&len(7));
        expected.extend_from_slice(b"Fassade");

        assert_eq!(key.finish(), expected);
    }

    #[test]
    fn field_bool_emits_one_byte_after_tag_b() {
        let mut key = builder();
        key.field_bool("required", true);
        key.field_bool("optional", false);

        let mut expected = header();
        frame(&mut expected, "required", b'b');
        expected.push(1);
        frame(&mut expected, "optional", b'b');
        expected.push(0);

        assert_eq!(key.finish(), expected);
    }

    #[test]
    fn field_u8_emits_one_byte_after_tag_one() {
        let mut key = builder();
        key.field_u8("severity", 200);

        let mut expected = header();
        frame(&mut expected, "severity", b'1');
        expected.push(200);

        assert_eq!(key.finish(), expected);
    }

    #[test]
    fn field_u32_emits_four_big_endian_bytes_after_tag_four() {
        let mut key = builder();
        key.field_u32("revision", 0xdead_beef);

        let mut expected = header();
        frame(&mut expected, "revision", b'4');
        expected.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);

        assert_eq!(key.finish(), expected);
    }

    #[test]
    fn field_i32_emits_four_big_endian_bytes_after_tag_capital_i() {
        let mut key = builder();
        key.field_i32("offset", -2);

        let mut expected = header();
        frame(&mut expected, "offset", b'I');
        expected.extend_from_slice(&[0xff, 0xff, 0xff, 0xfe]);

        assert_eq!(key.finish(), expected);
    }

    #[test]
    fn field_i64_emits_eight_big_endian_bytes_after_tag_capital_l() {
        let mut key = builder();
        key.field_i64("delta", -2);

        let mut expected = header();
        frame(&mut expected, "delta", b'L');
        expected.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe]);

        assert_eq!(key.finish(), expected);
    }

    #[test]
    fn field_u64_emits_eight_big_endian_bytes_after_tag_eight() {
        let mut key = builder();
        key.field_u64("byte_len", 0x0102_0304_0506_0708);

        let mut expected = header();
        frame(&mut expected, "byte_len", b'8');
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);

        assert_eq!(key.finish(), expected);
    }

    /// `field_usize` claims no tag of its own — it widens and hands the
    /// value to `field_u64`, so the same count spells the same bytes
    /// whichever width a caller happens to hold it in.
    #[test]
    fn field_usize_emits_exactly_what_field_u64_emits() {
        let mut usize_key = builder();
        usize_key.field_usize("count", 42);
        let mut u64_key = builder();
        u64_key.field_u64("count", 42);

        let mut expected = header();
        frame(&mut expected, "count", b'8');
        expected.extend_from_slice(&len(42));

        assert_eq!(usize_key.finish(), expected);
        assert_eq!(u64_key.finish(), expected);
    }

    /// The widening saturates rather than wrapping, so an unrepresentable
    /// count lands on `u64::MAX` and stays there.
    #[test]
    fn field_usize_saturates_into_field_u64() {
        let mut key = builder();
        key.field_usize("count", usize::MAX);
        let mut saturated = builder();
        saturated.field_u64("count", u64::try_from(usize::MAX).unwrap_or(u64::MAX));

        assert_eq!(key.finish(), saturated.finish());
    }

    #[test]
    fn field_uuid_emits_sixteen_raw_bytes_after_tag_u() {
        let mut key = builder();
        key.field_uuid("memory_id", uuid(16));

        let mut expected = header();
        frame(&mut expected, "memory_id", b'u');
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);

        assert_eq!(key.finish(), expected);
    }

    #[test]
    fn field_bytes_emits_a_length_prefixed_blob_after_tag_capital_b() {
        let mut key = builder();
        key.field_bytes("digest", &[0xaa, 0xbb, 0xcc]);

        let mut expected = header();
        frame(&mut expected, "digest", b'B');
        expected.extend_from_slice(&len(3));
        expected.extend_from_slice(&[0xaa, 0xbb, 0xcc]);

        assert_eq!(key.finish(), expected);
    }

    #[test]
    fn field_time_emits_unix_nanos_as_sixteen_big_endian_bytes_after_tag_t() {
        let mut key = builder();
        key.field_time("occurred_at", timestamp());

        let mut expected = header();
        frame(&mut expected, "occurred_at", b't');
        expected.extend_from_slice(&1_712_345_678_901_234_567_i128.to_be_bytes());

        assert_eq!(key.finish(), expected);
    }

    #[test]
    fn field_option_str_writes_a_presence_byte_before_the_string() {
        let mut some = builder();
        some.field_option_str("note", Some("ja"));
        let mut expected_some = header();
        frame(&mut expected_some, "note", b'S');
        expected_some.push(1);
        expected_some.extend_from_slice(&len(2));
        expected_some.extend_from_slice(b"ja");
        assert_eq!(some.finish(), expected_some);

        let mut none = builder();
        none.field_option_str("note", None);
        let mut expected_none = header();
        frame(&mut expected_none, "note", b'S');
        expected_none.push(0);
        assert_eq!(none.finish(), expected_none);
    }

    #[test]
    fn field_option_uuid_writes_a_presence_byte_before_the_sixteen_bytes() {
        let mut some = builder();
        some.field_option_uuid("parent_id", Some(uuid(16)));
        let mut expected_some = header();
        frame(&mut expected_some, "parent_id", b'U');
        expected_some.push(1);
        expected_some.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        assert_eq!(some.finish(), expected_some);

        let mut none = builder();
        none.field_option_uuid("parent_id", None);
        let mut expected_none = header();
        frame(&mut expected_none, "parent_id", b'U');
        expected_none.push(0);
        assert_eq!(none.finish(), expected_none);
    }

    #[test]
    fn field_option_bool_writes_a_presence_byte_before_the_flag() {
        let mut some = builder();
        some.field_option_bool("approved", Some(false));
        let mut expected_some = header();
        frame(&mut expected_some, "approved", b'O');
        expected_some.push(1);
        expected_some.push(0);
        assert_eq!(some.finish(), expected_some);

        let mut none = builder();
        none.field_option_bool("approved", None);
        let mut expected_none = header();
        frame(&mut expected_none, "approved", b'O');
        expected_none.push(0);
        assert_eq!(none.finish(), expected_none);
    }

    #[test]
    fn field_option_time_writes_a_presence_byte_before_the_nanos() {
        let mut some = builder();
        some.field_option_time("closed_at", Some(timestamp()));
        let mut expected_some = header();
        frame(&mut expected_some, "closed_at", b'T');
        expected_some.push(1);
        expected_some.extend_from_slice(&1_712_345_678_901_234_567_i128.to_be_bytes());
        assert_eq!(some.finish(), expected_some);

        let mut none = builder();
        none.field_option_time("closed_at", None);
        let mut expected_none = header();
        frame(&mut expected_none, "closed_at", b'T');
        expected_none.push(0);
        assert_eq!(none.finish(), expected_none);
    }

    #[test]
    fn field_option_u8_writes_a_presence_byte_before_the_byte() {
        let mut some = builder();
        some.field_option_u8("severity", Some(200));
        let mut expected_some = header();
        frame(&mut expected_some, "severity", b'2');
        expected_some.push(1);
        expected_some.push(200);
        assert_eq!(some.finish(), expected_some);

        let mut none = builder();
        none.field_option_u8("severity", None);
        let mut expected_none = header();
        frame(&mut expected_none, "severity", b'2');
        expected_none.push(0);
        assert_eq!(none.finish(), expected_none);
    }

    #[test]
    fn field_option_u32_writes_a_presence_byte_before_the_four_bytes() {
        let mut some = builder();
        some.field_option_u32("revision", Some(0xdead_beef));
        let mut expected_some = header();
        frame(&mut expected_some, "revision", b'5');
        expected_some.push(1);
        expected_some.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(some.finish(), expected_some);

        let mut none = builder();
        none.field_option_u32("revision", None);
        let mut expected_none = header();
        frame(&mut expected_none, "revision", b'5');
        expected_none.push(0);
        assert_eq!(none.finish(), expected_none);
    }

    #[test]
    fn field_option_i32_writes_a_presence_byte_before_the_four_bytes() {
        let mut some = builder();
        some.field_option_i32("offset", Some(-2));
        let mut expected_some = header();
        frame(&mut expected_some, "offset", b'i');
        expected_some.push(1);
        expected_some.extend_from_slice(&[0xff, 0xff, 0xff, 0xfe]);
        assert_eq!(some.finish(), expected_some);

        let mut none = builder();
        none.field_option_i32("offset", None);
        let mut expected_none = header();
        frame(&mut expected_none, "offset", b'i');
        expected_none.push(0);
        assert_eq!(none.finish(), expected_none);
    }

    #[test]
    fn field_option_i64_writes_a_presence_byte_before_the_eight_bytes() {
        let mut some = builder();
        some.field_option_i64("delta", Some(-2));
        let mut expected_some = header();
        frame(&mut expected_some, "delta", b'l');
        expected_some.push(1);
        expected_some.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe]);
        assert_eq!(some.finish(), expected_some);

        let mut none = builder();
        none.field_option_i64("delta", None);
        let mut expected_none = header();
        frame(&mut expected_none, "delta", b'l');
        expected_none.push(0);
        assert_eq!(none.finish(), expected_none);
    }

    #[test]
    fn field_option_u64_writes_a_presence_byte_before_the_eight_bytes() {
        let mut some = builder();
        some.field_option_u64("byte_len", Some(0x0102_0304_0506_0708));
        let mut expected_some = header();
        frame(&mut expected_some, "byte_len", b'9');
        expected_some.push(1);
        expected_some.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(some.finish(), expected_some);

        let mut none = builder();
        none.field_option_u64("byte_len", None);
        let mut expected_none = header();
        frame(&mut expected_none, "byte_len", b'9');
        expected_none.push(0);
        assert_eq!(none.finish(), expected_none);
    }

    /// Mirrors `field_usize`: no tag of its own, the same saturating
    /// widening, and therefore the same bytes as the `u64` spelling.
    #[test]
    fn field_option_usize_emits_exactly_what_field_option_u64_emits() {
        let mut some = builder();
        some.field_option_usize("count", Some(42));
        let mut expected_some = header();
        frame(&mut expected_some, "count", b'9');
        expected_some.push(1);
        expected_some.extend_from_slice(&len(42));
        assert_eq!(some.finish(), expected_some);

        let mut none = builder();
        none.field_option_usize("count", None);
        let mut expected_none = header();
        frame(&mut expected_none, "count", b'9');
        expected_none.push(0);
        assert_eq!(none.finish(), expected_none);

        let mut saturated = builder();
        saturated.field_option_usize("count", Some(usize::MAX));
        let mut widened = builder();
        widened.field_option_u64("count", Some(u64::try_from(usize::MAX).unwrap_or(u64::MAX)));
        assert_eq!(saturated.finish(), widened.finish());
    }

    #[test]
    fn field_option_bytes_writes_a_presence_byte_before_the_blob() {
        let mut some = builder();
        some.field_option_bytes("digest", Some(&[0xaa, 0xbb, 0xcc]));
        let mut expected_some = header();
        frame(&mut expected_some, "digest", b'C');
        expected_some.push(1);
        expected_some.extend_from_slice(&len(3));
        expected_some.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
        assert_eq!(some.finish(), expected_some);

        let mut none = builder();
        none.field_option_bytes("digest", None);
        let mut expected_none = header();
        frame(&mut expected_none, "digest", b'C');
        expected_none.push(0);
        assert_eq!(none.finish(), expected_none);
    }

    #[test]
    fn field_str_list_emits_a_count_then_every_string() {
        let mut key = builder();
        key.field_str_list("tags", &["a".to_string(), "bc".to_string()]);

        let mut expected = header();
        frame(&mut expected, "tags", b'[');
        expected.extend_from_slice(&len(2));
        expected.extend_from_slice(&len(1));
        expected.extend_from_slice(b"a");
        expected.extend_from_slice(&len(2));
        expected.extend_from_slice(b"bc");

        assert_eq!(key.finish(), expected);
    }

    #[test]
    fn field_uuid_list_emits_a_count_then_every_uuid() {
        let mut key = builder();
        key.field_uuid_list("chunk_ids", &[uuid(16), uuid(17)]);

        let mut expected = header();
        frame(&mut expected, "chunk_ids", b'[');
        expected.extend_from_slice(&len(2));
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        expected.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 17]);

        assert_eq!(key.finish(), expected);
    }

    /// `list` opens a count and lets the caller frame each element's
    /// fields itself — the shape a `Vec` of structs takes, as in the code
    /// flavor's `acceptance-criteria-v1`.
    #[test]
    fn list_emits_a_count_that_the_fields_after_it_belong_to() {
        let mut key = builder();
        key.list("criteria", 2);
        key.field_str("criterion.key", "a");
        key.field_bool("criterion.required", true);
        key.field_str("criterion.key", "b");
        key.field_bool("criterion.required", false);

        let mut expected = header();
        frame(&mut expected, "criteria", b'[');
        expected.extend_from_slice(&len(2));
        frame(&mut expected, "criterion.key", b's');
        expected.extend_from_slice(&len(1));
        expected.extend_from_slice(b"a");
        frame(&mut expected, "criterion.required", b'b');
        expected.push(1);
        frame(&mut expected, "criterion.key", b's');
        expected.extend_from_slice(&len(1));
        expected.extend_from_slice(b"b");
        frame(&mut expected, "criterion.required", b'b');
        expected.push(0);

        assert_eq!(key.finish(), expected);
    }

    fn triple(
        declare_none: impl FnOnce(&mut PayloadKeyBuilder),
        declare_some: impl FnOnce(&mut PayloadKeyBuilder),
    ) -> [Vec<u8>; 3] {
        let mut none = builder();
        declare_none(&mut none);
        let mut some = builder();
        declare_some(&mut some);
        [builder().finish(), none.finish(), some.finish()]
    }

    /// Not declaring a field, declaring it `None`, and declaring it
    /// `Some(zero)` are three different statements about the world, so
    /// they are three different identities. This is the property every
    /// hand-rolled absence encoding loses, for every option kind at once.
    #[test]
    fn absent_none_and_some_default_are_three_different_keys() {
        // `OffsetDateTime` has no `Default`; the epoch is its zero.
        let cases: Vec<(&str, [Vec<u8>; 3])> = vec![
            (
                "field_option_str",
                triple(
                    |key| key.field_option_str("f", None),
                    |key| key.field_option_str("f", Some(<&str>::default())),
                ),
            ),
            (
                "field_option_uuid",
                triple(
                    |key| key.field_option_uuid("f", None),
                    |key| key.field_option_uuid("f", Some(uuid::Uuid::default())),
                ),
            ),
            (
                "field_option_bool",
                triple(
                    |key| key.field_option_bool("f", None),
                    |key| key.field_option_bool("f", Some(bool::default())),
                ),
            ),
            (
                "field_option_time",
                triple(
                    |key| key.field_option_time("f", None),
                    |key| key.field_option_time("f", Some(time::OffsetDateTime::UNIX_EPOCH)),
                ),
            ),
            (
                "field_option_u8",
                triple(
                    |key| key.field_option_u8("f", None),
                    |key| key.field_option_u8("f", Some(u8::default())),
                ),
            ),
            (
                "field_option_u32",
                triple(
                    |key| key.field_option_u32("f", None),
                    |key| key.field_option_u32("f", Some(u32::default())),
                ),
            ),
            (
                "field_option_i32",
                triple(
                    |key| key.field_option_i32("f", None),
                    |key| key.field_option_i32("f", Some(i32::default())),
                ),
            ),
            (
                "field_option_i64",
                triple(
                    |key| key.field_option_i64("f", None),
                    |key| key.field_option_i64("f", Some(i64::default())),
                ),
            ),
            (
                "field_option_u64",
                triple(
                    |key| key.field_option_u64("f", None),
                    |key| key.field_option_u64("f", Some(u64::default())),
                ),
            ),
            (
                "field_option_usize",
                triple(
                    |key| key.field_option_usize("f", None),
                    |key| key.field_option_usize("f", Some(usize::default())),
                ),
            ),
            (
                "field_option_bytes",
                triple(
                    |key| key.field_option_bytes("f", None),
                    |key| key.field_option_bytes("f", Some(<&[u8]>::default())),
                ),
            ),
        ];

        for (kind, [absent, none, some]) in cases {
            assert_ne!(absent, none, "{kind}: not declared vs declared None");
            assert_ne!(absent, some, "{kind}: not declared vs declared Some(zero)");
            assert_ne!(none, some, "{kind}: declared None vs declared Some(zero)");
        }
    }

    /// Two kinds sharing a tag byte is the one way this encoding can lose
    /// identity without anyone editing a method body.
    #[test]
    fn every_tag_byte_is_distinct() {
        for (index, (left_kind, left_tag)) in TAG_TABLE.iter().enumerate() {
            for (right_kind, right_tag) in TAG_TABLE.iter().skip(index + 1) {
                assert_ne!(
                    left_tag, right_tag,
                    "{left_kind} and {right_kind} claim the same tag byte"
                );
            }
        }
    }

    /// A new spelling that claims a byte without joining the table would
    /// be unfenced by the test above, so the table's membership is pinned
    /// too.
    #[test]
    fn the_tag_table_names_every_kind_that_claims_a_tag() {
        let mut kinds: Vec<&str> = TAG_TABLE.iter().map(|(kind, _)| *kind).collect();
        kinds.sort_unstable();

        assert_eq!(
            kinds,
            [
                "field_bool",
                "field_bytes",
                "field_i32",
                "field_i64",
                "field_option_bool",
                "field_option_bytes",
                "field_option_i32",
                "field_option_i64",
                "field_option_str",
                "field_option_time",
                "field_option_u32",
                "field_option_u64",
                "field_option_u8",
                "field_option_uuid",
                "field_str",
                "field_time",
                "field_u32",
                "field_u64",
                "field_u8",
                "field_uuid",
                "list",
            ]
        );
    }

    /// The whole point, at the builder level: same declared values, same
    /// bytes — so the receipt id folds to the same Fact and the write
    /// replays instead of minting.
    #[test]
    fn the_same_declared_values_produce_the_same_key() {
        let build = || {
            let mut key = builder();
            key.field_str("stable_id", "same");
            key.field_option_u32("revision", Some(3));
            key.field_option_bytes("digest", None);
            key.field_option_time("closed_at", Some(timestamp()));
            key.finish()
        };

        assert_eq!(build(), build());
    }

    #[test]
    fn one_changed_declared_value_produces_a_different_key() {
        let build = |revision| {
            let mut key = builder();
            key.field_str("stable_id", "same");
            key.field_option_u32("revision", Some(revision));
            key.finish()
        };

        assert_ne!(build(3), build(4));
    }
}
