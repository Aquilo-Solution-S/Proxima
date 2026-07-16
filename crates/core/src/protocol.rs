//! Typed protocol identifiers shared by core MCP catalogs, profiles, and
//! resource routing.

pub mod tool {
    pub const CORE_SEARCH_MEMORIES: &str = "core_search_memories";
    pub const CORE_MEMORY_SPACES: &str = "core_memory_spaces";
    pub const CORE_REMEMBER: &str = "core_remember";
    pub const CORE_RECORD_UTTERANCE: &str = "core_record_utterance";
    pub const CORE_DERIVE: &str = "core_derive";
    pub const CORE_LINK: &str = "core_link";
    pub const CORE_GOAL: &str = "core_goal";
    pub const CORE_FACT: &str = "core_fact";
    pub const CORE_MEMBERSHIP: &str = "core_membership";
    pub const CORE_PUBLISH: &str = "core_publish";
}

pub mod action {
    pub const CORE_FACT_CITATION_OF_FACT: &str = "core_fact:citation_of_fact";
    pub const CORE_FACT_CITATION_OF_ENTITY_HEAD: &str = "core_fact:citation_of_entity_head";
    pub const CORE_FACT_FACTS_CITING_OBJECT: &str = "core_fact:facts_citing_object";

    pub const CORE_GOAL_SET: &str = "core_goal:set";
    pub const CORE_GOAL_TRANSITION: &str = "core_goal:transition";
    pub const CORE_GOAL_MODIFY: &str = "core_goal:modify";
    pub const CORE_GOAL_MARK_ACHIEVED: &str = "core_goal:mark_achieved";
    pub const CORE_GOAL_DECOMPOSE: &str = "core_goal:decompose";

    pub const CORE_MEMBERSHIP_ADD_MEMBER: &str = "core_membership:add_member";
    pub const CORE_MEMBERSHIP_REMOVE_MEMBER: &str = "core_membership:remove_member";
    pub const CORE_MEMBERSHIP_LIST_MEMBERS: &str = "core_membership:list_members";

    pub const CORE_PUBLISH_TO_WORLD: &str = "core_publish:publish_to_world";
}

pub mod resource {
    pub const MEMORY: &str = "resource:memory";
    pub const MEMORY_LINEAGE: &str = "resource:memory-lineage";
    pub const EDGE_TYPES: &str = "resource:edge-types";
    pub const TOOLS: &str = "resource:tools";
    pub const GRAPH: &str = "resource:graph";
    pub const CHANGE_EVENTS: &str = "resource:change-events";
    pub const WAKE_CANDIDATES: &str = "resource:wake-candidates";
    pub const SCHEMAS: &str = "resource:schemas";
    pub const GOALS: &str = "resource:goals";
    pub const GOAL: &str = "resource:goal";
}

pub mod resource_path {
    pub const SCHEMAS: &str = "schemas";
    pub const EDGE_TYPES: &str = "edge-types";
    pub const TOOLS: &str = "tools";
    pub const GRAPH: &str = "graph";
    pub const CHANGE_EVENTS: &str = "change-events";
    pub const WAKE_CANDIDATES: &str = "wake-candidates";
    pub const MEMORY: &str = "memory";
    pub const GOALS: &str = "goals";
    pub const GOAL: &str = "goal";
}

pub mod resource_uri {
    pub const SCHEMAS: &str = "proxima://schemas{?kind}";
    pub const EDGE_TYPES: &str = "proxima://edge-types";
    pub const TOOLS: &str = "proxima://tools";
    pub const GRAPH: &str = "proxima://graph";
    pub const MEMORY: &str = "proxima://memory/{id}{?expand_neighbors}";
    pub const MEMORY_LINEAGE: &str = "proxima://memory/{id}/lineage{?direction,depth,limit}";
    pub const CHANGE_EVENTS: &str = "proxima://change-events{?since,limit}";
    pub const WAKE_CANDIDATES: &str = "proxima://wake-candidates{?fact,limit}";
    pub const GOALS: &str = "proxima://goals{?state,limit,cursor}";
    pub const GOAL: &str = "proxima://goal/{id}";
}

pub mod profile {
    pub const FULL: &str = "full";
    pub const MEMORY: &str = "memory";
}
