import inspect
from typing import Any, get_type_hints

from pydantic import BaseModel


def resolve_handler_args(handler, event, context):
    signature = inspect.signature(handler)
    type_hints = get_type_hints(handler)
    payload = _merged_payload(event)

    args = []

    for name, parameter in signature.parameters.items():
        annotation = type_hints.get(name, parameter.annotation)

        if name == "context":
            args.append(context)
            continue

        if name == "event":
            args.append(event)
            continue

        if _is_pydantic_model(annotation):
            args.append(annotation(**payload))
            continue

        if name in payload:
            args.append(payload[name])
            continue

        if parameter.default is not inspect.Parameter.empty:
            args.append(parameter.default)
            continue

        raise TypeError(f"Unable to resolve handler parameter '{name}'")

    return args

def _is_pydantic_model(annotation: Any) -> bool:
    return (
        BaseModel is not None
        and isinstance(annotation, type)
        and issubclass(annotation, BaseModel)
    )


def _merged_payload(event) -> dict[str, Any]:
    body = event.body if getattr(event, "body", None) is not None else {}

    if not isinstance(body, dict):
        body = {"body": body}

    merged = {}
    merged.update(body)
    merged.update(getattr(event, "query_params", {}) or {})
    merged.update(getattr(event, "path_params", {}) or {})

    return merged