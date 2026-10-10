//! `PROXIMA_TEST_PG_URL` is the admin URL and has no default.
//!
//! The process environment is global and cannot be edited from a test
//! (`set_var` is unsafe), so each case re-executes this test binary with the
//! variable set the way it needs and reads the child's exit status and
//! output. `child` is the re-executed test: without
//! `PROXIMA_PG_TESTKIT_CHILD` it does nothing.

use std::process::{Command, Output};

use proxima_pg_testkit::{DbGuard, SplitRoleDb, admin_url, admin_url_or_skip, create_db, db_url};
use sqlx::{Connection, PgConnection};

const MODE_VAR: &str = "PROXIMA_PG_TESTKIT_CHILD";
const URL_VAR: &str = "PROXIMA_TEST_PG_URL";

#[tokio::test]
async fn child() {
    let Ok(mode) = std::env::var(MODE_VAR) else {
        return;
    };
    match mode.as_str() {
        "admin_url" => {
            let _ = admin_url();
        }
        "db_url" => {
            let _ = db_url("any_database");
        }
        "or_skip" => match admin_url_or_skip() {
            Some(_) => println!("RESULT:some"),
            None => println!("RESULT:none"),
        },
        "async" => match create_db("any_database").await {
            Err(sqlx::Error::Configuration(message)) => println!("RESULT:configuration:{message}"),
            other => println!("RESULT:unexpected:{other:?}"),
        },
        "guard_panic" => {
            // Dropped while unwinding: the guard keeps its database and
            // prints the `psql` line this case reads.
            let _guard = DbGuard::adopt("any_database".to_owned());
            panic!("the test body fails");
        }
        "role_probe" => {
            let db = SplitRoleDb::create("admin_url_probe", &[])
                .await
                .expect("a split-role database from this admin URL");
            for (label, url) in [
                ("runtime", db.runtime_url()),
                ("platform", db.platform_url()),
            ] {
                let mut conn = PgConnection::connect(url).await.expect("role connection");
                let (user, superuser, bypass_rls): (String, bool, bool) = sqlx::query_as(
                    "SELECT current_user::text, rolsuper, rolbypassrls \
                     FROM pg_roles WHERE rolname = current_user",
                )
                .fetch_one(&mut conn)
                .await
                .expect("role attributes");
                println!("RESULT:{label}:{user}:{superuser}:{bypass_rls}");
            }
        }
        other => panic!("unknown child mode {other}"),
    }
}

