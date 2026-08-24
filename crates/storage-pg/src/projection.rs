//! The projection generator: one contract in, one canonical shape out.
//!
//! This is the whole of it. `projection_artifacts(&FlavorContract)` is a
//! pure function with no parameters beyond the contract, no per-flavor
//! branch, and no configuration surface. Two flavors' emitted DDL differ in
//! the schema name and the index name and in nothing else — which is what
//! makes "the generator has not grown a hook" a checkable property rather
//! than an intention. A need the canonical shape cannot express becomes a
//! contract-vocabulary extension (`LanguagePolicy::PinnedUnion` is one),
//! never a generator feature.
//!
//! **Every artifact carries its derivable inverse.** [`Artifact`] is a
//! forward statement and its `DROP`, produced by the same call from the
//! same declaration, so they cannot drift. That is what makes provisioning
//! and de-provisioning a flavor in a deployment two readings of one
//! derivation instead of two hand-maintained scripts. A hand-written
//! inverse is the alternative, and it reaches whatever subset of the
//! sidecars its author remembered; `flavors/code/src/repos/erase.rs` is
//! the flavor's own inverse, where a test can compare it against the
//! contract.
//!
//! Two consumers, deliberately:
//!
//! 1. **The migration author.** The destructive baselines
//!    (`migrations/0001_v008.sql`, `flavors/code/migrations/*_v008_baseline.sql`)
//!    carry this function's output verbatim, pinned by
//!    `generator_output_is_the_migration_text` so a change to either side
//!    fails the build rather than the next boot.
//! 2. **The boot guardrail** (`ensure_projection_schema`), which re-runs it
//!    and compares against `information_schema` in the same bidirectional
//!    style as `ensure_lexical_language_stamps`.
//!
//! There is deliberately no third consumer that issues this DDL at
//! runtime: boot-time `CREATE TABLE` contradicts the migration ledger and,
//! in a shared multi-owner deployment, runs DDL once per process start.

use std::fmt::Write as _;

use proxima_core::StorageError;
use proxima_core::flavor::{
    FlavorContract, KeyShape, LanguagePolicy, SchemaContract, SearchProjectionDecl,
    TSVECTOR_WEIGHT_CLASSES,
};
use proxima_core::{SearchProjectionColumnKind, verbs::schema::MemorySearchProjection};

use crate::pg_ident::PgIdent;

/// The snippet bound the search verb applies to sidecar text.
pub(crate) const SNIPPET_BYTES: usize = 480;

/// One emitted object and the statement that removes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub forward: String,
    pub inverse: String,
}

/// Everything one flavor's projection is, as SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionArtifacts {
    pub table: Artifact,
    pub index: Artifact,
    pub language_trigger: Artifact,
}

impl ProjectionArtifacts {
    /// Forward statements in dependency order.
    #[must_use]
    pub fn forward(&self) -> Vec<&str> {
        vec![
            self.table.forward.as_str(),
            self.index.forward.as_str(),
            self.language_trigger.forward.as_str(),
        ]
    }

    /// Inverses in reverse dependency order — the de-provisioning script,
    /// derived rather than written.
    #[must_use]
    pub fn inverse(&self) -> Vec<&str> {
        vec![
            self.language_trigger.inverse.as_str(),
            self.index.inverse.as_str(),
            self.table.inverse.as_str(),
        ]
    }

    /// The whole emitted text, forward, as it appears in a baseline.
    #[must_use]
    pub fn render(&self) -> String {
        self.forward().join("\n\n")
    }
}

/// The extension the composite `gin (owner_id, search_tsv)` needs.
///
/// Prior art is `btree_gin`'s own documentation: "for queries that test
/// both a GIN-indexable column and a B-tree-indexable column, it might be
/// more efficient to create a multicolumn GIN index that uses one of these
/// operator classes than to create two separate indexes that would have to
/// be combined via bitmap `ANDing`" (`PostgreSQL` *F.9*). `uuid` is in its
/// supported type list.
pub const BTREE_GIN_EXTENSION: &str = "CREATE EXTENSION IF NOT EXISTS btree_gin;";

