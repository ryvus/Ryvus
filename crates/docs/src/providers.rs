use ryvus_protocol::ActionDefinition;

use crate::{
    openapi::build_public_openapi_json_from_actions, DocContent, DocContentType, DocPage,
    DocSource, DocsResult,
};

pub trait DocsSourceProvider {
    fn pages(&self) -> DocsResult<Vec<DocPage>>;
}

#[derive(Debug, Clone)]
pub struct GeneratedCatalogDocsSource {
    actions: Vec<ActionDefinition>,
}

impl GeneratedCatalogDocsSource {
    pub fn new<'a>(actions: impl IntoIterator<Item = &'a ActionDefinition>) -> Self {
        Self {
            actions: actions.into_iter().cloned().collect(),
        }
    }
}

impl DocsSourceProvider for GeneratedCatalogDocsSource {
    fn pages(&self) -> DocsResult<Vec<DocPage>> {
        Ok(vec![DocPage {
            id: "public-openapi".to_string(),
            title: "Public OpenAPI".to_string(),
            path: "/openapi.json".to_string(),
            source: DocSource::Generated,
            content_type: DocContentType::OpenApiJson,
            content: DocContent::Json(build_public_openapi_json_from_actions(&self.actions)),
        }])
    }
}

#[derive(Debug, Default, Clone)]
pub struct CoreDocsSource;

impl DocsSourceProvider for CoreDocsSource {
    fn pages(&self) -> DocsResult<Vec<DocPage>> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Default, Clone)]
pub struct SdkDocsSource;

impl DocsSourceProvider for SdkDocsSource {
    fn pages(&self) -> DocsResult<Vec<DocPage>> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Default, Clone)]
pub struct ProjectDocsSource;

impl DocsSourceProvider for ProjectDocsSource {
    fn pages(&self) -> DocsResult<Vec<DocPage>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ryvus_protocol::{ActionKind, ApiAction, RuntimeKind};
    use serde_json::json;

    use super::*;

    #[test]
    fn generated_catalog_provider_emits_openapi_page() {
        let actions = vec![ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: "GET".to_string(),
                path: "/hello".to_string(),
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
            }),
            source: PathBuf::from("src/hello.py"),
            entrypoint: "hello".to_string(),
            name: None,
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        }];

        let pages = GeneratedCatalogDocsSource::new(&actions)
            .pages()
            .expect("provider should build pages");

        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].path, "/openapi.json");
        assert_eq!(pages[0].content_type, DocContentType::OpenApiJson);

        let DocContent::Json(openapi) = &pages[0].content else {
            panic!("OpenAPI page should be JSON");
        };

        assert_eq!(
            openapi["paths"]["/hello"]["get"]["operationId"],
            json!("hello_get_hello")
        );
    }
}
