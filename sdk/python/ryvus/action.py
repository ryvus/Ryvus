from typing import Any, Callable, Optional, TypeVar, overload

from .runtime import run_api

F = TypeVar("F", bound=Callable[..., Any])


@overload
def api_action(func: F) -> F:
    ...


@overload
def api_action(*, method: str, path: str) -> Callable[[F], F]:
    ...


def api_action(
    func: Optional[F] = None,
    *,
    method: Optional[str] = None,
    path: Optional[str] = None,
):
    if func is None and (method is None or path is None):
        raise ValueError("api_action requires both 'method' and 'path' when used with options")

    def decorate(inner: F) -> F:
        metadata = {"type": "api"}

        if method is not None and path is not None:
            metadata["method"] = method
            metadata["path"] = path

        setattr(inner, "__ryvus_action__", metadata)

        if inner.__module__ == "__main__":
            run_api(inner)

        return inner

    if func is not None:
        return decorate(func)

    return decorate