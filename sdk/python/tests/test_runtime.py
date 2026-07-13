import json
import threading
import urllib.error
import urllib.request
from pathlib import Path

from ryvus.discover import discover_actions
from ryvus.events import ApiEvent, FlowEvent
from ryvus.runtime import (
    create_runtime_server,
    event_from_api_request,
    event_from_flow_request,
    handle_api_request,
    handle_request,
)


def build_request():
    return {
        "protocol_version": "ryvus.invoke.v1",
        "invocation_id": "test-id",
        "event": {
            "body": {
                "name": "Maikel",
            },
        },
        "metadata": {},
    }


def result_for(result):
    return result


def test_http_runtime_health_and_invoke():
    server = create_runtime_server(
        lambda event: {"message": f"Hello {event.body['name']}"},
        event_from_api_request,
    )
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    endpoint = f"http://127.0.0.1:{server.server_port}"

    try:
        with urllib.request.urlopen(f"{endpoint}/health") as response:
            assert response.status == 200
            assert json.load(response) == {"status": "ready", "busy": False}

        request = urllib.request.Request(
            f"{endpoint}/invoke",
            data=json.dumps(build_request()).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(request) as response:
            result = json.load(response)

        assert result["invocation_id"] == "test-id"
        assert result["status"] == "success"
        assert result["output"] == {"message": "Hello Maikel"}
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def test_http_runtime_rejects_concurrent_invocation():
    started = threading.Event()
    release = threading.Event()

    def handler(event):
        started.set()
        release.wait(timeout=2)
        return {"ok": True}

    server = create_runtime_server(handler, event_from_api_request)
    server_thread = threading.Thread(target=server.serve_forever)
    server_thread.start()
    endpoint = f"http://127.0.0.1:{server.server_port}"
    first_result = {}

    def invoke_first():
        request = urllib.request.Request(
            f"{endpoint}/invoke",
            data=json.dumps(build_request()).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urllib.request.urlopen(request) as response:
            first_result.update(json.load(response))

    first_thread = threading.Thread(target=invoke_first)
    first_thread.start()

    try:
        assert started.wait(timeout=1)
        with urllib.request.urlopen(f"{endpoint}/health") as response:
            assert json.load(response)["busy"] is True

        second = urllib.request.Request(
            f"{endpoint}/invoke",
            data=json.dumps(build_request()).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            urllib.request.urlopen(second)
            raise AssertionError("concurrent invocation should fail")
        except urllib.error.HTTPError as error:
            assert error.code == 409
            assert json.load(error)["code"] == "RUNTIME_BUSY"
    finally:
        release.set()
        first_thread.join()
        server.shutdown()
        server.server_close()
        server_thread.join()

    assert first_result["status"] == "success"


def test_http_runtime_rejects_invalid_protocol():
    server = create_runtime_server(lambda event: {}, event_from_api_request)
    thread = threading.Thread(target=server.serve_forever)
    thread.start()
    request = build_request()
    request["protocol_version"] = "other"

    try:
        with urllib.request.urlopen(
            urllib.request.Request(
                f"http://127.0.0.1:{server.server_port}/invoke",
                data=json.dumps(request).encode(),
                headers={"Content-Type": "application/json"},
                method="POST",
            )
        ):
            raise AssertionError("invalid protocol should fail")
    except urllib.error.HTTPError as error:
        assert error.code == 400
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


def test_handler_receives_api_event():
    def handler(event):
        return {
            "message": f"Hello {event.body['name']}"
        }

    result = result_for(handle_api_request(build_request(), handler))

    assert result["status"] == "success"
    assert result["output"] == {
        "message": "Hello Maikel"
    }


def test_handler_receives_context():
    def handler(event, context):
        return {
            "invocation_id": context.invocation_id
        }

    result = result_for(handle_api_request(build_request(), handler))

    assert result["status"] == "success"
    assert result["output"] == {
        "invocation_id": "test-id"
    }


def test_handler_receives_nested_context_metadata():
    def handler(context):
        return context.metadata

    request = build_request()
    request["context"] = {
        "metadata": {
            "flow": {
                "step_key": "receive_invoice",
            },
            "params": {
                "tenant": "demo",
            },
        }
    }

    result = result_for(handle_api_request(request, handler))

    assert result["status"] == "success"
    assert result["output"]["flow"]["step_key"] == "receive_invoice"
    assert result["output"]["params"]["tenant"] == "demo"


def test_api_event_body():
    captured = {}

    def handler(event):
        captured["event"] = event

        return {
            "ok": True
        }

    result = result_for(handle_api_request(build_request(), handler))

    assert result["status"] == "success"
    assert isinstance(captured["event"], ApiEvent)
    assert captured["event"].body["name"] == "Maikel"


def test_handler_exception_returns_error():
    def handler(event):
        raise ValueError("boom")

    result = result_for(handle_api_request(build_request(), handler))

    assert result["status"] == "failed"
    assert result["error"]["code"] == "ValueError"
    assert result["error"]["message"] == "boom"


def test_flow_handler_receives_flow_event():
    captured = {}

    def handler(event):
        captured["event"] = event
        return {"invoice_id": event.data["invoice_id"]}

    request = build_request()
    request["event"] = {"invoice_id": "inv_1"}

    result = result_for(handle_request(request, handler, event_from_flow_request))

    assert result["status"] == "success"
    assert isinstance(captured["event"], FlowEvent)
    assert result["output"] == {"invoice_id": "inv_1"}


def test_discovers_flow_action(tmp_path):
    source_root = tmp_path / "src"
    action_file = source_root / "billing.py"
    source_root.mkdir()
    action_file.write_text(
        "\n".join(
            [
                "from ryvus import flow_action",
                "",
                "@flow_action(name='billing/receive_invoice')",
                "def receive_invoice(event):",
                "    return event.data",
            ]
        )
    )

    actions = discover_actions(Path(tmp_path), source_root)

    assert actions == [
        {
            "runtime": "Python",
            "kind": {"Flow": {}},
            "source": "src/billing.py",
            "entrypoint": "receive_invoice",
            "name": "billing/receive_invoice",
        }
    ]


def test_discovers_authorizer_and_api_reference(tmp_path):
    source_root = tmp_path / "src"
    source_root.mkdir()
    (source_root / "auth.py").write_text(
        "\n".join(
            [
                "from ryvus import api_action, authorizer",
                "",
                "@authorizer(",
                "    name='petstore',",
                "    security={'type': 'http', 'scheme': 'bearer'},",
                "    parameters=[",
                "        {'name': 'X-Tenant-ID', 'in': 'header', 'required': True},",
                "        {'name': 'session', 'in': 'cookie', 'required': False, 'type': 'string'},",
                "    ],",
                "    cache_ttl_seconds=60,",
                ")",
                "def auth(event):",
                "    return {'effect': 'allow'}",
                "",
                "@api_action(method='GET', path='/pets', authorizer='petstore')",
                "def list_pets():",
                "    return []",
            ]
        )
    )

    actions = discover_actions(Path(tmp_path), source_root)

    assert actions == [
        {
            "runtime": "Python",
            "kind": {
                "Authorizer": {
                    "security": [{"type": "http", "scheme": "bearer"}],
                    "parameters": [
                        {
                            "name": "X-Tenant-ID",
                            "in": "header",
                            "required": True,
                            "type": "string",
                        },
                        {
                            "name": "session",
                            "in": "cookie",
                            "required": False,
                            "type": "string",
                        },
                    ],
                    "cache": {"ttl_seconds": 60},
                }
            },
            "source": "src/auth.py",
            "entrypoint": "auth",
            "name": "petstore",
        },
        {
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets",
                    "query_params": [],
                    "authorizer": "petstore",
                }
            },
            "source": "src/auth.py",
            "entrypoint": "list_pets",
            "name": "list_pets",
        },
    ]


def test_authorizer_handler_receives_headers_method_and_path():
    from ryvus.events import AuthorizerEvent
    from ryvus.runtime import handle_authorizer_request

    captured = {}

    def handler(event):
        captured["event"] = event
        return {"effect": "allow", "principal_id": event.headers["authorization"]}

    request = build_request()
    request["event"] = {
        "body": None,
        "path_params": {"pet_id": "p1"},
        "query_params": {"debug": "true"},
        "headers": {"authorization": "Bearer dev"},
        "method": "GET",
        "path": "/pets/p1",
    }

    result = result_for(handle_authorizer_request(request, handler))

    assert result["status"] == "success"
    assert result["output"] == {"effect": "allow", "principal_id": "Bearer dev"}
    assert isinstance(captured["event"], AuthorizerEvent)
    assert captured["event"].path == "/pets/p1"
