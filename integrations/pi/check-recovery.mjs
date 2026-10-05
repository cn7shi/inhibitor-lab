// Exercise R2 and R5 against a completed real Pi parent; these commands do not start the child.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFile, writeFile, readdir } from "node:fs/promises";
import { join } from "node:path";

export function recoveryStatus(launcher, plan, state, expectedCode = 0) {
  const result = spawnSync(launcher, ["recovery", "status", "--plan", plan, "--format", "json"], {
    encoding: "utf8", timeout: 10000, maxBuffer: 1024 * 1024, windowsHide: true,
  });
  assert.ifError(result.error);
  assert.equal(result.status, expectedCode, result.stderr || result.stdout);
  const report = JSON.parse(result.stdout);
  assert.equal(report.state, state);
  assert.equal(report.read_only, true);
  assert.equal(report.authorizes_start, false);
  return report;
}

export async function checkRecovery(launcher, journal, run, config) {
  const original = await readFile(journal);
  const effects = (await readdir(config.cwd)).sort();
  const configPath = join(run, "recovery-config.json");
  const planPath = join(run, "recovery-plan.json");
  const receiptPath = `${planPath}.confirmed.json`;
  await writeFile(configPath, `${JSON.stringify(config, null, 2)}\n`);
  function invoke(args, input = "") {
    const result = spawnSync(launcher, ["recovery", ...args], {
      input, encoding: "utf8", timeout: 10000, maxBuffer: 3 * 1024 * 1024, windowsHide: true,
    });
    assert.ifError(result.error);
    return result;
  }
  const prepared = invoke(["prepare", "--parent", journal, "--config", configPath, "--out", planPath]);
  assert.equal(prepared.status, 0, prepared.stderr);
  const planBytes = await readFile(planPath);
  const plan = JSON.parse(planBytes);
  const preparedStatus = recoveryStatus(launcher, planPath, "awaiting_confirmation");
  const cancelled = invoke(["confirm", "--plan", planPath], "cancel\n");
  assert.equal(cancelled.status, 3, cancelled.stderr);
  await assert.rejects(readFile(receiptPath), (error) => error.code === "ENOENT");
  recoveryStatus(launcher, planPath, "awaiting_confirmation");
  const confirmed = invoke(["confirm", "--plan", planPath], `CONFIRM ${plan.recovery_id}\n`);
  assert.equal(confirmed.status, 0, confirmed.stderr);
  const receiptBytes = await readFile(receiptPath);
  const receipt = JSON.parse(receiptBytes);
  const confirmedStatus = recoveryStatus(launcher, planPath, "awaiting_start");
  assert.equal(receipt.status, "confirmed_not_started");
  assert.equal(receipt.parent_run_id, plan.inputs.parent.run_id);
  assert.equal(receipt.planned_run_id, plan.planned_run_id);
  const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
  assert.equal(receipt.parent_sha256, sha256(original));
  assert.equal(receipt.plan_sha256, sha256(planBytes));
  assert.equal(receipt.config_sha256, sha256(await readFile(configPath)));
  assert.equal(receipt.rules_sha256, sha256(await readFile(config.rules)));
  const replay = invoke(["confirm", "--plan", planPath], `CONFIRM ${plan.recovery_id}\n`);
  assert.equal(replay.status, 2, replay.stderr);
  assert.deepEqual(await readFile(journal), original, "Parent log must remain unchanged");
  assert.deepEqual(await readFile(planPath), planBytes, "Plan must remain unchanged");
  assert.deepEqual(await readFile(receiptPath), receiptBytes, "Replay must not overwrite confirmation");
  assert.deepEqual((await readdir(config.cwd)).sort(), effects, "Confirmation must not create workspace effects");
  return { status: receipt.status, recovery_id: receipt.recovery_id, planned_run_id: receipt.planned_run_id,
    parent_run_id: receipt.parent_run_id, cancellation_exit: cancelled.status, replay_exit: replay.status,
    parent_unchanged: true, digests_verified_with_node: true,
    status_reports: { prepared: preparedStatus, confirmed: confirmedStatus } };
}
