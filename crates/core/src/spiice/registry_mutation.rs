use super::contract::SpiiceContract;
use super::{
    Arc, AuthorizationHook, BTreeSet, CapabilityTag, OwnerResolver, PayloadKind, RequestBehavior,
    SchemaCapabilityTags, SchemaId, SchemaVersion, SpiiceDescriptor, SpiiceRegistry,
    SpiiceRegistryError,
};

impl SpiiceRegistry {
    /// Attach opaque capability tags to a registered payload schema.
    ///
    /// # Panics
    ///
    /// Panics if any tag fails [`CapabilityTag::parse`]. The schema
    /// existence check runs at [`Self::try_freeze`], after every spiice has
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
    ) -> Result<(), SpiiceRegistryError> {
        let mut parsed = BTreeSet::new();
        for tag in tags {
            parsed.insert(CapabilityTag::parse(tag).map_err(|err| {
                SpiiceRegistryError::InvalidCapabilityTag {
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
    ) -> Result<(), SpiiceRegistryError> {
        if self.owner_resolver.is_some() {
            return Err(SpiiceRegistryError::DuplicateOwnerResolver);
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
    /// Currently infallible; duplicate spiice ids are checked by
    /// [`Self::try_freeze`].
    pub fn try_add_spiice(
        &mut self,
        descriptor: SpiiceDescriptor,
    ) -> Result<(), SpiiceRegistryError> {
        self.spiices.push(descriptor);
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_spiice_or_panic_for_tests(&mut self, descriptor: SpiiceDescriptor) {
        self.try_add_spiice(descriptor)
            .expect("spiice descriptor registration must be valid");
    }

    /// Register a spiice's [`SpiiceContract`].
    ///
    /// The contract is what erase, export, forget, transfer, the migration
    /// guardrail and the MCP manifest iterate. A spiice that registers
    /// schemas without one is invisible to every one of those walks, so
    /// [`Self::try_freeze`](crate::SpiiceRegistry::try_freeze) cross-checks
    /// the two against each other.
    ///
    /// # Errors
    ///
    /// Currently infallible; ordinal collisions, resource declarations from
    /// a non-core spiice, and contract/registration drift are all checked by
    /// `try_freeze`, after every spiice has registered.
    pub fn try_add_contract(
        &mut self,
        contract: &'static SpiiceContract,
    ) -> Result<(), SpiiceRegistryError> {
        self.contracts.push(contract);
        Ok(())
    }

    #[doc(hidden)]
    pub fn add_contract_or_panic_for_tests(&mut self, contract: &'static SpiiceContract) {
        self.try_add_contract(contract)
            .expect("spiice contract registration must be valid");
    }

    #[must_use]
    pub fn list_spiices(&self) -> &[SpiiceDescriptor] {
        &self.spiices
    }

    #[must_use]
    pub fn list_contracts(&self) -> &[&'static SpiiceContract] {
        &self.contracts
    }
}
