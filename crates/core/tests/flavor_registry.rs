use proxima_core::flavor::FlavorRegistry;
use proxima_core::verbs::schema::PayloadKind;

#[test]
fn wake_trace_schemas_are_registered_in_core_flavor() {
    let frozen = FlavorRegistry::default().freeze();
    let schemas = frozen.list();

    let has = |id: &str, kind: PayloadKind| {
        schemas
            .iter()
            .any(|s| s.schema_id.as_str() == id && s.kind == kind)
    };

    assert!(has("proxima-core/wake-trace-v1", PayloadKind::Fact));
    assert!(has(
        "proxima-core/wake-trace-jsonl-v1",
        PayloadKind::CitedObject
    ));
    assert!(has(
        "proxima-core/uploaded-blob-v1",
        PayloadKind::CitedObject
    ));
    assert!(has(
        "proxima-core/wake-trace-citation-v1",
        PayloadKind::CitationMapping
    ));
}
