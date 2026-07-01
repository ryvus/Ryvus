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
import type { InferSchema, InferShape, Schema } from "./schema.js";

export type ApiActionHandler =
  | ((event: JsonValue) => JsonValue | Promise<JsonValue>)
  | ((
      event: JsonValue,
      context: InvocationContext,
    ) => JsonValue | Promise<JsonValue>);

type QueryShape = Record<string, Schema>;
type BodySchema = Schema | undefined;
type ResponseSchema = Schema | undefined;

export interface ApiActionInput<
  Query extends QueryShape = QueryShape,
  Body extends BodySchema = undefined,
> {
  path: Record<string, string>;
  query: InferShape<Query>;
  body: Body extends Schema ? InferSchema<Body> : JsonValue | null;
  context: InvocationContext;
  event: JsonValue;
}

export type BoundApiActionHandler<
  Query extends QueryShape = QueryShape,
  Body extends BodySchema = undefined,
  Response extends ResponseSchema = undefined,
> = (
  input: ApiActionInput<Query, Body>,
) => Response extends Schema ? InferSchema<Response> | Promise<InferSchema<Response>> : JsonValue | Promise<JsonValue>;

export interface ApiActionOptions<
  Query extends QueryShape = QueryShape,
  Body extends BodySchema = undefined,
  Response extends ResponseSchema = undefined,
> {
  method?: string;
  path?: string;
  query?: Query;
  body?: Body;
  response?: Response;
}

export interface ApiActionDefinition {
  __ryvusAction: true;
  type: "api";
  method: string;
  path: string;
  query: QueryShape;
  body?: Schema;
  response?: Schema;
  handler: ApiActionHandler | BoundApiActionHandler;
}

export interface ScheduledActionInput {
  context: InvocationContext;
  event: JsonValue;
}

export type ScheduledActionHandler = (
  input: ScheduledActionInput,
) => JsonValue | Promise<JsonValue>;

export interface ScheduledActionDefinition {
  __ryvusAction: true;
  type: "schedule";
  expression: string;
  handler: ScheduledActionHandler;
}

export function apiAction(handler: ApiActionHandler): ApiActionDefinition;
export function apiAction(
  handler: ApiActionHandler,
  options: ApiActionOptions,
): ApiActionDefinition;
export function apiAction<
  Query extends QueryShape = QueryShape,
  Body extends BodySchema = undefined,
  Response extends ResponseSchema = undefined,
>(
  options: ApiActionOptions<Query, Body, Response> & {
    handler: BoundApiActionHandler<Query, Body, Response>;
  },
): ApiActionDefinition;
export function apiAction(
  handlerOrOptions: ApiActionHandler | (ApiActionOptions & { handler: BoundApiActionHandler }),
  maybeOptions: ApiActionOptions = {},
): ApiActionDefinition {
  const handler =
    typeof handlerOrOptions === "function"
      ? handlerOrOptions
      : handlerOrOptions.handler;
  const options =
    typeof handlerOrOptions === "function" ? maybeOptions : handlerOrOptions;

  const action: ApiActionDefinition = {
    __ryvusAction: true,
    type: "api",
    method: options.method ?? "GET",
    path: options.path ?? "/",
    query: options.query ?? {},
    handler,
  };

  if (options.body !== undefined) {
    action.body = options.body;
  }

  if (options.response !== undefined) {
    action.response = options.response;
  }

  if (process.env.RYVUS_DISCOVER !== "1") {
    void runApiAction(handler);
  }

  return action;
}

export function scheduledAction(options: {
  every: string;
  handler: ScheduledActionHandler;
}): ScheduledActionDefinition {
  const action: ScheduledActionDefinition = {
    __ryvusAction: true,
    type: "schedule",
    expression: options.every.startsWith("every ")
      ? options.every
      : `every ${options.every}`,
    handler: options.handler,
  };

  if (process.env.RYVUS_DISCOVER !== "1") {
    void runScheduledAction(options.handler);
  }

  return action;
}

async function runApiAction(handler: ApiActionHandler | BoundApiActionHandler): Promise<void> {
  let request: InvocationRequest | null = null;

  try {
    request = await readInvocationRequest();

    installConsoleCapture(request.invocation_id);

    const context = createInvocationContext(request);
    const output = await callHandler(handler, request, context);

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

async function runScheduledAction(handler: ScheduledActionHandler): Promise<void> {
  let request: InvocationRequest | null = null;

  try {
    request = await readInvocationRequest();

    installConsoleCapture(request.invocation_id);

    const context = createInvocationContext(request);
    const output = await handler({
      context,
      event: request.event,
    });

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

async function callHandler(
  handler: ApiActionHandler | BoundApiActionHandler,
  request: InvocationRequest,
  context: InvocationContext,
): Promise<JsonValue> {
  const event = eventObject(request.event);

  if (handler.length >= 2) {
    return await (handler as ApiActionHandler)(request.event, context);
  }

  return await (handler as BoundApiActionHandler)({
    path: event.path_params ?? {},
    query: coerceQuery(event.query_params ?? {}),
    body: event.body ?? null,
    context,
    event: request.event,
  });
}

function eventObject(event: JsonValue): {
  body?: JsonValue;
  query_params?: Record<string, string>;
  path_params?: Record<string, string>;
} {
  if (typeof event === "object" && event !== null && !Array.isArray(event)) {
    return event as {
      body?: JsonValue;
      query_params?: Record<string, string>;
      path_params?: Record<string, string>;
    };
  }

  return { body: event };
}

function coerceQuery(query: Record<string, string>): Record<string, JsonValue> {
  const output: Record<string, JsonValue> = {};

  for (const [key, value] of Object.entries(query)) {
    if (/^-?\d+$/.test(value)) {
      output[key] = Number.parseInt(value, 10);
    } else if (/^-?\d+\.\d+$/.test(value)) {
      output[key] = Number.parseFloat(value);
    } else if (["true", "false"].includes(value.toLowerCase())) {
      output[key] = value.toLowerCase() === "true";
    } else {
      output[key] = value;
    }
  }

  return output;
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

  console.trace = (...values: unknown[]) => {
    writeInvocationMessage(createLogMessage(invocationId, "trace", values));
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
