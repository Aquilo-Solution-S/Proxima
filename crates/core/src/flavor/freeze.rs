use super::{
    BTreeSet, CapabilityTag, FlavorRegistry, FlavorRegistryError, FlavorRegistryFrozen,
    McpToolDescriptor, PayloadKind, SchemaCapabilityTags, SchemaId, SchemaVersion,
};

/// Whether two schemas would hand `ts_rank` different weight arrays.
///
/// `total_cmp` rather than `==`: the values are `f32`s derived from
/// declared relative weights, and a bit-exact total order is both what the
/// renderer's `{}`-formatting reproduces and the only comparison that is
/// meaningful for a float nobody arithmetically combined.
fn weight_arrays_differ(first: Option<[f32; 4]>, second: Option<[f32; 4]>) -> bool {
    match (first, second) {
        (None, None) => false,
        (Some(first), Some(second)) => first
            .iter()
            .zip(second.iter())
            .any(|(a, b)| a.total_cmp(b) != std::cmp::Ordering::Equal),
        _ => true,
    }
}

impl FlavorRegistry {
    /// Validate the registry and seal it for runtime use.
    /// # Errors
    ///
    /// Returns typed registry errors for invalid descriptors, unregistered
    /// references, ingress mismatches, unsatisfiable tags, and duplicate ids.
    pub fn try_freeze(self) -> Result<FlavorRegistryFrozen, FlavorRegistryError> {
        self.validate_schema_capability_tags_resolve()?;
        self.validate_flavor_descriptors()?;
        let mut seen_schemas = std::collections::HashSet::new();
        for schema in &self.schemas {
            if !seen_schemas.insert((schema.schema_id.clone(), schema.schema_version, schema.kind))
            {
                return Err(FlavorRegistryError::DuplicateSchema {
                    schema_id: schema.schema_id.clone(),
                    schema_version: schema.schema_version,
                    kind: schema.kind,
                });
            }
        }
        // Every Memory/Goal schema is typed; only citation schemas may be
        // opaque. A typed descriptor whose ingress parser was dropped is
        // unusable at the protocol boundary, so catch that internal drift
        // at startup rather than at first write.
        for schema in &self.schemas {
            if !schema.has_typed_ingress
                && !matches!(
                    schema.kind,
                    PayloadKind::CitedObject | PayloadKind::CitationMapping
                )
            {
                return Err(FlavorRegistryError::OpaqueSchemaKind {
                    schema_id: schema.schema_id.clone(),
                    schema_version: schema.schema_version,
                    kind: schema.kind,
                });
            }
            let ingress_count = self
                .protocol_ingress
                .iter()
                .filter(|entry| {
                    entry.schema_id == schema.schema_id
                        && entry.schema_version == schema.schema_version
                        && entry.kind == schema.kind
                })
                .count();
            let expected_ingress_count = usize::from(schema.has_typed_ingress);
            if ingress_count != expected_ingress_count {
                return Err(FlavorRegistryError::SchemaIngressMismatch {
                    schema_id: schema.schema_id.clone(),
                    schema_version: schema.schema_version,
                    kind: schema.kind,
                });
            }
        }
        for entry in &self.protocol_ingress {
            let resolves_to_typed_schema = self.schemas.iter().any(|schema| {
                schema.schema_id == entry.schema_id
                    && schema.schema_version == entry.schema_version
                    && schema.kind == entry.kind
                    && schema.has_typed_ingress
            });
            if !resolves_to_typed_schema {
                return Err(FlavorRegistryError::SchemaIngressMismatch {
                    schema_id: entry.schema_id.clone(),
                    schema_version: entry.schema_version,
                    kind: entry.kind,
                });
            }
        }
        let mut seen_tools = std::collections::HashSet::new();
        for tool in &self.mcp_tools {
            if !seen_tools.insert(tool.name) {
                return Err(FlavorRegistryError::DuplicateTool { name: tool.name });
            }
        }
        self.validate_tools_declare_behavior()?;
        self.validate_dispatcher_action_specs()?;
        self.validate_contracts()?;
        Ok(FlavorRegistryFrozen::from_registry(self))
    }

    /// Cross-check the declarations against the registrations.
    ///
    /// This is the check that makes "everything is a flavor" structural
    /// rather than aspirational: a schema registered without a contract
    /// entry, or a contract entry with no registration, fails the build of
    /// the composed binary rather than going missing from an erase sweep.
    fn validate_contracts(&self) -> Result<(), FlavorRegistryError> {
        let mut seen_ordinals = std::collections::HashSet::new();
        let mut has_core = false;
        for contract in &self.contracts {
            if !seen_ordinals.insert(contract.ordinal) {
                return Err(FlavorRegistryError::DuplicateFlavorOrdinal {
                    ordinal: contract.ordinal,
                    flavor_id: contract.flavor_id,
                });
            }
            if contract.is_core() {
                has_core = true;
            } else if !contract.resources.is_empty() {
                // Narrow reading of the resource checkpoint: resources are
                // flavor #0's. A flavor resource would need its own
                // scope-key namespace, a URI-template parser for its
                // parameters and a pagination contract — a feature with its
                // own design, not a forwarding line.
                return Err(FlavorRegistryError::ResourcesNotPermitted {
                    flavor_id: contract.flavor_id,
                });
            }
            // Registrations first, declarations second, and deliberately
            // so. A contract entry with no registration is the defect that
            // makes every OTHER reading of that entry meaningless, so it is
            // the one to report. Reordering these two to make a test fixture
            // reachable was the wrong fix: it changed which error a
            // genuinely broken contract reports at boot in order to spare a
            // fixture two lines of registration. The fixture registers now
            // (see `register_fixture_schema`).
            self.validate_contract_schemas(contract)?;
            Self::validate_contract_projection(contract)?;
            Self::validate_contract_surfaces(contract)?;
            Self::validate_erase_legs(contract)?;
            Self::validate_transfer_legs(contract)?;
        }
        if !self.contracts.is_empty() && !has_core {
            return Err(FlavorRegistryError::MissingCoreContract);
        }
        Ok(())
    }

    /// Every surface the flavor says is exportable must be REACHABLE from
    /// the owner, and the check is here because the answer never depends on
    /// the request.
    ///
    /// The owner export no longer keeps a hand-written statement per table;
    /// it generates one per declared surface. That generator has exactly two
    /// shapes — filter the surface's own `owner_id`, or join the home table
    /// of its key and filter there — so a surface that declares `Rows` or
    /// `Allowlist` while carrying neither an owner column nor a key with a
    /// home is a bundle leg nothing can emit. Before the generator, such a
    /// surface simply went missing from every bundle, silently, which is the
    /// class of defect this refuses at boot.
    ///
    /// It is deliberately not a check on ERASE. An unreachable surface that
    /// declares `Excluded` is a stated non-export; one that cascades is
    /// deleted by a constraint whether or not anything can name its owner.
    fn validate_contract_surfaces(
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        use crate::flavor::contract::ExportRule;

        for surface in contract.all_surfaces() {
            if matches!(surface.export, ExportRule::Excluded { .. }) {
                continue;
            }
            if surface.owner_columns.is_empty() && surface.key.home().is_none() {
                return Err(FlavorRegistryError::UnreachableExportSurface {
                    flavor_id: contract.flavor_id,
                    table: surface.table,
                });
            }
        }
        Ok(())
    }

