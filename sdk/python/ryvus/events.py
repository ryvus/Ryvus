from dataclasses import dataclass, field
from typing import Any


@dataclass(frozen=True)
class ApiEvent:
    body: Any = None
    query_params: dict[str, Any] = field(default_factory=dict)
    path_params: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class ScheduleEvent:
    trigger: str
    scheduled_at: Any
    expression: str
