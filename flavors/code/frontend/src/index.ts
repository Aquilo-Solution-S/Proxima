import { CodeView } from "./code-view";
import { codePayloadCodec, codeRenderers } from "./payload-renderers";
import { registerEdgeStyle, registerPayloadRenderer, registerShellView } from "@proxima/core/registry";

export { CodeView };
export { ReposPanel } from "./repos-panel";

export function init(): void {
  for (const [schemaId, renderer] of Object.entries(codeRenderers)) {
    registerPayloadRenderer({
      schemaId,
      schemaVersion: 1,
      flavor: "proxima-code",
      codec: codePayloadCodec,
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