    /// Every surface must have a leg that destroys it, and the check is at
    /// boot because a missing one is discovered nowhere else.
    ///
    /// The erase partitions every declared surface into exactly one of five
    /// answers — a generated keyed leg, a generated owned leg, a named
    /// hand-written leg, a constraint, or a declared non-erase with a
    /// reason. There is no sixth answer, and the sixth that kept appearing
    /// was silence: `ByKey` on a key the erase builds no selection set for
    /// fell through both generic loops, and only an exemption list nobody
    /// had updated stood between that and a table nothing ever deletes.
    ///
    /// This was a unit test in `proxima-storage-pg` until it wasn't enough.
    /// That crate does not depend on any flavor, so the test could only ever
    /// see flavor #0; an out-of-tree flavor declaring `ByKey` on a
    /// `Custom` key froze cleanly, booted cleanly, and its erase reported
    /// `Completed` over rows that outlived their owner. Under a model where
    /// the host owns every promise about erasure, a substrate that quietly
    /// keeps rows is the one failure it must not have. So the partition is
    /// here, where every flavor passes through, in-tree or not.
    ///
    /// [`FlavorContract::erase_leg`] is the classifier both this and the
    /// erase itself call, so this is a check on the code that runs rather
    /// than on a second description of it.
    fn validate_erase_legs(
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        use crate::flavor::contract::{EraseLeg, EraseRule};

        for surface in contract.all_surfaces() {
            if contract.erase_leg(&surface) == EraseLeg::Unreachable {
                return Err(FlavorRegistryError::UndeletableSurface {
                    flavor_id: contract.flavor_id,
                    table: surface.table,
                });
            }
        }

        // A stale name is how the hand-written lists rotted in the first
        // place, and an exemption that claims a `Cascade` or `Never`
        // surface is a flavor arguing with itself about whether a statement
        // runs.
        for table in contract.bespoke_erase_legs {
            let why = match contract
                .all_surfaces()
                .find(|surface| surface.table == *table)
            {
                None => "this flavor does not declare",
                Some(surface) => match surface.erase {
                    EraseRule::ByKey | EraseRule::ByOwner => continue,
                    EraseRule::Cascade { .. } => {
                        "a constraint removes, so no hand-written statement should touch it"
                    }
                    EraseRule::Never { .. } => {
                        "is a declared non-erase, so no statement should touch it"
                    }
                },
            };
            return Err(FlavorRegistryError::BespokeEraseLegMismatch {
                flavor_id: contract.flavor_id,
                table,
                why,
            });
        }
        Ok(())
    }

    /// Every surface must have a leg that MOVES it, or a declaration
    /// saying it deliberately does not move, and the check is at boot for
    /// the same reason the erase's is: a missing one is discovered nowhere
    /// else.
    ///
    /// The transfer partitions every declared surface into exactly one of
    /// seven answers — a generated re-home, a generated drop, a generated
    /// dedupe, a named hand-written leg, a key-owned non-move, a deliberate
    /// retention at the source, or a refusal. There is no eighth, and the
    /// eighth that was there until Phase 4 was silence: `owner_columns.rs`
    /// named its fourteen tables as string literals and referenced no
    /// contract type at all, so a flavor adding a `Follow` surface got no
    /// statement, no error, and no way to find out.
    ///
    /// That silence is worse here than on the erase side, which is why the
    /// partition was worth its lines. An unerased row outlives its owner
    /// and is found by reconcile. An unmoved row is readable by the SOURCE
    /// owner after the memory became the destination's — a cross-tenant
    /// read under the multi-owner design centre, produced by nobody
    /// deciding anything.
    ///
    /// [`FlavorContract::transfer_leg`] is the classifier both this and the
    /// transfer itself call, so this is a check on the code that runs
    /// rather than on a second description of it.
    fn validate_transfer_legs(
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        use crate::flavor::contract::TransferLeg;

        for surface in contract.all_surfaces() {
            if contract.transfer_leg(&surface) == TransferLeg::Unreachable {
                return Err(FlavorRegistryError::UnmovableSurface {
                    flavor_id: contract.flavor_id,
                    table: surface.table,
                });
            }
        }

        // A stale name is how the hand-written lists rotted in the first
        // place, and an exemption claiming a surface whose rule says NO
        // statement runs is a flavor arguing with itself about whether one
        // does.
        for table in contract.bespoke_transfer_legs {
            let why = match contract
                .all_surfaces()
                .find(|surface| surface.table == *table)
            {
                None => "this flavor does not declare",
                Some(surface) => {
                    if contract.transfer_leg(&surface).moves_rows() {
                        continue;
                    }
                    "declares a transfer that moves no rows, so no hand-written statement \
                     should touch it"
                }
            };
            return Err(FlavorRegistryError::BespokeTransferLegMismatch {
                flavor_id: contract.flavor_id,
                table,
                why,
            });
        }
        Ok(())
    }

    /// What the flavor's PROJECTION declares, checked against the schemas
    /// that project into it.
    ///
    /// Two rules, both earning a declaration that would otherwise decorate:
    ///
    /// 1. `RankSource::Projection` means ONE statement serves the whole
    ///    flavor, so every property that statement can spell only once —
    ///    the lexical configuration, the score windows and `ts_rank`'s
    ///    weight array — must agree across the flavor's projected schemas,
    ///    and the renderer's three band names must all be declared.
    ///    Deciding this at freeze rather than at query-build time is the
    ///    point: the answer never depends on the request, and discovering
    ///    it on a hot path would be a `StorageError` where a boot refusal
    ///    belongs.
    ///
    ///    The weight array joined this list late. The renderer reads it off
    ///    the flavor's FIRST participating schema, like the other two, but
    ///    only language and bands were checked — so a flavor whose schemas
    ///    declared different weight LEVELS would have had one schema's
    ///    array applied to every schema's vector, silently, and the doc
    ///    claiming freeze guaranteed otherwise would have been wrong.
    /// 2. `BandComparability::CoreBands` is the claim a cross-flavor merge
    ///    compares scores on. A flavor whose bands leave flavor #0's
    ///    `[0, 1]` window cannot make it.
    fn validate_contract_projection(
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        use crate::flavor::contract::{
            BAND_NAME_EXACT, BAND_NAME_RESCUE, BAND_NAME_SUBSTRING, BandComparability,
        };

        let Some(spec) = contract.projection.spec() else {
            return Ok(());
        };
        let mut reference: Option<&'static crate::flavor::contract::SchemaContract> = None;
        for (schema, _) in contract.projected_schemas() {
            let schema_id = schema.schema_id();
            if matches!(spec.band_comparability, BandComparability::CoreBands) {
                for band in schema.search.bands() {
                    if band.floor < 0.0 || band.ceiling > 1.0 {
                        return Err(FlavorRegistryError::ProjectionBandOutsideCoreWindow {
                            flavor_id: contract.flavor_id,
                            schema_id,
                            band: band.name,
                            window: format!("[{}, {}]", band.floor, band.ceiling),
                        });
                    }
                }
            }
            if !spec.rank_source.is_projection() {
                continue;
            }
            for name in [BAND_NAME_EXACT, BAND_NAME_RESCUE, BAND_NAME_SUBSTRING] {
                if schema.search.band(name).is_none() {
                    return Err(FlavorRegistryError::ProjectionBandName {
                        flavor_id: contract.flavor_id,
                        schema_id,
                        missing: name,
                    });
                }
            }
            let Some(first) = reference else {
                reference = Some(schema);
                continue;
            };
            if first.search.language() != schema.search.language() {
                return Err(FlavorRegistryError::ProjectionRenderNotUniform {
                    flavor_id: contract.flavor_id,
                    schema_id,
                    property: "language",
                });
            }
            if first.search.bands() != schema.search.bands() {
                return Err(FlavorRegistryError::ProjectionRenderNotUniform {
                    flavor_id: contract.flavor_id,
                    schema_id,
                    property: "bands",
                });
            }
            if weight_arrays_differ(
                first.search.rank_weight_array(),
                schema.search.rank_weight_array(),
            ) {
                return Err(FlavorRegistryError::ProjectionRenderNotUniform {
                    flavor_id: contract.flavor_id,
                    schema_id,
                    property: "rank_weights",
                });
            }
        }
        Ok(())
    }

