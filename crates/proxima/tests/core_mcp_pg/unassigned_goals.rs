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
    let env = Env::new("proxima_goal_words_only")
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
            "SELECT count(*) FROM proxima_core.goal WHERE assignment_t IS NOT NULL OR cardinality(evidence_t) <> 0"
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
    let env = Env::new("proxima_episode_goal_words")
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
            sqlx::query_as("SELECT assignment_t, evidence_t FROM proxima_core.goal WHERE t = $1")
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
