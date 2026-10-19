//! #513: Goal evidence is a Fact or an Abstraction on every goal write
//! (`core_goal` set, modify, each decompose child, the `core_episode_commit`
//! goal entry), as on `mark_achieved`. A Perspective is still refused.

use proxima::testkit::SplitRoleDb;
use proxima::{BuiltProxima, CoreMcpError, CoreMcpTools, Proxima};
use proxima_core::{OwnerId, Role, UserId};
use proxima_pg_testkit::db_url;
use uuid::Uuid;

use super::split_core_db::clone_split_core_db;
use super::{AgentMemoryApp, ResolvedAuthz, call_test_model_tool, space_authz};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

pub(super) struct Env {
    split_db: SplitRoleDb,
    built: BuiltProxima,
    tools: CoreMcpTools,
    authz: ResolvedAuthz,
    owner: OwnerId,
}

impl Env {
    pub(super) async fn new(prefix: &str) -> TestResult<Self> {
        let split_db = clone_split_core_db(prefix).await?;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let built = Proxima::<AgentMemoryApp>::app()
            .database_url(split_db.runtime_url().to_owned())
            .store_number(0)
            .platform_database_url(split_db.platform_url().to_owned())
            .owner(owner)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(owner, vec![owner], Role::admin());
        Ok(Self {
            split_db,
            built,
            tools,
            authz,
            owner,
        })
    }

    pub(super) async fn call(
        &self,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, CoreMcpError> {
        call_test_model_tool(&self.tools, self.authz.clone(), self.owner, tool, args).await
    }

    async fn handle(&self, tool: &str, args: serde_json::Value) -> TestResult<String> {
        let made = self.call(tool, args).await?;
        Ok(made["handle"].as_str().ok_or("handle")?.to_owned())
    }

    pub(super) async fn remember(&self, title: &str) -> TestResult<String> {
        self.handle(
            "core_remember",
            serde_json::json!({"title": title, "body": title, "tags": []}),
        )
        .await
    }

    pub(super) async fn derive(&self, title: &str, source: &str) -> TestResult<String> {
        self.handle(
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction", "title": title, "body": title, "tags": [],
                "source_handles": [source], "model_id": "test-model",
            }),
        )
        .await
    }

    pub(super) async fn interpret(&self, claim: &str, subject: &str) -> TestResult<String> {
        self.handle(
            "core_interpret",
            serde_json::json!({"claim": claim, "confidence": 80, "subjects": [subject]}),
        )
        .await
    }

    pub(super) async fn set_goal(
        &self,
        evidence: &[&str],
        target: &str,
        key: &str,
    ) -> Result<serde_json::Value, CoreMcpError> {
        self.call(
            "core_goal",
            serde_json::json!({
                "action": "set", "schema_id": "core/simple-text-v1",
                "title": key, "text": key, "body": {},
                "evidence": evidence, "target_perspective": target,
                "idempotency_key": key,
            }),
        )
        .await
    }

    async fn modify_goal(
        &self,
        goal: &str,
        evidence: Option<&[&str]>,
        key: &str,
    ) -> Result<serde_json::Value, CoreMcpError> {
        let mut args = serde_json::json!({
            "action": "modify", "goal": goal, "schema_id": "core/simple-text-v1",
            "title": key, "text": key, "body": {}, "idempotency_key": key,
        });
        if let Some(evidence) = evidence {
            args["evidence"] = serde_json::json!(evidence);
        }
        self.call("core_goal", args).await
    }

    pub(super) async fn decompose_goal(
        &self,
        parent: &str,
        children: &[&[&str]],
        target: &str,
        key: &str,
    ) -> Result<serde_json::Value, CoreMcpError> {
        let children: Vec<serde_json::Value> = children
            .iter()
            .enumerate()
            .map(|(i, evidence)| {
                serde_json::json!({
                    "schema_id": "core/simple-text-v1", "title": format!("{key}-{i}"),
                    "text": format!("{key}-{i}"), "body": {}, "evidence": evidence,
                })
            })
            .collect();
        self.call(
            "core_goal",
            serde_json::json!({
                "action": "decompose", "parent_goal": parent, "children": children,
                "target_perspective": target, "idempotency_key": key,
            }),
        )
        .await
    }

    pub(super) async fn admin(&self) -> TestResult<sqlx::PgPool> {
        Ok(sqlx::PgPool::connect(&db_url(self.split_db.name())).await?)
    }

    /// The goal row's exact stored evidence vector.
    async fn evidence_of(&self, goal: &str) -> TestResult<Vec<String>> {
        let admin = self.admin().await?;
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT evidence_t FROM proxima_core.goal WHERE t = $1")
                .bind(Uuid::parse_str(goal)?)
                .fetch_one(&admin)
                .await?;
        admin.close().await;
        Ok(ids.into_iter().map(|id| id.to_string()).collect())
    }

    /// The series' versions (state, t), oldest first. This host publishes no
    /// goal-change records, so the goal rows are the history.
    pub(super) async fn history_of(&self, handle: &str) -> TestResult<Vec<(String, Uuid)>> {
        let admin = self.admin().await?;
        let rows = sqlx::query_as(
            "SELECT state::text, t FROM proxima_core.goal
              WHERE handle = (SELECT handle FROM proxima_core.goal WHERE t = $1)
              ORDER BY t",
        )
        .bind(Uuid::parse_str(handle)?)
        .fetch_all(&admin)
        .await?;
        admin.close().await;
        Ok(rows)
    }

    async fn goal_rows(&self) -> TestResult<i64> {
        let admin = self.admin().await?;
        let rows: i64 =
            sqlx::query_scalar("SELECT count(*) FROM proxima_core.goal WHERE owner_id = $1")
                .bind(self.owner.into_inner())
                .fetch_one(&admin)
                .await?;
        admin.close().await;
        Ok(rows)
    }

    pub(super) async fn shutdown(self) {
        self.built.shutdown().await;
    }
}

