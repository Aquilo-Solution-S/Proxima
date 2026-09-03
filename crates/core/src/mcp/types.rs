use std::sync::Arc;

use crate::authz::AuthzContext;
use crate::{FlavorServices, MemoryId, Owner, verbs::schema::FlavorRegistryFrozen};

/// Upper bound on the reserved `model_id` operator label, in characters.
///
/// Enforced in two places, both of which must agree or a deployment can
/// authenticate and then fail every write:
/// [`AuthzContext::with_trusted_model_id`](crate::AuthzContext::with_trusted_model_id)
/// (and the OIDC subject map that feeds it) bounds what an authenticator
/// may bind, and the operator-label resolvers
/// ([`ToolCtx::operator_label`](crate::ToolCtx::operator_label) and
/// `operator_label` for [`McpTool`](crate::mcp::McpTool)) bound the label a
/// tool records through them.
///
/// Two things it does not cover, deliberately. Transport edges only decide
/// precedence — a nested or per-item `model_id` never reaches them — and a
/// label that feeds a hash rather than a stored column (Goal
/// system-operator identity, which no replay reads back) is not routed
/// through a resolver, because bounding it would turn a caller's long
/// label into a failed write with nothing gained.
pub const MAX_OPERATOR_LABEL_CHARS: usize = 120;

/// Recorded operator label when neither the authenticated edge nor the
/// caller names one.
pub const UNKNOWN_OPERATOR_LABEL: &str = "unknown";

/// Operator provenance for one call.
///
/// `model_id` is the label that reaches append-only provenance.
/// `trusted_model_id` is the authenticated one, present only when the
/// edge authenticator bound a model identity to the principal; it is
/// never populated from tool arguments, request headers, MCP
/// `clientInfo`, or any `_proxima_*` reserved argument.
#[derive(Debug, Clone)]
pub struct McpAuthorContext {
    pub model_id: String,
    /// Model identity certified by the authenticated token, if any.
    ///
    /// When present, `model_id` equals it. That is an invariant of this
    /// struct, not of whoever built it: both fields are re-derived from
    /// the authorization context in `McpToolHost::ctx_for`, because a host
    /// assembles this by hand and `model_id` is what a tool records.
    pub trusted_model_id: Option<String>,
    pub client_name: String,
    pub client_version: String,
    pub caller_self_perspective: Option<MemoryId>,
}

/// The caller named a model and the authenticated token names a different
/// one. Refused rather than silently overridden: the caller believes it is
/// labelling an append-only write, and a surface that quietly relabels it
/// teaches the caller nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorLabelConflict {
    trusted: String,
    supplied: String,
}

impl OperatorLabelConflict {
    /// The model identity the authenticated token binds.
    #[must_use]
    pub fn trusted(&self) -> &str {
        &self.trusted
    }

    /// The label the caller sent, bounded for echoing back.
    ///
    /// Truncated to [`MAX_OPERATOR_LABEL_CHARS`] with a `…` marker at
    /// construction: the conflict is decided *before* the length check, so
    /// the value reaching here is whatever the caller sent, and a refusal
    /// that quoted it whole would let a request choose how much of itself
    /// to write into every error log that records the refusal.
    #[must_use]
    pub fn supplied(&self) -> &str {
        &self.supplied
    }

    /// Refusal text naming the transport's own field (`model_id` on MCP,
    /// `X-Proxima-Model-Id` on REST).
    #[must_use]
    pub fn detail(&self, field: &str) -> String {
        format!(
            "{field} is {supplied:?}, but the authenticated token already binds \
             the model identity {trusted:?}; omit {field} or send the bound value",
            supplied = self.supplied,
            trusted = self.trusted,
        )
    }
}

