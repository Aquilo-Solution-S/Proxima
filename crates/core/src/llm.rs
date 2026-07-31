//! Model-client contracts used by substrate provenance.

use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("LLM call failed: {0}")]
    Llm(String),
    #[error("embedding call failed: {0}")]
    Embed(String),
    /// Embedding rejected for a cause retries cannot fix (e.g. input over
    /// the model's token limit — HTTP 400/413/422). Jobs hitting this must
    /// fail terminally instead of burning retry attempts forever.
    #[error("embedding permanently rejected: {0}")]
    EmbedPermanent(String),
    #[error("output validation failed: {0}")]
    OutputValidation(String),
    #[error("internal: {0}")]
    Internal(String),
}

pub trait AnthropicClient: Send + Sync + std::fmt::Debug {
    fn model_id(&self) -> &str;
}

pub const EMBEDDING_DIM: usize = 1024;
pub const EMBEDDING_JOB_MAX_ATTEMPTS: i32 = 5;
/// How many texts the drain worker embeds per provider call. Providers'
/// `/embeddings` endpoints accept arrays; batching divides request count
/// (and therefore request-rate-limit pressure) by this factor.
pub const EMBEDDING_BATCH_SIZE: usize = 32;

/// Embedding client surface. Concrete impls live outside core.
#[async_trait]
pub trait EmbeddingClient: Send + Sync + std::fmt::Debug {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError>;

    /// Embed several texts in one provider call, one vector per input, in
    /// input order. The default falls back to sequential single embeds so
    /// existing impls stay correct; batching impls should override.
    async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        let mut out = Vec::with_capacity(texts.len());
        for text in texts {
            out.push(self.embed(text).await?);
        }
        Ok(out)
    }

    fn model_id(&self) -> &str;
    fn dim(&self) -> usize;
}

/// The trivial input used to ask a provider whether it is up, right after
/// it refused a real one.
///
/// Short enough that no cap, context window, or token limit can be what
/// refuses it, so the only thing its answer reports is liveness.
pub(crate) const EMBED_LIVENESS_PROBE: &str = "ok";

/// Whether an embedding failure is the *input's* fault rather than the
/// provider's — the question that decides whether retrying this input
/// unchanged could ever work.
///
/// [`LlmError::EmbedPermanent`] already answers it: the provider looked at
/// the input and refused it. Every other variant is ambiguous, and
/// unavoidably so. A provider that *dies on* an input and a provider that
/// was already down produce the same response — observed against a local
/// runner as `400 {"error": "… EOF"}` — so no per-request inspection can
/// separate them.
///
/// One extra tiny call can. If the provider answers
/// [`EMBED_LIVENESS_PROBE`] immediately after refusing the real input, it is
/// up, and the refusal is attributable to what was sent. If the probe fails
/// too, the provider really is down and nothing about the input has been
/// learned.
///
/// This is the same question [`crate::Engine::drain_embedding_jobs`] asks of
/// a failed batch, asked here of a single text, so a derived write and the
/// drain cannot disagree about whose fault a failure was.
pub(crate) async fn embed_failure_blames_the_input(
    client: &dyn EmbeddingClient,
    err: &LlmError,
) -> bool {
    if matches!(err, LlmError::EmbedPermanent(_)) {
        return true;
    }
    client.embed(EMBED_LIVENESS_PROBE).await.is_ok()
}

/// Smallest piece, in bytes, the chunked-embedding rescue is willing to
/// produce. A segment whose halves would fall below this is not split
/// further: at that size a rejection is read as genuinely invalid input
/// rather than an over-limit one, so the effective floor on a segment the
/// rescue will still bisect is twice this value.
pub const CHUNKED_EMBED_MIN_BYTES: usize = 2048;

/// The lowest [`crate::models::EmbedCaps::max_input_chars`] that
/// [`embed_in_chunks`] can still satisfy.
///
/// The two constants are coupled, and silently: a client-side cap refuses
/// over-long input as [`LlmError::EmbedPermanent`], which is exactly the
/// error that sends the input here to be bisected — so a cap the bisection
/// can never produce a small enough piece for turns a *rescuable* input into
/// a terminal one, with nothing in either component looking wrong.
///
/// The floor is the **largest piece the split can emit**, which follows from
/// how it terminates. A segment is cut only when each half would still be at
/// least [`CHUNKED_EMBED_MIN_BYTES`], so a segment of twice that is the
/// smallest one still divisible and every emitted piece is at most one byte
/// under it. A piece's character count never exceeds its byte count, so a cap
/// at or above this value accepts every piece; one character below it, an
/// input that bisects into pieces of exactly this size is refused at every
/// length and goes terminal — the outcome the rescue exists to prevent.
///
/// The off-by-one is load-bearing rather than cautious, and is pinned by
/// test: the interesting input is not the longest one but the one whose
/// halves land exactly on the boundary.
pub const MIN_EMBED_INPUT_CAP_CHARS: usize = 2 * CHUNKED_EMBED_MIN_BYTES - 1;

