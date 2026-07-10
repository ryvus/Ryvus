use std::{collections::HashSet, fs, io::ErrorKind, path::PathBuf};

use ryvus_action_catalog::{ActionCatalog, FileActionCatalog};
use ryvus_docs::{DocsRegistry, DocsRegistryBuilder, GeneratedCatalogDocsSource};
use ryvus_flow::FlowSpec;
use ryvus_protocol::{ActionDefinition, ActionKind};
use ryvus_scheduler::ScheduleInfo;
use serde_json::Value;

use crate::{ControlResult, RouteRegistry};

#[derive(Debug, Clone)]
pub struct LocalControlConfig {
    pub project_root: PathBuf,
    pub manifest_path: PathBuf,
}

pub struct ControlService {
    action_catalog: FileActionCatalog,
    route_registry: RouteRegistry,
    docs_registry: DocsRegistry,
    flow_spec: FlowSpec,
}

impl ControlService {
    pub fn load_local(config: LocalControlConfig) -> ControlResult<Self> {
        let manifest_path = if config.manifest_path.is_absolute() {
            config.manifest_path
        } else {
            config.project_root.join(config.manifest_path)
        };

        let action_catalog = FileActionCatalog::load(manifest_path)?;
        let route_registry = RouteRegistry::from_actions(action_catalog.all())?;
        ryvus_scheduler::validate_schedule_actions(action_catalog.all())?;
        validate_authorizers(action_catalog.all())?;
        let flow_spec = match fs::read_to_string(config.project_root.join(".ryvus/flows.json")) {
            Ok(content) => serde_json::from_str::<FlowSpec>(&content)?,
            Err(error) if error.kind() == ErrorKind::NotFound => FlowSpec::default(),
            Err(error) => return Err(error.into()),
        };
        ryvus_flow::validate_flow_spec(&flow_spec)?;
        ryvus_flow::validate_flow_actions(&flow_spec, action_catalog.all())?;
        let docs_registry = DocsRegistryBuilder::new()
            .add_provider(GeneratedCatalogDocsSource::new(action_catalog.all()))
            .build()?;

        Ok(Self {
            action_catalog,
            route_registry,
            docs_registry,
            flow_spec,
        })
    }

    pub fn action_catalog(&self) -> &FileActionCatalog {
        &self.action_catalog
    }

    pub fn route_registry(&self) -> &RouteRegistry {
        &self.route_registry
    }

    pub fn docs_registry(&self) -> &DocsRegistry {
        &self.docs_registry
    }

    pub fn schedule_infos(&self) -> ControlResult<Vec<ScheduleInfo>> {
        Ok(ryvus_scheduler::schedule_infos(self.action_catalog.all())?)
    }

    pub fn flow_spec(&self) -> ControlResult<Value> {
        Ok(serde_json::to_value(&self.flow_spec)?)
    }

    pub fn typed_flow_spec(&self) -> ControlResult<FlowSpec> {
        Ok(self.flow_spec.clone())
    }

    pub fn resolve_action(&self, action: &str) -> ControlResult<&ActionDefinition> {
        Ok(self.action_catalog.resolve(action)?)
    }

    pub fn resolve_authorizer(&self, name: &str) -> ControlResult<&ActionDefinition> {
        Ok(self.action_catalog.resolve_authorizer(name)?)
    }
}