/// Bound a caller-supplied value before it is quoted back in a refusal.
///
/// The conflict is decided before the length check — it has to be, or a
/// caller could relabel a bound write by padding the claim — so this is the
/// only thing standing between an unbounded argument and the error text.
fn bounded_for_echo(supplied: &str) -> String {
    if supplied.chars().count() <= MAX_OPERATOR_LABEL_CHARS {
        return supplied.to_string();
    }
    let kept: String = supplied.chars().take(MAX_OPERATOR_LABEL_CHARS).collect();
    format!("{kept}…")
}

/// Resolve the operator label that reaches append-only provenance, for one
/// call on any surface.
///
/// Precedence: the authenticated `trusted` id, else the caller-supplied
/// label, else [`UNKNOWN_OPERATOR_LABEL`]. A caller-supplied label that
/// differs from a bound one is a conflict, not an override — the token,
/// not the payload, decides which model this is.
///
/// Both values are trimmed first, and a `supplied` that is blank after
/// trimming is treated as absent. That is not cosmetic: REST drops an empty
/// header before it can reach here, so without the same rule an empty
/// `model_id` argument on MCP would mean something the identical REST
/// request does not.
///
/// Every surface that can name an operator calls this — both transport
/// edges, the embedded host API, and `operator_label` inside the tools
/// themselves — so their precedence cannot drift.
///
/// # Errors
///
/// Returns [`OperatorLabelConflict`] when `trusted` is present and
/// `supplied` names a different model.
pub fn resolve_operator_label(
    trusted: Option<&str>,
    supplied: Option<&str>,
) -> Result<String, OperatorLabelConflict> {
    let supplied = supplied.map(str::trim).filter(|value| !value.is_empty());
    match (trusted.map(str::trim), supplied) {
        (Some(trusted), Some(supplied)) if supplied != trusted => Err(OperatorLabelConflict {
            trusted: trusted.to_string(),
            supplied: bounded_for_echo(supplied),
        }),
        (Some(trusted), _) => Ok(trusted.to_string()),
        (None, Some(supplied)) => Ok(supplied.to_string()),
        (None, None) => Ok(UNKNOWN_OPERATOR_LABEL.to_string()),
    }
}

#[derive(Clone)]
pub struct McpToolCtx {
    pub owner: Owner,
    /// Caller's authorization context, threaded from the transport
    /// edge. Tools pass this to engine verbs — never a substituted
    /// engine identity (privilege-escalation guard).
    pub authz: AuthzContext,
    pub registry: Arc<FlavorRegistryFrozen>,
    pub author: McpAuthorContext,
    pub caller_self_perspective: Option<MemoryId>,
    /// Backend/flavor services supplied by the host. Core does not name
    /// concrete service types; PG-aware flavors may downcast their own
    /// dependencies here.
    pub services: FlavorServices,
    /// `Some` when the MCP server was constructed with `with_engine`.
    /// Tools that need to call engine verbs (CRUD-via-MCP) require this;
    /// pure read-only / projection tools can ignore it.
    pub engine: Option<Arc<crate::Engine>>,
}

