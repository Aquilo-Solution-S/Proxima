//! The search-projection half of the contract cross-checks: that a declared
//! projection is internally uniform, and that a query can actually reach it.
//!
//! Split from [`super::contracts`] because these two checks are the only
//! ones that reason about the merged projection statement rather than about
//! one flavor's declaration in isolation — uniformity is a property of what
//! every flavor contributes together, and reachability is a property of the
//! path a query takes to the rows.

use super::{FlavorRegistry, FlavorRegistryError};

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
    /// A non-core search projection against the query shapes that can
    /// reach it.
    ///
    /// A projection is not free: every write to the schema pays a
    /// projection row and a GIN index entry. `core_search_flavors`
    /// (`storage-pg/src/verbs/query/search.rs`) admits a NON-core
    /// projection only through a tag-filtered request — an unscoped search
    /// stays on flavor #0's sidecars — and only when the projection
    /// declares a `tag_column`, its flavor declares
    /// [`BandComparability::CoreBands`], and its flavor declares
    /// [`RankSource::Projection`]. Fail any of those and no request shape
    /// reaches the rows: the corpus is written, indexed, and unscannable.
    ///
    /// The `RankSource` gate is what makes this checkable rather than
    /// presumptuous. `SidecarWithProjectionOwner` is the declared statement
    /// that core's renderer does NOT serve this flavor — it ships its own
    /// tools, which reach the projection themselves — so its declaration
    /// says nothing about reachability and nothing here may judge it. Only
    /// a flavor that claimed core's renderer is held to what core's
    /// renderer will scan.
    ///
    /// Flavor #0 is exempt because it is the corpus every unscoped search
    /// already scans; a `tag_column` narrows its rows rather than admitting
    /// them.
    pub(super) fn validate_projection_is_reachable(
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        use crate::flavor::contract::BandComparability;

        if contract.is_core() {
            return Ok(());
        }
        let Some(spec) = contract.projection.spec() else {
            return Ok(());
        };
        if !spec.rank_source.is_projection() {
            return Ok(());
        }
        for (schema, _) in contract.projected_schemas() {
            let why = if matches!(spec.band_comparability, BandComparability::Divergent { .. }) {
                "its flavor claims BandComparability::Divergent, and the merge admits a \
                 non-core projection only under CoreBands"
            } else if schema.search.tag_column().is_none() {
                "it declares no tag_column, and a non-core projection is reached only by a \
                 tag-filtered request — an unscoped search scans flavor #0's sidecars alone"
            } else {
                continue;
            };
            return Err(FlavorRegistryError::UnreachableSearchProjection {
                flavor_id: contract.flavor_id,
                schema_id: schema.schema_id(),
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
    ///    The weight array is checked alongside language and bands because
    ///    the renderer reads all three off the flavor's FIRST participating
    ///    schema. Unchecked, a flavor whose schemas declare different weight
    ///    LEVELS would have one schema's array applied to every schema's
    ///    vector, silently.
    /// 2. `BandComparability::CoreBands` is the claim a cross-flavor merge
    ///    compares scores on. A flavor whose bands leave flavor #0's
    ///    `[0, 1]` window cannot make it.
    /// 3. Every projected schema's sidecar declares a surface keyed on the
    ///    memory `t`. The generator spells the projection's key from that
    ///    column, so a sidecar that declares no surface — or one keyed on
    ///    anything else — has no projection statement to generate.
    pub(super) fn validate_contract_projection(
        contract: &crate::flavor::contract::FlavorContract,
    ) -> Result<(), FlavorRegistryError> {
        use crate::flavor::contract::{
            BAND_NAME_EXACT, BAND_NAME_RESCUE, BAND_NAME_SUBSTRING, BandComparability, KeyShape,
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
        // A projection row is keyed on the memory its sidecar row belongs
        // to, and the generator reads that column off the sidecar's
        // SURFACE — the one declaration that says how the table is keyed.
        // A projected schema whose sidecar declares no surface, or one
        // keyed on anything but the memory `t`, therefore has no statement
        // to generate. Refused here rather than left to the generator,
        // because there it is found twice and late: once on the write side
        // when the DDL is emitted at boot, and again on the read side at
        // the first query that expects rows.
        //
        // Read off `all_surfaces()` rather than the schema's own list: a
        // flavor is free to declare its sidecar among the flavor's state
        // surfaces, and where the declaration sits does not change which
        // column the generator reads.
        for (schema, sidecar_table) in contract.projected_schemas() {
            let keyed_on_memory = contract.all_surfaces().any(|surface| {
                surface.table == sidecar_table && matches!(surface.key, KeyShape::MemoryT { .. })
            });
            if !keyed_on_memory {
                return Err(FlavorRegistryError::ProjectedSidecarNotMemoryKeyed {
                    flavor_id: contract.flavor_id,
                    schema_id: schema.schema_id(),
                    table: sidecar_table,
                });
            }
        }
        Ok(())
    }
}
