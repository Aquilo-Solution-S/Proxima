import type { FlavorScope } from "../../hub";
import { CodeView } from "./code-view";
import { codePayloadCodec, codeRenderers } from "./payload-renderers";

export { CodeView };
export { ReposPanel } from "./repos-panel";

export function registerCode(scope: FlavorScope): void {
  for (const [schemaId, renderer] of Object.entries(codeRenderers)) {
    scope.registerCodec(schemaId, 1, codePayloadCodec);
    scope.registerRenderer(schemaId, 1, renderer);
  }
  scope.registerView({
    id: "code",
    label: "Code",
    component: CodeView,
  });
}
