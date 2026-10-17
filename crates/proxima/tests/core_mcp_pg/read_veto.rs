//! #479 item 1: a host read veto hides an owner from every read path, not
//! only from search. By-id reads, batch reads, lineage, recall, think and
//! the write-path citation check all go through the one read gate, and a
//! vetoed owner's memory reads exactly like one that does not exist.

use super::*;
use proxima::flavor::{AuthorizationHook, AuthzInput, AuthzOperation, AuthzOutcome, AuthzVeto};
use std::sync::Mutex;

/// Owners the hook vetoes every `Read` on. Each test uses fresh owners, so
/// the shared statics never mix two tests' decisions.
static VETOED: Mutex<Vec<OwnerId>> = Mutex::new(Vec::new());
/// Every `Read` decision the observers saw: `(resolved owner, outcome)`.
static READ_EVENTS: Mutex<Vec<(OwnerId, AuthzOutcome)>> = Mutex::new(Vec::new());

#[derive(Debug)]
struct ReadVetoHook;

impl AuthorizationHook for ReadVetoHook {
    fn veto(&self, input: &AuthzInput<'_>) -> Result<(), AuthzVeto> {
        let vetoed = VETOED.lock().expect("veto set").contains(input.resolved);
        if vetoed && matches!(input.operation, AuthzOperation::Read { .. }) {
            return Err(AuthzVeto("host veto on this owner".into()));
        }
        Ok(())
    }

    fn observe(&self, input: &AuthzInput<'_>, outcome: AuthzOutcome) {
        if matches!(input.operation, AuthzOperation::Read { .. }) {
            READ_EVENTS
                .lock()
                .expect("read events")
                .push((*input.resolved, outcome));
        }
    }
}

struct ReadVetoApp;

impl FlavorBundle for ReadVetoApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), proxima_core::FlavorRegistryError> {
        registry.add_authorization_hook(Arc::new(ReadVetoHook));
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

impl FlavorApp for ReadVetoApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "read-veto-test",
            title: "Read Veto Test",
            version: "1",
        }
    }
}

