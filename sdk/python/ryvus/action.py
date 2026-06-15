from typing import Callable

from .runtime import run_api


def api_action(func: Callable):
    if func.__module__ == "__main__":
        run_api(func)

    return func