//! Server self-documentation: the profile-aware `instructions` string
//! returned at `initialize` and the `proxima://how-to` resource body.
//!
//! Both are generated from the *resolved* tool/resource set — the same
//! `authz.capabilities.tool_scope`-filtered descriptor lists that
//! `list_tools`, `resources/list`, and `resources/templates/list` advertise —
//! so the How-To never references a surface a given deployment profile
//! (`PROXIMA_TOOL_PROFILE` plus allow/deny) does not expose. A `memory`
//! deployment that drops code execution tools drops their guidance
//! too, automatically.
//!
//! The generators are pure functions of the advertised canonical tool-id and
//! resource scope-key sets, which makes them unit-testable without a database
//! or transport.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use proxima_core::mcp::McpTool;
use proxima_core::mcp::core_tools::{
    CoreGoalTool, DeriveTool, InterpretTool, MemorySpacesTool, RememberTool, SearchMemoriesTool,
};
use proxima_core::protocol::resource as protocol_resource;

/// Canonical URI of the on-demand How-To resource.
pub const HOW_TO_URI: &str = "proxima://how-to";
/// Machine name of the How-To resource (used in `resources/list`).
pub const HOW_TO_NAME: &str = "proxima-how-to";
/// Human title of the How-To resource.
pub const HOW_TO_TITLE: &str = "Proxima shared-brain: how to use it";
/// One-line description of the How-To resource.
pub const HOW_TO_DESCRIPTION: &str = "The Proxima memory contract in depth: the Fact/Abstraction/Perspective \
     layering law, remember-vs-derive-vs-interpret, worked examples, the edge-kind \
     table, and the read-resource decision guide.";
/// MIME type of the How-To resource body.
pub const HOW_TO_MIME: &str = "text/markdown";

// Code-flavor tools live in `proxima-code`, which `mcp-server` does not
// depend on, so these ids are matched as literals rather than via a
// `McpTool::NAME` const. They stay correct as long as the flavor keeps these
// registered tool names (the `memory` keep set in `apps/proxima-mcp` pins them too).
const CODE_SEARCH_CHUNKS: &str = "proxima-code_search_chunks";
const CODE_REGISTER_REPO: &str = "proxima-code_register_repo";
/// The advertised surface, distilled to the booleans the generators key off.
/// Computed once from the resolved tool-id and resource scope-key sets so
/// `build_instructions` and `how_to_markdown` agree on what is exposed. The
/// fields are independent surface-presence flags, not a state machine — an
/// enum would not model them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy)]
struct Surface {
    remember: bool,
    derive: bool,
    interpret: bool,
    search: bool,
    memory_spaces: bool,
    get_memory: bool,
    lineage: bool,
    goals: bool,
    code: bool,
}

impl Surface {
    fn from_advertised(
        advertised_tools: &BTreeSet<&str>,
        advertised_resources: &BTreeSet<&str>,
    ) -> Self {
        let has_tool = |id: &str| advertised_tools.contains(id);
        let has_resource = |id: &str| advertised_resources.contains(id);
        Self {
            remember: has_tool(RememberTool::NAME),
            derive: has_tool(DeriveTool::NAME),
            interpret: has_tool(InterpretTool::NAME),
            search: has_tool(SearchMemoriesTool::NAME),
            memory_spaces: has_tool(MemorySpacesTool::NAME),
            get_memory: has_resource(protocol_resource::MEMORY),
            lineage: has_resource(protocol_resource::MEMORY_LINEAGE),
            goals: has_tool(CoreGoalTool::NAME),
            code: has_tool(CODE_SEARCH_CHUNKS) || has_tool(CODE_REGISTER_REPO),
        }
    }
}

