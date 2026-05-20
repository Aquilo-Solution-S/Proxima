use std::collections::HashMap;
use std::sync::Mutex;

use crate::{EdgeId, GoalId, MemoryId, PersonalityInstanceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryHandleClass {
    Fact,
    Abstraction,
    Perspective,
}

impl MemoryHandleClass {
    #[must_use]
    pub const fn prefix(self) -> char {
        match self {
            Self::Fact => 'F',
            Self::Abstraction => 'A',
            Self::Perspective => 'P',
        }
    }

    #[must_use]
    pub fn from_memory_kind(kind: &str) -> Option<Self> {
        match kind {
            "Fact" | "fact" => Some(Self::Fact),
            "Abstraction" | "abstraction" => Some(Self::Abstraction),
            "Perspective" | "perspective" => Some(Self::Perspective),
            _ => None,
        }
    }
}

impl std::fmt::Display for MemoryHandleClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fact => write!(f, "Fact"),
            Self::Abstraction => write!(f, "Abstraction"),
            Self::Perspective => write!(f, "Perspective"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntityRef {
    Memory {
        id: MemoryId,
        class: MemoryHandleClass,
    },
    Edge(EdgeId),
    Goal(GoalId),
    FlavorObject {
        kind: String,
        id: uuid::Uuid,
    },
    Personality(PersonalityInstanceId),
    WakeEntry(uuid::Uuid),
}

impl EntityRef {
    #[must_use]
    pub fn kind(&self) -> EntityKind {
        match self {
            EntityRef::Memory { class, .. } => EntityKind::Memory(*class),
            EntityRef::Edge(_) => EntityKind::Edge,
            EntityRef::Goal(_) => EntityKind::Goal,
            EntityRef::FlavorObject { kind, .. } => EntityKind::FlavorObject { kind: kind.clone() },
            EntityRef::Personality(_) => EntityKind::Personality,
            EntityRef::WakeEntry(_) => EntityKind::WakeEntry,
        }
    }
}

/// Typed tag identifying which kind of entity a [`HandleTable`] entry
/// resolves to. Used by [`ResolveError::WrongKind`] to tell the model
/// what it asked for versus what it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityKind {
    Memory(MemoryHandleClass),
    AnyMemory,
    Edge,
    Goal,
    FlavorObject { kind: String },
    Personality,
    WakeEntry,
}

impl EntityKind {
    /// Stable handle prefix for this kind (`F`, `A`, `P`, `E`, `G`, `I`, `W`).
    /// Flavor objects use flavor-defined prefixes — `<flavor>` is a
    /// placeholder for messages.
    #[must_use]
    pub fn prefix(&self) -> &'static str {
        match self {
            EntityKind::Memory(MemoryHandleClass::Fact) => "F",
            EntityKind::Memory(MemoryHandleClass::Abstraction) => "A",
            EntityKind::Memory(MemoryHandleClass::Perspective) => "P",
            EntityKind::AnyMemory => "F/A/P",
            EntityKind::Edge => "E",
            EntityKind::Goal => "G",
            EntityKind::FlavorObject { .. } => "<flavor>",
            EntityKind::Personality => "I",
            EntityKind::WakeEntry => "W",
        }
    }
}

impl std::fmt::Display for EntityKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EntityKind::Memory(class) => write!(f, "{class} memory"),
            EntityKind::AnyMemory => write!(f, "Memory"),
            EntityKind::Edge => write!(f, "Edge"),
            EntityKind::Goal => write!(f, "Goal"),
            EntityKind::FlavorObject { kind } => write!(f, "FlavorObject({kind})"),
            EntityKind::Personality => write!(f, "Personality"),
            EntityKind::WakeEntry => write!(f, "WakeEntry"),
        }
    }
}

