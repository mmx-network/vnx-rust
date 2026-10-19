#![no_main]
use libfuzzer_sys::fuzz_target;
use vnx::binary::{Reader, Writer};
use vnx::code::Code;
use vnx::{Field, Registry, TypeCode};
use vnx_fuzz::{allocation_ceiling, byte_order, check_value, heap_budget, limits};

fn registry(selector: u8) -> (Registry, TypeCode) {
    let mut registry = Registry::new();
    let mut top = TypeCode {
        name: "fuzz.Base".into(),
        type_hash: 1,
        code_hash: 1,
        ..Default::default()
    };
    registry.register(top.clone()).unwrap();
    let levels = u64::from(selector % 120);
    for index in 1..=levels {
        let dependency = if selector & 128 != 0 && index == levels {
            // Preloaded registries may contain cycles even though ordinary
            // wire-schema learning requires dependencies to be known first.
            index + 1
        } else {
            index
        };
        top = TypeCode {
            name: format!("fuzz.Schema{index}"),
            type_hash: index + 1,
            code_hash: index + 1,
            depends: vec![
                dependency,
                if selector & 64 != 0 {
                    index / 2 + 1
                } else {
                    dependency
                },
            ],
            fields: ["left", "right"]
                .into_iter()
                .enumerate()
                .map(|(position, name)| Field {
                    name: name.into(),
                    code: vec![19, position as u16],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        registry.register(top.clone()).unwrap();
    }
    (registry, top)
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 5 {
        return;
    }
    let limits = limits(data[0]);
    let flags = data[1];
    let count = usize::from(u16::from_le_bytes([data[3], data[4]]));
    let Some(end) = count.checked_mul(2).and_then(|n| n.checked_add(5)) else {
        return;
    };
    if end > data.len() {
        return;
    }
    let words: Vec<_> = data[5..end]
        .chunks_exact(2)
        .map(|word| u16::from_le_bytes([word[0], word[1]]))
        .collect();
    let Ok(code) = heap_budget(8 * 1024 * 1024, || Code::from_words(&words)) else {
        return;
    };
    // Code::from_words is public and accepts descriptors longer than the
    // 32-word on-wire limit, so fuzz its recursion guard independently.
    assert_eq!(code.to_words(), words);
    let (registry, owner) = registry(data[2]);
    let _ = heap_budget(8 * 1024 * 1024, || code.fixed_size(&registry, Some(&owner)));
    let mut reader = Reader::new(&data[end..], registry.clone())
        .with_limits(limits.clone())
        .with_byte_order(byte_order(flags))
        .with_utf8_lossy(flags & 8 != 0);
    let ceiling = allocation_ceiling(&limits);
    let result = heap_budget(ceiling, || {
        if flags & 16 != 0 {
            reader.read_typed_payload(owner.code_hash)
        } else {
            reader.read_payload(&code, Some(&owner))
        }
    });
    if let Ok(value) = result {
        check_value(&value, &limits);
        // Exercise the writer on successful decodes too, including malformed
        // descriptor defaults carried by a decoded TypeCode. Errors are valid.
        let mut writer = Writer::new(Vec::new(), reader.registry().clone())
            .with_limits(limits)
            .with_byte_order(byte_order(flags));
        let _ = heap_budget(ceiling, || {
            if flags & 16 != 0 {
                writer.write_typed_payload(&value, owner.code_hash)
            } else {
                writer.write_payload(&value, &code, Some(&owner))
            }
        });
    }
});
