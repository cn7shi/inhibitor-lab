// R4/R5: real Pi A -> confirmation -> real Pi B, plus read-only status, using local fixed replies.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createWriteStream } from "node:fs";
import { mkdir, mkdtemp, readFile, readdir, realpath, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { captureOutput } from "./capture-output.mjs";
import { checkRecovery, recoveryStatus } from "./check-recovery.mjs";
import { stopRun } from "./process-cleanup.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "../..");
const piPackage = join(here, "node_modules/@earendil-works/pi-coding-agent");
const { version, bin } = JSON.parse(await readFile(join(piPackage, "package.json"), "utf8"));
assert.equal(version, "1.0.3", "Use npm ci to install the pinned Pi version");
const launcher = join(root, "target/debug", process.platform === "win32" ? "inhibitor-lab.exe" : "inhibitor-lab");
const prompt = "Read input-a.txt, then input-b.txt, then write effect.txt containing recovery-proof followed by a newline, then finish.";
const effect = "recovery-proof\n";
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const jsonFile = async (path) => JSON.parse(await readFile(path, "utf8"));
const eventsFile = async (path) => (await readFile(path, "utf8")).trim().split("\n").map(JSON.parse);
const save = (path, value) => writeFile(path, `${JSON.stringify(value, null, 2)}\n`);
const absent = (path) => assert.rejects(readFile(path), (error) => error.code === "ENOENT");

// Inspect only after the process has ended, when blocking cannot starve its model service.
async function inspect(journal, output) {
  const result = spawnSync(launcher, ["inspect", "--log", journal, "--format", "json"], {
    encoding: "utf8", timeout: 10000, maxBuffer: 3 * 1024 * 1024, windowsHide: true,
  });
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stderr);
  const report = JSON.parse(result.stdout);
  assert.equal(report.status, "valid");
  await save(output, report);
  return report;
}

async function workspaceSnapshot(cwd) {
  const files = (await readdir(cwd)).sort();
  const snapshot = {};
  for (const file of files) snapshot[file] = sha256(await readFile(join(cwd, file)));
  return snapshot;
}

function verifyRun(events, blocked) {
  assert.equal(events[0].kind, "run_started");
  assert.ok(events[0].data.pid > 0);
  assert.ok(events.every((e, index) => e.run_id === events[0].run_id && e.sequence === index + 1));
  const tools = events.filter((e) => e.kind === "agent_event");
  assert.deepEqual(tools.map((e) => e.data.event.tool_call_id),
    blocked ? ["recovery_tool_0", "recovery_tool_1"] : ["recovery_tool_0", "recovery_tool_1", "recovery_tool_2"]);
  assert.deepEqual(tools.map((e) => e.data.event.failed), blocked ? [true, true] : [false, false, false]);
  assert.deepEqual(tools.map((e) => e.data.event.tool_name), blocked ? ["read", "read"] : ["read", "read", "write"]);
  for (const tool of tools) {
    const raw = events[tool.data.raw_event_sequence - 1];
    assert.equal(raw.kind, "output");
    const pi = JSON.parse(raw.data.text);
    assert.equal(pi.type, "tool_execution_end");
    assert.equal(pi.toolCallId, tool.data.event.tool_call_id);
    assert.equal(pi.isError, tool.data.event.failed);
    if (blocked) assert.match(JSON.stringify(pi.result), /ENOENT/);
  }
  const triggers = events.filter((e) => e.kind === "rule_triggered");
  assert.equal(triggers.length, blocked ? 1 : 0);
  const decisions = events.filter((e) => e.kind === "gate_decision");
  const applied = events.filter((e) => e.kind === "gate_applied");
  assert.deepEqual(decisions.map((e) => e.data.allowed), [true, true, !blocked]);
  assert.deepEqual(applied.map((e) => e.data.allowed), [true, true, !blocked]);
  assert.deepEqual(applied.map((e) => e.data.decision_sequence), decisions.map((e) => e.sequence));
  assert.equal(events.filter((e) => e.kind === "tool_blocked_result").length, blocked ? 1 : 0);
  if (blocked) {
    assert.equal(triggers[0].data.trigger.matched_events, 2);
    assert.equal(triggers[0].data.matched_event_sequence, tools[1].sequence);
    assert.equal(decisions[2].data.trigger_sequence, triggers[0].sequence);
    assert.ok(decisions[1].sequence < triggers[0].sequence);
    assert.ok(decisions[2].sequence > triggers[0].sequence);
  }
  assert.equal(events.at(-1).kind, "run_finished");
  const finish = events.at(-1).data;
  assert.deepEqual(finish, {
    exit_code: 0, success: true, trigger_count: blocked ? 1 : 0,
    capture_complete: true, adapter_complete: true, control_outcome: blocked ? "blocked" : "completed",
    forced_cleanup: false, blocked_tool_count: blocked ? 1 : 0,
  });
  return { run_id: events[0].run_id, pid: events[0].data.pid, finish,
    normalized_sequences: tools.map((e) => e.sequence),
    trigger_sequence: triggers[0]?.sequence ?? null,
    decision_sequences: decisions.map((e) => e.sequence) };
}