/// Contract declaration in → canonical projection shape out.
///
/// Returns `None` for a flavor that declares no projection.
///
/// # Errors
///
/// `StorageError::Internal` when a declared table, index or column name is
/// not a valid `PostgreSQL` identifier.
pub fn projection_artifacts(
    contract: &FlavorContract,
) -> Result<Option<ProjectionArtifacts>, StorageError> {
    let Some(spec) = contract.projection.spec() else {
        return Ok(None);
    };
    let table = PgIdent::table(spec.table)?;
    let index = PgIdent::column(spec.index)?;
    let (schema, relation) = split_qualified(spec.table)?;
    let trigger = format!("{relation}_remember_lang");
    let trigger_ident = PgIdent::column(&trigger)?;

    // SQL-POLICY: PgIdent
    // Every spliced name above went through `PgIdent`, which rejects
    // anything that is not a bare (optionally schema-qualified) identifier.
    // The rest of the text is fixed: the column list, the types and the
    // constraints are the same characters for every flavor, which is the
    // slimness property stated as code.
    let create = format!(
        "CREATE TABLE {table} (
    memory_id        uuid      NOT NULL
                     REFERENCES proxima_core.memory (t) ON DELETE CASCADE,
    schema_id        text      NOT NULL,
    owner_id         uuid      NOT NULL
                     REFERENCES proxima_core.owners (owner_id),
    search_tsv       tsvector  NOT NULL,
    tag              text[]    NOT NULL DEFAULT '{{}}',
    lexical_language regconfig NOT NULL DEFAULT proxima_core.lexical_config()
                     REFERENCES proxima_core.lexical_languages (config),
    PRIMARY KEY (memory_id, schema_id)
);",
        table = table.as_str()
    );

    let create_index = format!(
        "CREATE INDEX {index} ON {table} USING gin (owner_id, search_tsv);",
        index = index.as_str(),
        table = table.as_str()
    );

    let create_trigger = format!(
        "CREATE TRIGGER {trigger}
    BEFORE INSERT ON {table}
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.remember_lexical_language();",
        trigger = trigger_ident.as_str(),
        table = table.as_str()
    );

    Ok(Some(ProjectionArtifacts {
        table: Artifact {
            forward: create,
            // SQL-POLICY: PgIdent
            inverse: format!("DROP TABLE IF EXISTS {};", table.as_str()),
        },
        index: Artifact {
            forward: create_index,
            // SQL-POLICY: PgIdent
            inverse: format!("DROP INDEX IF EXISTS {schema}.{};", index.as_str()),
        },
        language_trigger: Artifact {
            forward: create_trigger,
            inverse: format!(
                "DROP TRIGGER IF EXISTS {} ON {};",
                trigger_ident.as_str(),
                table.as_str()
            ),
        },
    }))
}

fn split_qualified(table: &str) -> Result<(&str, &str), StorageError> {
    table.split_once('.').ok_or_else(|| {
        StorageError::Internal(format!(
            "projection table must be schema-qualified: {table:?}"
        ))
    })
}

/// The text expression the vector is built from, over sidecar alias `c`.
///
/// This is the same shape the generated columns carry —
/// `lexical_join(VARIADIC ARRAY[NULLIF(col,''), lexical_text_array(arr)])`
/// — which is what makes the identity re-proof provable rather than
/// hopeful, not a re-derivation that happens to agree.
fn search_text_expr(fields: &[(&str, SearchProjectionColumnKind)]) -> Result<String, StorageError> {
    if fields.is_empty() {
        return Err(StorageError::Internal(
            "a projected schema declares no fields".into(),
        ));
    }
    let mut parts = Vec::with_capacity(fields.len());
    for (column, kind) in fields {
        let column = PgIdent::column(column)?;
        parts.push(match kind {
            SearchProjectionColumnKind::Text => {
                format!("NULLIF(c.{}, '')", column.as_str())
            }
            SearchProjectionColumnKind::TextArray => {
                format!("proxima_core.lexical_text_array(c.{})", column.as_str())
            }
        });
    }
    Ok(format!(
        "proxima_core.lexical_join(VARIADIC ARRAY[{}])",
        parts.join(", ")
    ))
}

/// `lexical_tsv(<config>, <text>)`, unioned across a `PinnedUnion` in the
/// declared order. Order is load-bearing: tsvector concatenation offsets
/// the right operand's positions and `ts_rank_cd` is position-sensitive.
fn vector_expr(language: LanguagePolicy, text: &str) -> Result<String, StorageError> {
    match language {
        LanguagePolicy::PerRow { .. } => {
            Ok(format!("proxima_core.lexical_tsv({LANGUAGE_BIND}, {text})"))
        }
        LanguagePolicy::Pinned(config) => {
            let config = PgIdent::column(config)?;
            Ok(format!(
                "proxima_core.lexical_tsv('{}'::regconfig, {text})",
                config.as_str()
            ))
        }
        LanguagePolicy::PinnedUnion(configs) => {
            if configs.is_empty() {
                return Err(StorageError::Internal(
                    "PinnedUnion declares no configuration".into(),
                ));
            }
            let mut sql = String::new();
            for (index, config) in configs.iter().enumerate() {
                let config = PgIdent::column(config)?;
                if index > 0 {
                    sql.push_str("\n            || ");
                }
                let _ = write!(
                    sql,
                    "proxima_core.lexical_tsv('{}'::regconfig, {text})",
                    config.as_str()
                );
            }
            Ok(sql)
        }
    }
}

