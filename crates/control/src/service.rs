use std::path::PathBuf;

use ryvus_action_catalog::{ActionCatalog, FileActionCatalog};
use ryvus_docs::{DocsRegistry, DocsRegistryBuilder, GeneratedCatalogDocsSource};
use ryvus_protocol::ActionDefinition;
use ryvus_scheduler::ScheduleInfo;

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
        let docs_registry = DocsRegistryBuilder::new()
            .add_provider(GeneratedCatalogDocsSource::new(action_catalog.all()))
            .build()?;

        Ok(Self {
            action_catalog,
            route_registry,
            docs_registry,
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

    pub fn resolve_action(&self, action: &str) -> ControlResult<&ActionDefinition> {
        Ok(self.action_catalog.resolve(action)?)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use ryvus_protocol::{ActionDefinition, ActionKind, ActionManifest, ApiAction, RuntimeKind};

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
                    request_schema: None,
                    response_schema: None,
                    query_params: Vec::new(),
                }),
                source: PathBuf::from("src/hello.py"),
                entrypoint: "hello".to_string(),
                name: None,
            }],
        };

        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest).expect("manifest should serialize"),
        )
        .expect("manifest should be written");

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
    }

    fn temp_project_root() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ryvus-control-{id}"))
    }
}
