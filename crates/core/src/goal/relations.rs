use crate::{AuthorshipKindMask, EntityKindMask, RelationClass, RelationDescriptor};

pub const CORE_MOTIVATED_BY_RELATION: &str = "core/motivated-by";

#[must_use]
pub fn motivated_by_descriptor() -> RelationDescriptor {
    RelationDescriptor::substrate(
        CORE_MOTIVATED_BY_RELATION,
        RelationClass::Structural,
        EntityKindMask::goal(),
        EntityKindMask::fact_abstraction(),
        AuthorshipKindMask::user()
            .union(AuthorshipKindMask::engine())
            .union(AuthorshipKindMask::external_agent()),
    )
}
