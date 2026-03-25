use oxigraph::store::Store;

use crate::config::{StoreConfig, StoreMode};
use crate::error::StoreResult;
use crate::on_disk::open_on_disk_store;

pub fn initialize_store(config: &StoreConfig) -> StoreResult<Store> {
    match config.mode {
        StoreMode::InMemory => Ok(Store::new()?),
        StoreMode::OnDisk => open_on_disk_store(&config.data_dir),
    }
}