/// Build the tight `initialize` instructions string for the given advertised
/// tool/resource set. One–two dense paragraphs; sections appear only when
/// their surfaces are exposed. Returns an empty string when no memory-authoring
/// or search tools are advertised (nothing useful to say).
#[must_use]
pub fn build_instructions(
    advertised_tools: &BTreeSet<&str>,
    advertised_resources: &BTreeSet<&str>,
) -> String {
    let s = Surface::from_advertised(advertised_tools, advertised_resources);
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
        out.push_str(
            "HARD LAW — NO TOOL WRITES A CONNECTION. Every edge follows from what a node says — \
             an `origin` entry from the handles a write declares it was made from, a `reference` \
             entry from a schema-declared payload field. ",
        );
        out.push_str(
            "To semantically relate Facts you `core_derive` an Abstraction (or Perspective) over \
             them and pass their handles as `source_handles`, which lands the `origin` entries. ",
        );
        if s.interpret {
            out.push_str(
                "When the claim is a judgment about memories that already exist — a reason and a \
                 confidence — `core_interpret` authors an interpretation Perspective over its \
                 `subjects` and returns a `P:` handle; the connections are that Perspective's own \
                 references. ",
            );
        }
        out.push_str(
            "A Fact never interprets: it is an observation, and an interpretation is a \
             Perspective. The urge to connect two Facts is the signal to abstract. ",
        );
    }

    if s.remember || s.derive {
        if s.memory_spaces {
            out.push_str("In multi-space hosts, call `core_memory_spaces` before durable memory writes. Use a returned `space` key in `core_remember`, `core_record_utterance`, `core_search_memories`, `core_derive`, and `core_interpret`; hydrate a memory by reading `proxima://memory/{id}`. Omitted `space` preserves the current bound owner. A cross-space derivation or interpretation may ground in readable handles outside the selected write space. ");
        }
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
                "Set intent that drives memory with `core_goal` action=set (decompose with \
                 action=decompose). ",
            );
        }
        out.push_str(
            "Pass `idempotency_key` on any write you might replay (imports, re-runs) so replays \
             are no-ops, not duplicates; tag consistently (domain + kind) and pass `model_id` as \
             your operator label. ",
        );
    }

    if s.search {
        out.push_str("`core_search_memories` (hybrid) is the primary recall path");
        if s.get_memory {
            out.push_str(
                ", then fetch one entity from `proxima://memory/{id}` (add \
                 `?expand_neighbors=true` for edges)",
            );
        }
        out.push_str(". ");
        if s.lineage {
            out.push_str(
                "Walk lineage via `proxima://memory/{id}/lineage?direction=ancestors`; use it \
                 only when provenance is the question. ",
            );
        }
        out.push_str("Discover reads with `resources/list` and `resources/templates/list`. ");
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
         examples, the edge-kind table, and the read-resource decision guide: read the \
         `proxima://how-to` resource.",
    );

    out
}

