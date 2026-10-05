import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { readStartedPid, stopRun } from "./process-cleanup.mjs";

async function removeTestDirectory(root) {
  assert.equal(dirname(resolve(root)), resolve(tmpdir()));
  assert.ok(basename(root).startsWith("inhibitor-cleanup-"));
  await rm(root, { recursive: true });
}

test("the startup PID survives an incomplete trailing journal record", async () => {
  const root = await mkdtemp(join(tmpdir(), "inhibitor-cleanup-"));
  try {
    const journal = join(root, "events.jsonl");
    await writeFile(journal, '{"kind":"run_started","data":{"pid":123}}\n{"kind":"output","data":');
    assert.equal(await readStartedPid(journal), 123);
  } finally {
    await removeTestDirectory(root);
  }
});

test("only a complete startup record with a positive integer PID can be used", async () => {
  const root = await mkdtemp(join(tmpdir(), "inhibitor-cleanup-"));
  try {
    const journal = join(root, "events.jsonl");
    for (const text of [
      '{"kind":"run_started","data":{"pid":123}}',
      '{"kind":"output","data":{"pid":123}}\n',
      ...[0, -1, "123", null].map((pid) => `${JSON.stringify({ kind: "run_started", data: { pid } })}\n`),
    ]) {
      await writeFile(journal, text);
      assert.equal(await readStartedPid(journal), undefined);
    }
  } finally {
    await removeTestDirectory(root);
  }
});

test("cleanup terminates owned processes even when the last journal line is partial", async () => {
  const root = await mkdtemp(join(tmpdir(), "inhibitor-cleanup-"));
  const launch = () => spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], {
    stdio: "ignore", windowsHide: true,
  });
  const pi = launch();
  const launcher = launch();
  const closed = Promise.all([once(pi, "close"), once(launcher, "close")]);
  let deadline;
  try {
    const journal = join(root, "events.jsonl");
    await writeFile(journal, `${JSON.stringify({ kind: "run_started", data: { pid: pi.pid } })}\n{"unfinished":`);
    await stopRun(launcher, journal);
    await Promise.race([closed, new Promise((_, reject) => {
      deadline = setTimeout(() => reject(new Error("owned processes were not stopped")), 3000);
    })]);
  } finally {
    clearTimeout(deadline);
    pi.kill();
    launcher.kill();
    await closed;
    await removeTestDirectory(root);
  }
});
