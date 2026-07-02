use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocsRegistry {
    pub nav: Vec<DocNavItem>,
    pub pages: Vec<DocPage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocPage {
    pub id: String,
    pub title: String,
    pub path: String,
    pub source: DocSource,
    pub content_type: DocContentType,
    pub content: DocContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocNavItem {
    pub id: String,
    pub title: String,
    pub path: Option<String>,
    pub children: Vec<DocNavItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocSource {
    Core,
    Sdk,
    Generated,
    Project,
    Plugin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocContentType {
    Markdown,
    OpenApiJson,
    Json,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DocContent {
    Text(String),
    Json(serde_json::Value),
}
