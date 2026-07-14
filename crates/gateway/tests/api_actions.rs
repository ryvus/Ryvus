mod support;

use axum::http::{Method, StatusCode};
use ryvus_execution::{ExecutionState, ExecutionStateStore, MemoryExecutionStateStore};
use ryvus_gateway::server;
use ryvus_protocol::ExecutionId;
use serde_json::json;
use std::sync::Arc;

use support::*;

#[tokio::test]
async fn invokes_get_api_action() {
    let project = TestProject::new("get");
    project.add_action(
        "hello.py",
        r#"
@api_action(method="GET", path="/hello")
def hello(event, context):
    print("hello log")
    return {"message": "Hello from Ryvus"}
"#,
    );
    project.write_manifest(&[action("GET", "/hello", "src/hello.py", "hello")]);

    let response = request(&project, Method::GET, "/hello", None).await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.raw_body);
    assert_eq!(response.body, json!({"message": "Hello from Ryvus"}));
}

#[tokio::test]
async fn local_gateway_records_execution_in_its_injected_store() {
    let project = TestProject::new("shared-execution-store");
    project.add_action(
        "identity.py",
        r#"
@api_action(method="GET", path="/identity")
def identity(context):
    return {"execution_id": context.execution_id}
"#,
    );
    project.write_manifest(&[action("GET", "/identity", "src/identity.py", "identity")]);

    let store = Arc::new(MemoryExecutionStateStore::default());
    let execution_service =
        server::build_execution_service_with_store(project.config().project_root, store.clone());
    let app = server::build_app_with_execution_service(&project.config(), execution_service)
        .expect("gateway app should build");

    let response = raw_request_with_headers_on_app(app, Method::GET, "/identity", "", &[]).await;

    assert_eq!(response.status, StatusCode::OK, "{}", response.raw_body);
    let execution_id = response.body["execution_id"]
        .as_str()
        .map(ExecutionId::from)
        .expect("action should return its execution id");
    let aggregate = store
        .load(&execution_id)
        .expect("store read should succeed")
        .expect("execution should be recorded");
    assert_eq!(aggregate.state, ExecutionState::Succeeded);
    assert_eq!(aggregate.attempts.len(), 1);
}

#[tokio::test]
async fn serves_openapi_but_not_docs() {
    let project = TestProject::new("docs");
    project.add_action(
        "hello.py",
        r#"
@api_action(method="GET", path="/hello")
def hello():
    return {"ok": True}
"#,
    );
    project.add_action(
        "restock.py",
        r#"
@scheduled_action(every="10s")
def restock_report(context):
    return {"ok": True}
"#,
    );
    project.write_manifest(&[
        action("GET", "/hello", "src/hello.py", "hello"),
        schedule_action("src/restock.py", "restock_report", "every 10s"),
    ]);

    let docs = request(&project, Method::GET, "/docs", None).await;
    assert_eq!(docs.status, StatusCode::NOT_FOUND);

    let openapi = request(&project, Method::GET, "/openapi.json", None).await;
    assert_eq!(openapi.status, StatusCode::OK);
    assert_eq!(openapi.body["openapi"], json!("3.1.0"));
    assert!(openapi.body["paths"]["/hello"]["get"].is_object());
    assert!(openapi.body["paths"]["/system/schedules"].is_null());
}

