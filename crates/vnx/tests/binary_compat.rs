use serde_json::json;
use std::io::Cursor;
use vnx::{
    binary::{self, ByteOrder, Limits, Reader, Writer},
    code::Code,
    Registry,
};
fn bytes(s: &str) -> Vec<u8> {
    s.trim()
        .as_bytes()
        .chunks_exact(2)
        .map(|b| u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap())
        .collect()
}
fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("key", bytes(include_str!("fixtures/key.hex"))),
        ("log", bytes(include_str!("fixtures/log.hex"))),
        ("params", bytes(include_str!("fixtures/params.hex"))),
        ("request", bytes(include_str!("fixtures/request.hex"))),
        ("block", bytes(include_str!("fixtures/block.hex"))),
        ("object", bytes(include_str!("fixtures/object.hex"))),
    ]
}
#[test]
fn cpp_files_reencode_byte_for_byte() {
    for (name, bytes) in fixtures() {
        let (value, registry) = binary::decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let reencoded =
            binary::encode(&value, &registry).unwrap_or_else(|e| panic!("{name} encode: {e}"));
        assert_eq!(reencoded, bytes, "fixture {name}");
    }
}
#[test]
fn cpp_values_have_expected_fields() {
    let list = fixtures();
    let (value, _) = binary::decode(&list[0].1).unwrap();
    assert_eq!(value["__type"], "mmx.KeyFile");
    assert_eq!(value["seed_value"], json!((1..=32).collect::<Vec<_>>()));
    let (value, _) = binary::decode(&list[1].1).unwrap();
    assert_eq!(value["time"], 1700000000123456i64);
    assert_eq!(value["message"], "Hello 世界\nnext line");
    let (value, _) = binary::decode(&list[3].1).unwrap();
    assert_eq!(value["method"]["__type"], "mmx.Node.get_height");
    assert_eq!(value["request_id"], 42);
    let (value, _) = binary::decode(&list[5].1).unwrap();
    assert_eq!(value["uint64"], u64::MAX);
    assert_eq!(value["int64"], i64::MIN);
}
#[test]
fn big_endian_dynamic_and_raw_payloads_are_readable() {
    let value = json!({"counter":u64::MAX,"signed":i64::MIN,"text":"世界","nested":[1,2,true],"float":1.25});
    let mut writer = Writer::new(Vec::new(), Registry::new()).with_byte_order(ByteOrder::Big);
    writer.write(&value).unwrap();
    let (restored, _) = binary::decode(&writer.into_inner()).unwrap();
    assert_eq!(restored, value);
    let code = Code::Tuple(vec![Code::U32, Code::U64, Code::String]);
    let value = json!([123, u64::MAX, "hello"]);
    let mut writer = Writer::new(Vec::new(), Registry::new()).with_byte_order(ByteOrder::Big);
    writer.write_payload(&value, &code, None).unwrap();
    let mut reader = Reader::new(Cursor::new(writer.into_inner()), Registry::new())
        .with_byte_order(ByteOrder::Big);
    assert_eq!(reader.read_payload(&code, None).unwrap(), value);
    let (_, bytes) = fixtures().remove(0);
    let (value, registry) = binary::decode(&bytes).unwrap();
    assert!(Writer::new(Vec::new(), registry)
        .with_byte_order(ByteOrder::Big)
        .write(&value)
        .is_err());
}
#[test]
fn truncation_limits_and_trailing_data_are_rejected() {
    for (name, bytes) in fixtures() {
        for end in 0..bytes.len() {
            assert!(
                binary::decode(&bytes[..end]).is_err(),
                "{name} truncated at {end}"
            );
        }
        let mut tail = bytes.clone();
        tail.push(0);
        assert!(binary::decode(&tail).is_err());
        let limits = Limits {
            max_value_bytes: 20,
            ..Default::default()
        };
        assert!(Reader::new(Cursor::new(&bytes), Registry::new())
            .with_limits(limits)
            .read()
            .is_err());
    }
}
#[test]
fn typed_containers_and_schema_reuse_in_stream() {
    let codes = [
        Code::from_words(&[13, 5, 23, 0, 0]).is_err(),
        Code::from_words(&[23, 2, 4, 5, 4, 32]).is_ok(),
        Code::from_words(&[11]).is_err(),
        Code::from_words(&[13, 0, 1, 1]).is_err(),
    ];
    assert!(codes.iter().all(|v| *v));
    let code = Code::from_words(&[13, 8, 23, 2, 4, 5, 4, 4, 12, 32]).unwrap();
    assert_eq!(code.to_words(), vec![13, 8, 23, 2, 4, 5, 4, 4, 12, 32]);
    let code = Code::Map(
        Box::new(Code::Tuple(vec![Code::U64, Code::U64])),
        Box::new(Code::List(Box::new(Code::String))),
    );
    let value = json!([[[u64::MAX, 3], ["one", "two"]], [[1, 2], ["世界"]]]);
    let mut writer = Writer::new(Vec::new(), Registry::new());
    writer.write_payload(&value, &code, None).unwrap();
    let mut reader = Reader::new(Cursor::new(writer.into_inner()), Registry::new());
    assert_eq!(reader.read_payload(&code, None).unwrap(), value);
    let (_, data) = fixtures().remove(1);
    let (value, r) = binary::decode(&data).unwrap();
    let mut writer = Writer::new(Vec::new(), r);
    writer.write(&value).unwrap();
    writer.write(&value).unwrap();
    let mut reader = Reader::new(Cursor::new(writer.into_inner()), Registry::new());
    assert_eq!(reader.read().unwrap(), value);
    assert_eq!(reader.read().unwrap(), value);
}
#[test]
fn scalar_ranges_are_checked() {
    for (code, value) in [
        (Code::U8, json!(256)),
        (Code::I8, json!(128)),
        (Code::I16, json!(-32769)),
        (Code::Array(2, Box::new(Code::U8)), json!([1])),
    ] {
        let mut w = Writer::new(Vec::new(), Registry::new());
        assert!(w.write_payload(&value, &code, None).is_err());
    }
}

