import contextlib
import io
import json
import logging
import sys
import time
import traceback
from typing import Any, Callable

from .context import Context
from .events import ApiEvent, AuthorizerEvent, FlowEvent, ScheduleEvent
from .resolver import resolve_handler_args

Handler = Callable[..., dict[str, Any]]


def run_api(handler: Handler) -> None:
    run_handler(handler, event_from_api_request)


def run_authorizer(handler: Handler) -> None:
    run_handler(handler, event_from_authorizer_request)


def run_schedule(handler: Handler) -> None:
    run_handler(handler, event_from_schedule_request)


def run_flow(handler: Handler) -> None:
    run_handler(handler, event_from_flow_request)


def run_handler(handler: Handler, event_factory) -> None:
    run_framed_worker(handler, event_factory)


def run_framed_worker(handler: Handler, event_factory) -> None:
    protocol_stdout = sys.stdout
    _write_frame(protocol_stdout, {"type": "ready"})

    request = json.loads(sys.stdin.readline())
    validate_invocation_request(request)
    captured_stdout = io.StringIO()
    log_handler = _InvocationLogHandler(request)
    root_logger = logging.getLogger()
    root_logger.addHandler(log_handler)
    try:
        with contextlib.redirect_stdout(captured_stdout):
            result = handle_request(request, handler, event_factory)
    finally:
        root_logger.removeHandler(log_handler)

    for line in captured_stdout.getvalue().splitlines():
        _write_frame(
            protocol_stdout,
            _log_frame(
                request,
                line,
                {"source": "stdout"},
                timestamp_unix_nanos=time.time_ns(),
            ),
        )
    for frame in log_handler.frames:
        _write_frame(protocol_stdout, frame)
    _write_frame(protocol_stdout, {"type": "result", "result": result})


class _InvocationLogHandler(logging.Handler):
    def __init__(self, request: dict[str, Any]) -> None:
        super().__init__()
        self.request = request
        self.frames: list[dict[str, Any]] = []

    def emit(self, record: logging.LogRecord) -> None:
        self.frames.append(
            _log_frame(
                self.request,
                record.getMessage(),
                {"logger": record.name},
                _log_level(record.levelno),
                timestamp_unix_nanos=int(record.created * 1_000_000_000),
                trace_id=getattr(record, "trace_id", None),
                span_id=getattr(record, "span_id", None),
            )
        )


def _log_frame(
    request: dict[str, Any],
    message: str,
    fields: dict[str, Any],
    level: str = "info",
    timestamp_unix_nanos: int | None = None,
    trace_id: str | None = None,
    span_id: str | None = None,
) -> dict[str, Any]:
    event = {
        "type": "log",
        "execution_id": request["execution_id"],
        "attempt_id": request["attempt_id"],
        "attempt_number": request["attempt_number"],
        "level": level,
        "message": message,
        "fields": fields,
    }
    if timestamp_unix_nanos is not None:
        event["timestamp_unix_nanos"] = timestamp_unix_nanos
    if trace_id is not None:
        event["trace_id"] = trace_id
    if span_id is not None:
        event["span_id"] = span_id
    return {
        "type": "event",
        "event": event,
    }


def _log_level(level: int) -> str:
    if level <= logging.DEBUG:
        return "debug"
    if level <= logging.INFO:
        return "info"
    if level <= logging.WARNING:
        return "warn"
    return "error"


def _write_frame(stream, frame: dict[str, Any]) -> None:
    json.dump(frame, stream, separators=(",", ":"))
    stream.write("\n")
    stream.flush()