#[tokio::test]
async fn keeps_system_health_without_system_docs() {
    let project = TestProject::new("system-health");
    project.write_manifest(&[]);

    let health = request(&project, Method::GET, "/system/health", None).await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.body, json!({ "status": "ok" }));

    let system_docs = request(&project, Method::GET, "/system/docs", None).await;
    assert_eq!(system_docs.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn does_not_serve_system_schedules() {
    let project = TestProject::new("system-schedules");
    project.add_action(
        "restock.py",
        r#"
@scheduled_action(every="10s")
def restock_report(context):
    return {"ok": True}
"#,
    );
    project.write_manifest(&[schedule_action(
        "src/restock.py",
        "restock_report",
        "every 10s",
    )]);

    let response = request(&project, Method::GET, "/system/schedules", None).await;

    assert_eq!(response.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn does_not_run_system_schedules() {
    let project = TestProject::new("system-schedule-run");
    project.add_action(
        "restock.py",
        r#"
@scheduled_action(every="10s")
def restock_report(event):
    return {
        "expression": event.expression,
    }
"#,
    );
    project.write_manifest(&[schedule_action(
        "src/restock.py",
        "restock_report",
        "every 10s",
    )]);

    let response = request(
        &project,
        Method::POST,
        "/system/schedules/restock_report/run",
        None,
    )
    .await;

    assert_eq!(response.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invokes_node_api_action() {
    let project = TestProject::new("node-get");
    project.add_node_action(
        "hello.js",
        r#"
export default apiAction({
  method: "GET",
  path: "/node/hello",
  handler(event, context) {
    return {
      message: "Hello from Node",
      execution_id: context.executionId,
      attempt_id: context.attemptId,
      attempt_number: context.attemptNumber,
      event,
    };
  },
});
"#,
    );
    project.write_manifest(&[node_action("GET", "/node/hello", "src/hello.js", "default")]);

    let response = request(&project, Method::GET, "/node/hello", None).await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body["message"], json!("Hello from Node"));
    assert!(response.body["execution_id"].is_string());
    assert!(response.body["attempt_id"].is_string());
    assert_eq!(response.body["attempt_number"], json!(1));
    assert_eq!(response.body["event"]["body"], json!(null));
}

#[tokio::test]
async fn invokes_node_api_action_with_object_binding_and_schemas() {
    let project = TestProject::new("node-binding");
    project.add_node_action(
        "store.js",
        r#"
const itemSchema = object({
  sku: string(),
  quantity: integer(),
});

export default apiAction({
  method: "POST",
  path: "/store/carts/{cart_id}",
  query: {
    confirm: boolean(),
  },
  body: object({
    sku: string(),
    quantity: integer(),
  }),
  response: object({
    cart: itemSchema,
    cart_id: string(),
    confirmed: boolean(),
  }),
  handler({ path, query, body }) {
    return {
      cart: body,
      cart_id: path.cart_id,
      confirmed: query.confirm,
    };
  },
});
"#,
    );
    project.write_manifest(&[json!({
        "runtime": "Node",
        "kind": {
            "Api": {
                "method": "POST",
                "path": "/store/carts/{cart_id}",
                "query_params": [
                    {
                        "name": "confirm",
                        "required": true,
                        "schema": { "type": "boolean" }
                    }
                ],
                "request_schema": {
                    "type": "object",
                    "required": ["sku", "quantity"],
                    "properties": {
                        "sku": { "type": "string" },
                        "quantity": { "type": "integer" }
                    }
                },
                "response_schema": {
                    "type": "object",
                    "required": ["cart", "cart_id", "confirmed"],
                    "properties": {
                        "cart": {
                            "type": "object",
                            "required": ["sku", "quantity"],
                            "properties": {
                                "sku": { "type": "string" },
                                "quantity": { "type": "integer" }
                            }
                        },
                        "cart_id": { "type": "string" },
                        "confirmed": { "type": "boolean" }
                    }
                }
            }
        },
        "source": "src/store.js",
        "entrypoint": "default"
    })]);

    let response = request(
        &project,
        Method::POST,
        "/store/carts/cart_123?confirm=true",
        Some(json!({ "sku": "food_salmon_2kg", "quantity": 2 })),
    )
    .await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body["cart_id"], json!("cart_123"));
    assert_eq!(response.body["confirmed"], json!(true));
    assert_eq!(response.body["cart"]["quantity"], json!(2));

    let invalid_query = request(
        &project,
        Method::POST,
        "/store/carts/cart_123?confirm=maybe",
        Some(json!({ "sku": "food_salmon_2kg", "quantity": 2 })),
    )
    .await;
    assert_eq!(invalid_query.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_query.body["error"],
        json!("request_validation_failed")
    );
}

#[tokio::test]
async fn authorizer_allows_and_passes_metadata() {
    let project = TestProject::new("authorizer-allow");
    project.add_action(
        "auth.py",
        r#"
@authorizer(name="petstore")
def auth(event):
    assert event.headers["authorization"] == "Bearer dev"
    assert event.method == "GET"
    assert event.path == "/pets"
    return {"effect": "allow", "principal_id": "dev-user", "context": {"role": "dev"}}
"#,
    );
    project.add_action(
        "pets.py",
        r#"
@api_action(method="GET", path="/pets", authorizer="petstore")
def pets(context):
    return context.metadata["authorizer"]
"#,
    );
    project.write_manifest(&[
        authorizer_action("src/auth.py", "auth", "petstore"),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets",
                    "query_params": [],
                    "authorizer": "petstore"
                }
            },
            "source": "src/pets.py",
            "entrypoint": "pets"
        }),
    ]);

    let response = raw_request_with_headers(
        &project,
        Method::GET,
        "/pets",
        "",
        &[("authorization", "Bearer dev")],
    )
    .await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body["name"], json!("petstore"));
    assert_eq!(response.body["principal_id"], json!("dev-user"));
    assert_eq!(response.body["context"]["role"], json!("dev"));
}

