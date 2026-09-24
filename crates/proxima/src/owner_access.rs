//! The one owner-access port a served runtime resolves roles through.
//!
//! The MCP edge (per-Group resolution behind `X-Proxima-Owner`), the
//! delegation service, and an environment-built OIDC authenticator all ask
//! the same [`OwnerAccessPort`]: the host's own
//! ([`crate::RuntimeBuilder::owner_access`]) or the runtime-pool Postgres
//! resolver, optionally under a [`ForwarderPolicy`].

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use proxima_core::{AccessError, GroupId, OwnerAccessPort, OwnerRoles, Role, UserId};

use crate::ProximaError;

const FORWARDER_SUBJECTS: &str = "PROXIMA_FORWARDER_SUBJECTS";
const FORWARDER_ROLE: &str = "PROXIMA_FORWARDER_ROLE";

/// Trusted forwarder subjects and the fixed role each holds in whichever
/// Group it selects per request (`X-Proxima-Owner: group:<uuid>`).
///
/// For a host that serves many parties through one authenticated subject: a
/// pack host forwards each party's call under its own token and names the
/// party's Group, and the Group need not list the forwarder as a member. The
/// policy answers only the per-Group probe the edge makes for a Group the
/// subject's membership map lacks — real memberships still win, a Personal
/// owner is never reachable through it, and the delegation service, which
/// reads memberships, is unaffected: a forwarder cannot delegate a role it
/// only holds by policy.
///
/// The role may not manage Groups: a forwarder that could administer any
/// Group it names could make itself, or anyone, a member of every one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwarderPolicy {
    subjects: BTreeSet<UserId>,
    role: Role,
}

impl ForwarderPolicy {
    /// # Errors
    ///
    /// [`ProximaError::Config`] when `subjects` is empty or `role` manages
    /// Groups.
    pub fn new(
        subjects: impl IntoIterator<Item = UserId>,
        role: Role,
    ) -> Result<Self, ProximaError> {
        let subjects: BTreeSet<UserId> = subjects.into_iter().collect();
        if subjects.is_empty() {
            return Err(ProximaError::Config(
                "a forwarder policy needs at least one subject".into(),
            ));
        }
        if role.manages() {
            return Err(ProximaError::Config(
                "a forwarder role may not manage groups; use viewer, ingest, or editor".into(),
            ));
        }
        Ok(Self { subjects, role })
    }

    #[must_use]
    pub fn is_forwarder(&self, subject: UserId) -> bool {
        self.subjects.contains(&subject)
    }

    #[must_use]
    pub const fn role(&self) -> Role {
        self.role
    }

    /// `inner` with this policy answering the per-Group probe for its
    /// subjects.
    #[must_use]
    pub fn wrap(self, inner: Arc<dyn OwnerAccessPort>) -> Arc<dyn OwnerAccessPort> {
        Arc::new(ForwarderOwnerAccess {
            policy: self,
            inner,
        })
    }
}

