use super::{
    StorageError, memory_insert_batch_sql, memory_insert_sql, memory_select_batch_sql,
    validate_sidecar_read_sql,
};

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

/// Golden text for the batched sidecar insert, spelled with the columns
/// `pg_sidecar!` passes for the core utterance sidecar — the one payload
/// that opts into `batch_insert: unnest`.
#[test]
fn memory_insert_batch_sql_unnests_one_array_per_column() {
    let sql = memory_insert_batch_sql(
        "proxima_core.utterance_v1",
        "memory_id",
        &[
            ("speaker", "text"),
            ("conversation_id", "text"),
            ("text", "text"),
        ],
    )
    .unwrap();

    assert_eq!(
        sql,
        "INSERT INTO proxima_core.utterance_v1 (memory_id, speaker, conversation_id, text) \
             SELECT * FROM unnest($1::uuid[], $2::text[], $3::text[], $4::text[])"
    );
    // A schema-qualified enum type is a legal array element type.
    assert!(
        memory_insert_batch_sql(
            "proxima_core.task_goal_v1",
            "goal_id",
            &[("priority", "proxima_core.task_priority")],
        )
        .unwrap()
        .ends_with("SELECT * FROM unnest($1::uuid[], $2::proxima_core.task_priority[])")
    );
    // Table, key column, payload column and type are all validated.
    assert!(memory_insert_batch_sql("bad table;", "memory_id", &[]).is_err());
    assert!(memory_insert_batch_sql("proxima_core.utterance_v1", "memory-id", &[]).is_err());
    assert!(
        memory_insert_batch_sql(
            "proxima_core.utterance_v1",
            "memory_id",
            &[("x); DROP TABLE y", "text")],
        )
        .is_err()
    );
    assert!(
        memory_insert_batch_sql(
            "proxima_core.utterance_v1",
            "memory_id",
            &[("speaker", "text[]; DROP TABLE y")],
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

/// Payload side of the `opt_u32_as_i32` converter: `Option<u32>`.
struct OptU32Payload {
    retry_count: Option<u32>,
}

/// Row side of the `opt_u32_as_i32` converter, spelled through the macro so
/// the test breaks if `pg_sidecar_row_ty!` stops yielding `Option<i32>`.
struct OptU32Row {
    retry_count: crate::pg_sidecar_row_ty!(opt_u32_as_i32),
}

fn bind_opt_u32(payload: &OptU32Payload) -> Result<Option<i32>, StorageError> {
    Ok(crate::pg_sidecar_bind!(
        (opt_u32_as_i32),
        payload,
        retry_count
    ))
}

// By value, like the owned `FromRow` row the generated `load_batch` decodes.
#[allow(clippy::needless_pass_by_value)]
fn decode_opt_u32(row: OptU32Row) -> Result<Option<u32>, StorageError> {
    Ok(crate::pg_sidecar_decode!(
        (opt_u32_as_i32),
        row,
        retry_count
    ))
}

#[test]
fn opt_u32_as_i32_binds_and_decodes_present_and_absent_values() {
    // `opt_u32_as_i32` needs no cast and no SELECT projection rewrite; it
    // falls through to the default arms of `pg_sidecar_cast!` and
    // `pg_sidecar_select_col!`.
    let cast: Option<&str> = crate::pg_sidecar_cast!(opt_u32_as_i32);
    assert_eq!(cast, None);
    assert_eq!(
        crate::pg_sidecar_select_col!((opt_u32_as_i32), retry_count),
        "retry_count"
    );

    assert_eq!(
        bind_opt_u32(&OptU32Payload {
            retry_count: Some(7)
        })
        .unwrap(),
        Some(7_i32)
    );
    assert_eq!(
        bind_opt_u32(&OptU32Payload { retry_count: None }).unwrap(),
        None
    );

    assert_eq!(
        decode_opt_u32(OptU32Row {
            retry_count: Some(7)
        })
        .unwrap(),
        Some(7_u32)
    );
    assert_eq!(
        decode_opt_u32(OptU32Row { retry_count: None }).unwrap(),
        None
    );
}

#[test]
fn opt_u32_as_i32_rejects_out_of_range_values_in_both_directions() {
    // Bind side: a `u32` above `i32::MAX` errors rather than saturating.
    let err = bind_opt_u32(&OptU32Payload {
        retry_count: Some(u32::try_from(i32::MAX).expect("i32::MAX fits u32") + 1),
    })
    .expect_err("out-of-range u32 is rejected on bind");
    assert!(
        matches!(err, StorageError::ConstraintViolation(ref msg)
            if msg.contains("retry_count out of range")),
        "message: {err}"
    );

    // Decode side: a negative SQL `integer` errors rather than wrapping.
    let err = decode_opt_u32(OptU32Row {
        retry_count: Some(-1),
    })
    .expect_err("negative integer is rejected on decode");
    assert!(
        matches!(err, StorageError::Internal(ref msg) if msg.contains("invalid retry_count")),
        "message: {err}"
    );
}

/// Payload side of the `opt_u32_as_i64` converter: `Option<u32>` widened into
/// a nullable `bigint`, so the bind direction cannot fail.
struct OptU32AsI64Payload {
    elapsed_ms: Option<u32>,
}

struct OptU32AsI64Row {
    elapsed_ms: crate::pg_sidecar_row_ty!(opt_u32_as_i64),
}

fn bind_opt_u32_as_i64(payload: &OptU32AsI64Payload) -> Option<i64> {
    crate::pg_sidecar_bind!((opt_u32_as_i64), payload, elapsed_ms)
}

// By value, like the owned `FromRow` row the generated `load_batch` decodes.
#[allow(clippy::needless_pass_by_value)]
fn decode_opt_u32_as_i64(row: OptU32AsI64Row) -> Result<Option<u32>, StorageError> {
    Ok(crate::pg_sidecar_decode!((opt_u32_as_i64), row, elapsed_ms))
}

#[test]
fn opt_u32_as_i64_binds_and_decodes_present_and_absent_values() {
    let cast: Option<&str> = crate::pg_sidecar_cast!(opt_u32_as_i64);
    assert_eq!(cast, None);
    assert_eq!(
        crate::pg_sidecar_select_col!((opt_u32_as_i64), elapsed_ms),
        "elapsed_ms"
    );

    assert_eq!(
        bind_opt_u32_as_i64(&OptU32AsI64Payload {
            elapsed_ms: Some(u32::MAX)
        }),
        Some(i64::from(u32::MAX))
    );
    assert_eq!(
        bind_opt_u32_as_i64(&OptU32AsI64Payload { elapsed_ms: None }),
        None
    );

    assert_eq!(
        decode_opt_u32_as_i64(OptU32AsI64Row {
            elapsed_ms: Some(i64::from(u32::MAX))
        })
        .unwrap(),
        Some(u32::MAX)
    );
    assert_eq!(
        decode_opt_u32_as_i64(OptU32AsI64Row { elapsed_ms: None }).unwrap(),
        None
    );
}

#[test]
fn opt_u32_as_i64_rejects_out_of_range_decodes() {
    // Bind widens and cannot fail, so only the read direction is guarded.
    for value in [-1, i64::from(u32::MAX) + 1] {
        let err = decode_opt_u32_as_i64(OptU32AsI64Row {
            elapsed_ms: Some(value),
        })
        .expect_err("bigint outside the u32 range is rejected on decode");
        assert!(
            matches!(err, StorageError::Internal(ref msg) if msg.contains("invalid elapsed_ms")),
            "value {value}, message: {err}"
        );
    }
}

/// Payload side of the `opt_u64_as_i64` converter: `Option<u64>`.
struct OptU64Payload {
    byte_count: Option<u64>,
}

struct OptU64Row {
    byte_count: crate::pg_sidecar_row_ty!(opt_u64_as_i64),
}

fn bind_opt_u64(payload: &OptU64Payload) -> Result<Option<i64>, StorageError> {
    Ok(crate::pg_sidecar_bind!(
        (opt_u64_as_i64),
        payload,
        byte_count
    ))
}

// By value, like the owned `FromRow` row the generated `load_batch` decodes.
#[allow(clippy::needless_pass_by_value)]
fn decode_opt_u64(row: OptU64Row) -> Result<Option<u64>, StorageError> {
    Ok(crate::pg_sidecar_decode!((opt_u64_as_i64), row, byte_count))
}

#[test]
fn opt_u64_as_i64_binds_and_decodes_present_and_absent_values() {
    let cast: Option<&str> = crate::pg_sidecar_cast!(opt_u64_as_i64);
    assert_eq!(cast, None);
    assert_eq!(
        crate::pg_sidecar_select_col!((opt_u64_as_i64), byte_count),
        "byte_count"
    );

    assert_eq!(
        bind_opt_u64(&OptU64Payload {
            byte_count: Some(4_096)
        })
        .unwrap(),
        Some(4_096_i64)
    );
    assert_eq!(
        bind_opt_u64(&OptU64Payload { byte_count: None }).unwrap(),
        None
    );

    assert_eq!(
        decode_opt_u64(OptU64Row {
            byte_count: Some(4_096)
        })
        .unwrap(),
        Some(4_096_u64)
    );
    assert_eq!(
        decode_opt_u64(OptU64Row { byte_count: None }).unwrap(),
        None
    );
}

#[test]
fn opt_u64_as_i64_rejects_out_of_range_values_in_both_directions() {
    // Bind side: a `u64` above `i64::MAX` errors rather than saturating.
    let err = bind_opt_u64(&OptU64Payload {
        byte_count: Some(u64::try_from(i64::MAX).expect("i64::MAX fits u64") + 1),
    })
    .expect_err("out-of-range u64 is rejected on bind");
    assert!(
        matches!(err, StorageError::ConstraintViolation(ref msg)
            if msg.contains("byte_count out of range")),
        "message: {err}"
    );

    // Decode side: a negative SQL `bigint` errors rather than wrapping.
    let err = decode_opt_u64(OptU64Row {
        byte_count: Some(-1),
    })
    .expect_err("negative bigint is rejected on decode");
    assert!(
        matches!(err, StorageError::Internal(ref msg) if msg.contains("invalid byte_count")),
        "message: {err}"
    );
}

#[test]
fn public_sidecar_batch_read_denies_core_schema_unless_registry_admits_it() {
    let sql = "SELECT memory_id FROM proxima_core.agent_note_v1 WHERE memory_id = ANY($1)";
    let err = validate_sidecar_read_sql(sql, false).expect_err("public helper denies core SQL");
    assert!(err.to_string().contains("proxima_core.*"), "message: {err}");
    validate_sidecar_read_sql(sql, true).expect("registered core sidecars may read core tables");
}
