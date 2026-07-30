//! The flavor's own schemas must state the bounds their descriptions promise.
//!
//! `crates/core` runs the same check over the substrate registry, but a flavor
//! registers its tools into a registry of its own, so core's suite never sees
//! them. Both call `schema_bound_mismatches` rather than keeping a second copy
//! of the rule -- `proxima-code_search_chunks.limit` and its four siblings were
//! among the parameters that promised `0 is rejected` while emitting
//! `minimum: 0`, and `proxima-code_emit_execution_request`'s three text fields
//! promised character caps no client could see. Neither set appears in the
//! default tool profile, so a check that walks the wire would have missed both.

use proxima_core::FlavorRegistry;

#[test]
fn a_flavor_schema_declares_the_bounds_its_description_promises() {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry).expect("register proxima-code");
    let frozen = registry.freeze_or_panic_for_tests();

    let offenders = proxima_core::mcp::schema_bound_mismatches(&frozen);
    assert!(
        offenders.is_empty(),
        "schema and description disagree about a bound:\n  {}",
        offenders.join("\n  "),
    );
}
