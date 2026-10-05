// Real Pi: distinguish prior batch admission from fresh admission, including nested calls.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createWriteStream } from "node:fs";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { captureOutput } from "./capture-output.mjs";
import { stopRun } from "./process-cleanup.mjs";
import { checkRecovery } from "./check-recovery.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "../..");
const piPackage = join(here, "node_modules/@earendil-works/pi-coding-agent");
const { version, bin } = JSON.parse(await readFile(join(piPackage, "package.json"), "utf8"));
assert.equal(version, "1.0.3", "Install the pinned Pi dependency with npm ci");
const launcher = join(root, "target/debug", process.platform === "win32" ? "inhibitor-lab.exe" : "inhibitor-lab");
const proof = "boundary-proof\n";
const call = (id, name, args) => ({ id, name, args });
const batches = [
  [call("batch_read_a", "read", { path: "missing-a.txt" }),
    call("batch_read_b", "read", { path: "missing-b.txt" }),
    call("batch_write", "write", { path: "batch-admitted.txt", content: proof })],
  [call("later_write", "write", { path: "batch-later.txt", content: proof }),
    call("later_retry", "write", { path: "batch-retry.txt", content: proof })],
];

await mkdir(join(root, "runs"), { recursive: true });
for (const scenario of ["batch", "nested"]) {
  for (const action of ["record", "block"]) await runCase(scenario, action);
}

