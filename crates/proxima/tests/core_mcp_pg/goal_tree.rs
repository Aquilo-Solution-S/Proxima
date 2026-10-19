//! #514 F9: `core_goal mark_achieved` on a parent with open decompose
//! children is refused as `InvalidArgument` naming each open child; once the
//! children are achieved, the parent is.

use super::goal_evidence_kinds::{Env, handle_of};

#[tokio::test]
async fn mark_achieved_on_a_parent_waits_for_its_children() {
    let env = Env::new("proxima_goal_tree_mcp")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fact = env.remember("tree fact").await?;
        let target = env.interpret("tree target", &fact).await?;
        let parent = handle_of(&env.set_goal(&[&fact], &target, "tree-parent").await?)?;
        let out = env
            .decompose_goal(&parent, &[&[&fact], &[&fact]], &target, "tree-children")
            .await?;
        let children: Vec<String> = out["children"]
            .as_array()
            .ok_or("children")?
            .iter()
            .map(handle_of)
            .collect::<Result<_, _>>()?;
        let achieve = |goal: String, key: &'static str| {
            env.call(
                "core_goal",
                serde_json::json!({
                    "action": "mark_achieved", "goal": goal,
                    "evidence": [fact.clone()], "idempotency_key": key,
                }),
            )
        };
        let refused = achieve(parent.clone(), "tree-parent-early")
            .await
            .expect_err("a parent with open children is refused");
        let text = refused.to_string();
        assert!(text.contains("InvalidArgument"), "{text}");
        assert!(text.contains("goal has open children: "), "{text}");
        for child in &children {
            assert!(text.contains(child.as_str()), "{child} not named: {text}");
        }
        achieve(children[0].clone(), "tree-child-0").await?;
        achieve(children[1].clone(), "tree-child-1").await?;
        achieve(parent, "tree-parent-done").await?;
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("mark_achieved waits for children");
}