/// `PROXIMA_FORWARDER_SUBJECTS` (comma-separated user UUIDs, as in the OIDC
/// subject map's `user_id`) and `PROXIMA_FORWARDER_ROLE`
/// (`viewer` | `ingest` | `editor`). Both or neither.
pub(crate) fn forwarder_from_lookup(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Option<ForwarderPolicy>, ProximaError> {
    match (lookup(FORWARDER_SUBJECTS), lookup(FORWARDER_ROLE)) {
        (None, None) => Ok(None),
        (Some(_), None) => Err(ProximaError::Config(format!(
            "{FORWARDER_SUBJECTS} set without {FORWARDER_ROLE}"
        ))),
        (None, Some(_)) => Err(ProximaError::Config(format!(
            "{FORWARDER_ROLE} set without {FORWARDER_SUBJECTS}"
        ))),
        (Some(subjects), Some(role)) => {
            let subjects = subjects
                .split(',')
                .map(str::trim)
                .filter(|raw| !raw.is_empty())
                .map(|raw| {
                    raw.parse::<uuid::Uuid>().map(UserId::new).map_err(|_| {
                        ProximaError::Config(format!(
                            "{FORWARDER_SUBJECTS} entry {raw:?} is not a user UUID"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let role = match role.to_ascii_lowercase().as_str() {
                "viewer" => Role::viewer(),
                "ingest" => Role::ingest(),
                "editor" => Role::editor(),
                other => {
                    return Err(ProximaError::Config(format!(
                        "{FORWARDER_ROLE} must be viewer, ingest, or editor, got {other:?}"
                    )));
                }
            };
            ForwarderPolicy::new(subjects, role).map(Some)
        }
    }
}

struct ForwarderOwnerAccess {
    policy: ForwarderPolicy,
    inner: Arc<dyn OwnerAccessPort>,
}

#[async_trait]
impl OwnerAccessPort for ForwarderOwnerAccess {
    async fn resolve_roles_for_subject(&self, subject: UserId) -> Result<OwnerRoles, AccessError> {
        self.inner.resolve_roles_for_subject(subject).await
    }

    async fn resolve_group_role(
        &self,
        subject: UserId,
        group: GroupId,
    ) -> Result<Option<Role>, AccessError> {
        if self.policy.is_forwarder(subject) {
            return Ok(Some(self.policy.role));
        }
        self.inner.resolve_group_role(subject, group).await
    }
}

/// An owner-access port whose target exists only after storage boots.
///
/// The environment OIDC authenticator is built while configuration
/// resolves — before any pool — yet must resolve roles through the port the
/// edge uses. Nothing authenticates before the listener serves, and the
/// runtime binds the port before serving, so an unbound call is a wiring
/// fault and refuses.
#[derive(Clone, Default)]
pub(crate) struct LateOwnerAccess {
    target: Arc<OnceLock<Arc<dyn OwnerAccessPort>>>,
}

impl LateOwnerAccess {
    /// Bind the target. A second bind is ignored: the first boot's port is
    /// the one every holder already resolved through.
    pub(crate) fn bind(&self, target: Arc<dyn OwnerAccessPort>) {
        let _ = self.target.set(target);
    }

    fn target(&self) -> Result<&Arc<dyn OwnerAccessPort>, AccessError> {
        self.target.get().ok_or_else(|| {
            AccessError::Resolution("owner access is not bound before the runtime booted".into())
        })
    }
}

impl std::fmt::Debug for LateOwnerAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LateOwnerAccess")
            .field("bound", &self.target.get().is_some())
            .finish()
    }
}

#[async_trait]
impl OwnerAccessPort for LateOwnerAccess {
    async fn resolve_roles_for_subject(&self, subject: UserId) -> Result<OwnerRoles, AccessError> {
        self.target()?.resolve_roles_for_subject(subject).await
    }

    async fn resolve_group_role(
        &self,
        subject: UserId,
        group: GroupId,
    ) -> Result<Option<Role>, AccessError> {
        self.target()?.resolve_group_role(subject, group).await
    }
}

#[cfg(test)]
mod tests {
    use proxima_core::{OwnerRef, OwnerRoles};

    use super::*;

    #[derive(Debug)]
    struct Members(OwnerRoles);

    #[async_trait]
    impl OwnerAccessPort for Members {
        async fn resolve_roles_for_subject(
            &self,
            _subject: UserId,
        ) -> Result<OwnerRoles, AccessError> {
            Ok(self.0.clone())
        }
    }

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(candidate, _)| *candidate == key)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[tokio::test]
    async fn a_forwarder_holds_the_fixed_role_in_any_group_it_selects() {
        let forwarder = UserId::new(uuid::Uuid::now_v7());
        let member = UserId::new(uuid::Uuid::now_v7());
        let group = GroupId::new(uuid::Uuid::now_v7());
        let port = ForwarderPolicy::new([forwarder], Role::editor())
            .unwrap()
            .wrap(Arc::new(Members(
                OwnerRoles::for_subject(member, []).unwrap(),
            )));

        assert_eq!(
            port.resolve_group_role(forwarder, group).await.unwrap(),
            Some(Role::editor())
        );
        assert_eq!(
            port.resolve_group_role(member, group).await.unwrap(),
            None,
            "a non-forwarder still resolves through the inner port"
        );
        assert!(
            port.resolve_roles_for_subject(forwarder)
                .await
                .unwrap()
                .role_for(&OwnerRef::Group(group))
                .is_none(),
            "the eager map is the inner port's, so delegation sees no policy role"
        );
    }

    #[test]
    fn the_env_policy_is_both_or_neither_and_never_admin() {
        let id = uuid::Uuid::now_v7().to_string();
        assert!(forwarder_from_lookup(&lookup(&[])).unwrap().is_none());
        let policy = forwarder_from_lookup(&lookup(&[
            (FORWARDER_SUBJECTS, &id),
            (FORWARDER_ROLE, "Editor"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(policy.role(), Role::editor());
        for pairs in [
            vec![(FORWARDER_SUBJECTS, id.as_str())],
            vec![(FORWARDER_ROLE, "viewer")],
            vec![(FORWARDER_SUBJECTS, id.as_str()), (FORWARDER_ROLE, "admin")],
            vec![(FORWARDER_SUBJECTS, "user:x"), (FORWARDER_ROLE, "viewer")],
            vec![(FORWARDER_SUBJECTS, " , "), (FORWARDER_ROLE, "viewer")],
        ] {
            assert!(
                forwarder_from_lookup(&lookup(&pairs)).is_err(),
                "{pairs:?} must be refused"
            );
        }
    }

    #[tokio::test]
    async fn an_unbound_late_port_refuses() {
        let late = LateOwnerAccess::default();
        let subject = UserId::new(uuid::Uuid::now_v7());
        assert!(late.resolve_roles_for_subject(subject).await.is_err());
        late.bind(Arc::new(Members(
            OwnerRoles::for_subject(subject, []).unwrap(),
        )));
        assert!(late.resolve_roles_for_subject(subject).await.is_ok());
    }
}
