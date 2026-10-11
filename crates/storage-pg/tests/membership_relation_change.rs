//! Single-relation removal and atomic relation replacement, through the
//! Engine and the Postgres membership port.
//!
//! A downgrade is one transaction under the group's membership lock: the
//! `from` row is deleted and the `to` row inserted, so no reader sees the
//! member with neither role and a failed second step takes the first with it.
//! Authorization, hooks and the manage gate are the ones `add_member` and
//! `remove_member` use.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use proxima_core::authz::{
    AuthorizationHook, AuthzInput, AuthzOperation, AuthzOutcome, AuthzVeto, MembershipChange,
};
use proxima_core::storage_ports::{OwnerMembershipAdminPort, OwnerWritePermit};
use proxima_core::{
    AccessCeiling, AccessKind, AuthPath, AuthzContext, Engine, ErrorCode, FlavorRegistry, GroupId,
    OwnerRef, Relation, Role, StorageError, UserId,
};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::{PgStorage, group_role};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

type Seen = Arc<Mutex<Vec<(MembershipChange, Relation, AuthzOutcome)>>>;

/// Records every membership change the engine asks about with its outcome,
/// and vetoes one `(change, relation)` pair when told to.
#[derive(Debug)]
struct Recorder {
    veto: Option<(MembershipChange, Relation)>,
    seen: Seen,
}

impl AuthorizationHook for Recorder {
    fn veto(&self, input: &AuthzInput<'_>) -> Result<(), AuthzVeto> {
        match (&input.operation, self.veto) {
            (
                AuthzOperation::Membership {
                    change, relation, ..
                },
                Some((vetoed_change, vetoed_relation)),
            ) if *change == vetoed_change && *relation == vetoed_relation => {
                Err(AuthzVeto("vetoed".into()))
            }
            _ => Ok(()),
        }
    }

    fn observe(&self, input: &AuthzInput<'_>, outcome: AuthzOutcome) {
        if let AuthzOperation::Membership {
            change, relation, ..
        } = &input.operation
        {
            self.seen
                .lock()
                .expect("seen lock")
                .push((*change, *relation, outcome));
        }
    }
}

struct Harness {
    db_name: String,
    pg: PgStorage,
    /// A second, independent connection source: what another reader sees.
    observer: PgPool,
    engine: Engine,
    group: GroupId,
    /// Holds `Role::admin()` on the group.
    admin_authz: AuthzContext,
    seen: Seen,
}

impl Harness {
    async fn new(veto: Option<(MembershipChange, Relation)>) -> Self {
        let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
        if let Err(error) = create_core_db(&db_name).await {
            panic!("PG required for tests but admin connect failed: {error}");
        }
        let pg = PgStorage::connect(&db_url(&db_name))
            .await
            .expect("connect");
        pg.run_before_owner_rls_migrations().await.expect("migrate");
        let observer = PgPoolOptions::new()
            .max_connections(4)
            .connect(&db_url(&db_name))
            .await
            .expect("observer pool");

        let seen = Seen::default();
        let mut registry = FlavorRegistry::new();
        registry.add_authorization_hook(Arc::new(Recorder {
            veto,
            seen: seen.clone(),
        }));
        let engine = Engine::new(registry.freeze_or_panic_for_tests())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());

