use crate::McpToolError;
use crate::MemoryId;
use crate::mcp::McpToolCtx;
use crate::verbs::goal_write::IdempotencyKey;

/// Resolve the reserved operator label for an authoring write.
///
/// The [`McpTool`](crate::mcp::McpTool) spelling of
/// [`ToolCtx::operator_label`](crate::ToolCtx::operator_label); both call
/// the same resolver, so a tool cannot get a different answer by picking a
/// different trait.
///
/// This is an enforcement point for trusted model provenance, not a
/// restatement of a transport check. `author_from_args` and
/// `author_from_headers` only ever see a *top-level* `model_id`; a nested
/// one (`core_episode_commit`'s per-item `derive.model_id` /
/// `stance[].model_id`) and every argument on the embedded host API reach
/// the tool untouched. Putting the rule here is what makes it hold for any
/// tool that takes a `model_id` argument, including ones not written yet.
///
/// Precedence: a bound `trusted_model_id` wins, a differing `explicit` is
/// refused, a blank one is no claim at all, and an absent one falls back to
/// the request-context label (which the MCP server strips out of the
/// reserved request field into `ctx.author.model_id`).
///
/// The bound identity is read from `ctx.authz`, not from
/// `ctx.author.trusted_model_id`: the author context is a per-call struct a
/// host can build by hand, while the authorization context is the carrier
/// the authenticator hardened.
///
/// The label is trimmed before it is *used*, not just before it is checked:
/// the stored label and any idempotency key derived from it must be the
/// same string, or `" example "` and `"example"` are one label to the
/// validator and two to the dedup key.
///
/// # Errors
///
/// Returns [`McpToolError::InvalidInput`] when `explicit` names a model
/// other than the one the authenticated token binds, or when the resolved
/// label is longer than
/// [`MAX_OPERATOR_LABEL_CHARS`](crate::mcp::MAX_OPERATOR_LABEL_CHARS).
pub fn operator_label(ctx: &McpToolCtx, explicit: Option<&str>) -> Result<String, McpToolError> {
    Ok(crate::tool::resolve_recorded_operator_label(
        ctx.authz.trusted_model_id(),
        Some(ctx.author.model_id.as_str()),
        explicit,
    )?)
}

/// Resolve a caller's handle list to memories in request order with repeats
/// dropped: a handle named twice is one input, and an empty result is not a
/// declaration.
///
/// `resolve` is the surface's own handle resolver — `core_derive` reads the
/// public handle vocabulary, `core_episode_commit` also reads the slots
/// written earlier in the same transaction — and `T` is whatever that
/// resolver classifies each handle as.
///
/// `field`, `max` and `empty_message` carry the calling surface's own error
/// strings. The `max` bound is re-checked here so the helper is safe on its
/// own, but every caller checks it first, against the raw argument and before
/// any space or handle resolution, which is where each surface's error
/// precedence is pinned.
///
/// # Errors
///
/// Returns [`McpToolError::InvalidInput`] when `handles` is over `max` or
/// resolves to nothing, and whatever `resolve` returns for a bad handle.
pub fn dedup_resolved<T>(
    handles: &[String],
    max: usize,
    field: &'static str,
    empty_message: &'static str,
    mut resolve: impl FnMut(&str) -> Result<(MemoryId, T), McpToolError>,
) -> Result<Vec<(MemoryId, T)>, McpToolError> {
    if handles.len() > max {
        return Err(McpToolError::InvalidInput(format!(
            "{field} must contain at most {max} handles"
        )));
    }
    let mut seen = std::collections::HashSet::with_capacity(handles.len());
    let mut resolved = Vec::with_capacity(handles.len());
    for handle in handles {
        let (memory_id, class) = resolve(handle)?;
        if seen.insert(memory_id.into_inner()) {
            resolved.push((memory_id, class));
        }
    }
    if resolved.is_empty() {
        return Err(McpToolError::InvalidInput(empty_message.into()));
    }
    Ok(resolved)
}

