use proxima_core::{Owner, OwnerPrincipalKind};

pub(super) fn owner_from_parts(
    kind: OwnerPrincipalKind,
    principal_id: uuid::Uuid,
    org_id: uuid::Uuid,
) -> Owner {
    Owner {
        principal: kind.with_uuid(principal_id),
        org_id: proxima_core::OrgId::new(org_id),
    }
}