        let group = GroupId::new(Uuid::now_v7());
        let admin = UserId::new(Uuid::now_v7());
        pg.bootstrap_group_admin(group, admin, admin.into_inner())
            .await
            .expect("bootstrap");
        let admin_authz = AuthzContext::for_subject_with_role(
            admin,
            [(OwnerRef::Group(group), Role::admin())],
            AuthPath::HostBearer,
        );
        Self {
            db_name,
            pg,
            observer,
            engine,
            group,
            admin_authz,
            seen,
        }
    }

    fn permit(&self) -> OwnerWritePermit {
        OwnerWritePermit::new_for_tests(OwnerRef::Group(self.group), AccessKind::Goal)
    }

    /// A member holding exactly `relations`.
    async fn member_with(&self, relations: &[Relation]) -> UserId {
        let member = UserId::new(Uuid::now_v7());
        for relation in relations {
            self.pg
                .add_group_member(
                    &self.permit(),
                    self.group,
                    member,
                    *relation,
                    Uuid::now_v7(),
                )
                .await
                .expect("seed relation");
        }
        member
    }

    /// The member's relations as another connection reads them, in the
    /// `membership_relation` enum order (Admin, Editor, Viewer, Ingest).
    async fn relations(&self, member: UserId) -> Vec<Relation> {
        sqlx::query_scalar(
            "SELECT relation FROM proxima_core.group_memberships
              WHERE group_id = $1 AND member_user_id = $2 ORDER BY relation",
        )
        .bind(self.group.into_inner())
        .bind(member.into_inner())
        .fetch_all(&self.observer)
        .await
        .expect("read relations")
    }

    fn seen(&self) -> Vec<(MembershipChange, Relation, AuthzOutcome)> {
        self.seen.lock().expect("seen lock").clone()
    }

    async fn finish(self) {
        self.observer.close().await;
        let _ = drop_db(&self.db_name).await;
    }
}

/// Wait until some backend of this database waits on `wait_event`.
async fn wait_until_blocked(pool: &PgPool, wait_event: &str) {
    let blocked = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1 FROM pg_stat_activity
                      WHERE datname = current_database()
                        AND wait_event_type = 'Lock'
                        AND wait_event = $1)",
            )
            .bind(wait_event)
            .fetch_one(pool)
            .await
            .expect("read pg_stat_activity");
            if blocked {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(blocked.is_ok(), "no backend ever waited on {wait_event}");
}

#[tokio::test]
async fn remove_member_relation_removes_one_row_and_remove_member_still_removes_all() {
    let h = Harness::new(None).await;
    let member = h
        .member_with(&[Relation::Editor, Relation::Viewer, Relation::Ingest])
        .await;
    let bystander = h.member_with(&[Relation::Viewer]).await;

    h.engine
        .remove_member_relation(&h.admin_authz, h.group, member, Relation::Viewer)
        .await
        .expect("remove one relation");
    assert_eq!(
        h.relations(member).await,
        vec![Relation::Editor, Relation::Ingest],
        "only the named relation goes"
    );
    assert_eq!(
        h.seen(),
        vec![(
            MembershipChange::Remove,
            Relation::Viewer,
            AuthzOutcome::Allowed
        )],
        "one Remove for the one relation"
    );

    // An absent row is Ok and changes nothing, for the member and the group.
    h.engine
        .remove_member_relation(&h.admin_authz, h.group, member, Relation::Viewer)
        .await
        .expect("an absent row is Ok");
    h.engine
        .remove_member_relation(
            &h.admin_authz,
            h.group,
            UserId::new(Uuid::now_v7()),
            Relation::Admin,
        )
        .await
        .expect("a member with no rows is Ok");
    assert_eq!(
        h.relations(member).await,
        vec![Relation::Editor, Relation::Ingest]
    );
    assert_eq!(h.relations(bystander).await, vec![Relation::Viewer]);

    // `remove_member` is unchanged: every relation of the member.
    h.engine
        .remove_member(&h.admin_authz, h.group, member)
        .await
        .expect("remove all");
    assert_eq!(h.relations(member).await, Vec::<Relation>::new());
    assert_eq!(h.relations(bystander).await, vec![Relation::Viewer]);
    h.finish().await;
}