/// Build the fuller How-To playbook (the `proxima://how-to` resource body),
/// profile-trimmed to the advertised tool/resource set.
#[must_use]
pub fn how_to_markdown(
    advertised_tools: &BTreeSet<&str>,
    advertised_resources: &BTreeSet<&str>,
) -> String {
    let s = Surface::from_advertised(advertised_tools, advertised_resources);
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
         `P:<uuid>`.\n\n",
    );

    push_law(&mut out, s);
    if s.memory_spaces {
        out.push_str("## Memory spaces\n\nIn multi-space hosts, call `core_memory_spaces` before durable memory writes. Use a returned `space` key in `core_remember`, `core_record_utterance`, `core_search_memories`, `core_derive`, and `core_interpret`; hydrate a memory by reading `proxima://memory/{id}`. Omitted `space` preserves the current bound owner. Space keys are selectors only; every write/read is re-authorized by the server. A cross-space derivation or interpretation may ground in readable handles outside the selected write space.\n\n");
    }
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
    out.push_str("## The one hard law for agent-authored connections\n\n");
    out.push_str(
        "**No tool writes a connection.** An edge carries no information beyond its existence, \
         so every edge is a consequence of what some node says: an `origin` entry from the \
         handles a write declares it was made from, a `reference` entry from a schema-declared \
         payload field. Nothing you call takes an edge kind as an argument.\n\n",
    );
    out.push_str("**To semantically relate Facts, derive over them:**\n\n");
    out.push_str("```\n");
    out.push_str(
        "core_derive(kind=\"Abstraction\", title=..., body=...,\n\
        \x20           source_handles=[\"F:aaaa\", \"F:bbbb\", \"F:cccc\"],\n\
        \x20           model_id=\"<your-operator-label>\")\n",
    );
    out.push_str("```\n\n");
    out.push_str(
        "`source_handles` lands `origin` entries from the new Abstraction/Perspective down to \
         each source. **That is the graph.** Wanting to connect two `F:` handles is the signal \
         to *abstract*.",
    );
    if s.interpret {
        out.push_str(
            "\n\n**A claim about memories that already exist is a Perspective, not an edge:**\n\n",
        );
        out.push_str("```\n");
        out.push_str(
            "core_interpret(claim=\"the outage followed the deploy\", confidence=80,\n\
            \x20              subjects=[\"F:aaaa\", \"A:bbbb\"])\n",
        );
        out.push_str("```\n\n");
        out.push_str(
            "It returns a `P:` handle. A reason and a confidence are a judgment, and a judgment \
             is a Perspective; its subjects become that Perspective's own references. A Fact \
             never interprets — layering refuses a Fact as an interpretation source.",
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
             there is no connect verb |\n",
        );
    }
    if s.interpret {
        out.push_str(
            "| Claim what existing memories mean, with a confidence | `core_interpret` → \
             interpretation Perspective |\n",
        );
    }
    if s.goals {
        out.push_str(
            "| Set an intent / objective to pursue | `core_goal` action=set (+ action=decompose) |\n",
        );
    }
    if s.search {
        out.push_str("| Find prior knowledge | `core_search_memories` (hybrid default)");
        if s.get_memory {
            out.push_str(" → `proxima://memory/{id}` (`?expand_neighbors=true`)");
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
    if !s.derive && !s.interpret {
        return;
    }
    out.push_str(
        "## Edge kinds\n\nTwo kinds, and the vocabulary is closed. The kind follows the write \
         that produced the row; nobody picks one.\n\n",
    );
    out.push_str("| Kind | Written by | Direction |\n|---|---|---|\n");
    if s.derive {
        out.push_str(
            "| `origin` | the write that declares what it was made from — `source_handles` on \
             `core_derive` | new Abstraction/Perspective → each source memory |\n",
        );
    }
    out.push_str(
        "| `reference` | a schema-declared payload field of the node itself | referrer → \
         referent |\n",
    );
    if s.interpret {
        out.push_str(
            "\n`core_interpret` writes no edge of its own: its `subjects` are payload fields of \
             the interpretation Perspective, so they arrive as `reference` entries.\n",
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
         `A:d`, with an `origin` entry to each Fact."
    );
    let _ = writeln!(
        out,
        "3. **Take a stance.** When your view genuinely shifts: \
         `core_derive(kind=\"Perspective\", source_handles=[\"A:d\"], …)` → `P:e`."
    );
    if s.goals {
        let _ = writeln!(
            out,
            "4. **Wire intent.** `core_goal` action=set to record what to pursue; \
             action=decompose to break it down."
        );
    }
    out.push('\n');
}

fn push_reading(out: &mut String, s: Surface) {
    if !s.search {
        return;
    }
    out.push_str("## Reading: which surface first\n\n");
    out.push_str(
        "1. `core_search_memories` (hybrid) — the default first reach for prior knowledge.\n",
    );
    if s.get_memory {
        out.push_str(
            "2. `proxima://memory/{id}` — fetch one entity after search locates it; add \
             `?expand_neighbors=true` for immediate edges.\n",
        );
    }
    if s.lineage {
        out.push_str(
            "3. `proxima://memory/{id}/lineage?direction=ancestors` — walk provenance/deep \
             lineage only when you specifically need it.\n",
        );
    }
    out.push_str(
        "\nDiscover available reads with `resources/list` and `resources/templates/list`. \
         Semantic ranking needs embeddings; if no embedding client is configured the server \
         degrades to lexical search.\n\n",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_tool_set() -> BTreeSet<&'static str> {
        [
            RememberTool::NAME,
            DeriveTool::NAME,
            InterpretTool::NAME,
            SearchMemoriesTool::NAME,
            CoreGoalTool::NAME,
            CODE_SEARCH_CHUNKS,
            CODE_REGISTER_REPO,
        ]
        .into_iter()
        .collect()
    }

    fn full_resource_set() -> BTreeSet<&'static str> {
        [
            protocol_resource::MEMORY,
            protocol_resource::MEMORY_LINEAGE,
        ]
        .into_iter()
        .collect()
    }

    /// A `memory`-style profile that keeps authoring + retrieval but, for the
    /// sake of the test, has had goal and code tools denied.
    fn memory_minus_goals_tool_set() -> BTreeSet<&'static str> {
        [
            RememberTool::NAME,
            DeriveTool::NAME,
            InterpretTool::NAME,
            SearchMemoriesTool::NAME,
        ]
        .into_iter()
        .collect()
    }

    fn memory_minus_goals_resource_set() -> BTreeSet<&'static str> {
        full_resource_set()
    }

    #[test]
    fn instructions_teach_the_hard_law_and_remember_vs_derive() {
        let s = build_instructions(&full_tool_set(), &full_resource_set());
        assert!(s.contains("NO TOOL WRITES A CONNECTION"));
        assert!(s.contains("A Fact never interprets"));
        assert!(s.contains("`core_interpret`"));
        assert!(s.contains("source_handles"));
        assert!(s.contains("`core_remember`"));
        assert!(s.contains("`core_derive`"));
        assert!(s.contains("proxima://memory/{id}"));
        assert!(s.contains("resources/templates/list"));
        assert!(s.contains("proxima://how-to"));
    }

    #[test]
    fn instructions_are_profile_aware() {
        let full = build_instructions(&full_tool_set(), &full_resource_set());
        assert!(full.contains("core_goal"));
        assert!(full.contains("proxima-code_"));

        let trimmed = build_instructions(
            &memory_minus_goals_tool_set(),
            &memory_minus_goals_resource_set(),
        );
        // Dropped tools drop their guidance.
        assert!(!trimmed.contains("core_goal"));
        assert!(!trimmed.contains("goal"));
        assert!(!trimmed.contains("proxima-code_"));
        // Core memory contract still present.
        assert!(trimmed.contains("NO TOOL WRITES A CONNECTION"));
        assert!(trimmed.contains("`core_remember`"));
    }

    #[test]
    fn instructions_never_name_retired_or_denied_tools() {
        // Regression guard for acceptance #2: no profile's instructions may
        // reference tools outside the memory contract.
        for (tools, resources) in [
            (full_tool_set(), full_resource_set()),
            (
                memory_minus_goals_tool_set(),
                memory_minus_goals_resource_set(),
            ),
        ] {
            let s = build_instructions(&tools, &resources);
            for forbidden in [
                "instantiate_personality", // PR9-RATCHET-ALLOW historical denied-tool regression fixture
                "add_wake_entry",
                "emit_execution_request",
                "wake_execute",
                concat!("I", ":"),
                concat!("W", ":"),
            ] {
                assert!(!s.contains(forbidden), "instructions leaked {forbidden}");
            }
        }
    }

    #[test]
    fn instructions_without_interpret_tool_omit_interpret_specifics() {
        let mut tools = full_tool_set();
        tools.remove(InterpretTool::NAME);
        let s = build_instructions(&tools, &full_resource_set());
        // The law (nobody writes an edge) survives; the core_interpret
        // specifics don't.
        assert!(s.contains("NO TOOL WRITES A CONNECTION"));
        assert!(!s.contains("`core_interpret`"));
    }

    /// The retired vocabulary must not survive anywhere in the generated
    /// text: a profile that still advertised `core_link` or an edge-type
    /// catalog would be telling agents to call surfaces that no longer exist.
    #[test]
    fn no_profile_names_the_retired_edge_vocabulary() {
        for (tools, resources) in [
            (full_tool_set(), full_resource_set()),
            (
                memory_minus_goals_tool_set(),
                memory_minus_goals_resource_set(),
            ),
        ] {
            for text in [
                build_instructions(&tools, &resources),
                how_to_markdown(&tools, &resources),
            ] {
                for retired in [
                    "core_link",
                    "edge-types",
                    "core_list_edge_types",
                    "agent-link-refers-to",
                    "derived-from",
                    "relation",
                ] {
                    assert!(!text.contains(retired), "selfdoc leaked {retired}");
                }
            }
        }
    }

    #[test]
    fn instructions_empty_when_no_memory_tools() {
        let tools = BTreeSet::new();
        let resources: BTreeSet<&str> = [protocol_resource::GRAPH].into_iter().collect();
        assert!(build_instructions(&tools, &resources).is_empty());
    }

    #[test]
    fn how_to_documents_law_examples_and_decision_guide() {
        let s = how_to_markdown(&full_tool_set(), &full_resource_set());
        assert!(s.contains("The one hard law for agent-authored connections"));
        assert!(s.contains("**No tool writes a connection.**"));
        assert!(s.contains("`origin`"));
        assert!(s.contains("`reference`"));
        assert!(s.contains("core_derive(kind=\"Abstraction\""));
        assert!(s.contains("core_interpret(claim="));
        assert!(s.contains("## What to capture → which tool"));
        assert!(s.contains("## Edge kinds"));
        assert!(s.contains("## Worked example"));
        assert!(s.contains("## Reading: which surface first"));
        assert!(!s.contains("proxima://edges"));
    }

    #[test]
    fn how_to_is_profile_aware() {
        let trimmed = how_to_markdown(
            &memory_minus_goals_tool_set(),
            &memory_minus_goals_resource_set(),
        );
        assert!(!trimmed.contains("core_goal"));
        assert!(!trimmed.contains("proxima-code_"));
        // Layering law + relate-memories row still taught.
        assert!(trimmed.contains("The one hard law for agent-authored connections"));
        assert!(trimmed.contains("Relate / connect memories"));
    }
}
