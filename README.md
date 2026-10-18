# vnx-rust

Native Rust VNX foundations for porting MMX. The Rust crates and generated bindings do not link to C++ or invoke the original VNX compiler. They implement the existing wire format and identity rules, rather than introducing a replacement protocol.

## Crates and tools

- **`vnx`**: binary codecs, reflection and type registries; dynamic values; ordered maps and fixed arrays; configuration; logging; authentication; process/module lifecycle; topics, RPC, timers and thread pools; TCP/Unix transport; file, hashing and clock utilities.
- **`vnx-codegen`**: VNI parser, resolver, schema compiler, Rust generator and `build.rs` integration.
- **`vnxrustcodegen`**: generates a Rust module tree and optional schema JSON from VNI directories.
- **`vnxdump`**: reads self-describing VNX files without generated types and prints their JSON value or schemas.

Rust 1.85 or newer is required. No MMX checkout, CMake or C++ compiler is needed for normal builds/tests.

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

## Implemented compatibility

### Serialization and reflection

`binary::Reader` and `binary::Writer` work with any Rust `Read`/`Write` stream. They support VNX magic, schema definitions, class headers, v1/v2 type/field metadata, primitives, UTF-8 strings, arrays, lists, arbitrary-key maps, tuples, optionals, matrices, images, objects, dynamic variants and polymorphic values. Inline fields precede extended fields exactly as in VNX. Unknown fields remain available in the dynamic object view; generated bindings apply VNI defaults and field aliases.

Schemas retain type hashes, version/code hashes, dependencies, inheritance, enums, permissions, method/return metadata and aliases. Decoders enforce configurable allocation, depth, container and schema limits. Integer conversion checks ranges and never converts oversized integer literals to floating point.

Unsigned field defaults retain native descriptor text such as `-1`. Generated Rust defaults and omitted fields apply the unsigned constructor cast when creating values; explicit negative inputs still fail unsigned range checks. Preserving the descriptor prevents a preloaded registry from rejecting a native peer's otherwise identical schema.

Self-describing typed output uses the canonical little-endian MMX schema ABI. Readers support alternate-endian bytecodes and independent node/field byte orders. Big-endian raw/dynamic payloads can be written, but typed big-endian output with little-endian compiler hashes is rejected: original VNX structural hashes include native-endian integer bytes, and reusing those identities corrupts C++ schema matching.

For database tables, use `write_payload` / `read_payload` for bytecoded fields, `write_typed_payload` / `read_typed_payload` for externally described records, or `with_header(false).with_type_codes(false)` for class values whose registry is persisted separately. `write_type_code` / `read_type_code` support schema-only streams; registries also import/export JSON. `write_typed` selects a particular schema version by code hash.

The dynamic object view is `serde_json::Value` with a `__type` discriminator for typed values. MMX-specific presentation of addresses, 128-bit amounts and cryptographic byte arrays belongs to the MMX port; the VNX codec retains their underlying declared wire layout. `Fixed<T,N>` supports arbitrary fixed array lengths. `Map<K,V>` provides sorted unique keys and arbitrary-key serde as arrays of pairs. VNI sets generate `BTreeSet`.

### VNI and generated code

The parser supports packages/imports, extern wire codes, typedefs, topics, abstract declarations, structs, classes, inheritance, modules/interfaces, annotations, aliases, constants, defaults, arrays, templates, enums, pointers and const/async methods. The compiler resolves references and rejects duplicate fields, invalid template arities, incompatible inheritance, non-orderable map/set keys and recursive layouts. It computes the original CRC-64/XZ type and structural hashes.

Generated bindings include Rust data types, enum labels and unknown enum values, fixed arrays, defaults, compatible schema metadata, request/return types, typed clients and service dispatchers. Polymorphic pointers use the dynamic value view. Async VNI methods additionally expose an RPC ticket; service methods run in runtime workers or a serialized module actor. This does not require an async executor.

Generate the current MMX bindings, including dependency schemas:

```sh
cargo run --locked -p vnx-codegen --bin vnxrustcodegen -- \
  --output /tmp/mmx_generated.rs --schema /tmp/mmx_schema.json \
  ../mmx-node/vnx-base/interface ../mmx-node/vnx-base/modules \
  ../mmx-node/vnx-addons/interface ../mmx-node/vnx-addons/modules \
  ../mmx-node/interface ../mmx-node/modules
```

Applications can depend on `vnx-codegen` in `[build-dependencies]`, call `vnx_codegen::generate_to(inputs, output)` from `build.rs`, and include the resulting module tree at their crate root. The library documentation includes a complete build-script example. Generated structs implement `vnx::VnxType` for object conversion and binary serialization. Typed clients accept either a local `runtime::Client` or remote `net::NetworkClient` through the common `RpcClient` trait.

### Logging, configuration and process control

Log messages retain `[Module] ERROR: ...`, `WARN`, `INFO`, `DEBUG` and numeric-level formatting, stderr output, warning/error colors, module/global display levels, pause/resume, grep, bounded history, error history, journal replay, log-file prefixes and date-based rotation. Messages can be published on `vnx.log_out`; subscriber callbacks run outside logger locks. `Process` connects configuration, authentication, the logger and runtime, and owns module shutdown and system-topic handling.

