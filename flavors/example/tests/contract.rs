//! The example flavor's contract, without Postgres.

use proxima::composed_schema_names;
use proxima::flavor::{FlavorBundle, FlavorRegistry, Tool, ToolEffect};
use proxima_example::{EXAMPLE_CONTRACT, ExampleFlavor, ListNotebooksTool};

fn frozen() -> proxima::FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    ExampleFlavor::register(&mut registry).expect("the example flavor registers");
    registry.try_freeze().expect("and the registry freezes")
}

#[test]
fn the_registry_freezes_with_the_bundle() {
    let registry = frozen();
    assert!(
        registry
            .contracts()
            .iter()
            .any(|contract| contract.flavor_id == "proxima-example"),
        "the frozen registry carries the example contract"
    );
}

#[test]
fn the_composed_schemas_are_core_and_the_example() {
    assert_eq!(
        composed_schema_names(&frozen()),
        ["proxima_core", "proxima_example"]
    );
}

#[test]
fn the_ordinal_stays_clear_of_the_reserved_ones() {
    assert!(!(1..=5).contains(&EXAMPLE_CONTRACT.ordinal));
    assert_eq!(EXAMPLE_CONTRACT.ordinal, 11);
}

#[test]
fn the_one_tool_is_list_notebooks_and_it_reads() {
    let tools: Vec<&str> = EXAMPLE_CONTRACT
        .tools
        .iter()
        .map(|tool| tool.wire_name)
        .collect();
    assert_eq!(tools, ["proxima-example_list_notebooks"]);
    assert_eq!(ListNotebooksTool::NAME, tools[0]);
    assert_eq!(ListNotebooksTool::EFFECT, Some(ToolEffect::ReadOnly));
}

#[test]
fn the_installer_section_is_one_call_then_the_census() {
    let calls = proxima_example::installer_calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].contains("install_owner_rls('proxima_example'"));
    assert!(calls[1].contains("assert_owner_rls_census('proxima_example')"));
    assert_eq!(
        proxima_example::baseline_sql_with_owner_rls(&calls),
        proxima_example::BASELINE_SQL
            .replace("-- owner-rls: begin\n", "")
            .replace("-- owner-rls: end\n", "")
    );
    assert!(!proxima_example::baseline_sql_without_installer().contains("install_owner_rls"));
}
