CREATE TABLE IF NOT EXISTS ryvus_schema_migrations (
    version BIGINT PRIMARY KEY,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE ryvus_executions (
    execution_id TEXT PRIMARY KEY,
    action JSONB NOT NULL,
    action_revision TEXT NOT NULL CHECK (action_revision <> ''),
    invocation_request JSONB NOT NULL,
    policy JSONB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'running', 'cancellation_requested', 'succeeded', 'failed', 'cancelled', 'timed_out')),
    active_attempt_id TEXT,
    created_at_unix_ns BIGINT NOT NULL,
    updated_at_unix_ns BIGINT NOT NULL,
    execution_version BIGINT NOT NULL CHECK (execution_version >= 0)
);

CREATE TABLE ryvus_attempts (
    execution_id TEXT NOT NULL REFERENCES ryvus_executions(execution_id) ON DELETE CASCADE,
    attempt_id TEXT PRIMARY KEY,
    attempt_number BIGINT NOT NULL CHECK (attempt_number >= 1),
    deadline_unix_ms BIGINT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'running', 'cancellation_requested', 'succeeded', 'failed', 'cancelled', 'timed_out')),
    ownership JSONB,
    outcome TEXT CHECK (outcome IN ('succeeded', 'failed', 'cancelled', 'timed_out', 'infrastructure_failed')),
    result JSONB,
    started_at_unix_ns BIGINT,
    finished_at_unix_ns BIGINT,
    UNIQUE (execution_id, attempt_number),
    UNIQUE (execution_id, attempt_id)
);

ALTER TABLE ryvus_executions
    ADD CONSTRAINT ryvus_executions_active_attempt_fk
    FOREIGN KEY (execution_id, active_attempt_id)
    REFERENCES ryvus_attempts(execution_id, attempt_id)
    DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE ryvus_cancellation_intents (
    execution_id TEXT PRIMARY KEY REFERENCES ryvus_executions(execution_id) ON DELETE CASCADE,
    requested_at_unix_ns BIGINT NOT NULL
);

CREATE TABLE ryvus_terminal_states (
    execution_id TEXT PRIMARY KEY REFERENCES ryvus_executions(execution_id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK (state IN ('succeeded', 'failed', 'cancelled', 'timed_out')),
    attempt_id TEXT,
    accepted_at_unix_ns BIGINT NOT NULL,
    FOREIGN KEY (execution_id, attempt_id)
        REFERENCES ryvus_attempts(execution_id, attempt_id)
        DEFERRABLE INITIALLY DEFERRED
);

CREATE INDEX ryvus_executions_active_idx
    ON ryvus_executions (execution_id)
    WHERE active_attempt_id IS NOT NULL;

CREATE INDEX ryvus_cancellation_reconciliation_idx
    ON ryvus_cancellation_intents (execution_id);
