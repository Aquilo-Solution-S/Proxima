//! Ignored live demo: measurable Visionary -> Planner -> Worker -> Verifier
//! -> Goal-Reviewer wheel.
//!
//! Compile:
//!
//! ```sh
//! cargo test -p proxima-demo-wheel --test demo_wheel_pg --no-run
//! ```
//!
//! Live:
//!
//! ```sh
//! set -a; source ~/.proxima/.env; set +a
//! PROXIMA_LIVE_MISTRAL=1 \
//! PROXIMA_DEMO_REPO=/private/tmp/proxima-signal-match \
//! cargo test -p proxima-demo-wheel --test demo_wheel_pg -- --ignored --nocapture --test-threads=1
//! ```

mod demo_wheel;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn measurable_complex_demo_wheel() -> Result<(), Box<dyn std::error::Error>> {
    demo_wheel::run_from_env().await
}
