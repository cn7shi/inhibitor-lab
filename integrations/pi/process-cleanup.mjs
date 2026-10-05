import { open } from "node:fs/promises";

export async function readStartedPid(journal) {
  // run_started is the first journal record. Later records may still be in flight.
  // Bound this read so cleanup does not load a potentially large journal.
  const file = await open(journal, "r");
  try {
    const buffer = Buffer.alloc(64 * 1024);
    const { bytesRead } = await file.read(buffer, 0, buffer.length, 0);
    const newline = buffer.subarray(0, bytesRead).indexOf(10);
    if (newline < 0) return undefined;
    const event = JSON.parse(buffer.toString("utf8", 0, newline));
    const pid = event.data?.pid;
    return event.kind === "run_started" && Number.isSafeInteger(pid) && pid > 0 ? pid : undefined;
  } finally {
    await file.close();
  }
}

export async function stopRun(child, journal) {
  try {
    const pid = await readStartedPid(journal);
    if (!pid) throw new Error("No complete startup PID; Pi cleanup could not be confirmed");
    try { process.kill(pid); }
    catch (error) { if (error.code !== "ESRCH") throw error; }
  } finally {
    child.kill();
  }
}