#[tokio::test]
async fn replace_member_relation_downgrades_and_keeps_unrelated_relations() {
    let h = Harness::new(None).await;
    // Admin beside an unrelated Ingest: the downgrade must not touch Ingest.
    let member = h.member_with(&[Relation::Admin, Relation::Ingest]).await;

    h.engine
        .replace_member_relation(
            &h.admin_authz,
            h.group,
            member,
            Relation::Admin,
            Relation::Viewer,
        )
        .await
        .expect("downgrade");
    assert_eq!(
        h.relations(member).await,
        vec![Relation::Viewer, Relation::Ingest]
    );
    assert_eq!(
        h.seen(),
        vec![
            (
                MembershipChange::Remove,
                Relation::Admin,
                AuthzOutcome::Allowed
            ),
            (
                MembershipChange::Add,
                Relation::Viewer,
                AuthzOutcome::Allowed
            ),
        ],
        "observers see Remove(from) then Add(to)"
    );

    // The probe a host runs resolves the new state to the join of what is left.
    let mut conn = h.observer.acquire().await.expect("connection");
    assert_eq!(
        group_role(&mut conn, member, h.group).await.expect("probe"),
        Some(Role::viewer().join(Role::ingest()))
    );
    drop(conn);

    // The member already holds `to`: the rows merge, nothing is duplicated.
    let merged = h.member_with(&[Relation::Editor, Relation::Viewer]).await;
    h.engine
        .replace_member_relation(
            &h.admin_authz,
            h.group,
            merged,
            Relation::Editor,
            Relation::Viewer,
        )
        .await
        .expect("replace onto an existing relation");
    assert_eq!(h.relations(merged).await, vec![Relation::Viewer]);
    h.finish().await;
}

#[tokio::test]
async fn replace_member_relation_refuses_a_member_without_from_and_changes_nothing() {
    let h = Harness::new(None).await;
    let holds_other = h.member_with(&[Relation::Viewer]).await;
    let holds_none = UserId::new(Uuid::now_v7());

    for (member, expected) in [
        (holds_other, vec![Relation::Viewer]),
        (holds_none, Vec::new()),
    ] {
        let err = h
            .engine
            .replace_member_relation(
                &h.admin_authz,
                h.group,
                member,
                Relation::Admin,
                Relation::Editor,
            )
            .await
            .expect_err("the member does not hold `from`");
        assert_eq!(
            err.code,
            ErrorCode::InvalidArgument,
            "a missing `from` is the caller's argument: {}",
            err.message
        );
        assert_eq!(h.relations(member).await, expected, "nothing changed");

        // The storage port names it `Conflict`.
        let storage =
            h.pg.replace_group_member_relation(
                &h.permit(),
                h.group,
                member,
                Relation::Admin,
                Relation::Editor,
                Uuid::now_v7(),
            )
            .await
            .expect_err("the member does not hold `from`");
        assert!(matches!(storage, StorageError::Conflict(_)), "{storage:?}");
        assert_eq!(h.relations(member).await, expected, "nothing changed");
    }
    h.finish().await;
}

#[tokio::test]
async fn replace_member_relation_refuses_the_same_relation_before_hooks_and_storage() {
    let h = Harness::new(None).await;
    let member = h.member_with(&[Relation::Editor]).await;

    let err = h
        .engine
        .replace_member_relation(
            &h.admin_authz,
            h.group,
            member,
            Relation::Editor,
            Relation::Editor,
        )
        .await
        .expect_err("from == to is not a change");
    assert_eq!(err.code, ErrorCode::InvalidArgument, "{}", err.message);
    assert!(h.seen().is_empty(), "no hook is asked about a non-change");
    assert_eq!(h.relations(member).await, vec![Relation::Editor]);

    // Straight at the port the same call is the same transaction: a held
    // `from` is replaced by itself, an unheld one is a Conflict.
    h.pg.replace_group_member_relation(
        &h.permit(),
        h.group,
        member,
        Relation::Editor,
        Relation::Editor,
        Uuid::now_v7(),
    )
    .await
    .expect("held");
    assert_eq!(h.relations(member).await, vec![Relation::Editor]);
    let missing =
        h.pg.replace_group_member_relation(
            &h.permit(),
            h.group,
            member,
            Relation::Viewer,
            Relation::Viewer,
            Uuid::now_v7(),
        )
        .await
        .expect_err("not held");
    assert!(matches!(missing, StorageError::Conflict(_)), "{missing:?}");
    h.finish().await;
}

