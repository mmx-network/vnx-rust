//! Native Rust VNX runtime and wire format for MMX.
#![forbid(unsafe_code)]
pub mod binary;
pub mod code;
pub mod error;
pub mod schema;
pub use error::{Error, Result};
pub use schema::{Field, Registry, TypeCode};
pub use serde;
pub use serde_json;
pub use serde_json::Value as Variant;
pub type Object = serde_json::Map<String, Variant>;
pub mod config;
pub mod hash;
pub mod logging;
pub mod util;
pub mod value;
pub use hash::{hash64, Crc64, Hash64};
pub use value::{Fixed, VnxType};
pub mod auth;
pub mod collections;
pub mod net;
pub mod process;
pub mod runtime;
pub mod terminal;
pub mod time;
pub use collections::Map;
pub mod rpc;
pub use rpc::RpcClient;
