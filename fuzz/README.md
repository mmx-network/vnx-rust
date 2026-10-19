# Serialization fuzzing

The `decode` and `bytecode` targets fuzz the public binary serialization code.
This is an independent Cargo workspace so nightly tools and `libfuzzer-sys` do
not become dependencies of the library or its normal tests.

`decode` exercises stream values, TypeCode definitions, dynamic values, and
objects with mutated wire bytes. `bytecode` exercises bytecode parsing, raw and
typed payload decoding, re-encoding, and fixed-size calculations for schema
dependency graphs. Graphs include shared dependencies, paths of different
lengths, and cycles in preloaded registries. The seed
corpus includes valid values, C++ compatibility fixtures, truncated data,
compact container expansion, deep recursion, and shared schema layouts.

The harness varies `Limits.max_depth` between 0, 1, 4, 16, and 100 and decoded
allocation budgets of 0, 128 bytes, 4 KiB, 64 KiB, 256 KiB and 1 MiB. A counting
allocator observes actual heap allocation requests during each public decoding
operation and aborts if
they exceed a generous multiple of the configured budget plus fixed overhead.
This is independent of the decoder's own accounting and also catches temporary
allocations that are freed before an operation returns. The ceiling is
`4 * max_allocated_bytes + 32 KiB` per decoding or writing operation; bytecode
parsing and schema layout calculations each have an independent 8 MiB ceiling.
These assertions measure cumulative allocation requests, rather than process RSS
or exact live memory. Successfully decoded values also have their node/string
storage and nesting checked independently of the decoder.
Expected decoding errors are allowed; panics, allocation assertion failures,
sanitizer findings, and resource-guard failures fail the fuzz run.

Run from the repository root on Linux with a nightly Rust toolchain:

```sh
rustup toolchain install nightly --component rustfmt,clippy
cargo install cargo-fuzz --version 0.13.2 --locked
cargo +nightly metadata --manifest-path fuzz/Cargo.toml --locked --format-version 1 > /dev/null
python3 fuzz/make_seeds.py
mkdir -p fuzz/corpus/decode fuzz/corpus/bytecode
cp fuzz/seeds/decode/* fuzz/corpus/decode/
cp fuzz/seeds/bytecode/* fuzz/corpus/bytecode/

DEBUGINFOD_URLS= cargo +nightly fuzz run decode fuzz/corpus/decode -- \
  -max_total_time=300 -timeout=5 -rss_limit_mb=512 \
  -malloc_limit_mb=128 -max_len=65536 -print_funcs=0 -print_final_stats=1
DEBUGINFOD_URLS= cargo +nightly fuzz run bytecode fuzz/corpus/bytecode -- \
  -max_total_time=300 -timeout=5 -rss_limit_mb=512 \
  -malloc_limit_mb=128 -max_len=65536 -print_funcs=0 -print_final_stats=1
```

`cargo-fuzz` enables AddressSanitizer, debug assertions, and integer overflow
checks by default. These commands use the prebuilt standard library; rebuilding
it with `--build-std` is optional and also requires the `rust-src` component.
The libFuzzer options limit each input to five seconds, process RSS to 512 MiB,
individual allocations to 128 MiB, and input
length to 64 KiB. The RSS and timeout guards run separately from the per-operation
allocation assertions. Large sanitizer overhead or a slow machine can require
adjusting these process limits when investigating a failure.
Disabling debuginfod lookups and newly covered function-name printing prevents
symbolizer network requests from delaying a time-bounded run; crash stacks
still use locally available debug symbols.

Committed seeds live in `fuzz/seeds/`; generated coverage inputs go in ignored
`fuzz/corpus/`. `make_seeds.py` deterministically regenerates the seed corpus.
Retain a useful new failing input as a named seed and add a focused regression
test after fixing the underlying bug.

Crashes are saved under `fuzz/artifacts/<target>/`. Replay a saved input with the
same target and resource guards:

```sh
cargo +nightly fuzz run decode fuzz/artifacts/decode/crash-HASH -- \
  -runs=1 -timeout=5 -rss_limit_mb=512 -malloc_limit_mb=128
```

Decoder inputs begin with profile (allocation/depth limits) and flags (entry
point, byte order and lossy UTF-8) bytes, followed by wire data. Bytecode inputs
begin with profile, flags and graph-selector bytes, then a little-endian `u16`
word count, that many `u16` bytecode words, and payload data. `make_seeds.py`
shows examples of both encodings.

Replace `HASH` with the saved filename's hash and `decode` with `bytecode` when
reproducing that target. Keep the complete fuzzer output with the input: a
timeout or RSS failure can depend on the local
environment, and sanitizer reports contain the failing call stack.
LeakSanitizer's exit scan needs permission to inspect process threads; a sandbox
that blocks this can produce a fatal LeakSanitizer/ptrace diagnostic even for
empty input. Run outside that restriction to retain the leak checks.

Check the independent harness workspace before submitting changes:

```sh
cargo +nightly fmt --manifest-path fuzz/Cargo.toml --all -- --check
cargo +nightly clippy --manifest-path fuzz/Cargo.toml --all-targets --locked -- -D warnings
```

GitHub Actions runs these checks and each target for 20 seconds on pushes and
pull requests and for 300 seconds each week. A manual run accepts 1 to 3600
seconds per target.
Successful runs cache their coverage corpus for subsequent campaigns. Failing
inputs are uploaded as workflow artifacts for 14 days. These bounded
runs complement the allocation and recursion regression tests; they provide
evidence for the exercised inputs, not a proof that all malformed inputs are
safe. Longer local runs reuse the writable corpus and explore more paths.
