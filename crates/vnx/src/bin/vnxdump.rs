//! Inspect VNX files using their embedded schemas; no generated types required.
use std::{
    fs::File,
    io::{self, BufReader},
};
use vnx::{binary::Reader, error::invalid, Registry, Result};
fn dump<R: io::Read>(input: R, schemas: bool) -> Result<()> {
    let mut reader = Reader::new(BufReader::new(input), Registry::new());
    while let Some(value) = reader.read_next()? {
        if !schemas {
            println!("{}", vnx::serde_json::to_string_pretty(&value)?);
        }
    }
    if schemas {
        println!("{}", reader.registry().to_json()?);
    }
    Ok(())
}
fn run() -> Result<()> {
    let mut schemas = false;
    let mut file = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--schemas" => schemas = true,
            "-h" | "--help" => {
                println!("Usage: vnxdump [--schemas] FILE|-\nRead VNX values and print JSON, or inspect their embedded type schemas.");
                return Ok(());
            }
            _ => {
                if file.replace(arg).is_some() {
                    return Err(invalid("expected one file"));
                }
            }
        }
    }
    let file = file.ok_or_else(|| invalid("expected a file; use --help"))?;
    if file == "-" {
        dump(io::stdin().lock(), schemas)
    } else {
        dump(File::open(file)?, schemas)
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("vnxdump: {error}");
        std::process::exit(1)
    }
}
