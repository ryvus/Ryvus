use std::collections::HashSet;

use crate::{
    DocContent, DocContentType, DocNavItem, DocPage, DocsError, DocsRegistry, DocsResult,
    DocsSourceProvider,
};

pub struct DocsRegistryBuilder {
    providers: Vec<Box<dyn DocsSourceProvider>>,
}

impl DocsRegistryBuilder {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn add_provider<P>(mut self, provider: P) -> Self
    where
        P: DocsSourceProvider + 'static,
    {
        self.providers.push(Box::new(provider));
        self
    }

    pub fn build(self) -> DocsResult<DocsRegistry> {
        let mut paths = HashSet::new();
        let mut pages = Vec::new();

        for provider in self.providers {
            for page in provider.pages()? {
                if !paths.insert(page.path.clone()) {
                    return Err(DocsError::DuplicatePath { path: page.path });
                }
                pages.push(page);
            }
        }

        Ok(DocsRegistry {
            nav: nav_from_pages(&pages),
            pages,
        })
    }
}

impl Default for DocsRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl DocsRegistry {
    pub fn page(&self, path: &str) -> DocsResult<&DocPage> {
        self.pages
            .iter()
            .find(|page| page.path == path)
            .ok_or_else(|| DocsError::PageNotFound {
                path: path.to_string(),
            })
    }

    pub fn json_page(&self, path: &str) -> DocsResult<&serde_json::Value> {
        let page = self.page(path)?;

        match (&page.content_type, &page.content) {
            (DocContentType::OpenApiJson | DocContentType::Json, DocContent::Json(value)) => {
                Ok(value)
            }
            _ => Err(DocsError::InvalidContentType {
                path: path.to_string(),
            }),
        }
    }
}

fn nav_from_pages(pages: &[DocPage]) -> Vec<DocNavItem> {
    pages
        .iter()
        .map(|page| DocNavItem {
            id: page.id.clone(),
            title: page.title.clone(),
            path: Some(page.path.clone()),
            children: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::DocSource;

    struct StaticProvider(Vec<DocPage>);

    impl DocsSourceProvider for StaticProvider {
        fn pages(&self) -> DocsResult<Vec<DocPage>> {
            Ok(self.0.clone())
        }
    }

    fn json_doc(path: &str) -> DocPage {
        DocPage {
            id: path.trim_start_matches('/').to_string(),
            title: path.to_string(),
            path: path.to_string(),
            source: DocSource::Generated,
            content_type: DocContentType::Json,
            content: DocContent::Json(json!({ "ok": true })),
        }
    }

    #[test]
    fn duplicate_page_paths_fail_registry_build() {
        let error = DocsRegistryBuilder::new()
            .add_provider(StaticProvider(vec![json_doc("/openapi.json")]))
            .add_provider(StaticProvider(vec![json_doc("/openapi.json")]))
            .build()
            .expect_err("duplicate path should fail");

        assert!(matches!(
            error,
            DocsError::DuplicatePath { path } if path == "/openapi.json"
        ));
    }

    #[test]
    fn json_page_returns_json_content() {
        let registry = DocsRegistryBuilder::new()
            .add_provider(StaticProvider(vec![json_doc("/openapi.json")]))
            .build()
            .expect("registry should build");

        assert_eq!(
            registry
                .json_page("/openapi.json")
                .expect("page should exist"),
            &json!({ "ok": true })
        );
    }
}
