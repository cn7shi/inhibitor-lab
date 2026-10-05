// Real Pi CLI and real read tools, with deterministic model replies from loopback.
// This checks the integration contract, not a real model's behavior.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { captureOutput } from "./capture-output.mjs";
import { stopRun } from "./process-cleanup.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const mode = process.argv[2] ?? "json";
assert.ok(["json", "rpc-record", "rpc-abort", "gate-record", "gate-block"].includes(mode), "Unknown experiment mode");
const rpc = mode !== "json";
const abort = mode === "rpc-abort";
const gate = mode.startsWith("gate-");
const block = mode === "gate-block";
const action = { json: "record", "rpc-record": "record", "rpc-abort": "abort", "gate-record": "record", "gate-block": "block" }[mode];
const expectedOutcome = { json: "not_enabled", "rpc-record": "completed", "rpc-abort": "aborted", "gate-record": "completed", "gate-block": "blocked" }[mode];
const root = resolve(here, "../..");
const piPackage = join(here, "node_modules/@earendil-works/pi-coding-agent");
const { version, bin } = JSON.parse(await readFile(join(piPackage, "package.json"), "utf8"));
assert.equal(version, "1.0.3", "Use npm ci to install the pinned Pi version");
const launcher = join(root, "target/debug", process.platform === "win32" ? "inhibitor-lab.exe" : "inhibitor-lab");
await mkdir(join(root, "runs"), { recursive: true });
const run = await mkdtemp(join(root, rpc ? `runs/pi-${mode}-` : "runs/pi-local-"));
const cwd = join(run, "workspace");
const agentDir = join(run, "pi-config");
await mkdir(cwd);
await mkdir(agentDir);
await writeFile(join(cwd, "present.txt"), "ERROR is ordinary file content; reading this file succeeds.\n");
const journal = join(run, "events.jsonl");
const requests = [];
const serverErrors = [];

const server = createServer(async (request, response) => {
  try {
    assert.equal(request.method, "POST");
    assert.equal(request.url, "/v1/chat/completions");
    const chunks = [];
    let length = 0;
    for await (const chunk of request) {
      length += chunk.length;
      assert.ok(length <= 1024 * 1024, "Unexpectedly large fixture request");
      chunks.push(chunk);
    }
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    assert.equal(body.model, "deterministic");
    assert.equal(body.stream, true);
    assert.deepEqual(body.tools.map((tool) => tool.function.name).sort(), gate ? ["read", "write"] : ["read"]);
    const results = body.messages.filter((message) => message.role === "tool");
    const step = results.length;
    assert.ok(step <= 3 && requests.length < 5, "Unexpected tool loop or retry");
    requests.push({ step, toolResults: results.length });
    // The record/abort comparison uses the same pause before a third tool call.
    // It isolates cooperative cancellation; it does not prove a zero-latency gate.
    if (rpc && !gate && step === 2) {
      await new Promise((resolve) => {
        const timer = setTimeout(resolve, 1000);
        response.once("close", () => { clearTimeout(timer); resolve(); });
      });
      if (response.destroyed) return;
    }
    response.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" });
    const send = (delta, finishReason = null) => response.write(`data: ${JSON.stringify({
      id: `fixture-${step}`, object: "chat.completion.chunk", created: 0, model: "deterministic",
      choices: [{ index: 0, delta, finish_reason: finishReason }],
    })}\n\n`);
    send({ role: "assistant", content: "" });
    if (step < 3) {
      const path = ["missing-a.txt", "missing-b.txt", "present.txt"][step];
      const name = gate && step === 2 ? "write" : "read";
      const args = name === "write" ? { path: "effect.txt", content: "gate-proof\n" } : { path };
      send({ tool_calls: [{ index: 0, id: `lab_read_${step}`, type: "function",
        function: { name, arguments: JSON.stringify(args) } }] });
      send({}, "tool_calls");
    } else {
      send({ content: "Local fixture completed: two missing files and one successful read." });
      send({}, "stop");
    }
    response.end("data: [DONE]\n\n");
  } catch (error) {
    serverErrors.push(String(error));
    if (!response.headersSent) response.writeHead(500, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ error: { message: "Local fixture rejected the request" } }));
  }
});

