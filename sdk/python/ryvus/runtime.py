import inspect
import json
import sys
import traceback
from typing import Any, Callable
from .events import ApiEvent
from .context import Context


Handler = Callable[..., dict[str, Any]]

def run_api(handler):

    request = json.load(sys.stdin)

    event = ApiEvent(
        body=request.get("event") or {}
    )

    context = Context(
        invocation_id=request["invocation_id"],
        protocol_version=request["protocol_version"],
        metadata=request.get("metadata") or {},
    )
    try:
        output = _call_handler(handler, request.get("event") or {}, context)

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
            "status": "error",
            "output": None,
            "error": {
                "message": str(exc),
                "type": exc.__class__.__name__,
                "traceback": traceback.format_exc(),
            },
        }

    json.dump(result, sys.stdout)


def _call_handler(handler: Handler, event: dict[str, Any], context: Context) -> dict[str, Any]:
    parameters = inspect.signature(handler).parameters

    if len(parameters) == 1:
        return handler(event)

    if len(parameters) == 2:
        return handler(event, context)

    raise TypeError("Ryvus action handler must accept either event or event, context")