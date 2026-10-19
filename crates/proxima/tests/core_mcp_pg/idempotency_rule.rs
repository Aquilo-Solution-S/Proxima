//! Acceptance 1–4 for Proxima#477: one meaning for `idempotency_key`.

use proxima::Proxima;
use proxima_core::{OwnerId, Role, UserId};
use uuid::Uuid;

use super::split_core_db::clone_split_core_db;
use super::{AgentMemoryApp, call_test_model_tool, read_test_model_resource, space_authz};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CITED_SCHEMA: &str = "test/facade-cited-object-v1";

fn handle(value: &serde_json::Value) -> Result<String, Box<dyn std::error::Error>> {
    Ok(value["handle"].as_str().ok_or("missing handle")?.to_owned())
}

fn is_idempotency_conflict(err: &proxima::CoreMcpError) -> bool {
    let text = err.to_string();
    text.contains("already names") || text.to_ascii_lowercase().contains("idempotency_conflict")
}

fn citation(locator: &str) -> serde_json::Value {
    serde_json::json!({
        "object_schema_id": CITED_SCHEMA,
        "object_schema_version": 1,
        "object_payload": { "locator": locator },
    })
}

#[allow(clippy::too_many_lines, clippy::similar_names)] // Acceptance 1–4 table: one flow, one fixture
#[tokio::test]
async fn idempotency_rule_table() -> TestResult {
    let split_db = clone_split_core_db("proxima_idempotency_rule")
        .await
        .expect("PG required for tests");
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
    let call = |tool: &'static str, args: serde_json::Value| {
        call_test_model_tool(&tools, authz.clone(), owner, tool, args)
    };

    // --- core_remember ---
    let base = serde_json::json!({
        "title": "note title",
        "body": "note body",
        "tags": ["alpha"],
        "idempotency_key": "note-k1",
    });
    let first = call("core_remember", base.clone()).await?;
    let h = handle(&first)?;
    assert_eq!(first["idempotent_replay"], false);
    let replay = call("core_remember", base.clone()).await?;
    assert_eq!(replay["idempotent_replay"], true);
    assert_eq!(handle(&replay)?, h);
    let mut changed = base.clone();
    changed["body"] = serde_json::json!("note body changed");
    let err = call("core_remember", changed)
        .await
        .expect_err("changed body");
    assert!(is_idempotency_conflict(&err), "{err}");
    assert!(err.to_string().contains(&h), "{err}");

    // changed tags → Conflict
    let mut tags_changed = base.clone();
    tags_changed["tags"] = serde_json::json!(["alpha", "beta"]);
    let err = call("core_remember", tags_changed)
        .await
        .expect_err("changed tags");
    assert!(is_idempotency_conflict(&err), "{err}");
    assert!(err.to_string().contains(&h), "{err}");

    // ruling 3: changed observed_at / language still replays
    let mut meta = base.clone();
    meta["observed_at"] = serde_json::json!("2020-01-01T00:00:00Z");
    meta["language"] = serde_json::json!("english");
    let meta_replay = call("core_remember", meta).await?;
    assert_eq!(meta_replay["idempotent_replay"], true);
    assert_eq!(handle(&meta_replay)?, h);

    // Acceptance 3: new key + supersedes → new version
    let v2 = call(
        "core_remember",
        serde_json::json!({
            "title": "note title v2",
            "body": "note body v2",
            "tags": ["alpha"],
            "idempotency_key": "note-k2",
            "supersedes": h,
        }),
    )
    .await?;
    let h2 = handle(&v2)?;
    assert_ne!(h2, h);
    assert_eq!(v2["idempotent_replay"], false);
    let v2_replay = call(
        "core_remember",
        serde_json::json!({
            "title": "note title v2",
            "body": "note body v2",
            "tags": ["alpha"],
            "idempotency_key": "note-k2",
            "supersedes": h,
        }),
    )
    .await?;
    assert_eq!(v2_replay["idempotent_replay"], true);
    assert_eq!(handle(&v2_replay)?, h2);

    // same key + supersedes → Conflict (default 1)
    let err = call(
        "core_remember",
        serde_json::json!({
            "title": "note title v3",
            "body": "note body v3",
            "tags": ["alpha"],
            "idempotency_key": "note-k1",
            "supersedes": h2,
        }),
    )
    .await
    .expect_err("same key + supersedes");
    assert!(is_idempotency_conflict(&err), "{err}");

    // changed citation → Conflict (existing = the note)
    let cited = call(
        "core_remember",
        serde_json::json!({
            "title": "cited note",
            "body": "cited body",
            "tags": [],
            "idempotency_key": "cite-k1",
            "citation": citation("artifact://cite-a"),
        }),
    )
    .await?;
    let ch = handle(&cited)?;
    let err = call(
        "core_remember",
        serde_json::json!({
            "title": "cited note",
            "body": "cited body",
            "tags": [],
            "idempotency_key": "cite-k1",
            "citation": citation("artifact://cite-b"),
        }),
    )
    .await
    .expect_err("changed citation");
    assert!(is_idempotency_conflict(&err), "{err}");
    assert!(err.to_string().contains(&ch), "{err}");

    // --- core_record_utterance ---
    let utt = serde_json::json!({
        "speaker": "user",
        "conversation_id": "c1",
        "text": "hello",
        "idempotency_key": "utt-k1",
    });
    let u1 = call("core_record_utterance", utt.clone()).await?;
    let uh = handle(&u1)?;
    let u_replay = call("core_record_utterance", utt.clone()).await?;
    assert_eq!(u_replay["idempotent_replay"], true);
    assert_eq!(handle(&u_replay)?, uh);
    let mut utt_changed = utt.clone();
    utt_changed["text"] = serde_json::json!("hello changed");
    let err = call("core_record_utterance", utt_changed)
        .await
        .expect_err("changed utterance");
    assert!(is_idempotency_conflict(&err), "{err}");
    assert!(err.to_string().contains(&uh), "{err}");

    // --- core_derive Abstraction ---
    let src = handle(
        &call(
            "core_remember",
            serde_json::json!({"title": "src", "body": "src", "tags": []}),
        )
        .await?,
    )?;
    let der = serde_json::json!({
        "kind": "Abstraction",
        "title": "abs",
        "body": "abs body",
        "tags": [],
        "source_handles": [src],
        "model_id": "test-model",
        "idempotency_key": "abs-k1",
    });
    let d1 = call("core_derive", der.clone()).await?;
    let dh = handle(&d1)?;
    let d_replay = call("core_derive", der.clone()).await?;
    assert_eq!(d_replay["idempotent_replay"], true);
    assert_eq!(handle(&d_replay)?, dh);
    // Acceptance 2: changed body → conflict, stored body unchanged
    let mut der_changed = der.clone();
    der_changed["body"] = serde_json::json!("abs body changed");
    let err = call("core_derive", der_changed)
        .await
        .expect_err("changed derive body");
    assert!(is_idempotency_conflict(&err), "{err}");
    assert!(err.to_string().contains(&dh), "{err}");
    let got = read_test_model_resource(
        &tools,
        authz.clone(),
        owner,
        &format!("proxima://memory/{dh}"),
    )
    .await?;
    let body = got.to_string();
    assert!(body.contains("abs body"), "{got}");
    assert!(!body.contains("abs body changed"), "{got}");

    // derive: new key + supersedes → new version
    let d2 = call(
        "core_derive",
        serde_json::json!({
            "kind": "Abstraction",
            "title": "abs v2",
            "body": "abs body v2",
            "tags": [],
            "source_handles": [src],
            "model_id": "test-model",
            "idempotency_key": "abs-k2",
            "supersedes": dh,
        }),
    )
    .await?;
    let dh2 = handle(&d2)?;
    assert_ne!(dh2, dh);

    // --- Perspective core_derive ---
    let pers = serde_json::json!({
        "kind": "Perspective",
        "title": "view",
        "body": "view body",
        "tags": [],
        "source_handles": [dh],
        "model_id": "test-model",
        "idempotency_key": "pers-k1",
    });
    let p1 = call("core_derive", pers.clone()).await?;
    let ph = handle(&p1)?;
    let p_replay = call("core_derive", pers.clone()).await?;
    assert_eq!(p_replay["idempotent_replay"], true);
    assert_eq!(handle(&p_replay)?, ph);
    let mut pers_changed = pers.clone();
    pers_changed["body"] = serde_json::json!("view body changed");
    let err = call("core_derive", pers_changed)
        .await
        .expect_err("changed perspective");
    assert!(is_idempotency_conflict(&err), "{err}");
    assert!(err.to_string().contains(&ph), "{err}");

    // --- episode derive ---
    let ep_src = handle(
        &call(
            "core_remember",
            serde_json::json!({"title": "epsrc", "body": "epsrc", "tags": []}),
        )
        .await?,
    )?;
    let ep_der = serde_json::json!({
        "remember": [{
            "title": "ep-der-src",
            "body": "ep-der-src",
            "idempotency_key": "ep-der-src-k",
        }],
        "derive": {
            "title": "ep abs",
            "body": "ep abs body",
            "source_handles": ["remember:0"],
            "model_id": "test-model",
            "idempotency_key": "ep-abs-k1",
        },
    });
    let ep_d1 = call("core_episode_commit", ep_der.clone()).await?;
    let ep_dh = ep_d1["derived"].as_str().ok_or("ep derived")?.to_owned();
    let ep_d_replay = call("core_episode_commit", ep_der.clone()).await?;
    assert_eq!(ep_d_replay["derived"].as_str().ok_or("replay")?, ep_dh);
    let mut ep_der_changed = ep_der.clone();
    ep_der_changed["derive"]["body"] = serde_json::json!("ep abs body changed");
    // remember slot must also change key or content to not conflict on remember
    ep_der_changed["remember"][0]["idempotency_key"] = serde_json::json!("ep-der-src-k2");
    ep_der_changed["remember"][0]["body"] = serde_json::json!("ep-der-src2");
    ep_der_changed["remember"][0]["title"] = serde_json::json!("ep-der-src2");
    let _ = ep_src; // silence if unused; src was for Perspective chain
    let err = call("core_episode_commit", ep_der_changed)
        .await
        .expect_err("changed episode derive");
    assert!(is_idempotency_conflict(&err), "{err}");

    // --- Acceptance 4: remember ↔ episode note share key space ---
    let cross_key = "cross-surface-k";
    let rem = call(
        "core_remember",
        serde_json::json!({
            "title": "cross",
            "body": "cross body",
            "tags": [],
            "idempotency_key": cross_key,
        }),
    )
    .await?;
    let rh = handle(&rem)?;
    let ep = call(
        "core_episode_commit",
        serde_json::json!({
            "remember": [{
                "title": "cross",
                "body": "cross body",
                "idempotency_key": cross_key,
            }],
        }),
    )
    .await?;
    let ep_handle = ep["remembered"][0].as_str().ok_or("ep handle")?;
    assert_eq!(ep_handle, rh);
    let err = call(
        "core_episode_commit",
        serde_json::json!({
            "remember": [{
                "title": "cross",
                "body": "cross body changed",
                "idempotency_key": cross_key,
            }],
        }),
    )
    .await
    .expect_err("episode changed");
    assert!(is_idempotency_conflict(&err), "{err}");

    let cross2 = "cross-surface-k2";
    let ep2 = call(
        "core_episode_commit",
        serde_json::json!({
            "remember": [{
                "title": "cross2",
                "body": "cross2 body",
                "idempotency_key": cross2,
            }],
        }),
    )
    .await?;
    let eh2 = ep2["remembered"][0]
        .as_str()
        .ok_or("ep2 handle")?
        .to_owned();
    let rem2 = call(
        "core_remember",
        serde_json::json!({
            "title": "cross2",
            "body": "cross2 body",
            "tags": [],
            "idempotency_key": cross2,
        }),
    )
    .await?;
    assert_eq!(handle(&rem2)?, eh2);
    assert_eq!(rem2["idempotent_replay"], true);

    // --- core_goal set / transition / mark_achieved / modify / decompose ---
    let fact = handle(
        &call(
            "core_remember",
            serde_json::json!({"title": "gfact", "body": "gfact", "tags": []}),
        )
        .await?,
    )?;
    let abs = handle(
        &call(
            "core_derive",
            serde_json::json!({
                "kind": "Abstraction", "title": "gabs", "body": "gabs", "tags": [],
                "source_handles": [fact], "model_id": "test-model",
                "idempotency_key": "gabs-k",
            }),
        )
        .await?,
    )?;
    let target = handle(
        &call(
            "core_derive",
            serde_json::json!({
                "kind": "Perspective", "title": "gtgt", "body": "gtgt", "tags": [],
                "source_handles": [abs], "model_id": "test-model",
                "idempotency_key": "gtgt-k",
            }),
        )
        .await?,
    )?;

    let set_args = serde_json::json!({
        "action": "set", "schema_id": "core/simple-text-v1",
        "title": "goal-set", "text": "goal-set", "body": {},
        "evidence": [fact], "target_perspective": target,
        "idempotency_key": "goal-set-k",
    });
    let g1 = call("core_goal", set_args.clone()).await?;
    let gh = handle(&g1)?;
    let g_replay = call("core_goal", set_args.clone()).await?;
    assert_eq!(g_replay["idempotent_replay"], true);
    assert_eq!(handle(&g_replay)?, gh);
    let mut set_changed = set_args.clone();
    set_changed["text"] = serde_json::json!("goal-set-changed");
    let err = call("core_goal", set_changed)
        .await
        .expect_err("changed goal set");
    assert!(is_idempotency_conflict(&err), "{err}");
    assert!(err.to_string().contains(&gh), "{err}");

    // transition
    let tr_args = serde_json::json!({
        "action": "transition", "goal": gh,
        "transition": "pause", "pending": "keep",
        "idempotency_key": "goal-tr-k",
    });
    let gt = call("core_goal", tr_args.clone()).await?;
    let gth = handle(&gt)?;
    let gt_replay = call("core_goal", tr_args.clone()).await?;
    assert_eq!(gt_replay["idempotent_replay"], true);
    assert_eq!(handle(&gt_replay)?, gth);
    // resume then conflict on changed transition under same key is awkward;
    // instead: same key with different pending → Conflict
    let err = call(
        "core_goal",
        serde_json::json!({
            "action": "transition", "goal": gth,
            "transition": "pause", "pending": "discard",
            "idempotency_key": "goal-tr-k",
        }),
    )
    .await
    .expect_err("changed transition");
    assert!(is_idempotency_conflict(&err), "{err}");
    assert!(err.to_string().contains(&gth), "{err}");

    // modify (resume first so goal is Active)
    let resumed = call(
        "core_goal",
        serde_json::json!({
            "action": "transition", "goal": gth,
            "transition": "resume", "idempotency_key": "goal-resume-k",
        }),
    )
    .await?;
    let grh = handle(&resumed)?;
    let mod_args = serde_json::json!({
        "action": "modify", "goal": grh, "schema_id": "core/simple-text-v1",
        "title": "goal-mod", "text": "goal-mod", "body": {},
        "evidence": [fact], "idempotency_key": "goal-mod-k",
    });
    let gm = call("core_goal", mod_args.clone()).await?;
    let gmh = handle(&gm)?;
    let gm_replay = call("core_goal", mod_args.clone()).await?;
    assert_eq!(gm_replay["idempotent_replay"], true);
    assert_eq!(handle(&gm_replay)?, gmh);
    let mut mod_changed = mod_args.clone();
    mod_changed["text"] = serde_json::json!("goal-mod-changed");
    let err = call("core_goal", mod_changed)
        .await
        .expect_err("changed modify");
    assert!(is_idempotency_conflict(&err), "{err}");
    assert!(err.to_string().contains(&gmh), "{err}");

    // mark_achieved
    let ma_args = serde_json::json!({
        "action": "mark_achieved", "goal": gmh,
        "evidence": [fact], "idempotency_key": "goal-ma-k",
    });
    let ga = call("core_goal", ma_args.clone()).await?;
    let gah = handle(&ga)?;
    let ga_replay = call("core_goal", ma_args.clone()).await?;
    assert_eq!(ga_replay["idempotent_replay"], true);
    assert_eq!(handle(&ga_replay)?, gah);
    // same key, different evidence set → Conflict
    let fact2 = handle(
        &call(
            "core_remember",
            serde_json::json!({"title": "gfact2", "body": "gfact2", "tags": []}),
        )
        .await?,
    )?;
    let err = call(
        "core_goal",
        serde_json::json!({
            "action": "mark_achieved", "goal": gmh,
            "evidence": [fact2], "idempotency_key": "goal-ma-k",
        }),
    )
    .await
    .expect_err("changed mark_achieved");
    assert!(is_idempotency_conflict(&err), "{err}");
    assert!(err.to_string().contains(&gah), "{err}");

    // decompose: need a fresh Active parent
    let parent = handle(
        &call(
            "core_goal",
            serde_json::json!({
                "action": "set", "schema_id": "core/simple-text-v1",
                "title": "parent", "text": "parent", "body": {},
                "evidence": [fact], "target_perspective": target,
                "idempotency_key": "goal-parent-k",
            }),
        )
        .await?,
    )?;
    let dec_args = serde_json::json!({
        "action": "decompose", "parent_goal": parent,
        "children": [{
            "schema_id": "core/simple-text-v1", "title": "child0",
            "text": "child0", "body": {}, "evidence": [fact],
        }],
        "target_perspective": target,
        "idempotency_key": "goal-dec-k",
    });
    let gd = call("core_goal", dec_args.clone()).await?;
    // decompose returns children; parent stays; conflict names existing child/parent
    let gd_replay = call("core_goal", dec_args.clone()).await?;
    assert_eq!(gd_replay["idempotent_replay"], true);
    let mut dec_changed = dec_args.clone();
    dec_changed["children"][0]["text"] = serde_json::json!("child0-changed");
    let err = call("core_goal", dec_changed)
        .await
        .expect_err("changed decompose");
    assert!(is_idempotency_conflict(&err), "{err}");
    let _ = gd;

    // episode goal
    let eg = call(
        "core_episode_commit",
        serde_json::json!({
            "remember": [{
                "title": "eg-fact", "body": "eg-fact",
                "idempotency_key": "eg-fact-k",
            }],
            "derive": {
                "title": "eg-abs", "body": "eg-abs",
                "source_handles": ["remember:0"], "model_id": "test-model",
                "idempotency_key": "eg-abs-k",
            },
            "stance": [{
                "claim": "eg claim about the fact",
                "confidence": 80,
                "subjects": ["derive"],
            }],
            "goal": [{
                "schema_id": "core/simple-text-v1",
                "title": "eg-goal", "text": "eg-goal", "body": {},
                "evidence": ["remember:0"],
                "target_perspective": "stance:0",
                "idempotency_key": "eg-goal-k",
            }],
        }),
    )
    .await?;
    let egh = eg["goals"][0].as_str().ok_or("eg goal")?.to_owned();
    let eg_replay = call(
        "core_episode_commit",
        serde_json::json!({
            "remember": [{
                "title": "eg-fact", "body": "eg-fact",
                "idempotency_key": "eg-fact-k",
            }],
            "derive": {
                "title": "eg-abs", "body": "eg-abs",
                "source_handles": ["remember:0"], "model_id": "test-model",
                "idempotency_key": "eg-abs-k",
            },
            "stance": [{
                "claim": "eg claim about the fact",
                "confidence": 80,
                "subjects": ["derive"],
            }],
            "goal": [{
                "schema_id": "core/simple-text-v1",
                "title": "eg-goal", "text": "eg-goal", "body": {},
                "evidence": ["remember:0"],
                "target_perspective": "stance:0",
                "idempotency_key": "eg-goal-k",
            }],
        }),
    )
    .await?;
    assert_eq!(eg_replay["goals"][0].as_str().ok_or("eg replay")?, egh);
    let err = call(
        "core_episode_commit",
        serde_json::json!({
            "remember": [{
                "title": "eg-fact2", "body": "eg-fact2",
                "idempotency_key": "eg-fact-k2",
            }],
            "derive": {
                "title": "eg-abs2", "body": "eg-abs2",
                "source_handles": ["remember:0"], "model_id": "test-model",
                "idempotency_key": "eg-abs-k2",
            },
            "stance": [{
                "claim": "eg claim2 about the fact",
                "confidence": 80,
                "subjects": ["derive"],
            }],
            "goal": [{
                "schema_id": "core/simple-text-v1",
                "title": "eg-goal", "text": "eg-goal-changed", "body": {},
                "evidence": ["remember:0"],
                "target_perspective": "stance:0",
                "idempotency_key": "eg-goal-k",
            }],
        }),
    )
    .await
    .expect_err("changed episode goal");
    assert!(is_idempotency_conflict(&err), "{err}");

    built.shutdown().await;
    Ok(())
}

