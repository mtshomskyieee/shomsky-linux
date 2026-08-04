//! In-container MCP client: stdio child processes only (no network transports).

mod framing;
mod registry;

pub use registry::McpRegistry;
