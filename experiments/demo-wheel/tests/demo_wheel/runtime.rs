use super::*;

pub(super) fn read_excerpt(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    if path.is_file() {
        Ok(std::fs::read_to_string(path)?)
    } else {
        Ok(String::new())
    }
}

#[derive(Debug)]
pub(super) struct DemoEmbedding;

#[async_trait]
impl EmbeddingClient for DemoEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![0.0; 4096])
    }

    fn model_id(&self) -> &'static str {
        EMBED_MODEL
    }

    fn dim(&self) -> usize {
        4096
    }
}

pub(super) fn registry() -> Arc<proxima_core::FlavorRegistryFrozen> {
    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    proxima_flavor_intent::register(&mut registry);
    proxima_code::register(&mut registry);
    Arc::new(registry.freeze())
}

pub(super) fn build_demo_engine(cfg: &DemoConfig, pg: PgStorage, owner: Owner) -> Engine {
    use proxima_core::verbs::query::MemoryStore;

    let mut registry = FlavorRegistry::new();
    proxima_flavor_goal::register(&mut registry);
    proxima_flavor_intent::register(&mut registry);
    proxima_code::register(&mut registry);
    registry.replace_workspace_runner(
        "proxima-code",
        Arc::new(
            proxima_code::workspace_runner::CodeWorkspaceRunner::new(pg.pool().clone())
                .with_worktrees_root(cfg.run_dir.join("worktrees"))
                .with_pnpm_store_root(cfg.run_dir.join("pnpm-store")),
        ),
    );

    Engine::new(
        registry.freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(owner.principal.clone(), owner)),
    )
    .with_storage(Arc::new(pg))
    .with_embed(Arc::new(DemoEmbedding))
}

pub(super) fn setup_author() -> McpAuthorContext {
    McpAuthorContext {
        model_id: "demo-wheel-setup".into(),
        client_name: "demo_wheel_pg".into(),
        client_version: "1".into(),
        caller_self_perspective: None,
    }
}

pub(super) fn env_u32(name: &str, default: u32) -> Result<u32, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) => Ok(value.parse()?),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(err) => Err(err.into()),
    }
}

pub(super) fn env_optional_u16(name: &str) -> Result<Option<u16>, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value.parse()?)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub(super) fn env_u16_with_fallback(
    name: &str,
    default: u16,
    fallback: Option<u16>,
) -> Result<u16, Box<dyn std::error::Error>> {
    match std::env::var(name) {
        Ok(value) => Ok(value.parse()?),
        Err(std::env::VarError::NotPresent) => Ok(fallback.unwrap_or(default)),
        Err(err) => Err(err.into()),
    }
}

pub(super) async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    let mut conn = PgConnection::connect(&admin).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

pub(super) async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    let mut conn = PgConnection::connect(&admin).await?;
    conn.execute(
        format!(
            "SELECT pg_terminate_backend(pid)
             FROM pg_stat_activity
             WHERE datname = '{name}'
               AND pid <> pg_backend_pid()"
        )
        .as_str(),
    )
    .await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

pub(super) fn db_url(db_name: &str) -> String {
    let admin = std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| ADMIN_URL.into());
    match admin.rfind('/') {
        Some(idx) => format!("{}/{}", &admin[..idx], db_name),
        None => format!("{admin}/{db_name}"),
    }
}

pub(super) fn git(path: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("git").args(args).current_dir(path).output()?;
    if !output.status.success() {
        return Err(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

pub(super) fn git_output(path: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git").args(args).current_dir(path).output()?;
    if !output.status.success() {
        return Err(format!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(super) fn count_file_lines(path: &Path) -> Result<u32, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    Ok(u32::try_from(content.lines().count()).unwrap_or(u32::MAX))
}

pub(super) fn assistant_text_from_jsonl(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes).lines().find_map(|line| {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value.get("record").and_then(serde_json::Value::as_str) == Some("assistant_message") {
            value
                .get("text_excerpt")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        } else {
            None
        }
    })
}

pub(super) fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end >= start).then_some(&text[start..=end])
}

pub(super) fn parse_reviewer_score(
    text: &str,
) -> Result<ReviewerScore, Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let categories = value
        .get("categories")
        .and_then(serde_json::Value::as_object);
    let score = score_field(&value, categories, "score")
        .or_else(|| score_field(&value, categories, "overall_score"))
        .unwrap_or_else(|| {
            [
                "requirements",
                "usability",
                "code_simplicity",
                "visual_polish",
                "robustness",
            ]
            .into_iter()
            .filter_map(|field| score_field(&value, categories, field))
            .sum::<u32>()
                / 5
        });
    Ok(ReviewerScore {
        score: score.min(100),
        requirements: score_field(&value, categories, "requirements")
            .unwrap_or(score)
            .min(100),
        usability: score_field(&value, categories, "usability")
            .unwrap_or(score)
            .min(100),
        code_simplicity: score_field(&value, categories, "code_simplicity")
            .or_else(|| score_field(&value, categories, "simplicity"))
            .unwrap_or(score)
            .min(100),
        visual_polish: score_field(&value, categories, "visual_polish")
            .or_else(|| score_field(&value, categories, "polish"))
            .unwrap_or(score)
            .min(100),
        robustness: score_field(&value, categories, "robustness")
            .unwrap_or(score)
            .min(100),
        rationale: value
            .get("rationale")
            .or_else(|| value.get("summary"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

pub(super) fn score_field(
    value: &serde_json::Value,
    categories: Option<&serde_json::Map<String, serde_json::Value>>,
    field: &str,
) -> Option<u32> {
    value
        .get(field)
        .or_else(|| categories.and_then(|map| map.get(field)))
        .and_then(serde_json::Value::as_u64)
        .and_then(|score| u32::try_from(score).ok())
}
