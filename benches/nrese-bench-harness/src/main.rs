mod cli;
mod compat_common;
mod compat_graph_store;
mod compat_query;
mod compat_update;
mod interpolation;
mod io;
mod layout;
mod model;
mod normalize;
mod payloads;
mod pack_validation;
mod runner;

use std::env;

use anyhow::Result;

use crate::cli::parse_cli;
use crate::model::Command;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = parse_cli(env::args().collect())?;

    match cli.command {
        Command::Bench(config) => {
            runner::run_bench(config).await?;
            Ok(())
        }
        Command::CatalogSync(config) => runner::run_catalog_sync(config).await,
        Command::Compat(config) => {
            runner::run_compat(config).await?;
            Ok(())
        }
        Command::Pack(config) => runner::run_pack(config).await,
        Command::PackMatrix(config) => runner::run_pack_matrix(config).await,
        Command::Seed(config) => runner::run_seed(config).await,
    }
}
