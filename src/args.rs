use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct NxalyzerArgs {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Queries different types of entities in a nx project
    QueryAll(QueryAllArgs),
    /// Queries different types of entities in a nx project
    Query(QueryArgs),
    /// Lists all unused entities in the nx project
    Unused(UnusedArgs),
}

#[derive(Args, Debug)]
pub struct QueryArgs {
    /// Path to the root of the nx project
    pub path: String,
    /// Query string to filter entities by
    pub query: String,
}

#[derive(Args, Debug)]
pub struct QueryAllArgs {
    /// Path to the root of the nx project
    pub path: String,
}

#[derive(Args, Debug)]
pub struct UnusedArgs {
    /// Path to the root of the nx project
    pub path: String,
}