    fn validate_contract_schemas(
        &self,
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        let prefix = format!("{}/", contract.flavor_id);
        for schema in contract.schemas {
            let schema_id = schema.schema_id();
            if !schema_id.as_str().starts_with(&prefix) {
                return Err(FlavorRegistryError::ContractSchemaPrefix {
                    flavor_id: contract.flavor_id,
                    schema_id,
                });
            }
            // A NotTransferable that names no enforcement site is a comment,
            // not a contract: the refusal has to survive a code path that
            // forgets to ask.
            if let crate::flavor::contract::TransferRule::NotTransferable { enforced_by, .. } =
                schema.transfer
                && enforced_by.is_empty()
            {
                return Err(FlavorRegistryError::UnenforcedTransferRefusal {
                    flavor_id: contract.flavor_id,
                    schema_id,
                });
            }
            // `PostgreSQL` forces four tsvector weight classes on the
            // storage; the declaration is free of that limit and states
            // relative floats. Where the two meet is here: more distinct
            // levels than classes has no honest bucketing, so it is a
            // freeze error naming the mechanism rather than a silent
            // collapse of two levels into one class.
            if let Err(levels) = schema.search.weight_levels() {
                return Err(FlavorRegistryError::ProjectionWeightLevels {
                    flavor_id: contract.flavor_id,
                    schema_id,
                    levels,
                    classes: crate::flavor::contract::TSVECTOR_WEIGHT_CLASSES.len(),
                });
            }
            // The shared-blob dedupe arm's blind spot, made loud.
            //
            // A cross-owner transfer of a shared blob now gives the
            // destination a NEW `blob` row and repoints the columns that
            // reference it. Those columns are enumerable exactly because
            // they are foreign keys. A cited-object or citation-mapping
            // sidecar references a blob by convention — `cited_object_id`
            // holds a `blob_id` with nothing in the catalog saying so — and
            // the remap would walk straight past it, leaving the rows
            // pointing at the source owner's row after the citation moved.
            //
            // Today every such schema is opaque (`sidecar_table: None`),
            // which is why the arm is safe to land. Declaring one is the
            // moment the remap needs designing, so that is the moment this
            // refuses, rather than the moment a transfer silently splits a
            // citation from its bytes.
            if matches!(
                schema.kind,
                crate::verbs::schema::PayloadKind::CitedObject
                    | crate::verbs::schema::PayloadKind::CitationMapping
            ) && let Some(table) = schema.sidecar_table
            {
                return Err(FlavorRegistryError::CitationSidecarNotRemappable {
                    flavor_id: contract.flavor_id,
                    schema_id,
                    table,
                });
            }
            // `PerRow { column }` carried a column name nothing read: the
            // generator emits one language column per projection table and
            // names it itself. Left unchecked, a flavor could declare
            // `PerRow { column: "row_config" }`, see no error, and get rows
            // stamped and ranked under a column its contract never named.
            // Consuming the payload as a constraint is the smallest honest
            // reading of it — the generator emits one column, so declaring
            // a different one is a need the shape cannot express, and the
            // rule is that such a need becomes a vocabulary extension
            // rather than a silent divergence.
            if let Some(declared) = schema.search.per_row_language_column() {
                let projection_column = contract
                    .projection
                    .spec()
                    .and_then(|spec| spec.surface().lexical_language_column);
                if projection_column != Some(declared) {
                    return Err(FlavorRegistryError::ProjectionLanguageColumn {
                        flavor_id: contract.flavor_id,
                        schema_id,
                        declared,
                        projection_column,
                    });
                }
            }
            let registered = self.schemas.iter().any(|info| {
                info.schema_id == schema_id
                    && info.schema_version == schema.schema_version()
                    && info.kind == schema.kind
            });
            if !registered {
                return Err(FlavorRegistryError::ContractSchemaNotRegistered {
                    flavor_id: contract.flavor_id,
                    schema_id,
                    schema_version: schema.schema_version(),
                    kind: schema.kind,
                });
            }
        }
        for info in &self.schemas {
            if !info.schema_id.as_str().starts_with(&prefix) {
                continue;
            }
            let declared = contract.schemas.iter().any(|schema| {
                schema.schema_id() == info.schema_id
                    && schema.schema_version() == info.schema_version
                    && schema.kind == info.kind
            });
            if !declared {
                return Err(FlavorRegistryError::SchemaWithoutContract {
                    flavor_id: contract.flavor_id,
                    schema_id: info.schema_id.clone(),
                    schema_version: info.schema_version,
                    kind: info.kind,
                });
            }
        }
        for tool in contract.tools {
            if !self
                .mcp_tools
                .iter()
                .any(|entry| entry.name == tool.wire_name)
            {
                return Err(FlavorRegistryError::ContractToolNotRegistered {
                    flavor_id: contract.flavor_id,
                    name: tool.wire_name,
                });
            }
        }
        Ok(())
    }

    #[must_use]
    #[doc(hidden)]
    pub fn freeze_or_panic_for_tests(self) -> FlavorRegistryFrozen {
        self.try_freeze()
            .expect("flavor registry must be valid before freeze")
    }

    fn validate_schema_capability_tags_resolve(&self) -> Result<(), FlavorRegistryError> {
        for binding in &self.schema_capability_tags {
            if !self.schemas.iter().any(|schema| {
                schema.schema_id == binding.schema_id
                    && schema.schema_version == binding.schema_version
                    && schema.kind == binding.kind
            }) {
                return Err(FlavorRegistryError::UnregisteredSchemaCapabilityTags {
                    schema_id: binding.schema_id.clone(),
                    schema_version: binding.schema_version,
                    kind: binding.kind,
                });
            }
        }
        Ok(())
    }

    /// Cross-check: the owner-role gate can classify every registered flat
    /// tool.
    ///
    /// `ScopeGateBehavior::enforce_owner_role` demands WRITE when it cannot
    /// tell. Same two steps, same order: the tool's `ANNOTATIONS`, then the
    /// core manifest. Unclassified is silently a write. Dispatcher actions
    /// missing per-action annotations classify as writes.
    fn validate_tools_declare_behavior(&self) -> Result<(), FlavorRegistryError> {
        for tool in &self.mcp_tools {
            if tool.action_arg_specs.is_empty()
                && tool.annotations.is_none()
                && crate::mcp::core_tool_annotations(tool.name).is_none()
            {
                return Err(FlavorRegistryError::UndeclaredToolBehavior { name: tool.name });
            }
        }
        Ok(())
    }