/// Error returned by [`HandleTable`] resolver methods. Replaces the
/// previous flat `Option<TypedId>` API; carries enough information for
/// the model-facing wrapper to format a corrective message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// `input` is not present in the table (typo, stale handle, or
    /// malformed shape).
    Unknown { input: String },
    /// `input` is a valid handle but refers to a different kind of
    /// entity than the caller asked for.
    WrongKind {
        input: String,
        got: EntityKind,
        expected: EntityKind,
    },
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Unknown { input } => write!(f, "unknown handle: {input}"),
            ResolveError::WrongKind {
                input,
                got,
                expected,
            } => write!(
                f,
                "expected {expected} handle ({}…), got {got} handle '{input}'",
                expected.prefix(),
            ),
        }
    }
}

impl std::error::Error for ResolveError {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Handle(String);

impl Handle {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Default)]
pub struct HandleTable {
    inner: Mutex<HandleTableInner>,
}

#[derive(Debug, Default)]
struct HandleTableInner {
    fact_counter: u32,
    abstraction_counter: u32,
    perspective_counter: u32,
    edge_counter: u32,
    goal_counter: u32,
    flavor_counters: HashMap<char, u32>,
    personality_counter: u32,
    wake_entry_counter: u32,
    by_memory: HashMap<MemoryId, (MemoryHandleClass, Handle)>,
    by_entity: HashMap<EntityRef, Handle>,
    by_handle: HashMap<String, EntityRef>,
}

