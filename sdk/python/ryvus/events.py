from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class ApiEvent:
    body: dict[str, Any]