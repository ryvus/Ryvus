use std::{net::SocketAddr, sync::Arc};

use axum::Router;
use ryvus_gateway::{
    config::routes::GatewayConfig,
    openapi::{public::build_public_openapi_json, system::ApiDoc},
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

    let public_openapi = build_public_openapi_json(&config);

    let state = AppState {
        route_registry: Arc::new(RouteRegistry::from_config(config)),
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
