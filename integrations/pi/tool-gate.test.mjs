import assert from "node:assert/strict";
import { test } from "node:test";
import toolGate, { COMMAND, PROTOCOL } from "./tool-gate.js";

function harness(confirm) {
  let handler;
  const notices = [];
  let marker;
  toolGate({ on(name, callback) { assert.equal(name, "tool_call"); handler = callback; },
    registerCommand(name, definition) { marker = { name, definition }; } });
  const ctx = { mode: "rpc", hasUI: true, ui: {
    confirm, notify(message) { notices.push(JSON.parse(message)); },
  } };
  return { call: (ctxOverride = {}) => handler({ toolCallId: "call-1", toolName: "write" }, { ...ctx, ...ctxOverride }), notices, marker };
}

test("execution stays pending until an explicit matching boolean approval", async () => {
  let respond;
  const h = harness((title, message, options) => {
    assert.equal(title, PROTOCOL);
    assert.deepEqual(JSON.parse(message), { tool_call_id: "call-1", tool_name: "write" });
    assert.equal(options.timeout, 5000);
    return new Promise((resolve) => { respond = resolve; });
  });
  assert.equal(h.marker.name, COMMAND);
  let returned = false;
  const result = h.call().then((value) => { returned = true; return value; });
  await Promise.resolve();
  assert.equal(returned, false);
  respond(true);
  assert.equal(await result, undefined);
  assert.equal(h.notices[0].allowed, true);
});

test("denial, timeout defaults and invalid replies block without executing", async () => {
  for (const reply of [false, undefined, null, "true", 1]) {
    const h = harness(async () => reply);
    assert.equal((await h.call()).block, true);
    assert.equal((await h.call()).terminate, true);
    assert.ok(h.notices.every((notice) => notice.allowed === false));
  }
});

test("a refused run cannot reopen on a later affirmative reply", async () => {
  let calls = 0;
  const h = harness(async () => calls++ > 0);
  assert.equal((await h.call()).block, true);
  assert.equal((await h.call()).block, true);
});

test("non-RPC mode, cancellation and transport errors fail closed", async () => {
  const h = harness(async () => true);
  assert.equal((await h.call({ mode: "json", hasUI: false })).block, true);
  const cancelled = harness(async () => true);
  assert.equal((await cancelled.call({ signal: AbortSignal.abort() })).block, true);
  const broken = harness(async () => { throw new Error("transport unavailable"); });
  assert.equal((await broken.call()).block, true);
});
