//! Unassigned Active Goals remain recallable in their selected memory space.

use super::goal_evidence_kinds::handle_of;
use super::*;

fn goal_args(title: &str, target: Option<&str>) -> serde_json::Value {
    let mut args = serde_json::json!({
        "action": "set", "schema_id": "core/simple-text-v1",
        "title": title, "text": title,
    });
    if let Some(target) = target {
        args["motivated_by"] = serde_json::json!([target]);
    }
    args
}

#[tokio::test]
async fn recall_unassigned_goals_keeps_space_scope_pages_and_dedupes_subjects() {
    let split_db = clone_split_core_db("proxiima_recall_unassigned")
        .await
        .expect("PG required for tests");
    let personal = OwnerId::from(UserId::new(Uuid::now_v7()));
    let shared = OwnerId::from(GroupId::new(Uuid::now_v7()));
    let built = Proxiima::<AgentMemoryApp>::app()
        .database_url(split_db.runtime_url().to_owned())
        .store_number(0)
        .platform_database_url(split_db.platform_url().to_owned())
        .owner(personal)
        .served_surface(proxiima_core::ServedSurface::All)
        .build()
        .await
        .expect("host builds");
    let tools = built.host().core_mcp_tools();
    let authz = space_authz(personal, vec![personal, shared], Role::admin());
    let call = |owner, tool: &'static str, args| {
        call_test_model_tool(&tools, authz.clone(), owner, tool, args)
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let shared_space =
            server_issued_group_space_selector(&tools, authz.clone(), personal).await;
        let shared_goal =
            handle_of(&call(shared, "core_goal", goal_args("shared standalone", None)).await?)?;
        let personal_goal = handle_of(
            &call(
                personal,
                "core_goal",
                goal_args("personal standalone", None),
            )
            .await?,
        )?;
        let fact = handle_of(
            &call(
                shared,
                "core_remember",
                serde_json::json!({"title":"assigned source", "body":"assigned source"}),
            )
            .await?,
        )?;
        let target = handle_of(
            &call(
                shared,
                "core_interpret",
                serde_json::json!({"claim":"assigned stance", "subjects":[fact]}),
            )
            .await?,
        )?;
        // Newer assigned Goals fill the first pages but are unrelated to this cue.
        for title in ["assigned first", "assigned second"] {
            call(shared, "core_goal", goal_args(title, Some(&target))).await?;
        }

        let selected = call(
            personal,
            "core_recall",
            serde_json::json!({
                "question":"unmatchedcue", "kind":"Goal", "space":shared_space,
                "limit":1,
            }),
        )
        .await?;
        let selected_rows = selected["sketches"].as_array().ok_or("sketches")?;
        assert_eq!(selected_rows.len(), 1, "{selected}");
        assert_eq!(selected_rows[0]["handle"], shared_goal, "{selected}");
        assert_eq!(selected_rows[0]["reason"], "unassigned_goal", "{selected}");

        let current = call(
            personal,
            "core_recall",
            serde_json::json!({"question":"unmatchedcue", "kind":"Goal", "limit":1}),
        )
        .await?;
        let current_rows = current["sketches"].as_array().ok_or("sketches")?;
        assert_eq!(current_rows.len(), 1, "{current}");
        assert_eq!(current_rows[0]["handle"], personal_goal, "{current}");

        let subject = call(
            personal,
            "core_recall",
            serde_json::json!({
                "subjects":[shared_goal], "kind":"Goal", "space":shared_space,
                "limit":16,
            }),
        )
        .await?;
        let subject_rows = subject["sketches"].as_array().ok_or("sketches")?;
        assert_eq!(subject_rows.len(), 1, "{subject}");
        assert_eq!(subject_rows[0]["handle"], shared_goal, "{subject}");
        assert_eq!(subject_rows[0]["reason"], "subject", "{subject}");
        Ok(())
    }
    .await;

    built.shutdown().await;
    result.expect("unassigned goal recall respects scope, paging, and dedupe");
}

#[tokio::test]
async fn recall_question_keeps_the_selected_owner_scope() {
    let split_db = clone_split_core_db("proxiima_recall_question_scope")
        .await
        .expect("PG required for tests");
    let personal = OwnerId::from(UserId::new(Uuid::now_v7()));
    let shared = OwnerId::from(GroupId::new(Uuid::now_v7()));
    let built = Proxiima::<AgentMemoryApp>::app()
        .database_url(split_db.runtime_url().to_owned())
        .store_number(0)
        .platform_database_url(split_db.platform_url().to_owned())
        .owner(personal)
        .served_surface(proxiima_core::ServedSurface::All)
        .build()
        .await
        .expect("host builds");
    let tools = built.host().core_mcp_tools();
    let authz = space_authz(personal, vec![personal, shared], Role::admin());
    let call = |owner, tool: &'static str, args| {
        call_test_model_tool(&tools, authz.clone(), owner, tool, args)
    };
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let shared_space =
            server_issued_group_space_selector(&tools, authz.clone(), personal).await;
        let shared_match = handle_of(
            &call(
                shared,
                "core_remember",
                serde_json::json!({"title":"needle shared fact", "body":"needle shared fact"}),
            )
            .await?,
        )?;
        let personal_match = handle_of(
            &call(
                personal,
                "core_remember",
                serde_json::json!({"title":"needle personal fact", "body":"needle personal fact"}),
            )
            .await?,
        )?;
        let question = call(
            personal,
            "core_recall",
            serde_json::json!({
                "question":"needle", "kind":"Fact", "space":shared_space,
                "limit":16,
            }),
        )
        .await?;
        let rows = question["sketches"].as_array().ok_or("sketches")?;
        let handles: Vec<&str> = rows
            .iter()
            .filter(|row| row["reason"] == "question")
            .filter_map(|row| row["handle"].as_str())
            .collect();
        assert!(handles.contains(&shared_match.as_str()), "{question}");
        assert!(!handles.contains(&personal_match.as_str()), "{question}");
        Ok(())
    }
    .await;
    built.shutdown().await;
    result.expect("recall question respects selected owner scope");
}
