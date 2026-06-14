import json
import sys

request = json.load(sys.stdin)

result = {
    "protocol_version": request["protocol_version"],
    "invocation_id": request["invocation_id"],
    "status": "success",
    "output": {
        "received": request["event"],
        "handled_by": "python"
    },
    "error": None
}

json.dump(result, sys.stdout)