    /// Cross-check: a dispatcher's declared actions and its derived schema
    /// describe the same dispatcher.
    ///
    /// `McpToolDescriptor::action_arg_specs` is the one enumeration.
    /// Discriminator must be `action`: `ToolScope` keys are `"{tool}:{action}"`,
    /// validators and the scope gate read `args["action"]`, REST injects
    /// `"action"` before dispatch.
    fn validate_dispatcher_action_specs(&self) -> Result<(), FlavorRegistryError> {
        for tool in &self.mcp_tools {
            // Absent and malformed are different answers. Reading them as one
            // — `.and_then(Value::as_object)` — let a schema that *carries*
            // the extension, and so may well be read as a dispatcher by
            // anything less forgiving, pass here as a flat tool. The derive
            // always writes an object; a hand-written `JsonSchema` need not.
            let extension = match tool.args_schema.get("x-proxima-actions") {
                Some(serde_json::Value::Object(extension)) => Some(extension),
                Some(malformed) => {
                    return Err(FlavorRegistryError::InvalidActionSpecs {
                        name: tool.name,
                        message: format!(
                            "its schema carries a malformed `x-proxima-actions` extension: \
                             expected an object keyed by action name, found {malformed}. \
                             Nothing can enumerate the actions of an extension it cannot read"
                        ),
                    });
                }
                None => None,
            };
            let Some(extension) = extension else {
                if tool.action_arg_specs.is_empty() {
                    continue;
                }
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: "declares ACTION_ARG_SPECS but its `Args` produced no action \
                              schema: either it is not an internally tagged enum, or one of \
                              its variants did not flatten — the flattener needs every variant \
                              to be an object schema carrying a string `const` at the \
                              discriminator. Either way there is nothing to validate against"
                        .to_string(),
                });
            };
            if tool.action_arg_specs.is_empty() {
                return Err(FlavorRegistryError::DispatcherWithoutActionSpecs { name: tool.name });
            }
            // The flattener writes `required = [discriminator]`, so the tag
            // name is readable straight off the normalized schema.
            let discriminator = tool
                .args_schema
                .get("required")
                .and_then(serde_json::Value::as_array)
                .and_then(|required| required.first())
                .and_then(serde_json::Value::as_str);
            if discriminator != Some("action") {
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: format!(
                        "dispatcher discriminator is {discriminator:?}; a dispatcher must tag on \
                         `action` (#[serde(tag = \"action\")]) because scope keys, the scope \
                         gate, and the REST action routes all read that field"
                    ),
                });
            }
            let declared = tool
                .action_arg_specs
                .iter()
                .map(|spec| spec.action)
                .collect::<BTreeSet<_>>();
            // Length before set equality: a set collapses duplicate action
            // names. The later spec is never read (`validate_action_args`
            // and the scope gate take the first match).
            if tool.action_arg_specs.len() != declared.len() {
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: format!(
                        "ACTION_ARG_SPECS holds {} specs naming {} distinct actions, so an \
                         action name is duplicated; the later spec for a duplicate action is \
                         never read",
                        tool.action_arg_specs.len(),
                        declared.len()
                    ),
                });
            }
            let derived = extension
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            if declared != derived {
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: format!(
                        "ACTION_ARG_SPECS names {declared:?} but the derived schema names \
                         {derived:?}"
                    ),
                });
            }
            validate_action_field_sets(tool, extension)?;
        }
        Ok(())
    }

    /// Cross-check: every `FlavorDescriptor::flavor_id` is unique.
    fn validate_flavor_descriptors(&self) -> Result<(), FlavorRegistryError> {
        let mut seen_ids = std::collections::HashSet::new();
        for flavor in &self.flavors {
            if !seen_ids.insert(flavor.flavor_id.as_str()) {
                return Err(FlavorRegistryError::DuplicateFlavor {
                    flavor_id: flavor.flavor_id.clone(),
                });
            }
        }
        Ok(())
    }
}

/// The per-action half of [`FlavorRegistry::validate_dispatcher_action_specs`]:
/// each spec's field lists against the ones the schema derived for that action.
/// The action sets are known to agree by the time this runs, so `extension`
/// has a key for every spec.
fn validate_action_field_sets(
    tool: &McpToolDescriptor,
    extension: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), FlavorRegistryError> {
    for spec in tool.action_arg_specs {
        let meta = &extension[spec.action];
        for (key, fields) in [
            ("allowed_fields", spec.allowed_fields),
            ("required_fields", spec.required_fields),
        ] {
            let declared_fields = fields.iter().copied().collect::<BTreeSet<_>>();
            let derived_fields = meta
                .get(key)
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            if declared_fields != derived_fields {
                return Err(FlavorRegistryError::InvalidActionSpecs {
                    name: tool.name,
                    message: format!(
                        "action `{}` declares {key} {declared_fields:?} but the derived schema \
                         says {derived_fields:?}",
                        spec.action
                    ),
                });
            }
        }
    }
    Ok(())
}

