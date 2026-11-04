//! #524: a goal's words suffice for its complete lifecycle, without an invented Perspective.

use super::goal_evidence_kinds::{Env, handle_of};
use serde_json::{Value, json};

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

fn words(title: &str) -> Value {
    json!({"schema_id": "core/simple-text-v1", "title": title, "text": title})
}

async fn recalled(env: &Env) -> TestResult<Vec<String>> {
    let out = env
        .call(
            "core_recall",
            json!({"question": "unrelated cue", "kind": "Goal"}),
        )
        .await?;
    out["sketches"]
        .as_array()
        .ok_or("sketches")?
        .iter()
        .map(|sketch| Ok(sketch["handle"].as_str().ok_or("handle")?.to_owned()))
        .collect()
}

async fn transition(env: &Env, goal: &str, state: &str) -> TestResult<String> {
    let mut args = json!({"action": "transition", "goal": goal, "transition": state});
    if state == "pause" {
        args["pending"] = json!("keep");
    }
    handle_of(&env.call("core_goal", args).await?)
}

#[tokio::test]
async fn a_goal_with_only_words_survives_every_write_and_recall() {
    let env = Env::new("proxiima_goal_words_only")
        .await
        .expect("PG required for tests");
    let result: TestResult<()> = async {
        let mut args = words("Finish the draft");
        args["action"] = json!("set");
        let initial = env.call("core_goal", args).await?;
        let goal = handle_of(&initial)?;
        assert_eq!(initial["edge_count"], 0, "no assignment or evidence was invented");
        assert_eq!(recalled(&env).await?.as_slice(), std::slice::from_ref(&goal));

        let paused = transition(&env, &goal, "pause").await?;
        assert!(recalled(&env).await?.is_empty(), "a historical Active version is not recalled");
        let resumed = transition(&env, &paused, "resume").await?;
        assert_eq!(recalled(&env).await?.as_slice(), std::slice::from_ref(&resumed));

        let mut args = words("Finish the revised draft");
        args["action"] = json!("modify");
        args["goal"] = json!(resumed);
        let modified = handle_of(&env.call("core_goal", args).await?)?;
        assert_eq!(recalled(&env).await?.as_slice(), std::slice::from_ref(&modified));

        let out = env.call("core_goal", json!({
            "action": "decompose", "parent_goal": modified,
            "children": [words("Write the final paragraph")],
            "idempotency_key": "words-child",
        })).await?;
        let child = handle_of(&out["children"][0])?;
        let mut open = recalled(&env).await?;
        open.sort();
        let mut expected = vec![modified.clone(), child.clone()];
        expected.sort();
        assert_eq!(open, expected);

        env.call("core_goal", json!({"action": "mark_achieved", "goal": child})).await?;
        env.call("core_goal", json!({"action": "mark_achieved", "goal": modified})).await?;
        assert!(recalled(&env).await?.is_empty());

        let admin = env.admin().await?;
        let invalid: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxiima_core.goal WHERE assignment_t IS NOT NULL OR cardinality(evidence_t) <> 0"
        ).fetch_one(&admin).await?;
        admin.close().await;
        assert_eq!(invalid, 0, "every successor preserves absent assignment and evidence");
        Ok(())
    }.await;
    env.shutdown().await;
    result.expect("words-only goal lifecycle and recall");
}

#[tokio::test]
async fn an_episode_can_commit_a_goal_with_only_words() {
    let env = Env::new("proxiima_episode_goal_words")
        .await
        .expect("PG required for tests");
    let result: TestResult<()> = async {
        let out = env
            .call(
                "core_episode_commit",
                json!({"goal": [words("Ship the draft")]}),
            )
            .await?;
        let goal = out["goals"][0].as_str().ok_or("goal")?;
        assert_eq!(recalled(&env).await?, [goal]);
        let admin = env.admin().await?;
        let topology: (Option<uuid::Uuid>, Vec<uuid::Uuid>) =
            sqlx::query_as("SELECT assignment_t, evidence_t FROM proxiima_core.goal WHERE t = $1")
                .bind(uuid::Uuid::parse_str(goal)?)
                .fetch_one(&admin)
                .await?;
        admin.close().await;
        assert_eq!(topology, (None, Vec::new()));
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("words-only episode goal");
}

#[tokio::test]
async fn caller_self_does_not_assign_goals_without_an_explicit_target() {
    let env = Env::new("proxiima_goal_explicit_assignment")
        .await
        .expect("PG required for tests");
    let result: TestResult<()> = async {
        let fact = env.remember("The caller is writing a draft").await?;
        let perspective = env.interpret("The caller values clarity", &fact).await?;
        let self_id = proxiima_core::MemoryId::new(uuid::Uuid::parse_str(&perspective)?);
        let mut args = words("Write the draft");
        args["action"] = json!("set");
        let goal = handle_of(&env.call_with_self("core_goal", args, self_id).await?)?;
        let children = env
            .call_with_self(
                "core_goal",
                json!({
                    "action": "decompose", "parent_goal": goal,
                    "children": [words("Write the final paragraph")],
                    "idempotency_key": "caller-self-child",
                }),
                self_id,
            )
            .await?;
        let child = handle_of(&children["children"][0])?;
        let episode = env
            .call_with_self(
                "core_episode_commit",
                json!({"goal": [words("Proofread the draft")]}),
                self_id,
            )
            .await?;
        let episode_goal = episode["goals"][0].as_str().ok_or("episode goal")?;
        let mut args = words("An explicitly assigned goal");
        args["action"] = json!("set");
        args["target_perspective"] = json!(perspective);
        let assigned = handle_of(&env.call_with_self("core_goal", args, self_id).await?)?;

        let admin = env.admin().await?;
        let unassigned: Vec<uuid::Uuid> = [goal.as_str(), child.as_str(), episode_goal]
            .into_iter()
            .map(uuid::Uuid::parse_str)
            .collect::<Result<_, _>>()?;
        let assignments: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxiima_core.goal WHERE t = ANY($1) AND assignment_t IS NOT NULL",
        )
        .bind(&unassigned)
        .fetch_one(&admin)
        .await?;
        assert_eq!(assignments, 0, "caller context never supplies an assignment");
        let explicit: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT assignment_t FROM proxiima_core.goal WHERE t = $1",
        )
        .bind(uuid::Uuid::parse_str(&assigned)?)
        .fetch_one(&admin)
        .await?;
        assert_eq!(explicit, Some(self_id.into_inner()));
        admin.close().await;
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("goal assignment is independent of caller self");
}
