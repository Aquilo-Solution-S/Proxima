//! `core_upload` — the cited-blob S3 lane over MCP.
//!
//! The MCP transport caps request bodies, so artefact bytes never travel
//! through a tool call: `prepare` mints a presigned S3 `PUT`, the client
//! uploads the bytes directly, `complete` verifies and returns the
//! canonical `cited_object_id`, and `read_url` mints a presigned `GET`
//! later. The tool talks to the host-wired
//! [`CitedBlobService`] extension; a host without S3 configured fails
//! typed at call time with a `PROXIMA_S3_*` hint.

use std::sync::Arc;

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;

use crate::error::ProtocolError;
use crate::mcp::{CoreActionMeta, McpActionArgSpec, McpTool, McpToolCtx, McpToolError};
use crate::protocol::{action as protocol_action, tool as protocol_tool};
use crate::storage_ports::CitedBlobService;
use crate::{AccessKind, AuthzContext, Owner, Relation};

use super::facts_citing_object::parse_cited_object_id;
use super::memory_spaces::{SpaceDefault, resolve_space_owner};
use super::{READ_ONLY, WRITE_NON_IDEMPOTENT};

pub const CORE_UPLOAD_ACTIONS: &[CoreActionMeta] = &[
    CoreActionMeta {
        tool: CoreUploadTool::NAME,
        action: "prepare",
        scope_key: protocol_action::CORE_UPLOAD_PREPARE,
        description: "Mint a presigned S3 PUT for one artefact and record the pending upload.",
        produces_schema_ids: &[],
    },
    CoreActionMeta {
        tool: CoreUploadTool::NAME,
        action: "complete",
        scope_key: protocol_action::CORE_UPLOAD_COMPLETE,
        description: "Verify an uploaded artefact, persist its canonical cited object, and record the upload as a core/upload-v1 Fact citing it.",
        produces_schema_ids: &[],
    },
    CoreActionMeta {
        tool: CoreUploadTool::NAME,
        action: "abort",
        scope_key: protocol_action::CORE_UPLOAD_ABORT,
        description: "Abort a pending upload and discard its pending object.",
        produces_schema_ids: &[],
    },
    CoreActionMeta {
        tool: CoreUploadTool::NAME,
        action: "read_url",
        scope_key: protocol_action::CORE_UPLOAD_READ_URL,
        description: "Mint a presigned download URL for a completed cited blob.",
        produces_schema_ids: &[],
    },
];