#[tokio::test]
async fn cached_authorizer_allow_skips_second_authorizer_run() {
    let project = TestProject::new("authorizer-cache-allow");
    project.add_action(
        "auth.py",
        r#"
from pathlib import Path

@authorizer(name="petstore")
def auth(event):
    count_file = Path(".auth_count")
    if count_file.exists():
        return {"effect": "deny", "reason": "authorizer ran twice"}
    count_file.write_text("1")
    return {"effect": "allow", "principal_id": "cached-user", "context": {"role": "cached"}}
"#,
    );
    project.add_action(
        "pets.py",
        r#"
@api_action(method="GET", path="/pets", authorizer="petstore")
def pets(context):
    return context.metadata["authorizer"]
"#,
    );
    project.write_manifest(&[
        json!({
            "runtime": "Python",
            "kind": {
                "Authorizer": {
                    "security": [{ "type": "http", "scheme": "bearer" }],
                    "cache": { "ttl_seconds": 60 }
                }
            },
            "source": "src/auth.py",
            "entrypoint": "auth",
            "name": "petstore"
        }),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets",
                    "query_params": [],
                    "authorizer": "petstore"
                }
            },
            "source": "src/pets.py",
            "entrypoint": "pets"
        }),
    ]);

    let app = server::build_app(&project.config()).expect("gateway app should build");

    for _ in 0..2 {
        let response = raw_request_with_headers_on_app(
            app.clone(),
            Method::GET,
            "/pets",
            "",
            &[("authorization", "Bearer dev")],
        )
        .await;

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body["principal_id"], json!("cached-user"));
    }
}

#[tokio::test]
async fn authorizer_denies_are_not_cached() {
    let project = TestProject::new("authorizer-cache-deny");
    project.add_action(
        "auth.py",
        r#"
from pathlib import Path

@authorizer(name="petstore")
def auth(event):
    count_file = Path(".auth_count")
    if count_file.exists():
        return {"effect": "allow", "principal_id": "second-run"}
    count_file.write_text("1")
    return {"effect": "deny", "reason": "first run denied"}
"#,
    );
    project.add_action(
        "pets.py",
        r#"
@api_action(method="GET", path="/pets", authorizer="petstore")
def pets(context):
    return context.metadata["authorizer"]
"#,
    );
    project.write_manifest(&[
        json!({
            "runtime": "Python",
            "kind": {
                "Authorizer": {
                    "security": [{ "type": "http", "scheme": "bearer" }],
                    "cache": { "ttl_seconds": 60 }
                }
            },
            "source": "src/auth.py",
            "entrypoint": "auth",
            "name": "petstore"
        }),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets",
                    "query_params": [],
                    "authorizer": "petstore"
                }
            },
            "source": "src/pets.py",
            "entrypoint": "pets"
        }),
    ]);

    let first = raw_request_with_headers(
        &project,
        Method::GET,
        "/pets",
        "",
        &[("authorization", "Bearer dev")],
    )
    .await;
    assert_public_error(first, StatusCode::FORBIDDEN, "forbidden");

    let second = raw_request_with_headers(
        &project,
        Method::GET,
        "/pets",
        "",
        &[("authorization", "Bearer dev")],
    )
    .await;
    assert_eq!(second.status, StatusCode::OK);
    assert_eq!(second.body["principal_id"], json!("second-run"));
}