#[tokio::test]
async fn a_veto_on_either_change_leaves_the_membership_unchanged() {
    for vetoed in [
        (MembershipChange::Remove, Relation::Admin),
        (MembershipChange::Add, Relation::Viewer),
    ] {
        let h = Harness::new(Some(vetoed)).await;
        let member = h.member_with(&[Relation::Admin]).await;

        let err = h
            .engine
            .replace_member_relation(
                &h.admin_authz,
                h.group,
                member,
                Relation::Admin,
                Relation::Viewer,
            )
            .await
            .expect_err("a veto refuses the replacement");
        assert_eq!(err.code, ErrorCode::Forbidden, "{vetoed:?}");
        assert_eq!(
            h.relations(member).await,
            vec![Relation::Admin],
            "a veto on {vetoed:?} must leave the membership unchanged"
        );
        let seen = h.seen();
        assert_eq!(
            seen.last()
                .map(|(change, relation, outcome)| (*change, *relation, *outcome)),
            Some((vetoed.0, vetoed.1, AuthzOutcome::DeniedVeto))
        );
        h.finish().await;
    }

    // `remove_member_relation` asks the same hooks once.
    let h = Harness::new(Some((MembershipChange::Remove, Relation::Viewer))).await;
    let member = h.member_with(&[Relation::Viewer]).await;
    let err = h
        .engine
        .remove_member_relation(&h.admin_authz, h.group, member, Relation::Viewer)
        .await
        .expect_err("a veto refuses the removal");
    assert_eq!(err.code, ErrorCode::Forbidden);
    assert_eq!(h.relations(member).await, vec![Relation::Viewer]);
    h.finish().await;
}

#[tokio::test]
async fn a_caller_with_write_but_without_manage_is_refused_by_both_methods() {
    let h = Harness::new(None).await;
    let member = h.member_with(&[Relation::Admin]).await;
    let write_only = Role::new(AccessCeiling::Goal, AccessCeiling::Goal, false)
        .expect("write == read is a valid role");
    let caller = AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(OwnerRef::Group(h.group), write_only)],
        AuthPath::HostBearer,
    );

    let replace = h
        .engine
        .replace_member_relation(&caller, h.group, member, Relation::Admin, Relation::Viewer)
        .await
        .expect_err("write without manage");
    let remove = h
        .engine
        .remove_member_relation(&caller, h.group, member, Relation::Admin)
        .await
        .expect_err("write without manage");
    assert_eq!(replace.code, ErrorCode::Forbidden);
    assert_eq!(remove.code, ErrorCode::Forbidden);
    assert_eq!(h.relations(member).await, vec![Relation::Admin]);
    assert!(h.seen().is_empty(), "the gate refuses before any hook");
    h.finish().await;
}

#[tokio::test]
async fn a_permit_for_another_group_is_refused_by_both_port_methods() {
    let h = Harness::new(None).await;
    let member = h.member_with(&[Relation::Admin]).await;
    let other = OwnerWritePermit::new_for_tests(
        OwnerRef::Group(GroupId::new(Uuid::now_v7())),
        AccessKind::Goal,
    );

    let replace =
        h.pg.replace_group_member_relation(
            &other,
            h.group,
            member,
            Relation::Admin,
            Relation::Viewer,
            Uuid::now_v7(),
        )
        .await
        .expect_err("permit names another owner");
    let remove =
        h.pg.remove_group_member_relation(&other, h.group, member, Relation::Admin)
            .await
            .expect_err("permit names another owner");
    assert!(matches!(replace, StorageError::ConstraintViolation(_)));
    assert!(matches!(remove, StorageError::ConstraintViolation(_)));
    assert_eq!(h.relations(member).await, vec![Relation::Admin]);
    h.finish().await;
}

/// The second step failing takes the first with it: the member is not left
/// removed (the failure case of remove-then-add).
#[tokio::test]
async fn a_failed_insert_rolls_the_delete_back() {
    let h = Harness::new(None).await;
    let member = h.member_with(&[Relation::Admin]).await;
    sqlx::raw_sql(
        "CREATE FUNCTION public.refuse_viewer_insert() RETURNS trigger
         LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'viewer refused'; END $$;
         CREATE TRIGGER refuse_viewer_insert
         BEFORE INSERT ON proxima_core.group_memberships
         FOR EACH ROW WHEN (NEW.relation = 'viewer')
         EXECUTE FUNCTION public.refuse_viewer_insert();",
    )
    .execute(&h.observer)
    .await
    .expect("install the failing step");

    h.engine
        .replace_member_relation(
            &h.admin_authz,
            h.group,
            member,
            Relation::Admin,
            Relation::Viewer,
        )
        .await
        .expect_err("the insert of `to` fails");
    assert_eq!(
        h.relations(member).await,
        vec![Relation::Admin],
        "the delete of `from` must have rolled back with the failed insert"
    );
    h.finish().await;
}

