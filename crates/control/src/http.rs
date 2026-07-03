use std::{
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use ryvus_docs::{DocContent, DocContentType};
use ryvus_protocol::RuntimeKind;
use serde_json::{json, Value};
use tower_http::{
    cors::CorsLayer,
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

use crate::ControlService;

#[derive(Clone)]
pub struct ControlState {
    pub control_service: Arc<ControlService>,
}

pub fn control_app(control_service: Arc<ControlService>) -> Router {
    control_app_with_routes(control_service, Router::new())
}

pub fn control_app_with_routes(control_service: Arc<ControlService>, routes: Router) -> Router {
    let portal_dist = portal_dist_dir();
    let portal_index = portal_dist.join("index.html");
    let control_routes = Router::new()
        .route("/control/catalog", get(catalog))
        .route("/control/specs/openapi", get(openapi))
        .route("/control/specs/schedules", get(schedules))
        .route("/control/specs/flows", get(flows))
        .route("/control/docs/registry", get(docs_registry))
        .route("/control/docs/pages/{id}", get(doc_page))
        .with_state(ControlState { control_service });

    Router::new()
        .merge(routes)
        .merge(control_routes)
        .fallback_service(ServeDir::new(portal_dist).fallback(ServeFile::new(portal_index)))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}

pub async fn serve(
    addr: SocketAddr,
    control_service: Arc<ControlService>,
) -> Result<(), Box<dyn std::error::Error>> {
    serve_with_routes(addr, control_service, Router::new()).await
}

pub async fn serve_with_routes(
    addr: SocketAddr,
    control_service: Arc<ControlService>,
    routes: Router,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = control_app_with_routes(control_service, routes);
    tracing::info!("ryvus-control listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn catalog(State(state): State<ControlState>) -> Json<Value> {
    Json(json!({
        "actions": state
            .control_service
            .action_catalog()
            .all()
            .cloned()
            .collect::<Vec<_>>()
    }))
}

async fn openapi(State(state): State<ControlState>) -> Result<Json<Value>, StatusCode> {
    state
        .control_service
        .docs_registry()
        .json_page("/openapi.json")
        .cloned()
        .map(Json)
        .map_err(|_| StatusCode::NOT_FOUND)
}

async fn schedules(State(state): State<ControlState>) -> Result<Json<Value>, StatusCode> {
    let actions = state
        .control_service
        .action_catalog()
        .all()
        .collect::<Vec<_>>();
    let infos = state
        .control_service
        .schedule_infos()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let schedules = infos
        .into_iter()
        .filter_map(|schedule| {
            let action = actions.iter().find(|action| {
                format!("{}::{}", action.source.display(), action.entrypoint) == schedule.action_key
            })?;

            Some(json!({
                "id": schedule.id,
                "name": schedule.name,
                "expression": schedule.expression,
                "runtime": runtime_label(&action.runtime),
                "handler": format!("{}::{}", schedule.source, schedule.entrypoint),
                "action": schedule.action_key,
                "enabled": true,
            }))
        })
        .collect::<Vec<_>>();

    Ok(Json(json!({ "schedules": schedules })))
}

async fn flows(State(state): State<ControlState>) -> Result<Json<Value>, StatusCode> {
    state
        .control_service
        .flow_spec()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn docs_registry(State(state): State<ControlState>) -> Json<Value> {
    let registry = state.control_service.docs_registry();
    let pages = registry
        .pages
        .iter()
        .map(|page| {
            json!({
                "id": page.id,
                "title": page.title,
                "path": page.path,
                "source": page.source,
                "content_type": page.content_type,
                "content_path": format!("/control/docs/pages/{}", page.id),
            })
        })
        .collect::<Vec<_>>();

    Json(json!({
        "nav": registry.nav,
        "pages": pages,
    }))
}

async fn doc_page(State(state): State<ControlState>, Path(id): Path<String>) -> Response {
    let Some(page) = state
        .control_service
        .docs_registry()
        .pages
        .iter()
        .find(|page| page.id == id)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    match (&page.content_type, &page.content) {
        (DocContentType::Markdown, DocContent::Text(content)) => (
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            content.clone(),
        )
            .into_response(),
        (_, DocContent::Json(content)) => Json(content.clone()).into_response(),
        (_, DocContent::Text(content)) => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            content.clone(),
        )
            .into_response(),
    }
}

fn runtime_label(runtime: &RuntimeKind) -> &'static str {
    match runtime {
        RuntimeKind::Python => "python",
        RuntimeKind::Node => "node",
        RuntimeKind::Rust => "rust",
    }
}

fn portal_dist_dir() -> PathBuf {
    FsPath::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/portal/dist")
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use axum::{
        body::{to_bytes, Body},
        http::{Method, Request, StatusCode},
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::LocalControlConfig;

    use super::*;

    #[tokio::test]
    async fn serves_control_artifacts() {
        let project_root = temp_project_root();
        let manifest_path = project_root.join(".ryvus/action-manifest.json");
        fs::create_dir_all(manifest_path.parent().expect("manifest should have parent"))
            .expect("manifest parent should be created");
        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&json!({
                "actions": [
                    {
                        "runtime": "Python",
                        "kind": {
                            "Api": {
                                "method": "GET",
                                "path": "/hello",
                                "query_params": []
                            }
                        },
                        "source": "src/hello.py",
                        "entrypoint": "hello"
                    },
                    {
                        "runtime": "Python",
                        "kind": {
                            "Schedule": {
                                "expression": "every 10s"
                            }
                        },
                        "source": "src/restock.py",
                        "entrypoint": "restock_report",
                        "name": "restock_report"
                    }
                ]
            }))
            .expect("manifest should serialize"),
        )
        .expect("manifest should be written");
        fs::write(
            project_root.join(".ryvus/flows.json"),
            serde_json::to_string_pretty(&json!({
                "flows": [
                    {
                        "key": "restock_flow",
                        "steps": [
                            {
                                "key": "restock",
                                "action": "restock_report"
                            }
                        ]
                    }
                ]
            }))
            .expect("flows should serialize"),
        )
        .expect("flows should be written");

        let control_service = Arc::new(
            ControlService::load_local(LocalControlConfig {
                project_root,
                manifest_path: ".ryvus/action-manifest.json".into(),
            })
            .expect("control should load"),
        );
        let app = control_app(control_service);

        let catalog = request_json(app.clone(), "/control/catalog").await;
        assert_eq!(
            catalog["actions"]
                .as_array()
                .expect("actions should be array")
                .len(),
            2
        );

        let openapi = request_json(app.clone(), "/control/specs/openapi").await;
        assert_eq!(
            openapi["paths"]["/hello"]["get"]["operationId"],
            json!("hello_get_hello")
        );
        assert!(openapi["paths"]["/system/schedules/restock_report/run"].is_null());

        let schedules = request_json(app.clone(), "/control/specs/schedules").await;
        assert_eq!(schedules["schedules"][0]["id"], json!("restock_report"));
        assert_eq!(
            schedules["schedules"][0]["handler"],
            json!("src/restock.py::restock_report")
        );
        assert_eq!(schedules["schedules"][0]["enabled"], json!(true));

        let flows = request_json(app.clone(), "/control/specs/flows").await;
        assert_eq!(flows["flows"][0]["key"], json!("restock_flow"));

        let registry = request_json(app.clone(), "/control/docs/registry").await;
        let openapi_page = registry["pages"]
            .as_array()
            .expect("pages should be array")
            .iter()
            .find(|page| page["path"] == "/openapi.json")
            .expect("openapi page should exist");
        assert_eq!(
            openapi_page["content_path"],
            json!("/control/docs/pages/public-openapi")
        );

        let page = request_json(app, "/control/docs/pages/public-openapi").await;
        assert_eq!(page["openapi"], json!("3.1.0"));
    }

    async fn request_json(app: Router, uri: &str) -> Value {
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("request should be handled");

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        serde_json::from_slice(&bytes).expect("body should be json")
    }

    fn temp_project_root() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ryvus-control-http-{id}"))
    }
}
