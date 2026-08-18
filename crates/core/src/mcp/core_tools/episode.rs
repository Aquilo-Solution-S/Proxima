//! `core_episode_commit` — one transaction, explicit `bind[]`.

use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::memory::util::{normalize_idempotency_key, normalize_tags};
use crate::engine::TypedFactIngest;
use crate::mcp::{McpTool, McpToolCtx, McpToolError};
use crate::memory::payloads::AgentNoteV1;
use crate::memory::payloads::write_act::WriteActV1;
use crate::protocol::tool as protocol_tool;
use crate::tool::validate_trimmed_len;

const MAX_REMEMBER: usize = 16;
const MAX_BIND: usize = 32;
const NOTE_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x3d, 0x9a, 0x4e, 0x11, 0x8c, 0x20, 0x4f, 0x6a, 0x9b, 0x77, 0x12, 0x44, 0x88, 0x01, 0xcc, 0x55,
]);

#[derive(Debug, Default)]
pub struct EpisodeCommitTool;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EpisodeRememberItem {
    #[schemars(length(max = 240), description = "Fact title, 1 to 240 chars.")]
    pub title: String,
    #[schemars(length(max = 20000), description = "Fact body, 1 to 20000 chars.")]
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EpisodeCommitArgs {
    #[schemars(
        length(min = 1, max = 16),
        description = "Facts authored in this episode."
    )]
    pub remember: Vec<EpisodeRememberItem>,
    #[serde(default)]
    #[schemars(
        length(max = 32),
        description = "Local keys to pin to the write-act (`remember:0` …). Only listed produced nodes get `refs += write-act t`."
    )]
    pub bind: Vec<String>,
    #[serde(default)]
    pub space: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EpisodeCommitOutput {
    pub write_act: String,
    pub remembered: Vec<String>,
    pub bound: Vec<String>,
}

impl McpTool for EpisodeCommitTool {
    const NAME: &'static str = protocol_tool::CORE_EPISODE_COMMIT;
    const DESCRIPTION: &'static str = "Commit one episode in a single transaction: remember Facts, mint a write-act Fact, and pin only bind[] members to that act (`remember:N` keys). Not a connect verb.";
    type Args = EpisodeCommitArgs;
    type Output = EpisodeCommitOutput;

    fn call(
        ctx: McpToolCtx,
        args: EpisodeCommitArgs,
    ) -> BoxFuture<'static, Result<EpisodeCommitOutput, McpToolError>> {
        Box::pin(async move { episode_commit(ctx, args).await })
    }
}

async fn episode_commit(
    ctx: McpToolCtx,
    args: EpisodeCommitArgs,
) -> Result<EpisodeCommitOutput, McpToolError> {
    if args.remember.is_empty() {
        return Err(McpToolError::InvalidInput(
            "episode_commit requires at least one remember item".into(),
        ));
    }
    if args.remember.len() > MAX_REMEMBER {
        return Err(McpToolError::InvalidInput(format!(
            "at most {MAX_REMEMBER} remember items"
        )));
    }
    if args.bind.len() > MAX_BIND {
        return Err(McpToolError::InvalidInput(format!(
            "at most {MAX_BIND} bind keys"
        )));
    }
    reject_duplicate_keys(&args.remember)?;
    let bind = parse_bind(&args.bind, args.remember.len())?;
    let space = super::memory_spaces::resolve_space_owner(
        &ctx,
        args.space.as_deref(),
        super::memory_spaces::SpaceDefault::Current,
    )?;
    let authz = ctx
        .authz
        .clone()
        .narrowed_to_owner(space.owner)
        .ok_or_else(|| McpToolError::NotAuthorized("memory space write".into()))?;
    let engine = ctx.require_engine()?;
    let mut uow = engine.unit_of_work(&authz).await?;
    let write_act = uow
        .ingest_fact(
            "core/episode-commit",
            &WriteActV1 {
                episode_id: uuid::Uuid::now_v7(),
            },
        )
        .await?;
    let act_t = write_act.memory_id.into_inner();
    let mut remembered = Vec::new();
    let mut bound = Vec::new();
    for (idx, item) in args.remember.iter().enumerate() {
        let title = validate_trimmed_len("title", &item.title, 240)?;
        let body = validate_trimmed_len("body", &item.body, 20_000)?;
        let idempotency_key = normalize_idempotency_key(item.idempotency_key.clone())?;
        let tags = normalize_tags(item.tags.clone())?;
        let note_id = idempotency_key
            .as_deref()
            .map_or_else(uuid::Uuid::now_v7, |key| {
                uuid::Uuid::new_v5(&NOTE_NAMESPACE, key.as_bytes())
            });
        let payload = AgentNoteV1 {
            note_id,
            title: title.to_string(),
            body: body.to_string(),
            tags,
            idempotency_key,
        };
        let mut spec = TypedFactIngest::new("core/episode-remember", &payload);
        if bind.contains(&idx) {
            spec = spec.refs([act_t]);
        }
        let outcome = uow.ingest_typed(spec).await?;
        if bind.contains(&idx) && outcome.idempotent_replay {
            return Err(McpToolError::InvalidInput(
                "bound remember replayed an existing Fact; bind requires a new admission that pins this write-act".into(),
            ));
        }
        let handle =
            ctx.format_memory_with_class(outcome.memory_id, crate::MemoryHandleClass::Fact);
        if bind.contains(&idx) {
            bound.push(handle.clone());
        }
        remembered.push(handle);
    }
    uow.commit().await?;
    Ok(EpisodeCommitOutput {
        write_act: ctx
            .format_memory_with_class(write_act.memory_id, crate::MemoryHandleClass::Fact),
        remembered,
        bound,
    })
}

fn reject_duplicate_keys(items: &[EpisodeRememberItem]) -> Result<(), McpToolError> {
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let Some(key) = item.idempotency_key.as_deref() else {
            continue;
        };
        if !seen.insert(key) {
            return Err(McpToolError::InvalidInput(format!(
                "duplicate idempotency_key in one episode: {key}"
            )));
        }
    }
    Ok(())
}

fn parse_bind(
    raw: &[String],
    remember_len: usize,
) -> Result<std::collections::HashSet<usize>, McpToolError> {
    let mut out = std::collections::HashSet::new();
    for key in raw {
        let Some(idx) = key.strip_prefix("remember:") else {
            return Err(McpToolError::InvalidInput(format!(
                "bind key {key} is not a produced node; use remember:<index>"
            )));
        };
        let idx: usize = idx.parse().map_err(|_| {
            McpToolError::InvalidInput(format!("bind key {key} is not remember:<index>"))
        })?;
        if idx >= remember_len {
            return Err(McpToolError::InvalidInput(format!(
                "bind key {key} is out of range"
            )));
        }
        out.insert(idx);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_keys_must_name_remember_slots() {
        assert!(parse_bind(&["remember:0".into()], 1).is_ok());
        assert!(parse_bind(&["derive".into()], 1).is_err());
        assert!(parse_bind(&["remember:3".into()], 1).is_err());
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        let items = vec![
            EpisodeRememberItem {
                title: "a".into(),
                body: "a".into(),
                tags: vec![],
                idempotency_key: Some("k".into()),
            },
            EpisodeRememberItem {
                title: "b".into(),
                body: "b".into(),
                tags: vec![],
                idempotency_key: Some("k".into()),
            },
        ];
        assert!(reject_duplicate_keys(&items).is_err());
    }
}
