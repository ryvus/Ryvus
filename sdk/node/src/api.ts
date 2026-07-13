import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import {
  createFailureResult,
  createInvocationContext,
  createSuccessResult,
  type InvocationContext,
  type InvocationRequest,
  type InvocationResult,
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
type MediaTypeInput = string | string[];

export interface RetryPolicyInput {
  max_attempts?: number;
  initial_delay?: string;
  backoff?: number;
}

export interface ActionPolicyInput {
  timeout?: string;
  retry?: RetryPolicyInput;
}

export interface ActionExecutionPolicy {
  timeout: string;
  retry: {
    max_attempts: number;
    initial_delay: string;
    backoff: number;
  };
}

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

export interface AuthorizerInput {
  body: JsonValue | null;
  path_params: Record<string, string>;
  query_params: Record<string, string>;
  headers: Record<string, string>;
  method: string;
  path: string;
  context: InvocationContext;
  event: JsonValue;
}

export type AuthorizerHandler = (
  input: AuthorizerInput,
) => JsonValue | Promise<JsonValue>;

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
  name?: string;
  method?: string;
  path?: string;
  query?: Query;
  body?: Body;
  response?: Response;
  consumes?: MediaTypeInput;
  produces?: MediaTypeInput;
  authorizer?: string;
  timeout?: string;
  retry?: RetryPolicyInput;
}

export interface ApiActionDefinition {
  __ryvusAction: true;
  type: "api";
  name?: string;
  method: string;
  path: string;
  query: QueryShape;
  body?: Schema;
  response?: Schema;
  consumes?: string[];
  produces?: string[];
  authorizer?: string;
  policy?: ActionPolicyInput;
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
  name?: string;
  expression: string;
  policy?: ActionPolicyInput;
  handler: ScheduledActionHandler;
}

export interface AuthorizerDefinition {
  __ryvusAction: true;
  type: "authorizer";
  name: string;
  security?: AuthorizerSecurity[];
  parameters?: AuthorizerParameter[];
  cache?: AuthorizerCache;
  policy?: ActionPolicyInput;
  handler: AuthorizerHandler;
}

export interface AuthorizerSecurity {
  type: string;
  scheme?: string;
  in?: "header" | "query" | "cookie";
  name?: string;
}

export interface AuthorizerParameter {
  name: string;
  in: "header" | "query" | "cookie";
  required?: boolean;
  type?: string;
}

export interface AuthorizerCache {
  ttl_seconds: number;
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
    ...(options.name ? { name: options.name } : {}),
    method: options.method ?? "GET",
    path: options.path ?? "/",
    query: options.query ?? {},
    handler,
  };
  const policy = actionPolicy(options.timeout, options.retry);
  if (policy !== undefined) {
    action.policy = policy;
  }

  if (options.body !== undefined) {
    action.body = options.body;
  }

  if (options.response !== undefined) {
    action.response = options.response;
  }

  if (options.consumes !== undefined) {
    action.consumes = normalizeMediaTypes(options.consumes);
  }

  if (options.produces !== undefined) {
    action.produces = normalizeMediaTypes(options.produces);
  }

  if (options.authorizer !== undefined) {
    action.authorizer = options.authorizer;
  }

  if (process.env.RYVUS_DISCOVER !== "1") {
    void runApiAction(handler);
  }

  return action;
}

function normalizeMediaTypes(value: MediaTypeInput): string[] {
  return typeof value === "string" ? [value] : value;
}

export function scheduledAction(options: {
  name?: string;
  every: string;
  timeout?: string;
  retry?: RetryPolicyInput;
  handler: ScheduledActionHandler;
}): ScheduledActionDefinition {
  const action: ScheduledActionDefinition = {
    __ryvusAction: true,
    type: "schedule",
    ...(options.name ? { name: options.name } : {}),
    expression: options.every.startsWith("every ")
      ? options.every
      : `every ${options.every}`,
    handler: options.handler,
  };
  const policy = actionPolicy(options.timeout, options.retry);
  if (policy !== undefined) {
    action.policy = policy;
  }

  if (process.env.RYVUS_DISCOVER !== "1") {
    void runScheduledAction(options.handler);
  }

  return action;
}

export function authorizer(options: {
  name: string;
  security?: AuthorizerSecurity | AuthorizerSecurity[];
  parameters?: AuthorizerParameter[];
  cacheTtlSeconds?: number;
  timeout?: string;
  retry?: RetryPolicyInput;
  handler: AuthorizerHandler;
}): AuthorizerDefinition {
  const action: AuthorizerDefinition = {
    __ryvusAction: true,
    type: "authorizer",
    name: options.name,
    handler: options.handler,
  };
  if (options.security !== undefined) {
    action.security = normalizeAuthorizerSecurity(options.security);
  }
  if (options.parameters !== undefined) {
    action.parameters = normalizeAuthorizerParameters(options.parameters);
  }
  if (options.cacheTtlSeconds !== undefined) {
    action.cache = { ttl_seconds: options.cacheTtlSeconds };
  }
  const policy = actionPolicy(options.timeout, options.retry);
  if (policy !== undefined) {
    action.policy = policy;
  }

  if (process.env.RYVUS_DISCOVER !== "1") {
    void runAuthorizer(options.handler);
  }

  return action;
}

function normalizeAuthorizerSecurity(
  value: AuthorizerSecurity | AuthorizerSecurity[],
): AuthorizerSecurity[] {
  return Array.isArray(value) ? value : [value];
}

