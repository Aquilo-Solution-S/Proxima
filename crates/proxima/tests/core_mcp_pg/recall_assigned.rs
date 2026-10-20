//! #490: recall reads the Goals assigned to its head Perspectives together.

use super::goal_evidence_kinds::{Env, handle_of};
use serde_json::json;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

#[tokio::test]
async fn recall_returns_the_goals_assigned_to_every_recalled_perspective() {
    let env = Env::new("proxima_recall_assigned")
        .await
        .expect("PG required for tests");
    let result: TestResult<()> = async {
        let fact = env.remember("assigned source").await?;
        let mut cued = Vec::new();
        let mut goals = Vec::new();
        for n in 0..3 {
            let perspective = env.interpret(&format!("stance {n}"), &fact).await?;
            let goal = handle_of(
                &env.set_goal(&[&fact], &perspective, &format!("goal {n}"))
                    .await?,
            )?;
            cued.push(perspective);
            goals.push(goal);
        }
        // Assigned to a Perspective the cue does not reach: not recalled.
        let other = env.interpret("an unrelated stance", &fact).await?;
        let uncued = handle_of(&env.set_goal(&[&fact], &other, "uncued goal").await?)?;

        let packet = env
            .call("core_recall", json!({"subjects": cued, "kind": "Goal"}))
            .await?;
        let mut assigned: Vec<&str> = packet["sketches"]
            .as_array()
            .ok_or("sketches")?
            .iter()
            .filter(|sketch| sketch["reason"] == "assigned_goal")
            .filter_map(|sketch| sketch["handle"].as_str())
            .collect();
        assigned.sort_unstable();
        let mut expected: Vec<&str> = goals.iter().map(String::as_str).collect();
        expected.sort_unstable();
        assert_eq!(assigned, expected, "{packet}");
        assert!(!assigned.contains(&uncued.as_str()), "{packet}");
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("assigned goals of every recalled perspective");
}
