use std::io::Cursor;
use vnx::{
    binary::{Reader, Writer},
    serde_json::json,
    Field, Registry, TypeCode,
};

#[test]
fn native_signed_default_metadata_constructs_unsigned_values_without_changing_explicit_inputs() {
    let mut registry = Registry::new();
    registry
        .register(TypeCode {
            name: "example.Defaults".into(),
            type_hash: 10,
            code_hash: 11,
            is_class: true,
            fields: [1u16, 2, 3, 4, 5, 6, 7, 8]
                .into_iter()
                .enumerate()
                .map(|(i, code)| Field {
                    name: format!("value{i}"),
                    code: vec![code],
                    value: "-1".into(),
                    data_size: [1, 2, 4, 8][i % 4],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
        .unwrap();
    let mut writer = Writer::new(Vec::new(), registry.clone());
    writer.write(&json!({"__type":"example.Defaults"})).unwrap();
    let mut reader = Reader::new(Cursor::new(writer.into_inner()), registry.clone());
    let result = reader.read().unwrap();
    assert_eq!(
        result,
        json!({"__type":"example.Defaults", "value0":u8::MAX,"value1":u16::MAX,
        "value2":u32::MAX,"value3":u64::MAX,"value4":-1,"value5":-1,"value6":-1,"value7":-1})
    );
    assert!(reader
        .registry()
        .by_name("example.Defaults")
        .unwrap()
        .fields
        .iter()
        .all(|f| f.value == "-1"));
    for index in 0..4 {
        let mut value = json!({"__type":"example.Defaults"});
        value[format!("value{index}")] = json!(-1);
        assert!(Writer::new(Vec::new(), registry.clone())
            .write(&value)
            .is_err());
    }
}
