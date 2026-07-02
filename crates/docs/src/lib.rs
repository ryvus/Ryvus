pub mod error;
pub mod model;
pub mod openapi;
pub mod providers;
pub mod registry;

pub use error::{DocsError, DocsResult};
pub use model::{DocContent, DocContentType, DocNavItem, DocPage, DocSource, DocsRegistry};
pub use providers::{
    CoreDocsSource, DocsSourceProvider, GeneratedCatalogDocsSource, ProjectDocsSource,
    SdkDocsSource,
};
pub use registry::DocsRegistryBuilder;
