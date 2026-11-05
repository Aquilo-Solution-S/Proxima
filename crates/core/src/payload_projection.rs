//! Typed semantic projections for registered payloads.
//!
//! Protocol JSON is an I/O representation.  This module carries the
//! projections that domain and storage code need after protocol decoding.

use std::any::TypeId;

use serde::Serialize;
use uuid::Uuid;

use crate::canonical_json_bytes;
use crate::verbs::query::SidecarAtom;

/// The display values a payload explicitly offers to read projections.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PayloadDisplay {
    /// A short title, when the schema has one.
    pub title: Option<String>,
    /// The body used by search and memory reads.
    pub body: Option<String>,
    /// The short fallback line used by sketches.
    pub sketch: Option<String>,
    /// Declared filterable tags.
    pub tags: Vec<String>,
}

/// One schema-owned natural-key field.
///
/// The field name and getter are private together: callers can use a field
/// only through the constructor matching its value type.
#[derive(Clone, Copy)]
pub struct KeyField<P> {
    name: &'static str,
    getter: KeyGetter<P>,
}

/// Closed scalar vocabulary accepted by natural-key storage ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyScalarKind {
    /// A UUID column.
    Uuid,
    /// A text column.
    Text,
    /// A `PostgreSQL` `CHAR(n)` column, canonicalized by removing padding.
    PaddedText,
    /// A 16-bit signed integer column.
    I16,
    /// A 32-bit signed integer column.
    I32,
    /// A 64-bit signed integer column.
    I64,
    /// A boolean column.
    Bool,
}

impl<P> std::fmt::Debug for KeyField<P> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyField")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy)]
enum KeyGetter<P> {
    Uuid(fn(&P) -> Uuid),
    Text(for<'a> fn(&'a P) -> &'a str),
    PaddedText(for<'a> fn(&'a P) -> &'a str),
    I16(fn(&P) -> i16),
    I32(fn(&P) -> i32),
    I64(fn(&P) -> i64),
    Bool(fn(&P) -> bool),
}

impl<P> KeyField<P> {
    /// Declare a UUID natural-key field.
    #[must_use]
    pub const fn uuid(name: &'static str, getter: fn(&P) -> Uuid) -> Self {
        Self {
            name,
            getter: KeyGetter::Uuid(getter),
        }
    }

    /// Declare a text natural-key field.
    ///
    /// ```
    /// # use proxiima_core::KeyField;
    /// struct Probe { name: String }
    /// let _: KeyField<Probe> = KeyField::text("name", |probe: &Probe| &probe.name);
    /// ```
    ///
    /// ```compile_fail
    /// # use proxiima_core::KeyField;
    /// struct Probe { id: uuid::Uuid }
    /// let _: KeyField<Probe> = KeyField::text("id", |probe: &Probe| probe.id);
    /// ```
    #[must_use]
    pub const fn text(name: &'static str, getter: for<'a> fn(&'a P) -> &'a str) -> Self {
        Self {
            name,
            getter: KeyGetter::Text(getter),
        }
    }

    /// Declare a fixed-width `PostgreSQL` `CHAR(n)` natural-key field.
    #[must_use]
    pub const fn padded_text(name: &'static str, getter: for<'a> fn(&'a P) -> &'a str) -> Self {
        Self {
            name,
            getter: KeyGetter::PaddedText(getter),
        }
    }

    /// Declare a signed 16-bit natural-key field.
    #[must_use]
    pub const fn i16(name: &'static str, getter: fn(&P) -> i16) -> Self {
        Self {
            name,
            getter: KeyGetter::I16(getter),
        }
    }

    /// Declare a signed 32-bit natural-key field.
    #[must_use]
    pub const fn i32(name: &'static str, getter: fn(&P) -> i32) -> Self {
        Self {
            name,
            getter: KeyGetter::I32(getter),
        }
    }

    /// Declare a signed 64-bit natural-key field.
    #[must_use]
    pub const fn i64(name: &'static str, getter: fn(&P) -> i64) -> Self {
        Self {
            name,
            getter: KeyGetter::I64(getter),
        }
    }

    /// Declare a boolean natural-key field.
    #[must_use]
    pub const fn bool(name: &'static str, getter: fn(&P) -> bool) -> Self {
        Self {
            name,
            getter: KeyGetter::Bool(getter),
        }
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The closed scalar kind selected by the constructor.
    #[must_use]
    pub const fn kind(&self) -> KeyScalarKind {
        match self.getter {
            KeyGetter::Uuid(_) => KeyScalarKind::Uuid,
            KeyGetter::Text(_) => KeyScalarKind::Text,
            KeyGetter::PaddedText(_) => KeyScalarKind::PaddedText,
            KeyGetter::I16(_) => KeyScalarKind::I16,
            KeyGetter::I32(_) => KeyScalarKind::I32,
            KeyGetter::I64(_) => KeyScalarKind::I64,
            KeyGetter::Bool(_) => KeyScalarKind::Bool,
        }
    }

    fn atom(&self, payload: &P) -> SidecarAtom {
        match self.getter {
            KeyGetter::Uuid(getter) => SidecarAtom::Uuid(getter(payload)),
            KeyGetter::Text(getter) => SidecarAtom::Text(getter(payload).to_owned()),
            KeyGetter::PaddedText(getter) => {
                SidecarAtom::Text(getter(payload).trim_end_matches(' ').to_owned())
            }
            KeyGetter::I16(getter) => SidecarAtom::I32(i32::from(getter(payload))),
            KeyGetter::I32(getter) => SidecarAtom::I32(getter(payload)),
            KeyGetter::I64(getter) => SidecarAtom::I64(getter(payload)),
            KeyGetter::Bool(getter) => SidecarAtom::Bool(getter(payload)),
        }
    }
}

/// A typed payload projection used by domain and storage code.
pub trait PayloadProjection: Send + Sync + 'static + Sized {
    /// Natural-key fields, in SQL and contract order.
    const KEY_FIELDS: &'static [KeyField<Self>];

    /// Typed display values.  No protocol serialization is involved.
    fn display(&self) -> PayloadDisplay;

    /// Stable, schema-owned durable representation.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload cannot be encoded into its durable representation.
    fn durable(&self) -> Result<DurablePayload, String>;
}

fn empty_key_columns() -> Vec<&'static str> {
    Vec::new()
}

fn empty_key_kinds() -> Vec<KeyScalarKind> {
    Vec::new()
}
fn projection_key_kinds<P: PayloadProjection>() -> Vec<KeyScalarKind> {
    P::KEY_FIELDS.iter().map(KeyField::kind).collect()
}

fn projection_key_columns<P: PayloadProjection>() -> Vec<&'static str> {
    P::KEY_FIELDS.iter().map(KeyField::name).collect()
}

