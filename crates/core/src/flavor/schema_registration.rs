use super::ingress::{
    ingest_abstraction_payload, ingest_citation_mapping_payload, ingest_cited_object_payload,
    ingest_fact_payload, ingest_goal_payload, ingest_perspective_payload,
};
use super::{
    AbstractionPayload, CitationMappingPayload, CitedObjectPayload, FactPayload, FlavorRegistry,
    FlavorRegistryError, GoalPayload, PayloadKind, PerspectivePayload, ProtocolPayloadIngress,
    ProtocolPayloadIngressEntry, SchemaId, SchemaInfo, SchemaVersion,
};

impl FlavorRegistry {
    /// Shared tail for the typed `add_*_schema` methods: records the
    /// `SchemaInfo` and the protocol ingress entry. Callers build the
    /// kind-specific `SchemaInfo`; `schema_id` / `schema_version` / `kind`
    /// for the ingress entry are read back off it so they cannot drift
    /// from the schema.
    ///
    /// A search projection is NOT recorded here any more. It used to be
    /// `FactPayload::search_projection()`, a second search vocabulary that
    /// the `FlavorContract`'s `SearchProjectionDecl` shadowed without
    /// governing. There is one vocabulary now, and the projections are
    /// derived from the contracts at freeze.
    fn register_schema(
        &mut self,
        schema_info: SchemaInfo,
        ingress: ProtocolPayloadIngress,
        json_schema: Option<serde_json::Value>,
    ) {
        let schema_id = schema_info.schema_id.clone();
        let schema_version = schema_info.schema_version;
        let kind = schema_info.kind;
        self.schemas.push(schema_info);
        self.protocol_ingress.push(ProtocolPayloadIngressEntry {
            schema_id,
            schema_version,
            kind,
            ingress,
            json_schema,
        });
    }

    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_fact_schema<F: FactPayload>(&mut self) -> Result<(), FlavorRegistryError> {
        self.register_schema(
            SchemaInfo {
                schema_id: F::schema_id(),
                schema_version: SchemaVersion::new(F::SCHEMA_VERSION),
                kind: PayloadKind::Fact,
                filter_keys: vec![],
                sidecar_table: F::sidecar_table().map(std::string::ToString::to_string),
                natural_key_columns: F::natural_key_columns()
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
                tombstone: F::tombstone().map(|t| crate::verbs::schema::SchemaTombstone {
                    column: t.column.to_string(),
                    value: t.value.to_string(),
                }),
                has_typed_ingress: true,
                cited_object_schema: None,
                embeddable: F::EMBEDDABLE,
            },
            ingest_fact_payload::<F>,
            F::json_schema(),
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_fact_schema_or_panic_for_tests<F: FactPayload>(&mut self) {
        self.try_add_fact_schema::<F>()
            .expect("fact schema registration must be valid");
    }

    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_abstraction_schema<A: AbstractionPayload>(
        &mut self,
    ) -> Result<(), FlavorRegistryError> {
        self.register_schema(
            SchemaInfo {
                schema_id: A::schema_id(),
                schema_version: SchemaVersion::new(A::SCHEMA_VERSION),
                kind: PayloadKind::Abstraction,
                filter_keys: vec![],
                sidecar_table: Some(A::sidecar_table().to_string()),
                natural_key_columns: vec![],
                tombstone: None,
                has_typed_ingress: true,
                cited_object_schema: None,
                embeddable: true,
            },
            ingest_abstraction_payload::<A>,
            A::json_schema(),
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_abstraction_schema_or_panic_for_tests<A: AbstractionPayload>(&mut self) {
        self.try_add_abstraction_schema::<A>()
            .expect("abstraction schema registration must be valid");
    }

    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_perspective_schema<P: PerspectivePayload>(
        &mut self,
    ) -> Result<(), FlavorRegistryError> {
        self.register_schema(
            SchemaInfo {
                schema_id: P::schema_id(),
                schema_version: SchemaVersion::new(P::SCHEMA_VERSION),
                kind: PayloadKind::Perspective,
                filter_keys: vec![],
                sidecar_table: Some(P::sidecar_table().to_string()),
                natural_key_columns: vec![],
                tombstone: None,
                has_typed_ingress: true,
                cited_object_schema: None,
                embeddable: true,
            },
            ingest_perspective_payload::<P>,
            P::json_schema(),
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_perspective_schema_or_panic_for_tests<P: PerspectivePayload>(&mut self) {
        self.try_add_perspective_schema::<P>()
            .expect("perspective schema registration must be valid");
    }

    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_goal_schema<G: GoalPayload>(&mut self) -> Result<(), FlavorRegistryError> {
        let sidecar_table = G::sidecar_table();
        self.register_schema(
            SchemaInfo {
                schema_id: G::schema_id(),
                schema_version: SchemaVersion::new(G::SCHEMA_VERSION),
                kind: PayloadKind::Goal,
                filter_keys: vec![],
                sidecar_table: sidecar_table.map(std::string::ToString::to_string),
                natural_key_columns: vec![],
                tombstone: None,
                has_typed_ingress: true,
                cited_object_schema: None,
                embeddable: true,
            },
            ingest_goal_payload::<G>,
            G::json_schema(),
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_goal_schema_or_panic_for_tests<G: GoalPayload>(&mut self) {
        self.try_add_goal_schema::<G>()
            .expect("goal schema registration must be valid");
    }

    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_cited_object_schema<C: CitedObjectPayload>(
        &mut self,
    ) -> Result<(), FlavorRegistryError> {
        self.register_schema(
            SchemaInfo {
                schema_id: C::schema_id(),
                schema_version: SchemaVersion::new(C::SCHEMA_VERSION),
                kind: PayloadKind::CitedObject,
                filter_keys: vec![],
                sidecar_table: None,
                natural_key_columns: vec![],
                tombstone: None,
                has_typed_ingress: true,
                cited_object_schema: None,
                embeddable: true,
            },
            ingest_cited_object_payload::<C>,
            C::json_schema(),
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_cited_object_schema_or_panic_for_tests<C: CitedObjectPayload>(&mut self) {
        self.try_add_cited_object_schema::<C>()
            .expect("cited-object schema registration must be valid");
    }

    /// # Errors
    ///
    /// Currently infallible; returns a registry error if schema admission adds
    /// validation.
    pub fn try_add_citation_mapping_schema<M: CitationMappingPayload>(
        &mut self,
    ) -> Result<(), FlavorRegistryError> {
        self.register_schema(
            SchemaInfo {
                schema_id: M::schema_id(),
                schema_version: SchemaVersion::new(M::SCHEMA_VERSION),
                kind: PayloadKind::CitationMapping,
                filter_keys: vec![],
                sidecar_table: M::sidecar_table().map(std::string::ToString::to_string),
                natural_key_columns: vec![],
                tombstone: None,
                has_typed_ingress: true,
                cited_object_schema: Some(M::cited_object_schema()),
                embeddable: true,
            },
            ingest_citation_mapping_payload::<M>,
            M::json_schema(),
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_citation_mapping_schema_or_panic_for_tests<M: CitationMappingPayload>(&mut self) {
        self.try_add_citation_mapping_schema::<M>()
            .expect("citation-mapping schema registration must be valid");
    }

    /// Register an *opaque* schema — one with no Rust payload type.
    /// The blessed path for content-addressed `CitedObject`s and
    /// structural `CitationMapping`s whose payload is an opaque blob;
    /// it carries no typed ingress parser and no JSON schema. Opaque
    /// content enters through the explicit citation APIs;
    /// `ingest_protocol_payload` rejects it.
    ///
    /// This is the *only* sanctioned way to register an untyped schema.
    /// `try_freeze()` asserts every other schema has a typed ingress parser,
    /// so a dropped parser fails the build rather than silently disabling
    /// validation and typed sidecar construction.
    /// # Errors
    ///
    /// Returns [`FlavorRegistryError::OpaqueSchemaKind`] for an opaque
    /// Fact, Abstraction, Perspective, or Goal schema.
    pub fn try_add_opaque_schema(
        &mut self,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    ) -> Result<(), FlavorRegistryError> {
        if !matches!(
            kind,
            PayloadKind::CitedObject | PayloadKind::CitationMapping
        ) {
            return Err(FlavorRegistryError::OpaqueSchemaKind {
                schema_id,
                schema_version,
                kind,
            });
        }
        self.schemas
            .push(SchemaInfo::opaque(schema_id, schema_version, kind));
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_opaque_schema_or_panic_for_tests(
        &mut self,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    ) {
        self.try_add_opaque_schema(schema_id, schema_version, kind)
            .expect("opaque schema registration must be valid");
    }
}
