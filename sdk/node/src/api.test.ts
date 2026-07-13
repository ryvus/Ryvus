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

function request(invocationId: string): RequestInit {
  return {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      protocol_version: "ryvus.invoke.v1",
      invocation_id: invocationId,
      event: {},
      context: { metadata: {} },
    }),
  };
}

function success(request: InvocationRequest): InvocationResult {
  return {
    protocol_version: request.protocol_version,
    invocation_id: request.invocation_id,
    status: "success",
    output: {},
    error: null,
  };
}
