//! Host template families: built once, collected by their own GC only.
//!
//! Every test names its families after a fresh id, so concurrent runs on one
//! server never see each other's templates. The one test that builds in a
//! built-in family owns `proxima_tmpl_split_*` (nothing else in the workspace
//! builds there since the split-core fixture moved to a host family).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use proxima_pg_testkit::{
    TemplateFamily, db_url, drop_db, drop_stale_templates, drop_stale_templates_in,
    ensure_template, ensure_template_in, sweep_stale_test_dbs,
};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

/// A hash no other test run shares.
fn random_hash() -> u64 {
    let bytes = *Uuid::now_v7().as_bytes();
    u64::from_be_bytes(bytes[8..].try_into().expect("8 bytes"))
}

/// 12 hex digits that no other test run shares.
fn unique_id() -> String {
    Uuid::now_v7().simple().to_string()[20..].to_owned()
}

fn family(prefix: &str) -> TemplateFamily {
    TemplateFamily::new(prefix).expect("test family prefix")
}

async fn exists(name: &str) -> bool {
    let mut conn = PgConnection::connect(&proxima_pg_testkit::admin_url())
        .await
        .expect("admin connect");
    let found = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
    )
    .bind(name)
    .fetch_one(&mut conn)
    .await
    .expect("exists");
    conn.close().await.expect("close");
    found
}

/// Ensure the template and give the lease straight back: for a test of what
/// collection does to templates nobody is about to clone.
async fn built(family: &TemplateFamily, hash: u64) -> String {
    let lease = ensure_template_in(family, hash, build_marker)
        .await
        .expect("ensure_template_in");
    let name = lease.name().to_owned();
    lease.release().await.expect("release");
    name
}

/// A build that leaves a table behind, so a template is not an empty database.
async fn build_marker(pool: sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("CREATE TABLE marker (id integer)")
        .execute(&pool)
        .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sixteen_concurrent_callers_build_the_template_once() {
    let family = family(&format!("tfonce{}_", unique_id()));
    let builds = Arc::new(AtomicUsize::new(0));
    let callers: Vec<_> = (0..16)
        .map(|_| {
            let family = family.clone();
            let builds = Arc::clone(&builds);
            tokio::spawn(async move {
                ensure_template_in(&family, 7, |pool| async move {
                    builds.fetch_add(1, Ordering::SeqCst);
                    build_marker(pool).await
                })
                .await
            })
        })
        .collect();
    let mut leases = Vec::new();
    for caller in callers {
        leases.push(caller.await.expect("task").expect("ensure_template_in"));
    }

    let template = family.template_name(7);
    let present = exists(&template).await;
    let names: Vec<String> = leases.iter().map(|lease| lease.name().to_owned()).collect();
    for lease in leases {
        lease.release().await.expect("release");
    }
    drop_db(&template).await.expect("clean up");
    assert_eq!(
        builds.load(Ordering::SeqCst),
        1,
        "one build, sixteen callers"
    );
    assert!(names.iter().all(|name| *name == template), "{names:?}");
    assert!(present);
}

#[tokio::test]
async fn a_template_is_the_prefix_and_sixteen_hex_digits() {
    let family = family(&format!("tfname{}_", unique_id()));
    let name = built(&family, 0xab).await;
    drop_db(&name).await.expect("clean up");
    assert_eq!(name, format!("{}00000000000000ab", family.prefix()));
}

#[tokio::test]
async fn a_failed_build_is_not_a_template() {
    let family = family(&format!("tffail{}_", unique_id()));
    let failed = ensure_template_in(&family, 1, |_| async {
        Err(sqlx::Error::Protocol("simulated migration failure".into()))
    })
    .await;
    let after_failure = exists(&family.template_name(1)).await;

    let rebuilt = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&rebuilt);
    let retry = ensure_template_in(&family, 1, |pool| async move {
        flag.store(true, Ordering::SeqCst);
        build_marker(pool).await
    })
    .await
    .expect("the retry builds");
    assert_eq!(retry.name(), family.template_name(1));
    retry.release().await.expect("release");
    drop_db(&family.template_name(1)).await.expect("clean up");

    assert!(failed.is_err());
    assert!(!after_failure, "a failed build must leave no template");
    assert!(rebuilt.load(Ordering::SeqCst), "the retry must build again");
}

#[tokio::test]
async fn a_new_hash_collects_the_old_template_once_it_is_idle() {
    let family = family(&format!("tfgc{}_", unique_id()));
    let old = built(&family, 1).await;

    // A session on the old template keeps it, as it keeps any busy database.
    let busy = PgConnection::connect(&db_url(&old)).await.expect("busy");
    let new = built(&family, 2).await;
    let kept_while_busy = exists(&old).await;
    busy.close().await.expect("close");

    let dropped = drop_stale_templates_in(&family, 2).await.expect("gc");
    let old_after = exists(&old).await;
    let new_after = exists(&new).await;
    drop_db(&new).await.expect("clean up");

    assert!(kept_while_busy, "a template with a session is not idle");
    assert_eq!(dropped, 1);
    assert!(!old_after, "idle and not kept: dropped");
    assert!(new_after, "the kept template stays");
}

