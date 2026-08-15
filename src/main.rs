//! ldtk-mcp: a Model Context Protocol server for editing LDtk projects.

mod diff;
mod fields;
mod project;
mod render;
mod schema;
mod tools;

use rmcp::{transport::stdio, ServiceExt};
use tools::LdtkServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Logs go to stderr so they never corrupt the stdio JSON-RPC stream on stdout.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "ldtk_mcp=info".into()),
        )
        .init();

    let service = LdtkServer::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