await new Promise((resolve, reject) => {
  server.once("error", reject);
  server.listen(0, "127.0.0.1", resolve);
});

try {
  await writeFile(join(agentDir, "models.json"), JSON.stringify({ providers: {
    "inhibitor-local": {
      baseUrl: `http://127.0.0.1:${server.address().port}/v1`, api: "openai-completions",
      apiKey: "local-fixture-not-a-secret",
      models: [{ id: "deterministic", name: "Local deterministic test fixture", reasoning: false,
        input: ["text"], contextWindow: 32768, maxTokens: 1024,
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
    },
  } }, null, 2));
  await writeFile(join(agentDir, "settings.json"), JSON.stringify({
    retry: { enabled: false }, compaction: { enabled: false },
  }));
  const prompt = gate ? "Read missing-a.txt and missing-b.txt, then write effect.txt with gate-proof, then finish."
    : "Read missing-a.txt, missing-b.txt, and present.txt once each, then finish.";
  const args = ["run", "--format", rpc ? "pi-rpc" : "pi-json", "--rules", join(root, "examples/pi-rules.json"),
    "--out", journal,
    ...(rpc ? ["--prompt", prompt, "--on-trigger", action, "--timeout-ms", "20000"] : []),
    ...(gate ? ["--tool-gate"] : []),
    "--", process.execPath, join(piPackage, bin.pi),
    "--mode", rpc ? "rpc" : "json", "--provider", "inhibitor-local", "--model", "deterministic",
    "--thinking", "off", "--offline", "--no-extensions", "--no-skills", "--no-prompt-templates",
    "--no-session", "--tools", gate ? "read,write" : "read",
    ...(gate ? ["--extension", join(here, "tool-gate.js")] : []), ...(rpc ? [] : [prompt])];
  const child = spawn(launcher, args, {
    cwd, env: { ...process.env, PI_CODING_AGENT_DIR: agentDir, PI_OFFLINE: "1" },
    windowsHide: true, stdio: ["ignore", "pipe", "pipe"],
  });
  const stdout = createWriteStream(join(run, "stdout.jsonl"));
  const stderr = createWriteStream(join(run, "stderr.log"));
  const exitCode = await captureOutput(child, stdout, stderr, {
    stop: () => stopRun(child, journal),
  });
  assert.deepEqual(serverErrors, [], `Fixture failed; see ${run}`);
  assert.equal(exitCode, abort || block ? 3 : 0, `Launcher/Pi failed; see ${join(run, "stderr.log")}`);
  if (abort) assert.ok(["0,1", "0,1,2"].includes(requests.map((request) => request.step).join(",")));
  else if (block) assert.deepEqual(requests.map((request) => request.step), [0, 1, 2]);
  else assert.deepEqual(requests.map((request) => request.step), [0, 1, 2, 3]);
  const events = await readEvents();
  const tools = events.filter((event) => event.kind === "agent_event");
  assert.deepEqual(tools.map((event) => event.data.event.failed), abort || block ? [true, true] : [true, true, false]);
  assert.deepEqual(tools.map((event) => event.data.event.tool_name), gate && !block ? ["read", "read", "write"] : tools.map(() => "read"));
  const triggers = events.filter((event) => event.kind === "rule_triggered");
  assert.equal(triggers.length, 1);
  assert.equal(triggers[0].data.trigger.matched_events, 2);
  assert.equal(triggers[0].data.matched_event_sequence, tools[1].sequence);
  assert.equal(triggers[0].data.action, action);
  for (const event of tools) {
    const raw = events.find((candidate) => candidate.sequence === event.data.raw_event_sequence);
    assert.equal(raw.kind, "output");
    const pi = JSON.parse(raw.data.text);
    assert.equal(pi.type, "tool_execution_end");
    assert.equal(pi.isError, event.data.event.failed);
    assert.equal(pi.toolCallId, event.data.event.tool_call_id);
    if (pi.isError) assert.match(JSON.stringify(pi.result), /ENOENT/);
    else if (!gate) assert.match(JSON.stringify(pi.result), /ERROR is ordinary file content/);
  }
  assert.equal(events.at(-1).kind, "run_finished");
  assert.equal(events.at(-1).data.adapter_complete, true);
  assert.equal(events.at(-1).data.capture_complete, true);
  assert.equal(events.at(-1).data.exit_code, 0);
  assert.equal(events.at(-1).data.forced_cleanup, false);
  assert.equal(events.at(-1).data.control_outcome, expectedOutcome);
  if (!abort && !block) assert.ok(tools[2].sequence > triggers[0].sequence, "Pi must continue after a record-only trigger");
  let confirmation;
  if (rpc) {
    const commands = events.filter((event) => event.kind === "rpc_command_requested");
    let expectedCommands = ["prompt"];
    if (abort) expectedCommands = ["prompt", "clear_queue", "abort", "get_state"];
    if (gate) expectedCommands = ["get_commands", "prompt"];
    assert.deepEqual(commands.map((event) => event.data.command), expectedCommands);
    for (const command of commands) {
      assert.ok(events.some((event) => event.kind === "rpc_response" && event.data.id === command.data.id
        && event.data.command === command.data.command && event.data.success === true));
    }
    assert.ok(events.some((event) => event.kind === "rpc_input_closed"));
    if (abort) {
      confirmation = events.find((event) => event.kind === "control_confirmed");
      assert.equal(confirmation.data.trigger_sequence, triggers[0].sequence);
      const state = JSON.parse(events.find((event) => event.sequence === confirmation.data.raw_event_sequence).data.text).data;
      assert.equal(state.isStreaming, false);
      assert.equal(state.isCompacting, false);
      assert.equal(state.pendingMessageCount, 0);
      assert.equal(events.filter((event) => event.kind === "tool_started_after_trigger").length, 0);
    }
  }
  let effectCreated;
  if (gate) {
    const decisions = events.filter((event) => event.kind === "gate_decision");
    const applied = events.filter((event) => event.kind === "gate_applied");
    assert.deepEqual(decisions.map((event) => event.data.allowed), [true, true, !block]);
    assert.deepEqual(applied.map((event) => event.data.decision_sequence), decisions.map((event) => event.sequence));
    assert.deepEqual(applied.map((event) => event.data.allowed), [true, true, !block]);
    assert.equal(events.at(-1).data.blocked_tool_count, block ? 1 : 0);
    assert.equal(events.filter((event) => event.kind === "tool_blocked_result").length, block ? 1 : 0);
    const readiness = events.find((event) => event.kind === "gate_ready");
    assert.ok(readiness.sequence < events.find((event) => event.kind === "rpc_command_requested" && event.data.command === "prompt").sequence);
    if (block) {
      assert.equal(decisions[2].data.trigger_sequence, triggers[0].sequence);
      await assert.rejects(readFile(join(cwd, "effect.txt")), (error) => error.code === "ENOENT");
      effectCreated = false;
    } else {
      assert.equal(await readFile(join(cwd, "effect.txt"), "utf8"), "gate-proof\n");
      effectCreated = true;
    }
  }
  const summary = {
    pi_version: version, node_version: process.version, model_mode: "local-deterministic-fixture", real_model_validated: false,
    mode, tool_results: tools.length, tool_failures: 2, rule_triggers: 1, exit_code: exitCode,
    child_exit_code: events.at(-1).data.exit_code, control_outcome: events.at(-1).data.control_outcome,
    confirmation_sequence: confirmation?.sequence,
    tool_starts_after_trigger: rpc ? events.filter((event) => event.kind === "tool_started_after_trigger").length : null,
    blocked_tool_count: events.at(-1).data.blocked_tool_count,
    effect_created: effectCreated, artificial_delay_ms: gate ? 0 : undefined,
    raw_output_sequences: tools.map((event) => event.data.raw_event_sequence),
    normalized_sequences: tools.map((event) => event.sequence),
    trigger_sequence: triggers[0].sequence, evidence_sequence: triggers[0].data.matched_event_sequence,
  };
  await writeFile(join(run, "summary.json"), `${JSON.stringify(summary, null, 2)}\n`);
  console.log(JSON.stringify({ ...summary, evidence_directory: run }, null, 2));
} finally {
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
}

async function readEvents() {
  return (await readFile(journal, "utf8")).split("\n").filter(Boolean).map((line) => JSON.parse(line));
}
