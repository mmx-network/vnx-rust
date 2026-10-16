use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use vnx::{
    binary::{Reader, Writer},
    code::Code,
    Field, Fixed, Map, Registry, TypeCode,
};

#[test]
fn maps_sort_keys_and_keep_last_duplicate_like_native_code_map() {
    let map: Map<u32, String> =
        serde_json::from_value(json!([[9, "nine"], [2, "old"], [4, "four"], [2, "new"]])).unwrap();
    assert_eq!(map.len(), 3);
    assert_eq!(map.keys().copied().collect::<Vec<_>>(), [2, 4, 9]);
    assert_eq!(
        serde_json::to_value(&map).unwrap(),
        json!([[2, "new"], [4, "four"], [9, "nine"]])
    );
    let collected: Map<_, _> = [(2, "first"), (1, "one"), (2, "last")]
        .into_iter()
        .collect();
    assert_eq!(
        collected.into_iter().collect::<Vec<_>>(),
        [(1, "one"), (2, "last")]
    );
    // Reordering insertion has no effect on serialized bytes.
    let mut a = Map::new();
    a.insert(10u32, "ten");
    a.insert(1, "one");
    let b = Map::from([(1, "one"), (10, "ten")]);
    let code = Code::Map(Box::new(Code::U32), Box::new(Code::String));
    assert_eq!(
        payload(&a, &code, None, &Registry::new()),
        payload(&b, &code, None, &Registry::new())
    );
    assert_eq!(
        payload(&a, &code, None, &Registry::new()),
        vec![
            2, 0, 0, 0, 1, 0, 0, 0, 3, 0, 0, 0, b'o', b'n', b'e', 10, 0, 0, 0, 3, 0, 0, 0, b't',
            b'e', b'n'
        ]
    );
}

fn payload(
    value: &impl Serialize,
    code: &Code,
    owner: Option<&TypeCode>,
    registry: &Registry,
) -> Vec<u8> {
    let mut writer = Writer::new(Vec::new(), registry.clone());
    writer
        .write_payload(&serde_json::to_value(value).unwrap(), code, owner)
        .unwrap();
    writer.into_inner()
}

