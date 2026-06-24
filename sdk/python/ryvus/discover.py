import argparse
import importlib.util
import inspect
import json
import re
import sys
from pathlib import Path
from typing import Any, Optional


try:
    from pydantic import BaseModel
except ImportError:
    BaseModel = None


SCALAR_TYPES = {
    str: "string",
    int: "integer",
    float: "number",
    bool: "boolean",
}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--project-root", required=True)
    parser.add_argument("--source-root", default="src")
    args = parser.parse_args()

    project_root = Path(args.project_root).resolve()
    source_root = project_root / args.source_root

    actions = discover_actions(project_root, source_root)

    print(json.dumps({"actions": actions}, indent=2))


def discover_actions(project_root: Path, source_root: Path) -> list[dict[str, Any]]:
    actions: list[dict[str, Any]] = []

    for path in source_root.rglob("*.py"):
        if path.name.startswith("__"):
            continue

        module = load_module(path)

        for _, obj in inspect.getmembers(module, inspect.isfunction):
            metadata = getattr(obj, "__ryvus_action__", None)

            if not metadata:
                continue

            if metadata.get("type") != "api":
                continue

            request_schema, response_schema = schemas_from_handler(obj)

            api_config: dict[str, Any] = {
                "method": metadata["method"],
                "path": metadata["path"],
                "query_params": query_params_from_handler(obj, metadata["path"]),
            }

            if request_schema is not None:
                api_config["request_schema"] = request_schema

            if response_schema is not None:
                api_config["response_schema"] = response_schema

            actions.append(
                {
                    "runtime": "Python",
                    "kind": {
                        "Api": api_config,
                    },
                    "source": str(path.relative_to(project_root)),
                    "entrypoint": obj.__name__,
                }
            )

    return actions


def load_module(path: Path):
    module_name = "ryvus_discovery_" + "_".join(path.with_suffix("").parts)

    spec = importlib.util.spec_from_file_location(module_name, path)

    if spec is None or spec.loader is None:
        raise RuntimeError(f"failed to load module spec for {path}")

    module = importlib.util.module_from_spec(spec)

    sys.modules[module_name] = module
    spec.loader.exec_module(module)

    return module


def schemas_from_handler(handler) -> tuple[Optional[dict[str, Any]], Optional[dict[str, Any]]]:
    signature = inspect.signature(handler)

    request_schema = None

    for parameter in signature.parameters.values():
        schema = schema_from_annotation(parameter.annotation)

        if schema is not None:
            request_schema = schema
            break

    response_schema = schema_from_annotation(signature.return_annotation)

    return request_schema, response_schema


def query_params_from_handler(handler, path: str) -> list[dict[str, Any]]:
    signature = inspect.signature(handler)
    path_param_names = set(extract_path_param_names(path))

    query_params: list[dict[str, Any]] = []

    for parameter in signature.parameters.values():
        name = parameter.name
        annotation = parameter.annotation

        if name in path_param_names:
            continue

        if is_context_annotation(annotation):
            continue

        if is_pydantic_model(annotation):
            continue

        schema = scalar_schema_from_annotation(annotation)

        if schema is None:
            continue

        if name in ("event", "context"):
            continue

        query_params.append(
            {
                "name": name,
                "required": parameter.default is inspect.Signature.empty,
                "schema": schema,
            }
        )

    return query_params


def extract_path_param_names(path: str) -> list[str]:
    return re.findall(r"{([^}]+)}", path)


def schema_from_annotation(annotation) -> Optional[dict[str, Any]]:
    if annotation is inspect.Signature.empty:
        return None

    if is_pydantic_model(annotation):
        return annotation.model_json_schema()

    return None


def is_pydantic_model(annotation) -> bool:
    if BaseModel is None:
        return False

    return isinstance(annotation, type) and issubclass(annotation, BaseModel)


def is_context_annotation(annotation) -> bool:
    return getattr(annotation, "__name__", None) == "Context"


def scalar_schema_from_annotation(annotation) -> Optional[dict[str, Any]]:
    if annotation is inspect.Signature.empty:
        return {"type": "string"}

    openapi_type = SCALAR_TYPES.get(annotation)

    if openapi_type is None:
        return None

    return {"type": openapi_type}


if __name__ == "__main__":
    main()