#[tokio::test]
async fn authorizer_denies_without_invoking_action() {
    let project = TestProject::new("authorizer-deny");
    project.add_action(
        "auth.py",
        r#"
@authorizer(name="petstore")
def auth(event):
    return {"effect": "deny", "reason": "missing scope"}
"#,
    );
    project.add_action(
        "pets.py",
        r#"
@api_action(method="GET", path="/pets", authorizer="petstore")
def pets():
    raise RuntimeError("should not run")
"#,
    );
    project.write_manifest(&[
        authorizer_action("src/auth.py", "auth", "petstore"),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets",
                    "query_params": [],
                    "authorizer": "petstore"
                }
            },
            "source": "src/pets.py",
            "entrypoint": "pets"
        }),
    ]);

    let response = request(&project, Method::GET, "/pets", None).await;

    assert_public_error(response, StatusCode::FORBIDDEN, "forbidden");
}

#[tokio::test]
async fn authorizer_unauthorized_without_invoking_action() {
    let project = TestProject::new("authorizer-unauthorized");
    project.add_action(
        "auth.py",
        r#"
@authorizer(name="petstore")
def auth(event):
    return {"effect": "unauthorized", "reason": "missing token"}
"#,
    );
    project.add_action(
        "pets.py",
        r#"
@api_action(method="GET", path="/pets", authorizer="petstore")
def pets():
    raise RuntimeError("should not run")
"#,
    );
    project.write_manifest(&[
        authorizer_action("src/auth.py", "auth", "petstore"),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets",
                    "query_params": [],
                    "authorizer": "petstore"
                }
            },
            "source": "src/pets.py",
            "entrypoint": "pets"
        }),
    ]);

    let response = request(&project, Method::GET, "/pets", None).await;

    assert_public_error(response, StatusCode::UNAUTHORIZED, "unauthorized");
}

#[tokio::test]
async fn malformed_authorizer_output_returns_internal_error() {
    let project = TestProject::new("authorizer-malformed");
    project.add_action(
        "auth.py",
        r#"
@authorizer(name="petstore")
def auth(event):
    return {"ok": True}
"#,
    );
    project.add_action(
        "pets.py",
        r#"
@api_action(method="GET", path="/pets", authorizer="petstore")
def pets():
    return {"ok": True}
"#,
    );
    project.write_manifest(&[
        authorizer_action("src/auth.py", "auth", "petstore"),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets",
                    "query_params": [],
                    "authorizer": "petstore"
                }
            },
            "source": "src/pets.py",
            "entrypoint": "pets"
        }),
    ]);

    let response = request(&project, Method::GET, "/pets", None).await;

    assert_public_error(
        response,
        StatusCode::INTERNAL_SERVER_ERROR,
        "authorizer_failed",
    );
}

#[tokio::test]
async fn missing_required_authorizer_parameter_returns_bad_request() {
    let project = TestProject::new("authorizer-required-param");
    project.add_action(
        "auth.py",
        r#"
@authorizer(name="petstore")
def auth(event):
    raise RuntimeError("should not run")
"#,
    );
    project.add_action(
        "pets.py",
        r#"
@api_action(method="GET", path="/pets", authorizer="petstore")
def pets():
    raise RuntimeError("should not run")
"#,
    );
    project.write_manifest(&[
        json!({
            "runtime": "Python",
            "kind": {
                "Authorizer": {
                    "parameters": [
                        { "name": "X-Tenant-ID", "in": "header", "required": true, "type": "string" }
                    ]
                }
            },
            "source": "src/auth.py",
            "entrypoint": "auth",
            "name": "petstore"
        }),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets",
                    "query_params": [],
                    "authorizer": "petstore"
                }
            },
            "source": "src/pets.py",
            "entrypoint": "pets"
        }),
    ]);

    let response = request(&project, Method::GET, "/pets", None).await;

    assert_public_error(
        response,
        StatusCode::BAD_REQUEST,
        "request_validation_failed",
    );
}

