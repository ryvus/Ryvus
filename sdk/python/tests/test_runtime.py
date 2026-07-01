from ryvus.events import ApiEvent
from ryvus.runtime import handle_api_request


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
