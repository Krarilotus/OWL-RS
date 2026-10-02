//! The repository catalogue is the store's ([`nrese_store::catalog`]); the server maps its
//! errors to HTTP and reads RDF4J's and GraphDB's repository configurations into its
//! settings ([`crate::repository_config`]).

pub use nrese_store::catalog::{
    Catalog, CatalogError, DEFAULT_REPOSITORY, PipelineSlot, Repository, check,
    default_settings_file, reconfigure, stored_default_settings, write_settings,
};

use crate::error::ApiError;

impl From<CatalogError> for ApiError {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::Invalid(message) => ApiError::bad_request(message),
            CatalogError::NotFound(message) => ApiError::not_found(message),
            CatalogError::Conflict(message) => ApiError::conflict(message),
            CatalogError::Store(message) => ApiError::internal(message),
        }
    }
}
