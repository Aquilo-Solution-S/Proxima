//! Embedding-client contracts used by vector retrieval.

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

/// Trivial liveness probe after a provider refuses a real input.
/// Short enough that no cap, context window, or token limit can refuse it.
pub(crate) const EMBED_LIVENESS_PROBE: &str = "ok";

/// Whether an embedding failure is the input's fault.
///
/// [`LlmError::EmbedPermanent`]: the provider refused this input. Every
/// other variant is ambiguous (a runner that dies on the input looks like
/// a runner that was already down). Probe [`EMBED_LIVENESS_PROBE`]: if it
/// succeeds, blame the input; if it fails, the provider is down.
/// Same question [`crate::Engine::drain_embedding_jobs`] asks of a batch.
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

/// Lowest [`crate::models::EmbedCaps::max_input_chars`] [`embed_in_chunks`]
/// can still satisfy. Coupled to the cap: over-long input is
/// [`LlmError::EmbedPermanent`], which triggers bisection — a cap below
/// this floor turns a rescuable input terminal.
///
/// Floor = largest piece the split can emit. A segment is cut only when
/// each half is still ≥ [`CHUNKED_EMBED_MIN_BYTES`], so a piece is at most
/// `2 * CHUNKED_EMBED_MIN_BYTES - 1` bytes. Character count ≤ byte count.
/// The `- 1` is load-bearing (test: halves land exactly on the boundary).
pub const MIN_EMBED_INPUT_CAP_CHARS: usize = 2 * CHUNKED_EMBED_MIN_BYTES - 1;

/// Bisect an over-limit input into pieces the provider accepts.
///
/// Rejection does not say *why*: over-limit starts embedding once pieces
/// are short enough; any other reason fails all the way down (`Ok(None)`).
/// Halves below [`CHUNKED_EMBED_MIN_BYTES`] abort — partial coverage would
/// mask poison input.
///
/// Lives here, not on `Engine`: both the in-process drain and
/// `maintain-embeddings --drain` must rescue the same way.
///
/// `Ok(Some(vectors))` in text order, `Ok(None)` if every length is
/// refused, `Err` on the first transient provider error.
///
/// # Errors
///
/// First non-`EmbedPermanent` provider error.
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

    /// Floor is a real bound: rescued at the floor, terminal one char below.
    /// Input length must be `2 * MIN_EMBED_INPUT_CAP_CHARS` so halves land
    /// on the widest piece the split can emit.
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

    /// Smallest cap that works for every length: the test above builds its
    /// input from the constant, so it would pass a range of wrong values.
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
