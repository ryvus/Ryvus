from ryvus.events import ApiEvent
from ryvus.runtime import handle_api_request


def build_request():
    return {
        "protocol_version": "1.0",
        "invocation_id": "test-id",
        "event": {
            "name": "Maikel",
        },
        "metadata": {},
    }


def test_handler_receives_api_event():
    def handler(event):
        return {
            "message": f"Hello {event.body['name']}"
        }

    result = handle_api_request(build_request(), handler)

    assert result["status"] == "success"
    assert result["output"] == {
        "message": "Hello Maikel"
    }


def test_handler_receives_context():
    def handler(event, context):
        return {
            "invocation_id": context.invocation_id
        }

    result = handle_api_request(build_request(), handler)

    assert result["status"] == "success"
    assert result["output"] == {
        "invocation_id": "test-id"
    }


def test_api_event_body():
    captured = {}

    def handler(event):
        captured["event"] = event

        return {
            "ok": True
        }

    result = handle_api_request(build_request(), handler)

    assert result["status"] == "success"
    assert isinstance(captured["event"], ApiEvent)
    assert captured["event"].body["name"] == "Maikel"


def test_handler_exception_returns_error():
    def handler(event):
        raise ValueError("boom")

    result = handle_api_request(build_request(), handler)

    assert result["status"] == "error"
    assert result["error"]["type"] == "ValueError"
    assert result["error"]["message"] == "boom"