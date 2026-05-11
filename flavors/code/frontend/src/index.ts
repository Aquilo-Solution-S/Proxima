import { CodeView } from "./code-view";
import { codeChunkCodec, codePayloadCodec, fileRevisionCodec } from "./codecs";
import { codeRenderers } from "./payload-renderers";
import type { PayloadCodec } from "@proxima/core/hub";
import {
  registerEdgeStyle,
  registerPayloadRenderer,
  registerShellView,
} from "@proxima/core/registry";

export { CodeView };
export { ReposPanel } from "./repos-panel";
export { RunsPanel } from "./runs-panel";

export function init(): void {
  for (const [schemaId, renderer] of Object.entries(codeRenderers)) {
    const codec =
      schemaId === "proxima-code/file-revision-v1"
        ? fileRevisionCodec
        : schemaId === "proxima-code/code-chunk-v1"
          ? codeChunkCodec
          : codePayloadCodec;
    registerPayloadRenderer({
      schemaId,
      schemaVersion: 1,
      flavor: "proxima-code",
      codec: codec as PayloadCodec<unknown>,
      renderer,
    });
  }
  registerEdgeStyle({
    relationId: "proxima-code/calls",
    style: {
      color: 0x6ca6ff,
      highlightColor: 0xb9d7ff,
      opacity: 0.34,
    },
  });
  registerShellView({
    id: "code",
    route: "code",
    label: "Code",
    flavor: "proxima-code",
    component: CodeView,
  });
}
