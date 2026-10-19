use std::io::Cursor;
use vnx::binary::{Limits, Reader};
use vnx::code::Code;
use vnx::serde_json::{json, Value};
use vnx::{Error, Field, Registry, TypeCode};

fn reader(bytes: Vec<u8>, max_depth: usize) -> Reader<Cursor<Vec<u8>>> {
    Reader::new(Cursor::new(bytes), Registry::new()).with_limits(Limits {
        max_depth,
        ..Default::default()
    })
}

fn assert_depth_limit(result: vnx::Result<impl std::fmt::Debug>) {
    assert!(
        matches!(result, Err(Error::Limit(ref reason)) if reason == "recursion depth"),
        "{result:?}"
    );
}

#[test]
fn payload_depth_boundary_honors_default_and_custom_limits() {
    for limit in [8, Limits::default().max_depth] {
        let mut code = Code::Null;
        for _ in 0..limit - 1 {
            code = Code::Optional(Box::new(code));
        }
        assert_eq!(
            reader(vec![1; limit - 1], limit)
                .read_payload(&code, None)
                .unwrap(),
            Value::Null
        );
        code = Code::Optional(Box::new(code));
        assert_depth_limit(reader(vec![1; limit], limit).read_payload(&code, None));
    }
}

#[test]
fn dynamic_headers_cannot_reset_the_depth_budget() {
    let mut payload = Vec::new();
    for _ in 0..200 {
        // A one-word dynamic descriptor containing another dynamic value.
        payload.extend_from_slice(&[1, 0, 17, 0]);
    }
    payload.extend_from_slice(&[1, 0, 0, 0]);
    let mut stream = vec![17, 0];
    stream.extend_from_slice(&payload);
    for limit in [8, Limits::default().max_depth] {
        assert_depth_limit(reader(stream.clone(), limit).read());
        assert_depth_limit(reader(payload.clone(), limit).read_payload(&Code::Dynamic, None));
    }
}

#[test]
fn object_fields_cannot_reset_the_depth_budget() {
    // {x: {x: ... {}}} using independent dynamic descriptors for each field.
    let mut payload = Vec::new();
    for _ in 0..200 {
        payload.extend_from_slice(&[1, 0, 0, 0, 1, 0, 0, 0, b'x']);
        payload.extend_from_slice(&[1, 0, 24, 0]);
    }
    // The innermost object is empty.
    payload.extend_from_slice(&[0, 0, 0, 0]);
    for limit in [8, Limits::default().max_depth] {
        assert_depth_limit(reader(payload.clone(), limit).read_payload(&Code::Object, None));
    }
    let mut shallow = vec![1, 0, 0, 0, 1, 0, 0, 0, b'x'];
    shallow.extend_from_slice(&[1, 0, 0, 0]);
    assert_eq!(
        reader(shallow, 8)
            .read_payload(&Code::Object, None)
            .unwrap(),
        json!({"x": null})
    );
}

#[test]
fn nested_schema_prefixes_stop_before_reading_the_leaf() {
    // TypeCode bodies can themselves start with nested TypeCode definitions.
    // No leaf is necessary: depth rejection must precede the truncated body.
    let bytes = [14, 0].repeat(200);
    for limit in [8, Limits::default().max_depth] {
        assert_depth_limit(reader(bytes.clone(), limit).read_type_code());
        assert_depth_limit(reader(bytes.clone(), limit).read());
    }
}

#[test]
fn cyclic_preloaded_structs_are_bounded_for_values_and_layouts() {
    let schema = TypeCode {
        name: "test.Cycle".into(),
        type_hash: 1,
        code_hash: 1,
        depends: vec![1],
        fields: vec![Field {
            name: "child".into(),
            code: vec![19, 0],
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut registry = Registry::new();
    registry.register(schema.clone()).unwrap();
    for limit in [8, Limits::default().max_depth] {
        let mut input = Reader::new(Cursor::new(vec![]), registry.clone()).with_limits(Limits {
            max_depth: limit,
            ..Default::default()
        });
        assert_depth_limit(input.read_typed_payload(1));
    }
    assert!(matches!(
        Code::Struct(0).fixed_size(&registry, Some(&schema)),
        Err(Error::Invalid(reason)) if reason == "recursive inline struct"
    ));
}

#[test]
fn bytecode_parser_has_an_independent_recursion_cap() {
    let mut words = vec![33; 99];
    words.push(0);
    assert!(Code::from_words(&words).is_ok());
    words.insert(0, 33);
    assert!(matches!(
        Code::from_words(&words),
        Err(Error::Invalid(reason)) if reason == "byte code recursion"
    ));
}

#[test]
fn cached_layouts_do_not_bypass_the_depth_limit() {
    let leaf = TypeCode {
        name: "test.Leaf".into(),
        type_hash: 1,
        code_hash: 1,
        fields: vec![Field {
            name: "value".into(),
            code: vec![0],
            ..Default::default()
        }],
        ..Default::default()
    };
    let owner = TypeCode {
        depends: vec![1],
        ..Default::default()
    };
    let mut registry = Registry::new();
    registry.register(leaf).unwrap();
    let mut deep = Code::Struct(0);
    for _ in 0..98 {
        deep = Code::Array(1, Box::new(deep));
    }
    // The shallow sibling populates the same schema's layout cache first.
    let code = Code::Tuple(vec![Code::Struct(0), deep]);
    assert!(matches!(
        code.fixed_size(&registry, Some(&owner)),
        Err(Error::Invalid(reason)) if reason == "recursive inline struct"
    ));
}

#[test]
fn sequential_any_prefixes_do_not_consume_recursion_depth() {
    let mut bytes = [16, 0].repeat(10_000);
    bytes.extend_from_slice(&[0, 0]);
    assert_eq!(reader(bytes, 1).read().unwrap(), Value::Null);
}
