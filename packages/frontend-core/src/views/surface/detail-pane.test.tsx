import { cleanup, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";
import type { DecodedMemory, MemoryProvenance } from "../../graph-store";
import { createHub } from "../../hub";
import { clearRegistriesForTests } from "../../registry";
import { DetailPane } from "./detail-pane";

afterEach(() => { cleanup(); clearRegistriesForTests(); });

const memory = (): DecodedMemory => ({
  row: {
    id: "m1", kind: "Fact",
    schema_id: "proxima-code/code-chunk-v1", schema_version: 1,
    owner: { principal: { User: "u" }, org_id: "o" },
    payload: [1, 2, 3, 4, 5],
  },
  payload: { state: "Present", chunk: 1, type: "block", language: "rust" },
});

const prov: MemoryProvenance = {
  creating_seq: "01ARYZ6S41TS5G7QFC0V44N5KH",
  authoring_personality_instance_id: "personality-rust",
  written_at_ms: 1469918176385,
};

describe("DetailPane", () => {
  it("renders header, payload, lineage, and metadata blocks", () => {
    render(() => (
      <DetailPane
        memory={memory()}
        provenance={prov}
        lineage={{ outbound: [], inbound: [] }}
        flavor="proxima-code"
        hub={createHub([])}
      />
    ));
    expect(screen.getByText(/PAYLOAD/)).not.toBeNull();
    expect(screen.getByText(/LINEAGE/)).not.toBeNull();
    expect(screen.getByText(/METADATA/)).not.toBeNull();
    expect(screen.getAllByText(/personality-rust/)).toHaveLength(2);
    expect(screen.getAllByText(/proxima-code\/code-chunk-v1/)).toHaveLength(2);
  });

  it("falls back to flat key/value when no renderer registered", () => {
    render(() => (
      <DetailPane
        memory={memory()}
        provenance={prov}
        lineage={{ outbound: [], inbound: [] }}
        flavor="proxima-code"
        hub={createHub([])}
      />
    ));
    expect(screen.getByText("state")).not.toBeNull();
    expect(screen.getByText("Present")).not.toBeNull();
    expect(screen.getByText("language")).not.toBeNull();
    expect(screen.getByText("rust")).not.toBeNull();
  });

  it("renders 1-hop lineage groups", () => {
    render(() => (
      <DetailPane
        memory={memory()}
        provenance={prov}
        lineage={{
          outbound: [{ relation: "informs", target_kind: "Abstraction", target_schema_id: "schema-b", count: 2 }],
          inbound: [],
        }}
        flavor="proxima-code"
        hub={createHub([])}
      />
    ));
    expect(screen.getByText(/informs/)).not.toBeNull();
    expect(screen.getByText(/schema-b/)).not.toBeNull();
    expect(screen.getByText(/×2/)).not.toBeNull();
  });
});
