//! Bundled stdio MCP adapter. Stdout is reserved exclusively for MCP frames.

use chronicle_mcp::{ServerConfig, run_stdio};

#[tokio::main]
async fn main() {
    let result = ServerConfig::parse_args(std::env::args_os().skip(1));
    let result = match result {
        Ok(config) => run_stdio(config).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        eprintln!("chronicle-mcp: {}", error.code());
        std::process::exit(1);
    }
}