/// The write's language, or the deployment default. `$2`, and it appears
/// ONLY in a `LanguagePolicy::PerRow` statement — a pinned policy renders
/// its configuration as a literal, so its statement has no language bind
/// and no caller can influence what it stamps.
///
/// The `COALESCE` arm is reached by two writers only: a write that asked
/// for the deployment configuration by name, and a hydrate rebuild, whose
/// cold record carries no stamp. A write that named no language at all
/// never reaches this statement — see
/// `PgSidecarWriter::insert_memory_sidecar`.
const LANGUAGE_BIND: &str = "COALESCE($2::regconfig, proxima_core.lexical_config())";

/// The row's `lexical_language` value.
fn language_value_expr(language: LanguagePolicy) -> Result<String, StorageError> {
    match language {
        LanguagePolicy::PerRow { .. } => Ok(LANGUAGE_BIND.to_owned()),
        LanguagePolicy::Pinned(_) | LanguagePolicy::PinnedUnion(_) => {
            let config = language.pinned_config().ok_or_else(|| {
                StorageError::Internal("a pinned language policy names no configuration".into())
            })?;
            let config = PgIdent::column(config)?;
            Ok(format!("'{}'::regconfig", config.as_str()))
        }
    }
}

/// The whole vector expression for one schema, `setweight` included.
///
/// One declared level ⇒ ONE `to_tsvector` call over the joined text, which
/// is textually what the generated column emits and therefore
/// position-identical. Two or more ⇒ one call per class, concatenated,
/// because `setweight` marks a whole vector and per-field marking is the
/// only way to mark fields differently.
///
/// # Errors
///
/// `StorageError::Internal` for an invalid identifier or a unit whose
/// weight levels exceed `PostgreSQL`'s four classes (the freeze rejects
/// that first; this is the backstop).
pub fn projection_vector_sql(search: &SearchProjectionDecl) -> Result<String, StorageError> {
    let SearchProjectionDecl::Projected {
        fields, language, ..
    } = search
    else {
        return Err(StorageError::Internal(
            "a declared non-surface has no vector".into(),
        ));
    };
    let levels = search.weight_levels().map_err(|levels| {
        StorageError::Internal(format!(
            "{levels} distinct weight levels exceed the {} PostgreSQL tsvector classes",
            TSVECTOR_WEIGHT_CLASSES.len()
        ))
    })?;
    if levels.len() < 2 {
        let all = fields
            .iter()
            .map(|field| (field.column, field.kind))
            .collect::<Vec<_>>();
        let vector = vector_expr(*language, &search_text_expr(&all)?)?;
        return Ok(format!("COALESCE({vector}, ''::tsvector)"));
    }
    let mut parts = Vec::new();
    for (index, level) in levels.iter().enumerate() {
        let class = TSVECTOR_WEIGHT_CLASSES[index];
        let in_class = fields
            .iter()
            .filter(|field| field.weight.total_cmp(level).is_eq())
            .map(|field| (field.column, field.kind))
            .collect::<Vec<_>>();
        if in_class.is_empty() {
            continue;
        }
        let vector = vector_expr(*language, &search_text_expr(&in_class)?)?;
        parts.push(format!(
            "setweight(COALESCE({vector}, ''::tsvector), '{class}')"
        ));
    }
    Ok(parts.join("\n            || "))
}

