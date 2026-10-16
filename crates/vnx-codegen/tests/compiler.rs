use vnx_codegen::{generate, parse, Compiler};
fn fixture() -> Compiler {
    let mut c = Compiler::new();
    c.add_source(
        "base.vni",
        r#"
package vnx;
abstract module Module;
extern Hash64 = {4};
extern Variant = {17};
enum permission_e { PUBLIC = 0, READ = 1 }
"#,
    )
    .unwrap();
    c.add_source("sample.vni",r#"
package demo;
import vnx.Hash64;
extern digest_t = {11, 32, 1};
enum state_e { EMPTY = 0, READY, LAST = -1 }
struct entry_t { uint height = -1; string name = "entry"; digest_t hash; }
class Base { uint version = 1; }
class Message extends Base { vector<entry_t> entries; optional<state_e> state; alias old_entries entries; }
interface Query { @Permission(vnx.permission_e.READ) Message* get(uint height = 10) const; }
module Node implements Query { uint count = 8; @External uint external; map<pair<Hash64, digest_t>, entry_t> entries; void submit(Message* message) async; void handle(Message message); static void helper(); }
"#).unwrap();
    c
}
#[test]
fn parse_resolve_and_generate() {
    let c = fixture().compile().unwrap();
    let message = c.registry.by_name("demo.Message").unwrap();
    assert_eq!(
        message
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["version", "entries", "state"]
    );
    assert_eq!(message.fields[0].code, [3]);
    assert_eq!(message.fields[1].code, [12, 19, 0]);
    assert!(message.fields[1].is_extended);
    assert_eq!(message.alias_map["old_entries"], "entries");
    let node = c.registry.by_name("demo.Node").unwrap();
    assert!(
        node.fields
            .iter()
            .find(|f| f.name == "external")
            .unwrap()
            .is_extended
    );
    assert_eq!(node.methods.len(), 2);
    assert!(!c
        .registry
        .iter()
        .any(|t| t.name.contains("handle") || t.name.contains("helper")));
    let req = c.registry.by_name("demo.Query.get").unwrap();
    assert!(req.is_method);
    assert!(req.is_const);
    assert_eq!(req.permission, "vnx.permission_e.READ");
    assert_eq!(req.fields[0].value, "10");
    assert!(c.registry.by_name("demo.Node.submit").unwrap().is_async);
    let rust = generate(&c).unwrap();
    assert!(rust.contains("pub struct NodeClient"));
    assert!(rust.contains("pub trait NodeService"));
    assert!(rust.contains("::vnx::Fixed<u8, 32>"));
}
#[test]
fn malformed_syntax_reports_location() {
    let err = parse("bad.vni", "package demo;\nstruct bad { vector<uint x; }")
        .unwrap_err()
        .to_string();
    assert!(err.contains("bad.vni:2"));
    assert!(parse("bad.vni", "package demo; /* open").is_err());
}
#[test]
fn invalid_references_and_cycles_are_rejected() {
    for source in [
        "package x; struct A { Missing value; }",
        "package x; struct A extends B {} struct B extends A {}",
        "package x; class A { vector<uint, uint> x; }",
        "package x; struct A { uint value; uint value; }",
        "package x; typedef B A; typedef A B;",
        "package x; struct A { void value; }",
    ] {
        let mut c = Compiler::new();
        c.add_source("bad.vni", source).unwrap();
        assert!(c.compile().is_err(), "{source}");
    }
}
#[test]
fn hashes_match_cpp_generated_keyfile() {
    let mut c = Compiler::new();
    c.add_source("keyfile.vni",r#"package mmx; extern hash_t = {11, 32, 1}; class KeyFile { hash_t seed_value; optional<string> finger_print; }"#).unwrap();
    let c = c.compile().unwrap();
    let code = c.registry.by_name("mmx.KeyFile").unwrap();
    assert_eq!(code.type_hash, 0xdf868931a939cba1);
    assert_eq!(code.code_hash, 0x35062c98e95706b2);
}
#[test]
fn current_mmx_schema_if_requested() {
    let Ok(root) = std::env::var("MMX_SOURCE_DIR") else {
        return;
    };
    let mut c = Compiler::new();
    for sub in [
        "vnx-base/interface",
        "vnx-base/modules",
        "vnx-addons/interface",
        "vnx-addons/modules",
        "interface",
        "modules",
    ] {
        c.add_path(std::path::Path::new(&root).join(sub)).unwrap();
    }
    let c = c.compile().unwrap();
    assert!(c.registry.len() > 500);
    let rust = generate(&c).unwrap();
    assert!(rust.contains("pub struct WalletClient"));
    compare_cpp_hashes(&root, &c);
    compile_mmx_bindings(&rust);

    if let Ok(dest) = std::env::var("VNX_GENERATED_OUTPUT") {
        std::fs::write(dest, &rust).unwrap();
    }
    for name in [
        "mmx.KeyFile",
        "mmx.Transaction",
        "mmx.Node.get_height",
        "mmx.Wallet.send",
    ] {
        assert!(c.registry.by_name(name).is_ok(), "{name}");
    }
}

#[test]
fn generated_bindings_compile_roundtrip_and_dispatch() {
    let mut c = Compiler::new();
    c.add_source(
        "counter.vni",
        r#"
package vnx;
abstract module Module;
"#,
    )
    .unwrap();
    c.add_source(
        "demo.vni",
        r#"
package demo;
extern digest_t = {11, 64, 1};
struct entry_t { uint height = -1; digest_t digest; }
class Message { entry_t entry; optional<string> label; }
module Counter { uint count = 8; uint sum(uint first, uint second = 1) const async; void reset(); }
"#,
    )
    .unwrap();
    let code = generate(&c.compile().unwrap()).unwrap();
    let root = std::env::temp_dir().join(format!("vnx-generated-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../vnx")
        .canonicalize()
        .unwrap();
    std::fs::write(root.join("Cargo.toml"), format!("[package]\nname = \"vnx-generated-test\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\nvnx = {{ path = {:?} }}\n", runtime.to_str().unwrap())).unwrap();
    std::fs::write(root.join("src/lib.rs"), format!("{code}\n{}", r#"
#[cfg(test)] mod tests {
use super::*;
use ::vnx::VnxType;
struct Counter;
impl demo::CounterService for Counter {
    fn sum(&self, first:u32, second:u32, _: &::vnx::runtime::RequestContext)->::vnx::Result<u32> { Ok(first+second) }
    fn reset(&self, _: &::vnx::runtime::RequestContext)->::vnx::Result<()> { Ok(()) }
}
#[test] fn generated_roundtrip_and_rpc() {
    let value=demo::Message::default();
    assert_eq!(value.entry.height, u32::MAX);
    assert_eq!(demo::Counter::default().count,8);
    let bytes=value.encode(&vnx_registry().unwrap()).unwrap();
    assert_eq!(demo::Message::decode(&bytes).unwrap(),value);
    let runtime=::vnx::runtime::Runtime::new().unwrap();
    runtime.register("Counter",demo::CounterDispatcher(Counter)).unwrap();
    let client=demo::CounterClient::new(runtime.client("Counter"));
    assert_eq!(client.sum(12,30).unwrap(),42);
    assert_eq!(client.sum_async(12,30).unwrap().wait().unwrap()["_ret_0"],42);
    client.reset().unwrap();
    let mut server=::vnx::net::Server::bind("tcp://127.0.0.1:0".parse().unwrap(),runtime.clone(),vnx_registry().unwrap(),::vnx::net::ServerOptions {services: ["Counter".to_owned()].into_iter().collect(),session: ::vnx::runtime::Session::internal(), ..Default::default()}).unwrap();
    let network=::vnx::net::NetworkClient::connect(server.endpoint(),"Counter",vnx_registry().unwrap(),std::time::Duration::from_secs(2)).unwrap();
    let client=demo::CounterClient::new(network.clone());
    assert_eq!(client.sum(10,32).unwrap(),42);
    assert_eq!(client.sum_async(10,32).unwrap().wait().unwrap()["_ret_0"],42);
    client.reset().unwrap();
    network.close();
    server.shutdown();
    runtime.shutdown();
}
}
"#)).unwrap();
    let output =
        std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args(["test", "--offline", "--quiet", "--manifest-path"])
            .arg(root.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", root.join("target"))
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "generated binding test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(root).unwrap();
}

fn compare_cpp_hashes(root: &str, compiled: &vnx_codegen::Compiled) {
    let mut checked = 0;
    for dir in [
        "generated/src",
        "generated/contract/src",
        "generated/operation/src",
        "generated/solution/src",
        "generated/vm/src",
        "vnx-base/generated/src",
        "vnx-addons/generated/src",
    ] {
        let Ok(files) = std::fs::read_dir(std::path::Path::new(root).join(dir)) else {
            continue;
        };
        for file in files {
            let path = file.unwrap().path();
            if path.extension().is_none_or(|s| s != "cpp") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let mut lines = source.lines();
            while let Some(line) = lines.next() {
                let Some(name) = line
                    .trim()
                    .strip_prefix("type_code->name = \"")
                    .and_then(|s| s.strip_suffix("\";"))
                else {
                    continue;
                };
                let type_line = lines.next().unwrap();
                let code_line = lines.next().unwrap();
                fn hash(line: &str) -> u64 {
                    u64::from_str_radix(
                        line.split("0x")
                            .nth(1)
                            .unwrap()
                            .split("ull")
                            .next()
                            .unwrap(),
                        16,
                    )
                    .unwrap()
                }
                let Ok(code) = compiled.registry.by_name(name) else {
                    continue;
                };
                assert_eq!(
                    code.type_hash,
                    hash(type_line),
                    "{}: {name} type hash",
                    path.display()
                );
                assert_eq!(
                    code.code_hash,
                    hash(code_line),
                    "{}: {name} code hash",
                    path.display()
                );
                checked += 1;
            }
        }
    }
    eprintln!("Compared {checked} generated C++ type/code hash pairs");
}
fn compile_mmx_bindings(rust: &str) {
    let root = std::env::temp_dir().join(format!("vnx-generated-mmx-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../vnx")
        .canonicalize()
        .unwrap();
    let fixtures = runtime.join("tests/fixtures");
    let body = format!(
        "{rust}\nconst CPP_FIXTURE_DIR: &str = {:?};\n{}",
        fixtures.to_str().unwrap(),
        include_str!("mmx_runtime_tests.rs.txt")
    );
    std::fs::write(root.join("src/lib.rs"), body).unwrap();
    std::fs::write(root.join("Cargo.toml"),format!("[package]\nname = \"vnx-generated-mmx-test\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\nvnx = {{ path = {:?} }}\n",runtime.to_str().unwrap())).unwrap();
    let output =
        std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args(["test", "--offline", "--quiet", "--manifest-path"])
            .arg(root.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", root.join("target"))
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "generated MMX bindings failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(root).unwrap();
}
