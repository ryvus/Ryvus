import assert from "node:assert/strict";
import { once } from "node:events";
import test from "node:test";
import { createRuntimeServer } from "./api.js";
import type { InvocationRequest, InvocationResult } from "./protocol.js";

test("runtime rejects a concurrent invocation", async () => {
  let release!: () => void;
  const blocked = new Promise<void>((resolve) => {
    release = resolve;
  });
  let started!: () => void;
  const invocationStarted = new Promise<void>((resolve) => {
    started = resolve;
  });
  const server = createRuntimeServer(async (request): Promise<InvocationResult> => {
    started();
    await blocked;
    return success(request);
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert(address !== null && typeof address === "object");
  const endpoint = `http://127.0.0.1:${address.port}`;
  const first = fetch(`${endpoint}/invoke`, request("inv_1"));

  try {
    await invocationStarted;
    const health = await fetch(`${endpoint}/health`);
    assert.deepEqual(await health.json(), { status: "ready", busy: true });

    const second = await fetch(`${endpoint}/invoke`, request("inv_2"));
    assert.equal(second.status, 409);
    assert.equal((await second.json() as { code: string }).code, "RUNTIME_BUSY");
  } finally {
    release();
    assert.equal((await first).status, 200);
    server.close();
    await once(server, "close");
  }
});

test("runtime rejects missing attempt identity", async () => {
  const server = createRuntimeServer(async (request): Promise<InvocationResult> => success(request));
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert(address !== null && typeof address === "object");

  try {
    const invalid = JSON.parse(request("attempt_1").body as string) as Record<string, unknown>;
    delete invalid.attempt_number;
    const response = await fetch(`http://127.0.0.1:${address.port}/invoke`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(invalid),
    });
    assert.equal(response.status, 400);
  } finally {
    server.close();
    await once(server, "close");
  }
});

function request(attemptId: string): RequestInit {
  return {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      protocol_version: "ryvus.invoke.v2",
      execution_id: "execution_1",
      attempt_id: attemptId,
      attempt_number: 1,
      event: {},
      context: { metadata: {} },
    }),
  };
}

function success(request: InvocationRequest): InvocationResult {
  return {
    protocol_version: request.protocol_version,
    execution_id: request.execution_id,
    attempt_id: request.attempt_id,
    attempt_number: request.attempt_number,
    status: "success",
    output: {},
    error: null,
  };
}