#[test]
fn maps_accept_json_objects_with_compatible_key_types() {
    let strings: Map<String, u32> = serde_json::from_str(r#"{"z":3,"a":1}"#).unwrap();
    assert_eq!(strings.get("a"), Some(&1));
    assert_eq!(
        serde_json::to_value(strings).unwrap(),
        json!([["a", 1], ["z", 3]])
    );
    let integers: Map<u32, String> =
        serde_json::from_str(r#"{"42":"forty-two","2":"two"}"#).unwrap();
    assert_eq!(integers.keys().copied().collect::<Vec<_>>(), [2, 42]);
    let from_value: Map<u32, String> =
        serde_json::from_value(json!({"42":"forty-two","2":"two"})).unwrap();
    assert_eq!(from_value, integers);
    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
    enum Key {
        First,
        Last,
    }
    let enums: Map<Key, bool> = serde_json::from_str(r#"{"Last":true,"First":false}"#).unwrap();
    assert_eq!(enums.get(&Key::First), Some(&false));
    // Duplicate object properties also follow native CODE_OBJECT overwrite behavior.
    let duplicate: Map<String, u32> = serde_json::from_str(r#"{"a":1,"a":2}"#).unwrap();
    assert_eq!(duplicate.get("a"), Some(&2));
}

#[test]
fn map_default_and_conveniences_need_no_key_or_value_default() {
    #[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct NoDefaultKey(u8);
    struct NoDefaultValue;
    let empty: Map<NoDefaultKey, NoDefaultValue> = Default::default();
    assert!(empty.is_empty());
    let mut map = Map::from([(3, 30), (1, 10)]);
    assert_eq!(map.insert(1, 11), Some(10));
    *map.entry(2).or_default() = 20;
    for (key, value) in &mut map {
        *value += *key;
    }
    assert_eq!(
        (&map)
            .into_iter()
            .map(|(k, v)| (*k, *v))
            .collect::<Vec<_>>(),
        [(1, 12), (2, 22), (3, 33)]
    );
    map.extend([(2, 200), (4, 400)]);
    assert_eq!(map[&2], 200);
    let references = [(5, 500), (6, 600)];
    map.extend(references.iter().map(|(k, v)| (k, v)));
    let native: BTreeMap<_, _> = map.clone().into();
    assert_eq!(Map::from(native), map);
    let native = map.into_inner();
    assert_eq!(native.len(), 6);
    assert!(Map::from([(1, 2)]) < Map::from([(1, 3)]));
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct CompoundKey {
    address: Fixed<u8, 32>,
    index: u16,
}
#[test]
fn compound_struct_and_fixed_byte_keys_roundtrip_native_map_payloads() {
    let key_schema = TypeCode {
        name: "test.CompoundKey".into(),
        type_hash: 1,
        code_hash: 2,
        fields: vec![
            Field {
                name: "address".into(),
                code: vec![11, 32, 1],
                data_size: 32,
                ..Default::default()
            },
            Field {
                name: "index".into(),
                code: vec![2],
                data_size: 2,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let owner = TypeCode {
        name: "test.Owner".into(),
        type_hash: 3,
        code_hash: 4,
        depends: vec![2],
        ..Default::default()
    };
    let mut registry = Registry::new();
    registry.register(key_schema).unwrap();
    let first = CompoundKey {
        address: Fixed([1; 32]),
        index: 0,
    };
    let last = CompoundKey {
        address: Fixed([0xff; 32]),
        index: 0,
    };
    let middle = CompoundKey {
        address: Fixed([1; 32]),
        index: 1,
    };
    let map = Map::from([
        (last.clone(), 30u64),
        (first.clone(), 10),
        (middle.clone(), 20),
    ]);
    assert_eq!(
        map.keys().cloned().collect::<Vec<_>>(),
        [first, middle, last]
    );
    let code = Code::Map(Box::new(Code::Struct(0)), Box::new(Code::U64));
    let bytes = payload(&map, &code, Some(&owner), &registry);
    let value = Reader::new(bytes.as_slice(), registry)
        .read_payload(&code, Some(&owner))
        .unwrap();
    let restored: Map<CompoundKey, u64> = serde_json::from_value(value).unwrap();
    assert_eq!(restored, map);
    // Compound keys serialize as pair arrays instead of invalid JSON object names.
    let json = serde_json::to_value(&map).unwrap();
    assert!(json[0][0]["address"].is_array());
    let restored: Map<CompoundKey, u64> =
        serde_json::from_str(&serde_json::to_string(&map).unwrap()).unwrap();
    assert_eq!(restored, map);
}

#[test]
fn map_decode_overwrites_duplicate_wire_keys_and_rejects_malformed_entries() {
    let mut bytes = vec![3, 0, 0, 0];
    for (key, value) in [(2u8, 10u8), (1, 20), (2, 30)] {
        bytes.extend_from_slice(&[key, value]);
    }
    let code = Code::Map(Box::new(Code::U8), Box::new(Code::U8));
    let value = Reader::new(bytes.as_slice(), Registry::new())
        .read_payload(&code, None)
        .unwrap();
    let map: Map<u8, u8> = serde_json::from_value(value).unwrap();
    assert_eq!(map, Map::from([(1, 20), (2, 30)]));
    for invalid in [
        json!([1]),
        json!([[1]]),
        json!([[1, 2, 3]]),
        json!([null]),
        json!(null),
        json!(true),
        json!(42),
    ] {
        assert!(serde_json::from_value::<Map<u8, u8>>(invalid).is_err());
    }
}

#[test]
fn sets_use_ordered_unique_native_list_representation() {
    let set: BTreeSet<u32> = serde_json::from_value(json!([9, 2, 9, 1, 2])).unwrap();
    assert_eq!(serde_json::to_value(&set).unwrap(), json!([1, 2, 9]));
    let code = Code::List(Box::new(Code::U32));
    let bytes = payload(&set, &code, None, &Registry::new());
    let value = Reader::new(bytes.as_slice(), Registry::new())
        .read_payload(&code, None)
        .unwrap();
    assert_eq!(serde_json::from_value::<BTreeSet<u32>>(value).unwrap(), set);
}