/// HIGH: supersedes must not answer as a replay of an older version that
/// shares the natural note receipt key (same title+body+tags under a new
/// request key). Repro: v1 'A'; v2 'B' supersedes; 'A' again as rv-3
/// superseding v2 must admit a new version, and a later rv-3 with 'C' Conflicts.
#[tokio::test]
async fn supersedes_revert_body_does_not_replay_older_version() -> TestResult {
    let split_db = clone_split_core_db("proxima_idempotency_revert")
        .await
        .expect("PG required for tests");
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
    let call = |tool: &'static str, args: serde_json::Value| {
        call_test_model_tool(&tools, authz.clone(), owner, tool, args)
    };

    let v1 = call(
        "core_remember",
        serde_json::json!({
            "title": "rev", "body": "A", "tags": [],
            "idempotency_key": "rv-1",
        }),
    )
    .await?;
    let h1 = handle(&v1)?;
    let v2 = call(
        "core_remember",
        serde_json::json!({
            "title": "rev", "body": "B", "tags": [],
            "idempotency_key": "rv-2", "supersedes": h1,
        }),
    )
    .await?;
    let h2 = handle(&v2)?;
    assert_ne!(h2, h1);
    assert_eq!(v2["idempotent_replay"], false);

    let v3 = call(
        "core_remember",
        serde_json::json!({
            "title": "rev", "body": "A", "tags": [],
            "idempotency_key": "rv-3", "supersedes": h2,
        }),
    )
    .await?;
    let h3 = handle(&v3)?;
    assert_eq!(v3["idempotent_replay"], false, "must not replay v1: {v3}");
    assert_ne!(h3, h1, "rv-3 must mint a new version, not return v1");
    assert_ne!(h3, h2);

    let err = call(
        "core_remember",
        serde_json::json!({
            "title": "rev", "body": "C", "tags": [],
            "idempotency_key": "rv-3", "supersedes": h2,
        }),
    )
    .await
    .expect_err("rv-3 with C must Conflict");
    assert!(is_idempotency_conflict(&err), "{err}");
    assert!(err.to_string().contains(&h3), "{err}");

    built.shutdown().await;
    Ok(())
}

