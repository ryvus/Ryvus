import contextlib
import inspect
import io
import json
import sys
import traceback
from typing import Any, Callable

from .context import Context
from .events import ApiEvent


Handler = Callable[..., dict[str, Any]]


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

    event = ApiEvent(
        body=request.get("event") or {},
    )

    context = Context(
        invocation_id=request["invocation_id"],
        protocol_version=request["protocol_version"],
        metadata=request.get("metadata") or {},
    )

    try:
        output = _call_handler(handler, event, context)

        result = {
            "protocol_version": request["protocol_version"],
            "invocation_id": request["invocation_id"],
            "status": "success",
            "output": output,
            "error": None,
        }

    except Exception as exc:
        result = {
            "protocol_version": request["protocol_version"],
            "invocation_id": request["invocation_id"],
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

    messages: list[dict[str, Any]] = []

    logs = captured_stdout.getvalue().strip()

    if logs:
        for line in logs.splitlines():
            messages.append(
                {
                    "type": "event",
                    "event": {
                        "type": "log",
                        "invocation_id": request["invocation_id"],
                        "level": "info",
                        "message": line,
                        "fields": {},
                    },
                }
            )

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
) -> dict[str, Any]:
    parameters = inspect.signature(handler).parameters

    if len(parameters) == 1:
        return handler(event)

    if len(parameters) == 2:
        return handler(event, context)

    raise TypeError(
        "Ryvus action handler must accept either event or event, context"
    )