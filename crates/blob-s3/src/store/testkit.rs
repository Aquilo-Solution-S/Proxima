//! Fixtures shared by the unit tests under `store/`.
//!
//! Nothing here connects to anything: `lazy_test_pool` is deliberately lazy
//! so a construction test can build a store without a database, which is
//! what makes the config validation in `CitedBlobStore::new` assertable in
//! a plain `cargo test`.

use proxima_core::{OwnerRef, UserId};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use super::dto::CitedBlobUploadPrepareTs;
use crate::config::S3RuntimeConfig;

pub(super) fn prepare_req(byte_len: u64) -> CitedBlobUploadPrepareTs {
    CitedBlobUploadPrepareTs {
        owner: OwnerRef::Personal(UserId::new(Uuid::now_v7())),
        filename: "test.pdf".into(),
        mime: "application/pdf".into(),
        byte_len,
    }
}

pub(super) fn store_config(
    endpoint_url: Option<&str>,
    max_blob_bytes: Option<u64>,
) -> S3RuntimeConfig {
    S3RuntimeConfig {
        bucket: "test-bucket".into(),
        region: "eu-central-1".into(),
        endpoint_url: endpoint_url.map(ToOwned::to_owned),
        force_path_style: false,
        upload_ttl_seconds: 900,
        read_ttl_seconds: 300,
        max_blob_bytes,
    }
}

pub(super) fn lazy_test_pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .connect_lazy("postgres://postgres@localhost/proxima_blob_test")
        .expect("test database URL is valid")
}
