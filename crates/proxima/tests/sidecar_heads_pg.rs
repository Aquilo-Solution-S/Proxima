//! Contract-derived note heads through the flavor facade and real owner RLS.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

#[path = "fixtures/sidecar_heads_tombstone.rs"]
mod tombstone_fixture;

use std::collections::BTreeSet;

use example_engine::ExampleEngine;
use proxima::flavor::{SidecarAtom, SidecarHeadsScope, ToolError, sidecar_heads_by_key};
use proxima::{AuthPath, AuthzContext, ErrorCode, GroupId, MemoryId, OwnerId, Role, UserId};
use proxima_example::testkit::remember_note;
use proxima_example::{NoteV1, StampV1, note_heads};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn owner() -> OwnerId {
    OwnerId::from(UserId::new(Uuid::now_v7()))
}

fn authz(owners: &[OwnerId]) -> AuthzContext {
    proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        owners.iter().map(|owner| (*owner, Role::admin())),
        AuthPath::HostBearer,
    ))
}

fn note(notebook_id: Uuid, key: &str, body: &str) -> NoteV1 {
    NoteV1 {
        notebook_id,
        key: key.to_owned(),
        body: body.to_owned(),
        written_at: time::OffsetDateTime::from_unix_timestamp(1_700_000_000)
            .expect("fixture payload clock"),
    }
}

async fn remember(
    fixture: &ExampleEngine,
    owner: OwnerId,
    notebook: Uuid,
    key: &str,
    body: &str,
) -> MemoryId {
    remember_note(
        &fixture.engine,
        &authz(&[owner]),
        owner,
        &note(notebook, key, body),
    )
    .await
    .expect("the fixture note is admitted")
}

fn protocol_error(error: ToolError, expected: ErrorCode) {
    let ToolError::Protocol(error) = error else {
        panic!("expected protocol {expected:?}, got {error}");
    };
    assert_eq!(error.code, expected, "{error}");
}

#[tokio::test]
async fn note_heads_return_only_current_revisions_and_whole_key_prefixes() -> TestResult {
    let fixture = ExampleEngine::new("sidecar_heads_revisions", |_| {}).await;
    let owner = owner();
    let authz = authz(&[owner]);
    let notebook = fixture.notebook(&owner).await;
    let elsewhere = fixture.notebook(&owner).await;
    let old = remember(&fixture, owner, notebook, "alpha", "old observation").await;
    let latest = remember(&fixture, owner, notebook, "alpha", "new observation").await;
    let beta = remember(&fixture, owner, notebook, "beta", "another series").await;
    let other_notebook = remember(&fixture, owner, elsewhere, "alpha", "elsewhere").await;

    let page = note_heads(
        &fixture.engine,
        &authz,
        SidecarHeadsScope::Owner(owner),
        &[],
        None,
        100,
    )
    .await?;
    let ids: BTreeSet<_> = page.heads.iter().map(|head| head.memory_id).collect();
    assert_eq!(ids, BTreeSet::from([latest, beta, other_notebook]));
    assert!(!ids.contains(&old));
    assert!(page.next_cursor.is_none());
    assert!(
        page.heads
            .windows(2)
            .all(|heads| heads[0].memory_id.into_inner() > heads[1].memory_id.into_inner())
    );
    assert!(
        page.heads
            .iter()
            .all(|head| head.admitted_at > note(notebook, "", "").written_at)
    );
    for head in &page.heads {
        assert_eq!(head.admitted_at, head.memory_id.created_at());
    }

    let notebook_heads = note_heads(
        &fixture.engine,
        &authz,
        SidecarHeadsScope::Owner(owner),
        &[SidecarAtom::Uuid(notebook)],
        None,
        100,
    )
    .await?;
    assert_eq!(
        notebook_heads
            .heads
            .iter()
            .map(|head| head.memory_id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([latest, beta])
    );
    let exact = note_heads(
        &fixture.engine,
        &authz,
        SidecarHeadsScope::Owner(owner),
        &[
            SidecarAtom::Uuid(notebook),
            SidecarAtom::Text("alpha".into()),
        ],
        None,
        100,
    )
    .await?;
    assert_eq!(exact.heads.len(), 1);
    assert_eq!(exact.heads[0].memory_id, latest);
    assert_eq!(
        exact.heads[0].natural_key,
        vec![
            ("notebook_id".into(), SidecarAtom::Uuid(notebook)),
            ("key".into(), SidecarAtom::Text("alpha".into())),
        ]
    );
    let partial_text = note_heads(
        &fixture.engine,
        &authz,
        SidecarHeadsScope::Owner(owner),
        &[SidecarAtom::Uuid(notebook), SidecarAtom::Text("al".into())],
        None,
        100,
    )
    .await?;
    assert!(
        partial_text.heads.is_empty(),
        "tuple prefixes compare complete values"
    );
    Ok(())
}

#[tokio::test]
async fn note_head_owner_and_readable_scopes_enforce_personal_and_group_access() -> TestResult {
    let fixture = ExampleEngine::new("sidecar_heads_owners", |_| {}).await;
    let personal = owner();
    let group = OwnerId::from(GroupId::new(Uuid::now_v7()));
    let hidden = owner();
    sqlx::query("INSERT INTO proxima_core.owners(owner_id, kind) VALUES ($1, 'group')")
        .bind(group.into_inner())
        .execute(&fixture.pool)
        .await?;
    let personal_book = fixture.notebook(&personal).await;
    let group_book = fixture.notebook(&group).await;
    let hidden_book = fixture.notebook(&hidden).await;
    let mine = remember(&fixture, personal, personal_book, "shared", "personal").await;
    let shared = remember(&fixture, group, group_book, "shared", "group").await;
    let secret = remember(&fixture, hidden, hidden_book, "shared", "private").await;
    let readable =
        proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(personal, Role::admin()), (group, Role::viewer())],
            AuthPath::HostBearer,
        ));
    let mine_only = note_heads(
        &fixture.engine,
        &readable,
        SidecarHeadsScope::Owner(personal),
        &[],
        None,
        100,
    )
    .await?;
    assert_eq!(
        mine_only
            .heads
            .iter()
            .map(|head| head.memory_id)
            .collect::<Vec<_>>(),
        vec![mine]
    );
    let group_only = note_heads(
        &fixture.engine,
        &readable,
        SidecarHeadsScope::Owner(group),
        &[],
        None,
        100,
    )
    .await?;
    assert_eq!(
        group_only
            .heads
            .iter()
            .map(|head| head.memory_id)
            .collect::<Vec<_>>(),
        vec![shared]
    );
    let both = note_heads(
        &fixture.engine,
        &readable,
        SidecarHeadsScope::Readable,
        &[],
        None,
        100,
    )
    .await?;
    let ids: BTreeSet<_> = both.heads.iter().map(|head| head.memory_id).collect();
    assert_eq!(ids, BTreeSet::from([mine, shared]));
    assert!(!ids.contains(&secret));
    protocol_error(
        note_heads(
            &fixture.engine,
            &readable,
            SidecarHeadsScope::Owner(hidden),
            &[],
            None,
            100,
        )
        .await
        .unwrap_err(),
        ErrorCode::Forbidden,
    );
    let personal_only = note_heads(
        &fixture.engine,
        &authz(&[personal]),
        SidecarHeadsScope::Readable,
        &[SidecarAtom::Uuid(group_book)],
        None,
        100,
    )
    .await?;
    assert!(personal_only.heads.is_empty());
    Ok(())
}