/// The maintenance statement: one row of `<flavor>.projection` from one row
/// of the schema's sidecar, in the writing transaction.
///
/// `$1` is the memory id; `$2` is the caller's lexical language, or NULL
/// for the deployment default. `owner_id` comes from the joined `memory`
/// row rather than from a bind, so the projection's owner is the memory's
/// owner by construction and no caller can supply a different one.
///
/// The sidecar side of the join and of the `WHERE` reads the column the
/// contract declares for that table (`KeyShape::MemoryT { column }`), which
/// is what keeps the statement a function of the contract alone. `m.t` is
/// the kernel memory table's own key and is fixed.
///
/// The whole contract is the input rather than the projection spec alone,
/// because a sidecar's `Surface` may be declared on a SIBLING registration
/// of the same table — see [`FlavorContract::surface_for`].
///
/// # Errors
///
/// `StorageError::Internal` for an invalid identifier, a flavor that
/// declares no projection, a schema that is not a search surface, or a
/// sidecar that declares no memory key.
pub fn projection_insert_sql(
    contract: &FlavorContract,
    schema: &SchemaContract,
) -> Result<String, StorageError> {
    let spec = contract.projection.spec().ok_or_else(|| {
        StorageError::Internal(format!(
            "flavor {} declares no projection table, so its schemas write no projection row",
            contract.flavor_id
        ))
    })?;
    let table = PgIdent::table(spec.table)?;
    let sidecar_table = schema.sidecar_table.ok_or_else(|| {
        StorageError::Internal("a projected schema declares no sidecar table".into())
    })?;
    let sidecar = PgIdent::table(sidecar_table)?;
    let key = PgIdent::column(sidecar_memory_key(contract, schema, sidecar_table)?)?;
    let SearchProjectionDecl::Projected {
        tag_column,
        language,
        ..
    } = &schema.search
    else {
        return Err(StorageError::Internal(
            "a declared non-surface writes no projection row".into(),
        ));
    };
    let vector = projection_vector_sql(&schema.search)?;
    let tag = match tag_column {
        Some(column) => format!("c.{}", PgIdent::column(column)?.as_str()),
        None => "'{}'::text[]".to_owned(),
    };
    let language_value = language_value_expr(*language)?;

    // SQL-POLICY: PgIdent
    // The projection table, the sidecar table, its declared memory-key
    // column, the tag column and every configuration name are validated
    // identifiers; the schema id is a bind ($3) and the memory id is a
    // bind ($1).
    //
    // `c.{key}` is the SIDECAR's key column as the contract declares it and
    // `m.t` is the kernel memory table's own, which is fixed. Spelling the
    // sidecar side `t` would make this statement a function of the contract
    // plus a naming convention no declaration states, and a sidecar keyed
    // on any other column would get no projection row.
    //
    // `m.schema_id = $3` makes `projection.schema_id = memory.schema_id` a
    // property of the only statement that writes a projection row, rather
    // than a convention every caller has to remember. Search relies on it:
    // the ranked arm narrows on `p.schema_id` where `admit_hits` narrows on
    // `m.schema_id`, and those two agreeing is what lets one stand in for
    // the other. Nothing in the schema enforces it, so a mismatched write
    // inserts nothing rather than writing a row search would spend a window
    // slot on and admission would drop.
    Ok(format!(
        "INSERT INTO {table}
       (memory_id, schema_id, owner_id, search_tsv, tag, lexical_language)
SELECT c.{key},
       $3,
       m.owner_id,
       {vector},
       {tag},
       {language_value}
  FROM {sidecar} c
  JOIN proxima_core.memory m ON m.t = c.{key}
 WHERE c.{key} = $1
   AND m.schema_id = $3",
        table = table.as_str(),
        sidecar = sidecar.as_str(),
        key = key.as_str()
    ))
}

/// The sidecar's memory-key column, as the contract's [`Surface`] for that
/// table declares it, or the refusal.
///
/// There is no `t` fallback on either arm, deliberately. A default here
/// reads as "the contract, unless it says something this generator does not
/// expect", which is the shape of defect that produces a flavor whose
/// projection is silently empty. The error names the declaration to fix.
///
/// [`Surface`]: proxima_core::flavor::Surface
fn sidecar_memory_key(
    contract: &FlavorContract,
    schema: &SchemaContract,
    sidecar_table: &str,
) -> Result<&'static str, StorageError> {
    let Some(surface) = contract.surface_for(sidecar_table) else {
        return Err(StorageError::Internal(format!(
            "projected schema {} declares sidecar table {sidecar_table}, which no Surface of \
             flavor {} declares; add that Surface so the projection can read the declared \
             memory-key column",
            schema.schema_id().as_str(),
            contract.flavor_id
        )));
    };
    match surface.key {
        KeyShape::MemoryT { column } => Ok(column),
        other => Err(StorageError::Internal(format!(
            "projected schema {} keys sidecar {sidecar_table} as {other:?}; a projection row is \
             one per memory, so the sidecar's memory column has to be declared as \
             KeyShape::MemoryT {{ column }}",
            schema.schema_id().as_str()
        ))),
    }
}

