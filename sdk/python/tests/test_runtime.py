import json
import os
import subprocess
import sys
from pathlib import Path

from ryvus.discover import discover_actions
from ryvus.events import ApiEvent, FlowEvent
from ryvus.runtime import (
    event_from_flow_request,
    handle_api_request,
    handle_request,
)


def build_request():
    return {
        "protocol_version": "ryvus.invoke.v3",
        "execution_id": "execution-id",
        "attempt_id": "attempt-id",
        "attempt_number": 1,
        "deadline_unix_ms": 4102444800000,
        "remaining_budget_ms": 3000,
        "event": {
            "body": {
                "name": "Maikel",
            },
        },
        "metadata": {},
    }


def result_for(result):
    return result


def test_framed_worker_emits_ready_logs_and_result(tmp_path):
    source = tmp_path / "action.py"
    source.write_text(
        """
import logging
from ryvus import api_action

@api_action
def handler(context):
    print("printed")
    logging.warning("logged")
    return {"attempt_id": context.attempt_id}
"""
    )
    env = os.environ.copy()
    env["RYVUS_ENTRYPOINT"] = "handler"
    env["PYTHONPATH"] = os.pathsep.join(
        filter(None, [str(Path(__file__).parents[1]), env.get("PYTHONPATH")])
    )
    process = subprocess.Popen(
        [sys.executable, str(source)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=env,
    )
    stdout, stderr = process.communicate(json.dumps(build_request()) + "\n", timeout=5)
    frames = [json.loads(line) for line in stdout.splitlines()]

    assert process.returncode == 0, stderr
    assert frames[0] == {"type": "ready"}
    assert [frame["event"]["message"] for frame in frames[1:-1]] == [
        "printed",
        "logged",
    ]
    assert all(frame["event"]["attempt_id"] == "attempt-id" for frame in frames[1:-1])
    assert frames[-1]["type"] == "result"
    assert frames[-1]["result"]["status"] == "success"
    assert frames[-1]["result"]["output"] == {"attempt_id": "attempt-id"}


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
            "execution_id": context.execution_id,
            "attempt_id": context.attempt_id,
            "attempt_number": context.attempt_number,
        }

    result = result_for(handle_api_request(build_request(), handler))

    assert result["status"] == "success"
    assert result["output"] == {
        "execution_id": "execution-id",
        "attempt_id": "attempt-id",
        "attempt_number": 1,
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