/// Split an input the provider rejected as over-limit into pieces it will
/// accept, so the memory lands as one chunked embedding version rather than
/// going terminally un-embedded.
///
/// The provider's rejection does not say *why* the input is invalid, and
/// this never needs to know: an over-limit input starts embedding once its
/// pieces are short enough, while an input rejected for any other reason
/// keeps failing all the way down and the caller sends the job terminal
/// exactly as before. A piece whose halves would fall below
/// [`CHUNKED_EMBED_MIN_BYTES`] is treated as genuinely invalid and aborts
/// the rescue — partial coverage would mask the poison input.
///
/// This lives here, rather than on `Engine`, because there are two drains:
/// the in-process engine worker and `maintain-embeddings --drain`. When only
/// the engine could rescue, whichever drain reached a job first decided
/// whether an over-limit memory was recoverable or permanently dead.
///
/// Returns `Ok(Some(vectors))` (in text order) on rescue, `Ok(None)` when
/// some piece is rejected at every length, and `Err` on the first transient
/// provider error so the caller records an ordinary retryable attempt.
///
/// # Errors
///
/// Returns the first non-`EmbedPermanent` provider error encountered.
pub async fn embed_in_chunks(
    client: &dyn EmbeddingClient,
    text: &str,
) -> Result<Option<Vec<Vec<f32>>>, LlmError> {
    // Depth-first, left-to-right bisection keeps chunk vectors in text
    // order without recursion (async fns don't recurse).
    let mut pending: Vec<&str> = vec![text];
    let mut vectors: Vec<Vec<f32>> = Vec::new();
    while let Some(segment) = pending.pop() {
        match client.embed(segment).await {
            Ok(vector) => vectors.push(vector),
            Err(LlmError::EmbedPermanent(_)) => {
                let mut cut = segment.len() / 2;
                while cut > 0 && !segment.is_char_boundary(cut) {
                    cut -= 1;
                }
                if cut < CHUNKED_EMBED_MIN_BYTES {
                    return Ok(None);
                }
                // Pop order: push right half first so the left half embeds
                // (or splits) next.
                pending.push(&segment[cut..]);
                pending.push(&segment[..cut]);
            }
            Err(err) => return Err(err),
        }
    }
    Ok(Some(vectors))
}

#[cfg(test)]
mod tests {
    use super::{
        CHUNKED_EMBED_MIN_BYTES, EmbeddingClient, LlmError, MIN_EMBED_INPUT_CAP_CHARS,
        embed_in_chunks,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// A provider that refuses over-cap input the way a client-side
    /// `max_input_chars` does — and records every length it was offered, so a
    /// test can assert what was *sent*, not merely what came back.
    #[derive(Debug)]
    struct CappedEmbedding {
        max_chars: usize,
        offered: Mutex<Vec<usize>>,
    }

    impl CappedEmbedding {
        fn new(max_chars: usize) -> Self {
            Self {
                max_chars,
                offered: Mutex::new(Vec::new()),
            }
        }

        fn accepted(&self) -> Vec<usize> {
            let offered = self
                .offered
                .lock()
                .expect("no test holds this across a panic");
            offered
                .iter()
                .copied()
                .filter(|chars| *chars <= self.max_chars)
                .collect()
        }
    }

    #[async_trait]
    impl EmbeddingClient for CappedEmbedding {
        async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
            let chars = text.chars().count();
            self.offered
                .lock()
                .expect("no test holds this across a panic")
                .push(chars);
            if chars > self.max_chars {
                return Err(LlmError::EmbedPermanent(format!(
                    "input of {chars} chars exceeds the {}-char limit",
                    self.max_chars
                )));
            }
            Ok(vec![0.0; 4])
        }

        fn model_id(&self) -> &'static str {
            "capped"
        }

