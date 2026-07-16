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
- durable scheduler for `every <number><s|m|h>`
- schedule discovery reconciliation, trigger history, and manual-run routes
- execution and attempt history in the Portal
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

@scheduled_action(every="10s", key="restock-report")
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

Scheduler and execution-history routes are mounted on the control service:

```text
GET  /internal/scheduler/schedules
GET  /internal/scheduler/schedules/{id}
GET  /internal/scheduler/schedules/{id}/triggers
POST /internal/scheduler/schedules/{id}/run
POST /internal/scheduler/schedules/{id}/enable
POST /internal/scheduler/schedules/{id}/disable
GET  /internal/executions
GET  /internal/executions/{id}
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

## Execution Persistence

Ryvus has two execution-state providers:

- The memory composition is the default used by `ryvus start`. It keeps execution, schedule, trigger, and Portal history for the lifetime of the Ryvus process and requires no database.
- The PostgreSQL composition stores executions, attempts, retries, cancellation intent, Runtime Host ownership, schedule definitions, trigger history, structured results, and terminal outcomes durably across restarts.

PostgreSQL support is explicit. Ryvus does not embed PostgreSQL, start it automatically, or run migrations during application startup.

The CLI loads configuration from `.env` in the current project directory. Values already present in the process environment take precedence over the file. New projects generate an ignored, credential-free `.env` using the memory provider.

### Start PostgreSQL with Docker Compose

The repository provides a PostgreSQL-only Compose service for local development and integration testing:

```bash
docker compose -f compose.postgres-test.yml up -d --wait
```

It exposes PostgreSQL on `localhost:55432` with these development credentials:

```text
Host:     localhost
Port:     55432
User:     ryvus_test
Password: ryvus_test
Admin DB: postgres
```

The Compose service creates only the administrative `postgres` database. Create a separate application database once:

```bash
docker compose -f compose.postgres-test.yml exec postgres \
  createdb -U ryvus_test ryvus
```

The application database URL is:

```text
postgres://ryvus_test:ryvus_test@localhost:55432/ryvus
```

Use that URL in a database application such as DBeaver, DataGrip, or `psql`. The integration-test administrator URL is different because it targets the administrative database:

```text
postgres://ryvus_test:ryvus_test@localhost:55432/postgres
```

### Run Ryvus migrations

Run migrations explicitly before using a new application database or after pulling schema changes:

```bash
DATABASE_URL=postgres://ryvus_test:ryvus_test@localhost:55432/ryvus \
  ryvus database migrate
```

When running the CLI directly from this workspace:

```bash
DATABASE_URL=postgres://ryvus_test:ryvus_test@localhost:55432/ryvus \
  cargo run -p ryvus-cli -- database migrate
```

Migrations are repeatable. They create and track the Ryvus-owned execution schema; they do not start Ryvus or execute application work.

### Start Ryvus

With no `RYVUS_EXECUTION_STORE` variable, or with this project configuration, Ryvus remains database-free:

```dotenv
RYVUS_EXECUTION_STORE=memory
```

```bash
ryvus start
```

To persist execution state, put both values in the project's ignored `.env`:

```dotenv
RYVUS_EXECUTION_STORE=postgres
DATABASE_URL=postgres://ryvus_test:ryvus_test@localhost:55432/ryvus
```

Run the explicit migration once, then start normally:

```bash
ryvus database migrate
ryvus start
```

The PostgreSQL provider is shared by API actions, scheduled actions, Flow steps, and manual scheduled execution through the same `ExecutionService`. The Scheduler persists orchestration history separately and links each trigger to its canonical execution; it does not duplicate attempts, logs, results, or errors. Setting only `DATABASE_URL` does not select PostgreSQL; `RYVUS_EXECUTION_STORE=postgres` is required. Invalid or unavailable explicit PostgreSQL configuration fails startup instead of silently falling back to memory.

### Stop PostgreSQL

Stop the container while preserving the database volume:

```bash
docker compose -f compose.postgres-test.yml down
```

Delete the container and all local PostgreSQL data:

```bash
docker compose -f compose.postgres-test.yml down -v
```

Do not use `-v` when you want to keep the local `ryvus` database.

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

The test harness creates and removes its own random database. It does not use the `ryvus` application database described above. See [PostgreSQL Integration Testing](docs/postgres-integration-testing.md) for external PostgreSQL and CI usage.

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
