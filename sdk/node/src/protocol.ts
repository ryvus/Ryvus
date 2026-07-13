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
  retryable: boolean;
  details: JsonValue;
}

export interface InvocationResult {
  protocol_version: string;
  invocation_id: string;
  status: "success" | "failed";
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
    status: "failed",
    output: null,
    error: {
      code: "handler_error",
      message: error instanceof Error ? error.message : String(error),
      retryable: false,
      details: {},
    },
  };
}