#[tokio::test]
async fn missing_authorizer_security_returns_unauthorized_before_parameters() {
    let project = TestProject::new("authorizer-missing-security");
    project.add_action(
        "auth.py",
        r#"
@authorizer(name="petstore")
def auth(event):
    raise RuntimeError("should not run")
"#,
    );
    project.add_action(
        "pets.py",
        r#"
@api_action(method="GET", path="/pets", authorizer="petstore")
def pets():
    raise RuntimeError("should not run")
"#,
    );
    project.write_manifest(&[
        json!({
            "runtime": "Python",
            "kind": {
                "Authorizer": {
                    "security": [
                        { "type": "http", "scheme": "bearer" }
                    ],
                    "parameters": [
                        { "name": "X-Tenant-ID", "in": "header", "required": true, "type": "string" }
                    ]
                }
            },
            "source": "src/auth.py",
            "entrypoint": "auth",
            "name": "petstore"
        }),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets",
                    "query_params": [],
                    "authorizer": "petstore"
                }
            },
            "source": "src/pets.py",
            "entrypoint": "pets"
        }),
    ]);

    let response = request(&project, Method::GET, "/pets", None).await;

    assert!(
        response
            .raw_body
            .contains("authorizer security credentials are required"),
        "{}",
        response.raw_body
    );
    assert_public_error(response, StatusCode::UNAUTHORIZED, "unauthorized");
}

#[tokio::test]
async fn authorizer_exception_returns_internal_error() {
    let project = TestProject::new("authorizer-exception");
    project.add_action(
        "auth.py",
        r#"
@authorizer(name="petstore")
def auth(event):
    raise TypeError("bad auth")
"#,
    );
    project.add_action(
        "pets.py",
        r#"
@api_action(method="GET", path="/pets", authorizer="petstore")
def pets():
    return {"ok": True}
"#,
    );
    project.write_manifest(&[
        authorizer_action("src/auth.py", "auth", "petstore"),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets",
                    "query_params": [],
                    "authorizer": "petstore"
                }
            },
            "source": "src/pets.py",
            "entrypoint": "pets"
        }),
    ]);

    let response = request(&project, Method::GET, "/pets", None).await;

    assert_public_error(
        response,
        StatusCode::INTERNAL_SERVER_ERROR,
        "authorizer_failed",
    );
}

#[tokio::test]
async fn authorizer_allow_with_non_object_context_returns_internal_error() {
    let project = TestProject::new("authorizer-bad-context");
    project.add_action(
        "auth.py",
        r#"
@authorizer(name="petstore")
def auth(event):
    return {"effect": "allow", "context": "bad"}
"#,
    );
    project.add_action(
        "pets.py",
        r#"
@api_action(method="GET", path="/pets", authorizer="petstore")
def pets():
    return {"ok": True}
"#,
    );
    project.write_manifest(&[
        authorizer_action("src/auth.py", "auth", "petstore"),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets",
                    "query_params": [],
                    "authorizer": "petstore"
                }
            },
            "source": "src/pets.py",
            "entrypoint": "pets"
        }),
    ]);

    let response = request(&project, Method::GET, "/pets", None).await;

    assert_public_error(
        response,
        StatusCode::INTERNAL_SERVER_ERROR,
        "authorizer_failed",
    );
}

