import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import test from "node:test";
import { scheduledAction } from "./api.js";
import type { InvocationRequest, LogEvent } from "./protocol.js";

test("scheduled actions retain an explicit stable key", () => {
  const previous = process.env.RYVUS_DISCOVER;
  process.env.RYVUS_DISCOVER = "1";
  const action = scheduledAction({
    key: "inventory-restock",
    every: "10s",
    handler: () => ({ ok: true }),
  });
  if (previous === undefined) {
    delete process.env.RYVUS_DISCOVER;
  } else {
    process.env.RYVUS_DISCOVER = previous;
  }

  assert.equal(action.key, "inventory-restock");
});

test("worker emits ready, structured logs, and a correlated result", async () => {
  const frames = await invokeWorker(`
    export default apiAction({
      path: "/pets/:id",
      handler: ({ path, query, body, context }) => {
        console.log("handling", path.id);
        return { path, query, body, attemptId: context.attemptId };
      },
    });
  `, request());

  assert.deepEqual(frames[0], { type: "ready" });
  assert.deepEqual(frames[1], {
    type: "event",
    event: {
      type: "log",
      execution_id: "execution_1",
      attempt_id: "attempt_1",
      attempt_number: 1,
      timestamp_unix_nanos: frames[1] && typeof frames[1] === "object"
        ? (frames[1] as { event: { timestamp_unix_nanos: number } }).event.timestamp_unix_nanos
        : 0,
      level: "info",
      message: "handling pet_1",
      fields: {},
    },
  });
  assert.equal(
    typeof (frames[1] as { event: { timestamp_unix_nanos: unknown } }).event.timestamp_unix_nanos,
    "number",
  );
  assert.deepEqual(frames[2], {
    type: "result",
    result: {
      protocol_version: "ryvus.invoke.v3",
      execution_id: "execution_1",
      attempt_id: "attempt_1",
      attempt_number: 1,
      status: "success",
      output: {
        path: { id: "pet_1" },
        query: { count: 2 },
        body: { name: "Milo" },
        attemptId: "attempt_1",
      },
      error: null,
    },
  });
});

test("log event metadata remains optional for legacy frames", () => {
  const legacy: LogEvent = {
    type: "log",
    execution_id: "execution_1",
    attempt_id: "attempt_1",
    attempt_number: 1,
    level: "info",
    message: "legacy",
    fields: {},
  };
  const traced: LogEvent = {
    ...legacy,
    timestamp_unix_nanos: 123,
    trace_id: "11".repeat(16),
    span_id: "22".repeat(8),
  };

  assert.equal(legacy.message, "legacy");
  assert.equal(traced.trace_id?.length, 32);
  assert.equal(traced.span_id?.length, 16);
});

test("worker serializes handler exceptions and exits cleanly", async () => {
  const frames = await invokeWorker(`
    export default apiAction(() => { throw new Error("boom"); });
  `, request());
  const terminal = frames.at(-1) as { type: string; result: { status: string; error: { message: string } } };

  assert.equal(terminal.type, "result");
  assert.equal(terminal.result.status, "failed");
  assert.equal(terminal.result.error.message, "boom");
});

async function invokeWorker(action: string, invocation: InvocationRequest): Promise<unknown[]> {
  const apiUrl = new URL("./api.js", import.meta.url).href;
  const source = `import { apiAction } from ${JSON.stringify(apiUrl)};\n${action}`;
  const child = spawn(process.execPath, ["--input-type=module", "--eval", source], {
    stdio: ["pipe", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk: string) => { stdout += chunk; });
  child.stderr.on("data", (chunk: string) => { stderr += chunk; });
  child.stdin.end(`${JSON.stringify(invocation)}\n`);
  const exitCode = await new Promise<number | null>((resolve, reject) => {
    child.once("error", reject);
    child.once("close", resolve);
  });
  assert.equal(exitCode, 0, stderr);
  return stdout.trim().split("\n").map((line) => JSON.parse(line) as unknown);
}

function request(): InvocationRequest {
  return {
    protocol_version: "ryvus.invoke.v3",
    execution_id: "execution_1",
    attempt_id: "attempt_1",
    attempt_number: 1,
    deadline_unix_ms: 4_102_444_800_000,
    remaining_budget_ms: 3_000,
    event: {
      path_params: { id: "pet_1" },
      query_params: { count: "2" },
      body: { name: "Milo" },
    },
    context: { metadata: {} },
  };
}