`Config` loads the existing MMX configuration trees: parent directories, namespace JSON files, bare config files, relaxed VNX JSON, `+` array appends, CLI aliases/overrides and protected namespaces. File writes use temporary files, fsync and atomic replacement. Process configuration RPCs honor access controls. Startup config printing is available through `ProcessOptions::print_config`.

Relaxed VNX strings follow native `read_json` escapes: `\t`, `\r`, `\n`, `\b` and `\f` decode as control bytes; other escaped bytes are copied literally. For example, `\u0041` reads as the text `u0041`. Standard JSON APIs remain available separately through `serde_json`.

`Terminal` supplies the familiar line commands: `debug`, `errors`, `grep`, `journal`, `config`, `module`, `topic`, `call`/`exec` and `quit`. Applications provide stdin and a writer, and may add their preferred line editor. Signal handlers and raw-terminal editing are application concerns.

### Messaging, RPC and transport

The runtime provides bounded pipes with blocking/reject/drop policies, hierarchical topic subscriptions, retained sample replay, sequence numbers and timestamps, named services, synchronous/asynchronous RPC, request IDs, deadlines, cancellation, errors and method permissions. Module tasks, timers and actor-service callbacks execute serially. Worker pools provide bounded submission, completion tickets, barriers/wait sets and panic handling.

`net::Server` and `net::NetworkClient` use native VNX Request/Return/Sample/Flow/Proxy messages over TCP or Unix sockets, without additional framing. They support greeting/connection identity, login, remote sessions, topic subscriptions/replay, heartbeat handling and clean shutdown. Services and topics are exported explicitly. Authentication accepts native client password hashes and existing VNX role/user configuration, creates random session IDs, and checks revocation through the process adapter.

`NetworkClient::connect` enables automatic reconnect after a connection drops. Recovery uses bounded exponential backoff and fresh binary readers/writers, authenticates again, and restores live topic subscriptions before reporting the connection ready. Successful logins retain the username and native SHA256 password hash in memory for recovery; plaintext passwords are not retained. Failed authentication during recovery never exposes an anonymous connection as ready. Subscription handles and their queues survive disconnects; subscriptions requesting replay receive the new peer's retained samples.

`wait_on_connect` waits for recovery to finish. New RPC calls wait for a ready connection within their request timeout. Calls already in flight fail on disconnect and are **never resent**, because a remote transaction may have executed before its response was lost. Publications are not queued or replayed. `connect_with_options` accepts `ClientOptions` to configure or disable retries and heartbeat expiry; `connection_status` distinguishes disconnected, recovering, ready and closed states. Initial connection errors are returned to the caller. Explicit close, or dropping the last client handle, stops recovery and wakes blocked calls and subscriptions.

Defaults are a 100 ms retry delay doubling to 5 seconds, a 1 second reconnect dial timeout, heartbeats every 10 seconds and connection expiry after 30 seconds without incoming bytes. A zero heartbeat interval or timeout disables that behavior. `Process::network_options` also releases authenticated server sessions when a login is replaced or its socket closes; custom authorities can provide `ServerOptions::session_releaser`.

Configured forwarding/tunnels and compression are not implemented. VNX Recorder/Player files and UI/add-on implementations (HTTP/WebAPI, OpenCL, Qt) are outside this MMX core dependency layer. Declaring their VNI schemas is supported; their application behavior is not generated. This repository supplies the VNX foundation, not the MMX node/VM/consensus implementation.

## Conformance checks

Checked-in public fixtures were produced by the optional C++ oracle in `tests/cpp`. Ordinary Rust tests read and re-encode them byte for byte, test malformed/truncated input and resource limits, and exercise configuration, logging, authentication, process control, collections, modules, queues, RPC and sockets. Generated-code tests compile and execute a separate consuming crate.

To verify against the MMX source tree:

```sh
MMX_SOURCE_DIR=../mmx-node cargo test --workspace --locked
```

This compiles all six current schema directories, compares 638 available native C++ type/code hash pairs, compiles the complete generated Rust binding crate, and exercises MMX transactions, maps, sets, enums, aliases, optional fields and polymorphic requests. It currently produces 644 type descriptors. C++ source metadata is read only; it is not invoked as a generator or linked into the generated crate.

An optional, independent C++ check requires an existing MMX CMake build with `mmx_iface` and `vnx_base`:

```sh
python3 tests/cpp/check.py --mmx-root ../mmx-node
```

That check builds a disposable oracle, verifies native C++ reading of Rust output, and runs live peer tests, including an actual `vnx::Proxy` connecting, logging in, calling a typed MMX client, importing a topic and surviving heartbeats, plus repeated Rust client reconnections to a native C++ peer. Only the disposable oracle links C++; it is never a Rust dependency.

## Sources and licensing

Apache-2.0; see `LICENSE` and `NOTICE`. The primary compatibility references are `vnx-base` and the original VNI compiler under `VNX/codegen`. The latter is not needed to build or use these crates.