#[tokio::test]
async fn authorizer_timeout_returns_gateway_timeout() {
    let project = TestProject::new("authorizer-timeout");
    project.add_action(
        "auth.py",
        r#"
import time

@authorizer(name="petstore")
def auth(event):
    time.sleep(1)
    return {"effect": "allow"}
"#,
    );
    project.add_action(
        "pets.py",
        r#"
@api_action(method="GET", path="/pets", authorizer="petstore")
def pets():
    return {"ok": True}
"#,
    );
    project.write_manifest(&[
        json!({
            "runtime": "Python",
            "kind": {
                "Authorizer": {}
            },
            "source": "src/auth.py",
            "entrypoint": "auth",
            "name": "petstore",
            "policy": {
                "timeout": "10ms"
            }
        }),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets",
                    "query_params": [],
                    "authorizer": "petstore"
                }
            },
            "source": "src/pets.py",
            "entrypoint": "pets"
        }),
    ]);

    let response = request(&project, Method::GET, "/pets", None).await;

    assert_public_error(response, StatusCode::GATEWAY_TIMEOUT, "authorizer_failed");
}

#[tokio::test]
async fn invokes_node_authorizer_with_request_binding() {
    let project = TestProject::new("node-authorizer");
    project.add_node_action(
        "auth.js",
        r#"
export default authorizer({
  name: "store",
  handler({ headers, method, path, path_params, query_params }) {
    return {
      effect: "allow",
      principal_id: [
        method,
        path,
        path_params.pet_id,
        query_params.debug,
        headers.authorization,
      ].join("|"),
      context: { runtime: "node" },
    };
  },
});
"#,
    );
    project.add_node_action(
        "pets.js",
        r#"
export default apiAction({
  method: "GET",
  path: "/pets/{pet_id}",
  authorizer: "store",
  handler({ context }) {
    return context.metadata.authorizer;
  },
});
"#,
    );
    project.write_manifest(&[
        json!({
            "runtime": "Node",
            "kind": {
                "Authorizer": {}
            },
            "source": "src/auth.js",
            "entrypoint": "default",
            "name": "store"
        }),
        json!({
            "runtime": "Node",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets/{pet_id}",
                    "query_params": [],
                    "authorizer": "store"
                }
            },
            "source": "src/pets.js",
            "entrypoint": "default"
        }),
    ]);

    let response = raw_request_with_headers(
        &project,
        Method::GET,
        "/pets/p1?debug=123",
        "",
        &[("authorization", "Bearer dev")],
    )
    .await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(
        response.body["principal_id"],
        json!("GET|/pets/p1|p1|123|Bearer dev")
    );
    assert_eq!(response.body["context"]["runtime"], json!("node"));
}

#[tokio::test]
async fn binds_path_params_query_params_and_json_body() {
    let project = TestProject::new("binding");
    project.add_action(
        "path.py",
        r#"
@api_action(method="GET", path="/hello/{name}")
def hello_name(name: str):
    return {"message": f"Hello {name}"}
"#,
    );
    project.add_action(
        "query.py",
        r#"
@api_action(method="GET", path="/search")
def search(limit: int, q: str | None = None):
    return {"limit": limit, "q": q}
"#,
    );
    project.add_action(
        "post.py",
        r#"
@api_action(method="POST", path="/users")
def create_user(name: str):
    return {"created": name}
"#,
    );
    project.write_manifest(&[
        action("GET", "/hello/{name}", "src/path.py", "hello_name"),
        action("GET", "/search", "src/query.py", "search"),
        action("POST", "/users", "src/post.py", "create_user"),
    ]);

    let path = request(&project, Method::GET, "/hello/Maikel", None).await;
    assert_eq!(path.status, StatusCode::OK);
    assert_eq!(path.body, json!({"message": "Hello Maikel"}));

    let query = request(&project, Method::GET, "/search?limit=5&q=ryvus", None).await;
    assert_eq!(query.status, StatusCode::OK);
    assert_eq!(query.body, json!({"limit": 5, "q": "ryvus"}));

    let post = request(
        &project,
        Method::POST,
        "/users",
        Some(json!({"name": "Ada"})),
    )
    .await;
    assert_eq!(post.status, StatusCode::OK);
    assert_eq!(post.body, json!({"created": "Ada"}));
}

