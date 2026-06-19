use std::{net::SocketAddr, sync::Arc};

use axum::Router;
use ryvus_action_catalog::{ActionService, FileActionCatalog};
use ryvus_executor::{LocalProcessExecutor, LocalRuntimeResolver, RecordingExecutor};
use ryvus_gateway::{
    config::routes::GatewayConfig,
    openapi::{public::build_public_openapi_json, system::ApiDoc},
    registry::route_registry::RouteRegistry,
    routes::{public::dyanmic::handle_dynamic_route, system::system_routes},
    state::AppState,
};
use ryvus_persistence::FilesystemExecutionPersistence;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_target(true).compact().init();
    let action_catalog =
        FileActionCatalog::load("config/actions.json").expect("failed to load action catalog");

    let action_service = Arc::new(ActionService::new(action_catalog));

    let config = GatewayConfig::load("config/routes.json").expect("failed to load gateway config");

    let public_openapi = build_public_openapi_json(&config);

    let execution_service = Arc::new(ryvus_execution_service::ExecutionService::new(
        LocalRuntimeResolver::new(),
        RecordingExecutor::new(LocalProcessExecutor::new()),
        FilesystemExecutionPersistence::new(".ryvus/executions"),
    ));
    let state = AppState {
        route_registry: Arc::new(RouteRegistry::from_config(config)),
        action_service,
        execution_service,
    };

    //
    // System documentation
    //
    let system_swagger: Router<AppState> = SwaggerUi::new("/system/docs")
        .url("/system/openapi.json", ApiDoc::openapi())
        .into();

    //
    // Public documentation
    //
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

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));

    tracing::info!("ryvus-gateway listening on http://{addr}");
    tracing::info!("public swagger available at http://{addr}/docs");
    tracing::info!("system swagger available at http://{addr}/system/docs");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind gateway listener");

    axum::serve(listener, app)
        .await
        .expect("gateway server failed");
}
