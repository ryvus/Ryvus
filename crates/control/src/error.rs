use thiserror::Error;

pub type ControlResult<T> = Result<T, ControlError>;

#[derive(Debug, Error)]
pub enum ControlError {
    #[error(transparent)]
    Catalog(#[from] ryvus_action_catalog::ActionCatalogError),

    #[error(transparent)]
    Routes(#[from] crate::routes::RouteRegistryError),

    #[error(transparent)]
    Docs(#[from] ryvus_docs::DocsError),

    #[error(transparent)]
    Schedules(#[from] ryvus_scheduler::SchedulerError),
}