#[tokio::test]
async fn pages_skip_forgotten_heads_and_reject_invalid_prefixes() -> TestResult {
    let fixture = ExampleEngine::new("sidecar_heads_pages", |_| {}).await;
    let owner = owner();
    let authz = authz(&[owner]);
    let notebook = fixture.notebook(&owner).await;
    let mut expected = Vec::new();
    for key in ["first", "second", "third", "fourth"] {
        expected.push(remember(&fixture, owner, notebook, key, key).await);
    }
    let forgotten = remember(
        &fixture,
        owner,
        notebook,
        "forgotten",
        "retained but hidden",
    )
    .await;
    fixture
        .engine
        .forget_memory(&authz, owner, forgotten)
        .await?;
    expected.sort_unstable_by_key(|id| std::cmp::Reverse(id.into_inner()));
    let mut after = None;
    let mut actual = Vec::new();
    for _ in 0..4 {
        let page = note_heads(
            &fixture.engine,
            &authz,
            SidecarHeadsScope::Owner(owner),
            &[],
            after,
            2,
        )
        .await?;
        assert!(page.heads.len() <= 2);
        for head in page.heads {
            actual.push(head.memory_id);
        }
        after = page.next_cursor;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
    assert!(after.is_none());
    assert!(!actual.contains(&forgotten));
    for (prefix, limit) in [
        (vec![], 0),
        (vec![SidecarAtom::Bool(true)], 100),
        (
            vec![
                SidecarAtom::Uuid(notebook),
                SidecarAtom::Text("first".into()),
                SidecarAtom::I32(7),
            ],
            100,
        ),
    ] {
        protocol_error(
            note_heads(
                &fixture.engine,
                &authz,
                SidecarHeadsScope::Owner(owner),
                &prefix,
                None,
                limit,
            )
            .await
            .unwrap_err(),
            ErrorCode::InvalidArgument,
        );
    }
    protocol_error(
        sidecar_heads_by_key::<StampV1>(
            &fixture.engine,
            &authz,
            SidecarHeadsScope::Owner(owner),
            &[],
            None,
            100,
        )
        .await
        .unwrap_err(),
        ErrorCode::InvalidArgument,
    );
    Ok(())
}

#[tokio::test]
async fn a_tombstone_is_the_current_head_and_does_not_resurrect_its_prior() -> TestResult {
    tombstone_fixture::assert_tombstone_head().await
}
