import {
  createFailureResult,
  createInvocationContext,
  createLogMessage,
  createResultMessage,
  createSuccessResult,
  type InvocationContext,
  type InvocationMessage,
  type InvocationRequest,
  type JsonValue,
} from "./protocol.js";

export type ApiActionHandler =
  | ((event: JsonValue) => JsonValue | Promise<JsonValue>)
  | ((
      event: JsonValue,
      context: InvocationContext,
    ) => JsonValue | Promise<JsonValue>);

export function apiAction(handler: ApiActionHandler): void {
  void runApiAction(handler);
}

async function runApiAction(handler: ApiActionHandler): Promise<void> {
  let request: InvocationRequest | null = null;

  try {
    request = await readInvocationRequest();

    installConsoleCapture(request.invocation_id);

    const context = createInvocationContext(request);
    const output = await handler(request.event, context);

    writeInvocationMessage(
      createResultMessage(createSuccessResult(request, output)),
    );
  } catch (error) {
    if (request === null) {
      throw error;
    }

    writeInvocationMessage(
      createResultMessage(createFailureResult(request, error)),
    );
  }
}

async function readInvocationRequest(): Promise<InvocationRequest> {
  const raw = await readStdin();

  if (raw.trim().length === 0) {
    throw new Error("No invocation request received on stdin");
  }

  return JSON.parse(raw) as InvocationRequest;
}

function writeInvocationMessage(message: InvocationMessage): void {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

function installConsoleCapture(invocationId: string): void {
  console.log = (...values: unknown[]) => {
    writeInvocationMessage(createLogMessage(invocationId, "info", values));
  };

  console.info = (...values: unknown[]) => {
    writeInvocationMessage(createLogMessage(invocationId, "info", values));
  };

  console.warn = (...values: unknown[]) => {
    writeInvocationMessage(createLogMessage(invocationId, "warn", values));
  };

  console.error = (...values: unknown[]) => {
    writeInvocationMessage(createLogMessage(invocationId, "error", values));
  };

  console.debug = (...values: unknown[]) => {
    writeInvocationMessage(createLogMessage(invocationId, "debug", values));
  };
}

async function readStdin(): Promise<string> {
  return new Promise((resolve, reject) => {
    let raw = "";

    process.stdin.setEncoding("utf8");

    process.stdin.on("data", (chunk: string) => {
      raw += chunk;
    });

    process.stdin.on("error", reject);

    process.stdin.on("end", () => {
      resolve(raw);
    });
  });
}
