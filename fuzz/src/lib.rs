//! Shared fuzzing oracles. This crate is separate from the production workspace.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use vnx::binary::{ByteOrder, Limits, Reader};
use vnx::serde_json::Value;
use vnx::Registry;

#[derive(Clone, Copy)]
struct Budget {
    remaining: usize,
}

thread_local! {
    static BUDGET: Cell<Option<Budget>> = const { Cell::new(None) };
}

struct Meter;

fn charge(bytes: usize) {
    let _ = BUDGET.try_with(|cell| {
        if let Some(mut budget) = cell.get() {
            let Some(remaining) = budget.remaining.checked_sub(bytes) else {
                // A global allocator must never unwind or recursively allocate.
                // libFuzzer handles SIGABRT and saves the reproducing input.
                const MESSAGE: &[u8] = b"vnx fuzz: actual allocation budget exceeded\n";
                unsafe {
                    libc::write(2, MESSAGE.as_ptr().cast(), MESSAGE.len());
                }
                std::process::abort();
            };
            budget.remaining = remaining;
            cell.set(Some(budget));
        }
    });
}

unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        charge(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        charge(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // Count the entire new request, including buffers temporarily retained
        // during growth. Deallocation deliberately does not replenish the cap.
        charge(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Meter = Meter;

pub fn heap_budget<T>(bytes: usize, run: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            BUDGET.with(|cell| cell.set(None));
        }
    }
    BUDGET.with(|cell| {
        assert!(cell.get().is_none(), "nested fuzz allocation meter");
        cell.set(Some(Budget { remaining: bytes }));
    });
    let _reset = Reset;
    run()
}

pub fn limits(profile: u8) -> Limits {
    let max_allocated_bytes = if profile & 128 != 0 {
        0
    } else if profile & 64 != 0 {
        128
    } else {
        [4096, 65_536, 262_144, 1_048_576][usize::from(profile & 3)]
    };
    Limits {
        max_depth: [0, 1, 4, 16, 100][usize::from(profile >> 2) % 5],
        max_container: 1_000_000,
        max_string_bytes: 65_536,
        max_value_bytes: 65_536,
        max_schemas: 32,
        max_allocated_bytes,
        max_schema_bytes: max_allocated_bytes,
    }
}

pub fn allocation_ceiling(limits: &Limits) -> usize {
    // Slack covers error messages, allocator capacity and bookkeeping absent
    // from the codec's conservative accounting. This is an actual-request cap,
    // independent of the decoder's own implementation of its allocation limit.
    limits.max_allocated_bytes * 4 + 32 * 1024
}

pub fn reader(data: &[u8], limits: Limits, flags: u8) -> Reader<&[u8]> {
    Reader::new(data, Registry::new())
        .with_limits(limits)
        .with_byte_order(byte_order(flags))
        .with_utf8_lossy(flags & 8 != 0)
}

pub fn byte_order(flags: u8) -> ByteOrder {
    if flags & 4 != 0 {
        ByteOrder::Big
    } else {
        ByteOrder::Little
    }
}

pub fn check_value(value: &Value, limits: &Limits) {
    assert!(limits.max_depth > 0 && limits.max_allocated_bytes > 0);
    let mut pending = vec![(value, 0usize)];
    let mut bytes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        assert!(depth <= limits.max_depth * 2 + 4);
        bytes += std::mem::size_of::<Value>();
        match value {
            Value::String(text) => bytes += text.len(),
            Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(fields) => {
                for (name, value) in fields {
                    bytes += name.len();
                    pending.push((value, depth + 1));
                }
            }
            _ => {}
        }
        assert!(bytes <= limits.max_allocated_bytes + 4096);
    }
}
