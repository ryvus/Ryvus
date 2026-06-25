import inspect
import types
from datetime import date, datetime
from decimal import Decimal
from enum import Enum
from typing import Any, Union, get_args, get_origin, get_type_hints
from uuid import UUID

from pydantic import BaseModel


def resolve_handler_args(handler, event, context):
    signature = inspect.signature(handler)
    type_hints = get_type_hints(handler)
    payload = _merged_payload(event)

    kwargs = {}

    for name, parameter in signature.parameters.items():
        if parameter.kind is inspect.Parameter.POSITIONAL_ONLY:
            raise TypeError(
                f"Ryvus does not support positional-only handler parameter '{name}'"
            )

        annotation = type_hints.get(name, parameter.annotation)
        annotation, nullable = unwrap_optional(annotation)

        if name == "context":
            kwargs[name] = context
            continue

        if name == "event":
            kwargs[name] = event
            continue

        if _is_pydantic_model(annotation):
            kwargs[name] = annotation(**payload)
            continue

        if name in payload:
            kwargs[name] = _coerce_value(payload[name], annotation, nullable)
            continue

        if parameter.default is not inspect.Parameter.empty:
            kwargs[name] = parameter.default
            continue

        raise TypeError(f"Unable to resolve handler parameter '{name}'")

    return kwargs


def unwrap_optional(annotation):
    origin = get_origin(annotation)
    args = get_args(annotation)

    if origin is Union or origin is types.UnionType:
        non_none = [arg for arg in args if arg is not type(None)]

        if len(non_none) == 1 and len(non_none) != len(args):
            return non_none[0], True

    return annotation, False


def _coerce_value(value: Any, annotation: Any, nullable: bool) -> Any:
    if nullable and value == "":
        return None

    if annotation is inspect.Signature.empty:
        return value

    converter = CONVERTERS.get(annotation)

    if converter is not None:
        return converter(value)

    if _is_enum(annotation):
        return annotation(value)

    return value


def _to_str(value: Any) -> str:
    if isinstance(value, str):
        return value

    return str(value)


def _to_bool(value: Any) -> bool:
    if isinstance(value, bool):
        return value

    normalized = str(value).strip().lower()

    if normalized in ("true", "1", "yes", "on"):
        return True

    if normalized in ("false", "0", "no", "off"):
        return False

    raise ValueError(f"Invalid boolean value: {value!r}")


def _to_datetime(value: Any) -> datetime:
    if isinstance(value, datetime):
        return value

    return datetime.fromisoformat(str(value))


def _to_date(value: Any) -> date:
    if isinstance(value, date) and not isinstance(value, datetime):
        return value

    return date.fromisoformat(str(value))


CONVERTERS = {
    str: _to_str,
    int: int,
    float: float,
    bool: _to_bool,
    UUID: UUID,
    Decimal: Decimal,
    datetime: _to_datetime,
    date: _to_date,
}


def _is_enum(annotation: Any) -> bool:
    return (
        isinstance(annotation, type)
        and issubclass(annotation, Enum)
    )


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