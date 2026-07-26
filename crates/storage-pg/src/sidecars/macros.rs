#[macro_export]
macro_rules! pg_sidecar_row_ty {
    (uuid) => {
        ::uuid::Uuid
    };
    (uuid_array) => {
        ::std::vec::Vec<::uuid::Uuid>
    };
    (opt_uuid) => {
        ::std::option::Option<::uuid::Uuid>
    };
    (text) => {
        ::std::string::String
    };
    (opt_text) => {
        ::std::option::Option<::std::string::String>
    };
    (text_array) => {
        ::std::vec::Vec<::std::string::String>
    };
    (decimal) => {
        ::rust_decimal::Decimal
    };
    (opt_decimal) => {
        ::std::option::Option<::rust_decimal::Decimal>
    };
    (naive_date) => {
        ::time::Date
    };
    (opt_naive_date) => {
        ::std::option::Option<::time::Date>
    };
    (jsonb) => {
        ::serde_json::Value
    };
    (opt_jsonb) => {
        ::std::option::Option<::serde_json::Value>
    };
    (bool) => {
        bool
    };
    (f32) => {
        f32
    };
    (timestamptz) => {
        ::time::OffsetDateTime
    };
    (bytea32) => {
        ::std::vec::Vec<u8>
    };
    (u32_as_i32) => {
        i32
    };
    (u32_as_i32_saturating) => {
        i32
    };
    (u32_as_i64) => {
        i64
    };
    (u64_as_i64) => {
        i64
    };
    (u64_as_i64_saturating) => {
        i64
    };
    (enum { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }) => {
        ::std::string::String
    };
    (enum_copy { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }) => {
        ::std::string::String
    };
}

#[macro_export]
macro_rules! pg_sidecar_cast {
    (decimal) => {
        ::std::option::Option::Some("numeric")
    };
    (opt_decimal) => {
        ::std::option::Option::Some("numeric")
    };
    (naive_date) => {
        ::std::option::Option::Some("date")
    };
    (opt_naive_date) => {
        ::std::option::Option::Some("date")
    };
    (jsonb) => {
        ::std::option::Option::Some("jsonb")
    };
    (opt_jsonb) => {
        ::std::option::Option::Some("jsonb")
    };
    (enum { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }) => {
        ::std::option::Option::Some($pg_type)
    };
    (enum_copy { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }) => {
        ::std::option::Option::Some($pg_type)
    };
    ($kind:ident) => {
        ::std::option::Option::None
    };
}

/// Per-column SELECT expression for the batch read. Enum-typed columns are a
/// PG enum on the read side, so they must be cast back to `text` (aliased to
/// the field name) for the `String`-typed `FromRow` decode; all other kinds
/// select the bare column.
#[macro_export]
macro_rules! pg_sidecar_select_col {
    (
        (enum { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }),
        $column:ident
    ) => {
        ::std::concat!(
            ::std::stringify!($column),
            "::text AS ",
            ::std::stringify!($column)
        )
    };
    (
        (enum_copy { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }),
        $column:ident
    ) => {
        ::std::concat!(
            ::std::stringify!($column),
            "::text AS ",
            ::std::stringify!($column)
        )
    };
    ((decimal), $column:ident) => {
        ::std::stringify!($column)
    };
    ((opt_decimal), $column:ident) => {
        ::std::stringify!($column)
    };
    ((naive_date), $column:ident) => {
        ::std::stringify!($column)
    };
    ((opt_naive_date), $column:ident) => {
        ::std::stringify!($column)
    };
    ((jsonb), $column:ident) => {
        ::std::stringify!($column)
    };
    ((opt_jsonb), $column:ident) => {
        ::std::stringify!($column)
    };
    (($other:tt), $column:ident) => {
        ::std::stringify!($column)
    };
}