/// Normalize an explicitly-provided idempotency key to the shared
/// write-surface contract — trimmed, 1..=180 chars — by parsing it
/// through the same [`IdempotencyKey`] type the goal tools use, so the
/// memory and goal families cannot drift on cap, whitespace handling,
/// or error text. The trimmed key is what feeds dedup (`uuid::new_v5`)
/// and the stored payload. An omitted key (`None`) is always allowed —
/// the caller derives one instead.
///
/// # Errors
///
/// Returns [`McpToolError::InvalidInput`] when `key` is `Some` and blank
/// after trimming or over the character cap.
pub fn normalize_idempotency_key(key: Option<String>) -> Result<Option<String>, McpToolError> {
    key.map(|raw| {
        IdempotencyKey::new(raw)
            .map(IdempotencyKey::into_string)
            .map_err(McpToolError::InvalidInput)
    })
    .transpose()
}

/// Clock-skew tolerance for caller-supplied `observed_at` timestamps.
const OBSERVED_AT_FUTURE_SKEW: time::Duration = time::Duration::minutes(5);

/// Parse an optional caller-supplied `observed_at` backdate (RFC3339).
/// Historical import writes the original observation time into the Fact's
/// receipt provenance (`fact_receipts.observed_at`/`occurred_at`); it does
/// not alter `memories.created_at`, which orders supersession heads and
/// recency and deliberately has no write path.
///
/// # Errors
///
/// Returns [`McpToolError::InvalidInput`] when the value is not RFC3339 or
/// lies in the future beyond a small clock-skew tolerance (an observation
/// cannot postdate its own recording).
pub fn parse_observed_at(raw: Option<&str>) -> Result<Option<time::OffsetDateTime>, McpToolError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let parsed = time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
        .map_err(|err| {
        McpToolError::InvalidInput(format!("observed_at must be an RFC3339 timestamp: {err}"))
    })?;
    if parsed > time::OffsetDateTime::now_utc() + OBSERVED_AT_FUTURE_SKEW {
        return Err(McpToolError::InvalidInput(
            "observed_at must not be in the future".into(),
        ));
    }
    Ok(Some(parsed))
}

/// Upper bound on distinct normalized tags per memory.
const MAX_TAGS: usize = 16;

/// Upper bound on one normalized tag, in characters.
const MAX_TAG_CHARS: usize = 48;

/// Fold one tag to the form it is stored and compared in: trimmed and
/// ASCII-lowercased.
///
/// The write side and the search filter must fold identically or a tag
/// written as `Rust` cannot be found by searching `Rust` — a silent miss,
/// since a filter that matches nothing is indistinguishable from a memory
/// that does not exist. Both call this rather than repeating the two steps.
#[must_use]
pub fn fold_tag(tag: &str) -> String {
    tag.trim().to_ascii_lowercase()
}