/// The interleaving that lost a template: caller A ensures a template and
/// has not cloned it yet; caller B ensures another hash, or collects, and the
/// first is idle and not kept. A's lease is what keeps it.
#[tokio::test]
async fn a_leased_template_survives_collection_until_the_lease_is_released() {
    let family = family(&format!("tflease{}_", unique_id()));
    let leased = ensure_template_in(&family, 1, build_marker)
        .await
        .expect("A ensures");
    let name = leased.name().to_owned();

    // B builds another hash: its collection finds `name` idle and not kept.
    let other = built(&family, 2).await;
    let after_ensure = exists(&name).await;
    // A standalone collection finds it too.
    let skipped = drop_stale_templates_in(&family, 2).await.expect("gc");
    let after_gc = exists(&name).await;

    leased.release().await.expect("A is done cloning");
    let collected = drop_stale_templates_in(&family, 2).await.expect("gc");
    let after_release = exists(&name).await;
    drop_db(&other).await.expect("clean up");

    assert!(
        after_ensure,
        "an ensure elsewhere never collects a leased template"
    );
    assert_eq!(skipped, 0);
    assert!(after_gc, "nor does a standalone collection");
    assert_eq!(collected, 1, "released: collected like any idle template");
    assert!(!after_release);
}

/// A lease is a lock on the name, so it holds across processes and across the
/// connections of one: the lease of a template that does not exist yet is
/// taken before it is built, and ensuring the same hash again shares it.
#[tokio::test]
async fn leases_of_one_template_are_shared() {
    let family = family(&format!("tfshare{}_", unique_id()));
    let first = ensure_template_in(&family, 1, build_marker)
        .await
        .expect("first");
    let second = ensure_template_in(&family, 1, build_marker)
        .await
        .expect("a second lease on the same template does not wait for the first");
    let name = first.name().to_owned();
    first.release().await.expect("release");
    let _ = built(&family, 2).await;
    let still_leased = exists(&name).await;
    second.release().await.expect("release");
    let dropped = drop_stale_templates_in(&family, 2).await.expect("gc");
    drop_db(&family.template_name(2)).await.expect("clean up");

    assert!(still_leased, "one lease left is still a lease");
    assert_eq!(dropped, 1);
}

#[tokio::test]
async fn a_family_collects_nothing_outside_itself() {
    let id = unique_id();
    // `outer` is a text prefix of `inner`'s templates, so `outer_%` matches
    // them; they are another family's and must survive `outer`'s GC.
    let outer = family(&format!("tfiso{id}_"));
    let inner = family(&format!("tfiso{id}_in_"));
    let beside = family(&format!("tfisx{id}_"));
    let outer_old = built(&outer, 1).await;
    let inner_old = built(&inner, 1).await;
    let beside_old = built(&beside, 1).await;

    let outer_new = built(&outer, 2).await;
    let outer_old_after = exists(&outer_old).await;
    let inner_after = exists(&inner_old).await;
    let beside_after = exists(&beside_old).await;

    let inner_dropped = drop_stale_templates_in(&inner, 2).await.expect("inner gc");
    let inner_old_after = exists(&inner_old).await;
    let outer_new_after = exists(&outer_new).await;
    for name in [&outer_new, &beside_old] {
        drop_db(name).await.expect("clean up");
    }

    assert!(!outer_old_after, "the family's own stale template goes");
    assert!(
        inner_after,
        "a family extending this prefix is not collected by it"
    );
    assert!(beside_after, "a sibling family is not collected");
    assert_eq!(inner_dropped, 1);
    assert!(!inner_old_after);
    assert!(
        outer_new_after,
        "inner's GC never reaches the family it extends"
    );
}

#[tokio::test]
async fn built_in_and_host_collection_never_meet() {
    let host = family(&format!("tfmeet{}_", unique_id()));
    let host_template = built(&host, 1).await;
    let builtin = || format!("proxima_tmpl_split_{:016x}", random_hash());
    let (first, second, third) = (builtin(), builtin(), builtin());

    // Built-in collection: building `second` collects `first`; collecting with
    // a `keep` that names nothing collects `second`; the host's template
    // survives both.
    ensure_template(&first, build_marker).await.expect("first");
    ensure_template(&second, build_marker)
        .await
        .expect("second");
    let first_after_build = exists(&first).await;
    let host_after_build_gc = exists(&host_template).await;
    drop_stale_templates(&builtin())
        .await
        .expect("built-in collection");
    let second_after_gc = exists(&second).await;
    let host_after_gc = exists(&host_template).await;

    // Host collection: with `keep` naming nothing it collects the host
    // template and no built-in one.
    ensure_template(&third, build_marker).await.expect("third");
    let host_dropped = drop_stale_templates_in(&host, 9)
        .await
        .expect("host collection");
    let host_after_host_gc = exists(&host_template).await;
    let third_after_host_gc = exists(&third).await;
    drop_db(&third).await.expect("clean up");

    assert!(!first_after_build, "built-in collection works");
    assert!(
        host_after_build_gc,
        "building a built-in template spares a host family"
    );
    assert!(!second_after_gc);
    assert!(
        host_after_gc,
        "built-in collection never reaches a host family"
    );
    assert_eq!(host_dropped, 1);
    assert!(!host_after_host_gc);
    assert!(
        third_after_host_gc,
        "host collection never reaches a built-in family"
    );
}

#[tokio::test]
async fn the_untracked_sweep_never_takes_a_template() {
    // A `proxima_` name is a candidate of the sweep's untracked leftovers;
    // only a UUID-suffixed name older than the grace is taken, and a template
    // ends in 16 hex digits.
    let family = family(&format!("proxima_tfsweep{}_", unique_id()));
    let template = built(&family, 3).await;
    sweep_stale_test_dbs().await.expect("sweep");
    let survived = exists(&template).await;
    drop_db(&template).await.expect("clean up");
    assert!(survived, "a template is not a clone");
}
