//! Native VNI parser, schema compiler and Rust bindings generator.
//! Hashes and wire layouts follow vnx-base's VNI type system.
//!
//! ```
//! use vnx_codegen::{Compiler, generate};
//! let mut compiler = Compiler::new();
//! compiler.add_source("Greeting.vni", "package demo; class Greeting { string text; }")?;
//! let compiled = compiler.compile()?;
//! let rust_bindings = generate(&compiled)?;
//! assert!(rust_bindings.contains("pub struct Greeting"));
//! # Ok::<(), vnx::Error>(())
//! ```
//!
//! To use the generator from `build.rs`, add `vnx-codegen` as a build dependency
//! and `vnx` as a normal dependency, then compile all dependency schemas together:
//!
//! ```no_run
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let inputs = ["../vnx-base/interface", "../vnx-base/modules", "interface", "modules"];
//!     for input in inputs { println!("cargo:rerun-if-changed={input}"); }
//!     let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
//!     vnx_codegen::generate_to(inputs, output.join("vnx_generated.rs"))?;
//!     Ok(())
//! }
//! ```
//!
//! At your library's crate root, include the generated module tree:
//! `include!(concat!(env!("OUT_DIR"), "/vnx_generated.rs"));`.
//!
//! Generated clients accept either a local `vnx::runtime::Client` or a remote
//! `vnx::net::NetworkClient`; `NodeClient::new(transport)` infers its transport
//! through the shared `vnx::rpc::RpcClient` trait.
pub mod ast;
mod compiler;
mod generate;
pub mod parser;
pub use compiler::{Compiled, Compiler};
pub use generate::generate;
pub use parser::parse;

/// Compile VNI files or directories and write one Rust module tree.
///
/// Dependency interfaces and modules must be included in `inputs`. The returned
/// compilation also provides the registry for schema JSON or other tooling.
pub fn generate_to<I, P, O>(inputs: I, output: O) -> vnx::Result<Compiled>
where
    I: IntoIterator<Item = P>,
    P: AsRef<std::path::Path>,
    O: AsRef<std::path::Path>,
{
    let mut compiler = Compiler::new();
    for path in inputs {
        compiler.add_path(path)?;
    }
    let compiled = compiler.compile()?;
    let rust = generate(&compiled)?;
    let output = output.as_ref();
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, rust)?;
    Ok(compiled)
}