fn capture_registered_key<P: crate::FactPayload>(
    payload: &crate::SidecarPayload,
) -> Option<NaturalKeySnapshot> {
    payload
        .downcast_ref::<P>()
        .and_then(NaturalKeySnapshot::capture)
}

/// Schema-owned natural-key metadata derived from a typed projection.
#[derive(Clone, Copy)]
pub struct NaturalKeyDeclaration {
    columns: fn() -> Vec<&'static str>,
    kinds: fn() -> Vec<KeyScalarKind>,
    schema_id: Option<fn() -> &'static str>,
    payload_type: Option<TypeId>,
    capture: Option<fn(&crate::SidecarPayload) -> Option<NaturalKeySnapshot>>,
}

impl std::fmt::Debug for NaturalKeyDeclaration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entries(self.column_names()).finish()
    }
}

impl NaturalKeyDeclaration {
    /// The stateless declaration.
    pub const EMPTY: Self = Self {
        columns: empty_key_columns,
        kinds: empty_key_kinds,
        schema_id: None,
        payload_type: None,
        capture: None,
    };

    /// Derive metadata from a typed payload projection.
    #[must_use]
    pub const fn of<P: crate::FactPayload + PayloadProjection>() -> Self {
        if P::KEY_FIELDS.is_empty() {
            return Self::EMPTY;
        }
        Self {
            columns: projection_key_columns::<P>,
            kinds: projection_key_kinds::<P>,
            schema_id: Some(payload_schema_id::<P>),
            payload_type: Some(TypeId::of::<P>()),
            capture: Some(capture_registered_key::<P>),
        }
    }

    pub(crate) fn capture(self, payload: &crate::SidecarPayload) -> Option<NaturalKeySnapshot> {
        self.capture.and_then(|capture| capture(payload))
    }

    #[must_use]
    pub fn column_names(self) -> Vec<&'static str> {
        (self.columns)()
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.column_names().is_empty()
    }

    /// Scalar meanings of the declared columns, in key order.
    #[must_use]
    pub fn scalar_kinds(self) -> Vec<KeyScalarKind> {
        (self.kinds)()
    }

    /// The schema id owned by this declaration, if stateful.
    #[must_use]
    pub fn schema_id(self) -> Option<&'static str> {
        self.schema_id.map(|getter| getter())
    }
}

fn payload_schema_id<P: crate::FactPayload>() -> &'static str {
    P::SCHEMA_ID
}

impl PartialEq for NaturalKeyDeclaration {
    fn eq(&self, other: &Self) -> bool {
        self.column_names() == other.column_names()
            && self.scalar_kinds() == other.scalar_kinds()
            && self.schema_id() == other.schema_id()
            && self.payload_type == other.payload_type
    }
}

impl Eq for NaturalKeyDeclaration {}

/// A schema-bound natural-key snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NaturalKeySnapshot {
    payload_type: TypeId,
    columns: Vec<(String, SidecarAtom)>,
}

impl NaturalKeySnapshot {
    /// Capture a payload's declared key fields.
    pub fn capture<P: PayloadProjection>(payload: &P) -> Option<Self> {
        if P::KEY_FIELDS.is_empty() {
            return None;
        }
        let columns = P::KEY_FIELDS
            .iter()
            .map(|field| (field.name().to_owned(), field.atom(payload)))
            .collect();
        Some(Self {
            payload_type: TypeId::of::<P>(),
            columns,
        })
    }

