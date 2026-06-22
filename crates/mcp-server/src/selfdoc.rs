//! Server self-documentation: the profile-aware `instructions` string
//! returned at `initialize` and the `proxima://how-to` resource body.
//!
//! Both are generated from the *resolved* tool set — the same
//! `authz.capabilities.tool_scope`-filtered descriptor list that
//! `list_tools` / `core/list_substrate_tools` advertise — so the How-To
//! never references a tool a given deployment profile (`PROXIMA_TOOL_PROFILE`
//! plus allow/deny) does not expose. A `memory` deployment that drops the
//! execution/personality tools drops their guidance too, automatically.
//!
//! The generators are pure functions of the advertised canonical tool-id
//! set, which makes them unit-testable without a database or transport.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use proxima_core::mcp::McpTool;
use proxima_core::mcp::core_tools::{
    DeriveTool, GetMemoryTool, GoalDecomposeTool, GoalSetTool, LinkTool, ListEdgeTypesTool,
    RememberTool, SearchMemoriesTool, WalkMemoryLineageTool,
};

/// Canonical URI of the on-demand How-To resource.
pub const HOW_TO_URI: &str = "proxima://how-to";
/// Machine name of the How-To resource (used in `resources/list`).
pub const HOW_TO_NAME: &str = "proxima-how-to";
/// Human title of the How-To resource.
pub const HOW_TO_TITLE: &str = "Proxima shared-brain: how to use it";
/// One-line description of the How-To resource.
pub const HOW_TO_DESCRIPTION: &str = "The Proxima memory contract in depth: the Fact/Abstraction/Perspective \
     layering law, remember-vs-derive-vs-link, worked examples, the edge-class \
     table, and the read-tool decision guide.";
/// MIME type of the How-To resource body.
pub const HOW_TO_MIME: &str = "text/markdown";

// Code-flavor tools live in `proxima-code`, which `mcp-server` does not
// depend on, so these ids are matched as literals rather than via a
// `McpTool::NAME` const. They stay correct as long as the flavor keeps these
// canonical ids (the `memory` keep set in `apps/proxima-mcp` pins them too).
const CODE_SEARCH_CHUNKS: &str = "proxima-code/search_chunks";
const CODE_REGISTER_REPO: &str = "proxima-code/register_repo";

/// The advertised surface, distilled to the booleans the generators key off.
/// Computed once from the resolved tool-id set so `build_instructions` and
/// `how_to_markdown` agree on what is exposed. The fields are independent
/// tool-presence flags, not a state machine — an enum would not model them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy)]
struct Surface {
    remember: bool,
    derive: bool,
    link: bool,
    search: bool,
    get_memory: bool,
    lineage: bool,
    list_edge_types: bool,
    goals: bool,
    code: bool,
}

impl Surface {
    fn from_advertised(advertised: &BTreeSet<&str>) -> Self {
        let has = |id: &str| advertised.contains(id);
        Self {
            remember: has(RememberTool::NAME),
            derive: has(DeriveTool::NAME),
            link: has(LinkTool::NAME),
            search: has(SearchMemoriesTool::NAME),
            get_memory: has(GetMemoryTool::NAME),
            lineage: has(WalkMemoryLineageTool::NAME),
            list_edge_types: has(ListEdgeTypesTool::NAME),
            goals: has(GoalSetTool::NAME) || has(GoalDecomposeTool::NAME),
            code: has(CODE_SEARCH_CHUNKS) || has(CODE_REGISTER_REPO),
        }
    }
}