/// Every projection table a composed binary declares, in name order.
#[must_use]
pub fn projection_tables(contracts: &[&'static FlavorContract]) -> Vec<String> {
    let mut tables = contracts
        .iter()
        .filter_map(|contract| contract.projection.table())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    tables.sort();
    tables.dedup();
    tables
}

/// The snippet expression the search verb joins back to the sidecar for.
///
/// The projection carries no snippet. Search ranks on the projection
/// alone and joins the owning sidecar for the surviving top-k rows only, so
/// the text lives in exactly one place and the hot path still scans one
/// indexed table.
///
/// # Errors
///
/// `StorageError::Internal` for an invalid column identifier.
pub(crate) fn snippet_sql(projection: &MemorySearchProjection) -> Result<String, StorageError> {
    Ok(format!(
        "left({}, {SNIPPET_BYTES})",
        sidecar_text_sql(projection)?
    ))
}

/// The joined sidecar text for one projected schema, over alias `c`.
///
/// The same expression the vector is built from, so the snippet, the
/// substring arm and the stored vector cannot disagree about what a row's
/// searchable text is.
///
/// # Errors
///
/// `StorageError::Internal` for an invalid column identifier.
pub(crate) fn sidecar_text_sql(
    projection: &MemorySearchProjection,
) -> Result<String, StorageError> {
    let fields = projection
        .fields
        .iter()
        .map(|field| (field.column.as_str(), field.kind))
        .collect::<Vec<_>>();
    search_text_expr(&fields)
}

/// The sidecar key column of a runtime search projection, validated.
///
/// The read-side twin of [`sidecar_memory_key`]. Freeze carried the
/// declared column onto [`MemorySearchProjection`]; every read statement
/// that joins or filters the sidecar spells it from here, so the substring
/// arm, the snippet lookup and the projection `INSERT` all key the sidecar
/// on the one column its `Surface` declares. Same rule as the write side:
/// no `t` fallback, because the fallback is the defect.
///
/// There is no "absent column" arm: freeze refuses a projected schema whose
/// sidecar is not `KeyShape::MemoryT`-keyed, so the field is a `String` and
/// the only thing left to check here is that it is a legal identifier.
///
/// # Errors
///
/// `StorageError::Internal` when the declared memory key column is not a
/// valid identifier.
pub(crate) fn projection_key_ident(
    projection: &MemorySearchProjection,
) -> Result<PgIdent<'_>, StorageError> {
    PgIdent::column(&projection.sidecar_key_column)
}

/// The boot half of the pin: what the generator says, and what the catalog
/// has, must be the same set of projection tables — in both directions.
///
/// A flavor linked without its baseline is a search that silently returns
/// nothing; a projection table left behind by a flavor that was unlinked is
/// rows nobody maintains. Both are the same kind of drift as a missing
/// migration, so they fail the same way: at boot, before a write.
///
/// # Errors
///
/// [`StorageError::Internal`] when a declared projection table, its index or
/// one of its columns is missing, or when a `<schema>.projection` table
/// exists that no linked flavor declares.
pub async fn ensure_projection_schema(
    pool: &sqlx::PgPool,
    contracts: &[&'static proxima_core::flavor::FlavorContract],
) -> Result<(), StorageError> {
    let declared = projection_tables(contracts);
    let found: Vec<String> = sqlx::query_scalar(
        "SELECT (table_schema || '.' || table_name)::text
           FROM information_schema.tables
          WHERE table_name = $1
          ORDER BY 1",
    )
    .bind(proxima_core::flavor::PROJECTION_TABLE_NAME)
    .fetch_all(pool)
    .await
    .map_err(crate::error::map_err)?;
    if found != declared {
        return Err(StorageError::Internal(format!(
            "projection tables in the database do not match the linked flavors: \
             declared {declared:?}, found {found:?}; apply migrations before boot"
        )));
    }

    for contract in contracts {
        let Some(spec) = contract.projection.spec() else {
            continue;
        };
        let (schema, _) = split_qualified(spec.table)?;
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT column_name::text
               FROM information_schema.columns
              WHERE table_schema = $1
                AND table_name = $2
              ORDER BY 1",
        )
        .bind(schema)
        .bind(proxima_core::flavor::PROJECTION_TABLE_NAME)
        .fetch_all(pool)
        .await
        .map_err(crate::error::map_err)?;
        if columns != PROJECTION_COLUMNS {
            return Err(StorageError::Internal(format!(
                "{} has columns {columns:?}, the generator emits {PROJECTION_COLUMNS:?}; \
                 the baseline was hand-edited away from the generator",
                spec.table
            )));
        }
        let index_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM pg_indexes
                  WHERE schemaname = $1 AND indexname = $2
             )",
        )
        .bind(schema)
        .bind(spec.index)
        .fetch_one(pool)
        .await
        .map_err(crate::error::map_err)?;
        if !index_exists {
            return Err(StorageError::Internal(format!(
                "missing index {}.{} on {}; apply migrations before boot",
                schema, spec.index, spec.table
            )));
        }
    }
    Ok(())
}