async function runCase(scenario, action) {
  const block = action === "block";
  const nested = scenario === "nested";
  const run = await mkdtemp(join(root, `runs/pi-boundary-${scenario}-${action}-`));
  const cwd = join(run, "workspace");
  const agentDir = join(run, "pi-config");
  await mkdir(cwd);
  await mkdir(agentDir);
  const journal = join(run, "events.jsonl");
  const tools = nested ? ["lab_nested", "lab_sequence", "read", "write"] : ["read", "write"];
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
        assert.ok(length <= 1024 * 1024, "Fixture input exceeded limit");
        chunks.push(chunk);
      }
      const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      assert.equal(body.model, "deterministic");
      assert.equal(body.stream, true);
      assert.deepEqual(body.tools.map((tool) => tool.function.name).sort(), [...tools].sort());
      const step = requests.length;
      const results = body.messages.filter((message) => message.role === "tool");
      assert.ok(step < (nested ? 1 : 3), "Unexpected model retry or continuation");
      assert.equal(results.length, nested ? 0 : [0, 3, 5][step]);
      requests.push({ step, transcript_tool_results: results.length });
      response.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" });
      const send = (delta, finishReason = null) => response.write(`data: ${JSON.stringify({
        id: `boundary-${step}`, object: "chat.completion.chunk", created: 0, model: "deterministic",
        choices: [{ index: 0, delta, finish_reason: finishReason }],
      })}\n\n`);
      send({ role: "assistant", content: "" });
      const calls = nested ? [call("nested_root", "lab_nested", {})] : batches[step];
      if (calls) {
        send({ tool_calls: calls.map((tool, index) => ({ index, id: tool.id, type: "function",
          function: { name: tool.name, arguments: JSON.stringify(tool.args) } })) });
        send({}, "tool_calls");
      } else {
        send({ content: "Boundary fixture completed." });
        send({}, "stop");
      }
      response.end("data: [DONE]\n\n");
    } catch (error) {
      serverErrors.push(String(error));
      if (!response.headersSent) response.writeHead(500, { "Content-Type": "application/json" });
      response.end(JSON.stringify({ error: { message: "Boundary fixture rejected request" } }));
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
        models: [{ id: "deterministic", name: "Boundary fixture", reasoning: false, input: ["text"],
          contextWindow: 32768, maxTokens: 2048, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
      },
    } }));
    await writeFile(join(agentDir, "settings.json"), JSON.stringify({
      retry: { enabled: false }, compaction: { enabled: false },
    }));
    const args = ["run", "--format", "pi-rpc", "--rules", join(root, "examples/pi-rules.json"),
      "--out", journal, "--prompt", "Run the isolated boundary fixture.", "--tool-gate",
      "--on-trigger", action, "--timeout-ms", "20000", "--", process.execPath, join(piPackage, bin.pi),
      "--mode", "rpc", "--provider", "inhibitor-local", "--model", "deterministic", "--thinking", "off",
      "--offline", "--no-extensions", "--no-skills", "--no-prompt-templates", "--no-session",
      "--tools", tools.join(","), "--extension", join(here, "tool-gate.js"),
      ...(nested ? ["--extension", join(here, "boundary-tools.js")] : [])];
    const child = spawn(launcher, args, {
      cwd, env: { ...process.env, PI_CODING_AGENT_DIR: agentDir, PI_OFFLINE: "1" },
      windowsHide: true, stdio: ["ignore", "pipe", "pipe"],
    });
    const exitCode = await captureOutput(child, createWriteStream(join(run, "stdout.jsonl")),
      createWriteStream(join(run, "stderr.log")), { stop: () => stopRun(child, journal) });
    assert.deepEqual(serverErrors, [], `Fixture failed: ${run}`);
    assert.equal(exitCode, block ? 3 : 0, `Launcher failed; see ${run}`);
    const journalBytes = await readFile(journal);
    const events = journalBytes.toString("utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
    const ofKind = (kind) => events.filter((event) => event.kind === kind);
    const raw = ofKind("output").filter((e) => e.data.source === "stdout")
      .map((e) => ({ sequence: e.sequence, value: JSON.parse(e.data.text) }));
    const decisions = ofKind("gate_decision");
    const applied = ofKind("gate_applied");
    const normalized = ofKind("agent_event");
    const triggers = ofKind("rule_triggered");
    const finished = events.at(-1);
    assert.equal(finished.kind, "run_finished");
    assert.equal(finished.data.exit_code, 0);
    assert.equal(finished.data.capture_complete, true);
    assert.equal(finished.data.adapter_complete, true);
    assert.equal(finished.data.forced_cleanup, false);
    assert.equal(finished.data.control_outcome, block ? "blocked" : "completed");
    assert.equal(finished.data.blocked_tool_count, block ? 2 : 0);
    assert.equal(triggers.length, 1);
    assert.equal(triggers[0].data.trigger.matched_events, 2);
    assert.equal(normalized.filter((e) => e.data.event.failed).length, 2);
    assert.equal(ofKind("tool_blocked_result").length, block ? 2 : 0);
    assert.equal(applied.length, decisions.length);
    for (const decision of decisions) {
      const confirmation = applied.find((e) => e.data.decision_sequence === decision.sequence);
      assert.ok(confirmation, "Each decision requires an application confirmation");
      assert.equal(confirmation.data.tool_call_id, decision.data.tool_call_id);
      assert.equal(confirmation.data.allowed, decision.data.allowed);
      if (block && decision.sequence > triggers[0].sequence) assert.equal(decision.data.allowed, false);
      if (!decision.data.allowed) assert.equal(decision.data.trigger_sequence, triggers[0].sequence);
    }
    const files = {};
    async function checkEffect(name, expected) {
      if (expected) assert.equal(await readFile(join(cwd, name), "utf8"), proof);
      else await assert.rejects(readFile(join(cwd, name)), (error) => error.code === "ENOENT");
      files[name] = expected;
    }
    const parentLinks = [];
    if (nested) {
      assert.equal(requests.length, 1, "Nested results must not create model transcript turns");
      assert.equal(decisions.length, 6);
      assert.equal(normalized.length, block ? 4 : 6);
      assert.deepEqual(decisions.map((e) => e.data.allowed), [true, true, true, true, !block, !block]);
      const rootCall = decisions.find((e) => e.data.tool_name === "lab_nested").data.tool_call_id;
      const sequenceCall = decisions.find((e) => e.data.tool_name === "lab_sequence").data.tool_call_id;
      const children = raw.filter((e) => e.value.type === "tool_execution_end" && e.value.parentToolCallId);
      assert.equal(children.length, 5);
      for (const child of children) {
        const parent = child.value.toolName === "lab_sequence" ? rootCall : sequenceCall;
        assert.equal(child.value.parentToolCallId, parent);
        const decision = decisions.find((e) => e.data.tool_call_id === child.value.toolCallId);
        assert.ok(decision && decision.data.tool_name === child.value.toolName);
        parentLinks.push({ id: child.value.toolCallId, parent, raw_sequence: child.sequence,
          decision_sequence: decision.sequence });
      }
      await checkEffect("nested-first.txt", !block);
      await checkEffect("nested-retry.txt", !block);
    } else {
      assert.equal(requests.length, block ? 2 : 3);
      assert.equal(decisions.length, 5);
      assert.equal(normalized.length, block ? 3 : 5);
      assert.deepEqual(decisions.map((e) => e.data.allowed), [true, true, true, !block, !block]);
      const firstResult = raw.find((e) => e.value.type === "tool_execution_end").sequence;
      assert.ok(decisions.slice(0, 3).every((e) => e.sequence < firstResult), "Pi must pre-admit the initial batch");
      const write = decisions.find((e) => e.data.tool_call_id === "batch_write");
      assert.ok(write.sequence < triggers[0].sequence, "First write was already admitted before closure");
      await checkEffect("batch-admitted.txt", true);
      await checkEffect("batch-later.txt", !block);
      await checkEffect("batch-retry.txt", !block);
    }
    const diagnosis = spawnSync(launcher, ["inspect", "--log", journal, "--format", "json"], {
      encoding: "utf8", timeout: 5000, maxBuffer: 1024 * 1024, windowsHide: true,
    });
    assert.ifError(diagnosis.error);
    assert.equal(diagnosis.status, 0, `Inspection failed: ${diagnosis.stdout}\n${diagnosis.stderr}`);
    const inspection = JSON.parse(diagnosis.stdout);
    assert.equal(inspection.status, "valid");
    assert.equal(inspection.run_id, events[0].run_id);
    assert.equal(inspection.eligible_for_manual_restart, block);
    assert.equal(inspection.confirmed_blocked_tools, block ? 2 : 0);
    assert.equal(inspection.triggers[0].evidence_sequence, triggers[0].data.matched_event_sequence);
    assert.deepEqual(await readFile(journal), journalBytes, "Inspection must preserve the original log bytes");
    await writeFile(join(run, "inspection.json"), `${JSON.stringify(inspection, null, 2)}\n`);
    const requestsBeforeRecovery = requests.length;
    const recovery = block ? await checkRecovery(launcher, journal, run, {
      schema_version: 1, reason: "R2: review a fresh task after the parent gate closed.",
      prompt: "Run the next isolated fixture only after a future explicit start step.",
      rules: join(root, "examples/pi-rules.json"), cwd, program: process.execPath,
      args: args.slice(args.indexOf("--") + 2), timeout_ms: 20000, control_timeout_ms: 5000,
    }) : null;
    assert.equal(requests.length, requestsBeforeRecovery, "R2 must not make additional model requests");
    const summary = {
      pi_version: version, node_version: process.version, scenario, action,
      model_mode: "local-deterministic-fixture", real_model_validated: false, artificial_delay_ms: 0,
      requests, files, ...finished.data, child_exit_code: finished.data.exit_code, exit_code: exitCode,
      trigger_sequence: triggers[0].sequence, evidence_sequence: triggers[0].data.matched_event_sequence,
      decisions: decisions.map((e) => ({ sequence: e.sequence, ...e.data })), parent_links: parentLinks,
      eligible_for_manual_restart: inspection.eligible_for_manual_restart,
      recovery,
    };
    await writeFile(join(run, "summary.json"), `${JSON.stringify(summary, null, 2)}\n`);
    console.log(JSON.stringify({ ...summary, evidence_directory: run }, null, 2));
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
}
