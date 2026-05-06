use proxima_core::{RelationClass, RelationDescriptor};

pub const MOTIVATED_BY_RELATION: &str = "proxima-goal/motivated-by";

#[must_use]
pub fn descriptor() -> RelationDescriptor {
    RelationDescriptor::substrate(MOTIVATED_BY_RELATION, RelationClass::Structural)
}
