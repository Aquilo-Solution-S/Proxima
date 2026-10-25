//! Opt-in local stage durations. Enable `proxima::stage_timing=debug` on the
//! host's existing tracing subscriber; no request content is recorded.

use std::future::Future;

use tokio::time::Instant;

/// Timing owns the awaited future's lifetime, including cancellation by an
/// outer deadline. A disabled target does not read the clock.
pub(crate) async fn timed<T>(stage: &'static str, future: impl Future<Output = T>) -> T {
    let mut timing = Timing::start(stage);
    let result = future.await;
    timing.completed = true;
    result
}

struct Timing {
    stage: &'static str,
    active: Option<(Instant, tracing::Span)>,
    completed: bool,
}

impl Timing {
    fn start(stage: &'static str) -> Self {
        let active = tracing::enabled!(target: "proxima::stage_timing", tracing::Level::DEBUG)
            .then(|| {
                let span = tracing::Span::current();
                tracing::debug!(target: "proxima::stage_timing", parent: &span,
                    stage, outcome = "started", "stage timing");
                (Instant::now(), span)
            });
        Self {
            stage,
            active,
            completed: false,
        }
    }
}

impl Drop for Timing {
    fn drop(&mut self) {
        if let Some((started, span)) = &self.active {
            tracing::debug!(target: "proxima::stage_timing", parent: span,
                stage = self.stage,
                outcome = if self.completed { "finished" } else { "cancelled" },
                elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                "stage timing");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tracing::field::{Field, Visit};
    use tracing::instrument::WithSubscriber as _;
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    use super::*;

    #[derive(Debug, Default, PartialEq, Eq)]
    struct Fields {
        stage: String,
        outcome: String,
        elapsed_us: Option<u64>,
    }

    impl Visit for Fields {
        fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
        fn record_str(&mut self, field: &Field, value: &str) {
            match field.name() {
                "stage" => self.stage = value.to_owned(),
                "outcome" => self.outcome = value.to_owned(),
                _ => {}
            }
        }
        fn record_u64(&mut self, field: &Field, value: u64) {
            if field.name() == "elapsed_us" {
                self.elapsed_us = Some(value);
            }
        }
    }

    #[derive(Debug, Clone, Default)]
    struct Capture(Arc<Mutex<Vec<Fields>>>);

    impl Subscriber for Capture {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.target() == "proxima::stage_timing"
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut fields = Fields::default();
            event.record(&mut fields);
            self.0.lock().expect("captured events").push(fields);
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    #[tokio::test(start_paused = true)]
    async fn completion_and_cancellation_record_local_elapsed_time() {
        let capture = Capture::default();
        async {
            let result = timed("complete", async {
                tokio::time::advance(std::time::Duration::from_millis(3)).await;
                7
            })
            .await;
            assert_eq!(result, 7, "timing preserves the future's value");
            let mut pending = Box::pin(timed("cancel", std::future::pending::<()>()));
            std::future::poll_fn(|cx| {
                assert!(pending.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            tokio::time::advance(std::time::Duration::from_secs(2)).await;
            drop(pending);
        }
        .with_subscriber(capture.clone())
        .await;
        let fields = capture.0.lock().expect("captured events");
        assert_eq!(
            *fields,
            vec![
                Fields {
                    stage: "complete".into(),
                    outcome: "started".into(),
                    elapsed_us: None
                },
                Fields {
                    stage: "complete".into(),
                    outcome: "finished".into(),
                    elapsed_us: Some(3_000)
                },
                Fields {
                    stage: "cancel".into(),
                    outcome: "started".into(),
                    elapsed_us: None
                },
                Fields {
                    stage: "cancel".into(),
                    outcome: "cancelled".into(),
                    elapsed_us: Some(2_000_000)
                },
            ]
        );
    }

    #[tokio::test]
    async fn disabled_timing_preserves_the_future() {
        let value = timed("disabled", async { 42 })
            .with_subscriber(tracing::subscriber::NoSubscriber::default())
            .await;
        assert_eq!(value, 42);
    }
}
