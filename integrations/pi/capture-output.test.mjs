import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import { Writable } from "node:stream";
import { test } from "node:test";
import { captureOutput } from "./capture-output.mjs";

function start(t, script) {
  const child = spawn(process.execPath, ["-e", script], {
    stdio: ["ignore", "pipe", "pipe"], windowsHide: true,
  });
  const closed = new Promise((resolve) => child.once("close", resolve));
  t.after(async () => {
    child.kill();
    await closed;
  });
  return { child, closed };
}

function sink(write = (_chunk, _encoding, callback) => callback()) {
  return new Writable({ write });
}

const running = "process.stdout.write('out'); process.stderr.write('err'); setInterval(() => {}, 1000)";

test("capture waits for both outputs to drain and preserves the exit code", { timeout: 10000 }, async (t) => {
  const { child } = start(t, "process.stdout.write('out'); process.stderr.write('err'); process.exitCode = 7");
  const output = [[], []];
  const streams = output.map((chunks) => sink((chunk, _encoding, callback) => {
    setTimeout(() => { chunks.push(chunk.toString()); callback(); }, 20);
  }));
  const exitCode = await captureOutput(child, ...streams, {
    stop: () => assert.fail("successful capture must not invoke cleanup"),
  });
  assert.equal(exitCode, 7);
  assert.deepEqual(output.map((chunks) => chunks.join("")), ["out", "err"]);
});

for (const failedStream of ["stdout", "stderr"]) {
  test(`${failedStream} write failure stops a running child`, { timeout: 10000 }, async (t) => {
    const { child, closed } = start(t, running);
    const failure = new Error(`injected ${failedStream} write failure`);
    const streams = ["stdout", "stderr"].map((name) => name === failedStream
      ? sink((_chunk, _encoding, callback) => callback(failure)) : sink());
    let stops = 0;
    await assert.rejects(captureOutput(child, ...streams, {
      stop: () => { stops++; child.kill(); },
    }), (error) => error === failure);
    await closed;
    assert.equal(stops, 1);
    assert.ok(streams.every((stream) => stream.destroyed));
  });
}

test("a file-open error is caught before the child exits", { timeout: 10000 }, async (t) => {
  const { child, closed } = start(t, running);
  // Opening an existing directory as a file fails without writing or removing files.
  const output = createWriteStream(new URL(".", import.meta.url));
  let stops = 0;
  await assert.rejects(captureOutput(child, output, sink(), {
    stop: () => { stops++; child.kill(); },
  }), (error) => ["EISDIR", "EPERM", "EACCES"].includes(error.code));
  await closed;
  assert.equal(stops, 1);
});

test("timeout stops a child even when its output streams remain open", { timeout: 10000 }, async (t) => {
  const { child, closed } = start(t, running);
  let stops = 0;
  await assert.rejects(captureOutput(child, sink(), sink(), {
    timeoutMs: 100,
    stop: () => { stops++; child.kill(); },
  }), /timed out/);
  await closed;
  assert.equal(stops, 1);
});

test("cleanup failure preserves both the original and cleanup errors", { timeout: 10000 }, async (t) => {
  const { child, closed } = start(t, running);
  const failure = new Error("injected write failure");
  const cleanupFailure = new Error("injected cleanup failure");
  await assert.rejects(captureOutput(child,
    sink((_chunk, _encoding, callback) => callback(failure)), sink(), {
      stop: () => { child.kill(); throw cleanupFailure; },
    }), (error) => error instanceof AggregateError
      && error.errors[0] === failure && error.errors[1] === cleanupFailure);
  await closed;
});
