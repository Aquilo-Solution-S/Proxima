use super::{memory_insert_sql, memory_select_batch_sql, validate_sidecar_read_sql};

#[test]
fn memory_insert_sql_validates_identifiers() {
    let sql = memory_insert_sql(
        "proxima_core.agent_note_v1",
        "memory_id",
        &[("title", Some("text")), ("tags", None)],
    )
    .unwrap();

    assert_eq!(
        sql,
        "INSERT INTO proxima_core.agent_note_v1 (memory_id, title, tags) \
             VALUES ($1, $2::text, $3)"
    );
    assert!(
        memory_insert_sql("proxima_core.agent_note_v1; DROP TABLE x", "memory_id", &[]).is_err()
    );
    assert!(memory_insert_sql("proxima_core.agent_note_v1", "memory-id", &[]).is_err());
    assert!(
        memory_insert_sql(
            "proxima_core.agent_note_v1",
            "memory_id",
            &[("x); DROP TABLE y", None)]
        )
        .is_err()
    );
}

#[test]
fn memory_select_batch_sql_validates_identifiers() {
    let sql = memory_select_batch_sql(
        "proxima_core.agent_note_v1",
        "memory_id",
        &["title", "tags"],
    )
    .unwrap();

    assert_eq!(
        sql,
        "SELECT memory_id, title, tags FROM proxima_core.agent_note_v1 \
             WHERE memory_id = ANY($1)"
    );
    // Regression: enum projection expressions (`<col>::text AS <col>`,
    // emitted by `pg_sidecar_select_col!`) are trusted compile-time
    // expressions and must be accepted verbatim, not rejected as
    // non-identifiers.
    let enum_sql = memory_select_batch_sql(
        "proxima_core.agent_note_v1",
        "memory_id",
        &["state::text AS state"],
    )
    .unwrap();
    assert!(enum_sql.contains("state::text AS state"));
    // The table and key column are still validated as identifiers.
    assert!(memory_select_batch_sql("bad table;", "memory_id", &["title"]).is_err());
    assert!(
        memory_select_batch_sql("proxima_core.agent_note_v1", "memory_id;", &["title"]).is_err()
    );
}

#[test]
fn public_sidecar_batch_read_denies_core_schema_unless_registry_admits_it() {
    let sql = "SELECT memory_id FROM proxima_core.agent_note_v1 WHERE memory_id = ANY($1)";
    let err = validate_sidecar_read_sql(sql, false).expect_err("public helper denies core SQL");
    assert!(err.to_string().contains("proxima_core.*"), "message: {err}");
    validate_sidecar_read_sql(sql, true).expect("registered core sidecars may read core tables");
}
