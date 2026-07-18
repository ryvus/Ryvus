use crate::{
    commands::{discover, project},
    error::{CliError, Result},
};
use ryvus_action_catalog::FileActionCatalog;
use ryvus_control::{ControlService, LocalControlConfig};
use std::sync::Arc;
use std::time::Duration;

fn observation_routes(
    control_service: Arc<ControlService>,
    execution_store: Arc<dyn ryvus_execution::ExecutionStateStore>,
    log_store: Arc<dyn ryvus_logging::ExecutionLogStore>,
    execution_scope: ryvus_execution::ExecutionScopeId,
) -> axum::Router {
    let action_read_service = Arc::new(ryvus_control::ActionReadService::new(
        control_service,
        Arc::clone(&execution_store),
        Arc::clone(&log_store),
        execution_scope.clone(),
    ));
    ryvus_control::action_read_routes(action_read_service)
        .merge(ryvus_execution::execution_history_routes(
            execution_store,
            execution_scope.clone(),
        ))
        .merge(ryvus_logging::http::log_history_routes(
            log_store,
            execution_scope,
        ))
}

pub fn run(run_schedules: bool) -> Result<()> {
    project::configure_python_path();

    discover::run()?;

    let config = project::gateway_config()?;
    let control_service = Arc::new(
        ControlService::load_local(LocalControlConfig {
            project_root: config.project_root.clone(),
            manifest_path: config.manifest_path(),
        })
        .map_err(|err| CliError::Validation(err.to_string()))?,
    );
    let control_addr = project::control_addr();
    let validation = ryvus_gateway::server::validate_config(&config)
        .map_err(|err| CliError::Validation(err.to_string()))?;
    let action_catalog = FileActionCatalog::load(config.manifest_path())
        .map_err(|err| CliError::Validation(err.to_string()))?;
    let composition = project::build_local_composition(&config)?;
    let execution_service = Arc::clone(&composition.execution_service);
    let scheduler_service = Arc::new(ryvus_scheduler::DurableSchedulerService::new(
        Arc::clone(&composition.schedule_store),
        Arc::clone(&execution_service),
        composition.execution_scope.clone(),
        composition.actor,
        "local-scheduler",
        Duration::from_secs(30),
    ));
    scheduler_service
        .reconcile(
            &action_catalog.all().cloned().collect::<Vec<_>>(),
            std::time::SystemTime::now(),
        )
        .map_err(|err| CliError::Validation(err.to_string()))?;
    let scheduler_routes = ryvus_scheduler::http::scheduler_routes(Arc::clone(&scheduler_service));
    let observation_routes = observation_routes(
        Arc::clone(&control_service),
        Arc::clone(&composition.execution_store),
        Arc::clone(&composition.log_store),
        composition.execution_scope.clone(),
    );
    let flow_store = Arc::new(ryvus_flow::InMemoryFlowStateStore::default());
    let flow_service = Arc::new(
        ryvus_flow::FlowService::new(
            control_service
                .typed_flow_spec()
                .map_err(|err| CliError::Validation(err.to_string()))?,
            action_catalog.all().cloned().collect(),
            flow_store,
            Arc::clone(&execution_service),
        )
        .map_err(|err| CliError::Validation(err.to_string()))?,
    );
    let control_routes = scheduler_routes
        .merge(observation_routes)
        .merge(ryvus_flow::http::flow_routes(flow_service));

    println!("Validated {} action(s)", validation.action_count);
    println!("Gateway: http://{}", config.addr);
    println!("Control: http://{}", control_addr);
    println!("Portal:  http://{}", control_addr);

    let runtime = tokio::runtime::Runtime::new().map_err(CliError::Io)?;
    let shutdown_grace = Duration::from_secs(3);

    if run_schedules {
        runtime.block_on(async move {
            let control_service = Arc::clone(&control_service);
            let shutdown_service = Arc::clone(&execution_service);
            tokio::select! {
                result = ryvus_gateway::server::serve_with_execution_service(
                    config,
                    execution_service.clone(),
                ) => result.map_err(|err| CliError::Gateway(err.to_string())),
                result = ryvus_control::http::serve_with_routes(control_addr, control_service, control_routes) => {
                    result.map_err(|err| CliError::Gateway(err.to_string()))
                },
                result = scheduler_service.run() => {
                    result.map_err(|err| CliError::Validation(err.to_string()))
                },
                result = tokio::signal::ctrl_c() => {
                    result.map_err(CliError::Io)?;
                    shutdown_service
                        .shutdown(shutdown_grace)
                        .map_err(|err| CliError::Gateway(err.to_string()))
                }
            }
        })?;
    } else {
        runtime.block_on(async move {
            let shutdown_service = Arc::clone(&execution_service);
            tokio::select! {
                result = ryvus_gateway::server::serve_with_execution_service(config, execution_service.clone()) => {
                    result.map_err(|err| CliError::Gateway(err.to_string()))
                },
                result = ryvus_control::http::serve_with_routes(control_addr, control_service, control_routes) => {
                    result.map_err(|err| CliError::Gateway(err.to_string()))
                },
                result = tokio::signal::ctrl_c() => {
                    result.map_err(CliError::Io)?;
                    shutdown_service
                        .shutdown(shutdown_grace)
                        .map_err(|err| CliError::Gateway(err.to_string()))
                }
            }
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, sync::Arc, time::SystemTime};

    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
        Router,
    };
    use ryvus_execution::{
        action_revision, ExecutionDataReferences, ExecutionPolicy, ExecutionStateStore,
        ExecutionTrigger, MemoryExecutionStateStore, NewExecution, RetryPolicy,
    };
    use ryvus_logging::{
        ExecutionLogStore, InMemoryExecutionLogStore, LogBatch, LogStreamId, LogStreamMetadata,
        LogStreamTransition,
    };
    use ryvus_protocol::{
        ActionDefinition, ActionExecutionPolicy, ActionKind, ActionManifest, ApiAction,
        ExecutionId, ExecutionScopeId, InvocationRequest, RuntimeHostId, RuntimeKind,
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::observation_routes;
    use ryvus_control::{ControlService, LocalControlConfig};

    #[tokio::test]
    async fn action_routes_share_the_injected_stores() -> Result<(), Box<dyn std::error::Error>> {
        let action = ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: "GET".into(),
                path: "/inventory".into(),
                consumes: vec!["application/json".into()],
                produces: vec!["application/json".into()],
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
                authorizer: None,
            }),
            source: PathBuf::from("src/inventory.py"),
            entrypoint: "inventory_handler".into(),
            name: Some("inventory".into()),
            policy: ActionExecutionPolicy::default(),
        };
        let root = std::env::temp_dir().join(format!(
            "ryvus-cli-action-routes-{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)?
                .as_nanos()
        ));
        fs::create_dir_all(root.join(".ryvus"))?;
        fs::write(
            root.join(".ryvus/action-manifest.json"),
            serde_json::to_vec(&ActionManifest {
                actions: vec![action.clone()],
            })?,
        )?;
        let control = Arc::new(ControlService::load_local(LocalControlConfig {
            project_root: root,
            manifest_path: PathBuf::from(".ryvus/action-manifest.json"),
        })?);
        let executions = Arc::new(MemoryExecutionStateStore::default());
        let logs = Arc::new(InMemoryExecutionLogStore::default());
        let scope = ExecutionScopeId::new("trusted-scope")?;
        let other_scope = ExecutionScopeId::new("other-scope")?;
        let revision = action_revision(&action)?;

        insert_execution(&executions, &action, &scope, &revision, "trusted-execution")?;
        insert_execution(
            &executions,
            &action,
            &other_scope,
            &revision,
            "other-execution",
        )?;
        insert_log(&logs, &scope, &revision)?;
        insert_log(&logs, &other_scope, &revision)?;

        let app = observation_routes(control, executions, logs, scope);
        let executions = get_json(app.clone(), "/internal/executions?action_id=inventory").await?;
        assert_eq!(executions["items"].as_array().map(Vec::len), Some(1));
        let streams = get_json(
            app.clone(),
            "/internal/logs/streams?action_key_id=inventory",
        )
        .await?;
        assert_eq!(streams["streams"].as_array().map(Vec::len), Some(1));
        let detail = get_json(app.clone(), "/internal/actions/detail?action_id=inventory").await?;
        assert_eq!(detail["recent_health"]["sample_size"], 1);
        let revisions = get_json(app, "/internal/actions/revisions?action_id=inventory").await?;
        assert_eq!(revisions["revisions"][0]["execution_count"], 1);
        assert_eq!(revisions["revisions"][0]["runtime_host_stream_count"], 1);
        Ok(())
    }

    fn insert_execution(
        store: &MemoryExecutionStateStore,
        action: &ActionDefinition,
        scope: &ExecutionScopeId,
        revision: &str,
        execution_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut request = InvocationRequest::new(json!({}));
        request.execution_id = ExecutionId::from(execution_id);
        store.create(NewExecution {
            action: action.clone(),
            action_revision: revision.into(),
            execution_scope_id: scope.clone(),
            action_id: "inventory".into(),
            trigger: ExecutionTrigger::Api,
            creation_fingerprint: format!("fingerprint-{execution_id}"),
            data_refs: ExecutionDataReferences::default(),
            request,
            policy: ExecutionPolicy {
                timeout: std::time::Duration::from_secs(3),
                retry: RetryPolicy {
                    max_attempts: 1,
                    initial_delay: std::time::Duration::from_secs(1),
                    backoff: 2.0,
                },
            },
            created_at: SystemTime::now(),
        })?;
        Ok(())
    }

    fn insert_log(
        store: &InMemoryExecutionLogStore,
        scope: &ExecutionScopeId,
        revision: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        store.append_batch(LogBatch {
            stream: LogStreamMetadata {
                stream_id: LogStreamId::new(scope.clone(), RuntimeHostId::from("shared-host")),
                action_key_id: "inventory".into(),
                action_revision: revision.into(),
                runtime_language: RuntimeKind::Python,
                started_at_unix_nanos: 1,
            },
            batch_id: format!("batch-{scope}"),
            records: Vec::new(),
            loss_ranges: Vec::new(),
            transition: Some(LogStreamTransition::Active),
        })?;
        Ok(())
    }

    async fn get_json(app: Router, uri: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let response = app.oneshot(Request::get(uri).body(Body::empty())?).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(serde_json::from_slice(&body)?)
    }
}
