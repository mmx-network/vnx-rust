use std::io::{self, Cursor, Read};
use std::sync::{Arc, Mutex};
use vnx::binary::{Limits, Reader, Writer};
use vnx::code::Code;
use vnx::serde_json::{json, Value};
use vnx::{Error, Field, Registry, TypeCode};

fn bounded(bytes: Vec<u8>, budget: usize) -> Reader<Cursor<Vec<u8>>> {
    Reader::new(Cursor::new(bytes), Registry::new()).with_limits(Limits {
        max_allocated_bytes: budget,
        ..Default::default()
    })
}
fn assert_allocation_limit(result: vnx::Result<impl std::fmt::Debug>) {
    assert!(
        matches!(result, Err(Error::Limit(ref reason)) if reason == "decoded allocation"),
        "{result:?}"
    );
}

#[test]
fn nested_zero_byte_elements_share_one_allocation_budget() {
    let words = [11_u16, 64, 11, 64, 11, 64, 0];
    let mut bytes = vec![17, 0, words.len() as u8, 0];
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    assert_eq!(bytes.len(), 18);
    // The serialized size and each individual array are small. The expanded
    // tree has 266,305 Value nodes and must share one budget across all arrays.
    assert_allocation_limit(bounded(bytes, 256 * 1024).read());
    let code = Code::Array(2, Box::new(Code::Array(2, Box::new(Code::Null))));
    assert_eq!(
        bounded(vec![], 16 * 1024)
            .read_payload(&code, None)
            .unwrap(),
        json!([[null, null], [null, null]])
    );
}

#[test]
fn default_limits_stop_compact_array_expansion() {
    let words = [11_u16, u16::MAX, 11, u16::MAX, 0];
    let mut bytes = vec![17, 0, words.len() as u8, 0];
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    // Without an aggregate budget this 14-byte value expands to billions of
    // elements even though every individual container is below its limit.
    assert_allocation_limit(Reader::new(Cursor::new(bytes), Registry::new()).read());
}

#[test]
fn maps_and_objects_charge_storage_before_allocating_entries() {
    let mut count = 1000_u32.to_le_bytes().to_vec();
    assert_allocation_limit(
        bounded(count.clone(), 4096)
            .read_payload(&Code::Map(Box::new(Code::Null), Box::new(Code::Null)), None),
    );
    count.splice(0..0, [24, 0]);
    assert_allocation_limit(bounded(count, 4096).read());
}

