// Self-test fixture for `check-architecture-guardrails.py --self-test`.
//
// This file lives under scripts/fixtures/ so neither the real spiice scan
// (spiices/**/src only) nor check-sql-policy.py (crates/apps/spiices/examples
// only) ever visits it; only the self-test reads it. Cases (a) and (b) must
// be flagged as spiice raw proxiima_core SQL; case (c) must never be.

use proxiima_core::verbs::query::EntityKind; // (c) Rust path syntax: allowed

// (b) single-line string literal with a schema-qualified reference.
async fn single_line_literal(pool: &sqlx::PgPool) -> sqlx::Result<()> {
    sqlx::query("SELECT memory_id FROM proxiima_core.memories")
        .fetch_all(pool)
        .await?;
    Ok(())
}

// (a) multi-line raw string whose opening delimiter and `proxiima_core.`
// reference sit on different lines -- the exact evasion a line-scoped regex
// missed before the whole-literal tokenizer.
async fn multiline_raw_string_literal(pool: &sqlx::PgPool) -> sqlx::Result<()> {
    sqlx::query(
        r#"
        SELECT m.memory_id
          FROM proxiima_core.memories m
         WHERE m.owner_kind = $1
        "#,
    )
    .bind("user")
    .fetch_all(pool)
    .await?;
    Ok(())
}

// (c) more Rust path syntax that must stay allowed everywhere.
fn rust_path_only() -> proxiima_core::MemoryId {
    proxiima_core::MemoryId::nil()
}
