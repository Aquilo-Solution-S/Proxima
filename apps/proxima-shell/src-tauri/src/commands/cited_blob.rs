use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::Region;
use aws_sdk_s3::presigning::PresigningConfig;
use proxima_core::{Owner, Principal, UPLOADED_BLOB_SCHEMA_ID};
use proxima_storage_pg::PgStorage;
use sha2::{Digest, Sha256};
use sqlx::Row;
use tauri::State;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use crate::command_error::CommandError;

const DEFAULT_UPLOAD_TTL_SECONDS: u64 = 900;
const DEFAULT_READ_TTL_SECONDS: u64 = 300;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CitedBlobUploadPrepareTs {
    pub owner: Owner,
    pub filename: String,
    pub mime: String,
    pub byte_len: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CitedBlobUploadPrepareOutcomeTs {
    pub upload_id: String,
    pub upload_url: String,
    pub expires_at: String,
    pub headers: Vec<PresignedHeaderTs>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct PresignedHeaderTs {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CitedBlobUploadCompleteTs {
    pub owner: Owner,
    pub upload_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CitedBlobUploadCompleteOutcomeTs {
    pub cited_object_id: String,
    pub schema: String,
    pub content_hash: String,
    pub sha256: String,
    pub byte_len: u64,
    pub mime: String,
    pub filename: String,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CitedBlobUploadAbortTs {
    pub owner: Owner,
    pub upload_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CitedBlobUploadAbortOutcomeTs {
    pub aborted: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CitedBlobReadUrlTs {
    pub owner: Owner,
    pub cited_object_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct CitedBlobReadUrlOutcomeTs {
    pub read_url: String,
    pub expires_at: String,
}

#[tauri::command]
#[specta::specta]
pub async fn cited_blob_upload_prepare(
    req: CitedBlobUploadPrepareTs,
    pg: State<'_, PgStorage>,
) -> Result<CitedBlobUploadPrepareOutcomeTs, CommandError> {
    validate_prepare(&req)?;
    let cfg = S3RuntimeConfig::from_env()?;
    let upload_id = Uuid::now_v7();
    let owner_hash = owner_hash_hex(&req.owner);
    let object_key = pending_object_key(&owner_hash, upload_id);
    let expires_at = OffsetDateTime::now_utc()
        + time::Duration::seconds(i64::try_from(cfg.upload_ttl_seconds).unwrap_or(i64::MAX));
    let client = cfg.client().await?;
    let presigned = client
        .put_object()
        .bucket(&cfg.bucket)
        .key(&object_key)
        .content_type(&req.mime)
        .presigned(presign_config(cfg.upload_ttl_seconds)?)
        .await
        .map_err(|e| CommandError::s3(format!("prepare upload URL failed: {e}")))?;

    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    sqlx::query(
        "INSERT INTO proxima_core.cited_object_uploads \
            (owner_principal_kind, owner_principal_id, owner_org_id, upload_id, \
             bucket, object_key, filename, mime, expected_byte_len, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(upload_id)
    .bind(&cfg.bucket)
    .bind(&object_key)
    .bind(req.filename.trim())
    .bind(req.mime.trim())
    .bind(
        i64::try_from(req.byte_len)
            .map_err(|_| CommandError::cited_object_upload("byte_len exceeds Postgres bigint"))?,
    )
    .bind(expires_at)
    .execute(pg.pool())
    .await
    .map_err(|e| CommandError::cited_object_upload(e.to_string()))?;

    Ok(CitedBlobUploadPrepareOutcomeTs {
        upload_id: upload_id.to_string(),
        upload_url: presigned.uri().to_string(),
        expires_at: format_time(expires_at)?,
        headers: presigned
            .headers()
            .filter_map(|(name, value)| {
                if name.eq_ignore_ascii_case("host") {
                    return None;
                }
                Some(PresignedHeaderTs {
                    name: name.to_string(),
                    value: value.to_string(),
                })
            })
            .collect(),
    })
}

#[tauri::command]
#[specta::specta]
pub async fn cited_blob_upload_complete(
    req: CitedBlobUploadCompleteTs,
    pg: State<'_, PgStorage>,
) -> Result<CitedBlobUploadCompleteOutcomeTs, CommandError> {
    let upload_id = parse_uuid(&req.upload_id)?;
    let cfg = S3RuntimeConfig::from_env()?;
    let row = load_upload(pg.pool(), &req.owner, upload_id).await?;
    match row.status.as_str() {
        "completed" => {
            let Some(cited_object_id) = row.cited_object_id else {
                return Err(CommandError::cited_object_upload(
                    "completed upload is missing cited_object_id",
                ));
            };
            return load_completed_blob(pg.pool(), &req.owner, cited_object_id, true).await;
        }
        "aborted" => {
            return Err(CommandError::cited_object_upload("upload is aborted"));
        }
        "expired" => {
            return Err(CommandError::cited_object_upload("upload is expired"));
        }
        "pending" => {}
        other => {
            return Err(CommandError::cited_object_upload(format!(
                "unknown upload status {other}"
            )));
        }
    }
    if row.expires_at < OffsetDateTime::now_utc() {
        mark_upload_expired(pg.pool(), &req.owner, upload_id).await?;
        return Err(CommandError::cited_object_upload("upload is expired"));
    }

    let client = cfg.client().await?;
    let object = client
        .get_object()
        .bucket(&row.bucket)
        .key(&row.object_key)
        .send()
        .await
        .map_err(|e| CommandError::s3(format!("read pending upload failed: {e}")))?;
    if let Some(len) = object.content_length()
        && len != row.expected_byte_len
    {
        return Err(CommandError::cited_object_upload(format!(
            "uploaded byte length {len} does not match expected {}",
            row.expected_byte_len
        )));
    }

    let streamed = Box::pin(hash_uploaded_object(object.body, row.expected_byte_len)).await?;
    let owner_hash = owner_hash_hex(&req.owner);
    let canonical_key = canonical_object_key(&owner_hash, &streamed.blake3_hex);
    let copy_source = format!("{}/{}", row.bucket, row.object_key);
    let copy_result = client
        .copy_object()
        .bucket(&row.bucket)
        .key(&canonical_key)
        .copy_source(copy_source)
        .send()
        .await
        .map_err(|e| CommandError::s3(format!("copy uploaded object failed: {e}")))?;
    let etag = copy_result
        .copy_object_result()
        .and_then(|r| r.e_tag())
        .map(ToString::to_string);

    client
        .delete_object()
        .bucket(&row.bucket)
        .key(&row.object_key)
        .send()
        .await
        .map_err(|e| CommandError::s3(format!("delete pending upload failed: {e}")))?;

    let completed = persist_completed_blob(
        pg.pool(),
        &req.owner,
        upload_id,
        &row,
        &canonical_key,
        &streamed,
        etag.as_deref(),
    )
    .await?;
    load_completed_blob(
        pg.pool(),
        &req.owner,
        completed.cited_object_id,
        completed.idempotent_replay,
    )
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn cited_blob_upload_abort(
    req: CitedBlobUploadAbortTs,
    pg: State<'_, PgStorage>,
) -> Result<CitedBlobUploadAbortOutcomeTs, CommandError> {
    let upload_id = parse_uuid(&req.upload_id)?;
    let row = load_upload(pg.pool(), &req.owner, upload_id).await?;
    if row.status == "completed" {
        return Ok(CitedBlobUploadAbortOutcomeTs { aborted: false });
    }
    if row.status == "aborted" || row.status == "expired" {
        return Ok(CitedBlobUploadAbortOutcomeTs { aborted: true });
    }

    let cfg = S3RuntimeConfig::from_env()?;
    let client = cfg.client().await?;
    client
        .delete_object()
        .bucket(&row.bucket)
        .key(&row.object_key)
        .send()
        .await
        .map_err(|e| CommandError::s3(format!("delete pending upload failed: {e}")))?;

    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(&req.owner);
    sqlx::query(
        "UPDATE proxima_core.cited_object_uploads \
            SET status = 'aborted', aborted_at = now() \
          WHERE owner_principal_kind = $1 \
            AND owner_principal_id = $2 \
            AND owner_org_id = $3 \
            AND upload_id = $4 \
            AND status = 'pending'",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(upload_id)
    .execute(pg.pool())
    .await
    .map_err(|e| CommandError::cited_object_upload(e.to_string()))?;

    Ok(CitedBlobUploadAbortOutcomeTs { aborted: true })
}

#[tauri::command]
#[specta::specta]
pub async fn cited_blob_read_url(
    req: CitedBlobReadUrlTs,
    pg: State<'_, PgStorage>,
) -> Result<CitedBlobReadUrlOutcomeTs, CommandError> {
    let cited_object_id = parse_uuid(&req.cited_object_id)?;
    let row = load_blob_location(pg.pool(), &req.owner, cited_object_id).await?;
    let cfg = S3RuntimeConfig::from_env()?;
    let client = cfg.client().await?;
    let expires_at = OffsetDateTime::now_utc()
        + time::Duration::seconds(i64::try_from(cfg.read_ttl_seconds).unwrap_or(i64::MAX));
    let presigned = client
        .get_object()
        .bucket(&row.bucket)
        .key(&row.object_key)
        .presigned(presign_config(cfg.read_ttl_seconds)?)
        .await
        .map_err(|e| CommandError::s3(format!("prepare read URL failed: {e}")))?;
    Ok(CitedBlobReadUrlOutcomeTs {
        read_url: presigned.uri().to_string(),
        expires_at: format_time(expires_at)?,
    })
}

#[derive(Debug, Clone)]
struct S3RuntimeConfig {
    bucket: String,
    region: String,
    endpoint_url: Option<String>,
    force_path_style: bool,
    upload_ttl_seconds: u64,
    read_ttl_seconds: u64,
}

impl S3RuntimeConfig {
    fn from_env() -> Result<Self, CommandError> {
        let bucket = required_env("PROXIMA_S3_BUCKET")?;
        let region = required_env("PROXIMA_S3_REGION")?;
        Ok(Self {
            bucket,
            region,
            endpoint_url: optional_env("PROXIMA_S3_ENDPOINT_URL"),
            force_path_style: parse_bool_env("PROXIMA_S3_FORCE_PATH_STYLE")?,
            upload_ttl_seconds: parse_u64_env(
                "PROXIMA_S3_UPLOAD_TTL_SECONDS",
                DEFAULT_UPLOAD_TTL_SECONDS,
            )?,
            read_ttl_seconds: parse_u64_env(
                "PROXIMA_S3_READ_TTL_SECONDS",
                DEFAULT_READ_TTL_SECONDS,
            )?,
        })
    }

    async fn client(&self) -> Result<Client, CommandError> {
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(self.region.clone()));
        if let Some(endpoint_url) = &self.endpoint_url {
            loader = loader.endpoint_url(endpoint_url);
        }
        let shared = loader.load().await;
        let mut builder = aws_sdk_s3::config::Builder::from(&shared);
        if self.force_path_style {
            builder = builder.force_path_style(true);
        }
        Ok(Client::from_conf(builder.build()))
    }
}

#[derive(Debug, Clone)]
struct UploadRow {
    bucket: String,
    object_key: String,
    filename: String,
    mime: String,
    expected_byte_len: i64,
    status: String,
    cited_object_id: Option<Uuid>,
    expires_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
struct StreamedObject {
    blake3: [u8; 32],
    blake3_hex: String,
    sha256: [u8; 32],
    byte_len: u64,
}

#[derive(Debug, Clone)]
struct CompletedBlob {
    cited_object_id: Uuid,
    idempotent_replay: bool,
}

#[derive(Debug, Clone)]
struct BlobLocation {
    bucket: String,
    object_key: String,
}

async fn load_upload(
    pool: &sqlx::PgPool,
    owner: &Owner,
    upload_id: Uuid,
) -> Result<UploadRow, CommandError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let row = sqlx::query(
        "SELECT bucket, object_key, filename, mime, expected_byte_len, \
                status::text AS status, cited_object_id, expires_at \
           FROM proxima_core.cited_object_uploads \
          WHERE owner_principal_kind = $1 \
            AND owner_principal_id = $2 \
            AND owner_org_id = $3 \
            AND upload_id = $4",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(upload_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| CommandError::cited_object_upload(e.to_string()))?
    .ok_or_else(|| CommandError::cited_object_upload("upload not found for Owner"))?;

    Ok(UploadRow {
        bucket: row.get("bucket"),
        object_key: row.get("object_key"),
        filename: row.get("filename"),
        mime: row.get("mime"),
        expected_byte_len: row.get("expected_byte_len"),
        status: row.get("status"),
        cited_object_id: row.get("cited_object_id"),
        expires_at: row.get("expires_at"),
    })
}

async fn mark_upload_expired(
    pool: &sqlx::PgPool,
    owner: &Owner,
    upload_id: Uuid,
) -> Result<(), CommandError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    sqlx::query(
        "UPDATE proxima_core.cited_object_uploads \
            SET status = 'expired', error_message = 'upload expired' \
          WHERE owner_principal_kind = $1 \
            AND owner_principal_id = $2 \
            AND owner_org_id = $3 \
            AND upload_id = $4 \
            AND status = 'pending'",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(upload_id)
    .execute(pool)
    .await
    .map_err(|e| CommandError::cited_object_upload(e.to_string()))?;
    Ok(())
}

async fn hash_uploaded_object(
    body: aws_sdk_s3::primitives::ByteStream,
    expected_byte_len: i64,
) -> Result<StreamedObject, CommandError> {
    let mut reader = body.into_async_read();
    let mut blake3_hasher = blake3::Hasher::new();
    let mut sha256_hasher = Sha256::new();
    let mut buf = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut byte_len = 0_u64;
    loop {
        let n = reader
            .read(&mut buf)
            .await
            .map_err(|e| CommandError::s3(format!("stream pending upload failed: {e}")))?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        blake3_hasher.update(chunk);
        sha256_hasher.update(chunk);
        byte_len = byte_len
            .checked_add(u64::try_from(n).unwrap_or(u64::MAX))
            .ok_or_else(|| CommandError::cited_object_upload("uploaded object is too large"))?;
    }
    if i64::try_from(byte_len).unwrap_or(i64::MAX) != expected_byte_len {
        return Err(CommandError::cited_object_upload(format!(
            "uploaded byte length {byte_len} does not match expected {expected_byte_len}"
        )));
    }
    let blake3 = *blake3_hasher.finalize().as_bytes();
    let sha256: [u8; 32] = sha256_hasher.finalize().into();
    Ok(StreamedObject {
        blake3,
        blake3_hex: hex::encode(blake3),
        sha256,
        byte_len,
    })
}

#[allow(clippy::too_many_arguments)]
async fn persist_completed_blob(
    pool: &sqlx::PgPool,
    owner: &Owner,
    upload_id: Uuid,
    upload: &UploadRow,
    canonical_key: &str,
    streamed: &StreamedObject,
    etag: Option<&str>,
) -> Result<CompletedBlob, CommandError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| CommandError::cited_object_upload(e.to_string()))?;

    let row = sqlx::query(
        "WITH ins AS ( \
             INSERT INTO proxima_core.cited_objects \
                 (cited_object_id, schema_id, owner_principal_kind, \
                  owner_principal_id, owner_org_id, content_hash) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (owner_principal_kind, owner_principal_id, owner_org_id, schema_id, content_hash) \
             DO NOTHING \
             RETURNING cited_object_id \
         ) \
         SELECT cited_object_id, false AS idempotent_replay FROM ins \
         UNION ALL \
         SELECT cited_object_id, true AS idempotent_replay \
           FROM proxima_core.cited_objects \
          WHERE owner_principal_kind = $3 \
            AND owner_principal_id = $4 \
            AND owner_org_id = $5 \
            AND schema_id = $2 \
            AND content_hash = $6 \
            AND NOT EXISTS (SELECT 1 FROM ins) \
          LIMIT 1",
    )
    .bind(Uuid::now_v7())
    .bind(UPLOADED_BLOB_SCHEMA_ID)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(&streamed.blake3[..])
    .fetch_one(tx.as_mut())
    .await
    .map_err(|e| CommandError::cited_object_upload(e.to_string()))?;
    let cited_object_id: Uuid = row.get("cited_object_id");
    let idempotent_replay: bool = row.get("idempotent_replay");

    sqlx::query(
        "INSERT INTO proxima_core.cited_uploaded_blob_v1 \
            (cited_object_id, bucket, object_key, sha256, byte_len, mime, filename, etag) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (cited_object_id) DO NOTHING",
    )
    .bind(cited_object_id)
    .bind(&upload.bucket)
    .bind(canonical_key)
    .bind(&streamed.sha256[..])
    .bind(i64::try_from(streamed.byte_len).unwrap_or(i64::MAX))
    .bind(&upload.mime)
    .bind(&upload.filename)
    .bind(etag)
    .execute(tx.as_mut())
    .await
    .map_err(|e| CommandError::cited_object_upload(e.to_string()))?;

    sqlx::query(
        "UPDATE proxima_core.cited_object_uploads \
            SET status = 'completed', cited_object_id = $1, completed_at = now() \
          WHERE owner_principal_kind = $2 \
            AND owner_principal_id = $3 \
            AND owner_org_id = $4 \
            AND upload_id = $5",
    )
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(upload_id)
    .execute(tx.as_mut())
    .await
    .map_err(|e| CommandError::cited_object_upload(e.to_string()))?;

    tx.commit()
        .await
        .map_err(|e| CommandError::cited_object_upload(e.to_string()))?;
    Ok(CompletedBlob {
        cited_object_id,
        idempotent_replay,
    })
}

async fn load_completed_blob(
    pool: &sqlx::PgPool,
    owner: &Owner,
    cited_object_id: Uuid,
    idempotent_replay: bool,
) -> Result<CitedBlobUploadCompleteOutcomeTs, CommandError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let row = sqlx::query(
        "SELECT encode(co.content_hash, 'hex') AS content_hash, \
                encode(b.sha256, 'hex') AS sha256, b.byte_len, b.mime, b.filename \
           FROM proxima_core.cited_objects co \
           JOIN proxima_core.cited_uploaded_blob_v1 b USING (cited_object_id) \
          WHERE co.cited_object_id = $1 \
            AND co.owner_principal_kind = $2 \
            AND co.owner_principal_id = $3 \
            AND co.owner_org_id = $4 \
            AND co.schema_id = $5",
    )
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(UPLOADED_BLOB_SCHEMA_ID)
    .fetch_optional(pool)
    .await
    .map_err(|e| CommandError::cited_object_upload(e.to_string()))?
    .ok_or_else(|| CommandError::cited_object_upload("cited object not found for Owner"))?;
    let byte_len: i64 = row.get("byte_len");
    Ok(CitedBlobUploadCompleteOutcomeTs {
        cited_object_id: cited_object_id.to_string(),
        schema: UPLOADED_BLOB_SCHEMA_ID.to_string(),
        content_hash: row.get("content_hash"),
        sha256: row.get("sha256"),
        byte_len: u64::try_from(byte_len).unwrap_or(u64::MAX),
        mime: row.get("mime"),
        filename: row.get("filename"),
        idempotent_replay,
    })
}

async fn load_blob_location(
    pool: &sqlx::PgPool,
    owner: &Owner,
    cited_object_id: Uuid,
) -> Result<BlobLocation, CommandError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let row = sqlx::query(
        "SELECT b.bucket, b.object_key \
           FROM proxima_core.cited_objects co \
           JOIN proxima_core.cited_uploaded_blob_v1 b USING (cited_object_id) \
          WHERE co.cited_object_id = $1 \
            AND co.owner_principal_kind = $2 \
            AND co.owner_principal_id = $3 \
            AND co.owner_org_id = $4 \
            AND co.schema_id = $5",
    )
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(UPLOADED_BLOB_SCHEMA_ID)
    .fetch_optional(pool)
    .await
    .map_err(|e| CommandError::cited_object_upload(e.to_string()))?
    .ok_or_else(|| CommandError::cited_object_upload("cited object not found for Owner"))?;
    Ok(BlobLocation {
        bucket: row.get("bucket"),
        object_key: row.get("object_key"),
    })
}

fn presign_config(ttl_seconds: u64) -> Result<PresigningConfig, CommandError> {
    PresigningConfig::expires_in(Duration::from_secs(ttl_seconds))
        .map_err(|e| CommandError::s3_config(format!("invalid presign TTL: {e}")))
}

fn validate_prepare(req: &CitedBlobUploadPrepareTs) -> Result<(), CommandError> {
    if req.filename.trim().is_empty() {
        return Err(CommandError::cited_object_upload("filename is required"));
    }
    if req.mime.trim().is_empty() {
        return Err(CommandError::cited_object_upload("mime is required"));
    }
    if req.byte_len > i64::MAX as u64 {
        return Err(CommandError::cited_object_upload(
            "byte_len exceeds Postgres bigint",
        ));
    }
    Ok(())
}

fn parse_uuid(value: &str) -> Result<Uuid, CommandError> {
    Uuid::parse_str(value).map_err(|_| CommandError::InvalidUuid {
        value: value.to_string(),
    })
}

fn required_env(key: &str) -> Result<String, CommandError> {
    std::env::var(key)
        .map(|value| value.trim().to_string())
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CommandError::s3_config(format!("missing {key}")))
}

fn optional_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_bool_env(key: &str) -> Result<bool, CommandError> {
    let Some(value) = optional_env(key) else {
        return Ok(false);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(CommandError::s3_config(format!(
            "invalid boolean {key}={value}"
        ))),
    }
}

fn parse_u64_env(key: &str, default: u64) -> Result<u64, CommandError> {
    let Some(value) = optional_env(key) else {
        return Ok(default);
    };
    value
        .parse::<u64>()
        .map_err(|_| CommandError::s3_config(format!("invalid integer {key}={value}")))
}

fn owner_columns(owner: &Owner) -> (&'static str, Uuid, Uuid) {
    match &owner.principal {
        Principal::User(user) => ("User", user.into_inner(), owner.org_id.into_inner()),
        Principal::Group(group) => ("Group", group.into_inner(), owner.org_id.into_inner()),
    }
}

fn owner_hash_hex(owner: &Owner) -> String {
    let (kind, principal_id, org_id) = owner_columns(owner);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"proxima-owner-s3-key-v1\0");
    hasher.update(kind.as_bytes());
    hasher.update(b"\0");
    hasher.update(principal_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(org_id.as_bytes());
    hex::encode(hasher.finalize().as_bytes())
}

fn pending_object_key(owner_hash: &str, upload_id: Uuid) -> String {
    format!("pending/{owner_hash}/{upload_id}")
}

fn canonical_object_key(owner_hash: &str, blake3_hex: &str) -> String {
    format!("objects/{owner_hash}/{UPLOADED_BLOB_SCHEMA_ID}/{blake3_hex}")
}

fn format_time(value: OffsetDateTime) -> Result<String, CommandError> {
    value
        .format(&Rfc3339)
        .map_err(|e| CommandError::cited_object_upload(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::{OrgId, UserId};

    #[test]
    fn object_keys_do_not_embed_raw_owner_ids() {
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let (owner_kind, principal_id, org_id) = owner_columns(&owner);
        let owner_hash = owner_hash_hex(&owner);
        let pending = pending_object_key(&owner_hash, Uuid::now_v7());
        let canonical = canonical_object_key(&owner_hash, &"a".repeat(64));

        assert_eq!(owner_hash.len(), 64);
        assert!(!pending.contains(owner_kind));
        assert!(!pending.contains(&principal_id.to_string()));
        assert!(!pending.contains(&org_id.to_string()));
        assert!(pending.starts_with("pending/"));
        assert!(canonical.contains(UPLOADED_BLOB_SCHEMA_ID));
        assert!(canonical.starts_with("objects/"));
    }

    #[test]
    fn owner_hash_is_owner_scoped() {
        let org_id = OrgId::new(Uuid::now_v7());
        let a = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id,
        };
        let b = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id,
        };
        assert_ne!(owner_hash_hex(&a), owner_hash_hex(&b));
    }
}
