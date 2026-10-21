//! Backend-neutral read DTOs shared by engine and storage ports.

use crate::{
    EntityKind, GoalId, MemoryId, OwnerId, SchemaId, SchemaVersion, SidecarPayload, StorageError,
    WriterAttribution,
};

/// Persisted recall/think one-liner. Plumbing; not a kernel sort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySketch {
    pub id: MemoryId,
    pub owner: OwnerId,
    pub kind: EntityKind,
    pub text: String,
}

/// How many current memories a read set holds, by kind: one per series head
/// for the three memory kinds, one per Goal in any state. A stateful series
/// counts once however many versions it has. Serialized as the body of
/// `proxima://memories/counts`; every kind is always present, zero when the
/// read set holds none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct MemoryCounts {
    #[serde(rename = "Fact")]
    pub fact: u64,
    #[serde(rename = "Abstraction")]
    pub abstraction: u64,
    #[serde(rename = "Perspective")]
    pub perspective: u64,
    #[serde(rename = "Goal")]
    pub goal: u64,
}

impl MemoryCounts {
    /// The count of `kind`. Exhaustive over [`EntityKind`]: a new kind is a
    /// compile error here, never a row that goes uncounted.
    pub fn slot(&mut self, kind: EntityKind) -> &mut u64 {
        match kind {
            EntityKind::Fact => &mut self.fact,
            EntityKind::Abstraction => &mut self.abstraction,
            EntityKind::Perspective => &mut self.perspective,
            EntityKind::Goal => &mut self.goal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub memory_id: MemoryId,
    /// The series this version belongs to; the same on every version of it.
    pub series: crate::SeriesHandle,
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub owner: OwnerId,
    pub writer: WriterAttribution,
    /// When the observation occurred (`memory.occurred_at`): `Some` for
    /// every Fact, `None` for an Abstraction or Perspective.
    pub occurred_at: Option<time::OffsetDateTime>,
    /// The display body, derived by [`MemorySchemaSpec::display_body`]: the
    /// same value a search hit reports as `body`.
    pub text: Option<String>,
    /// The display tags, derived by [`MemorySchemaSpec::display_tags`]: the
    /// same value a search hit reports as `tags`.
    pub tags: Vec<String>,
    pub payload: Option<SidecarPayload>,
}

#[derive(Debug, Clone)]
pub struct MemorySchemaSpec {
    pub kind: EntityKind,
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub sidecar_table: Option<String>,
    /// Payload field a memory of this schema displays as its body: the first
    /// declared `Text` search field that is not `title`. `None` when the
    /// schema declares no search projection.
    pub body_field: Option<String>,
    /// Payload field a memory of this schema displays as its tags: the
    /// declared `tag_column`, the field search filters tags on.
    pub tag_field: Option<String>,
}

impl MemorySchemaSpec {
    /// A spec with no declared display fields; reads fall back to the
    /// legacy payload keys. See [`Self::with_display_fields`].
    #[must_use]
    pub const fn new(
        kind: EntityKind,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        sidecar_table: Option<String>,
    ) -> Self {
        Self {
            kind,
            schema_id,
            schema_version,
            sidecar_table,
            body_field: None,
            tag_field: None,
        }
    }

    /// Set the display fields from a schema's search declaration.
    #[must_use]
    pub fn with_display_fields(
        mut self,
        body_field: Option<String>,
        tag_field: Option<String>,
    ) -> Self {
        self.body_field = body_field;
        self.tag_field = tag_field;
        self
    }

    /// The one body rule, shared by search hits and by-id reads: the payload
    /// string at [`Self::body_field`]; otherwise the legacy payload `body`,
    /// then `text` keys (schemas that declare no projection); otherwise
    /// `fallback` (text the read path already holds, such as a sketch).
    #[must_use]
    pub fn display_body(
        &self,
        payload: Option<&SidecarPayload>,
        fallback: Option<&str>,
    ) -> Option<String> {
        let json = payload.and_then(|payload| payload.to_protocol_json().ok());
        let field = |key: &str| {
            json.as_ref()
                .and_then(|value| value.get(key)?.as_str().map(ToOwned::to_owned))
        };
        self.body_field
            .as_deref()
            .and_then(field)
            .or_else(|| field("body"))
            .or_else(|| field("text"))
            .or_else(|| fallback.map(ToOwned::to_owned))
    }

    /// The one tags rule, shared by search hits and by-id reads: the payload
    /// string array at [`Self::tag_field`], so a memory reports exactly the
    /// tags search filters it on. A schema that declares no `tag_column`
    /// keeps the legacy payload `tags` key, so its tags are not dropped.
    #[must_use]
    pub fn display_tags(&self, payload: Option<&SidecarPayload>) -> Vec<String> {
        let key = self.tag_field.as_deref().unwrap_or("tags");
        payload
            .and_then(|payload| payload.to_protocol_json().ok())
            .and_then(|value| {
                value.get(key)?.as_array().map(|tags| {
                    tags.iter()
                        .filter_map(|tag| tag.as_str().map(ToOwned::to_owned))
                        .collect()
                })
            })
            .unwrap_or_default()
    }
}

/// Resolve the single registered declaration for a persisted Memory selector.
///
/// Memory rows store `(kind, schema_id)` but no version. A missing or
/// ambiguous declaration is a registry/storage invariant failure, never a
/// reason to invent a version or omit a visible row.
///
/// # Errors
///
/// Returns [`StorageError::ConstraintViolation`] when zero or multiple
/// declarations match the stored selector.
pub fn resolve_memory_schema<'a>(
    specs: &'a [MemorySchemaSpec],
    kind: EntityKind,
    schema_id: &SchemaId,
) -> Result<&'a MemorySchemaSpec, StorageError> {
    let mut matches = specs
        .iter()
        .filter(|spec| spec.kind == kind && spec.schema_id == *schema_id);
    let Some(first) = matches.next() else {
        return Err(StorageError::ConstraintViolation(format!(
            "no registered Memory schema for {kind:?} {}",
            schema_id.as_str()
        )));
    };
    if matches.next().is_some() {
        return Err(StorageError::ConstraintViolation(format!(
            "ambiguous registered Memory schema for {kind:?} {}",
            schema_id.as_str()
        )));
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::{MemoryCounts, MemorySchemaSpec, resolve_memory_schema};
    use crate::{EntityKind, SchemaId, SchemaVersion, StorageError};

    /// The wire keys are `EntityKind::as_str`, every one is present when
    /// zero, and `slot` reaches the field the key names.
    #[test]
    fn memory_counts_serialize_every_kind_under_its_entity_kind_name() {
        let zero = serde_json::to_value(MemoryCounts::default()).expect("serializes");
        assert_eq!(
            zero,
            serde_json::json!({"Fact": 0, "Abstraction": 0, "Perspective": 0, "Goal": 0})
        );
        for (kind, amount) in [
            (EntityKind::Fact, 1_u64),
            (EntityKind::Abstraction, 2),
            (EntityKind::Perspective, 3),
            (EntityKind::Goal, 4),
        ] {
            let mut counts = MemoryCounts::default();
            *counts.slot(kind) += amount;
            let wire = serde_json::to_value(counts).expect("serializes");
            for (key, value) in wire.as_object().expect("an object") {
                let expected = if key == kind.as_str() { amount } else { 0 };
                assert_eq!(value, expected, "{key} after counting a {kind:?}");
            }
        }
    }

    /// A payload whose filterable tags live in `labels`, next to an
    /// unrelated `tags` field, and whose body field is `claim`.
    #[derive(Debug, serde::Serialize, serde::Deserialize)]
    struct LabelledFact {
        claim: String,
        body: String,
        labels: Vec<String>,
        tags: Vec<String>,
    }

    impl crate::FactPayload for LabelledFact {
        const SCHEMA_ID: &'static str = "test/labelled-fact";
        const SCHEMA_VERSION: u32 = 1;

        fn receipt_key(&self) -> Vec<u8> {
            let mut key = crate::PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
            key.field_str("claim", &self.claim);
            key.finish()
        }

        fn render(&self) -> String {
            self.claim.clone()
        }
    }

    fn labelled() -> crate::SidecarPayload {
        crate::SidecarPayload::fact(LabelledFact {
            claim: "the claim".into(),
            body: "legacy body".into(),
            labels: vec!["x".into()],
            tags: vec!["not-filterable".into()],
        })
    }

    #[test]
    fn display_reads_the_declared_body_and_tag_fields() {
        let declared = spec(EntityKind::Fact, "test/labelled-fact", 1)
            .with_display_fields(Some("claim".into()), Some("labels".into()));
        let payload = labelled();

        assert_eq!(declared.display_tags(Some(&payload)), vec!["x".to_owned()]);
        assert_eq!(
            declared.display_body(Some(&payload), Some("sketch")),
            Some("the claim".to_owned())
        );
    }

    #[test]
    fn display_without_declared_fields_keeps_the_legacy_keys() {
        let undeclared = spec(EntityKind::Fact, "test/labelled-fact", 1);
        let payload = labelled();

        assert_eq!(
            undeclared.display_tags(Some(&payload)),
            vec!["not-filterable".to_owned()],
            "no tag_column: the payload `tags` key is kept, not dropped"
        );
        assert_eq!(
            undeclared.display_body(Some(&payload), Some("sketch")),
            Some("legacy body".to_owned())
        );
        assert_eq!(
            undeclared.display_body(None, Some("sketch")),
            Some("sketch".to_owned()),
            "no payload text: the read path's own text"
        );
        assert!(undeclared.display_tags(None).is_empty());
    }

    fn spec(kind: EntityKind, id: &str, version: u32) -> MemorySchemaSpec {
        MemorySchemaSpec::new(
            kind,
            SchemaId::new(id.to_owned()),
            SchemaVersion::new(version),
            None,
        )
    }

    #[test]
    fn resolver_requires_exactly_one_selector() {
        let id = SchemaId::new("test/book".to_owned());
        assert!(matches!(
            resolve_memory_schema(&[], EntityKind::Fact, &id),
            Err(StorageError::ConstraintViolation(_))
        ));
        let one = [spec(EntityKind::Fact, "test/book", 2)];
        assert_eq!(
            resolve_memory_schema(&one, EntityKind::Fact, &id)
                .expect("one declaration")
                .schema_version,
            SchemaVersion::new(2)
        );
        let ambiguous = [
            spec(EntityKind::Fact, "test/book", 1),
            spec(EntityKind::Fact, "test/book", 2),
        ];
        assert!(matches!(
            resolve_memory_schema(&ambiguous, EntityKind::Fact, &id),
            Err(StorageError::ConstraintViolation(_))
        ));
    }

    #[test]
    fn resolver_allows_same_id_across_memory_layers() {
        let id = SchemaId::new("test/book".to_owned());
        let specs = [
            spec(EntityKind::Fact, "test/book", 1),
            spec(EntityKind::Abstraction, "test/book", 2),
        ];
        assert_eq!(
            resolve_memory_schema(&specs, EntityKind::Fact, &id)
                .expect("Fact declaration")
                .schema_version,
            SchemaVersion::new(1)
        );
        assert_eq!(
            resolve_memory_schema(&specs, EntityKind::Abstraction, &id)
                .expect("Abstraction declaration")
                .schema_version,
            SchemaVersion::new(2)
        );
    }
}

/// Triage-level summary of one active Goal. Detail is reachable through
/// the Goal read/query surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveGoalSummary {
    pub goal_id: GoalId,
    pub writer: WriterAttribution,
    pub goal_activated_memory_id: Option<MemoryId>,
    pub title: String,
}
