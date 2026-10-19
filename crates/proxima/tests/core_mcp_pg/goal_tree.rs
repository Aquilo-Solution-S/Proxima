//! #514 F9: `core_goal mark_achieved` on a parent with open decompose
//! children is refused as `InvalidArgument` naming each open child; once the
//! children are achieved, the parent is. Abandon follows the same children
//! rule, and since #515 takes a Paused goal in one write.

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

/// T7: `core_goal transition abandon` on a parent with open children is
/// refused as `InvalidArgument` naming them; once they are abandoned, the
/// parent is.
#[tokio::test]
async fn transition_abandon_on_a_parent_waits_for_its_children() {
    let env = Env::new("proxima_goal_tree_abandon_mcp")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fact = env.remember("abandon fact").await?;
        let target = env.interpret("abandon target", &fact).await?;
        let parent = handle_of(&env.set_goal(&[&fact], &target, "abandon-parent").await?)?;
        let out = env
            .decompose_goal(&parent, &[&[&fact], &[&fact]], &target, "abandon-children")
            .await?;
        let children: Vec<String> = out["children"]
            .as_array()
            .ok_or("children")?
            .iter()
            .map(handle_of)
            .collect::<Result<_, _>>()?;
        let abandon = |goal: String, key: &'static str| {
            env.call(
                "core_goal",
                serde_json::json!({
                    "action": "transition", "goal": goal,
                    "transition": "abandon", "idempotency_key": key,
                }),
            )
        };
        let refused = match abandon(parent.clone(), "abandon-parent-early").await {
            Ok(out) => return Err(format!("a parent with open children is refused: {out}").into()),
            Err(e) => e.to_string(),
        };
        assert!(refused.contains("InvalidArgument"), "{refused}");
        assert!(refused.contains("goal has open children: "), "{refused}");
        for child in &children {
            assert!(
                refused.contains(child.as_str()),
                "{child} not named: {refused}"
            );
        }
        abandon(children[0].clone(), "abandon-child-0").await?;
        abandon(children[1].clone(), "abandon-child-1").await?;
        abandon(parent, "abandon-parent-done").await?;
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("transition abandon waits for children");
}

/// #515 M1: `transition abandon` on a Paused goal is one write: no Active
/// version between, it replays, and abandoning the result again is refused.
#[tokio::test]
async fn transition_abandon_on_a_paused_goal_is_one_write() {
    let env = Env::new("proxima_goal_paused_abandon_mcp")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fact = env.remember("paused fact").await?;
        let target = env.interpret("paused target", &fact).await?;
        let g = handle_of(&env.set_goal(&[&fact], &target, "m1-set").await?)?;
        let transition = |goal: String, transition: &'static str, key: &'static str| {
            let mut args = serde_json::json!({
                "action": "transition", "goal": goal,
                "transition": transition, "idempotency_key": key,
            });
            if transition == "pause" {
                args["pending"] = serde_json::json!("keep");
            }
            env.call("core_goal", args)
        };
        let paused = handle_of(&transition(g.clone(), "pause", "m1-pause").await?)?;
        let out = transition(paused.clone(), "abandon", "m1-abandon").await?;
        assert_eq!(out["idempotent_replay"], false, "{out}");
        let abandoned = handle_of(&out)?;
        let id = |h: &str| uuid::Uuid::parse_str(h);
        assert_eq!(
            env.history_of(&g).await?,
            vec![
                ("Active".to_owned(), id(&g)?),
                ("Paused".to_owned(), id(&paused)?),
                ("Abandoned".to_owned(), id(&abandoned)?),
            ],
            "set, pause, abandon: no Active version between"
        );
        let replay = transition(paused, "abandon", "m1-abandon").await?;
        assert_eq!(replay["idempotent_replay"], true, "{replay}");
        assert_eq!(handle_of(&replay)?, abandoned);
        let refused = match transition(abandoned, "abandon", "m1-again").await {
            Ok(out) => return Err(format!("abandoning an Abandoned goal is refused: {out}").into()),
            Err(e) => e.to_string(),
        };
        eprintln!("M1 refusal: {refused}");
        assert!(
            refused.contains("invalid goal transition: Abandoned -> Abandoned"),
            "{refused}"
        );
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("transition abandon on a Paused goal");
}

/// #515 M2: a Paused parent with open children is refused like an Active
/// one; once the children are abandoned, the Paused parent is.
#[tokio::test]
async fn transition_abandon_on_a_paused_parent_waits_for_its_children() {
    let env = Env::new("proxima_goal_paused_parent_mcp")
        .await
        .expect("PG required for tests");
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let fact = env.remember("paused parent fact").await?;
        let target = env.interpret("paused parent target", &fact).await?;
        let parent = handle_of(&env.set_goal(&[&fact], &target, "m2-parent").await?)?;
        let out = env
            .decompose_goal(&parent, &[&[&fact], &[&fact]], &target, "m2-children")
            .await?;
        let children: Vec<String> = out["children"]
            .as_array()
            .ok_or("children")?
            .iter()
            .map(handle_of)
            .collect::<Result<_, _>>()?;
        let transition = |goal: String, transition: &'static str, key: &'static str| {
            let mut args = serde_json::json!({
                "action": "transition", "goal": goal,
                "transition": transition, "idempotency_key": key,
            });
            if transition == "pause" {
                args["pending"] = serde_json::json!("keep");
            }
            env.call("core_goal", args)
        };
        let paused = handle_of(&transition(parent, "pause", "m2-parent-pause").await?)?;
        let refused = match transition(paused.clone(), "abandon", "m2-parent-early").await {
            Ok(out) => {
                return Err(format!("a Paused parent with open children is refused: {out}").into());
            }
            Err(e) => e.to_string(),
        };
        assert!(refused.contains("InvalidArgument"), "{refused}");
        assert!(refused.contains("goal has open children: "), "{refused}");
        for child in &children {
            assert!(
                refused.contains(child.as_str()),
                "{child} not named: {refused}"
            );
        }
        transition(children[0].clone(), "abandon", "m2-child-0").await?;
        transition(children[1].clone(), "abandon", "m2-child-1").await?;
        transition(paused, "abandon", "m2-parent-done").await?;
        Ok(())
    }
    .await;
    env.shutdown().await;
    result.expect("transition abandon on a Paused parent waits for children");
}