/// The generator's column list, in `information_schema` order.
const PROJECTION_COLUMNS: [&str; 6] = [
    "lexical_language",
    "memory_id",
    "owner_id",
    "schema_id",
    "search_tsv",
    "tag",
];

#[cfg(test)]
mod tests {
    use super::{
        PROJECTION_COLUMNS, projection_artifacts, projection_insert_sql, projection_vector_sql,
    };
    use proxima_core::FLAVOR_0;
    use proxima_core::flavor::{SchemaContract, SearchProjectionDecl};

    /// `PROJECTION_MEMORY_COLUMN` names one thing, and everything that
    /// spells that thing agrees with it.
    ///
    /// Being a constant buys no agreement here: it is read in exactly one
    /// place (`ProjectionSpec::surface`). The FK name is an independent literal,
    /// and the generator writes `memory_id` inline in the `CREATE TABLE`
    /// column list, in its `PRIMARY KEY`, in the projection `INSERT`, and in
    /// `PROJECTION_COLUMNS`. Interpolating the constant into fixed SQL text
    /// would trade four spellings for four `format!` holes and buy nothing,
    /// so the agreement is asserted instead of engineered.
    ///
    /// The DDL is read out of the generator's own emitted artifact rather
    /// than restated here, so this cannot pass against text nobody emits.
    #[test]
    fn the_projection_column_name_has_one_spelling() {
        use proxima_core::flavor::{
            PROJECTION_MEMORY_COLUMN, PROJECTION_MEMORY_FK, PROJECTION_TABLE_NAME,
        };

        assert_eq!(
            PROJECTION_MEMORY_FK,
            format!("{PROJECTION_TABLE_NAME}_{PROJECTION_MEMORY_COLUMN}_fkey"),
            "PostgreSQL mints an FK name as <table>_<column>_fkey, so these two \
             constants are one fact; they are separate literals, which is why this \
             is checked"
        );
        assert!(
            PROJECTION_COLUMNS.contains(&PROJECTION_MEMORY_COLUMN),
            "the catalog boot check compares against PROJECTION_COLUMNS, so a \
             renamed column that missed that list would pass boot and fail the \
             surface"
        );

        let artifacts = projection_artifacts(&FLAVOR_0)
            .expect("core artifacts")
            .expect("core declares a projection");
        let ddl = &artifacts.table.forward;
        assert!(
            ddl.contains(&format!("{PROJECTION_MEMORY_COLUMN} ")),
            "the emitted CREATE TABLE declares the column this constant names: {ddl}"
        );
        assert!(
            ddl.contains(&format!(
                "PRIMARY KEY ({PROJECTION_MEMORY_COLUMN}, schema_id)"
            )),
            "and keys on it: {ddl}"
        );

        let schema = FLAVOR_0
            .schemas
            .iter()
            .find(|schema| schema.search.is_projected())
            .expect("core projects at least one schema");
        let spec = FLAVOR_0.projection.spec().expect("core declares a spec");
        let insert = projection_insert_sql(&FLAVOR_0, schema).expect("the insert generates");
        assert!(
            insert.contains(&format!("({PROJECTION_MEMORY_COLUMN}, schema_id,")),
            "and the projection INSERT writes it: {insert}"
        );

        assert_eq!(
            spec.surface().key,
            proxima_core::flavor::KeyShape::MemoryT {
                column: PROJECTION_MEMORY_COLUMN,
            },
            "the one reader agrees with all of the above"
        );

        // Two surfaces, two key columns. The projection's own is this
        // constant and is fixed for every flavor; the SIDECAR's is whatever
        // that schema's `Surface` declares, and the `INSERT` reads it from
        // there. Asserting them together is what keeps a reader from
        // treating one as the other — the confusion that put a literal `t`
        // on the sidecar side of this statement.
        let sidecar_table = schema
            .sidecar_table
            .expect("a projected schema has a sidecar");
        let sidecar_key = FLAVOR_0
            .sidecar_memory_key_column(sidecar_table)
            .expect("a projected sidecar declares a memory key");
        assert!(
            insert.contains(&format!("FROM {sidecar_table} c")),
            "the sidecar is the FROM: {insert}"
        );
        assert!(
            insert.contains(&format!(
                "JOIN proxima_core.memory m ON m.t = c.{sidecar_key}"
            )),
            "and the join reads the DECLARED sidecar column against the kernel's fixed \
             `t`: {insert}"
        );
    }