await mkdir(join(root, "runs"), { recursive: true });
for (const scenario of ["repaired", "unchanged"]) await runCase(scenario);

async function runCase(scenario) {
  const run = await mkdtemp(join(root, `runs/pi-recovery-${scenario}-`));
  console.error(`Recovery experiment evidence: ${run}`);
  const cwd = join(run, "workspace");
  await mkdir(cwd);
  const rules = join(run, "rules.json");
  const ruleBytes = await readFile(join(root, "examples/pi-rules.json"));
  await writeFile(rules, ruleBytes);
  const parentLog = join(run, "parent.jsonl");
  const requests = { parent: [], child: [] };
  const serverErrors = [];
  let phase = null;
  let activePlan = null;
  let liveStatus = null;
  const server = createServer(async (request, response) => {
    try {
      const current = phase;
      assert.ok(current === "parent" || current === "child", "No model request is allowed outside a run");
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
      assert.deepEqual(body.tools.map((tool) => tool.function.name).sort(), ["read", "write"]);
      const users = body.messages.filter((m) => m.role === "user");
      assert.equal(users.length, 1, "B must start with only the explicitly supplied new prompt");
      assert.ok(JSON.stringify(users[0].content).includes(prompt));
      const results = body.messages.filter((m) => m.role === "tool");
      const step = results.length;
      assert.equal(step, requests[current].length, "Unexpected retry or inherited transcript");
      assert.ok(step <= 3, "Unexpected tool loop");
      assert.deepEqual(results.map((m) => m.tool_call_id),
        Array.from({ length: step }, (_, index) => `recovery_tool_${index}`));
      requests[current].push({ step, transcript_tool_results: step, user_messages: users.length });
      if (current === "child" && step === 0) {
        // Pi is waiting for this reply: only read evidence; do not drive its protocol here.
        liveStatus = recoveryStatus(launcher, activePlan, "outcome_unknown", 2);
        assert.equal(liveStatus.start_record_observed, true);
      }
      response.writeHead(200, { "Content-Type": "text/event-stream", "Cache-Control": "no-cache" });
      const send = (delta, finishReason = null) => response.write(`data: ${JSON.stringify({
        id: `recovery-fixture-${step}`, object: "chat.completion.chunk", created: 0, model: "deterministic",
        choices: [{ index: 0, delta, finish_reason: finishReason }],
      })}\n\n`);
      send({ role: "assistant", content: "" });
      if (step < 3) {
        const name = step < 2 ? "read" : "write";
        const args = step < 2 ? { path: ["input-a.txt", "input-b.txt"][step] } : { path: "effect.txt", content: effect };
        send({ tool_calls: [{ index: 0, id: `recovery_tool_${step}`, type: "function",
          function: { name, arguments: JSON.stringify(args) } }] });
        send({}, "tool_calls");
      } else {
        send({ content: "Deterministic fixture finished." });
        send({}, "stop");
      }
      response.end("data: [DONE]\n\n");
    } catch (error) {
      serverErrors.push(String(error));
      if (!response.headersSent) response.writeHead(500, { "Content-Type": "application/json" });
      response.end(JSON.stringify({ error: { message: "Recovery fixture rejected the request" } }));
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });

  async function configure(name) {
    const dir = join(run, `${name}-pi-config`);
    await mkdir(dir);
    await save(join(dir, "models.json"), { providers: { "inhibitor-local": {
      baseUrl: `http://127.0.0.1:${server.address().port}/v1`, api: "openai-completions",
      apiKey: "local-fixture-not-a-secret",
      models: [{ id: "deterministic", name: "Local recovery fixture", reasoning: false,
        input: ["text"], contextWindow: 32768, maxTokens: 1024,
        cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 } }],
    } } });
    await save(join(dir, "settings.json"), { retry: { enabled: false }, compaction: { enabled: false } });
    return { ...process.env, PI_CODING_AGENT_DIR: dir, PI_OFFLINE: "1" };
  }

  async function execute(args, journal, name, env, launchCwd = root) {
    const child = spawn(launcher, args, { cwd: launchCwd, env, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
    return captureOutput(child, createWriteStream(join(run, `${name}.stdout.jsonl`)),
      createWriteStream(join(run, `${name}.stderr.log`)), { stop: () => stopRun(child, journal), timeoutMs: 30000 });
  }

  try {
    const parentEnv = await configure("parent"), childEnv = await configure("child");
    const piArgs = [join(piPackage, bin.pi), "--mode", "rpc", "--provider", "inhibitor-local", "--model", "deterministic",
      "--thinking", "off", "--offline", "--no-extensions", "--no-skills", "--no-prompt-templates",
      "--no-session", "--tools", "read,write", "--extension", join(here, "tool-gate.js")];
    phase = "parent";
    const parentExit = await execute(["run", "--format", "pi-rpc", "--rules", rules, "--out", parentLog,
      "--prompt", prompt, "--tool-gate", "--on-trigger", "block", "--timeout-ms", "20000",
      "--control-timeout-ms", "5000", "--", process.execPath, ...piArgs], parentLog, "parent", parentEnv, cwd);
    phase = null;
    assert.equal(parentExit, 3);
    assert.deepEqual(serverErrors, []);
    assert.deepEqual(requests.parent.map((r) => r.step), [0, 1, 2]);
    const parentBytes = await readFile(parentLog);
    const parent = verifyRun(await eventsFile(parentLog), true);
    assert.equal((await inspect(parentLog, join(run, "parent-inspection.json"))).eligible_for_manual_restart, true);
    await absent(join(cwd, "effect.txt"));
    assert.deepEqual(await workspaceSnapshot(cwd), {});
    if (scenario === "repaired") {
      // This is an explicit fixture repair, not an action performed by inhibitor or the model.
      await writeFile(join(cwd, "input-a.txt"), "Available input A.\n");
      await writeFile(join(cwd, "input-b.txt"), "Available input B.\n");
    }
    const inputsBeforeChild = await workspaceSnapshot(cwd);
    const config = { schema_version: 1,
      reason: scenario === "repaired" ? "The fixture operator supplied both missing input files." : "Repeat unchanged inputs to verify the gate closes again.",
      prompt, rules, cwd, program: process.execPath, args: piArgs, timeout_ms: 20000, control_timeout_ms: 5000 };
    const reviewDir = join(run, "review");
    await mkdir(reviewDir);
    await save(join(reviewDir, "review-summary.json"), await checkRecovery(launcher, parentLog, reviewDir, config));
    let planPath = join(reviewDir, "recovery-plan.json");
    let staleStartRejected = null; // Only the repaired scenario exercises this negative path.
    if (scenario === "repaired") {
      // Keep the stale attempt intact, then prepare a different plan for the actual new task.
      const oldPlan = await readFile(planPath), oldReceipt = await readFile(`${planPath}.confirmed.json`);
      await save(join(reviewDir, "recovery-config.json"), { ...config, prompt: "This task was not confirmed." });
      const staleExit = await execute(["recovery", "start", "--plan", planPath], `${planPath}.run.jsonl`, "stale-start", childEnv);
      assert.equal(staleExit, 2);
      for (const suffix of [".start.json", ".run.jsonl", ".result.json"]) await absent(`${planPath}${suffix}`);
      assert.deepEqual(requests.child, []);
      assert.deepEqual(await workspaceSnapshot(cwd), inputsBeforeChild);
      assert.deepEqual(await readFile(planPath), oldPlan);
      assert.deepEqual(await readFile(`${planPath}.confirmed.json`), oldReceipt);
      await save(join(reviewDir, "stale-attempt.json"), { launcher_exit: staleExit,
        start_record_created: false, child_log_created: false, model_requests: 0, confirmation_unchanged: true });
      staleStartRejected = true;
      await save(join(reviewDir, "stale-status.json"), recoveryStatus(launcher, planPath, "stale_inputs", 2));
      const freshDir = join(run, "review-fresh");
      await mkdir(freshDir);
      await save(join(freshDir, "review-summary.json"), await checkRecovery(launcher, parentLog, freshDir, config));
      planPath = join(freshDir, "recovery-plan.json");
    }
    assert.deepEqual(serverErrors, []);
    assert.deepEqual(requests.child, [], "Preparation and confirmation must not query the model");
    const planBytes = await readFile(planPath), receiptBytes = await readFile(`${planPath}.confirmed.json`);
    const plan = JSON.parse(planBytes), receipt = JSON.parse(receiptBytes);
    const childLog = `${planPath}.run.jsonl`;
    activePlan = planPath;
    phase = "child";
    const childExit = await execute(["recovery", "start", "--plan", planPath], childLog, "child", childEnv);
    phase = null;
    const blocked = scenario === "unchanged";
    assert.equal(childExit, blocked ? 3 : 0);
    assert.deepEqual(serverErrors, []);
    assert.deepEqual(requests.child.map((r) => r.step), blocked ? [0, 1, 2] : [0, 1, 2, 3]);
    const events = await eventsFile(childLog);
    const child = verifyRun(events, blocked);
    assert.notEqual(child.run_id, parent.run_id);
    assert.equal(child.run_id, plan.planned_run_id);
    assert.equal(events[1].kind, "run_linked");
    assert.deepEqual(events[1].data, { recovery_id: plan.recovery_id, parent_run_id: parent.run_id,
      parent_sha256: sha256(parentBytes), plan_sha256: sha256(planBytes),
      confirmation_sha256: sha256(receiptBytes), counter_reset: "new_run" });
    const report = await inspect(childLog, join(run, "child-inspection.json"));
    assert.equal(report.eligible_for_manual_restart, blocked);
    assert.deepEqual(report.linkage, events[1].data);
    const claimBytes = await readFile(`${planPath}.start.json`), claim = JSON.parse(claimBytes);
    const result = await jsonFile(`${planPath}.result.json`);
    assert.equal(claim.status, "start_reserved");
    assert.equal(claim.planned_run_id, child.run_id);
    assert.equal(claim.parent_run_id, parent.run_id);
    assert.equal(claim.parent_sha256, sha256(parentBytes));
    assert.equal(claim.plan_sha256, sha256(planBytes));
    assert.equal(claim.confirmation_sha256, sha256(receiptBytes));
    assert.equal(receipt.plan_sha256, sha256(planBytes));
    assert.equal(receipt.config_sha256, sha256(await readFile(plan.inputs.config_path)));
    assert.equal(receipt.rules_sha256, sha256(ruleBytes));
    assert.equal(result.status, "run_finished");
    assert.equal(result.start_sha256, sha256(claimBytes));
    assert.equal(result.run_id, child.run_id);
    assert.equal(result.pid, child.pid);
    assert.equal(result.launcher_exit_code, childExit);
    assert.deepEqual(result.finish, child.finish);
    assert.equal(await realpath(claim.output_path), await realpath(childLog));
    assert.equal(await realpath(claim.result_path), await realpath(`${planPath}.result.json`));
    assert.equal(await realpath(result.output_path), await realpath(childLog));
    if (blocked) await absent(join(cwd, "effect.txt"));
    else assert.equal(await readFile(join(cwd, "effect.txt"), "utf8"), effect);
    const effects = await workspaceSnapshot(cwd);
    assert.deepEqual(effects, blocked ? inputsBeforeChild : { ...inputsBeforeChild, "effect.txt": sha256(effect) });
    assert.deepEqual(await readFile(parentLog), parentBytes);
    assert.deepEqual(await readFile(planPath), planBytes);
    assert.deepEqual(await readFile(`${planPath}.confirmed.json`), receiptBytes);
    assert.deepEqual(await readFile(rules), ruleBytes);
    const paths = [parentLog, planPath, `${planPath}.confirmed.json`, `${planPath}.start.json`, childLog, `${planPath}.result.json`];
    const snapshots = await Promise.all(paths.map((path) => readFile(path)));
    const requestSnapshot = JSON.stringify(requests);
    const finalStatus = recoveryStatus(launcher, planPath, blocked ? "blocked" : "completed");
    assert.deepEqual(finalStatus.finish, child.finish);
    assert.equal(finalStatus.source_check, "matches");
    await save(join(run, "live-status.json"), liveStatus);
    await save(join(run, "final-status.json"), finalStatus);
    const replayExit = await execute(["recovery", "start", "--plan", planPath], childLog, "replay", childEnv);
    assert.equal(replayExit, 2);
    assert.deepEqual(await Promise.all(paths.map((path) => readFile(path))), snapshots);
    assert.deepEqual(await workspaceSnapshot(cwd), effects);
    assert.equal(JSON.stringify(requests), requestSnapshot);
    assert.deepEqual(serverErrors, []);
    const summary = { scenario, pi_version: version, node_version: process.version,
      model_mode: "local-deterministic-fixture", real_model_validated: false, confirmation_mode: "test-input",
      artificial_delay_ms: 0, parent, child, parent_launcher_exit: parentExit, child_launcher_exit: childExit,
      requests, input_repair: scenario === "repaired" ? "fixture operator supplied two files before confirmation" : "none",
      workspace_before_child: inputsBeforeChild, workspace_after_child: effects,
      effect_created: !blocked, stale_start_rejected: staleStartRejected, replay_exit: replayExit,
      parent_unchanged: true, independent_digest_checks: true, plan_path: planPath,
      evidence_sha256: { parent: sha256(parentBytes), rules: sha256(ruleBytes), config: receipt.config_sha256,
        plan: sha256(planBytes), confirmation: sha256(receiptBytes), start: sha256(claimBytes),
        child: sha256(await readFile(childLog)), result: sha256(await readFile(`${planPath}.result.json`)) } };
    await save(join(run, "summary.json"), summary);
    console.log(JSON.stringify({ ...summary, evidence_directory: run }, null, 2));
  } catch (error) {
    try {
      await save(join(run, "failure.json"), { error: String(error), phase, requests, serverErrors });
    } catch (saveError) {
      throw new AggregateError([error, saveError], `Recovery experiment and failure recording failed; see ${run}`);
    }
    throw error;
  } finally {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
}