/// Trim, lowercase, sort, and dedup `tags`, then cap the *distinct* result
/// at [`MAX_TAGS`]. The cap deliberately applies after normalization: a
/// caller sending `["Rust", "rust", " RUST "]` holds one tag, not three,
/// and must not be rejected for a duplicate-heavy spelling of an
/// in-contract set.
///
/// # Errors
///
/// Returns [`McpToolError::InvalidInput`] when a tag is blank, a tag
/// exceeds [`MAX_TAG_CHARS`], or more than [`MAX_TAGS`] distinct tags
/// remain after normalization. Blank and oversized get different
/// messages — see [`crate::tool::validate_trimmed_len`].
pub fn normalize_tags(tags: Vec<String>) -> Result<Vec<String>, McpToolError> {
    let mut out = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = fold_tag(&tag);
        out.push(crate::tool::validate_trimmed_len("tag", &tag, MAX_TAG_CHARS)?.to_string());
    }
    out.sort();
    out.dedup();
    if out.len() > MAX_TAGS {
        return Err(McpToolError::InvalidInput(
            "at most 16 distinct tags".into(),
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::super::memory_spaces::test_ctx::ctx_for;
    use super::{
        MAX_TAGS, McpToolError, dedup_resolved, normalize_idempotency_key, normalize_tags,
        operator_label, parse_observed_at,
    };
    use crate::mcp::MAX_OPERATOR_LABEL_CHARS;
    use crate::verbs::goal_write::IdempotencyKey;
    use crate::{McpToolCtx, MemoryId, UserId};
    use uuid::Uuid;

    fn test_ctx() -> McpToolCtx {
        ctx_for(
            UserId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000001").expect("uuid")),
            Vec::new(),
        )
    }

    /// An omitted `model_id` falls back to the request context's label
    /// rather than being rejected: the MCP server strips the reserved
    /// request field into `ctx.author.model_id`, and that is the operator.
    #[test]
    fn operator_label_falls_back_to_the_request_context() {
        let ctx = test_ctx();
        assert_eq!(operator_label(&ctx, None).expect("context label"), "test");
        assert_eq!(
            operator_label(&ctx, Some("explicit-model")).expect("explicit label"),
            "explicit-model",
        );
    }

    /// The label is trimmed before it is stored, not just before it is
    /// checked: `" m "` and `"m"` must be one operator on every surface,
    /// or they are one label to the validator and two to the dedup key.
    #[test]
    fn operator_label_is_trimmed_before_it_is_stored() {
        assert_eq!(
            operator_label(&test_ctx(), Some("  spaced  ")).expect("trimmed label"),
            "spaced",
        );
    }

    #[test]
    fn an_oversized_operator_label_is_rejected() {
        let ctx = test_ctx();
        assert!(operator_label(&ctx, Some(&"m".repeat(MAX_OPERATOR_LABEL_CHARS))).is_ok());
        assert!(operator_label(&ctx, Some(&"m".repeat(MAX_OPERATOR_LABEL_CHARS + 1))).is_err());
    }

    /// A blank label is no claim, so the request context's label stands —
    /// the same answer REST gives, where an empty `X-Proxima-Model-Id` is
    /// dropped at the edge and never reaches a tool. Rejecting it here
    /// would make `{"derive": {"model_id": ""}}` fail a request that the
    /// byte-identical REST call accepts.
    #[test]
    fn a_blank_operator_label_is_absent_not_an_error() {
        let ctx = test_ctx();
        for blank in ["", "   "] {
            assert_eq!(
                operator_label(&ctx, Some(blank)).expect("blank is no claim"),
                "test",
                "blank {blank:?}"
            );
        }
    }

    /// A context whose token binds a runner identity, with a deliberately
    /// different `model_id` so a test cannot pass by reading the label slot.
    ///
    /// The binding lives on `authz`, not on the author context: that is
    /// the carrier the authenticator hardened, and the only one
    /// `operator_label` consults.
    fn trusted_ctx() -> McpToolCtx {
        let mut ctx = test_ctx();
        ctx.authz = ctx
            .authz
            .clone()
            .with_trusted_model_id("acme/runner-v3")
            .expect("a well-formed runner id binds");
        ctx.author.trusted_model_id = Some("acme/runner-v3".to_string());
        ctx.author.model_id = "acme/runner-v3".to_string();
        ctx
    }

    /// The enforcement point. Transport edges only ever see a *top-level*
    /// `model_id`; a per-item one (`core_episode_commit`'s
    /// `derive.model_id` / `stance[].model_id`) and every argument on the
    /// embedded host API arrive here untouched, so this is the check that
    /// actually covers them.
    #[test]
    fn an_explicit_label_differing_from_the_bound_identity_is_refused() {
        let err = operator_label(&trusted_ctx(), Some("openai/gpt-9"))
            .expect_err("a caller may not relabel an authenticated runner");

        let McpToolError::InvalidInput(message) = err else {
            panic!("a bad reserved argument is invalid input");
        };
        assert!(message.contains("model_id"), "{message}");
        assert!(message.contains("openai/gpt-9"), "{message}");
        assert!(message.contains("acme/runner-v3"), "{message}");
        assert!(
            message.contains("authenticated token already binds"),
            "{message}"
        );
    }

    #[test]
    fn an_agreeing_or_absent_explicit_label_records_the_bound_identity() {
        let ctx = trusted_ctx();
        for explicit in [None, Some("acme/runner-v3"), Some("  acme/runner-v3  ")] {
            assert_eq!(
                operator_label(&ctx, explicit).expect("agreeing or absent is accepted"),
                "acme/runner-v3",
                "explicit {explicit:?}"
            );
        }
    }

    /// A blank claim is no claim, so it is not a conflict — and with a
    /// binding present the bound identity is still what is recorded.
    #[test]
    fn a_blank_explicit_label_is_not_a_conflict_under_a_binding() {
        assert_eq!(
            operator_label(&trusted_ctx(), Some("   ")).expect("blank is absent, not a conflict"),
            "acme/runner-v3"
        );
    }

    /// The bound identity is recorded even if the author context
    /// disagrees on both of its model fields, so a host that assembles
    /// one by hand cannot launder a label past the binding. The transport
    /// reconciles this at `ctx_for`; the tool does not depend on it having
    /// done so.
    #[test]
    fn the_bound_identity_outranks_a_mismatched_author_context() {
        let mut ctx = trusted_ctx();
        ctx.author.model_id = "stale/label".to_string();
        ctx.author.trusted_model_id = None;

        assert_eq!(
            operator_label(&ctx, None).expect("bound identity wins"),
            "acme/runner-v3"
        );
        assert!(
            operator_label(&ctx, Some("openai/gpt-9")).is_err(),
            "the binding still refuses a differing claim"
        );
    }

    fn memory(byte: u8) -> MemoryId {
        MemoryId::new(Uuid::from_bytes([byte; 16]))
    }

    fn resolve_by_last_char(handle: &str) -> Result<(MemoryId, char), McpToolError> {
        let last = handle
            .chars()
            .next_back()
            .ok_or_else(|| McpToolError::InvalidInput("blank handle".into()))?;
        Ok((memory(last as u8), last))
    }

    /// Request order survives, repeats collapse to their first occurrence,
    /// and the resolver's classification rides along with each memory.
    #[test]
    fn dedup_resolved_keeps_request_order_and_drops_repeats() {
        let handles = vec![
            "mem-b".to_string(),
            "mem-a".to_string(),
            "other-b".to_string(),
            "mem-a".to_string(),
        ];
        assert_eq!(
            dedup_resolved(
                &handles,
                8,
                "source_handles",
                "nonempty",
                resolve_by_last_char
            )
            .expect("resolved"),
            vec![(memory(b'b'), 'b'), (memory(b'a'), 'a')],
        );
    }

    #[test]
    fn dedup_resolved_rejects_an_empty_result_with_the_callers_message() {
        let McpToolError::InvalidInput(message) = dedup_resolved(
            &[],
            8,
            "source_handles",
            "source_handles must be nonempty for operator derivation",
            resolve_by_last_char,
        )
        .expect_err("no declared inputs") else {
            panic!("an empty declaration must be invalid input");
        };
        assert_eq!(
            message,
            "source_handles must be nonempty for operator derivation"
        );
    }

    #[test]
    fn dedup_resolved_rejects_more_handles_than_the_cap() {
        let handles: Vec<String> = (0..3).map(|i| format!("mem-{i}")).collect();
        let McpToolError::InvalidInput(message) =
            dedup_resolved(&handles, 2, "subjects", "nonempty", resolve_by_last_char)
                .expect_err("over the cap")
        else {
            panic!("an over-cap declaration must be invalid input");
        };
        assert_eq!(message, "subjects must contain at most 2 handles");
    }

    /// A handle the surface cannot resolve fails the whole declaration:
    /// silently dropping it would write a derivation over fewer inputs
    /// than the caller declared.
    #[test]
    fn dedup_resolved_propagates_a_resolver_error() {
        let handles = vec!["ok".to_string(), String::new()];
        assert!(dedup_resolved(&handles, 8, "subjects", "nonempty", resolve_by_last_char).is_err());
    }

    #[test]
    fn omitted_observed_at_is_allowed() {
        assert_eq!(parse_observed_at(None).expect("no backdate"), None);
    }

    #[test]
    fn historical_observed_at_parses() {
        let parsed = parse_observed_at(Some("2023-03-22T17:47:00Z"))
            .expect("valid RFC3339")
            .expect("some timestamp");
        assert_eq!(parsed.year(), 2023);
        assert_eq!(parsed.offset(), time::UtcOffset::UTC);
    }

    #[test]
    fn non_rfc3339_observed_at_is_rejected() {
        assert!(parse_observed_at(Some("22.03.2023")).is_err());
        assert!(parse_observed_at(Some("2023-03-22")).is_err());
        assert!(parse_observed_at(Some("")).is_err());
    }

    #[test]
    fn future_observed_at_is_rejected_beyond_clock_skew() {
        let far_future = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
        let raw = far_future
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format");
        assert!(parse_observed_at(Some(&raw)).is_err());
        // Small skew (under the 5-minute tolerance) must pass: two hosts'
        // clocks disagreeing by seconds is not a caller error.
        let near_now = time::OffsetDateTime::now_utc() + time::Duration::seconds(30);
        let raw = near_now
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format");
        assert!(parse_observed_at(Some(&raw)).is_ok());
    }

    const MAX_IDEMPOTENCY_KEY_CHARS: usize = IdempotencyKey::MAX_CHARS;

    #[test]
    fn tags_are_trimmed_lowercased_sorted_and_deduped() {
        let tags = vec![" Rust ".into(), "mcp".into(), "RUST".into()];
        assert_eq!(
            normalize_tags(tags).expect("valid tags"),
            vec!["mcp".to_string(), "rust".to_string()],
        );
    }

    #[test]
    fn blank_and_oversized_tags_are_rejected() {
        assert!(normalize_tags(vec!["  ".into()]).is_err());
        assert!(normalize_tags(vec!["x".repeat(49)]).is_err());
        assert!(normalize_tags(vec!["x".repeat(48)]).is_ok());
    }

    /// A blank tag and an over-length one are different mistakes, and the
    /// old shared `tag must be 1..=48 chars` quoted the blank one a range
    /// it satisfies.
    #[test]
    fn a_blank_tag_and_an_oversized_tag_are_told_apart() {
        let McpToolError::InvalidInput(blank) =
            normalize_tags(vec!["  ".into()]).expect_err("blank tag")
        else {
            panic!("a bad tag must be invalid input");
        };
        let McpToolError::InvalidInput(over) =
            normalize_tags(vec!["x".repeat(49)]).expect_err("oversized tag")
        else {
            panic!("a bad tag must be invalid input");
        };
        assert_ne!(blank, over, "one message for two mistakes tells neither");
        assert!(
            !blank.contains("48"),
            "blank quoted a bound it meets: {blank}"
        );
        assert!(
            over.contains("49"),
            "oversized must say what it sent: {over}"
        );
    }

    #[test]
    fn tag_cap_counts_distinct_tags_not_raw_input() {
        // MAX_TAGS + 1 raw spellings collapsing to one tag are in
        // contract; the cap must not fire on the pre-dedup length.
        let duplicates: Vec<String> = (0..=MAX_TAGS)
            .map(|i| {
                if i % 2 == 0 {
                    "rust".to_string()
                } else {
                    " RUST ".to_string()
                }
            })
            .collect();
        assert_eq!(
            normalize_tags(duplicates).expect("one distinct tag"),
            vec!["rust".to_string()],
        );
    }

    #[test]
    fn too_many_distinct_tags_are_rejected() {
        let at_cap: Vec<String> = (0..MAX_TAGS).map(|i| format!("tag-{i:02}")).collect();
        assert_eq!(normalize_tags(at_cap).expect("at the cap").len(), MAX_TAGS);
        let over_cap: Vec<String> = (0..=MAX_TAGS).map(|i| format!("tag-{i:02}")).collect();
        assert!(normalize_tags(over_cap).is_err());
    }

    #[test]
    fn omitted_idempotency_key_is_allowed() {
        assert_eq!(normalize_idempotency_key(None).expect("no key"), None);
    }

    #[test]
    fn blank_idempotency_key_is_rejected() {
        // An empty or whitespace-only string must not slip through as a
        // real dedup key.
        assert!(normalize_idempotency_key(Some(String::new())).is_err());
        assert!(normalize_idempotency_key(Some("   ".into())).is_err());
    }

    #[test]
    fn idempotency_key_is_trimmed_like_the_goal_family() {
        // `" k "` and `"k"` must be the same dedup key on every write
        // surface; the goal family trims, so the memory family must too.
        assert_eq!(
            normalize_idempotency_key(Some(" k ".into())).expect("valid key"),
            Some("k".to_string()),
        );
    }

    #[test]
    fn idempotency_key_at_the_cap_is_allowed() {
        let key = "k".repeat(MAX_IDEMPOTENCY_KEY_CHARS);
        assert_eq!(
            normalize_idempotency_key(Some(key.clone())).expect("at the cap"),
            Some(key),
        );
    }

    #[test]
    fn idempotency_key_over_the_cap_is_rejected() {
        let key = "k".repeat(MAX_IDEMPOTENCY_KEY_CHARS + 1);
        assert!(normalize_idempotency_key(Some(key)).is_err());
    }

    #[test]
    fn idempotency_key_cap_counts_characters_not_bytes() {
        // Multi-byte chars: MAX of them is fine, MAX+1 is not, even though
        // the byte length is well over the character cap in both cases.
        let at_cap = "é".repeat(MAX_IDEMPOTENCY_KEY_CHARS);
        let over_cap = "é".repeat(MAX_IDEMPOTENCY_KEY_CHARS + 1);
        assert!(normalize_idempotency_key(Some(at_cap)).is_ok());
        assert!(normalize_idempotency_key(Some(over_cap)).is_err());
    }
}