def validate_invocation_request(request: Any) -> None:
    if not isinstance(request, dict):
        raise ValueError("request must be a JSON object")
    if request.get("protocol_version") != "ryvus.invoke.v3":
        raise ValueError("unsupported protocol_version")
    if not isinstance(request.get("execution_id"), str) or not request["execution_id"]:
        raise ValueError("execution_id is required")
    if not isinstance(request.get("attempt_id"), str) or not request["attempt_id"]:
        raise ValueError("attempt_id is required")
    if not isinstance(request.get("attempt_number"), int) or request["attempt_number"] < 1:
        raise ValueError("attempt_number must be a positive integer")
    if not isinstance(request.get("deadline_unix_ms"), int) or isinstance(
        request["deadline_unix_ms"], bool
    ):
        raise ValueError("deadline_unix_ms must be an integer")
    if not isinstance(request.get("remaining_budget_ms"), int) or isinstance(
        request["remaining_budget_ms"], bool
    ):
        raise ValueError("remaining_budget_ms must be an integer")
    if request["remaining_budget_ms"] < 1:
        raise ValueError("remaining_budget_ms must be positive")
    if "event" not in request:
        raise ValueError("event is required")
    if "context" in request and not isinstance(request["context"], dict):
        raise ValueError("context must be an object")


def handle_api_request(
    request: dict[str, Any],
    handler: Handler,
) -> dict[str, Any]:
    return handle_request(
        request=request,
        handler=handler,
        event_factory=event_from_api_request,
    )


def handle_authorizer_request(
    request: dict[str, Any],
    handler: Handler,
) -> dict[str, Any]:
    return handle_request(
        request=request,
        handler=handler,
        event_factory=event_from_authorizer_request,
    )


def handle_request(
    request: dict[str, Any],
    handler: Handler,
    event_factory,
) -> dict[str, Any]:
    execution_id = request["execution_id"]
    attempt_id = request["attempt_id"]
    attempt_number = request["attempt_number"]

    event = event_factory(request.get("event") or {})

    context = Context(
        execution_id=execution_id,
        attempt_id=attempt_id,
        attempt_number=attempt_number,
        protocol_version=request["protocol_version"],
        metadata=_request_metadata(request),
    )

    try:
        output =_serialize_output(_call_handler(handler, event, context))

        result = {
            "protocol_version": request["protocol_version"],
            "execution_id": execution_id,
            "attempt_id": attempt_id,
            "attempt_number": attempt_number,
            "status": "success",
            "output": output,
            "error": None,
        }

    except Exception as exc:
        result = {
            "protocol_version": request["protocol_version"],
            "execution_id": execution_id,
            "attempt_id": attempt_id,
            "attempt_number": attempt_number,
            "status": "failed",
            "output": None,
            "error": {
                "code": exc.__class__.__name__,
                "message": str(exc),
                "retryable": False,
                "details": {
                    "traceback": traceback.format_exc(),
                },
            },
        }

    return result


def _call_handler(
    handler: Handler,
    event: ApiEvent,
    context: Context,
) -> Any:
    kwargs = resolve_handler_args(handler, event, context)
    return handler(**kwargs)


def event_from_api_request(raw_event: dict[str, Any]) -> ApiEvent:
    return ApiEvent(
        body=raw_event.get("body"),
        query_params=raw_event.get("query_params") or {},
        path_params=raw_event.get("path_params") or {},
    )


def event_from_authorizer_request(raw_event: dict[str, Any]) -> AuthorizerEvent:
    return AuthorizerEvent(
        body=raw_event.get("body"),
        query_params=raw_event.get("query_params") or {},
        path_params=raw_event.get("path_params") or {},
        headers=raw_event.get("headers") or {},
        method=raw_event.get("method") or "",
        path=raw_event.get("path") or "",
    )


def event_from_schedule_request(raw_event: dict[str, Any]) -> ScheduleEvent:
    return ScheduleEvent(
        trigger=raw_event.get("trigger") or "schedule",
        scheduled_at=raw_event.get("scheduled_at"),
        expression=raw_event.get("expression") or "",
    )


def event_from_flow_request(raw_event: Any) -> FlowEvent:
    return FlowEvent(data=raw_event)


def _request_metadata(request: dict[str, Any]) -> dict[str, Any]:
    context = request.get("context")

    if isinstance(context, dict) and isinstance(context.get("metadata"), dict):
        return context["metadata"]

    return request.get("metadata") or {}


def _serialize_output(value: Any) -> Any:
    if hasattr(value, "model_dump"):
        return value.model_dump()

    if hasattr(value, "dict"):
        return value.dict()

    return value
