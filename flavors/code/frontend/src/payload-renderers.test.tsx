import { cleanup, render, screen } from "@solidjs/testing-library";
import { encode } from "cbor-x";
import { afterEach, describe, expect, it } from "vitest";
import type { MemoryRow } from "@proxima/core";
import { clearRegistriesForTests } from "@proxima/core/registry";
import { createHub } from "@proxima/core/hub";
import { init } from "./index";

const owner = {
  principal: { User: "00000000-0000-0000-0000-000000000000" },
  org_id: "00000000-0000-0000-0000-000000000000",
};

const memory = (schemaId: string, payload: number[]): MemoryRow => ({
  id: "019df9e1-cb61-7031-8e93-6facbe711cb2",
  kind: "Fact",
  schema_id: schemaId,
  schema_version: 1,
  owner,
  payload,
});

describe("code payload renderers", () => {
  afterEach(() => {
    cleanup();
    clearRegistriesForTests();
  });

  it("decodes and renders file-revision-v1 as typed fields", () => {
    init();
    const hub = createHub([]);
    const payload = {
      repo_id: "018f0000-0000-7000-8000-000000000001",
      file_path: "src/lib.rs",
      language: "Rust",
      content_sha256: new Uint8Array(32).fill(11),
      size_bytes: 194,
      indexed_commit_sha: "0123456789abcdef0123456789abcdef01234567",
      state: "Present",
    };
    const row = memory(
      "proxima-code/file-revision-v1",
      Array.from(encode(payload)),
    );

    const decoded = hub
      .codecFor(row.schema_id, row.schema_version)
      ?.decode(new Uint8Array(row.payload));
    const renderer = hub.rendererFor(row.schema_id, row.schema_version);

    render(() => renderer?.render({ memory: row, payload: decoded }));

    expect(screen.getByText("src/lib.rs")).toBeTruthy();
    expect(screen.getByText("194 bytes")).toBeTruthy();
    expect(screen.getByText("Present")).toBeTruthy();
    expect(screen.queryByText(/CBOR bytes/)).toBeNull();
  });

  it("highlights code chunks using the payload language", () => {
    init();
    const hub = createHub([]);
    const payload = {
      repo_id: "018f0000-0000-7000-8000-000000000001",
      file_path: "src/main.rs",
      language: "Rust",
      text: "fn main() {\n    let answer = 42;\n}\n",
      line_range_start: 1,
      line_range_end: 3,
      byte_range_start: 0,
      byte_range_end: 35,
      chunk_index: 0,
      chunk_type: "function",
      state: "Present",
    };
    const row = memory(
      "proxima-code/code-chunk-v1",
      Array.from(encode(payload)),
    );

    const decoded = hub
      .codecFor(row.schema_id, row.schema_version)
      ?.decode(new Uint8Array(row.payload));
    const renderer = hub.rendererFor(row.schema_id, row.schema_version);

    const { container } = render(() =>
      renderer?.render({ memory: row, payload: decoded }),
    );

    expect(container.querySelector("code.language-rust")).toBeTruthy();
    expect(container.querySelector(".hljs-keyword")?.textContent).toBe("fn");
  });
});
