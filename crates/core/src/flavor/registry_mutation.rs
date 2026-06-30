use super::{
    Arc, AuthorizationHook, BTreeSet, CapabilityTag, DependencySatisfactionRule, FlavorDescriptor,
    FlavorRegistry, FlavorRegistryError, OwnerResolver, PayloadKind, RelationDescriptor,
    RequestBehavior, SchemaCapabilityTags, SchemaId, SchemaVersion,
};

impl FlavorRegistry {
    /// Register a relation. Substrate-only relations carry no
    /// `payload_schema`; typed relations point at a registered
    /// `EdgePayload` schema.
    /// # Errors
    ///
    /// Returns `InvalidRelationDescriptor` when descriptor-local masks are
    /// invalid.
    pub fn try_add_relation(
        &mut self,
        descriptor: RelationDescriptor,
    ) -> Result<(), FlavorRegistryError> {
        if let Err(message) = descriptor.validate_descriptor() {
            return Err(FlavorRegistryError::InvalidRelationDescriptor {
                relation: descriptor.relation,
                message,
            });
        }
        self.relations.push(descriptor);
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_relation_or_panic_for_tests(&mut self, descriptor: RelationDescriptor) {
        self.try_add_relation(descriptor)
            .expect("relation descriptor registration must be valid");
    }

    /// Attach opaque capability tags to a registered payload schema.
    ///
    /// # Panics
    ///
    /// Panics if any tag fails [`CapabilityTag::parse`]. The schema
    /// existence check runs at [`Self::try_freeze`], after every flavor has
    /// registered its schemas.
    /// # Errors
    ///
    /// Returns `InvalidCapabilityTag` when any tag fails capability syntax.
    pub fn try_add_schema_capability_tags<'a>(
        &mut self,
        kind: PayloadKind,
        schema_id: SchemaId,
        version: SchemaVersion,
        tags: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), FlavorRegistryError> {
        let mut parsed = BTreeSet::new();
        for tag in tags {
            parsed.insert(CapabilityTag::parse(tag).map_err(|err| {
                FlavorRegistryError::InvalidCapabilityTag {
                    schema_id: schema_id.clone(),
                    schema_version: version,
                    kind,
                    tag: tag.to_string(),
                    message: err.to_string(),
                }
            })?);
        }
        self.schema_capability_tags.push(SchemaCapabilityTags {
            schema_id,
            schema_version: version,
            kind,
            tags: parsed,
        });
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_schema_capability_tags_or_panic_for_tests<'a>(
        &mut self,
        kind: PayloadKind,
        schema_id: SchemaId,
        version: SchemaVersion,
        tags: impl IntoIterator<Item = &'a str>,
    ) {
        self.try_add_schema_capability_tags(kind, schema_id, version, tags)
            .expect("schema capability tags must be valid");
    }

    /// # Errors
    ///
    /// Currently infallible; duplicate rule ids are checked by
    /// [`Self::try_freeze`].
    pub fn try_add_dependency_satisfaction_rule(
        &mut self,
        schema_id: impl Into<String>,
        rule: Arc<dyn DependencySatisfactionRule>,
    ) -> Result<(), FlavorRegistryError> {
        self.dependency_satisfaction_rules
            .push((schema_id.into(), rule));
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_dependency_satisfaction_rule_or_panic_for_tests(
        &mut self,
        schema_id: impl Into<String>,
        rule: Arc<dyn DependencySatisfactionRule>,
    ) {
        self.try_add_dependency_satisfaction_rule(schema_id, rule)
            .expect("dependency satisfaction rule registration must be valid");
    }

    /// Register the composed app's owner resolver.
    /// # Errors
    ///
    /// Returns `DuplicateOwnerResolver` when a resolver is already registered.
    pub fn try_set_owner_resolver(
        &mut self,
        resolver: Arc<dyn OwnerResolver>,
    ) -> Result<(), FlavorRegistryError> {
        if self.owner_resolver.is_some() {
            return Err(FlavorRegistryError::DuplicateOwnerResolver);
        }
        self.owner_resolver = Some(resolver);
        Ok(())
    }

    #[doc(hidden)]
    pub fn set_owner_resolver_or_panic_for_tests(&mut self, resolver: Arc<dyn OwnerResolver>) {
        self.try_set_owner_resolver(resolver)
            .expect("owner resolver registration must be valid");
    }

    pub fn add_authorization_hook(&mut self, hook: Arc<dyn AuthorizationHook>) {
        self.authorization_hooks.push(hook);
    }

    pub fn add_request_behavior(&mut self, behavior: impl RequestBehavior + 'static) {
        self.request_behaviors.push(Arc::new(behavior));
    }

    /// # Errors
    ///
    /// Currently infallible; duplicate flavor ids are checked by
    /// [`Self::try_freeze`].
    pub fn try_add_flavor(
        &mut self,
        descriptor: FlavorDescriptor,
    ) -> Result<(), FlavorRegistryError> {
        self.flavors.push(descriptor);
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_flavor_or_panic_for_tests(&mut self, descriptor: FlavorDescriptor) {
        self.try_add_flavor(descriptor)
            .expect("flavor descriptor registration must be valid");
    }

    #[must_use]
    pub fn list_flavors(&self) -> &[FlavorDescriptor] {
        &self.flavors
    }
}
