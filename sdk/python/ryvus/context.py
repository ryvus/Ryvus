from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class Context:
    invocation_id: str
    protocol_version: str
    metadata: dict[str, Any]