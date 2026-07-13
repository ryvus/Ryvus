from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class Context:
    execution_id: str
    attempt_id: str
    attempt_number: int
    protocol_version: str
    metadata: dict[str, Any]
