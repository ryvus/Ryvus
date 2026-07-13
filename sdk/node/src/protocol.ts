export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export interface InvocationRequest {
  protocol_version: string;
  execution_id: string;
  attempt_id: string;
  attempt_number: number;
  event: JsonValue;
  context?: {
    metadata?: JsonValue;
  };
}

export interface InvocationContext {
  executionId: string;
  attemptId: string;
  attemptNumber: number;
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
  execution_id: string;
  attempt_id: string;
  attempt_number: number;
  status: "success" | "failed";
  output: JsonValue | null;
  error: InvocationError | null;
}

export interface LogEvent {
  type: "log";
  execution_id: string;
  attempt_id: string;
  attempt_number: number;
  level: "debug" | "info" | "warn" | "error" | "trace";
  message: string;
  fields: JsonValue;
}

export type InvocationEvent = LogEvent;

export function createInvocationContext(
  request: InvocationRequest,
): InvocationContext {
  return {
    executionId: request.execution_id,
    attemptId: request.attempt_id,
    attemptNumber: request.attempt_number,
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
    execution_id: request.execution_id,
    attempt_id: request.attempt_id,
    attempt_number: request.attempt_number,
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
    execution_id: request.execution_id,
    attempt_id: request.attempt_id,
    attempt_number: request.attempt_number,
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