#[macro_export]
macro_rules! pg_sidecar_bind {
    ((uuid), $self:ident, $field:ident) => {
        $self.$field
    };
    ((uuid_array), $self:ident, $field:ident) => {
        &$self.$field
    };
    ((opt_uuid), $self:ident, $field:ident) => {
        $self.$field
    };
    ((text), $self:ident, $field:ident) => {
        &$self.$field
    };
    ((opt_text), $self:ident, $field:ident) => {
        $self.$field.as_deref()
    };
    ((text_array), $self:ident, $field:ident) => {
        &$self.$field
    };
    ((decimal), $self:ident, $field:ident) => {
        $self.$field
    };
    ((opt_decimal), $self:ident, $field:ident) => {
        $self.$field
    };
    ((naive_date), $self:ident, $field:ident) => {
        $self.$field
    };
    ((opt_naive_date), $self:ident, $field:ident) => {
        $self.$field
    };
    ((jsonb), $self:ident, $field:ident) => {
        &$self.$field
    };
    ((opt_jsonb), $self:ident, $field:ident) => {
        $self.$field.as_ref()
    };
    ((bool), $self:ident, $field:ident) => {
        $self.$field
    };
    ((f32), $self:ident, $field:ident) => {
        $self.$field
    };
    ((timestamptz), $self:ident, $field:ident) => {
        $self.$field
    };
    ((bytea32), $self:ident, $field:ident) => {
        $self.$field.to_vec()
    };
    ((u32_as_i32), $self:ident, $field:ident) => {
        <::std::primitive::i32 as ::std::convert::TryFrom<_>>::try_from($self.$field).map_err(
            |err| {
                $crate::core::StorageError::ConstraintViolation(::std::format!(
                    "{} out of range: {err}",
                    ::std::stringify!($field)
                ))
            },
        )?
    };
    ((u32_as_i32_saturating), $self:ident, $field:ident) => {
        <::std::primitive::i32 as ::std::convert::TryFrom<_>>::try_from($self.$field)
            .unwrap_or(::std::primitive::i32::MAX)
    };
    ((u32_as_i64), $self:ident, $field:ident) => {
        ::std::primitive::i64::from($self.$field)
    };
    ((u64_as_i64), $self:ident, $field:ident) => {
        <::std::primitive::i64 as ::std::convert::TryFrom<_>>::try_from($self.$field).map_err(
            |err| {
                $crate::core::StorageError::ConstraintViolation(::std::format!(
                    "{} out of range: {err}",
                    ::std::stringify!($field)
                ))
            },
        )?
    };
    ((u64_as_i64_saturating), $self:ident, $field:ident) => {
        <::std::primitive::i64 as ::std::convert::TryFrom<_>>::try_from($self.$field)
            .unwrap_or(::std::primitive::i64::MAX)
    };
    (
        (
            enum { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }
        ),
        $self:ident,
        $field:ident
    ) => {
        $to_str(&$self.$field)
    };
    (
        (
            enum_copy { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }
        ),
        $self:ident,
        $field:ident
    ) => {
        $to_str($self.$field)
    };
}

#[macro_export]
macro_rules! pg_sidecar_decode {
    ((uuid), $row:ident, $field:ident) => {
        $row.$field
    };
    ((uuid_array), $row:ident, $field:ident) => {
        $row.$field
    };
    ((opt_uuid), $row:ident, $field:ident) => {
        $row.$field
    };
    ((text), $row:ident, $field:ident) => {
        $row.$field
    };
    ((opt_text), $row:ident, $field:ident) => {
        $row.$field
    };
    ((text_array), $row:ident, $field:ident) => {
        $row.$field
    };
    ((decimal), $row:ident, $field:ident) => {
        $row.$field
    };
    ((opt_decimal), $row:ident, $field:ident) => {
        $row.$field
    };
    ((naive_date), $row:ident, $field:ident) => {
        $row.$field
    };
    ((opt_naive_date), $row:ident, $field:ident) => {
        $row.$field
    };
    ((jsonb), $row:ident, $field:ident) => {
        $row.$field
    };
    ((opt_jsonb), $row:ident, $field:ident) => {
        $row.$field
    };
    ((bool), $row:ident, $field:ident) => {
        $row.$field
    };
    ((f32), $row:ident, $field:ident) => {
        $row.$field
    };
    ((timestamptz), $row:ident, $field:ident) => {
        $row.$field
    };
    ((bytea32), $row:ident, $field:ident) => {
        $crate::sidecars::bytes32(&$row.$field, ::std::stringify!($field))?
    };
    ((u32_as_i32), $row:ident, $field:ident) => {
        $crate::sidecars::int_to_u32($row.$field, ::std::stringify!($field))?
    };
    ((u32_as_i32_saturating), $row:ident, $field:ident) => {
        $crate::sidecars::int_to_u32($row.$field, ::std::stringify!($field))?
    };
    ((u32_as_i64), $row:ident, $field:ident) => {
        <::std::primitive::u32 as ::std::convert::TryFrom<_>>::try_from($row.$field).map_err(
            |err| {
                $crate::core::StorageError::Internal(::std::format!(
                    "invalid {}: {err}",
                    ::std::stringify!($field)
                ))
            },
        )?
    };
    ((u64_as_i64), $row:ident, $field:ident) => {
        $crate::sidecars::int_to_u64($row.$field, ::std::stringify!($field))?
    };
    ((u64_as_i64_saturating), $row:ident, $field:ident) => {
        $crate::sidecars::int_to_u64($row.$field, ::std::stringify!($field))?
    };
    (
        (
            enum { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }
        ),
        $row:ident,
        $field:ident
    ) => {
        ($from_str)($row.$field.as_str())?
    };
    (
        (
            enum_copy { to_str: $to_str:path, pg_type: $pg_type:literal, from_str: $from_str:expr }
        ),
        $row:ident,
        $field:ident
    ) => {
        ($from_str)($row.$field.as_str())?
    };
}