        fn dim(&self) -> usize {
            4
        }
    }

    /// The cap and the rescue have to compose, because the cap's refusal is
    /// what triggers the rescue. Everything over the limit comes back split,
    /// and every piece actually embedded is within it.
    #[tokio::test]
    async fn a_client_side_cap_is_rescued_into_chunks() {
        let client = CappedEmbedding::new(MIN_EMBED_INPUT_CAP_CHARS);
        let text = "a".repeat(MIN_EMBED_INPUT_CAP_CHARS * 5);

        let vectors = embed_in_chunks(&client, &text)
            .await
            .expect("a cap refusal is not a transient error")
            .expect("an over-cap input is splittable, not invalid");

        assert!(vectors.len() > 1, "an over-cap input must come back split");
        assert_eq!(
            vectors.len(),
            client.accepted().len(),
            "one vector per piece the provider accepted",
        );
        for chars in client.accepted() {
            assert!(
                chars <= MIN_EMBED_INPUT_CAP_CHARS,
                "a piece of {chars} chars was embedded above the cap",
            );
        }
    }

    /// The floor is a real bound, not a cautious one — tested at the two
    /// values that decide it: at the floor the input is rescued, one
    /// character below it the same input goes terminal.
    ///
    /// The input matters as much as the cap. A first draft of this test used
    /// a comfortable multiple of the floor and passed at *both* caps, because
    /// halving a power-of-two length lands every piece exactly on
    /// [`CHUNKED_EMBED_MIN_BYTES`] — well under either. The failure needs the
    /// length whose halves are the largest pieces the split can emit, which
    /// is the same length the constant is derived from. A sweep of long
    /// inputs would have reported this bound as unnecessary.
    ///
    /// Without the floor, a cap set too low reads as a working
    /// configuration: every component behaves exactly as documented and the
    /// memory is simply never embedded.
    #[tokio::test]
    async fn one_char_under_the_floor_is_the_difference_between_split_and_terminal() {
        // Halves to exactly MIN_EMBED_INPUT_CAP_CHARS, the widest piece the
        // bisection can hand the provider.
        let text = "a".repeat(2 * MIN_EMBED_INPUT_CAP_CHARS);

        let at_floor = CappedEmbedding::new(MIN_EMBED_INPUT_CAP_CHARS);
        assert_eq!(
            embed_in_chunks(&at_floor, &text)
                .await
                .expect("not a transient error")
                .map(|vectors| vectors.len()),
            Some(2),
            "at the floor the widest piece is acceptable, so the rescue finishes",
        );

        let under_floor = CappedEmbedding::new(MIN_EMBED_INPUT_CAP_CHARS - 1);
        assert!(
            embed_in_chunks(&under_floor, &text)
                .await
                .expect("not a transient error")
                .is_none(),
            "one char lower, that piece is refused and is too small to split \
             again, so a rescuable input goes terminal — which is why a cap \
             under MIN_EMBED_INPUT_CAP_CHARS is refused at construction",
        );
    }

    /// The floor is derived, so it must move if what it is derived from
    /// moves.
    #[test]
    fn the_floor_tracks_the_split_minimum_it_is_derived_from() {
        assert_eq!(MIN_EMBED_INPUT_CAP_CHARS, 2 * CHUNKED_EMBED_MIN_BYTES - 1);
    }

    /// The floor must be the *smallest* cap that works for every input, and
    /// the test above cannot show that: it builds its input out of the
    /// constant, so it passes for a range of wrong values. This checks the
    /// property the constant claims instead — the cap is sufficient for all
    /// lengths, and one character less is not sufficient for some length.
    ///
    /// Sweeping lengths rather than asserting arithmetic is the point. The
    /// bound is a consequence of where a recursive split happens to stop,
    /// which is exactly the kind of reasoning that is easier to get wrong on
    /// paper than to observe.
    #[tokio::test]
    async fn the_floor_is_the_smallest_cap_that_works_for_every_input() {
        async fn rescues_every_length(cap: usize) -> bool {
            // Every length whose halves can straddle the split minimum.
            for len in CHUNKED_EMBED_MIN_BYTES..=(4 * CHUNKED_EMBED_MIN_BYTES) {
                let client = CappedEmbedding::new(cap);
                let rescued = embed_in_chunks(&client, &"a".repeat(len))
                    .await
                    .expect("not a transient error");
                if rescued.is_none() {
                    return false;
                }
            }
            true
        }

        assert!(
            rescues_every_length(MIN_EMBED_INPUT_CAP_CHARS).await,
            "the floor must leave no input un-rescuable",
        );
        assert!(
            !rescues_every_length(MIN_EMBED_INPUT_CAP_CHARS - 1).await,
            "one char below the floor some input must fail, or the floor is \
             higher than it needs to be and refuses workable configurations",
        );
    }

    /// A cap is not the only reason a provider says `EmbedPermanent`. Input
    /// that is invalid rather than long is refused all the way down, and the
    /// caller must see that as "no rescue" so the job goes terminal instead
    /// of cycling — the behaviour the cap must not change.
    #[tokio::test]
    async fn input_refused_at_every_length_is_not_rescued() {
        let client = CappedEmbedding::new(0);
        let outcome = embed_in_chunks(&client, &"a".repeat(MIN_EMBED_INPUT_CAP_CHARS * 4))
            .await
            .expect("not a transient error");
        assert!(outcome.is_none(), "nothing was acceptable at any length");
    }
}
