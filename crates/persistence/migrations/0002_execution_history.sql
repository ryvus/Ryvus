ALTER TABLE ryvus_executions
    ADD COLUMN execution_scope_id TEXT,
    ADD COLUMN action_id TEXT,
    ADD COLUMN trigger JSONB,
    ADD COLUMN creation_fingerprint TEXT,
    ADD COLUMN data_refs JSONB NOT NULL DEFAULT '{}';

UPDATE ryvus_executions
SET execution_scope_id = 'legacy',
    action_id = COALESCE(action ->> 'name', action ->> 'entrypoint', 'legacy'),
    trigger = '{"type":"unknown"}',
    creation_fingerprint = 'legacy:' || execution_id;

ALTER TABLE ryvus_executions
    ALTER COLUMN execution_scope_id SET NOT NULL,
    ALTER COLUMN action_id SET NOT NULL,
    ALTER COLUMN trigger SET NOT NULL,
    ALTER COLUMN creation_fingerprint SET NOT NULL,
    ADD CONSTRAINT ryvus_executions_scope_nonempty CHECK (execution_scope_id <> ''),
    ADD CONSTRAINT ryvus_executions_action_id_nonempty CHECK (action_id <> ''),
    ADD CONSTRAINT ryvus_executions_fingerprint_nonempty CHECK (creation_fingerprint <> '');

ALTER TABLE ryvus_attempts
    ADD COLUMN data_refs JSONB NOT NULL DEFAULT '{}';

CREATE INDEX ryvus_executions_history_idx
    ON ryvus_executions (execution_scope_id, created_at_unix_ns DESC, execution_id DESC);

CREATE INDEX ryvus_executions_action_history_idx
    ON ryvus_executions (
        execution_scope_id,
        action_id,
        action_revision,
        created_at_unix_ns DESC,
        execution_id DESC
    );

CREATE TABLE ryvus_schedules (
    schedule_id TEXT PRIMARY KEY,
    execution_scope_id TEXT NOT NULL,
    stable_schedule_key TEXT NOT NULL,
    next_trigger_at_unix_ns BIGINT,
    record JSONB NOT NULL,
    UNIQUE (execution_scope_id, stable_schedule_key)
);

CREATE TABLE ryvus_schedule_revisions (
    schedule_id TEXT NOT NULL REFERENCES ryvus_schedules(schedule_id) ON DELETE CASCADE,
    schedule_revision BIGINT NOT NULL,
    record JSONB NOT NULL,
    PRIMARY KEY (schedule_id, schedule_revision)
);

CREATE TABLE ryvus_schedule_triggers (
    trigger_id TEXT PRIMARY KEY,
    schedule_id TEXT NOT NULL REFERENCES ryvus_schedules(schedule_id) ON DELETE CASCADE,
    schedule_revision BIGINT NOT NULL,
    scheduled_for_unix_ns BIGINT,
    execution_id TEXT,
    status TEXT NOT NULL,
    claim_expires_at_unix_ns BIGINT,
    record JSONB NOT NULL,
    UNIQUE (schedule_id, schedule_revision, scheduled_for_unix_ns)
);

CREATE TABLE ryvus_schedule_manual_idempotency (
    execution_scope_id TEXT NOT NULL,
    schedule_id TEXT NOT NULL REFERENCES ryvus_schedules(schedule_id) ON DELETE CASCADE,
    key_hash TEXT NOT NULL,
    record JSONB NOT NULL,
    PRIMARY KEY (execution_scope_id, schedule_id, key_hash)
);

CREATE TABLE ryvus_schedule_operational_events (
    event_order BIGSERIAL PRIMARY KEY,
    schedule_id TEXT NOT NULL REFERENCES ryvus_schedules(schedule_id) ON DELETE CASCADE,
    record JSONB NOT NULL
);

CREATE INDEX ryvus_schedules_due_idx
    ON ryvus_schedules (execution_scope_id, next_trigger_at_unix_ns)
    WHERE next_trigger_at_unix_ns IS NOT NULL;

CREATE INDEX ryvus_schedule_triggers_recovery_idx
    ON ryvus_schedule_triggers (status, claim_expires_at_unix_ns);

CREATE INDEX ryvus_schedule_triggers_execution_idx
    ON ryvus_schedule_triggers (execution_id)
    WHERE execution_id IS NOT NULL;
