//! Shared command-frequency store and stdio MCP server for Shomsky Linux.

mod store;
pub mod mcp_stdio;

pub use store::{default_store_path, format_top, FrequencyStore, MAX_ENTRIES, MAX_TOP};

pub const DEFAULT_TOP_LIMIT: usize = 10;