#[tokio::test]
async fn supports_text_and_form_api_action_media_types() {
    let project = TestProject::new("content-types");
    project.add_action(
        "echo.py",
        r#"
@api_action(method="POST", path="/echo", consumes="text/plain", produces="text/plain")
def echo(body: str):
    return body
"#,
    );
    project.add_action(
        "form.py",
        r#"
@api_action(method="POST", path="/form")
def form(name: str, quantity: int):
    return {"name": name, "quantity": quantity}
"#,
    );
    project.write_manifest(&[
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "POST",
                    "path": "/echo",
                    "consumes": ["text/plain"],
                    "produces": ["text/plain"],
                    "query_params": []
                }
            },
            "source": "src/echo.py",
            "entrypoint": "echo"
        }),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "POST",
                    "path": "/form",
                    "consumes": ["application/x-www-form-urlencoded"],
                    "produces": ["application/json"],
                    "query_params": [],
                    "request_schema": {
                        "type": "object",
                        "required": ["name", "quantity"],
                        "properties": {
                            "name": { "type": "string" },
                            "quantity": { "type": "string" }
                        }
                    }
                }
            },
            "source": "src/form.py",
            "entrypoint": "form"
        }),
    ]);

    let text = raw_request_with_content_type(
        &project,
        Method::POST,
        "/echo",
        "hello ryvus",
        "text/plain; charset=utf-8",
    )
    .await;
    assert_eq!(text.status, StatusCode::OK);
    assert_eq!(text.raw_body, "hello ryvus");

    assert_public_error(
        raw_request_with_content_type(&project, Method::POST, "/echo", "", "text/plain").await,
        StatusCode::BAD_REQUEST,
        "invalid_request_body",
    );

    let form = raw_request_with_content_type(
        &project,
        Method::POST,
        "/form",
        "name=food_salmon_2kg&quantity=2",
        "application/x-www-form-urlencoded",
    )
    .await;
    assert_eq!(form.status, StatusCode::OK);
    assert_eq!(form.body, json!({"name": "food_salmon_2kg", "quantity": 2}));

    assert_public_error(
        raw_request_with_content_type(&project, Method::POST, "/echo", "{}", "application/json")
            .await,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
    );
}

#[tokio::test]
async fn returns_expected_http_errors() {
    let project = TestProject::new("errors");
    project.add_action(
        "hello.py",
        r#"
@api_action(method="GET", path="/hello")
def hello():
    return {"ok": True}
"#,
    );
    project.add_action(
        "post.py",
        r#"
@api_action(method="POST", path="/post")
def post(name: str):
    return {"name": name}
"#,
    );
    project.add_action(
        "fails.py",
        r#"
@api_action(method="GET", path="/fails")
def fails():
    raise RuntimeError("boom")
"#,
    );
    project.add_action(
        "needs.py",
        r#"
@api_action(method="GET", path="/needs")
def needs(required: str):
    return {"required": required}
"#,
    );
    project.write_manifest(&[
        action("GET", "/hello", "src/hello.py", "hello"),
        action("POST", "/post", "src/post.py", "post"),
        action("GET", "/fails", "src/fails.py", "fails"),
        action("GET", "/needs", "src/needs.py", "needs"),
    ]);

    assert_public_error(
        request(&project, Method::GET, "/missing", None).await,
        StatusCode::NOT_FOUND,
        "route_not_configured",
    );
    assert_public_error(
        request(&project, Method::POST, "/hello", None).await,
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
    );
    assert_public_error(
        raw_request(&project, Method::POST, "/post", "{").await,
        StatusCode::BAD_REQUEST,
        "invalid_json_body",
    );
    assert_invocation_error(
        request(&project, Method::GET, "/needs", None).await,
        StatusCode::BAD_REQUEST,
        "action_failed",
    );
    assert_invocation_error(
        request(&project, Method::GET, "/fails", None).await,
        StatusCode::INTERNAL_SERVER_ERROR,
        "action_failed",
    );
}

