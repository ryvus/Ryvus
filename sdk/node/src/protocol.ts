export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export interface InvocationRequest {
  protocol_version: string;
  invocation_id: string;
  event: JsonValue;
  context?: {
    metadata?: JsonValue;
  };
}

export interface InvocationContext {
  invocationId: string;
  protocolVersion: string;
  metadata: JsonValue | null;
}

export interface InvocationError {
  code: string;
  message: string;
  details?: JsonValue;
}

export interface InvocationResult {
  protocol_version: string;
  invocation_id: string;
  status: "success" | "failure";
  output: JsonValue | null;
  error: InvocationError | null;
}

export interface LogEvent {
  type: "log";
  invocation_id: string;
  level: "debug" | "info" | "warn" | "error" | "trace";
  message: string;
  fields: JsonValue;
}

export type InvocationEvent = LogEvent;

export type InvocationMessage =
  | {
      type: "event";
      event: InvocationEvent;
    }
  | {
      type: "result";
      result: InvocationResult;
    };

export function createInvocationContext(
  request: InvocationRequest,
): InvocationContext {
  return {
    invocationId: request.invocation_id,
    protocolVersion: request.protocol_version,
    metadata: request.context?.metadata ?? null,
  };
}

export function createSuccessResult(
  request: InvocationRequest,
  output: JsonValue,
): InvocationResult {
  return {
    protocol_version: request.protocol_version,
    invocation_id: request.invocation_id,
    status: "success",
    output,
    error: null,
  };
}

export function createFailureResult(
  request: InvocationRequest,
  error: unknown,
): InvocationResult {
  return {
    protocol_version: request.protocol_version,
    invocation_id: request.invocation_id,
    status: "failure",
    output: null,
    error: {
      code: "handler_error",
      message: error instanceof Error ? error.message : String(error),
    },
  };
}

export function createLogMessage(
  invocationId: string,
  level: LogEvent["level"],
  values: unknown[],
): InvocationMessage {
  return {
    type: "event",
    event: {
      type: "log",
      invocation_id: invocationId,
      level,
      message: values.map(formatConsoleValue).join(" "),
      fields: {},
    },
  };
}

export function createResultMessage(
  result: InvocationResult,
): InvocationMessage {
  return {
    type: "result",
    result,
  };
}

function formatConsoleValue(value: unknown): string {
  if (typeof value === "string") {
    return value;
  }

  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}
