use std::{net::SocketAddr, sync::Arc};

use axum::Router;
use ryvus_gateway::{
    config::routes::GatewayConfig,
    openapi::ApiDoc,
    registry::route_registry::RouteRegistry,
    routes::{public::dyanmic::handle_dynamic_route, system::system_routes},
    state::AppState,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .init();

    let config = GatewayConfig::load("config/routes.json").expect("failed to load gateway config");

    let state = AppState {
        route_registry: Arc::new(RouteRegistry::from_config(config)),
    };

    let swagger_routes: Router<AppState> = SwaggerUi::new("/system/docs")
        .url("/system/openapi.json", ApiDoc::openapi())
        .into();

    let app = Router::<AppState>::new()
        .merge(system_routes())
        .merge(swagger_routes)
        .fallback(handle_dynamic_route)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));

    tracing::info!("ryvus-gateway listening on http://{addr}");
    tracing::info!("swagger ui available at http://{addr}/system/docs");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind gateway listener");

    axum::serve(listener, app)
        .await
        .expect("gateway server failed");
}
