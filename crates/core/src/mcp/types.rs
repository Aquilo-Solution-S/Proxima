use std::sync::Arc;

use crate::authz::AuthzContext;
use crate::{FlavorServices, MemoryId, Owner, verbs::schema::FlavorRegistryFrozen};

/// Upper bound on the reserved `model_id` operator label, in characters.
///
/// Shared by every surface that can name an operator: the tool-argument
/// label, the REST header, and the `trusted_model_id` an authenticator
/// binds. One bound, one constant — a token-bound id that a tool would
/// then refuse as over-long is a deployment that authenticates and cannot
/// write.
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
    /// When present, `model_id` equals it.
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

    /// The label the caller sent.
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

/// Resolve the operator label that reaches append-only provenance, for one
/// call on any transport.
///
/// Precedence: the authenticated `trusted` id, else the caller-supplied
/// label, else [`UNKNOWN_OPERATOR_LABEL`]. A caller-supplied label that
/// differs from a bound one (after trimming) is a conflict, not an
/// override — the token, not the payload, decides which model this is.
///
/// Both transports call this so their precedence cannot drift.
///
/// # Errors
///
/// Returns [`OperatorLabelConflict`] when `trusted` is present and
/// `supplied` names a different model.
pub fn resolve_operator_label(
    trusted: Option<&str>,
    supplied: Option<&str>,
) -> Result<String, OperatorLabelConflict> {
    match (trusted, supplied) {
        (Some(trusted), Some(supplied)) if supplied.trim() != trusted.trim() => {
            Err(OperatorLabelConflict {
                trusted: trusted.to_string(),
                supplied: supplied.to_string(),
            })
        }
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
}
