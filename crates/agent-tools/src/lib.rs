//! Transport-independent agent tools. Live calls are validated here and applied by the host.
pub mod analytics;
pub mod args;
pub use host::edits;
mod data;
mod host;
mod registry;
mod result;

pub use host::{DataHost, History, HostFuture, LiveHost, MarketDataHost, PriceBasis};
pub use registry::{AnalysisCache, CallBudget, CallContext, Registry, ToolKind, ToolSet, ToolSpec};
pub use result::{Content, MAX_TEXT_BYTES, ToolResult, scrub_text};