fn validate_authorizers<'a>(
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
) -> ControlResult<()> {
    let actions: Vec<&ActionDefinition> = actions.into_iter().collect();
    let mut authorizers = HashSet::new();

    for action in &actions {
        if matches!(action.kind, ActionKind::Authorizer(_)) {
            let Some(name) = action.name.as_deref() else {
                return Err(crate::ControlError::InvalidConfig(
                    "authorizer actions require a name".to_string(),
                ));
            };
            if !authorizers.insert(name.to_string()) {
                return Err(crate::ControlError::InvalidConfig(format!(
                    "duplicate authorizer `{name}`"
                )));
            }
        }
    }

    for action in &actions {
        if let ActionKind::Api(api) = &action.kind {
            if let Some(name) = &api.authorizer {
                if !authorizers.contains(name) {
                    return Err(crate::ControlError::InvalidConfig(format!(
                        "api action references unknown authorizer `{name}`"
                    )));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use ryvus_protocol::{
        ActionDefinition, ActionKind, ActionManifest, ApiAction, AuthorizerAction, RuntimeKind,
    };

    use super::*;

    #[test]
    fn local_control_loads_catalog_routes_and_docs() {
        let project_root = temp_project_root();
        let manifest_path = project_root.join(".ryvus/action-manifest.json");
        fs::create_dir_all(manifest_path.parent().expect("manifest should have parent"))
            .expect("manifest parent should be created");

        let manifest = ActionManifest {
            actions: vec![ActionDefinition {
                runtime: RuntimeKind::Python,
                kind: ActionKind::Api(ApiAction {
                    method: "GET".to_string(),
                    path: "/hello/{name}".to_string(),
                    consumes: vec!["application/json".to_string()],
                    produces: vec!["application/json".to_string()],
                    request_schema: None,
                    response_schema: None,
                    query_params: Vec::new(),
                    authorizer: None,
                }),
                source: PathBuf::from("src/hello.py"),
                entrypoint: "hello".to_string(),
                name: None,
                policy: ryvus_protocol::ActionExecutionPolicy::default(),
            }],
        };

        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).expect("manifest should serialize"),
        )
        .expect("manifest should be written");
        fs::write(
            project_root.join(".ryvus/flows.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "flows": [
                    {
                        "key": "hello_flow",
                        "steps": [
                            {
                                "key": "hello",
                                "action": "src/hello.py::hello"
                            }
                        ]
                    }
                ]
            }))
            .expect("flow spec should serialize"),
        )
        .expect("flow spec should be written");

        let control = ControlService::load_local(LocalControlConfig {
            project_root,
            manifest_path: PathBuf::from(".ryvus/action-manifest.json"),
        })
        .expect("control should load");

        assert!(control
            .route_registry()
            .resolve("GET", "/hello/ryvus")
            .is_some());
        assert!(control.docs_registry().json_page("/openapi.json").is_ok());
        assert_eq!(control.schedule_infos().expect("schedules should load"), []);
        assert_eq!(
            control.flow_spec().expect("flows should load"),
            serde_json::json!({
                "flows": [
                    {
                        "key": "hello_flow",
                        "steps": [
                            {
                                "key": "hello",
                                "action": "src/hello.py::hello",
                                "params": null,
                                "config": null,
                                "next_when": []
                            }
                        ]
                    }
                ]
            })
        );
        assert_eq!(
            control.typed_flow_spec().expect("typed flows should load"),
            FlowSpec {
                flows: vec![ryvus_flow::FlowDefinition {
                    key: "hello_flow".to_string(),
                    description: None,
                    version: None,
                    steps: vec![ryvus_flow::FlowStep {
                        key: "hello".to_string(),
                        action: "src/hello.py::hello".to_string(),
                        policy: ryvus_protocol::ActionExecutionPolicy::default(),
                        params: Value::Null,
                        config: Value::Null,
                        next: None,
                        next_when: Vec::new(),
                        otherwise: None,
                        on_error: None,
                        end: None,
                    }],
                }],
            }
        );
    }

    #[test]
    fn local_control_rejects_unknown_flow_actions() {
        let project_root = temp_project_root();
        let manifest_path = project_root.join(".ryvus/action-manifest.json");
        fs::create_dir_all(manifest_path.parent().expect("manifest should have parent"))
            .expect("manifest parent should be created");

        let manifest = ActionManifest {
            actions: vec![ActionDefinition {
                runtime: RuntimeKind::Python,
                kind: ActionKind::Api(ApiAction {
                    method: "GET".to_string(),
                    path: "/hello/{name}".to_string(),
                    consumes: vec!["application/json".to_string()],
                    produces: vec!["application/json".to_string()],
                    request_schema: None,
                    response_schema: None,
                    query_params: Vec::new(),
                    authorizer: None,
                }),
                source: PathBuf::from("src/hello.py"),
                entrypoint: "hello".to_string(),
                name: None,
                policy: ryvus_protocol::ActionExecutionPolicy::default(),
            }],
        };

        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).expect("manifest should serialize"),
        )
        .expect("manifest should be written");
        fs::write(
            project_root.join(".ryvus/flows.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "flows": [
                    {
                        "key": "hello_flow",
                        "steps": [
                            {
                                "key": "hello",
                                "action": "does_not_exist"
                            }
                        ]
                    }
                ]
            }))
            .expect("flow spec should serialize"),
        )
        .expect("flow spec should be written");

        let error = ControlService::load_local(LocalControlConfig {
            project_root,
            manifest_path: PathBuf::from(".ryvus/action-manifest.json"),
        });

        let error = match error {
            Ok(_) => panic!("unknown flow action should fail validation"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            crate::ControlError::Flow(ryvus_flow::FlowError::ActionNotFound { action })
                if action == "does_not_exist"
        ));
    }

    #[test]
    fn local_control_rejects_unknown_api_authorizer() {
        let project_root = temp_project_root();
        let manifest_path = project_root.join(".ryvus/action-manifest.json");
        fs::create_dir_all(manifest_path.parent().expect("manifest should have parent"))
            .expect("manifest parent should be created");

        let manifest = ActionManifest {
            actions: vec![ActionDefinition {
                runtime: RuntimeKind::Python,
                kind: ActionKind::Api(ApiAction {
                    method: "GET".to_string(),
                    path: "/hello".to_string(),
                    consumes: vec!["application/json".to_string()],
                    produces: vec!["application/json".to_string()],
                    request_schema: None,
                    response_schema: None,
                    query_params: Vec::new(),
                    authorizer: Some("missing".to_string()),
                }),
                source: PathBuf::from("src/hello.py"),
                entrypoint: "hello".to_string(),
                name: None,
                policy: ryvus_protocol::ActionExecutionPolicy::default(),
            }],
        };

        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).expect("manifest should serialize"),
        )
        .expect("manifest should be written");

        let error = ControlService::load_local(LocalControlConfig {
            project_root,
            manifest_path: PathBuf::from(".ryvus/action-manifest.json"),
        });

        assert!(matches!(
            error,
            Err(crate::ControlError::InvalidConfig(message))
                if message == "api action references unknown authorizer `missing`"
        ));
    }

    #[test]
    fn local_control_rejects_duplicate_authorizers() {
        let project_root = temp_project_root();
        let manifest_path = project_root.join(".ryvus/action-manifest.json");
        fs::create_dir_all(manifest_path.parent().expect("manifest should have parent"))
            .expect("manifest parent should be created");

        let manifest = ActionManifest {
            actions: vec![
                ActionDefinition {
                    runtime: RuntimeKind::Python,
                    kind: ActionKind::Authorizer(AuthorizerAction {
                        security: Vec::new(),
                        parameters: Vec::new(),
                        cache: None,
                    }),
                    source: PathBuf::from("src/auth.py"),
                    entrypoint: "auth_one".to_string(),
                    name: Some("petstore".to_string()),
                    policy: ryvus_protocol::ActionExecutionPolicy::default(),
                },
                ActionDefinition {
                    runtime: RuntimeKind::Python,
                    kind: ActionKind::Authorizer(AuthorizerAction {
                        security: Vec::new(),
                        parameters: Vec::new(),
                        cache: None,
                    }),
                    source: PathBuf::from("src/auth.py"),
                    entrypoint: "auth_two".to_string(),
                    name: Some("petstore".to_string()),
                    policy: ryvus_protocol::ActionExecutionPolicy::default(),
                },
            ],
        };

        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).expect("manifest should serialize"),
        )
        .expect("manifest should be written");

        let error = ControlService::load_local(LocalControlConfig {
            project_root,
            manifest_path: PathBuf::from(".ryvus/action-manifest.json"),
        });

        assert!(matches!(
            error,
            Err(crate::ControlError::InvalidConfig(message))
                if message == "duplicate authorizer `petstore`"
        ));
    }

    fn temp_project_root() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ryvus-control-{id}"))
    }
}
