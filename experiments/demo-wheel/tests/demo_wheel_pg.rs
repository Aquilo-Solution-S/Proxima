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
//!
//! Forced continuation:
//!
//! ```sh
//! set -a; source ~/.proxima/.env; set +a
//! PROXIMA_LIVE_MISTRAL=1 \
//! PROXIMA_DEMO_REPO=/private/tmp/proxima-forced-continue \
//! cargo test -p proxima-demo-wheel --test demo_wheel_pg forced_intervention_continue_demo_wheel -- --ignored --nocapture --test-threads=1
//!
//! Real planner target:
//!
//! ```sh
//! set -a; source ~/.proxima/.env; set +a
//! PROXIMA_LIVE_MISTRAL=1 \
//! PROXIMA_DEMO_REPO=/private/tmp/proxima-real-planner-signal-match \
//! cargo test -p proxima-demo-wheel --test demo_wheel_pg real_planner_signal_match_target_demo_wheel -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Vision document target:
//!
//! ```sh
//! set -a; source ~/.proxima/.env; set +a
//! PROXIMA_LIVE_MISTRAL=1 \
//! PROXIMA_DEMO_REPO=/private/tmp/proxima-vision-document \
//! cargo test -p proxima-demo-wheel --test demo_wheel_pg goal_to_vision_document_demo_wheel -- --ignored --nocapture --test-threads=1
//! ```

mod demo_wheel;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn measurable_complex_demo_wheel() -> Result<(), Box<dyn std::error::Error>> {
    demo_wheel::run_from_env().await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn forced_intervention_continue_demo_wheel() -> Result<(), Box<dyn std::error::Error>> {
    demo_wheel::run_forced_continue_from_env().await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn real_planner_signal_match_target_demo_wheel() -> Result<(), Box<dyn std::error::Error>> {
    demo_wheel::run_real_planner_signal_match_target_from_env().await
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live Mistral API call; set PROXIMA_LIVE_MISTRAL=1"]
async fn goal_to_vision_document_demo_wheel() -> Result<(), Box<dyn std::error::Error>> {
    demo_wheel::run_goal_to_vision_document_from_env().await
}
