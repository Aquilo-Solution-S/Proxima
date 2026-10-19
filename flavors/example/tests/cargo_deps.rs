//! #474 A2: the example flavor names only `proxima` among workspace crates.

#[test]
fn the_example_depends_on_no_workspace_crate_but_proxima() {
    let manifest = include_str!("../Cargo.toml");
    let offenders: Vec<&str> = manifest
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| name.starts_with("proxima-") && *name != "proxima-example")
        .collect();
    assert!(
        offenders.is_empty(),
        "flavors/example/Cargo.toml names workspace crates other than proxima: {offenders:?}"
    );
}
