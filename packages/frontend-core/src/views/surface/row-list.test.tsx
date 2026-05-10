import { cleanup, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import type { DecodedMemory } from "../../graph-store";
import { RowList } from "./row-list";

afterEach(cleanup);

const memory = (id: string, kind: DecodedMemory["row"]["kind"], schemaId: string): DecodedMemory => ({
  row: {
    id, kind, schema_id: schemaId, schema_version: 1,
    owner: { principal: { User: "00000000-0000-0000-0000-000000000000" }, org_id: "00000000-0000-0000-0000-000000000000" },
    payload: [1, 2, 3, 4],
  },
  payload: {},
});

describe("RowList", () => {
  it("shows pillar badge column on All tab", () => {
    render(() => (
      <RowList
        rows={[memory("m1", "Fact", "schema-a")]}
        provenance={new Map()}
        activeTab="All"
        selectedId={null}
        onSelect={() => {}}
      />
    ));
    expect(screen.getByText("F")).not.toBeNull();
    expect(screen.getByText("schema-a")).not.toBeNull();
  });

  it("hides pillar badge column on per-pillar tabs", () => {
    render(() => (
      <RowList
        rows={[memory("m1", "Fact", "schema-a")]}
        provenance={new Map()}
        activeTab="Fact"
        selectedId={null}
        onSelect={() => {}}
      />
    ));
    expect(screen.queryByText("F")).toBeNull();
    expect(screen.getByText("schema-a")).not.toBeNull();
  });

  it("invokes onSelect when a row is clicked", () => {
    let chosen = "";
    render(() => (
      <RowList
        rows={[memory("m1", "Fact", "schema-a")]}
        provenance={new Map()}
        activeTab="All"
        selectedId={null}
        onSelect={(id) => { chosen = id; }}
      />
    ));
    screen.getByText("schema-a").closest("[role='row']")?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(chosen).toBe("m1");
  });
});
