//! Optional C++ interoperability harness; see tests/cpp/check.py.
use vnx::{
    binary::{self, ByteOrder, Writer},
    error::invalid,
    Result,
};
fn main() -> Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .ok_or_else(|| invalid("expected fixture directory"))?;
    let root = std::path::PathBuf::from(root);
    for name in ["key", "log", "params", "block", "request", "object"] {
        let original = std::fs::read(root.join(format!("{name}.dat")))?;
        let (value, registry) = binary::decode(&original)?;
        let bytes = binary::encode(&value, &registry)?;
        if original != bytes {
            return Err(invalid(format!("C++ byte mismatch for {name}")));
        }
        std::fs::write(root.join(format!("{name}.rust.dat")), bytes)?;
        if name == "object" {
            let mut writer = Writer::new(Vec::new(), registry).with_byte_order(ByteOrder::Big);
            writer.write(&value)?;
            std::fs::write(
                root.join(format!("{name}.rust-big.dat")),
                writer.into_inner(),
            )?;
        }
    }
    println!("Six C++ fixtures reproduced byte for byte; a big-endian dynamic object emitted.");
    Ok(())
}
