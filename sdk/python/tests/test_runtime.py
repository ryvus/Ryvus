from pathlib import Path

from ryvus.discover import discover_actions
from ryvus.events import ApiEvent, FlowEvent
from ryvus.runtime import handle_api_request, handle_request, event_from_flow_request


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


def result_for(messages):
    return messages[-1]["result"]


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
