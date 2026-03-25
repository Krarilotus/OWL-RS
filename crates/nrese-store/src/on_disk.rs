use std::path::Path;

use oxigraph::store::Store;

use crate::error::{StoreError, StoreResult};

#[cfg(feature = "durable-storage")]
pub fn open_on_disk_store(data_dir: &Path) -> StoreResult<Store> {
    std::fs::create_dir_all(data_dir)?;
    Store::open(data_dir).map_err(StoreError::from)
}

#[cfg(not(feature = "durable-storage"))]
pub fn open_on_disk_store(_data_dir: &Path) -> StoreResult<Store> {
    Err(StoreError::DurableStorageFeatureDisabled)
}