impl HandleTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn assign_memory(&self, id: MemoryId) -> Handle {
        self.assign_fact_memory(id)
    }

    pub fn assign_fact_memory(&self, id: MemoryId) -> Handle {
        self.assign_memory_with_class(id, MemoryHandleClass::Fact)
    }

    pub fn assign_abstraction_memory(&self, id: MemoryId) -> Handle {
        self.assign_memory_with_class(id, MemoryHandleClass::Abstraction)
    }

    pub fn assign_perspective_memory(&self, id: MemoryId) -> Handle {
        self.assign_memory_with_class(id, MemoryHandleClass::Perspective)
    }

    pub fn assign_memory_kind(&self, id: MemoryId, kind: &str) -> Handle {
        self.assign_memory_with_class(
            id,
            MemoryHandleClass::from_memory_kind(kind).unwrap_or(MemoryHandleClass::Fact),
        )
    }

    pub fn assign_memory_with_class(&self, id: MemoryId, class: MemoryHandleClass) -> Handle {
        let mut inner = self.inner.lock().expect("handle table mutex poisoned");
        if let Some((existing_class, handle)) = inner.by_memory.get(&id) {
            assert_eq!(
                *existing_class,
                class,
                "memory handle class changed for {}: existing {}, requested {}",
                id.into_inner(),
                existing_class,
                class
            );
            return handle.clone();
        }

        let counter = match class {
            MemoryHandleClass::Fact => &mut inner.fact_counter,
            MemoryHandleClass::Abstraction => &mut inner.abstraction_counter,
            MemoryHandleClass::Perspective => &mut inner.perspective_counter,
        };
        *counter = counter.checked_add(1).expect("handle counter overflow");
        let raw = format!("{}{}", class.prefix(), counter);
        let handle = Handle(raw.clone());
        let entity = EntityRef::Memory { id, class };
        inner.by_memory.insert(id, (class, handle.clone()));
        inner.by_entity.insert(entity.clone(), handle.clone());
        inner.by_handle.insert(raw, entity);
        handle
    }

    pub fn memory_handle(&self, id: MemoryId) -> Option<Handle> {
        self.inner
            .lock()
            .expect("handle table mutex poisoned")
            .by_memory
            .get(&id)
            .map(|(_, handle)| handle.clone())
    }

    pub fn assign_edge(&self, id: EdgeId) -> Handle {
        self.assign(EntityRef::Edge(id), 'E', |inner| &mut inner.edge_counter)
    }

    pub fn assign_goal(&self, id: GoalId) -> Handle {
        self.assign(EntityRef::Goal(id), 'G', |inner| &mut inner.goal_counter)
    }

    pub fn assign_flavor_object(
        &self,
        kind: impl Into<String>,
        id: uuid::Uuid,
        prefix: char,
    ) -> Handle {
        assert!(
            prefix.is_ascii_uppercase(),
            "flavor object handle prefix must be ASCII uppercase"
        );
        self.assign(
            EntityRef::FlavorObject {
                kind: kind.into(),
                id,
            },
            prefix,
            |inner| inner.flavor_counters.entry(prefix).or_insert(0),
        )
    }

    pub fn assign_personality(&self, id: PersonalityInstanceId) -> Handle {
        self.assign(EntityRef::Personality(id), 'I', |inner| {
            &mut inner.personality_counter
        })
    }

    pub fn assign_wake_entry(&self, id: uuid::Uuid) -> Handle {
        self.assign(EntityRef::WakeEntry(id), 'W', |inner| {
            &mut inner.wake_entry_counter
        })
    }

    fn assign(
        &self,
        entity: EntityRef,
        prefix: char,
        counter: impl FnOnce(&mut HandleTableInner) -> &mut u32,
    ) -> Handle {
        let mut inner = self.inner.lock().expect("handle table mutex poisoned");
        if let Some(handle) = inner.by_entity.get(&entity) {
            return handle.clone();
        }

        let next = counter(&mut inner);
        *next = next.checked_add(1).expect("handle counter overflow");
        let raw = format!("{prefix}{next}");
        let handle = Handle(raw.clone());
        inner.by_entity.insert(entity.clone(), handle.clone());
        inner.by_handle.insert(raw, entity);
        handle
    }

    pub fn resolve_entity(&self, raw: &str) -> Result<EntityRef, ResolveError> {
        if !is_valid_handle_shape(raw) {
            return Err(ResolveError::Unknown {
                input: raw.to_string(),
            });
        }
        self.inner
            .lock()
            .expect("handle table mutex poisoned")
            .by_handle
            .get(raw)
            .cloned()
            .ok_or_else(|| ResolveError::Unknown {
                input: raw.to_string(),
            })
    }

    pub fn resolve_memory(&self, raw: &str) -> Result<MemoryId, ResolveError> {
        match self.resolve_entity(raw)? {
            EntityRef::Memory { id, .. } => Ok(id),
            other => Err(ResolveError::WrongKind {
                input: raw.to_string(),
                got: other.kind(),
                expected: EntityKind::AnyMemory,
            }),
        }
    }

    pub fn resolve_fact_memory(&self, raw: &str) -> Result<MemoryId, ResolveError> {
        self.resolve_memory_class(raw, MemoryHandleClass::Fact)
    }

    pub fn resolve_abstraction_memory(&self, raw: &str) -> Result<MemoryId, ResolveError> {
        self.resolve_memory_class(raw, MemoryHandleClass::Abstraction)
    }

    pub fn resolve_perspective_memory(&self, raw: &str) -> Result<MemoryId, ResolveError> {
        self.resolve_memory_class(raw, MemoryHandleClass::Perspective)
    }

    fn resolve_memory_class(
        &self,
        raw: &str,
        expected_class: MemoryHandleClass,
    ) -> Result<MemoryId, ResolveError> {
        match self.resolve_entity(raw)? {
            EntityRef::Memory { id, class } if class == expected_class => Ok(id),
            other => Err(ResolveError::WrongKind {
                input: raw.to_string(),
                got: other.kind(),
                expected: EntityKind::Memory(expected_class),
            }),
        }
    }

    pub fn resolve_edge(&self, raw: &str) -> Result<EdgeId, ResolveError> {
        match self.resolve_entity(raw)? {
            EntityRef::Edge(id) => Ok(id),
            other => Err(ResolveError::WrongKind {
                input: raw.to_string(),
                got: other.kind(),
                expected: EntityKind::Edge,
            }),
        }
    }

    pub fn resolve_goal(&self, raw: &str) -> Result<GoalId, ResolveError> {
        match self.resolve_entity(raw)? {
            EntityRef::Goal(id) => Ok(id),
            other => Err(ResolveError::WrongKind {
                input: raw.to_string(),
                got: other.kind(),
                expected: EntityKind::Goal,
            }),
        }
    }

    pub fn resolve_flavor_object(&self, raw: &str, kind: &str) -> Result<uuid::Uuid, ResolveError> {
        match self.resolve_entity(raw)? {
            EntityRef::FlavorObject {
                kind: actual_kind,
                id,
            } if actual_kind == kind => Ok(id),
            other => Err(ResolveError::WrongKind {
                input: raw.to_string(),
                got: other.kind(),
                expected: EntityKind::FlavorObject {
                    kind: kind.to_string(),
                },
            }),
        }
    }

    pub fn resolve_personality(&self, raw: &str) -> Result<PersonalityInstanceId, ResolveError> {
        match self.resolve_entity(raw)? {
            EntityRef::Personality(id) => Ok(id),
            other => Err(ResolveError::WrongKind {
                input: raw.to_string(),
                got: other.kind(),
                expected: EntityKind::Personality,
            }),
        }
    }

    pub fn resolve_wake_entry(&self, raw: &str) -> Result<uuid::Uuid, ResolveError> {
        match self.resolve_entity(raw)? {
            EntityRef::WakeEntry(id) => Ok(id),
            other => Err(ResolveError::WrongKind {
                input: raw.to_string(),
                got: other.kind(),
                expected: EntityKind::WakeEntry,
            }),
        }
    }
}

