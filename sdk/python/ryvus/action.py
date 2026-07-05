import os
from typing import Any, Callable, Optional, TypeVar, overload

from .runtime import run_api, run_flow, run_schedule

F = TypeVar("F", bound=Callable[..., Any])


def _should_run(inner: Callable[..., Any]) -> bool:
    module_name = getattr(inner, "__module__", None)

    if module_name != "__main__":
        return False

    entrypoint = os.environ.get("RYVUS_ENTRYPOINT")
    return entrypoint is None or entrypoint == inner.__name__


@overload
def api_action(func: F) -> F:
    ...


@overload
def api_action(*, name: str) -> Callable[[F], F]:
    ...


@overload
def api_action(
    *,
    method: str,
    path: str,
    name: Optional[str] = None,
    timeout: Optional[str] = None,
    retry: Optional[dict[str, Any]] = None,
) -> Callable[[F], F]:
    ...


def api_action(
    func: Optional[F] = None,
    *,
    method: Optional[str] = None,
    path: Optional[str] = None,
    name: Optional[str] = None,
    timeout: Optional[str] = None,
    retry: Optional[dict[str, Any]] = None,
):
    if func is None and ((method is None) != (path is None)):
        raise ValueError(
            "api_action requires both 'method' and 'path' when used with options"
        )

    def decorate(inner: F) -> F:
        metadata = {
            "type": "api",
            "name": name or inner.__name__,
            "method": method or "GET",
            "path": path or f"/{inner.__name__.replace('_', '-')}",
        }
        policy = optional_policy(timeout, retry)
        if policy is not None:
            metadata["policy"] = policy

        setattr(inner, "__ryvus_action__", metadata)

        if _should_run(inner):
            run_api(inner)

        return inner

    if func is not None:
        return decorate(func)

    return decorate


@overload
def scheduled_action(func: F) -> F:
    ...


@overload
def scheduled_action(
    *,
    every: str = "60s",
    name: Optional[str] = None,
    timeout: Optional[str] = None,
    retry: Optional[dict[str, Any]] = None,
) -> Callable[[F], F]:
    ...


def scheduled_action(
    func: Optional[F] = None,
    *,
    every: str = "60s",
    name: Optional[str] = None,
    timeout: Optional[str] = None,
    retry: Optional[dict[str, Any]] = None,
):
    def decorate(inner: F) -> F:
        metadata = {
            "type": "schedule",
            "name": name or inner.__name__,
            "expression": f"every {every}" if not every.startswith("every ") else every,
        }
        policy = optional_policy(timeout, retry)
        if policy is not None:
            metadata["policy"] = policy

        setattr(inner, "__ryvus_action__", metadata)

        if _should_run(inner):
            run_schedule(inner)

        return inner

    if func is not None:
        return decorate(func)

    return decorate


@overload
def flow_action(func: F) -> F:
    ...


@overload
def flow_action(*, name: Optional[str] = None) -> Callable[[F], F]:
    ...


def flow_action(
    func: Optional[F] = None,
    *,
    name: Optional[str] = None,
):
    def decorate(inner: F) -> F:
        metadata = {
            "type": "flow",
            "name": name or inner.__name__,
        }

        setattr(inner, "__ryvus_action__", metadata)

        if _should_run(inner):
            run_flow(inner)

        return inner

    if func is not None:
        return decorate(func)

    return decorate


def optional_policy(
    timeout: Optional[str], retry: Optional[dict[str, Any]]
) -> Optional[dict[str, Any]]:
    if timeout is None and retry is None:
        return None

    policy: dict[str, Any] = {}
    if timeout is not None:
        policy["timeout"] = timeout
    if retry is not None:
        policy["retry"] = retry
    return policy
