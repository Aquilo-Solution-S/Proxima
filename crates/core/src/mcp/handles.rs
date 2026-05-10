use std::collections::HashMap;
use std::sync::Mutex;

use crate::{EdgeId, GoalId, MemoryId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityRef {
    Memory(MemoryId),
    Edge(EdgeId),
    Goal(GoalId),
    Repo(uuid::Uuid),
}

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
    memory_counter: u32,
    edge_counter: u32,
    goal_counter: u32,
    repo_counter: u32,
    by_entity: HashMap<EntityRef, Handle>,
    by_handle: HashMap<String, EntityRef>,
}

impl HandleTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn assign_memory(&self, id: MemoryId) -> Handle {
        self.assign(EntityRef::Memory(id), 'N', |inner| {
            &mut inner.memory_counter
        })
    }

    pub fn assign_edge(&self, id: EdgeId) -> Handle {
        self.assign(EntityRef::Edge(id), 'E', |inner| &mut inner.edge_counter)
    }

    pub fn assign_goal(&self, id: GoalId) -> Handle {
        self.assign(EntityRef::Goal(id), 'G', |inner| &mut inner.goal_counter)
    }

    pub fn assign_repo(&self, id: uuid::Uuid) -> Handle {
        self.assign(EntityRef::Repo(id), 'R', |inner| &mut inner.repo_counter)
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
        inner.by_entity.insert(entity, handle.clone());
        inner.by_handle.insert(raw, entity);
        handle
    }

    #[must_use]
    pub fn resolve(&self, raw: &str) -> Option<EntityRef> {
        if !is_valid_handle_shape(raw) {
            return None;
        }
        self.inner
            .lock()
            .expect("handle table mutex poisoned")
            .by_handle
            .get(raw)
            .copied()
    }

    #[must_use]
    pub fn resolve_memory(&self, raw: &str) -> Option<MemoryId> {
        match self.resolve(raw)? {
            EntityRef::Memory(id) => Some(id),
            EntityRef::Edge(_) | EntityRef::Goal(_) | EntityRef::Repo(_) => None,
        }
    }

    #[must_use]
    pub fn resolve_edge(&self, raw: &str) -> Option<EdgeId> {
        match self.resolve(raw)? {
            EntityRef::Edge(id) => Some(id),
            EntityRef::Memory(_) | EntityRef::Goal(_) | EntityRef::Repo(_) => None,
        }
    }

    #[must_use]
    pub fn resolve_goal(&self, raw: &str) -> Option<GoalId> {
        match self.resolve(raw)? {
            EntityRef::Goal(id) => Some(id),
            EntityRef::Memory(_) | EntityRef::Edge(_) | EntityRef::Repo(_) => None,
        }
    }

    #[must_use]
    pub fn resolve_repo(&self, raw: &str) -> Option<uuid::Uuid> {
        match self.resolve(raw)? {
            EntityRef::Repo(id) => Some(id),
            EntityRef::Memory(_) | EntityRef::Edge(_) | EntityRef::Goal(_) => None,
        }
    }
}

fn is_valid_handle_shape(raw: &str) -> bool {
    let mut chars = raw.chars();
    match chars.next() {
        Some('N' | 'E' | 'G' | 'R') => {}
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
        let m1 = MemoryId::new(Uuid::now_v7());
        let m2 = MemoryId::new(Uuid::now_v7());
        let e1 = EdgeId::new(Uuid::now_v7());
        assert_eq!(table.assign_memory(m1).as_str(), "N1");
        assert_eq!(table.assign_memory(m2).as_str(), "N2");
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
    fn resolve_unknown_handle_returns_none() {
        let table = HandleTable::new();
        assert!(table.resolve("N99").is_none());
    }

    #[test]
    fn resolve_round_trips() {
        let table = HandleTable::new();
        let m = MemoryId::new(Uuid::now_v7());
        let h = table.assign_memory(m);
        let r = table.resolve(h.as_str()).expect("known handle");
        assert!(matches!(r, EntityRef::Memory(x) if x == m));
    }

    #[test]
    fn malformed_handle_string_is_rejected() {
        let table = HandleTable::new();
        assert!(table.resolve("nope").is_none());
        assert!(table.resolve("N").is_none());
        assert!(table.resolve("Nfoo").is_none());
        assert!(table.resolve("X1").is_none());
    }
}