pub(crate) fn schema_capability_map(
    bindings: &[SchemaCapabilityTags],
) -> std::collections::HashMap<(SchemaId, SchemaVersion, PayloadKind), BTreeSet<CapabilityTag>> {
    let mut out: std::collections::HashMap<_, BTreeSet<CapabilityTag>> =
        std::collections::HashMap::new();
    for binding in bindings {
        out.entry((
            binding.schema_id.clone(),
            binding.schema_version,
            binding.kind,
        ))
        .or_default()
        .extend(binding.tags.iter().cloned());
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::SearchProjectionColumnKind;
    use crate::flavor::contract::{
        DbConstraint, EmbeddingRecipe, EraseRule, ExportRule, FlavorContract, ForgetRule, KeyShape,
        LanguagePolicy, ProjectionDecl, Provenance, ResourceContract, SchemaContract, SchemaRef,
        SearchProjectionDecl, SubstringArm, Surface, ToolContract, TransferRule, WeightedField,
    };
    use crate::verbs::schema::{PayloadKind, SchemaInfo};
    use crate::{FlavorRegistry, FlavorRegistryError, SchemaId, SchemaVersion};

    const FIXTURE_FLAVOR: &str = "test-flavor";

    const fn contract(
        ordinal: u16,
        schemas: &'static [SchemaContract],
        tools: &'static [ToolContract],
        resources: &'static [ResourceContract],
    ) -> FlavorContract {
        FlavorContract {
            flavor_id: FIXTURE_FLAVOR,
            ordinal,
            schemas,
            state_surfaces: &[],
            kernel_surfaces: &[],
            tools,
            resources,
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
            projection: ProjectionDecl::None {
                why: "a fixture registry has no schema that is a search surface",
            },
        }
    }

    const fn schema(id: SchemaRef, transfer: TransferRule) -> SchemaContract {
        SchemaContract {
            id,
            kind: PayloadKind::CitedObject,
            sidecar_table: None,
            search: SearchProjectionDecl::None {
                why: "a fixture, not a surface",
            },
            embedding: EmbeddingRecipe::Never {
                why: "a fixture, not a memory",
            },
            transfer,
            provenance: Provenance::None,
            surfaces: &[],
            natural_key_columns: &[],
            special_category: false,
        }
    }

    const RESOURCE: ResourceContract = ResourceContract {
        uri_template: "proxima://fixture",
        path: "fixture",
        name: "proxima-fixture",
        title: "Fixture",
        description: "a resource no flavor may declare",
        scope_key: "resource:fixture",
        is_template: false,
        read_only: true,
        reads: &[],
    };

    /// Nothing declared at all: used for the two cases the *registrations*
    /// have to disagree with.
    static EMPTY: FlavorContract = contract(7, &[], &[], &[]);
    static DUPLICATE_ORDINAL: FlavorContract = contract(0, &[], &[], &[]);
    static DECLARES_A_RESOURCE: FlavorContract = contract(7, &[], &[], &[RESOURCE]);
    static FOREIGN_SCHEMA: FlavorContract = contract(
        7,
        &[schema(
            SchemaRef::new("some-other-flavor", "thing", 1),
            TransferRule::StaysOnKey,
        )],
        &[],
        &[],
    );
    static UNENFORCED_REFUSAL: FlavorContract = contract(
        7,
        &[schema(
            SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
            TransferRule::NotTransferable {
                why: "says so and nothing else",
                enforced_by: &[],
            },
        )],
        &[],
        &[],
    );
    static UNREGISTERED_SCHEMA: FlavorContract = contract(
        7,
        &[schema(
            SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
            TransferRule::StaysOnKey,
        )],
        &[],
        &[],
    );
    /// A citation payload that declared a table of its own.
    ///
    /// The shared-blob dedupe arm repoints a citation at a new `blob` row,
    /// and finds the columns to repoint by following foreign keys. This
    /// table's `cited_object_id` would hold a `blob_id` with no FK saying
    /// so, so the remap would walk past it and leave the rows pointing at
    /// the wrong owner's blob after a transfer.
    static CITATION_WITH_A_SIDECAR: FlavorContract = contract(
        7,
        &[SchemaContract {
            id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
            kind: PayloadKind::CitationMapping,
            sidecar_table: Some("test_flavor.thing_v1"),
            search: SearchProjectionDecl::None {
                why: "a fixture, not a surface",
            },
            embedding: EmbeddingRecipe::Never {
                why: "a fixture, not a memory",
            },
            transfer: TransferRule::StaysOnKey,
            provenance: Provenance::None,
            surfaces: &[],
            natural_key_columns: &[],
            special_category: false,
        }],
        &[],
        &[],
    );
    /// The shape a fixture surface takes when the fixture is about the
    /// erase: exportable and owned, so nothing but the erase rule is under
    /// test.
    const fn state_surface(
        table: &'static str,
        key: KeyShape,
        erase: EraseRule,
        export: ExportRule,
    ) -> Surface {
        Surface {
            table,
            key,
            owner_columns: &["owner_id"],
            transfer: TransferRule::StaysOnKey,
            erase,
            export,
            forget: ForgetRule::Keep {
                why: "a fixture, not a memory",
            },
            lexical_language_column: None,
            counter: None,
            completeness: None,
        }
    }

    /// `erase_fixture`'s twin for the transfer partition: the surfaces are
    /// declared non-erasing so the erase check cannot fire first and mask
    /// the case under test.
    const fn transfer_fixture(
        surfaces: &'static [Surface],
        bespoke: &'static [&'static str],
    ) -> FlavorContract {
        FlavorContract {
            flavor_id: FIXTURE_FLAVOR,
            ordinal: 7,
            schemas: &[],
            state_surfaces: surfaces,
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: bespoke,
            projection: ProjectionDecl::None {
                why: "a fixture registry has no schema that is a search surface",
            },
        }
    }

    const fn erase_fixture(
        surfaces: &'static [Surface],
        bespoke: &'static [&'static str],
    ) -> FlavorContract {
        FlavorContract {
            flavor_id: FIXTURE_FLAVOR,
            ordinal: 7,
            schemas: &[],
            state_surfaces: surfaces,
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            bespoke_erase_legs: bespoke,
            bespoke_transfer_legs: &[],
            projection: ProjectionDecl::None {
                why: "a fixture registry has no schema that is a search surface",
            },
        }
    }

    /// The out-of-tree flavor this whole check exists for: `ByKey` on a key
    /// the erase builds no selection set for, claimed by no bespoke leg.
    /// Both generic loops skip it, so nothing deletes these rows and the
    /// erase still reports `Completed`.
    static UNDELETABLE_SURFACE: FlavorContract = erase_fixture(
        &[state_surface(
            "test_flavor.thing_v1",
            KeyShape::Custom(&["thing_id"]),
            EraseRule::ByKey,
            ExportRule::Rows,
        )],
        &[],
    );

    /// A bespoke leg claiming a table the flavor does not declare — the
    /// stale name that let the hand-written lists rot.
    static BESPOKE_LEG_FOR_NOTHING: FlavorContract = erase_fixture(
        &[state_surface(
            "test_flavor.thing_v1",
            KeyShape::MemoryT { column: "t" },
            EraseRule::ByKey,
            ExportRule::Rows,
        )],
        &["test_flavor.gone_v1"],
    );

    /// A `Follow` surface whose transfer no leg can perform, and the whole
    /// reason the transfer partition exists.
    ///
    /// Keyed on a `Custom` column the transfer builds no `t` set for, and
    /// claimed by no bespoke leg. Before Phase 4 this froze cleanly, booted
    /// cleanly, and its rows stayed with the SOURCE owner after every
    /// memory that referenced them moved — which is not a stale row, it is
    /// a cross-tenant read arrived at by silence.
    static UNMOVABLE_SURFACE: FlavorContract = transfer_fixture(
        &[Surface {
            table: "test_flavor.thing_v1",
            key: KeyShape::Custom(&["thing_id"]),
            owner_columns: &["owner_id"],
            transfer: TransferRule::Follow,
            erase: EraseRule::Never {
                why: "the transfer rule is what this fixture is about",
            },
            export: ExportRule::Excluded {
                why: "the transfer rule is what this fixture is about",
            },
            forget: ForgetRule::Keep {
                why: "a fixture, not a memory",
            },
            lexical_language_column: None,
            counter: None,
            completeness: None,
        }],
        &[],
    );

    /// `Follow` with no owner column to set. The rows are reached through
    /// their key's owner, which is what `StaysOnKey` says and what an empty
    /// `owner_columns` claims; declaring `Follow` over it asks for an
    /// `UPDATE` with an empty `SET`.
    static FOLLOW_WITH_NOTHING_TO_SET: FlavorContract = transfer_fixture(
        &[Surface {
            table: "test_flavor.thing_v1",
            key: KeyShape::MemoryT { column: "t" },
            owner_columns: &[],
            transfer: TransferRule::Follow,
            erase: EraseRule::Never {
                why: "the transfer rule is what this fixture is about",
            },
            export: ExportRule::Excluded {
                why: "the transfer rule is what this fixture is about",
            },
            forget: ForgetRule::Keep {
                why: "a fixture, not a memory",
            },
            lexical_language_column: None,
            counter: None,
            completeness: None,
        }],
        &[],
    );

    /// A bespoke transfer leg naming a table the flavor does not declare.
    static BESPOKE_TRANSFER_LEG_FOR_NOTHING: FlavorContract = transfer_fixture(
        &[state_surface(
            "test_flavor.thing_v1",
            KeyShape::MemoryT { column: "t" },
            EraseRule::Never {
                why: "the transfer rule is what this fixture is about",
            },
            ExportRule::Excluded {
                why: "the transfer rule is what this fixture is about",
            },
        )],
        &["test_flavor.gone_v1"],
    );

    /// A flavor arguing with itself: `StaysOnKey` says nothing moves, and
    /// the exemption list says a hand-written statement moves it.
    static BESPOKE_TRANSFER_LEG_OVER_A_NON_MOVE: FlavorContract = transfer_fixture(
        &[state_surface(
            "test_flavor.thing_v1",
            KeyShape::MemoryT { column: "t" },
            EraseRule::Never {
                why: "the transfer rule is what this fixture is about",
            },
            ExportRule::Excluded {
                why: "the transfer rule is what this fixture is about",
            },
        )],
        &["test_flavor.thing_v1"],
    );

    /// A flavor arguing with itself: the declaration says a constraint
    /// removes the rows, and the exemption list says a hand-written
    /// statement does.
    static BESPOKE_LEG_OVER_A_CASCADE: FlavorContract = erase_fixture(
        &[state_surface(
            "test_flavor.thing_v1",
            KeyShape::MemoryT { column: "t" },
            EraseRule::Cascade {
                via: DbConstraint {
                    relation: "test_flavor.thing_v1",
                    name: "thing_v1_t_fkey",
                },
            },
            ExportRule::Rows,
        )],
        &["test_flavor.thing_v1"],
    );

    /// Exportable while carrying neither an owner column nor a key with a
    /// home table: the generator has no statement that reaches it from the
    /// owner, so it would go missing from every bundle in silence.
    static UNREACHABLE_EXPORT_SURFACE: FlavorContract = erase_fixture(
        &[Surface {
            table: "test_flavor.thing_v1",
            key: KeyShape::Custom(&["thing_id"]),
            owner_columns: &[],
            transfer: TransferRule::StaysOnKey,
            erase: EraseRule::Never {
                why: "the export rule is what this fixture is about",
            },
            export: ExportRule::Rows,
            forget: ForgetRule::Keep {
                why: "a fixture, not a memory",
            },
            lexical_language_column: None,
            counter: None,
            completeness: None,
        }],
        &[],
    );

    /// A `PerRow` policy naming a column that is not the projection
    /// table's. The generator emits one language column per projection
    /// table and names it `lexical_language`; a second name is a
    /// declaration nothing renders.
    static PER_ROW_ON_THE_WRONG_COLUMN: FlavorContract = FlavorContract {
        flavor_id: FIXTURE_FLAVOR,
        ordinal: 7,
        schemas: &[SchemaContract {
            // A Fact, not a citation payload: a citation schema declaring a
            // sidecar trips CitationSidecarNotRemappable first and this
            // fixture would test that instead.
            id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
            kind: PayloadKind::Fact,
            sidecar_table: Some("test_flavor.thing_v1"),
            search: SearchProjectionDecl::Projected {
                fields: &[WeightedField {
                    column: "a",
                    kind: SearchProjectionColumnKind::Text,
                    weight: 1.0,
                }],
                tag_column: None,
                language: LanguagePolicy::PerRow {
                    column: "row_config",
                },
                bands: &[],
                substring: SubstringArm::Off,
            },
            embedding: EmbeddingRecipe::Never {
                why: "a fixture, not a memory",
            },
            transfer: TransferRule::StaysOnKey,
            provenance: Provenance::None,
            surfaces: &[],
            natural_key_columns: &[],
            special_category: false,
        }],
        state_surfaces: &[],
        kernel_surfaces: &[],
        tools: &[],
        resources: &[],
        bespoke_erase_legs: &[],
        bespoke_transfer_legs: &[],
        projection: ProjectionDecl::Table(crate::flavor::contract::ProjectionSpec {
            table: "test_flavor.projection",
            index: "test_flavor_projection_owner_tsv_gin",
            overfetch_k: 0,
            band_comparability: crate::flavor::contract::BandComparability::CoreBands,
            // Sidecar-ranked so this fixture tests ONE rule. Under
            // `Projection` the empty band set would trip
            // `ProjectionBandName` as well, and a fixture that can fail two
            // ways proves neither.
            rank_source: crate::flavor::contract::RankSource::SidecarWithProjectionOwner {
                why: "a fixture, not a search surface",
            },
        }),
    };
    /// One projected schema for a uniformity fixture, differing from its
    /// twin in exactly ONE property.
    ///
    /// `validate_contract_projection` checks three properties in order —
    /// language, bands, weight array — and returns on the first. A fixture
    /// that differs in two of them proves only the earlier one, which is why
    /// all three fixtures below are built from this one function with a
    /// single argument changed.
    const fn uniformity_schema(
        name: &'static str,
        table: &'static str,
        fields: &'static [WeightedField],
        language: LanguagePolicy,
        bands: &'static [crate::flavor::contract::Band],
    ) -> SchemaContract {
        SchemaContract {
            id: SchemaRef::new(FIXTURE_FLAVOR, name, 1),
            kind: PayloadKind::Fact,
            sidecar_table: Some(table),
            search: SearchProjectionDecl::Projected {
                fields,
                tag_column: None,
                language,
                bands,
                substring: SubstringArm::Off,
            },
            embedding: EmbeddingRecipe::Never {
                why: "a fixture, not a memory",
            },
            transfer: TransferRule::StaysOnKey,
            provenance: Provenance::None,
            surfaces: &[],
            natural_key_columns: &[],
            special_category: false,
        }
    }

    /// The `RankSource::Projection` wrapper the three uniformity fixtures
    /// share: one statement serves the whole flavor, which is what makes
    /// disagreement between its schemas a boot refusal.
    const fn uniformity_contract(schemas: &'static [SchemaContract]) -> FlavorContract {
        FlavorContract {
            flavor_id: FIXTURE_FLAVOR,
            ordinal: 7,
            schemas,
            state_surfaces: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
            projection: ProjectionDecl::Table(crate::flavor::contract::ProjectionSpec {
                table: "test_flavor.projection",
                index: "test_flavor_projection_owner_tsv_gin",
                overfetch_k: 0,
                band_comparability: crate::flavor::contract::BandComparability::CoreBands,
                rank_source: crate::flavor::contract::RankSource::Projection,
            }),
        }
    }

    /// One weight level: no array at all.
    static ONE_LEVEL: &[WeightedField] = &[WeightedField {
        column: "a",
        kind: SearchProjectionColumnKind::Text,
        weight: 1.0,
    }];

    /// Two levels: an array [`ONE_LEVEL`] does not have.
    static TWO_LEVELS: &[WeightedField] = &[
        WeightedField {
            column: "a",
            kind: SearchProjectionColumnKind::Text,
            weight: 1.0,
        },
        WeightedField {
            column: "b",
            kind: SearchProjectionColumnKind::Text,
            weight: 2.0,
        },
    ];

    /// Two projected schemas under one `RankSource::Projection` flavor
    /// agreeing on language and bands and DISAGREEING on weight levels.
    ///
    /// One statement serves both, and it reads the weight array off the
    /// first participating schema — so without this check the second
    /// schema's vector would be ranked with an array that describes a
    /// document it is not scoring.
    static WEIGHTS_NOT_UNIFORM: FlavorContract = uniformity_contract(&[
        uniformity_schema(
            "thing",
            "test_flavor.thing_v1",
            ONE_LEVEL,
            LanguagePolicy::Pinned("simple"),
            FIXTURE_BANDS,
        ),
        uniformity_schema(
            "other",
            "test_flavor.other_v1",
            TWO_LEVELS,
            LanguagePolicy::Pinned("simple"),
            FIXTURE_BANDS,
        ),
    ]);

    /// …and disagreeing on the LEXICAL CONFIGURATION, which one statement
    /// can spell exactly once.
    ///
    /// Without its own fixture this arm was decorative: deleting it left the
    /// whole workspace green, because the weight fixture agreed on language
    /// and never reached it.
    static LANGUAGE_NOT_UNIFORM: FlavorContract = uniformity_contract(&[
        uniformity_schema(
            "thing",
            "test_flavor.thing_v1",
            ONE_LEVEL,
            LanguagePolicy::Pinned("simple"),
            FIXTURE_BANDS,
        ),
        uniformity_schema(
            "other",
            "test_flavor.other_v1",
            ONE_LEVEL,
            LanguagePolicy::Pinned("english"),
            FIXTURE_BANDS,
        ),
    ]);

    /// …and disagreeing on the SCORE WINDOWS, which is what makes two
    /// schemas' scores comparable inside one page.
    ///
    /// [`SHIFTED_BANDS`] carries the same three names in the same order, so
    /// the band-NAME rule cannot fire and only the uniformity arm is left.
    static BANDS_NOT_UNIFORM: FlavorContract = uniformity_contract(&[
        uniformity_schema(
            "thing",
            "test_flavor.thing_v1",
            ONE_LEVEL,
            LanguagePolicy::Pinned("simple"),
            FIXTURE_BANDS,
        ),
        uniformity_schema(
            "other",
            "test_flavor.other_v1",
            ONE_LEVEL,
            LanguagePolicy::Pinned("simple"),
            SHIFTED_BANDS,
        ),
    ]);

    /// Core's own windows, which is what makes `CoreBands` above legal and
    /// keeps the fixtures from tripping the band-name rule.
    static FIXTURE_BANDS: &[crate::flavor::contract::Band] = &[
        crate::flavor::flavor0::BAND_EXACT,
        crate::flavor::flavor0::BAND_RESCUE,
        crate::flavor::flavor0::BAND_SUBSTRING,
    ];

    /// The same three names inside `[0, 1]`, at a different exact floor.
    /// Staying inside core's window is the point: a band that left it would
    /// trip `ProjectionBandOutsideCoreWindow` instead.
    static SHIFTED_BANDS: &[crate::flavor::contract::Band] = &[
        crate::flavor::contract::Band {
            name: crate::flavor::contract::BAND_NAME_EXACT,
            floor: 0.60,
            ceiling: 1.00,
            normalization: crate::flavor::flavor0::BAND_EXACT.normalization,
        },
        crate::flavor::flavor0::BAND_RESCUE,
        crate::flavor::flavor0::BAND_SUBSTRING,
    ];

    /// Five distinct relative weights on one projection unit. The
    /// declaration is free of `PostgreSQL`'s four-class limit right up to
    /// the moment the generator has to emit `setweight`, and this is that
    /// moment.
    static TOO_MANY_WEIGHT_LEVELS: FlavorContract = contract(
        7,
        &[SchemaContract {
            id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
            kind: PayloadKind::CitedObject,
            sidecar_table: Some("test_flavor.thing_v1"),
            search: SearchProjectionDecl::Projected {
                fields: &[
                    WeightedField {
                        column: "a",
                        kind: SearchProjectionColumnKind::Text,
                        weight: 5.0,
                    },
                    WeightedField {
                        column: "b",
                        kind: SearchProjectionColumnKind::Text,
                        weight: 4.0,
                    },
                    WeightedField {
                        column: "c",
                        kind: SearchProjectionColumnKind::Text,
                        weight: 3.0,
                    },
                    WeightedField {
                        column: "d",
                        kind: SearchProjectionColumnKind::Text,
                        weight: 2.0,
                    },
                    WeightedField {
                        column: "e",
                        kind: SearchProjectionColumnKind::Text,
                        weight: 1.0,
                    },
                ],
                tag_column: None,
                language: LanguagePolicy::Pinned("simple"),
                bands: &[],
                substring: SubstringArm::Off,
            },
            embedding: EmbeddingRecipe::Never {
                why: "a fixture, not a memory",
            },
            transfer: TransferRule::StaysOnKey,
            provenance: Provenance::None,
            surfaces: &[],
            natural_key_columns: &[],
            special_category: false,
        }],
        &[],
        &[],
    );

    static UNREGISTERED_TOOL: FlavorContract = contract(
        7,
        &[],
        &[ToolContract {
            wire_name: "test_flavor_absent",
            actions: &[],
            idempotent: true,
        }],
        &[],
    );

    /// A fixture's ingress. Never called: the registries below are built to
    /// be REFUSED, so nothing reaches a payload parser.
    fn fixture_ingress(
        _payload: &serde_json::Value,
    ) -> Result<crate::verbs::schema::ProtocolPayload, String> {
        Err("a fixture, not an ingress".to_owned())
    }

    /// Register a fixture contract's schema, the way the typed
    /// `add_*_schema` methods would.
    ///
    /// `validate_contract_schemas` runs BEFORE
    /// `validate_contract_projection`, so a fixture whose subject is a
    /// PROJECTION rule has to be registered to reach it. Registering is two
    /// lines; the alternative — reordering the two validators — changes
    /// which error every genuinely broken contract reports at boot, which is
    /// a production behaviour change bought for a test's convenience.
    ///
    /// A typed registration needs its ingress entry too, or
    /// `SchemaIngressMismatch` fires first and the fixture proves that
    /// instead.
    fn register_fixture_schema(registry: &mut FlavorRegistry, name: &str, table: &str) {
        let schema_id = SchemaId::new(format!("{FIXTURE_FLAVOR}/{name}-v1"));
        let schema_version = SchemaVersion::new(1);
        registry.schemas.push(SchemaInfo {
            schema_id: schema_id.clone(),
            schema_version,
            kind: PayloadKind::Fact,
            filter_keys: Vec::new(),
            sidecar_table: Some(table.to_owned()),
            natural_key_columns: Vec::new(),
            tombstone: None,
            has_typed_ingress: true,
            cited_object_schema: None,
            embeddable: true,
        });
        registry
            .protocol_ingress
            .push(crate::verbs::schema::ProtocolPayloadIngressEntry {
                schema_id,
                schema_version,
                kind: PayloadKind::Fact,
                ingress: fixture_ingress,
                json_schema: None,
            });
    }

    /// The two schemas every uniformity fixture declares.
    fn register_uniformity_schemas(registry: &mut FlavorRegistry) {
        register_fixture_schema(registry, "thing", "test_flavor.thing_v1");
        register_fixture_schema(registry, "other", "test_flavor.other_v1");
    }

    /// A registration with no contract entry: consistent enough to reach
    /// `validate_contracts` (opaque kinds are allowed to have no typed
    /// ingress, and it declares no ingress entry to mismatch).
    fn registration_without_a_contract() -> SchemaInfo {
        SchemaInfo {
            schema_id: SchemaId::new(format!("{FIXTURE_FLAVOR}/thing-v1")),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitedObject,
            filter_keys: Vec::new(),
            sidecar_table: None,
            natural_key_columns: Vec::new(),
            tombstone: None,
            has_typed_ingress: false,
            cited_object_schema: None,
            embeddable: true,
        }
    }

    /// Every contract cross-check, each with a registry shaped to trip it
    /// and nothing else.
    ///
    /// These are the checks that make "everything is a flavor" structural.
    /// An unpinned check is one a refactor can delete without a single test
    /// going red — and the resource rejection in particular is the whole of
    /// the resources-are-substrate ruling, enforced in five lines.
    #[test]
    // One line per cross-check plus its fixture reference. Splitting it
    // would put half the checks in a second function to forget one in.
    #[allow(clippy::too_many_lines)]
    fn each_contract_cross_check_rejects_its_own_shape() {
        #[allow(clippy::type_complexity)]
        let cases: Vec<(
            &'static str,
            fn(&mut FlavorRegistry),
            fn(&FlavorRegistryError) -> bool,
        )> = vec![
            (
                "a flavor other than #0 declares a proxima:// resource",
                |registry| registry.contracts.push(&DECLARES_A_RESOURCE),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::ResourcesNotPermitted { flavor_id }
                            if *flavor_id == FIXTURE_FLAVOR
                    )
                },
            ),
            (
                "two contracts claim the same ordinal",
                |registry| registry.contracts.push(&DUPLICATE_ORDINAL),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::DuplicateFlavorOrdinal { ordinal: 0, .. }
                    )
                },
            ),
            (
                "contracts were registered but core's is not among them",
                |registry| {
                    registry.contracts.clear();
                    registry.contracts.push(&EMPTY);
                },
                |err| matches!(err, FlavorRegistryError::MissingCoreContract),
            ),
            (
                "a contract entry's schema id carries another flavor's prefix",
                |registry| registry.contracts.push(&FOREIGN_SCHEMA),
                |err| matches!(err, FlavorRegistryError::ContractSchemaPrefix { .. }),
            ),
            (
                "a NotTransferable schema names no enforcement site",
                |registry| registry.contracts.push(&UNENFORCED_REFUSAL),
                |err| matches!(err, FlavorRegistryError::UnenforcedTransferRefusal { .. }),
            ),
            (
                "the contract declares a schema nothing registered",
                |registry| registry.contracts.push(&UNREGISTERED_SCHEMA),
                |err| matches!(err, FlavorRegistryError::ContractSchemaNotRegistered { .. }),
            ),
            (
                "a schema was registered under a flavor that does not declare it",
                |registry| {
                    registry.schemas.push(registration_without_a_contract());
                    registry.contracts.push(&EMPTY);
                },
                |err| matches!(err, FlavorRegistryError::SchemaWithoutContract { .. }),
            ),
            (
                "one projection unit declares more weight levels than PG has classes",
                |registry| registry.contracts.push(&TOO_MANY_WEIGHT_LEVELS),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::ProjectionWeightLevels {
                            levels: 5,
                            classes: 4,
                            ..
                        }
                    )
                },
            ),
            (
                "a citation payload declares a sidecar the blob remap cannot reach",
                |registry| registry.contracts.push(&CITATION_WITH_A_SIDECAR),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::CitationSidecarNotRemappable {
                            table: "test_flavor.thing_v1",
                            ..
                        }
                    )
                },
            ),
            (
                "a PerRow policy names a column the projection table does not have",
                |registry| registry.contracts.push(&PER_ROW_ON_THE_WRONG_COLUMN),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::ProjectionLanguageColumn {
                            declared: "row_config",
                            projection_column: Some("lexical_language"),
                            ..
                        }
                    )
                },
            ),
            (
                "two projection-ranked schemas disagree about the lexical configuration",
                |registry| {
                    register_uniformity_schemas(registry);
                    registry.contracts.push(&LANGUAGE_NOT_UNIFORM);
                },
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::ProjectionRenderNotUniform {
                            property: "language",
                            ..
                        }
                    )
                },
            ),
            (
                "two projection-ranked schemas disagree about the score windows",
                |registry| {
                    register_uniformity_schemas(registry);
                    registry.contracts.push(&BANDS_NOT_UNIFORM);
                },
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::ProjectionRenderNotUniform {
                            property: "bands",
                            ..
                        }
                    )
                },
            ),
            (
                "two projection-ranked schemas disagree about the ts_rank weight array",
                |registry| {
                    register_uniformity_schemas(registry);
                    registry.contracts.push(&WEIGHTS_NOT_UNIFORM);
                },
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::ProjectionRenderNotUniform {
                            property: "rank_weights",
                            ..
                        }
                    )
                },
            ),
            (
                "a surface declares an erase no leg can perform",
                |registry| registry.contracts.push(&UNDELETABLE_SURFACE),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::UndeletableSurface {
                            table: "test_flavor.thing_v1",
                            ..
                        }
                    )
                },
            ),
            (
                "a surface declares a transfer no leg can perform",
                |registry| registry.contracts.push(&UNMOVABLE_SURFACE),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::UnmovableSurface {
                            table: "test_flavor.thing_v1",
                            ..
                        }
                    )
                },
            ),
            (
                "a surface declares Follow with no owner column to set",
                |registry| registry.contracts.push(&FOLLOW_WITH_NOTHING_TO_SET),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::UnmovableSurface {
                            table: "test_flavor.thing_v1",
                            ..
                        }
                    )
                },
            ),
            (
                "a bespoke transfer leg names a table the flavor does not declare",
                |registry| registry.contracts.push(&BESPOKE_TRANSFER_LEG_FOR_NOTHING),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::BespokeTransferLegMismatch {
                            table: "test_flavor.gone_v1",
                            ..
                        }
                    )
                },
            ),
            (
                "a bespoke transfer leg claims a surface nothing moves",
                |registry| {
                    registry
                        .contracts
                        .push(&BESPOKE_TRANSFER_LEG_OVER_A_NON_MOVE);
                },
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::BespokeTransferLegMismatch {
                            table: "test_flavor.thing_v1",
                            ..
                        }
                    )
                },
            ),
            (
                "a bespoke erase leg names a table the flavor does not declare",
                |registry| registry.contracts.push(&BESPOKE_LEG_FOR_NOTHING),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::BespokeEraseLegMismatch {
                            table: "test_flavor.gone_v1",
                            ..
                        }
                    )
                },
            ),
            (
                "a bespoke erase leg claims a surface a constraint removes",
                |registry| registry.contracts.push(&BESPOKE_LEG_OVER_A_CASCADE),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::BespokeEraseLegMismatch {
                            table: "test_flavor.thing_v1",
                            ..
                        }
                    )
                },
            ),
            (
                "an exportable surface has neither an owner column nor a key with a home",
                |registry| registry.contracts.push(&UNREACHABLE_EXPORT_SURFACE),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::UnreachableExportSurface {
                            table: "test_flavor.thing_v1",
                            ..
                        }
                    )
                },
            ),
            (
                "the contract names an MCP tool nothing registered",
                |registry| registry.contracts.push(&UNREGISTERED_TOOL),
                |err| {
                    matches!(
                        err,
                        FlavorRegistryError::ContractToolNotRegistered { name, .. }
                            if *name == "test_flavor_absent"
                    )
                },
            ),
        ];

        for (shape, break_it, expected) in cases {
            let mut registry = FlavorRegistry::new();
            break_it(&mut registry);
            let Err(err) = registry.try_freeze() else {
                panic!("freeze accepted a registry where {shape}");
            };
            assert!(expected(&err), "{shape}: freeze reported {err} instead");
        }
    }

    /// The counterpart: the registry as the binary actually composes it,
    /// with core's contract in place, freezes.
    #[test]
    fn the_shipped_registry_freezes() {
        assert!(FlavorRegistry::new().try_freeze().is_ok());
    }
}
