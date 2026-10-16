use std::path::PathBuf;
use vnx_codegen::{generate, Compiler};
fn run() -> vnx::Result<()> {
    let mut compiler = Compiler::new();
    let mut output = None;
    let mut schema = None;
    let mut inputs = 0;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("Usage: vnxrustcodegen [--output FILE] [--schema FILE] INPUT...\n\nCompile VNI files/directories into native Rust bindings and VNX schema JSON.\nDirectories are searched recursively. Pass dependency interfaces before the MMX interfaces.\nWithout --output, Rust bindings are written to stdout.");
                return Ok(());
            }
            "-o" | "--output" => {
                output =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        vnx::error::invalid("--output requires a file")
                    })?))
            }
            "--schema" => {
                schema =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        vnx::error::invalid("--schema requires a file")
                    })?))
            }
            "-I" | "--include" => {
                let path = args
                    .next()
                    .ok_or_else(|| vnx::error::invalid("--include requires a directory"))?;
                compiler.add_path(path)?;
                inputs += 1;
            }
            _ => {
                if arg.starts_with('-') {
                    return Err(vnx::error::invalid(format!("unknown argument {arg}")));
                }
                compiler.add_path(arg)?;
                inputs += 1;
            }
        }
    }
    if inputs == 0 {
        return Err(vnx::error::invalid("no VNI input files; use --help"));
    }
    let compiled = compiler.compile()?;
    let rust = generate(&compiled)?;
    if let Some(path) = output {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, rust)?;
    } else {
        print!("{rust}");
    }
    if let Some(path) = schema {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, compiled.registry.to_json()?)?;
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("vnxrustcodegen: {error}");
        std::process::exit(1);
    }
}
