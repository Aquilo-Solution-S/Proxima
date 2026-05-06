import type { FlavorScope } from "../../hub";
import { mcpPayloadCodec, mcpRenderers } from "./payload-renderers";

export function registerMcp(scope: FlavorScope): void {
  for (const [schemaId, renderer] of Object.entries(mcpRenderers)) {
    scope.registerCodec(schemaId, 1, mcpPayloadCodec);
    scope.registerRenderer(schemaId, 1, renderer);
  }
}
