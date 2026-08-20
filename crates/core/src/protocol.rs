//! Typed protocol identifiers shared by core MCP catalogs, profiles, and
//! resource routing.

pub mod tool {
    pub const CORE_SEARCH_MEMORIES: &str = "core_search_memories";
    pub const CORE_RECALL: &str = "core_recall";
    pub const CORE_THINK: &str = "core_think";
    pub const CORE_MEMORY_SPACES: &str = "core_memory_spaces";
    pub const CORE_REMEMBER: &str = "core_remember";
    pub const CORE_EPISODE_COMMIT: &str = "core_episode_commit";
    pub const CORE_RECORD_UTTERANCE: &str = "core_record_utterance";
    pub const CORE_DERIVE: &str = "core_derive";
    pub const CORE_INTERPRET: &str = "core_interpret";
    pub const CORE_GOAL: &str = "core_goal";
    pub const CORE_FACT: &str = "core_fact";
    pub const CORE_MEMBERSHIP: &str = "core_membership";
    pub const CORE_TRANSFER: &str = "core_transfer";
    pub const CORE_UPLOAD: &str = "core_upload";
    pub const CORE_FORGET: &str = "core_forget";
}

pub mod action {
    pub const CORE_FACT_CITATION_OF_FACT: &str = "core_fact:citation_of_fact";
    pub const CORE_FACT_FACTS_CITING_OBJECT: &str = "core_fact:facts_citing_object";

    pub const CORE_GOAL_SET: &str = "core_goal:set";
    pub const CORE_GOAL_TRANSITION: &str = "core_goal:transition";
    pub const CORE_GOAL_MODIFY: &str = "core_goal:modify";
    pub const CORE_GOAL_MARK_ACHIEVED: &str = "core_goal:mark_achieved";
    pub const CORE_GOAL_DECOMPOSE: &str = "core_goal:decompose";

    pub const CORE_MEMBERSHIP_ADD_MEMBER: &str = "core_membership:add_member";
    pub const CORE_MEMBERSHIP_REMOVE_MEMBER: &str = "core_membership:remove_member";
    pub const CORE_MEMBERSHIP_LIST_MEMBERS: &str = "core_membership:list_members";

    pub const CORE_TRANSFER_TO_OWNER: &str = "core_transfer:transfer_to_owner";

    pub const CORE_UPLOAD_PREPARE: &str = "core_upload:prepare";
    pub const CORE_UPLOAD_COMPLETE: &str = "core_upload:complete";
    pub const CORE_UPLOAD_ABORT: &str = "core_upload:abort";
    pub const CORE_UPLOAD_READ_URL: &str = "core_upload:read_url";
}

pub mod resource {
    /// Namespace every resource scope key carries. An MCP resource is a read
    /// by definition — the protocol has no resource write verb — so this
    /// prefix is what tells the authorization gate a request is a read.
    pub const SCOPE_PREFIX: &str = "resource:";

    pub const MEMORY: &str = "resource:memory";
    pub const MEMORIES: &str = "resource:memories";
    pub const MEMORY_LINEAGE: &str = "resource:memory-lineage";
    pub const TOOLS: &str = "resource:tools";
    pub const GRAPH: &str = "resource:graph";
    pub const CHANGE_EVENTS: &str = "resource:change-events";
    pub const WAKE_CANDIDATES: &str = "resource:wake-candidates";
    pub const SCHEMAS: &str = "resource:schemas";
    pub const GOALS: &str = "resource:goals";
    pub const GOAL: &str = "resource:goal";
}

// `resource_path` and `resource_uri` used to live here: two more tables of
// ten, one keying the dispatcher's match and one keying the advertised
// manifest, neither aware of the other. Both are now fields on the
// `ResourceContract` entries in `flavor::flavor0`, reachable by scope key
// through `flavor0::resource`, so a resource's URI template, its dispatch
// path and its palette entry cannot drift apart.

pub mod profile {
    pub const FULL: &str = "full";
    pub const MEMORY: &str = "memory";
}