    /// The declared fields and their typed values.
    #[must_use]
    pub fn columns(&self) -> &[(String, SidecarAtom)] {
        &self.columns
    }

    /// The concrete payload type captured by this snapshot.
    #[must_use]
    pub fn payload_type(&self) -> TypeId {
        self.payload_type
    }

    /// The declared database column names.
    #[must_use]
    pub fn column_names<P: PayloadProjection>() -> Vec<&'static str> {
        P::KEY_FIELDS.iter().map(KeyField::name).collect()
    }
}

/// A durable, explicitly constructed payload codec.
#[derive(Debug, Clone, PartialEq)]
pub struct DurablePayload(serde_json::Value);

impl DurablePayload {
    /// Canonical bytes for durable hashing and comparison.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_json_bytes(&self.0)
    }
}

impl Serialize for DurablePayload {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

/// Builds a durable object without exposing an untyped object to consumers.
#[derive(Debug, Default)]
pub struct DurablePayloadBuilder {
    fields: serde_json::Map<String, serde_json::Value>,
}

impl DurablePayloadBuilder {
    /// Start an empty schema-owned durable object.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one explicit durable field.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization fails or when `name` was already added.
    pub fn field<T: Serialize>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<&mut Self, String> {
        let value = serde_json::to_value(value).map_err(|error| error.to_string())?;
        if self.fields.insert(name.to_owned(), value).is_some() {
            return Err(format!("duplicate durable field {name}"));
        }
        Ok(self)
    }

    /// Finish the durable object.
    #[must_use]
    pub fn finish(self) -> DurablePayload {
        DurablePayload(serde_json::Value::Object(self.fields))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Debug, Serialize)]
    struct Probe {
        #[serde(rename = "wire_key")]
        key: String,
        companion: Uuid,
    }

    fn key(probe: &Probe) -> &str {
        &probe.key
    }
    fn companion(probe: &Probe) -> Uuid {
        probe.companion
    }

    impl PayloadProjection for Probe {
        const KEY_FIELDS: &'static [KeyField<Self>] = &[
            KeyField::text("key", key),
            KeyField::uuid("companion", companion),
        ];

        fn display(&self) -> PayloadDisplay {
            PayloadDisplay {
                body: Some(self.key.clone()),
                ..PayloadDisplay::default()
            }
        }

        fn durable(&self) -> Result<DurablePayload, String> {
            let mut builder = DurablePayloadBuilder::new();
            builder
                .field("key", &self.key)?
                .field("companion", &self.companion)?;
            Ok(builder.finish())
        }
    }

    #[derive(Debug)]
    struct PaddedProbe {
        key: String,
    }

    fn padded_key(probe: &PaddedProbe) -> &str {
        &probe.key
    }

    impl PayloadProjection for PaddedProbe {
        const KEY_FIELDS: &'static [KeyField<Self>] = &[KeyField::padded_text("key", padded_key)];

        fn display(&self) -> PayloadDisplay {
            PayloadDisplay::default()
        }

        fn durable(&self) -> Result<DurablePayload, String> {
            let mut fields = DurablePayloadBuilder::new();
            fields.field("key", &self.key)?;
            Ok(fields.finish())
        }
    }

    #[test]
    fn typed_key_ignores_protocol_field_name() {
        let companion = Uuid::nil();
        let probe = Probe {
            key: companion.to_string(),
            companion,
        };
        let snapshot = NaturalKeySnapshot::capture(&probe).expect("key");
        assert_eq!(
            snapshot.columns()[0],
            ("key".to_owned(), SidecarAtom::Text(probe.key.clone()))
        );
        assert_eq!(
            snapshot.columns()[1],
            ("companion".to_owned(), SidecarAtom::Uuid(companion))
        );
    }

    #[test]
    fn durable_projection_is_explicit_and_stable() {
        let companion = Uuid::nil();
        let probe = Probe {
            key: "x".into(),
            companion,
        };
        let durable = probe.durable().expect("durable");
        assert!(
            durable
                .canonical_bytes()
                .windows(b"wire_key".len())
                .all(|window| window != b"wire_key")
        );
        assert!(
            durable
                .canonical_bytes()
                .windows(b"key".len())
                .any(|window| window == b"key")
        );
    }

    #[test]
    fn duplicate_durable_fields_are_refused() {
        let mut builder = DurablePayloadBuilder::new();
        builder.field("x", &1).expect("first");
        assert!(builder.field("x", &2).is_err());
    }

    #[test]
    fn padded_text_canonicalizes_database_padding_but_text_does_not() {
        let padded = NaturalKeySnapshot::capture(&PaddedProbe { key: "x ".into() }).expect("key");
        let padded_again =
            NaturalKeySnapshot::capture(&PaddedProbe { key: "x".into() }).expect("key");
        assert_eq!(padded, padded_again);

        let exact = NaturalKeySnapshot::capture(&Probe {
            key: "x ".into(),
            companion: Uuid::nil(),
        })
        .expect("key");
        let exact_again = NaturalKeySnapshot::capture(&Probe {
            key: "x".into(),
            companion: Uuid::nil(),
        })
        .expect("key");
        assert_ne!(exact, exact_again);
    }
}