    const NOTE_SIDECAR: &str = "proxima_core.agent_note_v1";

    /// Flavor #0's note declaration with its sidecar re-keyed, and nothing
    /// else changed — so an assertion that fails below is about the key
    /// column and nothing else.
    ///
    /// The contract is narrowed to the one schema so `surface_for` cannot
    /// answer from a sibling's declaration of the same table.
    fn a_contract_keying_the_note_on(
        key: proxima_core::flavor::KeyShape,
    ) -> (
        proxima_core::flavor::FlavorContract,
        &'static SchemaContract,
    ) {
        let mut surface = FLAVOR_0
            .surface_for(NOTE_SIDECAR)
            .expect("the note sidecar is declared");
        surface.key = key;
        let mut schema = *FLAVOR_0
            .schemas
            .iter()
            .find(|schema| schema.sidecar_table == Some(NOTE_SIDECAR))
            .expect("agent_note_v1");
        schema.surfaces = Box::leak(Box::new([surface]));
        let schema: &'static SchemaContract = Box::leak(Box::new(schema));
        let mut contract = FLAVOR_0;
        contract.schemas = std::slice::from_ref(schema);
        (contract, schema)
    }

    /// The generator is a function of the contract, INCLUDING the column the
    /// sidecar keys its memory on.
    ///
    /// A downstream flavor may key its sidecar on a name of its own —
    /// `KeyShape::MemoryT { column }` exists precisely because the erase and
    /// export lanes could not otherwise find the id. A literal `t` on the
    /// sidecar side of this statement made the projection a function of the
    /// contract PLUS a naming convention no declaration states, and such a
    /// flavor got no projection rows at all.
    #[test]
    fn the_insert_keys_the_sidecar_on_the_column_the_contract_declares() {
        const RENAMED: &str = "note_memory_id";

        let (contract, schema) =
            a_contract_keying_the_note_on(proxima_core::flavor::KeyShape::MemoryT {
                column: RENAMED,
            });
        let insert = projection_insert_sql(&contract, schema).expect("the insert generates");

        // Substituted into a shape rather than built with `format!`, so the
        // SQL-policy scanner does not read an expectation about a statement
        // as a statement being built.
        for shape in [
            "SELECT c.<key>,",
            "JOIN proxima_core.memory m ON m.t = c.<key>",
            "WHERE c.<key> = $1",
        ] {
            let expected = shape.replace("<key>", RENAMED);
            assert!(
                insert.contains(&expected),
                "the declared key column is spelled at every sidecar reference; \
                 missing {expected:?} in: {insert}"
            );
        }
        // The three spellings the defect had, named literally. A blanket
        // `!contains("c.t")` would also reject `c.tags`, the tag column this
        // very statement copies.
        for forbidden in ["SELECT c.t,", "m.t = c.t\n", "WHERE c.t = $1"] {
            assert!(
                !insert.contains(forbidden),
                "nothing keys the sidecar on `t` behind the declaration's back; \
                 found {forbidden:?} in: {insert}"
            );
        }
        assert!(
            insert.contains("m.t = "),
            "`m.t` is the kernel memory table's own key and stays: {insert}"
        );
    }

    /// A sidecar the generator cannot key on a memory is a refusal that
    /// names the declaration to fix — never a fall back to `t`.
    ///
    /// The fallback is the defect: it writes a statement the contract did
    /// not ask for, which either files nothing or errors in the writing
    /// transaction, and in both cases the flavor's search is empty with
    /// nothing saying why.
    #[test]
    fn a_sidecar_that_is_not_memory_keyed_is_refused_by_name() {
        let (contract, schema) =
            a_contract_keying_the_note_on(proxima_core::flavor::KeyShape::Custom(&["note_id"]));
        let err = projection_insert_sql(&contract, schema).expect_err("the generator refuses");
        let message = err.to_string();
        assert!(
            message.contains("KeyShape::MemoryT"),
            "the refusal names the declaration to fix: {message}"
        );
        assert!(
            message.contains("proxima_core.agent_note_v1"),
            "and the surface it is about: {message}"
        );
    }