/// While the replacement is between its delete and its commit, another
/// connection still sees the old relation: no state with neither is
/// observable. The insert of `to` is held open by a conflicting uncommitted
/// row, which freezes the replacement exactly there.
#[tokio::test]
async fn no_reader_sees_the_member_without_a_role_mid_replacement() {
    let h = Harness::new(None).await;
    let member = h.member_with(&[Relation::Admin]).await;

    let mut blocker = h.observer.begin().await.expect("blocker");
    sqlx::query(
        "INSERT INTO proxima_core.group_memberships (group_id, member_user_id, relation)
         VALUES ($1, $2, 'viewer')",
    )
    .bind(h.group.into_inner())
    .bind(member.into_inner())
    .execute(&mut *blocker)
    .await
    .expect("uncommitted conflicting row");

    let pg = h.pg.clone();
    let (group, permit) = (h.group, h.permit());
    let replacement = tokio::spawn(async move {
        pg.replace_group_member_relation(
            &permit,
            group,
            member,
            Relation::Admin,
            Relation::Viewer,
            Uuid::now_v7(),
        )
        .await
    });
    wait_until_blocked(&h.observer, "transactionid").await;

    // Delete done, insert waiting: a reader sees exactly the old state.
    assert_eq!(h.relations(member).await, vec![Relation::Admin]);
    assert!(!replacement.is_finished());

    blocker.commit().await.expect("release the replacement");
    replacement
        .await
        .expect("join")
        .expect("replacement completes");
    assert_eq!(h.relations(member).await, vec![Relation::Viewer]);
    h.finish().await;
}

#[tokio::test]
async fn two_replacements_of_the_same_relation_serialize_one_wins() {
    let h = Harness::new(None).await;
    let member = h.member_with(&[Relation::Admin]).await;

    let replace = |pg: PgStorage| {
        let (group, permit) = (h.group, h.permit());
        async move {
            pg.replace_group_member_relation(
                &permit,
                group,
                member,
                Relation::Admin,
                Relation::Viewer,
                Uuid::now_v7(),
            )
            .await
        }
    };
    let (first, second) = tokio::join!(replace(h.pg.clone()), replace(h.pg.clone()));

    let mut outcomes = [first, second];
    outcomes.sort_by_key(Result::is_err);
    let [winner, loser] = outcomes;
    winner.expect("one replacement succeeds");
    assert!(
        matches!(loser, Err(StorageError::Conflict(_))),
        "the other finds `from` gone: {loser:?}"
    );
    assert_eq!(h.relations(member).await, vec![Relation::Viewer]);
    h.finish().await;
}

/// The replacement takes the group's membership lock, the one `add_member`
/// and `remove_member` take: while another transaction holds it, the
/// replacement waits and changes nothing.
#[tokio::test]
async fn replacement_waits_on_the_groups_membership_lock() {
    let h = Harness::new(None).await;
    let member = h.member_with(&[Relation::Admin]).await;

    let mut holder = h.observer.begin().await.expect("holder");
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('proxima-group-membership:' || $1::text, 0))",
    )
    .bind(h.group.into_inner())
    .execute(&mut *holder)
    .await
    .expect("take the membership lock");

    let pg = h.pg.clone();
    let (group, permit) = (h.group, h.permit());
    let replacement = tokio::spawn(async move {
        pg.replace_group_member_relation(
            &permit,
            group,
            member,
            Relation::Admin,
            Relation::Viewer,
            Uuid::now_v7(),
        )
        .await
    });
    wait_until_blocked(&h.observer, "advisory").await;
    assert!(!replacement.is_finished());
    assert_eq!(h.relations(member).await, vec![Relation::Admin]);

    holder.commit().await.expect("release the lock");
    replacement
        .await
        .expect("join")
        .expect("replacement completes");
    assert_eq!(h.relations(member).await, vec![Relation::Viewer]);
    h.finish().await;
}
