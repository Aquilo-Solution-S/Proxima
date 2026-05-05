use std::collections::HashSet;

use proxima_core::{AuthResolver, Credentials, NoAuth, OrgId, Owner, Principal, UserId};
use uuid::Uuid;

#[test]
fn noauth_returns_fixed_owner() {
    let principal = Principal::User(UserId::new(Uuid::now_v7()));
    let owner = Owner {
        principal: principal.clone(),
        org_id: OrgId::new(Uuid::now_v7()),
    };

    let resolver = NoAuth::new(principal.clone(), owner.clone());
    let resolved = resolver.resolve(&Credentials::None).unwrap();

    assert_eq!(resolved.principal, principal);
    assert_eq!(
        resolved.accessible_principals,
        HashSet::from([owner.principal])
    );
}