fn handles(rows: &serde_json::Value, list: &str, field: &str) -> Vec<String> {
    rows[list]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row[field].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// A well-formed local memory id that names nothing: `existing` with its
/// random tail flipped, so the timestamp and the store number (bytes 10-11)
/// still say "this store" and no remote lookup is involved.
fn absent_local_id(existing: &str) -> Result<String, uuid::Error> {
    let mut bytes = *existing.parse::<Uuid>()?.as_bytes();
    for byte in &mut bytes[12..] {
        *byte ^= 0xff;
    }
    Ok(Uuid::from_bytes(bytes).to_string())
}

/// What a caller can observe of a failed call: its kind and its text, with
/// the one id it named replaced, so a vetoed id and an absent id compare
/// equal iff they are indistinguishable.
fn observed(err: &CoreMcpError, id: &str) -> String {
    format!("{err:?}").replace(id, "<id>")
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one fixture, every read path, control then veto
async fn a_host_read_veto_hides_an_owner_from_every_read_and_citation_path() {
    let split_db = clone_split_core_db("proxima_core_read_veto")
        .await
        .expect("PG required for tests");
    let (runtime_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let personal = OwnerId::from(UserId::new(Uuid::now_v7()));
        let group = OwnerId::from(GroupId::new(Uuid::now_v7()));
        let built = Proxima::<ReadVetoApp>::app()
            .database_url(runtime_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(personal)
            .served_surface(proxima_core::ServedSurface::All)
            .build()
            .await?;
        let tools = built.host().core_mcp_tools();
        let authz = space_authz(personal, vec![personal, group], Role::admin());
        let shared = server_issued_group_space_selector(&tools, authz.clone(), personal).await;
        let call = |name: &'static str, args: serde_json::Value| {
            call_test_model_tool(&tools, authz.clone(), personal, name, args)
        };
        let read = |uri: String| {
            let (tools, authz) = (&tools, authz.clone());
            async move { read_test_model_resource(tools, authz, personal, &uri).await }
        };

        let m_p = call(
            "core_remember",
            serde_json::json!({"space":"current","title":"veto personal fact",
                "body":"vetoneedle personal observation","tags":[]}),
        )
        .await?["handle"]
            .as_str()
            .ok_or("personal handle")?
            .to_owned();
        let m_gf = call(
            "core_remember",
            serde_json::json!({"space":shared.clone(),"title":"veto group fact",
                "body":"vetoneedle group observation","tags":[]}),
        )
        .await?["handle"]
            .as_str()
            .ok_or("group fact handle")?
            .to_owned();
        // The group abstraction is made from the personal fact: a lineage
        // edge mG -> mP that a descendants walk from mP follows.
        let m_g = call(
            "core_derive",
            serde_json::json!({"space":shared.clone(),"kind":"Abstraction",
                "title":"veto group pattern","body":"vetoneedle group pattern",
                "tags":[],"source_handles":[m_p.clone()],"model_id":"test-model"}),
        )
        .await?["handle"]
            .as_str()
            .ok_or("group abstraction handle")?
            .to_owned();
        let absent = absent_local_id(&m_p)?;

        let lineage_uri =
            format!("proxima://memory/{m_p}/lineage?direction=descendants&depth=3&limit=50");
        // A question cue searches the current space only (search is gated
        // already); subject cues read sketches through the read set.
        let recall_args = serde_json::json!({"subjects":[m_p.clone(), m_gf.clone()],"limit":16});
        let think_args = serde_json::json!({"seeds":[m_p.clone()],"direction":"descendants",
            "depth":2,"limit":16});
        let think_handles = |page: &serde_json::Value| handles(page, "visits", "handle");
        let recall_handles = |page: &serde_json::Value| handles(page, "sketches", "handle");

        // Control: without the veto every path reaches the group's memories,
        // so the assertions below are about the veto and not the fixture.
        assert_eq!(
            read(format!("proxima://memory/{m_g}")).await?["memory"],
            m_g
        );
        let both = read(format!("proxima://memories?ids={m_p},{m_g}")).await?;
        assert_eq!(handles(&both, "memories", "memory").len(), 2, "{both}");
        let lineage = read(lineage_uri.clone()).await?;
        assert!(
            handles(&lineage, "nodes", "memory").contains(&m_g),
            "{lineage}"
        );
        let recall = call("core_recall", recall_args.clone()).await?;
        assert!(recall_handles(&recall).contains(&m_gf), "{recall}");
        let think = call("core_think", think_args.clone()).await?;
        assert!(think_handles(&think).contains(&m_g), "{think}");
        call(
            "core_derive",
            serde_json::json!({"space":"current","kind":"Abstraction",
                "title":"control citation","body":"cites the group fact","tags":[],
                "source_handles":[m_gf.clone()],"model_id":"test-model",
                "idempotency_key":"read-veto-control-citation"}),
        )
        .await?;

        // Veto every read on the group.
        VETOED.lock().expect("veto set").push(group);
        let events_from = READ_EVENTS.lock().expect("read events").len();

        // 1. Search: refused over the group, and the group's memories are
        // absent from an unscoped search (regression guard; already gated).
        let scoped = call(
            "core_search_memories",
            serde_json::json!({"query":"vetoneedle","mode":"lexical","spaces":[shared.clone()],
                "limit":10}),
        )
        .await;
        assert!(
            scoped.is_err(),
            "search over a vetoed space is refused: {scoped:?}"
        );
        let unscoped = call(
            "core_search_memories",
            serde_json::json!({"query":"vetoneedle","mode":"lexical","limit":10}),
        )
        .await?;
        let hits = handles(&unscoped, "memories", "memory");
        assert!(!hits.contains(&m_g) && !hits.contains(&m_gf), "{unscoped}");

        // 2. By-id read: identical to an id that does not exist.
        let vetoed = read(format!("proxima://memory/{m_g}"))
            .await
            .expect_err("a vetoed memory is not returned");
        let missing = read(format!("proxima://memory/{absent}"))
            .await
            .expect_err("an absent memory is not returned");
        assert_eq!(observed(&vetoed, &m_g), observed(&missing, &absent));

        // 3. Batch read: the vetoed id is reported exactly like a missing one.
        let batch = read(format!("proxima://memories?ids={m_p},{m_g}")).await?;
        assert_eq!(
            handles(&batch, "memories", "memory"),
            vec![m_p.clone()],
            "{batch}"
        );
        assert_eq!(
            batch["missing"],
            serde_json::json!([m_g.clone()]),
            "{batch}"
        );
        let batch_absent = read(format!("proxima://memories?ids={m_p},{absent}")).await?;
        assert_eq!(
            batch.to_string().replace(&m_g, "<id>"),
            batch_absent.to_string().replace(&absent, "<id>"),
        );

        // 4. Lineage from the personal fact no longer reaches the group.
        let lineage = read(lineage_uri).await?;
        assert!(!lineage.to_string().contains(&m_g), "{lineage}");

        // 5. Recall and think never surface the group's memories.
        let recall = call("core_recall", recall_args).await?;
        let recalled = recall_handles(&recall);
        assert!(
            !recalled.contains(&m_g) && !recalled.contains(&m_gf),
            "{recall}"
        );
        assert!(
            recalled.contains(&m_p),
            "the personal fact is still recalled: {recall}"
        );
        let think = call("core_think", think_args).await?;
        assert!(!think.to_string().contains(&m_g), "{think}");

        // 6. A write citing the vetoed memory fails exactly like citing an
        // id that does not exist.
        let cite = |source: String, key: &'static str| {
            call(
                "core_derive",
                serde_json::json!({"space":"current","kind":"Abstraction",
                    "title":"vetoed citation","body":"cites a hidden fact","tags":[],
                    "source_handles":[source],"model_id":"test-model","idempotency_key":key}),
            )
        };
        let cited_vetoed = cite(m_gf.clone(), "read-veto-cite-vetoed")
            .await
            .expect_err("citing a vetoed memory is refused");
        let cited_absent = cite(absent.clone(), "read-veto-cite-absent")
            .await
            .expect_err("citing an absent memory is refused");
        assert_eq!(
            observed(&cited_vetoed, &m_gf),
            observed(&cited_absent, &absent)
        );
        assert!(
            format!("{cited_vetoed:?}").contains("entry not found"),
            "{cited_vetoed:?}"
        );

        // 7. The observers saw the denials, and never an allow on the group.
        let events = READ_EVENTS.lock().expect("read events")[events_from..].to_vec();
        assert!(
            events.contains(&(group, AuthzOutcome::DeniedVeto)),
            "{events:?}"
        );
        assert!(
            !events.contains(&(group, AuthzOutcome::Allowed)),
            "{events:?}"
        );

        VETOED
            .lock()
            .expect("veto set")
            .retain(|owner| *owner != group);
        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("read veto integration test failed");
}