pub(super) fn handle_of(value: &serde_json::Value) -> TestResult<String> {
    Ok(value["handle"].as_str().ok_or("handle")?.to_owned())
}

#[tokio::test]
async fn core_goal_set_with_one_fact_points_its_evidence_at_the_fact() {
    let env = Env::new("proxima_goal_fact_set")
        .await
        .expect("PG required for tests");
    let result: TestResult<()> = async {
        let fact = env.remember("set fact").await?;
        let target = env.interpret("set target", &fact).await?;
        let goal = env.set_goal(&[&fact], &target, "fact-set").await?;
        assert_eq!(env.evidence_of(&handle_of(&goal)?).await?, vec![fact]);
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("set with one Fact");
}

#[tokio::test]
async fn core_goal_modify_with_fact_replacement_evidence() {
    let env = Env::new("proxima_goal_fact_modify")
        .await
        .expect("PG required for tests");
    let result: TestResult<()> = async {
        let fact = env.remember("modify fact").await?;
        let abs = env.derive("modify abs", &fact).await?;
        let target = env.interpret("modify target", &fact).await?;
        let head = handle_of(&env.set_goal(&[&abs], &target, "mod-abs").await?)?;
        let modified = handle_of(&env.modify_goal(&head, Some(&[&fact]), "mod-fact").await?)?;
        assert_eq!(
            env.evidence_of(&modified).await?,
            vec![fact.clone()],
            "replacement"
        );
        let carried = handle_of(&env.modify_goal(&modified, None, "mod-carried").await?)?;
        assert_eq!(env.evidence_of(&carried).await?, vec![fact], "carried");
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("modify with Fact evidence");
}

#[tokio::test]
async fn core_goal_decompose_children_cite_facts() {
    let env = Env::new("proxima_goal_fact_decompose")
        .await
        .expect("PG required for tests");
    let result: TestResult<()> = async {
        let fact = env.remember("decompose fact").await?;
        let abs = env.derive("decompose abs", &fact).await?;
        let target = env.interpret("decompose target", &fact).await?;
        let parent = handle_of(&env.set_goal(&[&abs], &target, "dec-parent").await?)?;
        let out = env
            .decompose_goal(&parent, &[&[&fact], &[&fact, &abs]], &target, "dec-fact")
            .await?;
        let children = out["children"].as_array().ok_or("children")?;
        assert_eq!(children.len(), 2, "{out}");
        assert_eq!(
            env.evidence_of(&handle_of(&children[0])?).await?,
            vec![fact.clone()]
        );
        assert_eq!(
            env.evidence_of(&handle_of(&children[1])?).await?,
            vec![fact, abs]
        );
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("decompose children cite Facts");
}

#[tokio::test]
async fn core_goal_set_with_mixed_fact_and_abstraction() {
    let env = Env::new("proxima_goal_fact_mixed")
        .await
        .expect("PG required for tests");
    let result: TestResult<()> = async {
        let fact = env.remember("mixed fact").await?;
        let abs = env.derive("mixed abs", &fact).await?;
        let target = env.interpret("mixed target", &fact).await?;
        let goal = env.set_goal(&[&fact, &abs], &target, "mixed").await?;
        assert_eq!(env.evidence_of(&handle_of(&goal)?).await?, vec![fact, abs]);
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("mixed Fact and Abstraction");
}

#[tokio::test]
async fn episode_goal_cites_a_remembered_fact_slot() {
    let env = Env::new("proxima_goal_fact_episode_slot")
        .await
        .expect("PG required for tests");
    let result: TestResult<()> = async {
        let out = env
            .call(
                "core_episode_commit",
                serde_json::json!({
                    "remember": [{"title": "ep fact", "body": "ep fact", "idempotency_key": "ep-f"}],
                    "stance": [{"claim": "ep stance", "confidence": 70,
                        "subjects": ["remember:0"], "model_id": "test-model"}],
                    "goal": [{"schema_id": "core/simple-text-v1", "title": "ep goal",
                        "text": "ep goal", "body": {}, "evidence": ["remember:0"],
                        "target_perspective": "stance:0", "idempotency_key": "ep-g"}],
                }),
            )
            .await?;
        let fact = out["remembered"][0].as_str().ok_or("fact")?.to_owned();
        let goal = out["goals"][0].as_str().ok_or("goal")?.to_owned();
        assert_eq!(env.evidence_of(&goal).await?, vec![fact]);
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("episode goal cites a remember slot");
}

#[tokio::test]
async fn episode_goal_cites_an_existing_fact_uuid() {
    let env = Env::new("proxima_goal_fact_episode_uuid")
        .await
        .expect("PG required for tests");
    let result: TestResult<()> = async {
        let fact = env.remember("ep uuid fact").await?;
        let target = env.interpret("ep uuid target", &fact).await?;
        let out = env
            .call(
                "core_episode_commit",
                serde_json::json!({
                    "goal": [{"schema_id": "core/simple-text-v1", "title": "ep goal",
                        "text": "ep goal", "body": {}, "evidence": [fact],
                        "target_perspective": target, "idempotency_key": "ep-uuid-g"}],
                }),
            )
            .await?;
        let goal = out["goals"][0].as_str().ok_or("goal")?.to_owned();
        assert_eq!(env.evidence_of(&goal).await?, vec![fact]);
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("episode goal cites a Fact uuid");
}

/// The Perspective refusal survives removing the operator kind check: set,
/// modify, decompose child, episode `stance:N` slot and an episode
/// Perspective uuid are each refused, and none writes a goal row.
#[tokio::test]
async fn perspective_evidence_is_refused_on_every_goal_write() {
    let env = Env::new("proxima_goal_perspective_refused")
        .await
        .expect("PG required for tests");
    let result: TestResult<()> = async {
        let fact = env.remember("persp fact").await?;
        let abs = env.derive("persp abs", &fact).await?;
        let target = env.interpret("persp target", &fact).await?;
        let other = env.interpret("persp evidence", &fact).await?;
        let head = handle_of(&env.set_goal(&[&abs], &target, "persp-head").await?)?;
        let before = env.goal_rows().await?;
        let refused = |result: Result<serde_json::Value, CoreMcpError>, what: &str| -> String {
            match result {
                Ok(value) => panic!("{what}: a Perspective was accepted as evidence: {value}"),
                Err(err) => {
                    let text = err.to_string();
                    assert!(text.contains("Fact or Abstraction"), "{what}: {text}");
                    text
                }
            }
        };
        refused(env.set_goal(&[&other], &target, "persp-set").await, "set");
        refused(
            env.modify_goal(&head, Some(&[&fact, &other]), "persp-modify").await,
            "modify",
        );
        assert_eq!(env.evidence_of(&head).await?, vec![abs.clone()], "head unchanged");
        refused(
            env.decompose_goal(&head, &[&[&other]], &target, "persp-decompose").await,
            "decompose",
        );
        let slot = refused(
            env.call(
                "core_episode_commit",
                serde_json::json!({
                    "remember": [{"title": "persp ep", "body": "persp ep", "idempotency_key": "persp-ep-f"}],
                    "stance": [{"claim": "persp ep stance", "confidence": 70,
                        "subjects": ["remember:0"], "model_id": "test-model"}],
                    "goal": [{"schema_id": "core/simple-text-v1", "title": "persp ep goal",
                        "text": "persp ep goal", "body": {}, "evidence": ["stance:0"],
                        "target_perspective": "stance:0", "idempotency_key": "persp-ep-g"}],
                }),
            )
            .await,
            "episode stance:0",
        );
        assert!(
            slot.contains("Goal evidence must reference a Fact or Abstraction"),
            "episode stance:0: {slot}"
        );
        refused(
            env.call(
                "core_episode_commit",
                serde_json::json!({
                    "goal": [{"schema_id": "core/simple-text-v1", "title": "persp ep uuid",
                        "text": "persp ep uuid", "body": {}, "evidence": [other],
                        "target_perspective": target, "idempotency_key": "persp-ep-uuid"}],
                }),
            )
            .await,
            "episode Perspective uuid",
        );
        assert_eq!(env.goal_rows().await?, before, "a refused call wrote a goal row");
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("Perspective evidence refused");
}
