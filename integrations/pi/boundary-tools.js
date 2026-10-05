// Test-only tools. Loaded explicitly by check-boundaries.mjs in an isolated workspace.
import { Type } from "@earendil-works/pi-ai";

const result = (details) => ({
  content: [{ type: "text", text: "Fixture collected nested outcomes; this is not a task-success judgement." }],
  details,
  terminate: true,
});

export default function boundaryTools(pi) {
  pi.registerTool({
    name: "lab_nested", label: "Nested boundary fixture",
    description: "Run the deterministic two-level nested admission fixture.",
    parameters: Type.Object({}),
    async execute(_id, _params, signal, _onUpdate, ctx) {
      const child = await ctx.executeTool("lab_sequence", {}, { signal });
      return result({ child_id: child.toolCall.id, child_failed: child.isError, sequence: child.result.details });
    },
  });
  pi.registerTool({
    name: "lab_sequence", label: "Nested sequence fixture",
    description: "Read two missing files, then attempt two writes even after the first refusal.",
    parameters: Type.Object({}),
    async execute(_id, _params, signal, _onUpdate, ctx) {
      const outcomes = [];
      // Deliberately attempt a second new call after a refusal to check the closed latch.
      for (const [name, args] of [
        ["read", { path: "missing-a.txt" }],
        ["read", { path: "missing-b.txt" }],
        ["write", { path: "nested-first.txt", content: "boundary-proof\n" }],
        ["write", { path: "nested-retry.txt", content: "boundary-proof\n" }],
      ]) {
        const outcome = await ctx.executeTool(name, args, { signal });
        outcomes.push({ id: outcome.toolCall.id, name, failed: outcome.isError });
      }
      return result({ outcomes });
    },
  });
}
