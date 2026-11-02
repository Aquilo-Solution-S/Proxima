//! The example spiice's contract, without Postgres.

use proxiima::composed_schema_names;
use proxiima::spiice::{SpiiceBundle, SpiiceRegistry, Tool, ToolEffect};
use proxiima_example::{EXAMPLE_CONTRACT, ExampleSpiice, ListNotebooksTool};

fn frozen() -> proxiima::SpiiceRegistryFrozen {
    let mut registry = SpiiceRegistry::new();
    ExampleSpiice::register(&mut registry).expect("the example spiice registers");
    registry.try_freeze().expect("and the registry freezes")
}

#[test]
fn the_registry_freezes_with_the_bundle() {
    let registry = frozen();
    assert!(
        registry
            .contracts()
            .iter()
            .any(|contract| contract.spiice_id == "proxiima-example"),
        "the frozen registry carries the example contract"
    );
}

#[test]
fn the_composed_schemas_are_core_and_the_example() {
    assert_eq!(
        composed_schema_names(&frozen()),
        ["proxiima_core", "proxiima_example"]
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
    assert_eq!(tools, ["proxiima-example_list_notebooks"]);
    assert_eq!(ListNotebooksTool::NAME, tools[0]);
    assert_eq!(ListNotebooksTool::EFFECT, Some(ToolEffect::ReadOnly));
}

#[test]
fn the_installer_section_is_one_call_then_the_census() {
    let calls = proxiima_example::installer_calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].contains("install_owner_rls('proxiima_example'"));
    assert!(calls[1].contains("assert_owner_rls_census('proxiima_example')"));
    assert_eq!(
        proxiima_example::baseline_sql_with_owner_rls(&calls),
        proxiima_example::BASELINE_SQL
            .replace("-- owner-rls: begin\n", "")
            .replace("-- owner-rls: end\n", "")
    );
    assert!(!proxiima_example::baseline_sql_without_installer().contains("install_owner_rls"));
}