/// Handles assigned by `pre_seed_wake_handles` for entities the wake
/// context already names. Brief formatters and substrate tools that
/// need to refer to "the memory that woke me" / "the root
/// perspective" / "self" by handle read from this struct rather than
/// hard-coding handle strings.
#[derive(Debug, Clone)]
pub struct PreSeededHandles {
    pub triggering: Handle,
    pub root_perspective: Handle,
    pub self_instance: Handle,
    pub continuation_decision: Option<Handle>,
    pub continuation_request: Option<Handle>,
    pub continuation_wake_trace: Option<Handle>,
    pub continuation_original_triggering: Option<Handle>,
}

fn is_valid_handle_shape(raw: &str) -> bool {
    let mut chars = raw.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    let rest = chars.as_str();
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn handles_grow_monotonically_per_kind() {
        let table = HandleTable::new();
        let f1 = MemoryId::new(Uuid::now_v7());
        let f2 = MemoryId::new(Uuid::now_v7());
        let a1 = MemoryId::new(Uuid::now_v7());
        let p1 = MemoryId::new(Uuid::now_v7());
        let e1 = EdgeId::new(Uuid::now_v7());
        assert_eq!(table.assign_fact_memory(f1).as_str(), "F1");
        assert_eq!(table.assign_fact_memory(f2).as_str(), "F2");
        assert_eq!(table.assign_abstraction_memory(a1).as_str(), "A1");
        assert_eq!(table.assign_perspective_memory(p1).as_str(), "P1");
        assert_eq!(table.assign_edge(e1).as_str(), "E1");
    }

    #[test]
    fn assigning_same_uuid_returns_same_handle() {
        let table = HandleTable::new();
        let m = MemoryId::new(Uuid::now_v7());
        let h1 = table.assign_memory(m);
        let h2 = table.assign_memory(m);
        assert_eq!(h1, h2);
    }

    #[test]
    fn resolve_unknown_handle_returns_unknown_error() {
        let table = HandleTable::new();
        let err = table.resolve_entity("F99").unwrap_err();
        assert_eq!(
            err,
            ResolveError::Unknown {
                input: "F99".into()
            }
        );
    }

    #[test]
    fn resolve_round_trips() {
        let table = HandleTable::new();
        let m = MemoryId::new(Uuid::now_v7());
        let h = table.assign_fact_memory(m);
        let r = table.resolve_entity(h.as_str()).expect("known handle");
        assert!(matches!(r, EntityRef::Memory { id, class: MemoryHandleClass::Fact } if id == m));
    }

    #[test]
    fn malformed_handle_string_is_rejected() {
        let table = HandleTable::new();
        for raw in ["nope", "F", "Ffoo", "X1"] {
            let err = table.resolve_entity(raw).unwrap_err();
            assert_eq!(
                err,
                ResolveError::Unknown {
                    input: raw.to_string()
                },
                "input {raw}"
            );
        }
    }

    #[test]
    fn personality_handles_use_i_prefix() {
        let table = HandleTable::new();
        let p1 = PersonalityInstanceId::new(uuid::Uuid::now_v7());
        let p2 = PersonalityInstanceId::new(uuid::Uuid::now_v7());
        assert_eq!(table.assign_personality(p1).as_str(), "I1");
        assert_eq!(table.assign_personality(p2).as_str(), "I2");
        assert_eq!(table.assign_personality(p1).as_str(), "I1", "idempotent");
    }

    #[test]
    fn wake_entry_handles_use_w_prefix() {
        let table = HandleTable::new();
        let w1 = uuid::Uuid::now_v7();
        let w2 = uuid::Uuid::now_v7();
        assert_eq!(table.assign_wake_entry(w1).as_str(), "W1");
        assert_eq!(table.assign_wake_entry(w2).as_str(), "W2");
        assert_eq!(table.assign_wake_entry(w1).as_str(), "W1", "idempotent");
    }

    #[test]
    fn resolve_personality_rejects_non_i_handle() {
        let table = HandleTable::new();
        let p = PersonalityInstanceId::new(uuid::Uuid::now_v7());
        let _ = table.assign_personality(p);
        let m = MemoryId::new(uuid::Uuid::now_v7());
        let mh = table.assign_memory(m);
        let err = table.resolve_personality(mh.as_str()).unwrap_err();
        assert_eq!(
            err,
            ResolveError::WrongKind {
                input: mh.as_str().to_string(),
                got: EntityKind::Memory(MemoryHandleClass::Fact),
                expected: EntityKind::Personality,
            }
        );
    }

    #[test]
    fn malformed_personality_handle_rejected() {
        let table = HandleTable::new();
        for raw in ["Ifoo", "I", "i1"] {
            let err = table.resolve_personality(raw).unwrap_err();
            assert_eq!(
                err,
                ResolveError::Unknown {
                    input: raw.to_string()
                },
                "input {raw}"
            );
        }
    }

    #[test]
    fn resolve_memory_wrong_kind_returns_typed_error() {
        let table = HandleTable::new();
        let g = GoalId::new(Uuid::now_v7());
        let h = table.assign_goal(g);
        let err = table.resolve_memory(h.as_str()).unwrap_err();
        assert_eq!(
            err,
            ResolveError::WrongKind {
                input: h.as_str().to_string(),
                got: EntityKind::Goal,
                expected: EntityKind::AnyMemory,
            }
        );
    }

    #[test]
    fn resolve_error_display_includes_kinds() {
        let err = ResolveError::WrongKind {
            input: "G7".into(),
            got: EntityKind::Goal,
            expected: EntityKind::AnyMemory,
        };
        let msg = err.to_string();
        assert!(msg.contains("expected Memory"), "msg: {msg}");
        assert!(msg.contains("got Goal"), "msg: {msg}");
        assert!(msg.contains("G7"), "msg: {msg}");
    }

    #[test]
    fn resolve_flavor_object_wrong_kind_returns_typed_error() {
        let table = HandleTable::new();
        let id = uuid::Uuid::now_v7();
        let h = table.assign_flavor_object("code/repository", id, 'R');
        let err = table
            .resolve_flavor_object(h.as_str(), "code/file")
            .unwrap_err();
        assert_eq!(
            err,
            ResolveError::WrongKind {
                input: h.as_str().to_string(),
                got: EntityKind::FlavorObject {
                    kind: "code/repository".into()
                },
                expected: EntityKind::FlavorObject {
                    kind: "code/file".into()
                },
            }
        );
    }
}
