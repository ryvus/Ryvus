use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::Router;
use ryvus_action_catalog::{ActionService, FileActionCatalog};
use ryvus_executor::{Executor, LocalProcessExecutor, LocalRuntimeResolver, RuntimeResolver};
use ryvus_persistence::{ConsoleExecutionPersistence, ExecutionPersistence};
use ryvus_protocol::{ActionDefinition, ActionKind};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::{
    openapi::{public::build_public_openapi_json_from_actions, system::ApiDoc},
    registry::route_registry::RouteRegistry,
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
    let manifest_path = config.manifest_path();

    let action_catalog = FileActionCatalog::load(&manifest_path)?;

    validate_action_schemas(action_catalog.all())?;
    let public_openapi = build_public_openapi_json_from_actions(action_catalog.all());
    let route_registry = RouteRegistry::from_actions(action_catalog.all())?;

    let action_service = Arc::new(ActionService::new(action_catalog));

    let execution_service: Arc<GatewayExecutionService> =
        Arc::new(ryvus_execution_service::ExecutionService::new(
            Arc::new(LocalRuntimeResolver::with_project_root(
                config.project_root.clone(),
            )) as Arc<dyn RuntimeResolver>,
            Arc::new(LocalProcessExecutor::new()) as Arc<dyn Executor>,
            Arc::new(ConsoleExecutionPersistence) as Arc<dyn ExecutionPersistence>,
        ));

    let state = AppState {
        route_registry: Arc::new(route_registry),
        action_service,
        execution_service,
    };

    let system_swagger: Router<AppState> = SwaggerUi::new("/system/docs")
        .url("/system/openapi.json", ApiDoc::openapi())
        .into();

    let public_swagger: Router<AppState> = SwaggerUi::new("/docs")
        .external_url_unchecked("/openapi.json", public_openapi)
        .into();

    Ok(Router::<AppState>::new()
        .merge(system_routes())
        .merge(system_swagger)
        .merge(public_swagger)
        .fallback(handle_dynamic_route)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state))
}

pub fn validate_config(
    config: &GatewayServerConfig,
) -> Result<GatewayValidation, Box<dyn std::error::Error>> {
    let action_catalog = FileActionCatalog::load(config.manifest_path())?;
    let actions = action_catalog.all().collect::<Vec<_>>();

    validate_action_schemas(actions.iter().copied())?;
    RouteRegistry::from_actions(actions.iter().copied())?;

    Ok(GatewayValidation {
        action_count: actions.len(),
        routes: route_summaries(actions.iter().copied()),
    })
}

pub async fn serve(config: GatewayServerConfig) -> Result<(), Box<dyn std::error::Error>> {
    let app = build_app(&config)?;

    tracing::info!("ryvus-gateway listening on http://{}", config.addr);
    tracing::info!("public swagger available at http://{}/docs", config.addr);
    tracing::info!(
        "system swagger available at http://{}/system/docs",
        config.addr
    );

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
