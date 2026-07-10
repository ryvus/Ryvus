import argparse
import importlib.util
import inspect
import json
import re
import sys
from pathlib import Path
from typing import Any, Optional, Union, get_args, get_origin
import types

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
    source_root_value = str(source_root)

    if source_root_value not in sys.path:
        sys.path.insert(0, source_root_value)

    for path in source_root.rglob("*.py"):
        if path.name.startswith("__"):
            continue

        module = load_module(path)

        for _, obj in inspect.getmembers(module, inspect.isfunction):
            metadata = getattr(obj, "__ryvus_action__", None)

            if not metadata:
                continue

            if metadata.get("type") == "api":
                request_schema, response_schema = schemas_from_handler(obj)

                api_config: dict[str, Any] = {
                    "method": metadata["method"],
                    "path": metadata["path"],
                    "query_params": query_params_from_handler(
                        obj,
                        metadata["path"],
                        metadata.get("consumes", ["application/json"]),
                    ),
                }
                if "consumes" in metadata:
                    api_config["consumes"] = metadata["consumes"]
                if "produces" in metadata:
                    api_config["produces"] = metadata["produces"]
                if "authorizer" in metadata:
                    api_config["authorizer"] = metadata["authorizer"]

                if request_schema is not None:
                    api_config["request_schema"] = request_schema

                if response_schema is not None:
                    api_config["response_schema"] = response_schema

                kind = {
                    "Api": api_config,
                }
            elif metadata.get("type") == "schedule":
                kind = {
                    "Schedule": {
                        "expression": metadata["expression"],
                    },
                }
            elif metadata.get("type") == "authorizer":
                authorizer_config: dict[str, Any] = {}
                if "security" in metadata:
                    authorizer_config["security"] = metadata["security"]
                if "parameters" in metadata:
                    authorizer_config["parameters"] = metadata["parameters"]

                kind = {
                    "Authorizer": authorizer_config,
                }
            elif metadata.get("type") == "flow":
                kind = {
                    "Flow": {},
                }
            else:
                continue

            action = {
                "runtime": "Python",
                "kind": kind,
                "source": str(path.relative_to(project_root)),
                "entrypoint": obj.__name__,
                "name": metadata.get("name", obj.__name__),
            }
            if "policy" in metadata:
                action["policy"] = metadata["policy"]
            actions.append(action)

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

def query_params_from_handler(handler, path: str, consumes: list[str]) -> list[dict[str, Any]]:
    signature = inspect.signature(handler)
    path_param_names = set(extract_path_param_names(path))

    query_params: list[dict[str, Any]] = []

    for parameter in signature.parameters.values():
        name = parameter.name
        annotation = parameter.annotation

        if name in ("event", "context"):
            continue

        if name == "body" and "application/json" not in consumes:
            continue

        if name in path_param_names:
            continue

        if is_context_annotation(annotation):
            continue

        if is_pydantic_model(annotation):
            continue

        schema = scalar_schema_from_annotation(annotation)

        if schema is None:
            continue

        param_config = {
            "name": name,
            "required": parameter.default is inspect.Signature.empty,
            "schema": schema,
        }

        if (
            parameter.default is not inspect.Signature.empty
            and parameter.default is not None
        ):
            param_config["schema"]["default"] = parameter.default

        query_params.append(param_config)

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

    annotation, nullable = unwrap_optional(annotation)

    openapi_type = SCALAR_TYPES.get(annotation)

    if openapi_type is None:
        return None

    schema = {"type": openapi_type}

    if nullable:
        schema["nullable"] = True

    return schema

def unwrap_optional(annotation):
    origin = get_origin(annotation)
    args = get_args(annotation)

    if origin is Union:
        non_none = [arg for arg in args if arg is not type(None)]

        if len(non_none) == 1 and len(non_none) != len(args):
            return non_none[0], True

    if origin is types.UnionType:
        non_none = [arg for arg in args if arg is not type(None)]

        if len(non_none) == 1 and len(non_none) != len(args):
            return non_none[0], True

    return annotation, False

if __name__ == "__main__":
    main()
