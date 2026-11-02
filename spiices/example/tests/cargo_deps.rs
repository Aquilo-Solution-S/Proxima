//! #474 A2: the example spiice names only `proxiima` among workspace crates.
//!
//! The manifest is parsed, not grepped: every dependency table (plain, dev,
//! build, their legacy spellings, and each of those under any `target.<cfg>`)
//! is walked, `workspace = true` entries are resolved through the root
//! `[workspace.dependencies]`, and the effective package (`package = …` if
//! renamed, the key otherwise) and any `path` decide. Any workspace crate other
//! than `proxiima` (or the example itself, as its own dev-dependency) fails.

const MANIFEST: &str = include_str!("../Cargo.toml");
const WORKSPACE_MANIFEST: &str = include_str!("../../../Cargo.toml");

/// Dependency table names Cargo reads, including the legacy underscore spellings.
const DEPENDENCY_TABLES: [&str; 5] = [
    "dependencies",
    "dev-dependencies",
    "build-dependencies",
    "dev_dependencies",
    "build_dependencies",
];

/// The paths an allowed entry may name: `proxiima`, or the example itself.
const ALLOWED_PATHS: [&str; 3] = ["../../crates/proxiima", ".", "crates/proxiima"];

fn collect_dependency_tables<'a>(
    prefix: &str,
    root: &'a toml::Table,
    tables: &mut Vec<(String, &'a toml::Table)>,
) {
    for name in DEPENDENCY_TABLES {
        if let Some(table) = root.get(name).and_then(toml::Value::as_table) {
            tables.push((format!("{prefix}{name}"), table));
        }
    }
}

fn workspace_crate_offenders(manifest: &str, workspace: &str) -> Vec<String> {
    let manifest: toml::Table = manifest.parse().expect("the manifest is TOML");
    let workspace: toml::Table = workspace.parse().expect("the workspace manifest is TOML");
    let inherited = workspace
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(toml::Value::as_table);

    let mut tables: Vec<(String, &toml::Table)> = Vec::new();
    collect_dependency_tables("", &manifest, &mut tables);
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (cfg, target) in targets {
            if let Some(target) = target.as_table() {
                collect_dependency_tables(&format!("target.{cfg}."), target, &mut tables);
            }
        }
    }

    let mut offenders = Vec::new();
    for (table, deps) in tables {
        for (key, value) in deps {
            let mut entry = value;
            let mut from_workspace = false;
            if value.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
                let Some(resolved) = inherited.and_then(|deps| deps.get(key)) else {
                    offenders.push(format!(
                        "{table}.{key}: workspace = true but no [workspace.dependencies] entry"
                    ));
                    continue;
                };
                entry = resolved;
                from_workspace = true;
            }
            let package = entry
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(key)
                .replace('_', "-");
            let path = entry.get("path").and_then(toml::Value::as_str);
            let package_offends = package.starts_with("proxiima")
                && package != "proxiima"
                && package != "proxiima-example";
            let path_offends = path.is_some_and(|path| !ALLOWED_PATHS.contains(&path));
            if package_offends || path_offends {
                let origin = if from_workspace { " (workspace)" } else { "" };
                offenders.push(format!(
                    "{table}.{key}{origin}: package {package}, path {path:?}"
                ));
            }
        }
    }
    offenders
}

const EMPTY_WORKSPACE: &str = "[workspace]\n[workspace.dependencies]\n";

fn assert_refused(manifest: &str, workspace: &str) {
    let offenders = workspace_crate_offenders(manifest, workspace);
    assert!(
        !offenders.is_empty(),
        "the guardrail admitted a workspace crate other than proxiima:\n{manifest}"
    );
}

#[test]
fn the_example_depends_on_no_workspace_crate_but_proxiima() {
    let offenders = workspace_crate_offenders(MANIFEST, WORKSPACE_MANIFEST);
    assert!(
        offenders.is_empty(),
        "spiices/example/Cargo.toml names workspace crates other than proxiima: {offenders:?}"
    );
}

#[test]
fn table_form_dependency_is_refused() {
    assert_refused(
        "[dependencies.proxiima-core]\npath = \"../../crates/core\"\n",
        EMPTY_WORKSPACE,
    );
}

#[test]
fn target_table_form_dependency_is_refused() {
    assert_refused(
        "[target.'cfg(unix)'.dependencies.proxiima-core]\npath = \"../../crates/core\"\n",
        EMPTY_WORKSPACE,
    );
}

#[test]
fn package_rename_is_refused() {
    assert_refused(
        "[dependencies]\ncore = { package = \"proxiima-core\", path = \"../../crates/core\" }\n",
        EMPTY_WORKSPACE,
    );
}

#[test]
fn table_form_package_rename_is_refused() {
    assert_refused(
        "[dev-dependencies.core]\npackage = \"proxiima-core\"\n",
        EMPTY_WORKSPACE,
    );
}

#[test]
fn workspace_inherited_rename_is_refused() {
    assert_refused(
        "[dependencies]\ncore = { workspace = true }\n",
        "[workspace]\n[workspace.dependencies]\n\
         core = { package = \"proxiima-core\", path = \"crates/core\" }\n",
    );
}

/// A workspace crate is refused by its path even when its name does not
/// start with `proxiima` (a future crate named otherwise).
#[test]
fn path_to_another_workspace_crate_is_refused() {
    assert_refused(
        "[dependencies]\nfixtures = { path = \"../../tools/fixtures\" }\n",
        EMPTY_WORKSPACE,
    );
}

#[test]
fn inline_dev_build_and_target_dependencies_are_refused() {
    for table in [
        "dev-dependencies",
        "build-dependencies",
        "target.'cfg(unix)'.dependencies",
    ] {
        assert_refused(
            &format!("[{table}]\nproxiima-core = {{ path = \"../../crates/core\" }}\n"),
            EMPTY_WORKSPACE,
        );
    }
}

#[test]
fn allowed_forms_pass() {
    let manifest = "[dependencies]\n\
                    proxiima = { path = \"../../crates/proxiima\" }\n\
                    serde = { workspace = true }\n\
                    sqlx = \"0.9\"\n\
                    [dev-dependencies]\n\
                    proxiima-example = { path = \".\" }\n";
    let workspace = "[workspace]\n[workspace.dependencies]\n\
                     serde = { version = \"1\", features = [\"derive\"] }\n";
    let offenders = workspace_crate_offenders(manifest, workspace);
    assert!(offenders.is_empty(), "allowed forms refused: {offenders:?}");
}
