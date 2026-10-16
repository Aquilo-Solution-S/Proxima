//! The one owner-access port a served runtime resolves roles through.
//!
//! The MCP edge's membership and verified-owner resolution, the
//! delegation service, and an environment-built OIDC authenticator all ask
//! the same [`OwnerAccessPort`]: the host's own
//! ([`crate::RuntimeBuilder::owner_access`]) or the runtime's Postgres
//! resolver, optionally under a [`ForwarderPolicy`].

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use proxima_core::{
    AccessError, ForwarderRole, GroupId, OwnerAccessPort, OwnerId, OwnerRoles, Role, UserId,
};

use crate::ProximaError;

const FORWARDER_SUBJECTS: &str = "PROXIMA_FORWARDER_SUBJECTS";
const FORWARDER_ROLE: &str = "PROXIMA_FORWARDER_ROLE";

/// Trusted forwarder subjects and their fixed role for a verified owner claim.
///
/// The edge verifies the credential's owner claim and request selection
/// before asking for one registered owner's role. Personal and Group owners
/// are both supported. The answer is the policy ceiling alone: memberships
/// do not raise it. Ordinary role resolution still delegates to the wrapped
/// port, so delegation never sees policy authority as a membership.
///
/// The role may not manage Groups: a forwarder that could administer any
/// Group it names could make itself, or anyone, a member of every one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwarderPolicy {
    subjects: BTreeSet<UserId>,
    role: ForwarderRole,
}

