# Ryvus Python SDK

The Ryvus Python SDK provides a simple developer experience for building actions that run through the Ryvus Gateway.

The SDK hides the underlying Ryvus invocation protocol and allows developers to focus on handling input and returning output.

## Quick Start

Create a Python action:

```python
from ryvus import api_action


@api_action
def handler(event):
    return {
        "message": "Hello from Ryvus"
    }
```

The action receives an event object and returns a JSON-serializable response.

## API Actions

API actions are invoked by the Ryvus Gateway.

```python
from ryvus import api_action


@api_action
def handler(event):
    return {
        "message": f"Hello {event.body['name']}"
    }
```

Example request:

```json
{
  "name": "Maikel"
}
```

Example response:

```json
{
  "message": "Hello Maikel"
}
```

## Event Object

API actions receive an `ApiEvent`.

```python
@api_action
def handler(event):
    return {
        "body": event.body
    }
```

### Properties

| Property | Type | Description     |
| -------- | ---- | --------------- |
| body     | dict | Request payload |

## Context

Actions may optionally receive a context parameter.

```python
from ryvus import api_action


@api_action
def handler(event, context):
    return {
        "execution_id": context.execution_id,
        "attempt_id": context.attempt_id,
        "attempt_number": context.attempt_number,
    }
```

### Context Properties

| Property         | Type | Description                         |
| ---------------- | ---- | ----------------------------------- |
| execution_id     | str  | Stable logical execution identifier |
| attempt_id       | str  | Physical attempt identifier         |
| attempt_number   | int  | One-based attempt number            |
| protocol_version | str  | Ryvus protocol version              |
| metadata         | dict | Additional invocation metadata      |

## Error Handling

Unhandled exceptions are automatically captured and returned to the Ryvus runtime as failed executions.

```python
from ryvus import api_action


@api_action
def handler(event):
    raise ValueError("Something went wrong")
```

The SDK converts the exception into a Ryvus error response.

## Developer Experience Goals

The SDK is designed around the following principles:

- Developers should not interact with the Ryvus invocation protocol directly.
- Developers should not read from stdin or write to stdout.
- Actions should focus only on business logic.
- Input should be represented through typed event objects.
- Output should be plain JSON-compatible data structures.
- The same programming model should work across local development and cloud execution environments.

## Future Event Types

The SDK currently supports API actions.

Additional event types are expected in the future:

- Flow actions
- Queue actions
- Scheduled actions

These event types will use the same handler signature while exposing different event objects.

```python
def handler(event, context):
    ...
```

## Example

```python
from ryvus import api_action


@api_action
def handler(event):
    return {
        "message": "Hello from Ryvus Python SDK"
    }
```
