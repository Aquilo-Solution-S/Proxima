//! Shared migration facade for embedded Proxima hosts.
//!
//! Core and flavor migrators share `SQLx`'s default `_sqlx_migrations`
//! table in v0.0.1. The facade keeps that global namespace explicit:
//! core runs first, flavors run in composition order, every migrator has
//! `ignore_missing(true)`, and duplicate versions fail before the
//! database is touched.

use std::collections::BTreeMap;

use proxima_storage_pg::{PgStorage, core_migrator};
use sqlx::migrate::{MigrateError, Migrator};

const CORE_SOURCE: &str = "proxima-core";

/// One named `SQLx` migration source in a composite Proxima binary.
#[derive(Debug)]
pub struct NamedMigrator {
    source: &'static str,
    migrator: Migrator,
}

impl NamedMigrator {
    /// Build a named migrator. Use the flavor id or host app id as
    /// `source`, e.g. `proxima-code`.
    #[must_use]
    pub fn new(source: &'static str, migrator: Migrator) -> Self {
        Self { source, migrator }
    }

    /// Source id used in reports and errors.
    #[must_use]
    pub fn source(&self) -> &'static str {
        self.source
    }

    /// Borrow the underlying `SQLx` migrator.
    #[must_use]
    pub fn migrator(&self) -> &Migrator {
        &self.migrator
    }
}

/// One migration version contributed by one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationVersion {
    pub source: &'static str,
    pub version: i64,
    pub description: String,
}

/// Successful migration run metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRunReport {
    pub sources: Vec<&'static str>,
    pub versions: Vec<MigrationVersion>,
}

/// Errors raised by the framework migration facade.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error(
        "duplicate migration version {version}: {first_source} ({first_description}) and {second_source} ({second_description})"
    )]
    DuplicateVersion {
        version: i64,
        first_source: &'static str,
        first_description: String,
        second_source: &'static str,
        second_description: String,
    },
    #[error("core migrations failed: {0}")]
    Core(#[source] MigrateError),
    #[error("flavor migrations failed for {source}: {err}")]
    Flavor {
        source: &'static str,
        #[source]
        err: MigrateError,
    },
}

/// Run core migrations followed by the provided flavor/host migrators.
///
/// # Errors
///
/// Returns `MigrationError::DuplicateVersion` before any database write
/// if two sources claim the same migration version. Returns `Core` or
/// `Flavor` if `SQLx` fails while applying that source.
pub async fn run_core_and_flavor_migrations(
    pg: &PgStorage,
    flavors: impl IntoIterator<Item = NamedMigrator>,
) -> Result<MigrationRunReport, MigrationError> {
    let sources = prepare_sources(flavors)?;
    let report = MigrationRunReport::from_sources(&sources);

    for source in sources {
        if source.source == CORE_SOURCE {
            source
                .migrator
                .run(pg.pool())
                .await
                .map_err(MigrationError::Core)?;
        } else {
            source
                .migrator
                .run(pg.pool())
                .await
                .map_err(|err| MigrationError::Flavor {
                    source: source.source,
                    err,
                })?;
        }
    }

    Ok(report)
}

impl MigrationRunReport {
    fn from_sources(sources: &[NamedMigrator]) -> Self {
        let versions = sources
            .iter()
            .flat_map(|source| {
                source.migrator.iter().map(|migration| MigrationVersion {
                    source: source.source,
                    version: migration.version,
                    description: migration.description.to_string(),
                })
            })
            .collect();
        let sources = sources.iter().map(|source| source.source).collect();
        Self { sources, versions }
    }
}

fn prepare_sources(
    flavors: impl IntoIterator<Item = NamedMigrator>,
) -> Result<Vec<NamedMigrator>, MigrationError> {
    let mut sources = Vec::new();
    sources.push(NamedMigrator::new(CORE_SOURCE, core_migrator()));

    for mut source in flavors {
        source.migrator.set_ignore_missing(true);
        sources.push(source);
    }

    reject_duplicate_versions(&sources)?;
    Ok(sources)
}

fn reject_duplicate_versions(sources: &[NamedMigrator]) -> Result<(), MigrationError> {
    let mut seen: BTreeMap<i64, (&'static str, String)> = BTreeMap::new();

    for source in sources {
        for migration in source.migrator.iter() {
            let description = migration.description.to_string();
            if let Some((first_source, first_description)) =
                seen.insert(migration.version, (source.source, description.clone()))
            {
                return Err(MigrationError::DuplicateVersion {
                    version: migration.version,
                    first_source,
                    first_description,
                    second_source: source.source,
                    second_description: description,
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use sqlx::migrate::{Migration, MigrationType, Migrator};

    use super::{MigrationError, NamedMigrator, prepare_sources};

    fn migrator(versions: &[i64]) -> Migrator {
        let migrations = versions
            .iter()
            .map(|version| {
                Migration::new(
                    *version,
                    Cow::Owned(format!("test {version}")),
                    MigrationType::Simple,
                    Cow::Owned(format!("SELECT {version};")),
                    false,
                )
            })
            .collect();
        Migrator {
            migrations: Cow::Owned(migrations),
            ..Migrator::DEFAULT
        }
    }

    #[test]
    fn duplicate_versions_fail_before_run() {
        let err = prepare_sources([
            NamedMigrator::new("alpha", migrator(&[9])),
            NamedMigrator::new("beta", migrator(&[9])),
        ])
        .expect_err("duplicate migration version should fail");

        assert!(matches!(
            err,
            MigrationError::DuplicateVersion {
                version: 9,
                first_source: "alpha",
                second_source: "beta",
                ..
            }
        ));
    }

    #[test]
    fn flavor_sources_are_forced_to_ignore_missing() {
        let sources =
            prepare_sources([NamedMigrator::new("alpha", migrator(&[9]))]).expect("valid sources");
        let alpha = sources
            .iter()
            .find(|source| source.source() == "alpha")
            .expect("alpha source");

        assert!(alpha.migrator().ignore_missing);
    }
}
