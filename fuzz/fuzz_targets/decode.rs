#![no_main]
use libfuzzer_sys::fuzz_target;
use vnx::code::Code;
use vnx_fuzz::{allocation_ceiling, check_value, heap_budget, limits, reader};

fuzz_target!(|data: &[u8]| {
    let Some((&profile, rest)) = data.split_first() else {
        return;
    };
    let Some((&flags, payload)) = rest.split_first() else {
        return;
    };
    let limits = limits(profile);
    let ceiling = allocation_ceiling(&limits);
    let mut reader = reader(payload, limits.clone(), flags);
    match flags & 3 {
        0 => {
            // Keep one reader so learned schemas and per-value resets interact.
            for _ in 0..16 {
                match heap_budget(ceiling, || reader.read_next()) {
                    Ok(Some(value)) => check_value(&value, &limits),
                    Ok(None) | Err(_) => break,
                }
            }
        }
        1 => {
            for _ in 0..16 {
                match heap_budget(ceiling, || reader.read_type_code()) {
                    Ok(_) => assert!(limits.max_depth > 0 && limits.max_allocated_bytes > 0),
                    Err(_) => break,
                }
            }
        }
        mode => {
            let code = if mode == 2 {
                Code::Dynamic
            } else {
                Code::Object
            };
            if let Ok(value) = heap_budget(ceiling, || reader.read_payload(&code, None)) {
                check_value(&value, &limits);
            }
        }
    }
});
