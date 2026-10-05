import { pipeline } from "node:stream/promises";

// Observe stream failures immediately, including file-open errors before child exit.
export async function captureOutput(child, stdout, stderr, { stop, timeoutMs = 30000 }) {
  const closed = new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", resolve);
  });
  const completed = Promise.all([
    closed,
    pipeline(child.stdout, stdout),
    pipeline(child.stderr, stderr),
  ]);
  let timeout;
  try {
    const [exitCode] = await Promise.race([
      completed,
      new Promise((_, reject) => {
        timeout = setTimeout(() => reject(new Error(`Pi run timed out after ${timeoutMs} ms`)), timeoutMs);
      }),
    ]);
    return exitCode;
  } catch (error) {
    try {
      await stop();
    } catch (cleanupError) {
      throw new AggregateError([error, cleanupError], "Pi capture failed and cleanup could not be confirmed");
    } finally {
      child.stdout.destroy();
      child.stderr.destroy();
      stdout.destroy();
      stderr.destroy();
    }
    throw error;
  } finally {
    clearTimeout(timeout);
  }
}