/// Build the tight `initialize` instructions string for the given advertised
/// tool set. One–two dense paragraphs; sections appear only when their tools
/// are exposed. Returns an empty string when no memory-authoring or -reading
/// tools are advertised (nothing useful to say).
#[must_use]
pub fn build_instructions(advertised: &BTreeSet<&str>) -> String {
    let s = Surface::from_advertised(advertised);
    if !s.remember && !s.derive && !s.search {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(
        "Proxima is a shared-memory substrate, not an agent. It stores memory in three layers \
         — Facts (immutable observations) → Abstractions (patterns over Facts) → Perspectives \
         (stances / self-models) — and YOU are the cognition that moves between them: nothing \
         derives or reflects for you. ",
    );

    if s.derive {
        out.push_str("HARD LAW: Facts cannot link Facts. ");
        if s.link {
            out.push_str(
                "`core_link` authors edges only from an Abstraction or Perspective; a Fact \
                 source is rejected at storage (\"rejects source kind Fact\"). ",
            );
        }
        out.push_str(
            "To relate Facts you do not link them — you `core_derive` an Abstraction (or \
             Perspective) over them and pass their handles as `source_handles`, which \
             auto-creates the `derived-from` provenance edges. That IS how the graph is built; \
             the urge to connect two Facts is the signal to abstract, not to link. ",
        );
    }

    if s.remember || s.derive {
        if s.remember {
            out.push_str("`core_remember` appends a Fact (an observation). ");
        }
        if s.derive {
            out.push_str(
                "`core_derive` with kind=Abstraction captures a generalization over ≥2 Facts, \
                 kind=Perspective records a stance or self-model — never store a generalization \
                 as a Fact, it loses its grounding. ",
            );
        }
        if s.goals {
            out.push_str(
                "Set intent that drives memory with `core_goal_set` (decompose with \
                 `core_goal_decompose`). ",
            );
        }
        out.push_str(
            "Pass `idempotency_key` on any write you might replay (imports, re-runs) so replays \
             are no-ops, not duplicates; tag consistently (domain + kind) and pass `model_id` as \
             your operator label. ",
        );
    }

    if s.search {
        out.push_str("`core_search_memories` (hybrid) is the primary read");
        if s.get_memory {
            out.push_str(", then `core_get_memory` (`expand_neighbors: true`) for one entity");
        }
        out.push_str(". ");
        if s.lineage {
            out.push_str(
                "Lineage / citation tools (`core_walk_memory_lineage`, …) are power tools, not \
                 first reach. ",
            );
        }
        out.push_str(
            "Semantic search needs embeddings, which drain in-process when an embedding client \
             is configured. ",
        );
    }

    if s.code {
        out.push_str(
            "Source code is memory too: the `proxima-code_*` tools search and open indexed \
             repository revisions. ",
        );
    }

    out.push_str(
        "Recall before you act, and consolidate at natural breaks. Full playbook with worked \
         examples, the edge-class table, and the read-tool decision guide: read the \
         `proxima://how-to` resource.",
    );

    out
}

/// Build the fuller How-To playbook (the `proxima://how-to` resource body),
/// profile-trimmed to the advertised tool set.
#[must_use]
pub fn how_to_markdown(advertised: &BTreeSet<&str>) -> String {
    let s = Surface::from_advertised(advertised);
    let mut out = String::new();

    out.push_str("# Proxima shared-brain — how to use it\n\n");
    out.push_str(
        "Proxima is the **substrate**: storage, embeddings, the \
         Fact → Abstraction → Perspective structure, goals, retrieval. **You are the \
         cognition.** There is no autonomous engine deriving things for you — when you remember, \
         abstract, or reflect, *you* are the operator. The brain only helps if you query it \
         before acting and feed it as you learn.\n\n",
    );
    out.push_str("## The model: three layers\n\n");
    out.push_str(
        "- **Fact** — an immutable observation; something that happened or that you learned. \
         Wire handle `F:<uuid>`.\n\
         - **Abstraction** — a pattern, generalization, or lesson over ≥2 Facts. Wire handle \
         `A:<uuid>`.\n\
         - **Perspective** — a stance or self-model (\"how I see X\", \"who I am\"). Wire handle \
         `I:<uuid>` (identity-bearing).\n\n",
    );

    push_law(&mut out, s);
    push_capture_table(&mut out, s);
    push_edges(&mut out, s);
    push_worked_example(&mut out, s);
    push_reading(&mut out, s);

    out.push_str("## Recall before acting\n\n");
    out.push_str(
        "Before architectural decisions, domain shifts, or debugging unfamiliar areas, search \
         the brain first and pull your Perspective plus the relevant Facts. Memory you never \
         query is dead weight. Consolidate at natural breaks: remember the key Facts, derive an \
         Abstraction when a pattern recurred, update your Perspective when your stance shifts.\n",
    );

    out
}

fn push_law(out: &mut String, s: Surface) {
    if !s.derive {
        return;
    }
    out.push_str("## The one hard law: Facts cannot link Facts\n\n");
    if s.link {
        out.push_str(
            "`core_link` authors edges **only from an Abstraction or Perspective**. A Fact \
             source is rejected at storage: `relation core/agent-link-refers-to rejects source \
             kind Fact`. Facts are immutable observations — they do not interpret each other.\n\n",
        );
    }
    out.push_str("**To relate Facts, do not link them — derive over them:**\n\n");
    out.push_str("```\n");
    out.push_str(
        "core_derive(kind=\"Abstraction\", title=..., body=...,\n\
        \x20           source_handles=[\"F:aaaa\", \"F:bbbb\", \"F:cccc\"],\n\
        \x20           model_id=\"<your-operator-label>\")\n",
    );
    out.push_str("```\n\n");
    out.push_str(
        "`source_handles` auto-creates `derived-from` provenance edges from the new \
         Abstraction/Perspective down to each source. **That is the graph.** Wanting to connect \
         two `F:` handles is the signal to *abstract*, not to link.",
    );
    if s.link {
        out.push_str(
            " (`core_link` is for the rarer case of one Abstraction/Perspective pointing at \
             other memories.)",
        );
    }
    out.push_str("\n\n");
}

fn push_capture_table(out: &mut String, s: Surface) {
    out.push_str("## What to capture → which tool\n\n");
    out.push_str("| You want to… | Use |\n|---|---|\n");
    if s.remember {
        out.push_str(
            "| Record an observation / something that happened / a fact you learned | \
             `core_remember` → Fact |\n",
        );
    }
    if s.derive {
        out.push_str(
            "| Capture a recurring pattern / generalization / lesson across ≥2 Facts | \
             `core_derive` kind=**Abstraction**, `source_handles`=those Facts |\n\
             | Record or update a stance / self-model (\"how I see X\", \"who I am\") | \
             `core_derive` kind=**Perspective** |\n\
             | **Relate / connect memories** | derive an Abstraction/Perspective over them — \
             **NOT** a Fact→Fact link |\n",
        );
    }
    if s.goals {
        out.push_str(
            "| Set an intent / objective to pursue | `core_goal_set` (+ `core_goal_decompose`) |\n",
        );
    }
    if s.search {
        out.push_str("| Find prior knowledge | `core_search_memories` (hybrid default)");
        if s.get_memory {
            out.push_str(" → `core_get_memory` (`expand_neighbors: true`)");
        }
        out.push_str(" |\n");
    }
    if s.code {
        out.push_str(
            "| Search / open indexed source code | `proxima-code_search_chunks`, \
             `proxima-code_open_file_revision` |\n",
        );
    }
    out.push_str(
        "\nA generalization stored as a Fact flattens the hierarchy and loses its grounding — \
         derive it instead.\n\n",
    );
}

fn push_edges(out: &mut String, s: Surface) {
    if !s.derive && !s.link {
        return;
    }
    out.push_str("## Edge classes\n\n| Edge | Authored by | Direction |\n|---|---|---|\n");
    if s.derive {
        out.push_str(
            "| `derived-from` | auto, when you pass `source_handles` to `core_derive` | new \
             Abstraction/Perspective → each source memory |\n",
        );
    }
    if s.link {
        out.push_str(
            "| `core/agent-link-refers-to` | `core_link` (source must be an Abstraction or \
             Perspective) | interpreter → referent |\n",
        );
    }
    if s.list_edge_types {
        out.push_str(
            "\nFor the authoritative, live list of edge classes in this deployment, call \
             `core_list_edge_types`.\n",
        );
    }
    out.push('\n');
}

fn push_worked_example(out: &mut String, s: Surface) {
    if !(s.remember && s.derive) {
        return;
    }
    out.push_str("## Worked example: turning the wheel\n\n");
    let _ = writeln!(
        out,
        "1. **Observe.** `core_remember(title=..., body=...)` for each distinct thing that \
         happened → Facts `F:a`, `F:b`, `F:c`."
    );
    let _ = writeln!(
        out,
        "2. **Abstract.** Once a pattern recurs across those Facts: \
         `core_derive(kind=\"Abstraction\", source_handles=[\"F:a\",\"F:b\",\"F:c\"], …)` → \
         `A:d`, with `derived-from` edges to each Fact."
    );
    let _ = writeln!(
        out,
        "3. **Take a stance.** When your view genuinely shifts: \
         `core_derive(kind=\"Perspective\", source_handles=[\"A:d\"], …)` → `I:e`."
    );
    if s.goals {
        let _ = writeln!(
            out,
            "4. **Wire intent.** `core_goal_set(…)` to record what to pursue; \
             `core_goal_decompose` to break it down."
        );
    }
    out.push('\n');
}

fn push_reading(out: &mut String, s: Surface) {
    if !s.search {
        return;
    }
    out.push_str("## Reading: which tool first\n\n");
    out.push_str(
        "1. `core_search_memories` (hybrid) — the default first reach for prior knowledge.\n",
    );
    if s.get_memory {
        out.push_str(
            "2. `core_get_memory` with `expand_neighbors: true` — pull one entity and its \
             immediate graph once search has located it.\n",
        );
    }
    if s.lineage {
        out.push_str(
            "3. `core_walk_memory_lineage` / citation tools — power tools for provenance and \
             deep lineage; reach for these only when you specifically need them.\n",
        );
    }
    out.push_str(
        "\nSemantic ranking needs embeddings; if no embedding client is configured the server \
         degrades to lexical search.\n\n",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_set() -> BTreeSet<&'static str> {
        [
            RememberTool::NAME,
            DeriveTool::NAME,
            LinkTool::NAME,
            SearchMemoriesTool::NAME,
            GetMemoryTool::NAME,
            WalkMemoryLineageTool::NAME,
            ListEdgeTypesTool::NAME,
            GoalSetTool::NAME,
            GoalDecomposeTool::NAME,
            CODE_SEARCH_CHUNKS,
            CODE_REGISTER_REPO,
        ]
        .into_iter()
        .collect()
    }

    /// A `memory`-style profile that keeps authoring + retrieval but, for the
    /// sake of the test, has had goal and code tools denied — standing in for
    /// any execution/personality tools a `full` deployment would carry.
    fn memory_minus_goals_set() -> BTreeSet<&'static str> {
        [
            RememberTool::NAME,
            DeriveTool::NAME,
            LinkTool::NAME,
            SearchMemoriesTool::NAME,
            GetMemoryTool::NAME,
            WalkMemoryLineageTool::NAME,
            ListEdgeTypesTool::NAME,
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn instructions_teach_the_hard_law_and_remember_vs_derive() {
        let s = build_instructions(&full_set());
        assert!(s.contains("Facts cannot link Facts"));
        assert!(s.contains("rejects source kind Fact"));
        assert!(s.contains("source_handles"));
        assert!(s.contains("`core_remember`"));
        assert!(s.contains("`core_derive`"));
        assert!(s.contains("proxima://how-to"));
    }

    #[test]
    fn instructions_are_profile_aware() {
        let full = build_instructions(&full_set());
        assert!(full.contains("core_goal_set"));
        assert!(full.contains("proxima-code_"));

        let trimmed = build_instructions(&memory_minus_goals_set());
        // Dropped tools drop their guidance.
        assert!(!trimmed.contains("core_goal_set"));
        assert!(!trimmed.contains("goal"));
        assert!(!trimmed.contains("proxima-code_"));
        // Core memory contract still present.
        assert!(trimmed.contains("Facts cannot link Facts"));
        assert!(trimmed.contains("`core_remember`"));
    }

    #[test]
    fn instructions_never_name_execution_or_personality_tools() {
        // Regression guard for acceptance #2: no profile's instructions may
        // reference tools outside the memory contract.
        for set in [full_set(), memory_minus_goals_set()] {
            let s = build_instructions(&set);
            for forbidden in [
                "instantiate_personality",
                "add_wake_entry",
                "emit_execution_request",
                "wake_execute",
            ] {
                assert!(!s.contains(forbidden), "instructions leaked {forbidden}");
            }
        }
    }

    #[test]
    fn instructions_without_link_tool_omit_link_specifics() {
        let mut set = full_set();
        set.remove(LinkTool::NAME);
        let s = build_instructions(&set);
        // The law (derive over Facts) survives; the core_link specifics don't.
        assert!(s.contains("Facts cannot link Facts"));
        assert!(!s.contains("`core_link`"));
    }

    #[test]
    fn instructions_empty_when_no_memory_tools() {
        let set: BTreeSet<&str> = [ListEdgeTypesTool::NAME].into_iter().collect();
        assert!(build_instructions(&set).is_empty());
    }

    #[test]
    fn how_to_documents_law_examples_and_decision_guide() {
        let s = how_to_markdown(&full_set());
        assert!(s.contains("Facts cannot link Facts"));
        assert!(s.contains("derived-from"));
        assert!(s.contains("core_derive(kind=\"Abstraction\""));
        assert!(s.contains("## What to capture → which tool"));
        assert!(s.contains("## Edge classes"));
        assert!(s.contains("## Worked example"));
        assert!(s.contains("## Reading: which tool first"));
    }

    #[test]
    fn how_to_is_profile_aware() {
        let trimmed = how_to_markdown(&memory_minus_goals_set());
        assert!(!trimmed.contains("core_goal_set"));
        assert!(!trimmed.contains("proxima-code_"));
        // Layering law + relate-memories row still taught.
        assert!(trimmed.contains("Facts cannot link Facts"));
        assert!(trimmed.contains("Relate / connect memories"));
    }
}
