from typing import Any, Callable, Optional, TypeVar, overload

from .runtime import run_api, run_schedule

F = TypeVar("F", bound=Callable[..., Any])


@overload
def api_action(func: F) -> F:
    ...


@overload
def api_action(*, name: str) -> Callable[[F], F]:
    ...


@overload
def api_action(
    *, method: str, path: str, name: Optional[str] = None
) -> Callable[[F], F]:
    ...


def api_action(
    func: Optional[F] = None,
    *,
    method: Optional[str] = None,
    path: Optional[str] = None,
    name: Optional[str] = None,
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

        setattr(inner, "__ryvus_action__", metadata)

        module_name = getattr(inner, "__module__", None)

        if module_name == "__main__":
            run_api(inner)

        return inner

    if func is not None:
        return decorate(func)

    return decorate


@overload
def scheduled_action(func: F) -> F:
    ...


@overload
def scheduled_action(*, every: str = "60s", name: Optional[str] = None) -> Callable[[F], F]:
    ...


def scheduled_action(
    func: Optional[F] = None,
    *,
    every: str = "60s",
    name: Optional[str] = None,
):
    def decorate(inner: F) -> F:
        metadata = {
            "type": "schedule",
            "name": name or inner.__name__,
            "expression": f"every {every}" if not every.startswith("every ") else every,
        }

        setattr(inner, "__ryvus_action__", metadata)

        module_name = getattr(inner, "__module__", None)

        if module_name == "__main__":
            run_schedule(inner)

        return inner

    if func is not None:
        return decorate(func)

    return decorate
