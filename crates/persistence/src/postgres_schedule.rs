use std::{
    sync::{mpsc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use postgres::{Client, NoTls, Transaction};
use ryvus_execution::{ActorRef, ExecutionScopeId, ScheduleId, ScheduleTriggerId};
use ryvus_protocol::ExecutionId;
use ryvus_scheduler::{
    ClaimOccurrenceRequest, ClaimOccurrenceResult, ClaimedTrigger, DiscoveredSchedule, DueSchedule,
    ManualTriggerRequest, ManualTriggerResult, MemoryScheduleStore, ReconcileResult,
    ScheduleOperationalEvent, ScheduleQuery, ScheduleRecord, ScheduleRevisionRecord, ScheduleStore,
    ScheduleStoreSnapshot, ScheduleTriggerRecord, SchedulerError, SchedulerResult, TriggerFailure,
    TriggerQuery,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

type ClientCommand = Box<dyn FnOnce(&mut Client) + Send>;

pub struct PostgresScheduleStore {
    commands: Option<mpsc::Sender<ClientCommand>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl PostgresScheduleStore {
    pub fn connect(database_url: &str) -> SchedulerResult<Self> {
        let database_url = database_url.to_owned();
        let (commands, receiver) = mpsc::channel::<ClientCommand>();
        let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("ryvus-postgres-scheduler".into())
            .spawn(move || {
                let mut client = match Client::connect(&database_url, NoTls) {
                    Ok(client) => client,
                    Err(error) => {
                        let _ = startup_sender.send(Err(backend("connect to PostgreSQL", error)));
                        return;
                    }
                };
                if startup_sender.send(Ok(())).is_err() {
                    return;
                }
                while let Ok(command) = receiver.recv() {
                    command(&mut client);
                }
            })
            .map_err(|error| SchedulerError::StoreBackend(error.to_string()))?;
        if let Err(error) = startup_receiver
            .recv()
            .map_err(|_| SchedulerError::StoreBackend("PostgreSQL worker stopped".into()))?
        {
            let _ = worker.join();
            return Err(error);
        }
        Ok(Self {
            commands: Some(commands),
            worker: Mutex::new(Some(worker)),
        })
    }

    fn with_store<T>(
        &self,
        write: bool,
        operation: impl FnOnce(&MemoryScheduleStore) -> SchedulerResult<T> + Send + 'static,
    ) -> SchedulerResult<T>
    where
        T: Send + 'static,
    {
        self.run(move |client| {
            let mut transaction = client
                .transaction()
                .map_err(|error| backend("begin schedule transaction", error))?;
            // ponytail: one database-wide scheduler lock favors correctness; replace with row-level writes when scheduler throughput requires it.
            transaction
                .batch_execute(if write {
                    "LOCK TABLE ryvus_schedules IN EXCLUSIVE MODE"
                } else {
                    "LOCK TABLE ryvus_schedules IN SHARE MODE"
                })
                .map_err(|error| backend("lock schedule state", error))?;
            let memory = MemoryScheduleStore::from_snapshot(load_snapshot(&mut transaction)?);
            let result = operation(&memory)?;
            if write {
                persist_snapshot(&mut transaction, memory.snapshot()?)?;
            }
            transaction
                .commit()
                .map_err(|error| backend("commit schedule transaction", error))?;
            Ok(result)
        })
    }

    fn run<T>(
        &self,
        operation: impl FnOnce(&mut Client) -> SchedulerResult<T> + Send + 'static,
    ) -> SchedulerResult<T>
    where
        T: Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.commands
            .as_ref()
            .ok_or_else(|| SchedulerError::StoreBackend("PostgreSQL store is closed".into()))?
            .send(Box::new(move |client| {
                let _ = sender.send(operation(client));
            }))
            .map_err(|_| SchedulerError::StoreBackend("PostgreSQL worker stopped".into()))?;
        receiver
            .recv()
            .map_err(|_| SchedulerError::StoreBackend("PostgreSQL worker stopped".into()))?
    }
}

impl Drop for PostgresScheduleStore {
    fn drop(&mut self) {
        self.commands.take();
        let worker = self
            .worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

impl ScheduleStore for PostgresScheduleStore {
    fn reconcile(
        &self,
        scope: &ExecutionScopeId,
        discovered: &[DiscoveredSchedule],
        observed_at: SystemTime,
    ) -> SchedulerResult<ReconcileResult> {
        let scope = scope.clone();
        let discovered = discovered.to_vec();
        self.with_store(true, move |store| {
            store.reconcile(&scope, &discovered, observed_at)
        })
    }

    fn list_due(
        &self,
        scope: &ExecutionScopeId,
        now: SystemTime,
        limit: usize,
    ) -> SchedulerResult<Vec<DueSchedule>> {
        let scope = scope.clone();
        self.with_store(false, move |store| store.list_due(&scope, now, limit))
    }

    fn claim_occurrence(
        &self,
        request: ClaimOccurrenceRequest,
    ) -> SchedulerResult<ClaimOccurrenceResult> {
        self.with_store(true, move |store| store.claim_occurrence(request))
    }

    fn recover_incomplete(
        &self,
        scope: &ExecutionScopeId,
        owner: &str,
        now: SystemTime,
        lease: Duration,
        limit: usize,
    ) -> SchedulerResult<Vec<ClaimedTrigger>> {
        let scope = scope.clone();
        let owner = owner.to_string();
        self.with_store(true, move |store| {
            store.recover_incomplete(&scope, &owner, now, lease, limit)
        })
    }

    fn link_execution(
        &self,
        trigger_id: &ScheduleTriggerId,
        execution_id: &ExecutionId,
        expected_version: u64,
    ) -> SchedulerResult<ScheduleTriggerRecord> {
        let trigger_id = trigger_id.clone();
        let execution_id = execution_id.clone();
        self.with_store(true, move |store| {
            store.link_execution(&trigger_id, &execution_id, expected_version)
        })
    }

    fn advance_schedule(
        &self,
        schedule_id: &ScheduleId,
        completed_trigger_id: &ScheduleTriggerId,
        expected_version: u64,
        next_trigger_at: SystemTime,
    ) -> SchedulerResult<ScheduleRecord> {
        let schedule_id = schedule_id.clone();
        let trigger_id = completed_trigger_id.clone();
        self.with_store(true, move |store| {
            store.advance_schedule(&schedule_id, &trigger_id, expected_version, next_trigger_at)
        })
    }

    fn miss_trigger(
        &self,
        trigger_id: &ScheduleTriggerId,
        expected_version: u64,
    ) -> SchedulerResult<ScheduleTriggerRecord> {
        let trigger_id = trigger_id.clone();
        self.with_store(true, move |store| {
            store.miss_trigger(&trigger_id, expected_version)
        })
    }

    fn fail_trigger(
        &self,
        trigger_id: &ScheduleTriggerId,
        failure: TriggerFailure,
        expected_version: u64,
    ) -> SchedulerResult<ScheduleTriggerRecord> {
        let trigger_id = trigger_id.clone();
        self.with_store(true, move |store| {
            store.fail_trigger(&trigger_id, failure, expected_version)
        })
    }

    fn enable(
        &self,
        schedule_id: &ScheduleId,
        actor: &ActorRef,
        at: SystemTime,
    ) -> SchedulerResult<ScheduleRecord> {
        let schedule_id = schedule_id.clone();
        let actor = actor.clone();
        self.with_store(true, move |store| store.enable(&schedule_id, &actor, at))
    }

    fn disable(
        &self,
        schedule_id: &ScheduleId,
        actor: &ActorRef,
        at: SystemTime,
    ) -> SchedulerResult<ScheduleRecord> {
        let schedule_id = schedule_id.clone();
        let actor = actor.clone();
        self.with_store(true, move |store| store.disable(&schedule_id, &actor, at))
    }

    fn create_manual_trigger(
        &self,
        request: ManualTriggerRequest,
    ) -> SchedulerResult<ManualTriggerResult> {
        self.with_store(true, move |store| store.create_manual_trigger(request))
    }

    fn get_schedule(&self, schedule_id: &ScheduleId) -> SchedulerResult<Option<ScheduleRecord>> {
        let schedule_id = schedule_id.clone();
        self.with_store(false, move |store| store.get_schedule(&schedule_id))
    }

    fn get_trigger(
        &self,
        trigger_id: &ScheduleTriggerId,
    ) -> SchedulerResult<Option<ScheduleTriggerRecord>> {
        let trigger_id = trigger_id.clone();
        self.with_store(false, move |store| store.get_trigger(&trigger_id))
    }

    fn list_schedules(&self, query: ScheduleQuery) -> SchedulerResult<Vec<ScheduleRecord>> {
        self.with_store(false, move |store| store.list_schedules(query))
    }

    fn list_revisions(
        &self,
        schedule_id: &ScheduleId,
    ) -> SchedulerResult<Vec<ScheduleRevisionRecord>> {
        let schedule_id = schedule_id.clone();
        self.with_store(false, move |store| store.list_revisions(&schedule_id))
    }

    fn list_triggers(&self, query: TriggerQuery) -> SchedulerResult<Vec<ScheduleTriggerRecord>> {
        self.with_store(false, move |store| store.list_triggers(query))
    }

    fn list_operational_events(
        &self,
        schedule_id: &ScheduleId,
        limit: usize,
    ) -> SchedulerResult<Vec<ScheduleOperationalEvent>> {
        let schedule_id = schedule_id.clone();
        self.with_store(false, move |store| {
            store.list_operational_events(&schedule_id, limit)
        })
    }
}

fn load_snapshot(transaction: &mut Transaction<'_>) -> SchedulerResult<ScheduleStoreSnapshot> {
    Ok(ScheduleStoreSnapshot {
        schedules: load_records(transaction, "SELECT record FROM ryvus_schedules ORDER BY schedule_id")?,
        revisions: load_records(transaction, "SELECT record FROM ryvus_schedule_revisions ORDER BY schedule_id, schedule_revision")?,
        triggers: load_records(transaction, "SELECT record FROM ryvus_schedule_triggers ORDER BY trigger_id")?,
        manual_idempotency: load_records(transaction, "SELECT record FROM ryvus_schedule_manual_idempotency ORDER BY execution_scope_id, schedule_id, key_hash")?,
        operational_events: load_records(transaction, "SELECT record FROM ryvus_schedule_operational_events ORDER BY event_order")?,
    })
}

fn load_records<T: DeserializeOwned>(
    transaction: &mut Transaction<'_>,
    sql: &str,
) -> SchedulerResult<Vec<T>> {
    transaction
        .query(sql, &[])
        .map_err(|error| backend("load schedule state", error))?
        .into_iter()
        .map(|row| {
            serde_json::from_value(row.get::<_, Value>(0))
                .map_err(|error| SchedulerError::StoreBackend(error.to_string()))
        })
        .collect()
}

fn persist_snapshot(
    transaction: &mut Transaction<'_>,
    snapshot: ScheduleStoreSnapshot,
) -> SchedulerResult<()> {
    transaction
        .batch_execute(
            "DELETE FROM ryvus_schedule_operational_events; \
             DELETE FROM ryvus_schedule_manual_idempotency; \
             DELETE FROM ryvus_schedule_triggers; \
             DELETE FROM ryvus_schedule_revisions; \
             DELETE FROM ryvus_schedules;",
        )
        .map_err(|error| backend("replace schedule state", error))?;
    for record in snapshot.schedules {
        let next = optional_time(record.next_trigger_at)?;
        transaction
            .execute(
                "INSERT INTO ryvus_schedules \
                 (schedule_id, execution_scope_id, stable_schedule_key, next_trigger_at_unix_ns, record) \
                 VALUES ($1, $2, $3, $4, $5)",
                &[
                    &record.schedule_id.as_ref(),
                    &record.execution_scope_id.as_ref(),
                    &record.stable_schedule_key,
                    &next,
                    &json(&record)?,
                ],
            )
            .map_err(|error| backend("insert schedule", error))?;
    }
    for record in snapshot.revisions {
        let revision = i64::try_from(record.schedule_revision)
            .map_err(|_| SchedulerError::StoreBackend("schedule revision overflow".into()))?;
        transaction
            .execute(
                "INSERT INTO ryvus_schedule_revisions (schedule_id, schedule_revision, record) \
                 VALUES ($1, $2, $3)",
                &[&record.schedule_id.as_ref(), &revision, &json(&record)?],
            )
            .map_err(|error| backend("insert schedule revision", error))?;
    }
    for record in snapshot.triggers {
        let revision = i64::try_from(record.schedule_revision)
            .map_err(|_| SchedulerError::StoreBackend("schedule revision overflow".into()))?;
        let scheduled_for = optional_time(record.scheduled_for)?;
        let claim_expires_at = optional_time(record.claim_expires_at)?;
        let status = serde_json::to_value(record.status)
            .map_err(|error| SchedulerError::StoreBackend(error.to_string()))?
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        transaction
            .execute(
                "INSERT INTO ryvus_schedule_triggers \
                 (trigger_id, schedule_id, schedule_revision, scheduled_for_unix_ns, execution_id, status, claim_expires_at_unix_ns, record) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
                &[
                    &record.trigger_id.as_ref(),
                    &record.schedule_id.as_ref(),
                    &revision,
                    &scheduled_for,
                    &record.execution_id.as_ref().map(AsRef::as_ref),
                    &status,
                    &claim_expires_at,
                    &json(&record)?,
                ],
            )
            .map_err(|error| backend("insert schedule trigger", error))?;
    }
    for record in snapshot.manual_idempotency {
        transaction
            .execute(
                "INSERT INTO ryvus_schedule_manual_idempotency \
                 (execution_scope_id, schedule_id, key_hash, record) VALUES ($1, $2, $3, $4)",
                &[
                    &record.execution_scope_id.as_ref(),
                    &record.schedule_id.as_ref(),
                    &record.key_hash,
                    &json(&record)?,
                ],
            )
            .map_err(|error| backend("insert manual idempotency", error))?;
    }
    for record in snapshot.operational_events {
        transaction
            .execute(
                "INSERT INTO ryvus_schedule_operational_events (schedule_id, record) VALUES ($1, $2)",
                &[&record.schedule_id.as_ref(), &json(&record)?],
            )
            .map_err(|error| backend("insert schedule event", error))?;
    }
    Ok(())
}

fn json(value: &impl Serialize) -> SchedulerResult<Value> {
    serde_json::to_value(value).map_err(|error| SchedulerError::StoreBackend(error.to_string()))
}

fn optional_time(value: Option<SystemTime>) -> SchedulerResult<Option<i64>> {
    value.map(time).transpose()
}

fn time(value: SystemTime) -> SchedulerResult<i64> {
    let nanos = value
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SchedulerError::StoreBackend("time before Unix epoch".into()))?
        .as_nanos();
    i64::try_from(nanos).map_err(|_| SchedulerError::StoreBackend("time overflow".into()))
}

fn backend(context: &str, error: postgres::Error) -> SchedulerError {
    SchedulerError::StoreBackend(format!("{context}: {error}"))
}
