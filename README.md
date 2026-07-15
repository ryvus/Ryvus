# Ryvus

Ryvus is an open execution platform that makes building distributed applications as simple as writing code, while remaining portable across local development, cloud, and on-premises environments.

Developers should not have to choose between an excellent developer experience and production-grade infrastructure. Ryvus aims to provide both through a code-first platform that abstracts infrastructure without hiding its capabilities.

The goal is not to become another cloud provider. The goal is to become the execution layer that applications target regardless of where they ultimately run.

That is the long-term direction. The current implementation is v0 and is focused on local developer workflow.

The current v0 path is intentionally narrow:

```text
ApiActions -> Scheduled actions -> Flow
```

The canonical local command is:

```bash
ryvus start
```

`ryvus start` discovers a project, writes generated runtime artifacts, starts the gateway, starts the control/Portal service, runs the local scheduler, and executes actions through one shared `ExecutionService` path.

## What Works Now

- Python and Node/TypeScript ApiActions
- Python and Node/TypeScript scheduled actions
- local Flow definitions from recursive `*.flows.json` files
- generated `.ryvus/action-manifest.json`
- generated `.ryvus/flows.json`
- public gateway routes for ApiActions
- local scheduler for `every <number><s|m|h>`
- internal scheduler list/manual-run routes
- internal Flow start/get/cancel/retry-step routes
- Portal pages for APIs, schedules, docs, and flows
- request/query/body/schema validation
- OpenAPI generation for public API docs
- local process execution through `ExecutionService`
- default local timeout/retry execution policy
- console-default logging/persistence

## Not Done Yet

The vision includes portability across local, cloud, and on-premises environments, but v0 does not provide all of that yet.

Not done:

- on-premises deployment runtime
- distributed scheduler ownership and leases
- durable Flow run persistence by default
- queue trigger runtime
- object storage trigger runtime
- provider marketplace
- infrastructure provider plugins
- deployment service
- governance/admin controls
- OpenTelemetry exporter
- production multi-node execution
- Rust code-first SDK/discovery parity

Current v0 is local-first: ApiActions, scheduled actions, Flow, Portal, generated artifacts, and shared local process execution.

## Workspace Layout

```text
crates/
  protocol/        shared action and invocation protocol types
  execution/       runtime resolution, process execution, policy, recording
  persistence/     console and filesystem persistence boundaries
  action-catalog/  file-backed and in-memory action catalogs
  gateway/         public HTTP ApiAction adapter
  scheduler/       local scheduled-action runtime
  flow/            local Flow runtime
  control/         control/Portal service composition
  docs/            OpenAPI/docs artifact providers
  cli/             ryvus command line entrypoint

sdk/
  python/          Python decorators, discovery, and runtime protocol
  node/            TypeScript/JavaScript definitions, discovery, runtime protocol

apps/
  portal/          React Portal served by the control service
```

Older parked/reference crates may still exist in the repository. The active v0 implementation is the crate set above.

## Running A Project

From this workspace, build the CLI:

```bash
cargo build -p ryvus-cli
```

Then run Ryvus from a project directory. If the CLI is not on `PATH`, use the built binary by absolute path:

```bash
ryvus start
```

Services:

```text
Gateway: http://127.0.0.1:8080
Portal:  http://127.0.0.1:8079
Control: http://127.0.0.1:8079
```

## ApiAction Example

```python
from ryvus import api_action

@api_action(method="GET", path="/hello")
def hello(event, context):
    return {"message": "Hello from Ryvus"}
```

## Scheduled Action Example

```python
from ryvus import scheduled_action

@scheduled_action(every="10s")
def restock_report(context):
    print("checking stock")
    return {"ok": True}
```

## Flow Example

```json
{
  "key": "retry_probe_flow",
  "steps": [
    {
      "key": "probe_dependency",
      "action": "system/retry_probe",
      "retry": {
        "max_attempts": 2,
        "initial_delay": "250ms",
        "backoff": 1
      },
      "next": "done"
    },
    {
      "key": "done",
      "action": "ryvus/log",
      "end": "succeeded"
    }
  ]
}
```

## Internal Runtime Routes

Scheduler routes are mounted on the control service:

```text
GET  /internal/scheduler/schedules
POST /internal/scheduler/schedules/{id}/run
```

Flow routes are mounted on the control service:

```text
GET  /internal/flows
POST /internal/flows/{key}/runs
GET  /internal/flows/runs/{id}
POST /internal/flows/runs/{id}/cancel
POST /internal/flows/runs/{id}/steps/{step_key}/retry
```

Schedules and Flows do not execute through the public gateway.

## Development

Build:

```bash
cargo build --workspace
```

Test:

```bash
cargo test --workspace
```

### PostgreSQL integration tests

PostgreSQL validation is opt-in; normal tests and `ryvus start` continue to use the in-memory store. Start the test database and run the gated suite:

```bash
docker compose -f compose.postgres-test.yml up -d --wait

RYVUS_POSTGRES_TEST_ADMIN_URL=postgres://ryvus_test:ryvus_test@localhost:55432/postgres \
  cargo test -p ryvus-persistence --features postgres-integration

docker compose -f compose.postgres-test.yml down -v
```

The test harness creates and removes its own random database. See [PostgreSQL Integration Testing](docs/postgres-integration-testing.md) for external PostgreSQL and CI usage.

PostgreSQL-backed application execution is not wired into `ryvus start` yet. Setting `DATABASE_URL` and running `ryvus database migrate` creates the schema, but `ryvus start` still selects `MemoryExecutionStateStore`. An explicit startup persistence option is required before PostgreSQL can store normal Ryvus executions.

Format:

```bash
cargo fmt
```

Portal build:

```bash
npm --prefix apps/portal run build
```

Node SDK build:

```bash
pnpm --dir sdk/node build
```

Python SDK tests:

```bash
pytest sdk/python/tests
```
