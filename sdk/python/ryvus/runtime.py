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
    request = json.load(sys.stdin)
    result = handle_api_request(request, handler)
    json.dump(result, sys.stdout)


def handle_api_request(
    request: dict[str, Any],
    handler: Handler,
) -> dict[str, Any]:
    event = ApiEvent(
        body=request.get("event") or {},
    )

    context = Context(
        invocation_id=request["invocation_id"],
        protocol_version=request["protocol_version"],
        metadata=request.get("metadata") or {},
    )

    captured_stdout = io.StringIO()

    try:
        with contextlib.redirect_stdout(captured_stdout):
            output = _call_handler(handler, event, context)

        return {
            "protocol_version": request["protocol_version"],
            "invocation_id": request["invocation_id"],
            "status": "success",
            "output": output,
            "logs": captured_stdout.getvalue(),
            "error": None,
        }

    except Exception as exc:
        return {
            "protocol_version": request["protocol_version"],
            "invocation_id": request["invocation_id"],
            "status": "error",
            "output": None,
            "logs": captured_stdout.getvalue(),
            "error": {
                "message": str(exc),
                "type": exc.__class__.__name__,
                "traceback": traceback.format_exc(),
            },
        }


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