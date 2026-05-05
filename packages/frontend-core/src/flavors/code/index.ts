import type { FlavorScope } from "../../hub";
import { Mono } from "../../primitives";
import { CodeView } from "./code-view";

export { CodeView };
export { ReposPanel } from "./repos-panel";

const rawCborCodec = {
  decode(bytes: Uint8Array): Uint8Array {
    return bytes;
  },
  encode(value: Uint8Array): Uint8Array {
    return value;
  },
};

const codeSchemas = [
  "proxima-code/commit-v1",
  "proxima-code/file-revision-v1",
  "proxima-code/code-chunk-v1",
  "proxima-code/commit-summary-v1",
  "proxima-code/calls",
] as const;

export function registerCode(scope: FlavorScope): void {
  for (const schemaId of codeSchemas) {
    scope.registerCodec(schemaId, 1, rawCborCodec);
    scope.registerRenderer<Uint8Array>(schemaId, 1, {
      render: (props) =>
        Mono({
          style: { "font-size": "10px", color: "var(--ink-50)" },
          children: `${props.payload.byteLength} CBOR bytes`,
        }),
    });
  }
  scope.registerView({
    id: "code",
    label: "Code",
    component: CodeView,
  });
}