#[test]
fn typed_records_do_not_reset_the_budget_for_nested_fields() {
    let schema = TypeCode {
        name: "test.Expansion".into(),
        type_hash: 123,
        code_hash: 456,
        is_class: true,
        fields: vec![Field {
            name: "values".into(),
            code: vec![11, 64, 11, 64, 0],
            is_extended: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut writer = Writer::new(Vec::new(), Registry::new());
    writer.write_type_code(&schema).unwrap();
    let mut bytes = writer.into_inner();
    bytes.extend_from_slice(&15_u16.to_le_bytes());
    bytes.extend_from_slice(&schema.code_hash.to_le_bytes());
    assert_allocation_limit(bounded(bytes, 64 * 1024).read());
}

#[test]
fn schema_storage_budget_survives_between_read_operations() {
    let mut writer = Writer::new(Vec::new(), Registry::new());
    for hash in 1..=4 {
        writer
            .write_type_code(&TypeCode {
                name: format!("test.Schema{hash}"),
                type_hash: hash,
                code_hash: hash,
                ..Default::default()
            })
            .unwrap();
    }
    let mut reader =
        Reader::new(Cursor::new(writer.into_inner()), Registry::new()).with_limits(Limits {
            max_schema_bytes: 12 * 1024,
            ..Default::default()
        });
    reader.read_type_code().unwrap();
    reader.read_type_code().unwrap();
    assert!(
        matches!(reader.read_type_code(), Err(Error::Limit(ref reason)) if reason == "schema allocation")
    );
    assert_eq!(reader.registry().len(), 2);
}

#[test]
fn repeated_schema_does_not_consume_another_registry_allocation() {
    let schema = TypeCode {
        name: "test.Shared".into(),
        type_hash: 1,
        code_hash: 2,
        ..Default::default()
    };
    let mut writer = Writer::new(Vec::new(), Registry::new());
    writer.write_type_code(&schema).unwrap();
    let mut bytes = writer.into_inner();
    bytes.extend_from_within(..);
    let mut reader = Reader::new(Cursor::new(bytes), Registry::new()).with_limits(Limits {
        max_schema_bytes: 10 * 1024,
        ..Default::default()
    });
    reader.read_type_code().unwrap();
    reader.read_type_code().unwrap();
    assert_eq!(reader.registry().len(), 1);
}

#[test]
fn schema_json_views_have_the_same_allocation_limit() {
    let schema = TypeCode {
        name: "test.Metadata".into(),
        type_hash: 1,
        code_hash: 2,
        ..Default::default()
    };
    let mut writer = Writer::new(Vec::new(), Registry::new());
    writer
        .write_payload(
            &vnx::serde_json::to_value(schema).unwrap(),
            &Code::TypeCode,
            None,
        )
        .unwrap();
    assert_allocation_limit(
        bounded(writer.into_inner(), 20 * 1024).read_payload(&Code::TypeCode, None),
    );
}

struct ObservedReader {
    bytes: Cursor<Vec<u8>>,
    largest: Arc<Mutex<usize>>,
}
impl Read for ObservedReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let mut largest = self.largest.lock().unwrap();
        *largest = (*largest).max(bytes.len());
        self.bytes.read(bytes)
    }
}

#[test]
fn truncated_strings_are_read_in_bounded_chunks() {
    let largest = Arc::new(Mutex::new(0));
    let mut bytes = vec![17, 0, 1, 0, 32, 0];
    bytes.extend_from_slice(&(16_u32 * 1024 * 1024).to_le_bytes());
    let mut reader = Reader::new(
        ObservedReader {
            bytes: Cursor::new(bytes),
            largest: largest.clone(),
        },
        Registry::new(),
    );
    assert!(
        matches!(reader.read(), Err(Error::Io(ref error)) if error.kind() == io::ErrorKind::UnexpectedEof)
    );
    assert!(*largest.lock().unwrap() <= 16 * 1024);
}

#[test]
fn chunked_strings_and_byte_lists_preserve_exact_data() {
    let text = "世界abc".repeat(9000);
    let value = Value::String(text);
    for code in [Code::String, Code::List(Box::new(Code::I8))] {
        let mut writer = Writer::new(Vec::new(), Registry::new());
        writer.write_payload(&value, &code, None).unwrap();
        assert_eq!(
            bounded(writer.into_inner(), 1024 * 1024)
                .read_payload(&code, None)
                .unwrap(),
            value
        );
    }
}

#[test]
fn input_padding_is_skipped_without_charging_a_heap_buffer() {
    let padding = 48 * 1024_u32;
    let mut bytes = vec![18, 0];
    bytes.extend_from_slice(&padding.to_le_bytes());
    bytes.resize(bytes.len() + padding as usize, 0);
    bytes.extend_from_slice(&[0, 0]);
    assert_eq!(
        bounded(bytes, std::mem::size_of::<Value>()).read().unwrap(),
        Value::Null
    );
}

#[test]
fn per_value_budget_resets_between_stream_values() {
    let value = json!("small string");
    let mut writer = Writer::new(Vec::new(), Registry::new());
    writer.write(&value).unwrap();
    writer.write(&value).unwrap();
    let mut reader = bounded(writer.into_inner(), 512);
    assert_eq!(reader.read().unwrap(), value);
    assert_eq!(reader.read().unwrap(), value);
}

fn shared_layouts(levels: u64) -> (Registry, TypeCode) {
    let base = TypeCode {
        name: "test.Empty".into(),
        type_hash: 1,
        code_hash: 1,
        ..Default::default()
    };
    let mut registry = Registry::new();
    registry.register(base.clone()).unwrap();
    let mut top = base;
    for level in 1..=levels {
        top = TypeCode {
            name: format!("test.Shared{level}"),
            type_hash: level + 1,
            code_hash: level + 1,
            depends: vec![level],
            fields: ["left", "right"]
                .map(|name| Field {
                    name: name.into(),
                    code: vec![19, 0],
                    ..Default::default()
                })
                .into(),
            ..Default::default()
        };
        registry.register(top.clone()).unwrap();
    }
    (registry, top)
}

#[test]
fn shared_schema_layouts_are_not_expanded_as_an_exponential_tree() {
    let (registry, top) = shared_layouts(60);
    let mut writer = Writer::new(Vec::new(), registry);
    writer.write_type_code(&top).unwrap();
    let mut bytes = writer.into_inner();
    bytes.extend_from_slice(&[0, 0]);
    let mut reader = Reader::new(Cursor::new(bytes), Registry::new());
    assert_eq!(reader.read().unwrap(), Value::Null);
    assert_eq!(reader.registry().len(), 61);
}

#[test]
fn schema_layout_cache_uses_the_decoder_allocation_budget() {
    let (registry, top) = shared_layouts(60);
    let mut writer = Writer::new(Vec::new(), registry.clone());
    writer.write_type_code(&top).unwrap();
    let mut prefix = Reader::new(Cursor::new(writer.into_inner()), Registry::new());
    for _ in 0..60 {
        prefix.read_type_code().unwrap();
    }
    let registry = prefix.registry().clone();
    let mut reader = Reader::new(prefix.into_inner(), registry).with_limits(Limits {
        max_allocated_bytes: 16 * 1024,
        ..Default::default()
    });
    assert_allocation_limit(reader.read_type_code());
}