    /// The slimness rule, as a test: two flavors' DDL differs in the schema
    /// name and the index name and nowhere else. If it ever differs
    /// elsewhere, the generator has grown a hook.
    #[test]
    fn two_flavors_emit_the_same_ddl_modulo_their_names() {
        let flavor_zero = projection_artifacts(&FLAVOR_0)
            .expect("core artifacts")
            .expect("core declares a projection");
        let the_code_flavor = projection_artifacts(&proxima_code_contract())
            .expect("code artifacts")
            .expect("code declares a projection");
        let normalize = |text: &str| {
            text.replace("proxima_code", "SCHEMA")
                .replace("proxima_core.projection", "SCHEMA.projection")
                .replace("code_projection_owner_tsv_gin", "INDEX")
                .replace("core_projection_owner_tsv_gin", "INDEX")
        };
        // `proxima_core` also appears in the referenced kernel tables, so
        // normalize the projection's own schema by its qualified name and
        // leave the references alone.
        assert_eq!(
            normalize(&flavor_zero.table.forward).replace("SCHEMA.projection", "P"),
            normalize(&the_code_flavor.table.forward).replace("SCHEMA.projection", "P"),
        );
        assert_eq!(
            normalize(&flavor_zero.index.forward),
            normalize(&the_code_flavor.index.forward)
        );
    }

    fn proxima_code_contract() -> proxima_core::flavor::FlavorContract {
        // The code flavor is not a dependency of this crate; rebuild the
        // shape its contract declares so the slimness property is testable
        // here rather than only in a downstream crate.
        let mut contract = FLAVOR_0;
        contract.flavor_id = "proxima-code";
        contract.ordinal = 1;
        contract.projection =
            proxima_core::flavor::ProjectionDecl::Table(proxima_core::flavor::ProjectionSpec {
                table: "proxima_code.projection",
                index: "code_projection_owner_tsv_gin",
                overfetch_k: 1_000,
                band_comparability: proxima_core::flavor::BandComparability::CoreBands,
                // The DDL is what this fixture tests, and the generator
                // emits the same table whatever the read shape says.
                rank_source: proxima_core::flavor::RankSource::Projection,
            });
        contract
    }

    /// Every artifact's inverse is derived from the same declaration, so a
    /// de-provisioning script is a reading of the generator rather than a
    /// second hand-maintained file.
    #[test]
    fn every_artifact_carries_its_inverse() {
        let artifacts = projection_artifacts(&FLAVOR_0)
            .expect("artifacts")
            .expect("core declares a projection");
        assert_eq!(
            artifacts.inverse(),
            vec![
                "DROP TRIGGER IF EXISTS projection_remember_lang ON proxima_core.projection;",
                "DROP INDEX IF EXISTS proxima_core.core_projection_owner_tsv_gin;",
                "DROP TABLE IF EXISTS proxima_core.projection;",
            ]
        );
    }

    /// Uniform weights emit ONE `to_tsvector` over the joined text — the
    /// generated column's own expression. Per-field `setweight` would shift
    /// lexeme positions and move every `ts_rank_cd`.
    #[test]
    fn a_uniform_unit_emits_the_generated_columns_expression() {
        let note = FLAVOR_0
            .schemas
            .iter()
            .find(|schema| schema.sidecar_table == Some("proxima_core.agent_note_v1"))
            .expect("agent_note_v1");
        let vector = projection_vector_sql(&note.search).expect("vector");
        assert_eq!(
            vector,
            "COALESCE(proxima_core.lexical_tsv(\
             COALESCE($2::regconfig, proxima_core.lexical_config()), \
             proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.title, ''), NULLIF(c.body, ''), \
             proxima_core.lexical_text_array(c.tags)])), ''::tsvector)"
        );
        assert!(!vector.contains("setweight"), "uniform emits no setweight");
    }

    #[test]
    fn the_maintenance_statement_takes_its_owner_from_the_memory_row() {
        let note = FLAVOR_0
            .schemas
            .iter()
            .find(|schema| schema.sidecar_table == Some(NOTE_SIDECAR))
            .expect("agent_note_v1");
        let sql = projection_insert_sql(&FLAVOR_0, note).expect("insert");
        assert!(sql.contains("m.owner_id"), "owner comes from memory");
        assert!(
            sql.contains("JOIN proxima_core.memory m ON m.t = c.t"),
            "the owner is joined, never bound"
        );
        assert!(sql.contains("c.tags"), "the tag column is copied");
    }

    #[test]
    fn a_declared_non_surface_has_no_vector() {
        let decl = SearchProjectionDecl::None { why: "a fixture" };
        assert!(projection_vector_sql(&decl).is_err());
    }
}