#[test]
fn database_payloads_and_separately_persisted_schemas() {
    let (_, bytes) = fixtures().remove(0);
    let (value, registry) = binary::decode(&bytes).unwrap();
    let hash = registry.by_name("mmx.KeyFile").unwrap().code_hash;
    let mut schema_writer = Writer::new(Vec::new(), registry.clone()).with_header(false);
    for schema in registry.iter() {
        schema_writer.write_type_code(schema).unwrap();
    }
    let mut schema_reader = Reader::new(Cursor::new(schema_writer.into_inner()), Registry::new());
    let restored_schema = schema_reader.read_type_code().unwrap();
    assert_eq!(restored_schema.code_hash, hash);
    let registry = schema_reader.registry().clone();
    let mut payload_writer = Writer::new(Vec::new(), registry.clone());
    payload_writer.write_typed_payload(&value, hash).unwrap();
    let payload = payload_writer.into_inner();
    assert_eq!(payload.len(), 32 + 1 + 4 + 10);
    let mut reader = Reader::new(Cursor::new(payload), registry.clone());
    assert_eq!(reader.read_typed_payload(hash).unwrap(), value);
    let mut writer = Writer::new(Vec::new(), registry.clone())
        .with_header(false)
        .with_type_codes(false);
    writer.write_typed(&value, hash).unwrap();
    let bytes = writer.into_inner();
    assert_eq!(&bytes[..2], &15u16.to_le_bytes());
    let mut reader = Reader::new(Cursor::new(bytes), registry);
    assert_eq!(reader.read().unwrap(), value);
}
#[test]
fn no_lossy_integer_coercion_and_clean_eof() {
    let too_big: serde_json::Value = serde_json::from_str("18446744073709551616").unwrap();
    assert!(binary::encode(&too_big, &Registry::new()).is_err());
    let bytes = binary::encode(&json!("hello"), &Registry::new()).unwrap();
    let mut reader = Reader::new(Cursor::new(bytes.clone()), Registry::new());
    assert_eq!(reader.read_next().unwrap(), Some(json!("hello")));
    assert_eq!(reader.read_next().unwrap(), None);
    for n in 1..bytes.len() {
        let mut reader = Reader::new(Cursor::new(&bytes[..n]), Registry::new());
        assert!(reader.read_next().is_err());
    }
}
#[test]
fn malformed_network_bytes_never_panic() {
    let limits = Limits {
        max_container: 1024,
        max_value_bytes: 16384,
        max_string_bytes: 4096,
        max_depth: 32,
        ..Default::default()
    };
    let mut seed = 1u64;
    for (_, bytes) in fixtures() {
        for _ in 0..500 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let mut input = bytes.clone();
            let pos = (seed as usize) % input.len();
            input[pos] ^= (seed >> 32) as u8;
            assert!(
                std::panic::catch_unwind(|| Reader::new(Cursor::new(&input), Registry::new())
                    .with_limits(limits.clone())
                    .read())
                .is_ok()
            );
        }
    }
}

#[test]
fn mixed_endian_nodes_and_nested_any_do_not_change_outer_order() {
    let code = Code::Alt(Box::new(Code::List(Box::new(Code::U16))));
    let mut writer = Writer::new(Vec::new(), Registry::new());
    writer
        .write_payload(&json!([0x1234, 0x5678]), &code, None)
        .unwrap();
    let bytes = writer.into_inner();
    assert_eq!(bytes, [0, 0, 0, 2, 0x34, 0x12, 0x78, 0x56]);
    let mut reader = Reader::new(Cursor::new(bytes), Registry::new());
    assert_eq!(
        reader.read_payload(&code, None).unwrap(),
        json!([0x1234, 0x5678])
    );
    let code = Code::Tuple(vec![
        Code::Alt(Box::new(Code::Any)),
        Code::U32,
        Code::Alt(Box::new(Code::U16)),
    ]);
    let value = json!([{"number":123,"label":"world"},0x12345678,0x1234]);
    let mut writer = Writer::new(Vec::new(), Registry::new());
    writer.write_payload(&value, &code, None).unwrap();
    let bytes = writer.into_inner();
    assert_eq!(
        &bytes[bytes.len() - 6..],
        &[0x78, 0x56, 0x34, 0x12, 0x12, 0x34]
    );
    let mut reader = Reader::new(Cursor::new(bytes), Registry::new());
    assert_eq!(reader.read_payload(&code, None).unwrap(), value);
}