impl ForwarderPolicy {
    /// # Errors
    ///
    /// [`ProximaError::Config`] when `subjects` is empty.
    pub fn new(
        subjects: impl IntoIterator<Item = UserId>,
        role: ForwarderRole,
    ) -> Result<Self, ProximaError> {
        let subjects: BTreeSet<UserId> = subjects.into_iter().collect();
        if subjects.is_empty() {
            return Err(ProximaError::Config(
                "a forwarder policy needs at least one subject".into(),
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
        self.role.role()
    }

    /// `inner` with this policy answering verified-owner probes for its
    /// subjects, limited to owners `owners` holds as registered.
    #[must_use]
    pub fn wrap(
        self,
        inner: Arc<dyn OwnerAccessPort>,
        owners: Arc<dyn OwnerRegistry>,
    ) -> Arc<dyn OwnerAccessPort> {
        Arc::new(ForwarderOwnerAccess {
            policy: self,
            inner,
            owners,
        })
    }
}

/// The authoritative owner registry's one answer the forwarder policy needs.
#[async_trait]
pub trait OwnerRegistry: Send + Sync {
    /// Whether this id is a registered Personal or Group owner.
    ///
    /// # Errors
    ///
    /// [`AccessError::Resolution`] when the registry cannot be read.
    async fn is_registered_owner(&self, owner: OwnerId) -> Result<bool, AccessError>;
}

#[async_trait]
impl OwnerRegistry for proxima_storage_pg::PgOwnerAccessResolver {
    async fn is_registered_owner(&self, owner: OwnerId) -> Result<bool, AccessError> {
        Self::is_registered_owner(self, owner).await
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
                "viewer" => ForwarderRole::Viewer,
                "ingest" => ForwarderRole::Ingest,
                "editor" => ForwarderRole::Editor,
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
    owners: Arc<dyn OwnerRegistry>,
}

#[async_trait]
impl OwnerAccessPort for ForwarderOwnerAccess {
    async fn resolve_roles_for_subject(&self, subject: UserId) -> Result<OwnerRoles, AccessError> {
        self.inner.resolve_roles_for_subject(subject).await
    }

    fn is_forwarder(&self, subject: UserId) -> Result<bool, AccessError> {
        Ok(self.policy.is_forwarder(subject))
    }

    async fn resolve_forwarded_owner_role(
        &self,
        subject: UserId,
        owner: OwnerId,
    ) -> Result<Option<ForwarderRole>, AccessError> {
        if !self.policy.is_forwarder(subject) {
            return Ok(None);
        }
        Ok(self
            .owners
            .is_registered_owner(owner)
            .await?
            .then_some(self.policy.role))
    }

    async fn resolve_group_role(
        &self,
        subject: UserId,
        group: GroupId,
    ) -> Result<Option<Role>, AccessError> {
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

    fn is_forwarder(&self, subject: UserId) -> Result<bool, AccessError> {
        self.target()?.is_forwarder(subject)
    }

    async fn resolve_forwarded_owner_role(
        &self,
        subject: UserId,
        owner: OwnerId,
    ) -> Result<Option<ForwarderRole>, AccessError> {
        self.target()?
            .resolve_forwarded_owner_role(subject, owner)
            .await
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
    use proxima_core::{OwnerId, OwnerRoles};

    use super::*;

    #[derive(Debug)]
    struct Members(OwnerRoles);

    #[async_trait]
    impl OwnerAccessPort for Members {
        async fn resolve_roles_for_subject(
            &self,
            subject: UserId,
        ) -> Result<OwnerRoles, AccessError> {
            Ok(if self.0.subject() == subject {
                self.0.clone()
            } else {
                OwnerRoles::empty_for_subject(subject)
            })
        }
    }

    /// A registry holding exactly the listed owners.
    struct Registered(Vec<OwnerId>);

    #[async_trait]
    impl OwnerRegistry for Registered {
        async fn is_registered_owner(&self, owner: OwnerId) -> Result<bool, AccessError> {
            Ok(self.0.contains(&owner))
        }
    }

    struct Unavailable;

    #[async_trait]
    impl OwnerRegistry for Unavailable {
        async fn is_registered_owner(&self, _owner: OwnerId) -> Result<bool, AccessError> {
            Err(AccessError::Resolution("owner registry unavailable".into()))
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
    async fn a_forwarded_owner_gets_only_the_policy_ceiling_even_with_admin_membership() {
        let forwarder = UserId::new(uuid::Uuid::now_v7());
        let member = UserId::new(uuid::Uuid::now_v7());
        let group = GroupId::new(uuid::Uuid::now_v7());
        let port = ForwarderPolicy::new([forwarder], ForwarderRole::Editor)
            .unwrap()
            .wrap(
                Arc::new(Members(
                    OwnerRoles::for_subject(forwarder, [(OwnerId::from(group), Role::admin())])
                        .unwrap(),
                )),
                Arc::new(Registered(vec![OwnerId::from(group)])),
            );

        assert!(port.is_forwarder(forwarder).unwrap());
        assert!(!port.is_forwarder(member).unwrap());
        let policy_role = port
            .resolve_forwarded_owner_role(forwarder, OwnerId::from(group))
            .await
            .unwrap()
            .expect("registered owner gets the configured policy role");
        assert_eq!(policy_role.role(), Role::editor());
        assert!(!policy_role.role().manages());
        assert_eq!(
            port.resolve_group_role(forwarder, group).await.unwrap(),
            Some(Role::admin()),
            "ordinary membership resolution delegates unchanged"
        );
        assert_eq!(
            port.resolve_group_role(member, group).await.unwrap(),
            None,
            "a non-forwarder still resolves only its own ordinary memberships"
        );
        assert_eq!(
            port.resolve_forwarded_owner_role(member, OwnerId::from(group))
                .await
                .unwrap(),
            None,
            "an unconfigured subject receives no forwarding authority"
        );
        assert_eq!(
            port.resolve_roles_for_subject(forwarder)
                .await
                .unwrap()
                .role_for(&OwnerId::from(group)),
            Some(Role::admin()),
            "the eager membership map does not join the policy role"
        );
    }

    #[tokio::test]
    async fn verified_personal_and_group_owners_are_supported_without_creating_memberships() {
        let forwarder = UserId::new(uuid::Uuid::now_v7());
        let user = UserId::new(uuid::Uuid::now_v7());
        let group = GroupId::new(uuid::Uuid::now_v7());
        let port = ForwarderPolicy::new([forwarder], ForwarderRole::Editor)
            .unwrap()
            .wrap(
                Arc::new(Members(OwnerRoles::for_subject(forwarder, []).unwrap())),
                Arc::new(Registered(vec![OwnerId::from(user), OwnerId::from(group)])),
            );

        for owner in [OwnerId::from(user), OwnerId::from(group)] {
            assert_eq!(
                port.resolve_forwarded_owner_role(forwarder, owner)
                    .await
                    .unwrap()
                    .map(ForwarderRole::role),
                Some(Role::editor())
            );
            assert!(
                port.resolve_roles_for_subject(forwarder)
                    .await
                    .unwrap()
                    .role_for(&owner)
                    .is_none(),
                "the eager map supplies no policy role to delegation"
            );
        }
        assert_eq!(
            port.resolve_group_role(forwarder, OwnerId::from(user).as_group())
                .await
                .unwrap(),
            None,
            "the ordinary group probe does not create personal-owner authority"
        );
        assert_eq!(
            port.resolve_forwarded_owner_role(forwarder, OwnerId::new(uuid::Uuid::now_v7()))
                .await
                .unwrap(),
            None,
            "an owner the authoritative registry never saw is refused"
        );
        assert_eq!(
            port.resolve_group_role(forwarder, group).await.unwrap(),
            None,
            "a registered group does not become a policy membership"
        );
    }

    #[tokio::test]
    async fn registry_failure_refuses_forwarding_without_affecting_policy_or_memberships() {
        let forwarder = UserId::new(uuid::Uuid::now_v7());
        let other = UserId::new(uuid::Uuid::now_v7());
        let group = GroupId::new(uuid::Uuid::now_v7());
        let port = ForwarderPolicy::new([forwarder], ForwarderRole::Viewer)
            .unwrap()
            .wrap(
                Arc::new(Members(OwnerRoles::for_subject(forwarder, []).unwrap())),
                Arc::new(Unavailable),
            );
        assert!(port.is_forwarder(forwarder).unwrap());
        assert!(!port.is_forwarder(other).unwrap());
        assert!(matches!(
            port.resolve_forwarded_owner_role(forwarder, OwnerId::from(group))
                .await,
            Err(AccessError::Resolution(_))
        ));
        assert_eq!(
            port.resolve_forwarded_owner_role(other, OwnerId::from(group))
                .await
                .unwrap(),
            None,
            "an unconfigured subject never triggers a target-owner lookup"
        );
        assert_eq!(
            port.resolve_group_role(forwarder, group).await.unwrap(),
            None
        );
    }

    #[test]
    fn policy_configuration_accepts_closed_roles_and_refuses_empty_subjects() {
        let forwarder = UserId::new(uuid::Uuid::now_v7());
        for role in [
            ForwarderRole::Viewer,
            ForwarderRole::Ingest,
            ForwarderRole::Editor,
        ] {
            let policy = ForwarderPolicy::new([forwarder], role).unwrap();
            assert_eq!(policy.role(), role.role());
            assert!(!policy.role().manages());
        }
        assert!(ForwarderPolicy::new([], ForwarderRole::Viewer).is_err());
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
        let group = GroupId::new(uuid::Uuid::now_v7());
        let owner = OwnerId::from(group);
        assert!(late.resolve_roles_for_subject(subject).await.is_err());
        assert!(late.resolve_group_role(subject, group).await.is_err());
        assert!(late.is_forwarder(subject).is_err());
        assert!(
            late.resolve_forwarded_owner_role(subject, owner)
                .await
                .is_err()
        );
        late.bind(
            ForwarderPolicy::new([subject], ForwarderRole::Ingest)
                .unwrap()
                .wrap(
                    Arc::new(Members(OwnerRoles::for_subject(subject, []).unwrap())),
                    Arc::new(Registered(vec![owner])),
                ),
        );
        assert!(late.resolve_roles_for_subject(subject).await.is_ok());
        assert!(late.is_forwarder(subject).unwrap());
        assert_eq!(
            late.resolve_forwarded_owner_role(subject, owner)
                .await
                .unwrap()
                .map(ForwarderRole::role),
            Some(Role::ingest())
        );
        assert_eq!(late.resolve_group_role(subject, group).await.unwrap(), None);
    }
}
