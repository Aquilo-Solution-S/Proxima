import { describe, expect, it } from "vitest";
import type { PersonalityInstanceTs, ProducesTs } from "../../bindings";
import { computeLayout } from "./layout";
import {
  paletteKey,
  type BuildModelInput,
  type CanvasNode,
  type ProducesByPaletteKey,
} from "./types";

const owner = {
  principal: { User: "00000000-0000-0000-0000-000000000000" },
  org_id: "00000000-0000-0000-0000-000000000000",
} as const;

const wakeEntry = (
  overrides: Partial<PersonalityInstanceTs["wake_entries"][number]> = {},
): PersonalityInstanceTs["wake_entries"][number] => ({
  wake_entry_id: "11111111-1111-7111-8111-111111111111",
  trigger_kind: "on_memory",
  trigger_id: "proxima-code/commit-summary-v1",
  label: "react-to-commit",
  enabled: true,
  execution_mode: "substrate_only",
  authored_by: "any",
  probability_promille: 1000,
  recipe_ref: "user:default.yaml",
  model_tier: "standard",
  inference_target_ref: null,
  substrate_tool_palette: [],
  workspace_tool_palette: [],
  max_rounds: 4,
  disabled_reason: null,
  ...overrides,
});

const instance = (
  overrides: Partial<PersonalityInstanceTs> = {},
): PersonalityInstanceTs => ({
  owner,
  personality_instance_id: "018f0000-0000-7000-8000-000000000001",
  current_root_perspective_memory_id: "018f0000-0000-7000-8000-000000000101",
  display_name: "Engineer",
  status: "active",
  wake_entries: [wakeEntry()],
  ...overrides,
});

const buildInput = (
  instances: PersonalityInstanceTs[],
  producesEntries: Array<[string[], ProducesTs]> = [],
): BuildModelInput => {
  const map: ProducesByPaletteKey = new Map();
  for (const [palette, produces] of producesEntries) {
    map.set(paletteKey(palette), produces);
  }
  return { instances, producesByPaletteKey: map };
};

describe("computeLayout — produces edges and relation nodes", () => {
  it("emits no produces edges when an entry's palette is empty (terminal entry)", async () => {
    const model = await computeLayout(buildInput([instance()]));
    expect(model.edges.every((e) => e.kind !== "produces")).toBe(true);
  });

  it("emits a relation node when an entry's palette contains create_edge", async () => {
    const model = await computeLayout(
      buildInput(
        [
          instance({
            wake_entries: [
              wakeEntry({ substrate_tool_palette: ["core/create_edge"] }),
            ],
          }),
        ],
        [
          [
            ["core/create_edge"],
            { schema_ids: [], relation_ids: ["core/derived-from"] },
          ],
        ],
      ),
    );
    const relationNodes = model.nodes.filter(
      (n: CanvasNode) => n.kind === "relation",
    );
    expect(relationNodes.length).toBe(1);
    expect(relationNodes[0].data).toEqual({
      kind: "relation",
      relation_id: "core/derived-from",
    });
  });

  it("emits a produces edge from each entry to each schema it could write", async () => {
    const model = await computeLayout(
      buildInput(
        [
          instance({
            wake_entries: [
              wakeEntry({
                substrate_tool_palette: ["core/emit_abstraction"],
              }),
            ],
          }),
        ],
        [
          [
            ["core/emit_abstraction"],
            {
              schema_ids: ["proxima-code/commit-summary-v1"],
              relation_ids: [],
            },
          ],
        ],
      ),
    );
    const produces = model.edges.filter((e) => e.kind === "produces");
    expect(produces.length).toBe(1);
    expect(produces[0].shape_id).toBe("proxima-code/commit-summary-v1");
  });

  it("loop closure: a schema that's both a trigger and produced renders as one node with both edges", async () => {
    // Entry W1 triggers on `proxima-code/commit-summary-v1`.
    // Entry W2 has emit_abstraction in its palette and could write
    // `proxima-code/commit-summary-v1`. The single schema node should
    // have both an outgoing trigger edge (to W1) and an incoming
    // produces edge (from W2).
    const model = await computeLayout(
      buildInput(
        [
          instance({
            display_name: "Engineer",
            personality_instance_id: "018f0000-0000-7000-8000-000000000001",
            wake_entries: [
              wakeEntry({
                wake_entry_id: "aaaaaaaa-aaaa-7aaa-8aaa-aaaaaaaaaaaa",
                trigger_id: "proxima-code/commit-summary-v1",
                substrate_tool_palette: [],
              }),
              wakeEntry({
                wake_entry_id: "bbbbbbbb-bbbb-7bbb-8bbb-bbbbbbbbbbbb",
                trigger_id: "proxima-code/code-chunk-v1",
                substrate_tool_palette: ["core/emit_abstraction"],
              }),
            ],
          }),
        ],
        [
          [[], { schema_ids: [], relation_ids: [] }],
          [
            ["core/emit_abstraction"],
            {
              schema_ids: ["proxima-code/commit-summary-v1"],
              relation_ids: [],
            },
          ],
        ],
      ),
    );
    const schemaNodes = model.nodes.filter(
      (n) =>
        n.kind === "schema" &&
        (n.data as { schema_id: string }).schema_id ===
          "proxima-code/commit-summary-v1",
    );
    expect(schemaNodes.length).toBe(1);
    const schemaNodeId = schemaNodes[0].id;
    const triggers = model.edges.filter(
      (e) => e.kind === "trigger" && e.source === schemaNodeId,
    );
    const produces = model.edges.filter(
      (e) => e.kind === "produces" && e.target === schemaNodeId,
    );
    expect(triggers.length).toBe(1);
    expect(produces.length).toBe(1);
  });
});
