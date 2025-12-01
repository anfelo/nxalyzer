mod args;

use std::path::Path;

use anyhow::{Context, Result};
use args::{Commands, NxalyzerArgs};
use clap::Parser;

fn canonicalize_path(path_str: &str) -> Result<std::path::PathBuf> {
    let path = Path::new(path_str);
    path.canonicalize()
        .with_context(|| format!("Unable to resolve path: {}", path_str))
}

fn main() -> Result<()> {
    let cli = NxalyzerArgs::parse();

    match &cli.command {
        Commands::QueryAll(args) => {
            let path = canonicalize_path(&args.path)?;

            nxalyzer::query_all(&path)
                .with_context(|| format!("Unable to query in path: {}", path.display()))?
        }
        Commands::Query(args) => {
            let path = canonicalize_path(&args.path)?;

            nxalyzer::query(&path, &args.query)
                .with_context(|| format!("Unable to query in path: {}", path.display()))?
        }
        Commands::Unused(args) => {
            let path = canonicalize_path(&args.path)?;

            nxalyzer::unused(&path)
                .with_context(|| format!("Unable to find unused entities in path: {}", path.display()))?
        }
    }

    Ok(())
}
