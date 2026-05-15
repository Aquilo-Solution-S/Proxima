use proxima_core::{AuthorshipKindMask, EntityKindMask, RelationClass, RelationDescriptor};

pub const MOTIVATED_BY_RELATION: &str = "proxima-goal/motivated-by";

#[must_use]
pub fn descriptor() -> RelationDescriptor {
    RelationDescriptor::substrate(
        MOTIVATED_BY_RELATION,
        RelationClass::Structural,
        EntityKindMask::goal(),
        EntityKindMask::fact_abstraction(),
        AuthorshipKindMask::user()
            .union(AuthorshipKindMask::engine())
            .union(AuthorshipKindMask::external_agent()),
    )
}
