use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::Router;
use ryvus_action_catalog::{ActionService, FileActionCatalog};
use ryvus_executor::{Executor, LocalProcessExecutor, LocalRuntimeResolver, RuntimeResolver};
use ryvus_persistence::{ConsoleExecutionPersistence, ExecutionPersistence};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::{
    openapi::{public::build_public_openapi_json_from_actions, system::ApiDoc},
    registry::route_registry::RouteRegistry,
    routes::{public::dyanmic::handle_dynamic_route, system::system_routes},
    state::{AppState, GatewayExecutionService},
};

#[derive(Debug, Clone)]
pub struct GatewayServerConfig {
    pub project_root: PathBuf,
    pub manifest_path: PathBuf,
    pub addr: SocketAddr,
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

pub async fn serve(config: GatewayServerConfig) -> Result<(), Box<dyn std::error::Error>> {
    let manifest_path = config.manifest_path();

    let action_catalog = FileActionCatalog::load(&manifest_path)?;

    let public_openapi = build_public_openapi_json_from_actions(action_catalog.all());
    let route_registry = RouteRegistry::from_actions(action_catalog.all());

    let action_service = Arc::new(ActionService::new(action_catalog));

    let execution_service: Arc<GatewayExecutionService> =
        Arc::new(ryvus_execution_service::ExecutionService::new(
            Arc::new(LocalRuntimeResolver::new()) as Arc<dyn RuntimeResolver>,
            Arc::new(LocalProcessExecutor::new()) as Arc<dyn Executor>,
            Arc::new(ConsoleExecutionPersistence::default()) as Arc<dyn ExecutionPersistence>,
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

    let app = Router::<AppState>::new()
        .merge(system_routes())
        .merge(system_swagger)
        .merge(public_swagger)
        .fallback(handle_dynamic_route)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

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