function normalizeAuthorizerParameters(
  parameters: AuthorizerParameter[],
): AuthorizerParameter[] {
  return parameters.map((parameter) => ({
    ...parameter,
    type: parameter.type ?? "string",
  }));
}

function actionPolicy(
  timeout: string | undefined,
  retry: RetryPolicyInput | undefined,
): ActionPolicyInput | undefined {
  if (timeout === undefined && retry === undefined) {
    return undefined;
  }

  return {
    ...(timeout !== undefined ? { timeout } : {}),
    ...(retry !== undefined ? { retry } : {}),
  };
}

function runApiAction(handler: ApiActionHandler | BoundApiActionHandler): void {
  startRuntime(async (request) => {
    try {
      const context = createInvocationContext(request);
      const output = await callHandler(handler, request, context);
      return createSuccessResult(request, output);
    } catch (error) {
      return createFailureResult(request, error);
    }
  });
}

function runScheduledAction(handler: ScheduledActionHandler): void {
  startRuntime(async (request) => {
    try {
      const context = createInvocationContext(request);
      const output = await handler({ context, event: request.event });
      return createSuccessResult(request, output);
    } catch (error) {
      return createFailureResult(request, error);
    }
  });
}

function runAuthorizer(handler: AuthorizerHandler): void {
  startRuntime(async (request) => {
    try {
      const context = createInvocationContext(request);
      const event = eventObject(request.event);
      const output = await handler({
        body: event.body ?? null,
        path_params: event.path_params ?? {},
        query_params: event.query_params ?? {},
        headers: event.headers ?? {},
        method: event.method ?? "",
        path: event.path ?? "",
        context,
        event: request.event,
      });
      return createSuccessResult(request, output);
    } catch (error) {
      return createFailureResult(request, error);
    }
  });
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
  headers?: Record<string, string>;
  method?: string;
  path?: string;
} {
  if (typeof event === "object" && event !== null && !Array.isArray(event)) {
    return event as {
      body?: JsonValue;
      query_params?: Record<string, string>;
      path_params?: Record<string, string>;
      headers?: Record<string, string>;
      method?: string;
      path?: string;
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

function startRuntime(
  invoke: (request: InvocationRequest) => Promise<InvocationResult>,
): void {
  const server = createRuntimeServer(invoke);
  const host = process.env.RYVUS_RUNTIME_HOST ?? "127.0.0.1";
  const port = Number.parseInt(process.env.RYVUS_RUNTIME_PORT ?? "8080", 10);
  server.listen(port, host);
}

export function createRuntimeServer(
  invoke: (request: InvocationRequest) => Promise<InvocationResult>,
): Server {
  const state: { activeInvocationId: string | null } = {
    activeInvocationId: null,
  };
  return createServer((request, response) => {
    void handleRuntimeRequest(request, response, invoke, state);
  });
}

async function handleRuntimeRequest(
  request: IncomingMessage,
  response: ServerResponse,
  invoke: (request: InvocationRequest) => Promise<InvocationResult>,
  state: { activeInvocationId: string | null },
): Promise<void> {
  if (request.method === "GET" && request.url === "/health") {
    writeJson(response, 200, {
      status: "ready",
      busy: state.activeInvocationId !== null,
    });
    return;
  }
  if (request.url !== "/invoke") {
    writeJson(response, 404, { error: "not_found" });
    return;
  }
  if (request.method !== "POST") {
    writeJson(response, 405, { error: "method_not_allowed" });
    return;
  }
  if (String(request.headers["content-type"] ?? "").split(";", 1)[0] !== "application/json") {
    writeJson(response, 415, { error: "unsupported_media_type" });
    return;
  }

  try {
    const invocation = JSON.parse(await readBody(request)) as InvocationRequest;
    validateInvocationRequest(invocation);
    if (state.activeInvocationId !== null) {
      writeJson(response, 409, {
        code: "RUNTIME_BUSY",
        message: "This runtime instance is already processing an invocation.",
      });
      return;
    }

    state.activeInvocationId = invocation.invocation_id;
    try {
      writeJson(response, 200, await invoke(invocation));
    } finally {
      state.activeInvocationId = null;
    }
  } catch (error) {
    writeJson(response, 400, {
      error: "invalid_request",
      message: error instanceof Error ? error.message : String(error),
    });
  }
}

function validateInvocationRequest(request: InvocationRequest): void {
  if (request === null || typeof request !== "object" || Array.isArray(request)) {
    throw new Error("request must be a JSON object");
  }
  if (request.protocol_version !== "ryvus.invoke.v1") {
    throw new Error("unsupported protocol_version");
  }
  if (typeof request.invocation_id !== "string" || request.invocation_id.length === 0) {
    throw new Error("invocation_id is required");
  }
  if (!("event" in request)) {
    throw new Error("event is required");
  }
}

async function readBody(request: IncomingMessage): Promise<string> {
  return await new Promise((resolve, reject) => {
    let body = "";
    request.setEncoding("utf8");
    request.on("data", (chunk: string) => { body += chunk; });
    request.on("end", () => { resolve(body); });
    request.on("error", reject);
  });
}

function writeJson(response: ServerResponse, status: number, body: unknown): void {
  const payload = JSON.stringify(body);
  response.writeHead(status, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(payload),
  });
  response.end(payload);
}
