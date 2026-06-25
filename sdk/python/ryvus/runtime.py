import contextlib
import inspect
import io
import json
import logging
import sys
import traceback
from typing import Any, Callable

from .context import Context
from .events import ApiEvent
from .resolver import resolve_handler_args

Handler = Callable[..., dict[str, Any]]


class _InvocationLogHandler(logging.Handler):
    def __init__(self, invocation_id: str) -> None:
        super().__init__()
        self.invocation_id = invocation_id
        self.messages: list[dict[str, Any]] = []

    def emit(self, record: logging.LogRecord) -> None:
        self.messages.append(
            {
                "type": "event",
                "event": {
                    "type": "log",
                    "invocation_id": self.invocation_id,
                    "level": _map_python_log_level(record.levelno),
                    "message": record.getMessage(),
                    "fields": {
                        "logger": record.name,
                    },
                },
            }
        )


def run_api(handler: Handler) -> None:
    protocol_stdout = sys.stdout
    captured_stdout = io.StringIO()

    with contextlib.redirect_stdout(captured_stdout):
        request = json.load(sys.stdin)
        messages = handle_api_request(
            request=request,
            handler=handler,
            captured_stdout=captured_stdout,
        )

    for message in messages:
        json.dump(message, protocol_stdout)
        protocol_stdout.write("\n")
        protocol_stdout.flush()


def handle_api_request(
    request: dict[str, Any],
    handler: Handler,
    captured_stdout: io.StringIO | None = None,
) -> list[dict[str, Any]]:
    captured_stdout = captured_stdout or io.StringIO()

    invocation_id = request["invocation_id"]

    log_handler = _InvocationLogHandler(invocation_id)

    root_logger = logging.getLogger()
    root_logger.addHandler(log_handler)

    raw_event = request.get("event") or {}

    event = ApiEvent(
        body=raw_event.get("body"),
        query_params=raw_event.get("query_params") or {},
        path_params=raw_event.get("path_params") or {},
    )

    context = Context(
        invocation_id=invocation_id,
        protocol_version=request["protocol_version"],
        metadata=request.get("metadata") or {},
    )

    try:
        output =_serialize_output(_call_handler(handler, event, context))

        result = {
            "protocol_version": request["protocol_version"],
            "invocation_id": invocation_id,
            "status": "success",
            "output": output,
            "error": None,
        }

    except Exception as exc:
        result = {
            "protocol_version": request["protocol_version"],
            "invocation_id": invocation_id,
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

    finally:
        root_logger.removeHandler(log_handler)

    messages: list[dict[str, Any]] = []

    logs = captured_stdout.getvalue().strip()

    if logs:
        for line in logs.splitlines():
            messages.append(
                {
                    "type": "event",
                    "event": {
                        "type": "log",
                        "invocation_id": invocation_id,
                        "level": "info",
                        "message": line,
                        "fields": {
                            "source": "stdout",
                        },
                    },
                }
            )

    messages.extend(log_handler.messages)

    messages.append(
        {
            "type": "result",
            "result": result,
        }
    )

    return messages


def _call_handler(
    handler: Handler,
    event: ApiEvent,
    context: Context,
) -> Any:
    kwargs = resolve_handler_args(handler, event, context)
    return handler(**kwargs)


def _map_python_log_level(level: int) -> str:
    if level <= logging.DEBUG:
        return "debug"

    if level <= logging.INFO:
        return "info"

    if level <= logging.WARNING:
        return "warn"

    return "error"

def _serialize_output(value: Any) -> Any:
    if hasattr(value, "model_dump"):
        return value.model_dump()

    if hasattr(value, "dict"):
        return value.dict()

    return value