#[derive(Debug, Default)]
pub struct CoreUploadTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UploadPrepareArgs {
    #[schemars(description = "Filename of the artefact being uploaded, e.g. 'handbuch.pdf'.")]
    pub filename: String,
    #[schemars(description = "MIME type of the artefact bytes, e.g. 'application/pdf'.")]
    pub mime: String,
    #[schemars(
        description = "Exact byte length of the artefact. Completion verifies the uploaded length against it."
    )]
    pub byte_len: u64,
    #[serde(default)]
    #[schemars(description = "Memory space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UploadCompleteArgs {
    #[schemars(description = "The `upload_id` returned by the prepare action.")]
    pub upload_id: String,
    #[serde(default)]
    #[schemars(description = "Memory space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UploadAbortArgs {
    #[schemars(description = "The `upload_id` returned by the prepare action.")]
    pub upload_id: String,
    #[serde(default)]
    #[schemars(description = "Memory space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UploadReadUrlArgs {
    #[schemars(
        description = "Cited object uuid from the complete action (or a citation read-back), optionally prefixed as `C:<uuid>`."
    )]
    pub cited_object_id: String,
    #[serde(default)]
    #[schemars(description = "Memory space key from core_memory_spaces. Omit for current owner.")]
    pub space: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoreUploadArgs {
    Prepare(UploadPrepareArgs),
    Complete(UploadCompleteArgs),
    Abort(UploadAbortArgs),
    ReadUrl(UploadReadUrlArgs),
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UploadHeaderOutput {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UploadPrepareOutput {
    pub upload_id: String,
    pub upload_url: String,
    pub expires_at: String,
    pub headers: Vec<UploadHeaderOutput>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UploadCompleteOutput {
    pub cited_object_id: String,
    pub schema: String,
    pub content_hash: String,
    pub sha256: String,
    pub byte_len: u64,
    pub mime: String,
    pub filename: String,
    /// True when this completion added nothing: the artefact and its
    /// arrival were already recorded for this owner.
    pub idempotent_replay: bool,
    /// Handle of the `core/upload-v1` Fact recording this arrival. It
    /// cites the artefact, so `core_fact` on it reaches the same
    /// `cited_object_id`, and `core/search_memories` finds the file by
    /// its name.
    pub fact: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UploadAbortOutput {
    pub aborted: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UploadReadUrlOutput {
    pub read_url: String,
    pub expires_at: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum CoreUploadOutput {
    Prepare(UploadPrepareOutput),
    Complete(UploadCompleteOutput),
    Abort(UploadAbortOutput),
    ReadUrl(UploadReadUrlOutput),
}

impl McpTool for CoreUploadTool {
    const NAME: &'static str = protocol_tool::CORE_UPLOAD;
    const DESCRIPTION: &'static str = "Upload dispatcher for cited artefacts (documents, images, transcripts) — prepare/complete/abort/read_url. Bytes never travel through MCP: `prepare` (filename, mime, byte_len) returns a presigned `upload_url` plus `headers`; HTTP PUT the raw bytes to that URL with exactly those headers before `expires_at`; then `complete` (upload_id) verifies the bytes and returns the canonical `cited_object_id`. `complete` also records the arrival itself as a `core/upload-v1` Fact citing that artefact and returns its handle as `fact`, so an uploaded file is findable by name through core_search_memories without anyone writing a Fact for it. Cite the artefact from further Facts via core_remember's `citation.cited_object_id`; fetch it later with `read_url` (cited_object_id), which returns a presigned download URL. `abort` discards a pending upload.";
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[
        McpActionArgSpec {
            action: "prepare",
            allowed_fields: &["filename", "mime", "byte_len", "space"],
            required_fields: &["filename", "mime", "byte_len"],
            annotations: Some(WRITE_NON_IDEMPOTENT),
        },
        McpActionArgSpec {
            action: "complete",
            allowed_fields: &["upload_id", "space"],
            required_fields: &["upload_id"],
            annotations: Some(WRITE_NON_IDEMPOTENT),
        },
        McpActionArgSpec {
            action: "abort",
            allowed_fields: &["upload_id", "space"],
            required_fields: &["upload_id"],
            annotations: Some(WRITE_NON_IDEMPOTENT),
        },
        McpActionArgSpec {
            action: "read_url",
            allowed_fields: &["cited_object_id", "space"],
            required_fields: &["cited_object_id"],
            annotations: Some(READ_ONLY),
        },
    ];
    type Args = CoreUploadArgs;
    type Output = CoreUploadOutput;

    fn call(
        ctx: McpToolCtx,
        args: CoreUploadArgs,
    ) -> BoxFuture<'static, Result<CoreUploadOutput, McpToolError>> {
        Box::pin(async move {
            match args {
                CoreUploadArgs::Prepare(args) => {
                    let (authz, owner) =
                        narrowed_space(&ctx, args.space.as_deref(), SpaceAuthority::Write)?;
                    let service = blob_service(&ctx)?;
                    let outcome = service
                        .0
                        .prepare_upload(
                            &authz,
                            owner,
                            args.filename.trim(),
                            args.mime.trim(),
                            args.byte_len,
                        )
                        .await?;
                    Ok(CoreUploadOutput::Prepare(UploadPrepareOutput {
                        upload_id: outcome.upload_id,
                        upload_url: outcome.upload_url,
                        expires_at: format_expiry(outcome.expires_at)?,
                        headers: outcome
                            .headers
                            .into_iter()
                            .map(|header| UploadHeaderOutput {
                                name: header.name,
                                value: header.value,
                            })
                            .collect(),
                    }))
                }
                CoreUploadArgs::Complete(args) => {
                    let (authz, owner) =
                        narrowed_space(&ctx, args.space.as_deref(), SpaceAuthority::Write)?;
                    let service = blob_service(&ctx)?;
                    let engine = ctx.require_engine()?;
                    // No extension sidecars from the MCP surface: a tool
                    // caller has no registered schema of its own to add.
                    // Flavors reach the same verb in-process and pass theirs.
                    let completed = engine
                        .complete_upload_as_fact(
                            service.0.as_ref(),
                            &authz,
                            owner,
                            args.upload_id.trim(),
                            &[],
                        )
                        .await?;
                    let outcome = completed.blob;
                    Ok(CoreUploadOutput::Complete(UploadCompleteOutput {
                        cited_object_id: outcome.cited_object_id,
                        schema: outcome.schema,
                        content_hash: outcome.content_hash,
                        sha256: outcome.sha256,
                        byte_len: outcome.byte_len,
                        mime: outcome.mime,
                        filename: outcome.filename,
                        idempotent_replay: outcome.idempotent_replay,
                        fact: ctx.format_fact_memory(completed.fact.memory_id),
                    }))
                }
                CoreUploadArgs::Abort(args) => {
                    let (authz, owner) =
                        narrowed_space(&ctx, args.space.as_deref(), SpaceAuthority::Write)?;
                    let service = blob_service(&ctx)?;
                    let outcome = service
                        .0
                        .abort_upload(&authz, owner, args.upload_id.trim())
                        .await?;
                    Ok(CoreUploadOutput::Abort(UploadAbortOutput {
                        aborted: outcome.aborted,
                    }))
                }
                CoreUploadArgs::ReadUrl(args) => {
                    let cited_object_id = parse_cited_object_id(&args.cited_object_id)?;
                    let (authz, owner) =
                        narrowed_space(&ctx, args.space.as_deref(), SpaceAuthority::Read)?;
                    let service = blob_service(&ctx)?;
                    let outcome = service.0.read_url(&authz, owner, cited_object_id).await?;
                    Ok(CoreUploadOutput::ReadUrl(UploadReadUrlOutput {
                        read_url: outcome.read_url,
                        expires_at: format_expiry(outcome.expires_at)?,
                    }))
                }
            }
        })
    }
}

/// The authority an upload action needs on the resolved space owner:
/// prepare/complete/abort mutate rows under it, `read_url` only reads a
/// completed blob.
#[derive(Debug, Clone, Copy)]
enum SpaceAuthority {
    Write,
    Read,
}

/// Space resolution + authz narrowing, exactly like `core_remember`: the
/// key is a selector only, and the narrowed context is what the port
/// re-authorizes against.
///
/// `narrowed_to_owner` keeps whatever role the caller holds — a group
/// Viewer narrows successfully — so role authority is gated here, with
/// the same `forbidden` denial `core_remember` raises for the identical
/// refusal. The port re-checks the same predicate as defense in depth,
/// but its storage taxonomy would misreport the denial as caller-fixable
/// invalid input.
fn narrowed_space(
    ctx: &McpToolCtx,
    space: Option<&str>,
    authority: SpaceAuthority,
) -> Result<(AuthzContext, Owner), McpToolError> {
    let space = resolve_space_owner(ctx, space, SpaceDefault::Current)?;
    let authz = ctx
        .authz
        .clone()
        .narrowed_to_owner(space.owner)
        .ok_or_else(|| McpToolError::NotAuthorized("memory space write".into()))?;
    let (allowed, required) = match authority {
        SpaceAuthority::Write => (
            authz.may_write(&space.owner, AccessKind::Fact),
            Relation::Editor,
        ),
        SpaceAuthority::Read => (
            authz.may_read(&space.owner, AccessKind::Fact),
            Relation::Viewer,
        ),
    };
    if !allowed {
        return Err(McpToolError::Protocol(ProtocolError::forbidden(
            required.denied_message(),
        )));
    }
    Ok((authz, space.owner))
}

fn blob_service(ctx: &McpToolCtx) -> Result<Arc<CitedBlobService>, McpToolError> {
    ctx.extensions.get::<CitedBlobService>().ok_or_else(|| {
        McpToolError::Unavailable(
            "blob storage is not configured for this host: set PROXIMA_S3_BUCKET / \
             PROXIMA_S3_REGION (see PROXIMA_S3_* in docs/10-configuration.md) to enable \
             the cited-blob upload lane"
                .into(),
        )
    })
}

fn format_expiry(value: time::OffsetDateTime) -> Result<String, McpToolError> {
    value
        .format(&Rfc3339)
        .map_err(|err| McpToolError::Other(format!("format expiry timestamp: {err}")))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::access::Role;
    use crate::mcp::{McpToolErrorKind, validate_action_args};
    use crate::storage_ports::{
        CitedBlobHeld, CitedBlobPort, CitedBlobReadUrl, CitedBlobService, CitedBlobStaged,
        CitedBlobUploadAborted, CitedBlobUploadPrepared,
    };
    use crate::{AuthzContext, GroupId, OwnerRef, StorageError, UserId};

    use super::super::memory_spaces::test_ctx::ctx_for;
    use super::*;

    /// One recorded port call: which action ran, against which owner,
    /// and whether the passed authz could still see a foreign owner.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SeenCall {
        action: &'static str,
        owner: OwnerRef,
        authz_reaches_owner: bool,
    }

    #[derive(Debug, Default)]
    struct RecordingBlobPort {
        calls: Mutex<Vec<SeenCall>>,
    }

    impl RecordingBlobPort {
        fn record(&self, action: &'static str, authz: &AuthzContext, owner: OwnerRef) {
            self.calls.lock().expect("lock").push(SeenCall {
                action,
                owner,
                authz_reaches_owner: authz.can_access_owner(&owner),
            });
        }
    }

    #[async_trait::async_trait]
    impl CitedBlobPort for RecordingBlobPort {
        async fn prepare_upload(
            &self,
            authz: &AuthzContext,
            owner: OwnerRef,
            _filename: &str,
            _mime: &str,
            _byte_len: u64,
        ) -> Result<CitedBlobUploadPrepared, StorageError> {
            self.record("prepare", authz, owner);
            Ok(CitedBlobUploadPrepared {
                upload_id: "0198c0de-0000-7000-8000-000000000001".into(),
                upload_url: "https://s3.test/put".into(),
                expires_at: time::OffsetDateTime::UNIX_EPOCH,
                headers: Vec::new(),
            })
        }

        async fn stage_upload(
            &self,
            authz: &AuthzContext,
            owner: OwnerRef,
            _upload_id: &str,
        ) -> Result<CitedBlobStaged, StorageError> {
            self.record("stage", authz, owner);
            Ok(CitedBlobStaged {
                payload: crate::citations::UploadedBlobPayload {
                    content_hash: [0x00; 32],
                    bucket: "test-bucket".into(),
                    object_key: "objects/test/x.pdf".into(),
                    sha256: [0x11; 32],
                    byte_len: 4,
                    mime: "application/pdf".into(),
                    filename: "x.pdf".into(),
                    etag: None,
                    uploaded_at: time::OffsetDateTime::UNIX_EPOCH,
                },
                already_completed: None,
            })
        }

        async fn finish_upload(
            &self,
            authz: &AuthzContext,
            owner: OwnerRef,
            _upload_id: &str,
            _cited_object_id: uuid::Uuid,
        ) -> Result<(), StorageError> {
            self.record("finish", authz, owner);
            Ok(())
        }

        async fn abort_upload(
            &self,
            authz: &AuthzContext,
            owner: OwnerRef,
            _upload_id: &str,
        ) -> Result<CitedBlobUploadAborted, StorageError> {
            self.record("abort", authz, owner);
            Ok(CitedBlobUploadAborted { aborted: true })
        }

        async fn read_url(
            &self,
            authz: &AuthzContext,
            owner: OwnerRef,
            _cited_object_id: uuid::Uuid,
        ) -> Result<CitedBlobReadUrl, StorageError> {
            self.record("read_url", authz, owner);
            Ok(CitedBlobReadUrl {
                read_url: "https://s3.test/get".into(),
                expires_at: time::OffsetDateTime::UNIX_EPOCH,
            })
        }

        async fn find_held_blobs(
            &self,
            authz: &AuthzContext,
            owner: OwnerRef,
            _content_hashes: &[[u8; 32]],
        ) -> Result<Vec<CitedBlobHeld>, StorageError> {
            self.record("find_held", authz, owner);
            Ok(Vec::new())
        }
    }

    fn ctx_with_port(
        subject: UserId,
        group_roles: Vec<(OwnerRef, Role)>,
    ) -> (McpToolCtx, Arc<RecordingBlobPort>) {
        let mut ctx = ctx_for(subject, group_roles);
        let port = Arc::new(RecordingBlobPort::default());
        ctx.extensions
            .insert(CitedBlobService(port.clone() as Arc<dyn CitedBlobPort>));
        (ctx, port)
    }

    fn prepare_args(space: Option<&str>) -> CoreUploadArgs {
        CoreUploadArgs::Prepare(UploadPrepareArgs {
            filename: "doc.pdf".into(),
            mime: "application/pdf".into(),
            byte_len: 4,
            space: space.map(ToOwned::to_owned),
        })
    }

    #[tokio::test]
    async fn missing_service_fails_typed_with_the_s3_config_hint() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let ctx = ctx_for(subject, vec![]);

        let err = CoreUploadTool::call(ctx, prepare_args(None))
            .await
            .expect_err("no CitedBlobService wired");
        // A precondition fault, not an internal error: the message must
        // reach the caller verbatim and name the enabling configuration.
        assert_eq!(err.kind(), McpToolErrorKind::InvalidRequest);
        assert!(
            err.client_message().contains("PROXIMA_S3_"),
            "message must name the enabling env: {err}"
        );
    }

    #[tokio::test]
    async fn omitted_space_reaches_the_port_with_the_current_owner() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let (ctx, port) = ctx_with_port(subject, vec![]);

        CoreUploadTool::call(ctx, prepare_args(None))
            .await
            .expect("prepare succeeds");
        let calls = port.calls.lock().expect("lock");
        assert_eq!(
            *calls,
            vec![SeenCall {
                action: "prepare",
                owner: OwnerRef::Personal(subject),
                authz_reaches_owner: true,
            }]
        );
    }

    #[tokio::test]
    async fn explicit_group_space_narrows_the_authz_to_that_owner() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let group = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let (ctx, port) = ctx_with_port(subject, vec![(group, Role::editor())]);
        let space_key = super::super::memory_spaces::MemorySpaceKey::owner(group).to_wire();

        CoreUploadTool::call(ctx, prepare_args(Some(&space_key)))
            .await
            .expect("prepare succeeds in the group space");
        let calls = port.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].owner, group);
        // The port receives an authz narrowed to the space owner, not the
        // caller's full context.
        assert!(calls[0].authz_reaches_owner);
    }

    #[tokio::test]
    async fn unknown_space_key_is_rejected_before_the_port() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let (ctx, port) = ctx_with_port(subject, vec![]);
        let foreign = format!("group:{}", uuid::Uuid::now_v7());

        let err = CoreUploadTool::call(ctx, prepare_args(Some(&foreign)))
            .await
            .expect_err("inaccessible space rejected");
        assert_eq!(err.kind(), McpToolErrorKind::InvalidInput);
        assert!(err.to_string().contains("unknown memory space"));
        assert!(port.calls.lock().expect("lock").is_empty());
    }

    /// A group Viewer's write denial carries the exact error `core_remember`
    /// raises for the same refusal (forbidden -> `invalid_request`), not
    /// model-fixable invalid input: a client that re-prompts on
    /// `invalid_params` must not retry a permissions failure. The port is
    /// never reached.
    #[tokio::test]
    async fn viewer_write_denial_matches_core_remember_error_class() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let group = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let (ctx, port) = ctx_with_port(subject, vec![(group, Role::viewer())]);
        let space_key = super::super::memory_spaces::MemorySpaceKey::owner(group).to_wire();

        let err = CoreUploadTool::call(ctx, prepare_args(Some(&space_key)))
            .await
            .expect_err("viewer cannot prepare an upload in the group space");
        let remember_denial =
            McpToolError::Protocol(ProtocolError::forbidden(Relation::Editor.denied_message()));
        assert_eq!(err.kind(), remember_denial.kind());
        assert_eq!(err.client_message(), remember_denial.client_message());
        assert!(port.calls.lock().expect("lock").is_empty());
    }

    /// `read_url` needs read authority only: the same Viewer that cannot
    /// prepare can mint a download URL for the group's blobs.
    #[tokio::test]
    async fn viewer_read_url_reaches_the_port() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let group = OwnerRef::Group(GroupId::new(uuid::Uuid::now_v7()));
        let (ctx, port) = ctx_with_port(subject, vec![(group, Role::viewer())]);
        let space_key = super::super::memory_spaces::MemorySpaceKey::owner(group).to_wire();

        CoreUploadTool::call(
            ctx,
            CoreUploadArgs::ReadUrl(UploadReadUrlArgs {
                cited_object_id: uuid::Uuid::now_v7().to_string(),
                space: Some(space_key),
            }),
        )
        .await
        .expect("viewer read_url reaches the port");
        assert_eq!(port.calls.lock().expect("lock")[0].action, "read_url");
    }

    #[tokio::test]
    async fn read_url_accepts_the_optional_c_prefix() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let (ctx, port) = ctx_with_port(subject, vec![]);
        let id = uuid::Uuid::now_v7();

        CoreUploadTool::call(
            ctx,
            CoreUploadArgs::ReadUrl(UploadReadUrlArgs {
                cited_object_id: format!("C:{id}"),
                space: None,
            }),
        )
        .await
        .expect("prefixed cited_object_id accepted");
        assert_eq!(port.calls.lock().expect("lock")[0].action, "read_url");
    }

    #[tokio::test]
    async fn read_url_rejects_a_malformed_cited_object_id() {
        let subject = UserId::new(uuid::Uuid::now_v7());
        let (ctx, port) = ctx_with_port(subject, vec![]);

        let err = CoreUploadTool::call(
            ctx,
            CoreUploadArgs::ReadUrl(UploadReadUrlArgs {
                cited_object_id: "C:not-a-uuid".into(),
                space: None,
            }),
        )
        .await
        .expect_err("malformed id rejected");
        assert_eq!(err.kind(), McpToolErrorKind::InvalidInput);
        assert!(port.calls.lock().expect("lock").is_empty());
    }

    #[test]
    fn action_arg_specs_reject_foreign_missing_and_unknown() {
        // Foreign field for the action.
        let err = validate_action_args(
            CoreUploadTool::NAME,
            CoreUploadTool::ACTION_ARG_SPECS,
            &serde_json::json!({ "action": "complete", "upload_id": "u", "filename": "x" }),
        )
        .expect_err("prepare-only field on complete rejected");
        assert!(err.to_string().contains("filename"), "got {err}");

        // Missing required field.
        let err = validate_action_args(
            CoreUploadTool::NAME,
            CoreUploadTool::ACTION_ARG_SPECS,
            &serde_json::json!({ "action": "prepare", "filename": "x", "mime": "y" }),
        )
        .expect_err("missing byte_len rejected");
        assert!(err.to_string().contains("byte_len"), "got {err}");

        // Unknown action.
        let err = validate_action_args(
            CoreUploadTool::NAME,
            CoreUploadTool::ACTION_ARG_SPECS,
            &serde_json::json!({ "action": "download" }),
        )
        .expect_err("unknown action rejected");
        assert!(err.to_string().contains("prepare"), "got {err}");
    }
}