impl std::fmt::Debug for McpToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpToolCtx")
            .field("owner", &self.owner)
            .field("author", &self.author)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_OPERATOR_LABEL_CHARS, UNKNOWN_OPERATOR_LABEL, resolve_operator_label};

    #[test]
    fn the_trusted_id_wins_over_a_caller_supplied_label() {
        assert_eq!(
            resolve_operator_label(Some("runner/pinned"), None).expect("no conflict"),
            "runner/pinned"
        );
        assert_eq!(
            resolve_operator_label(Some("runner/pinned"), Some("runner/pinned")).expect("equal"),
            "runner/pinned"
        );
    }

    #[test]
    fn an_equal_label_is_compared_after_trimming() {
        assert_eq!(
            resolve_operator_label(Some("runner/pinned"), Some("  runner/pinned  "))
                .expect("trim-equal is not a conflict"),
            "runner/pinned",
            "the bound value is recorded, not the caller's spacing"
        );
    }

    #[test]
    fn a_differing_label_is_refused_and_names_both_sides() {
        let conflict = resolve_operator_label(Some("runner/pinned"), Some("claimed/model"))
            .expect_err("a different label must not silently lose");
        assert_eq!(conflict.trusted(), "runner/pinned");
        assert_eq!(conflict.supplied(), "claimed/model");

        let detail = conflict.detail("model_id");
        assert!(detail.contains("model_id"), "{detail}");
        assert!(detail.contains("claimed/model"), "{detail}");
        assert!(detail.contains("runner/pinned"), "{detail}");
        assert!(
            detail.contains("authenticated token already binds"),
            "{detail}"
        );
    }

    /// A blank claim is no claim, on either transport. REST already drops
    /// an empty header upstream; if MCP did not drop an empty argument here,
    /// two identical requests would mean different things.
    #[test]
    fn a_blank_supplied_label_is_absent_not_a_conflict() {
        for blank in ["", "   ", "\t"] {
            assert_eq!(
                resolve_operator_label(Some("runner/pinned"), Some(blank))
                    .expect("a blank claim cannot conflict"),
                "runner/pinned"
            );
            assert_eq!(
                resolve_operator_label(None, Some(blank)).expect("no conflict"),
                UNKNOWN_OPERATOR_LABEL,
                "blank {blank:?} is absent, not an empty label"
            );
        }
    }

    /// Whatever is recorded is the trimmed form: the stored label and any
    /// idempotency key derived from it must be the same string.
    #[test]
    fn both_sides_are_returned_trimmed() {
        assert_eq!(
            resolve_operator_label(Some("  runner/pinned  "), None).expect("no conflict"),
            "runner/pinned"
        );
        assert_eq!(
            resolve_operator_label(None, Some("  caller/model  ")).expect("no conflict"),
            "caller/model"
        );
    }

    #[test]
    fn without_a_trusted_id_the_caller_label_stands_and_absence_is_unknown() {
        assert_eq!(
            resolve_operator_label(None, Some("caller/model")).expect("no conflict"),
            "caller/model"
        );
        assert_eq!(
            resolve_operator_label(None, None).expect("no conflict"),
            UNKNOWN_OPERATOR_LABEL
        );
    }

    /// The bound is one constant precisely so an authenticator cannot bind an
    /// id that a tool would then refuse as over-long.
    #[test]
    fn the_operator_label_bound_is_the_shared_constant() {
        assert_eq!(MAX_OPERATOR_LABEL_CHARS, 120);
    }

    /// The conflict is decided before any length check — it has to be, or
    /// padding a claim would evade the binding — so the refusal is the one
    /// place an unbounded argument could reach a log. It is quoted back
    /// bounded, and still says enough for the caller to recognise what it
    /// sent.
    #[test]
    fn an_over_long_claim_is_bounded_before_it_is_quoted_back() {
        let claim = "x".repeat(200);
        let conflict = resolve_operator_label(Some("runner/pinned"), Some(&claim))
            .expect_err("a differing claim conflicts however long it is");

        assert_eq!(
            conflict.supplied().chars().count(),
            MAX_OPERATOR_LABEL_CHARS + 1,
            "the bound plus the marker, not the caller's 200"
        );
        assert!(conflict.supplied().ends_with('…'), "truncation is visible");
        assert!(conflict.supplied().starts_with("xxx"));
        assert!(
            conflict.detail("model_id").chars().count() < claim.chars().count() + 120,
            "the message cannot grow without bound with the claim"
        );
    }

    /// A claim exactly at the bound is quoted whole: the truncation is
    /// there to bound the echo, not to mangle every refusal.
    #[test]
    fn a_claim_within_the_bound_is_quoted_whole() {
        let claim = "x".repeat(MAX_OPERATOR_LABEL_CHARS);
        let conflict = resolve_operator_label(Some("runner/pinned"), Some(&claim))
            .expect_err("still a conflict");

        assert_eq!(conflict.supplied(), claim);
    }
}
