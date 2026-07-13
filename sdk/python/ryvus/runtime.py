import json
import os
import threading
import traceback
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
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
    host = os.environ.get("RYVUS_RUNTIME_HOST", "127.0.0.1")
    port = int(os.environ.get("RYVUS_RUNTIME_PORT", "8080"))
    create_runtime_server(handler, event_factory, host, port).serve_forever()


def create_runtime_server(
    handler: Handler,
    event_factory,
    host: str = "127.0.0.1",
    port: int = 0,
) -> ThreadingHTTPServer:
    invocation_lock = threading.Lock()

    class RuntimeRequestHandler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            if self.path == "/health":
                self._write_json(
                    200,
                    {"status": "ready", "busy": invocation_lock.locked()},
                )
            else:
                self._write_json(404, {"error": "not_found"})

        def do_POST(self) -> None:
            if self.path != "/invoke":
                self._write_json(404, {"error": "not_found"})
                return

            content_type = self.headers.get("Content-Type", "").split(";", 1)[0]
            if content_type != "application/json":
                self._write_json(415, {"error": "unsupported_media_type"})
                return

            try:
                length = int(self.headers.get("Content-Length", "0"))
                request = json.loads(self.rfile.read(length))
                validate_invocation_request(request)
            except (TypeError, ValueError, json.JSONDecodeError) as error:
                self._write_json(400, {"error": "invalid_request", "message": str(error)})
                return

            if not invocation_lock.acquire(blocking=False):
                self._write_json(
                    409,
                    {
                        "code": "RUNTIME_BUSY",
                        "message": "This runtime instance is already processing an invocation.",
                    },
                )
                return

            try:
                result = handle_request(request, handler, event_factory)
            finally:
                invocation_lock.release()

            self._write_json(200, result)

        def _write_json(self, status: int, body: dict[str, Any]) -> None:
            payload = json.dumps(body, separators=(",", ":")).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, format: str, *args: Any) -> None:
            return

    return ThreadingHTTPServer((host, port), RuntimeRequestHandler)


def validate_invocation_request(request: Any) -> None:
    if not isinstance(request, dict):
        raise ValueError("request must be a JSON object")
    if request.get("protocol_version") != "ryvus.invoke.v1":
        raise ValueError("unsupported protocol_version")
    if not isinstance(request.get("invocation_id"), str) or not request["invocation_id"]:
        raise ValueError("invocation_id is required")
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
    invocation_id = request["invocation_id"]

    event = event_factory(request.get("event") or {})

    context = Context(
        invocation_id=invocation_id,
        protocol_version=request["protocol_version"],
        metadata=_request_metadata(request),
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
