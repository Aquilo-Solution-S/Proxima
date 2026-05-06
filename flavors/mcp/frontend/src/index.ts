import { mcpPayloadCodec, mcpRenderers } from "./payload-renderers";
import { registerEdgeStyle, registerPayloadRenderer } from "@proxima/core/registry";

export function init(): void {
  for (const [schemaId, renderer] of Object.entries(mcpRenderers)) {
    registerPayloadRenderer({
      schemaId,
      schemaVersion: 1,
      flavor: "proxima-mcp",
      codec: mcpPayloadCodec,
      renderer,
    });
  }
  registerEdgeStyle({
    relationId: "proxima-mcp/agent-link-refers-to",
    style: {
      color: 0x77c8a4,
      highlightColor: 0xc5f5df,
      opacity: 0.34,
    },
  });
}
