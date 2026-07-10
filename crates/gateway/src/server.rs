use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::Router;
use ryvus_control::{ControlService, LocalControlConfig};
use ryvus_execution::{
    ExecutionPersistence, ExecutionService, Executor, LocalProcessExecutor, LocalRuntimeResolver,
    RuntimeResolver,
};
use ryvus_persistence::ConsoleExecutionPersistence;
use ryvus_protocol::{ActionDefinition, ActionKind};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

use crate::{
    cache::InMemoryAuthorizerCache,
    routes::{public::dynamic::handle_dynamic_route, system::system_routes},
    state::{AppState, GatewayExecutionService},
};

#[derive(Debug, Clone)]
pub struct GatewayServerConfig {
    pub project_root: PathBuf,
    pub manifest_path: PathBuf,
    pub addr: SocketAddr,
}

#[derive(Debug, Clone)]
pub struct GatewayValidation {
    pub action_count: usize,
    pub routes: Vec<RouteSummary>,
}

#[derive(Debug, Clone)]
pub struct RouteSummary {
    pub method: String,
    pub path: String,
    pub action: String,
}

impl GatewayServerConfig {
    pub fn manifest_path(&self) -> PathBuf {
        if self.manifest_path.is_absolute() {
            self.manifest_path.clone()
        } else {
            self.project_root.join(&self.manifest_path)
        }
    }
}

pub fn build_app(config: &GatewayServerConfig) -> Result<Router, Box<dyn std::error::Error>> {
    build_app_with_execution_service(config, build_execution_service(config.project_root.clone()))
}

pub fn build_app_with_execution_service(
    config: &GatewayServerConfig,
    execution_service: Arc<GatewayExecutionService>,
) -> Result<Router, Box<dyn std::error::Error>> {
    let control_service = Arc::new(ControlService::load_local(LocalControlConfig {
        project_root: config.project_root.clone(),
        manifest_path: config.manifest_path(),
    })?);

    validate_action_schemas(control_service.action_catalog().all())?;
    validate_runtime_targets(
        config.project_root.clone(),
        control_service.action_catalog().all(),
    )?;

    let state = AppState {
        control_service: Arc::clone(&control_service),
        execution_service,
        authorizer_cache: Arc::new(InMemoryAuthorizerCache::default()),
    };

    Ok(Router::<AppState>::new()
        .merge(system_routes())
        .fallback(handle_dynamic_route)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state))
}

pub fn build_execution_service(project_root: PathBuf) -> Arc<GatewayExecutionService> {
    Arc::new(ExecutionService::new(
        Arc::new(LocalRuntimeResolver::with_project_root(project_root)) as Arc<dyn RuntimeResolver>,
        Arc::new(LocalProcessExecutor::new()) as Arc<dyn Executor>,
        Arc::new(ConsoleExecutionPersistence) as Arc<dyn ExecutionPersistence>,
    ))
}

pub fn validate_config(
    config: &GatewayServerConfig,
) -> Result<GatewayValidation, Box<dyn std::error::Error>> {
    let control_service = ControlService::load_local(LocalControlConfig {
        project_root: config.project_root.clone(),
        manifest_path: config.manifest_path(),
    })?;
    let actions = control_service.action_catalog().all().collect::<Vec<_>>();

    validate_action_schemas(actions.iter().copied())?;
    validate_runtime_targets(config.project_root.clone(), actions.iter().copied())?;

    Ok(GatewayValidation {
        action_count: actions.len(),
        routes: route_summaries(actions.iter().copied()),
    })
}

pub async fn serve(config: GatewayServerConfig) -> Result<(), Box<dyn std::error::Error>> {
    serve_with_execution_service(config.clone(), build_execution_service(config.project_root)).await
}

pub async fn serve_with_execution_service(
    config: GatewayServerConfig,
    execution_service: Arc<GatewayExecutionService>,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = build_app_with_execution_service(&config, execution_service)?;

    tracing::info!("ryvus-gateway listening on http://{}", config.addr);

    let listener = tokio::net::TcpListener::bind(config.addr).await?;

    axum::serve(listener, app).await?;

    Ok(())
}

fn validate_action_schemas<'a>(
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
) -> Result<(), Box<dyn std::error::Error>> {
    for action in actions {
        let ActionKind::Api(api) = &action.kind else {
            continue;
        };

        if let Some(schema) = &api.request_schema {
            jsonschema::validator_for(schema).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "invalid request schema for {}::{}: {}",
                        action.source.display(),
                        action.entrypoint,
                        error
                    ),
                )
            })?;
        }

        if let Some(schema) = &api.response_schema {
            jsonschema::validator_for(schema).map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "invalid response schema for {}::{}: {}",
                        action.source.display(),
                        action.entrypoint,
                        error
                    ),
                )
            })?;
        }
    }

    Ok(())
}

fn validate_runtime_targets<'a>(
    project_root: PathBuf,
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
) -> Result<(), Box<dyn std::error::Error>> {
    let resolver = LocalRuntimeResolver::with_project_root(project_root);

    for action in actions {
        resolver.resolve(action)?;
    }

    Ok(())
}

fn route_summaries<'a>(
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
) -> Vec<RouteSummary> {
    let mut routes = actions
        .into_iter()
        .filter_map(|action| {
            let ActionKind::Api(api) = &action.kind else {
                return None;
            };

            Some(RouteSummary {
                method: api.method.clone(),
                path: api.path.clone(),
                action: format!("{}::{}", action.source.display(), action.entrypoint),
            })
        })
        .collect::<Vec<_>>();

    routes.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.method.cmp(&right.method))
            .then_with(|| left.action.cmp(&right.action))
    });

    routes
}