/// HIGH: same key, same body, different citation must Conflict (not Replay).
#[tokio::test]
async fn citation_only_change_conflicts() -> TestResult {
    let split_db = clone_split_core_db("proxima_idempotency_citation")
        .await
        .expect("PG required for tests");
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
    let call = |tool: &'static str, args: serde_json::Value| {
        call_test_model_tool(&tools, authz.clone(), owner, tool, args)
    };
    let first = call(
        "core_remember",
        serde_json::json!({
            "title": "cite-only", "body": "same", "tags": [],
            "idempotency_key": "cite-only-k",
            "citation": citation("artifact://one"),
        }),
    )
    .await?;
    let h = handle(&first)?;
    let err = call(
        "core_remember",
        serde_json::json!({
            "title": "cite-only", "body": "same", "tags": [],
            "idempotency_key": "cite-only-k",
            "citation": citation("artifact://two"),
        }),
    )
    .await
    .expect_err("citation-only change");
    assert!(is_idempotency_conflict(&err), "{err}");
    assert!(err.to_string().contains(&h), "{err}");
    built.shutdown().await;
    Ok(())
}

/// HIGH: concurrent same-key same-body different-citation: one Fresh, one Conflict
/// (not both Replay of the first writer's handle).
#[tokio::test]
async fn concurrent_same_key_different_citation_one_conflicts() -> TestResult {
    let split_db = clone_split_core_db("proxima_idempotency_cite_race")
        .await
        .expect("PG required for tests");
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
    let a = serde_json::json!({
        "title": "cite-race", "body": "same", "tags": [],
        "idempotency_key": "cite-race-k",
        "citation": citation("artifact://race-a"),
    });
    let b = serde_json::json!({
        "title": "cite-race", "body": "same", "tags": [],
        "idempotency_key": "cite-race-k",
        "citation": citation("artifact://race-b"),
    });
    let (ra, rb) = tokio::join!(
        call_test_model_tool(&tools, authz.clone(), owner, "core_remember", a),
        call_test_model_tool(&tools, authz.clone(), owner, "core_remember", b),
    );
    let outcomes = [ra, rb];
    let oks: Vec<_> = outcomes.iter().filter(|r| r.is_ok()).collect();
    let errs: Vec<_> = outcomes.iter().filter(|r| r.is_err()).collect();
    assert_eq!(oks.len(), 1, "exactly one admission: {outcomes:?}");
    assert_eq!(errs.len(), 1, "exactly one conflict: {outcomes:?}");
    let err = errs[0].as_ref().err().unwrap();
    assert!(is_idempotency_conflict(err), "{err}");
    let ok = oks[0].as_ref().unwrap();
    assert_eq!(ok["idempotent_replay"], false, "{ok}");
    built.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn forgotten_key_replays_and_changed_content_conflicts() -> TestResult {
    let split_db = clone_split_core_db("proxima_idempotency_forgotten")
        .await
        .expect("PG required for tests");
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
    let call = |tool: &'static str, args: serde_json::Value| {
        call_test_model_tool(&tools, authz.clone(), owner, tool, args)
    };
    let args = serde_json::json!({
        "title": "forget-me",
        "body": "forget-me",
        "tags": [],
        "idempotency_key": "forget-key",
    });
    let first = call("core_remember", args.clone()).await?;
    let h = handle(&first)?;
    call("core_forget", serde_json::json!({"memory": h})).await?;
    let replay = call("core_remember", args.clone()).await?;
    assert_eq!(replay["idempotent_replay"], true);
    assert_eq!(replay["forgotten"], true);
    assert_eq!(handle(&replay)?, h);
    let mut changed = args;
    changed["body"] = serde_json::json!("forget-me-changed");
    let err = call("core_remember", changed)
        .await
        .expect_err("changed after forget");
    assert!(is_idempotency_conflict(&err), "{err}");
    built.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn concurrent_same_key_different_content_one_wins() -> TestResult {
    let split_db = clone_split_core_db("proxima_idempotency_race")
        .await
        .expect("PG required for tests");
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
    let a = serde_json::json!({
        "title": "race",
        "body": "body-a",
        "tags": [],
        "idempotency_key": "race-key",
    });
    let b = serde_json::json!({
        "title": "race",
        "body": "body-b",
        "tags": [],
        "idempotency_key": "race-key",
    });
    let (ra, rb) = tokio::join!(
        call_test_model_tool(&tools, authz.clone(), owner, "core_remember", a),
        call_test_model_tool(&tools, authz.clone(), owner, "core_remember", b),
    );
    let outcomes = [ra, rb];
    let oks: Vec<_> = outcomes.iter().filter(|r| r.is_ok()).collect();
    let errs: Vec<_> = outcomes.iter().filter(|r| r.is_err()).collect();
    assert_eq!(oks.len(), 1, "exactly one admission: {outcomes:?}");
    assert_eq!(errs.len(), 1, "exactly one conflict: {outcomes:?}");
    let err = errs[0].as_ref().err().unwrap();
    assert!(is_idempotency_conflict(err), "{err}");

    let same = serde_json::json!({
        "title": "race2",
        "body": "same",
        "tags": [],
        "idempotency_key": "race-key-same",
    });
    let (s1, s2) = tokio::join!(
        call_test_model_tool(&tools, authz.clone(), owner, "core_remember", same.clone()),
        call_test_model_tool(&tools, authz.clone(), owner, "core_remember", same),
    );
    let (o1, o2) = (s1?, s2?);
    let handles = [handle(&o1)?, handle(&o2)?];
    assert_eq!(handles[0], handles[1]);
    let replays = [
        o1["idempotent_replay"] == true,
        o2["idempotent_replay"] == true,
    ];
    assert_eq!(
        replays.iter().filter(|r| **r).count(),
        1,
        "one fresh + one replay: {o1} / {o2}"
    );
    built.shutdown().await;
    Ok(())
}
