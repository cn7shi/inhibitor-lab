// Real Pi CLI and real read tools, with deterministic model replies from loopback.
// This checks the integration contract, not a real model's behavior.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, join, resolve } from "node:path";
import { finished } from "node:stream/promises";
import { fileURLToPath } from "node:url";
import { stopRun } from "./process-cleanup.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "../..");
const piPackage = join(here, "node_modules/@earendil-works/pi-coding-agent");
const { version, bin } = JSON.parse(await readFile(join(piPackage, "package.json"), "utf8"));
assert.equal(version, "1.0.3", "Use npm ci to install the pinned Pi version");
const launcher = join(root, "target/debug", process.platform === "win32" ? "inhibitor-lab.exe" : "inhibitor-lab");
await mkdir(join(root, "runs"), { recursive: true });
const run = await mkdtemp(join(root, "runs/pi-local-"));
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
    assert.deepEqual(body.tools.map((tool) => tool.function.name), ["read"]);
    const results = body.messages.filter((message) => message.role === "tool");
    const step = results.length;
    assert.ok(step <= 3 && requests.length < 5, "Unexpected tool loop or retry");
    requests.push({ step, toolResults: results.length });
    response.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" });
    const send = (delta, finishReason = null) => response.write(`data: ${JSON.stringify({
      id: `fixture-${step}`, object: "chat.completion.chunk", created: 0, model: "deterministic",
      choices: [{ index: 0, delta, finish_reason: finishReason }],
    })}\n\n`);
    send({ role: "assistant", content: "" });
    if (step < 3) {
      const path = ["missing-a.txt", "missing-b.txt", "present.txt"][step];
      send({ tool_calls: [{ index: 0, id: `lab_read_${step}`, type: "function",
        function: { name: "read", arguments: JSON.stringify({ path }) } }] });
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
  const args = ["run", "--format", "pi-json", "--rules", join(root, "examples/pi-rules.json"),
    "--out", journal, "--", process.execPath, join(piPackage, bin.pi),
    "--mode", "json", "--provider", "inhibitor-local", "--model", "deterministic",
    "--thinking", "off", "--offline", "--no-extensions", "--no-skills", "--no-prompt-templates",
    "--no-session", "--tools", "read",
    "Read missing-a.txt, missing-b.txt, and present.txt once each, then finish."];
  const child = spawn(launcher, args, {
    cwd, env: { ...process.env, PI_CODING_AGENT_DIR: agentDir, PI_OFFLINE: "1" },
    windowsHide: true, stdio: ["ignore", "pipe", "pipe"],
  });
  const stdout = createWriteStream(join(run, "stdout.jsonl"));
  const stderr = createWriteStream(join(run, "stderr.log"));
  child.stdout.pipe(stdout);
  child.stderr.pipe(stderr);
  let timedOut = false;
  let cleanup = Promise.resolve();
  const timeout = setTimeout(() => {
    timedOut = true;
    cleanup = stopRun(child, journal).catch((error) => {
      serverErrors.push(`Timeout cleanup failed: ${error}`);
      console.error(`Timeout cleanup failed: ${error}`);
    });
  }, 30000);
  let exitCode;
  try {
    exitCode = await new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", resolve);
    });
    await Promise.all([finished(stdout), finished(stderr)]);
  } finally {
    clearTimeout(timeout);
    await cleanup;
  }
  assert.equal(timedOut, false, `Pi timed out; see ${run}`);
  assert.deepEqual(serverErrors, [], `Fixture failed; see ${run}`);
  assert.equal(exitCode, 0, `Launcher/Pi failed; see ${join(run, "stderr.log")}`);
  assert.deepEqual(requests.map((request) => request.step), [0, 1, 2, 3]);
  const events = await readEvents();
  const tools = events.filter((event) => event.kind === "agent_event");
  assert.deepEqual(tools.map((event) => event.data.event.failed), [true, true, false]);
  assert.ok(tools.every((event) => event.data.event.tool_name === "read"));
  const triggers = events.filter((event) => event.kind === "rule_triggered");
  assert.equal(triggers.length, 1);
  assert.equal(triggers[0].data.trigger.matched_events, 2);
  assert.equal(triggers[0].data.matched_event_sequence, tools[1].sequence);
  assert.equal(triggers[0].data.action, "record");
  for (const event of tools) {
    const raw = events.find((candidate) => candidate.sequence === event.data.raw_event_sequence);
    assert.equal(raw.kind, "output");
    const pi = JSON.parse(raw.data.text);
    assert.equal(pi.type, "tool_execution_end");
    assert.equal(pi.isError, event.data.event.failed);
    assert.equal(pi.toolCallId, event.data.event.tool_call_id);
    if (pi.isError) assert.match(JSON.stringify(pi.result), /ENOENT/);
    else assert.match(JSON.stringify(pi.result), /ERROR is ordinary file content/);
  }
  assert.equal(events.at(-1).kind, "run_finished");
  assert.equal(events.at(-1).data.adapter_complete, true);
  assert.equal(events.at(-1).data.capture_complete, true);
  assert.equal(events.at(-1).data.exit_code, 0);
  assert.ok(tools[2].sequence > triggers[0].sequence, "Pi must continue after a record-only trigger");
  const summary = {
    pi_version: version, model_mode: "local-deterministic-fixture", real_model_validated: false,
    tool_results: 3, tool_failures: 2, rule_triggers: 1, exit_code: exitCode,
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
