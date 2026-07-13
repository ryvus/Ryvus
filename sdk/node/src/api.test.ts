import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import test from "node:test";
import type { InvocationRequest } from "./protocol.js";

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
      level: "info",
      message: "handling pet_1",
      fields: {},
    },
  });
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