#[macro_export]
macro_rules! pg_sidecar_payload {
    (@wrap Fact, $payload:expr) => {
        $crate::core::SidecarPayload::fact($payload)
    };
    (@wrap Abstraction, $payload:expr) => {
        $crate::core::SidecarPayload::abstraction($payload)
    };
    (@wrap Perspective, $payload:expr) => {
        $crate::core::SidecarPayload::perspective($payload)
    };
    ($payload_ty:path, $kind:expr, $payload:expr, [$($payload_kind:ident),+ $(,)?]) => {{
        match $kind {
            $(
                $crate::core::verbs::schema::PayloadKind::$payload_kind => {
                    ::std::result::Result::Ok($crate::pg_sidecar_payload!(@wrap $payload_kind, $payload))
                }
            )+
            other => ::std::result::Result::Err($crate::core::StorageError::ConstraintViolation(::std::format!(
                "payload kind {other:?} is not valid for {}",
                ::std::any::type_name::<$payload_ty>(),
            ))),
        }
    }};
}

#[macro_export]
macro_rules! pg_sidecar {
    (
        payload: $($payload_ty:ident)::+,
        row: $row_ty:ident,
        kinds: [$($payload_kind:ident),+ $(,)?],
        table: $table:literal,
        key: $key_column:ident,
        fields: {
            $(
                $field:ident => $column:ident : $column_kind:tt
            ),+ $(,)?
        } $(,)?
    ) => {
        impl $crate::sidecars::PgMemorySidecar for $($payload_ty)::+ {
            fn insert_memory_sidecar<'t>(
                &'t self,
                tx: &'t mut ::sqlx::Transaction<'_, ::sqlx::Postgres>,
                memory_id: $crate::core::MemoryId,
            ) -> $crate::sidecars::PgSidecarFuture<'t> {
                ::std::boxed::Box::pin(async move {
                    let sql = $crate::sidecars::memory_insert_sql(
                        $table,
                        ::std::stringify!($key_column),
                        &[$(
                            (::std::stringify!($column), $crate::pg_sidecar_cast! $column_kind),
                        )+],
                    )?;
                    // SQL-POLICY: PgIdent — `sql` is built by memory_insert_sql
                    // from macro-literal table/column names validated as PgIdent;
                    // every value below is bound.
                    ::sqlx::query(::sqlx::AssertSqlSafe(sql))
                        .bind(memory_id.into_inner())
                        $(
                            .bind($crate::pg_sidecar_bind!($column_kind, self, $field))
                        )+
                        .execute(tx.as_mut())
                        .await
                        .map_err($crate::map_err)?;
                    ::std::result::Result::Ok(())
                })
            }
        }

        #[derive(Debug, ::sqlx::FromRow)]
        struct $row_ty {
            $key_column: ::uuid::Uuid,
            $(
                $field: $crate::pg_sidecar_row_ty! $column_kind,
            )+
        }

        impl $crate::sidecars::PgMemoryPayload for $($payload_ty)::+ {
            fn load_batch<'t>(
                ctx: $crate::sidecars::PgSidecarReadCtx<'t>,
                kind: $crate::core::verbs::schema::PayloadKind,
                memory_ids: &'t [$crate::core::MemoryId],
            ) -> $crate::sidecars::PgMemoryPayloadBatchFuture<'t> {
                ::std::boxed::Box::pin(async move {
                    if memory_ids.is_empty() {
                        return ::std::result::Result::Ok(::std::vec::Vec::new());
                    }
                    let sql = $crate::sidecars::memory_select_batch_sql(
                        $table,
                        ::std::stringify!($key_column),
                        &[$($crate::pg_sidecar_select_col!($column_kind, $column)),+],
                    )?;
                    let rows = ctx.fetch_all_by_memory_ids::<$row_ty>(&sql, memory_ids).await?;
                    rows.into_iter()
                        .map(|row| {
                            let memory_id = $crate::core::MemoryId::new(row.$key_column);
                            let payload = $($payload_ty)::+ {
                                $(
                                    $field: $crate::pg_sidecar_decode!($column_kind, row, $field),
                                )+
                            };
                            ::std::result::Result::Ok((
                                memory_id,
                                $crate::pg_sidecar_payload!(
                                    $($payload_ty)::+,
                                    kind,
                                    payload,
                                    [$($payload_kind),+]
                                )?,
                            ))
                        })
                        .collect::<::std::result::Result<::std::vec::Vec<_>, $crate::core::StorageError>>()
                })
            }
        }
    };
}

#[macro_export]
macro_rules! goal_lifecycle_fact {
    ($($payload_ty:ident)::+, $row_ty:ident, $table:literal) => {
        $crate::pg_sidecar! {
            payload: $($payload_ty)::+,
            row: $row_ty,
            kinds: [Fact],
            table: $table,
            key: memory_id,
            fields: {
                goal_id => goal_id: (uuid),
                transitioned_at => transitioned_at: (timestamptz),
            },
        }
    };
}