/// Run `child` in `mode` with `PROXIMA_TEST_PG_URL` set to `url` (`None`:
/// removed) and `CI` set to `ci` (`None`: removed).
fn run_child(mode: &str, url: Option<&str>, ci: Option<&str>) -> Output {
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(MODE_VAR, mode)
        .env("RUST_BACKTRACE", "0");
    match url {
        Some(url) => command.env(URL_VAR, url),
        None => command.env_remove(URL_VAR),
    };
    match ci {
        Some(ci) => command.env("CI", ci),
        None => command.env_remove("CI"),
    };
    command.output().expect("re-execute the test binary")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The three spellings of "not configured".
const UNCONFIGURED: [Option<&str>; 3] = [None, Some(""), Some(" \t ")];

#[test]
fn an_unconfigured_admin_url_panics_naming_the_variable() {
    for mode in ["admin_url", "db_url"] {
        for url in UNCONFIGURED {
            let output = run_child(mode, url, None);
            let stderr = text(&output.stderr);
            assert!(!output.status.success(), "{mode} with {url:?} must panic");
            assert!(stderr.contains(URL_VAR), "{mode} with {url:?}: {stderr}");
            assert!(
                stderr.contains("no default"),
                "{mode} with {url:?}: {stderr}"
            );
        }
    }
}

#[test]
fn a_configured_admin_url_does_not_panic() {
    let output = run_child(
        "admin_url",
        Some("postgres://u:private-secret@localhost/db"),
        None,
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
}

#[test]
fn the_async_entry_points_return_a_configuration_error() {
    for url in UNCONFIGURED {
        let output = run_child("async", url, None);
        let stdout = text(&output.stdout);
        assert!(output.status.success(), "{url:?}: {}", text(&output.stderr));
        assert!(
            stdout.contains("RESULT:configuration:") && stdout.contains(URL_VAR),
            "{url:?}: {stdout}"
        );
    }
}

#[test]
fn skipping_is_visible_locally_and_a_failure_under_ci() {
    for url in UNCONFIGURED {
        let local = run_child("or_skip", url, None);
        assert!(local.status.success(), "{url:?}: {}", text(&local.stderr));
        assert!(text(&local.stdout).contains("RESULT:none"), "{url:?}");
        let stderr = text(&local.stderr);
        assert_eq!(
            stderr.lines().filter(|line| line.contains(URL_VAR)).count(),
            1,
            "one line on stderr names the variable: {stderr}"
        );

        let ci = run_child("or_skip", url, Some("true"));
        assert!(!ci.status.success(), "{url:?}: CI=true must not skip");
        assert!(text(&ci.stderr).contains(URL_VAR), "{url:?}");
    }
}

#[test]
fn skipping_only_skips_the_unconfigured() {
    let output = run_child("or_skip", Some("postgres://u:p@localhost/db"), Some("true"));
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("RESULT:some"));
}

/// The same login as `admin`, carried only in the query string: no user and
/// no password in the authority.
fn with_credentials_in_the_query(admin: &str) -> String {
    let mut url = url::Url::parse(admin).expect("admin URL");
    let user = url.username().to_owned();
    let password = url.password().map(str::to_owned);
    url.set_username("").expect("clear the user");
    url.set_password(None).expect("clear the password");
    url.query_pairs_mut().append_pair("user", &user);
    if let Some(password) = password {
        url.query_pairs_mut().append_pair("password", &password);
    }
    assert_eq!(url.username(), "", "the authority carries no credentials");
    assert_eq!(url.password(), None, "the authority carries no credentials");
    url.to_string()
}

/// `RESULT:<label>:<user>:<superuser>:<bypassrls>` from `role_probe`.
fn probe_result(stdout: &str, label: &str) -> String {
    let prefix = format!("RESULT:{label}:");
    stdout
        .lines()
        .find_map(|line| {
            line.split_once(&prefix)
                .map(|(_, rest)| rest.trim().to_owned())
        })
        .unwrap_or_else(|| panic!("no {label} result in {stdout}"))
}

#[test]
fn split_roles_stay_unprivileged_when_the_admin_credentials_are_in_the_query() {
    let Some(admin) = admin_url_or_skip() else {
        return;
    };
    let url = with_credentials_in_the_query(&admin);
    let output = run_child("role_probe", Some(&url), None);
    let stdout = text(&output.stdout);
    assert!(output.status.success(), "{}{stdout}", text(&output.stderr));
    // Were the query's user and password to win over the role in the
    // authority, both connections would be the admin: a superuser.
    assert_eq!(
        probe_result(&stdout, "runtime"),
        "proxima_test_runtime:false:false"
    );
    assert_eq!(
        probe_result(&stdout, "platform"),
        "proxima_test_platform:false:false"
    );
}

#[test]
fn a_password_in_the_admin_url_is_not_printed_when_a_test_panics() {
    for url in [
        "postgres://u@localhost/db?password=private-secret",
        "postgres://u@localhost/db?sslmode=disable&user=u&password=private-secret",
        "postgres://u:private-secret@localhost/db",
        "postgres://u:private-secret@localhost/db?password=private-secret",
    ] {
        let output = run_child("guard_panic", Some(url), None);
        let stderr = text(&output.stderr);
        assert!(!output.status.success(), "{url}: the child test must panic");
        assert!(
            stderr.contains("keeping test database") && stderr.contains("psql"),
            "{url}: the diagnostic is printed: {stderr}"
        );
        assert!(
            !stderr.contains("private-secret"),
            "{url}: the password reached stderr: {stderr}"
        );
        assert!(!text(&output.stdout).contains("private-secret"), "{url}");
    }
}
