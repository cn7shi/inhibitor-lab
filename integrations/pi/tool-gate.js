// Only the matching Rust supervisor should answer this owned RPC subprotocol.
export const PROTOCOL = "inhibitor-tool-gate/v1";
export const COMMAND = "inhibitor-gate-v1";

export default function toolGate(pi) {
  let latched = false;
  pi.on("tool_call", async (event, ctx) => {
    const blocked = { block: true, terminate: true, reason: "Inhibitor tool gate denied execution" };
    if (ctx.mode !== "rpc" || !ctx.hasUI) {
      latched = true;
      return blocked;
    }
    try {
      const reply = await ctx.ui.confirm(PROTOCOL, JSON.stringify({
        tool_call_id: event.toolCallId, tool_name: event.toolName,
      }), { timeout: 5000, signal: ctx.signal });
      const allowed = !latched && !ctx.signal?.aborted && reply === true;
      if (!allowed) latched = true;
      ctx.ui.notify(JSON.stringify({ protocol: PROTOCOL,
        tool_call_id: event.toolCallId, tool_name: event.toolName, allowed }), "info");
      return allowed ? undefined : blocked;
    } catch {
      latched = true;
      return blocked;
    }
  });
  // A marker queried before the supervisor submits any model prompt.
  pi.registerCommand(COMMAND, { description: PROTOCOL, handler: async () => {} });
}
