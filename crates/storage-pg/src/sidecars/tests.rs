use super::{
    StorageError, memory_insert_sql, memory_select_batch_owner_pinned_sql, memory_select_batch_sql,
    validate_sidecar_read_sql,
};

#[test]
fn memory_insert_sql_validates_identifiers() {
    let sql = memory_insert_sql(
        "proxima_core.agent_note_v1",
        "memory_id",
        &[("title", Some("text")), ("tags", None)],
        false,
    )
    .unwrap();

    assert_eq!(
        sql,
        "INSERT INTO proxima_core.agent_note_v1 (memory_id, title, tags) \
             VALUES ($1, $2::text, $3)"
    );
    assert!(
        memory_insert_sql(
            "proxima_core.agent_note_v1; DROP TABLE x",
            "memory_id",
            &[],
            false
        )
        .is_err()
    );
    assert!(memory_insert_sql("proxima_core.agent_note_v1", "memory-id", &[], false).is_err());
    assert!(
        memory_insert_sql(
            "proxima_core.agent_note_v1",
            "memory_id",
            &[("x); DROP TABLE y", None)],
            false
        )
        .is_err()
    );
}

/// An owner-pinned insert takes its `owner_id` from the Memory row in the
/// same statement. The value is therefore never a bind: no caller can
/// choose which owner an audit row is attributed to, and the row is stamped
/// with the owner that held the Memory at the moment of the write rather
/// than whoever holds it later.
#[test]
fn owner_pinned_insert_reads_the_owner_from_the_memory_row() {
    let sql = memory_insert_sql(
        "proxima_core.mcp_call_logged_v1",
        "t",
        &[("tool_name", None), ("ok", None)],
        true,
    )
    .unwrap();

    assert_eq!(
        sql,
        "INSERT INTO proxima_core.mcp_call_logged_v1 (t, owner_id, tool_name, ok) \
             SELECT $1, m.owner_id, $2, $3 FROM proxima_core.memory m WHERE m.t = $1"
    );
    // The owner is not among the bound values.
    assert!(!sql.contains("VALUES"));
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

/// An owner-pinned read hydrates a row only while the Memory is still held
/// by the owner that wrote the row. The join IS the rule: drop it and a
/// transfer destination hydrates the prior owner's `actor_upn` into its own
/// `get_memory` payload.
#[test]
fn owner_pinned_select_hydrates_only_while_the_memory_has_not_moved() {
    let sql = memory_select_batch_owner_pinned_sql(
        "proxima_core.mcp_call_logged_v1",
        "t",
        &["tool_name", "actor_upn"],
    )
    .unwrap();

    assert_eq!(
        sql,
        "SELECT s.t, s.tool_name, s.actor_upn FROM proxima_core.mcp_call_logged_v1 s \
           JOIN proxima_core.memory m \
             ON m.t = s.t AND m.owner_id = s.owner_id \
          WHERE s.t = ANY($1)"
    );

    // Enum projections keep working under the alias.
    let enum_sql = memory_select_batch_owner_pinned_sql(
        "proxima_core.mcp_call_logged_v1",
        "t",
        &["state::text AS state"],
    )
    .unwrap();
    assert!(enum_sql.contains("s.state::text AS state"));
    assert!(memory_select_batch_owner_pinned_sql("bad table;", "t", &["x"]).is_err());
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

/// Parity pin for the RA-11 unification.
///
/// `owner_pinned` was declared in `pg_sidecar!` and consumed only by the
/// Postgres adapter, which appended it to the owner-inverse table lists on the
/// way past — a fifth leg core never saw. The engine now builds all five
/// legs from the flavor contracts, and `freeze_against` refuses a
/// registration whose macro flag contradicts its schema's transfer rule.
/// The literal below is the whole owner-pinned set as of v0.0.8.
#[test]
fn the_owner_pinned_set_is_the_contracts_retain_at_source_set() {
    let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
    let sidecars = crate::sidecars::core_pg_sidecars();

    let expected = vec!["proxima_core.mcp_call_logged_v1".to_owned()];

    assert_eq!(
        registry.retain_at_source_sidecar_tables(),
        expected,
        "core/mcp-call-logged-v1 is the only schema whose rows stay with \
         the writing owner across a transfer"
    );
    assert_eq!(
        sidecars.owner_pinned_memory_sidecar_tables(),
        expected,
        "the pg_sidecar! flag and the contract are two statements of one fact"
    );
}

/// The second half of the same unification, for the forget.
///
/// `ForgetRule::Keep` on a memory sidecar says the forget leaves its rows
/// alone. Nothing in the forget reads it: both walks — the dump and the
/// delete — test `is_owner_pinned` and nothing else. So `Keep` is honoured
/// exactly when the sidecar is also owner-pinned, and `freeze_against`'s
/// `check_keep_is_owner_pinned` now refuses any registry where the two
/// disagree in either direction.
///
/// This is the literal set as of v0.0.8, and it is the same one line for
/// line as the transfer sibling above — which is the point.
/// `mcp_call_logged_v1` is `RetainAtSource` AND `Keep` AND owner-pinned
/// because all three are the same fact about an audit row: it belongs to the
/// owner that acted, not to the memory.
#[test]
fn the_kept_memory_sidecar_set_is_the_owner_pinned_set() {
    let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
    let surfaces = proxima_core::owner_inverse::OwnerSurfaces::for_registry(&registry);
    let sidecars = crate::sidecars::core_pg_sidecars();

    let expected = vec!["proxima_core.mcp_call_logged_v1".to_owned()];

    let kept = sidecars
        .memory_sidecar_tables()
        .into_iter()
        .filter(|table| {
            matches!(
                surfaces.forget_leg(table),
                proxima_core::flavor::ForgetLeg::Kept { .. }
            )
        })
        .map(ToOwned::to_owned)
        .collect::<Vec<String>>();

    assert_eq!(
        kept, expected,
        "core/mcp-call-logged-v1 is the only memory sidecar the forget is \
         declared to leave alone"
    );
    assert_eq!(
        sidecars.owner_pinned_memory_sidecar_tables(),
        expected,
        "and owner-pinning is the only mechanism by which the forget can \
         honour that declaration"
    );
}
