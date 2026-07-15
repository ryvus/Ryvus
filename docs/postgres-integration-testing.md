# PostgreSQL Integration Testing

Ordinary Ryvus development is database-free. `ryvus start` uses `MemoryExecutionStateStore`, and `cargo test --workspace` does not run the PostgreSQL integration suite.

## Local Compose workflow

Start the repository's PostgreSQL-only test service:

```bash
docker compose -f compose.postgres-test.yml up -d --wait
```

Run the explicitly gated suite:

```bash
RYVUS_POSTGRES_TEST_ADMIN_URL=postgres://ryvus_test:ryvus_test@localhost:55432/postgres \
  cargo test -p ryvus-persistence --features postgres-integration
```

Stop PostgreSQL and remove its test volume:

```bash
docker compose -f compose.postgres-test.yml down -v
```

The Rust harness depends only on `RYVUS_POSTGRES_TEST_ADMIN_URL`. It creates one random `ryvus_test_<uuid>` database, runs the production migrations and provider checks, closes its connections, and drops that database. Docker is not used by the harness.

The suite reports migration, provider-contract, restart, CAS, and rollback phases while retaining one database per run. Rollback validation asserts PostgreSQL SQLSTATE `23505` and verifies that the failed transaction leaves no partial aggregate writes.

## CI and external PostgreSQL

Point the same command at the administrator database of a CI service container, developer-managed server, or compatible external test server:

```bash
RYVUS_POSTGRES_TEST_ADMIN_URL=postgres://user:password@host:5432/postgres \
  cargo test -p ryvus-persistence --features postgres-integration
```

The administrator must be allowed to create databases, terminate connections to the generated test database, and drop it. Missing, invalid, or unreachable administrator URLs fail the explicitly enabled suite.

## Interrupted-run cleanup

A hard process kill can prevent RAII cleanup. List only Ryvus integration databases before removing stale entries:

```sql
SELECT datname FROM pg_database WHERE datname ~ '^ryvus_test_[a-z0-9_]+$';
```

After checking the result, terminate connections and drop a named stale database explicitly. Never automate deletion from a broader pattern.

Generated execution output under `.ryvus-test/` is ignored repository-wide. It is developer output, not a test fixture; curated fixtures must use an intentional fixture directory instead.

## Current limitations

This suite validates persistence correctness, not hosted production readiness. The provider currently owns one synchronous client on a dedicated worker thread; callers block while awaiting each result. It uses `NoTls` and has no pool or bounded query timeout. Managed PostgreSQL and configurable TLS compatibility require separate operational validation.

If a worker result reaches the control plane but PostgreSQL fails before the terminal compare-and-set commits, the durable aggregate can remain `Running` after the worker exits. Completed-result acknowledgement and retention are not implemented yet.