#[tokio::test]
async fn validates_query_params_and_request_body_before_invocation() {
    let project = TestProject::new("gateway-validation");
    project.add_action(
        "query.py",
        r#"
@api_action(method="GET", path="/search")
def search(limit: int):
    return {"limit": limit}
"#,
    );
    project.add_action(
        "path.py",
        r#"
@api_action(method="GET", path="/pets/{id}")
def find_pet(id: str):
    return {"id": id}
"#,
    );
    project.add_action(
        "post.py",
        r#"
@api_action(method="POST", path="/pets")
def create_pet(name: str, age: int):
    return {"name": name, "age": age}
"#,
    );
    project.write_manifest(&[
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/search",
                    "query_params": [
                        {
                            "name": "limit",
                            "required": true,
                            "schema": { "type": "integer" }
                        }
                    ]
                }
            },
            "source": "src/query.py",
            "entrypoint": "search"
        }),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "GET",
                    "path": "/pets/{id}",
                    "query_params": []
                }
            },
            "source": "src/path.py",
            "entrypoint": "find_pet"
        }),
        json!({
            "runtime": "Python",
            "kind": {
                "Api": {
                    "method": "POST",
                    "path": "/pets",
                    "query_params": [],
                    "request_schema": {
                        "type": "object",
                        "required": ["name", "age"],
                        "properties": {
                            "name": { "type": "string" },
                            "age": { "type": "integer" }
                        }
                    }
                }
            },
            "source": "src/post.py",
            "entrypoint": "create_pet"
        }),
    ]);

    let missing_query = request(&project, Method::GET, "/search", None).await;
    assert_eq!(missing_query.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        missing_query.body["error"],
        json!("request_validation_failed")
    );
    assert!(missing_query.body.get("execution_id").is_none());
    assert!(missing_query.body.get("attempt_id").is_none());

    let empty_query = request(&project, Method::GET, "/search?limit=", None).await;
    assert_eq!(empty_query.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        empty_query.body["error"],
        json!("request_validation_failed")
    );

    let invalid_query = request(&project, Method::GET, "/search?limit=nope", None).await;
    assert_eq!(invalid_query.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_query.body["error"],
        json!("request_validation_failed")
    );

    let empty_path = request(&project, Method::GET, "/pets/%20", None).await;
    assert_eq!(empty_path.status, StatusCode::BAD_REQUEST);
    assert_eq!(empty_path.body["error"], json!("request_validation_failed"));

    let missing_body_field = request(
        &project,
        Method::POST,
        "/pets",
        Some(json!({ "name": "Nori" })),
    )
    .await;
    assert_eq!(missing_body_field.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        missing_body_field.body["error"],
        json!("request_validation_failed")
    );

    let invalid_body_type = request(
        &project,
        Method::POST,
        "/pets",
        Some(json!({ "name": "Nori", "age": "old" })),
    )
    .await;
    assert_eq!(invalid_body_type.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid_body_type.body["error"],
        json!("request_validation_failed")
    );
}

#[tokio::test]
async fn validates_duplicate_routes_at_startup() {
    let project = TestProject::new("duplicate");
    project.write_manifest(&[
        action("GET", "/users/{id}", "src/a.py", "a"),
        action("GET", "/users/{name}", "src/b.py", "b"),
    ]);

    let error = server::build_app(&project.config()).expect_err("duplicate route should fail");

    assert!(error.to_string().contains("duplicate route"));
}

#[tokio::test]
async fn validates_unsupported_methods_and_invalid_paths_at_startup() {
    let bad_method = TestProject::new("bad-method");
    bad_method.write_manifest(&[action("TRACE", "/hello", "src/a.py", "a")]);

    let error = server::build_app(&bad_method.config()).expect_err("method should fail");
    assert!(error.to_string().contains("unsupported HTTP method"));

    let bad_path = TestProject::new("bad-path");
    bad_path.write_manifest(&[action("GET", "hello", "src/a.py", "a")]);

    let error = server::build_app(&bad_path.config()).expect_err("path should fail");
    assert!(error.to_string().contains("path must start"));
}

#[tokio::test]
async fn validates_runtime_sources_at_startup() {
    let project = TestProject::new("missing-runtime-source");
    project.write_manifest(&[node_action("GET", "/missing", "src/missing.js", "default")]);

    let error = server::build_app(&project.config()).expect_err("missing source should fail");

    assert!(error
        .to_string()
        .contains("runtime source file not found for Node action"));
    assert!(error.to_string().contains("src/missing.js"));
}
