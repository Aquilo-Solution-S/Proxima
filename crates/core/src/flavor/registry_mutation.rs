use super::contract::FlavorContract;
use super::{
    Arc, AuthorizationHook, BTreeSet, CapabilityTag, FlavorDescriptor, FlavorRegistry,
    FlavorRegistryError, OwnerResolver, PayloadKind, RequestBehavior, SchemaCapabilityTags,
    SchemaId, SchemaVersion,
};

impl FlavorRegistry {
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

    /// Register a flavor's [`FlavorContract`].
    ///
    /// The contract is what erase, export, forget, transfer, the migration
    /// guardrail and the MCP manifest iterate. A flavor that registers
    /// schemas without one is invisible to every one of those walks, so
    /// [`Self::try_freeze`](crate::FlavorRegistry::try_freeze) cross-checks
    /// the two against each other.
    ///
    /// # Errors
    ///
    /// Currently infallible; ordinal collisions, resource declarations from
    /// a non-core flavor, and contract/registration drift are all checked by
    /// `try_freeze`, after every flavor has registered.
    pub fn try_add_contract(
        &mut self,
        contract: &'static FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        self.contracts.push(contract);
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_contract_or_panic_for_tests(&mut self, contract: &'static FlavorContract) {
        self.try_add_contract(contract)
            .expect("flavor contract registration must be valid");
    }

    #[must_use]
    pub fn list_flavors(&self) -> &[FlavorDescriptor] {
        &self.flavors
    }

    #[must_use]
    pub fn list_contracts(&self) -> &[&'static FlavorContract] {
        &self.contracts
    }
}
