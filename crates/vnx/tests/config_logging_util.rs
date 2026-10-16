use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use vnx::{
    config::{parse_args, parse_json, Config},
    logging::{LogMsg, LogOptions, Logger, DEBUG, ERROR, INFO, WARN},
    util,
};

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "vnx-rust-test-{}-{}",
            std::process::id(),
            util::random_u64().unwrap()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, text: &str) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);
impl Buffer {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn relaxed_config_json_preserves_vnx_scalars_and_precision() {
    let value = parse_json(r#"{mode:mainnet,seed:0xFEDCBA9876543210,large:340282366920938463463374607431768211455,ratio:+42, list:[hello, true, null,], nested:{on:false,},}"#).unwrap();
    assert_eq!(value["mode"], "mainnet");
    assert_eq!(value["seed"], json!(0xfedcba9876543210u64));
    assert_eq!(
        value["large"].to_string(),
        "340282366920938463463374607431768211455"
    );
    assert_eq!(value["ratio"], 42);
    assert_eq!(value["list"], json!(["hello", true, null]));
    assert_eq!(parse_json("../default\n").unwrap(), "../default");
    assert_eq!(parse_json(" unquoted text ").unwrap(), "unquotedtext");
    assert!(parse_json("{x:1").is_err());
    assert!(parse_json("[1,2").is_err());
    assert!(parse_json(r#""unterminated"#).is_err());
    assert!(parse_json(&format!("{}0{}", "[".repeat(150), "]".repeat(150))).is_err());
}

#[test]
fn config_namespaces_append_protection_and_public_write_are_atomic() {
    let config = Config::new();
    config
        .set(
            "Node",
            json!({"port":11330,"peers":["one"],"nested":{"foo":1}}),
        )
        .unwrap();
    config.set("Node.peers+", json!(["two", "three"])).unwrap();
    config.set("Node.peers+", json!("four")).unwrap();
    assert_eq!(config.read::<u16>("Node.port").unwrap(), Some(11330));
    assert_eq!(
        config.get("Node").unwrap().unwrap()["peers"],
        json!(["one", "two", "three", "four"])
    );
    assert_eq!(config.get("Node.nested").unwrap(), Some(json!({"foo":1})));
    config.protect("Node.secret").unwrap();
    config.set("Node.secret.key", json!("not printed")).unwrap();
    assert!(config.get_public("Node.secret.key").is_err());
    assert_eq!(
        config.get_object_public("Node").unwrap().get("secret"),
        None
    );
    assert!(!config
        .entries(true)
        .unwrap()
        .contains_key("Node.secret.key"));
    assert!(config
        .set_public("Node", json!({"port":80,"secret":{"key":"wrong"}}))
        .is_err());
    assert_eq!(config.get("Node.port").unwrap(), Some(json!(11330)));
    assert!(config.set("Node.port+", json!(42)).is_err());
    assert_eq!(config.get("missing").unwrap(), None);
}

#[test]
fn config_tree_parent_files_and_order_match_vnx() {
    let temp = TempDir::new();
    temp.file("base/Node.json", r#"{"port":1,"peers":["base"]}"#);
    temp.file("base/vnx/protected_config+", r#"["secret"]"#);
    temp.file("child/parent", "../base\n");
    temp.file("child/Node.json", r#"{"port":2,"peers+":["child"]}"#);
    temp.file("child/Node/cache_size", "100\n");
    temp.file("child/lists.lst", "first\nsecond\n");
    temp.file("child/text.txt", "spaces and\nnewlines\n");
    temp.file("child/records.jary", "{a:1}\n\n{a:2}\n");
    temp.file("child/.ignored.json", "invalid");
    temp.file("child/ignored.bin", "invalid");
    let config = Config::new();
    config.load_tree(temp.0.join("child")).unwrap();
    assert_eq!(
        config.get("Node").unwrap(),
        Some(json!({"port":2,"peers":["base","child"],"cache_size":100}))
    );
    assert_eq!(
        config.get("lists").unwrap(),
        Some(json!(["first", "second"]))
    );
    assert_eq!(
        config.get("text").unwrap(),
        Some(json!("spaces and\nnewlines\n"))
    );
    assert_eq!(
        config.get("records").unwrap(),
        Some(json!([{"a":1},{"a":2}]))
    );
    temp.file("base/parent", "../child");
    assert!(config
        .load_tree(temp.0.join("child"))
        .unwrap_err()
        .to_string()
        .contains("cycle"));
}

#[test]
fn cli_groups_aliases_negative_json_and_layers() {
    let temp = TempDir::new();
    temp.file("Node.json", r#"{"port":1}"#);
    temp.file("vnx/protected_config+", r#"["secret"]"#);
    let args = vec![
        "start".into(),
        "-c".into(),
        temp.0.to_string_lossy().into_owned(),
        "--Node.port=99".into(),
        "--Node.peers".into(),
        "one".into(),
        "two".into(),
        "--offset".into(),
        "-1".into(),
        "-d".into(),
        "--name".into(),
        "a b".into(),
    ];
    let config = Config::new();
    assert_eq!(
        config.apply_args(&args, &BTreeMap::new()).unwrap(),
        ["start"]
    );
    assert_eq!(config.get("$1").unwrap(), Some(json!("start")));
    assert_eq!(config.get("Node.port").unwrap(), Some(json!(99)));
    assert_eq!(
        config.get("Node.peers").unwrap(),
        Some(json!(["one", "two"]))
    );
    assert_eq!(config.get("offset").unwrap(), Some(json!(-1)));
    assert_eq!(config.get("log_level").unwrap(), Some(json!(4)));
    assert_eq!(config.get("name").unwrap(), Some(json!("a b")));
    assert!(config.is_protected("vnx.authentication").unwrap());
    assert!(config.is_protected("secret.key").unwrap());
    assert_eq!(
        parse_args(
            &[
                "--foo".into(),
                "[1,2]".into(),
                "--".into(),
                "--ignored".into()
            ],
            &BTreeMap::new()
        )
        .unwrap(),
        vec![("foo".into(), vec![json!([1, 2])])]
    );
}

#[test]
fn logs_have_vnx_layout_ansi_and_visibility_rules() {
    for (level, label) in [
        (ERROR, "ERROR: "),
        (WARN, "WARN: "),
        (INFO, "INFO: "),
        (DEBUG, "DEBUG: "),
        (5, "L5"),
    ] {
        let message = LogMsg::new("mmx_node", "Node", level, "hello 世界");
        assert_eq!(message.get_output(), format!("[Node] {label}hello 世界"));
        let expected = match level {
            ERROR => "\x1b[31m[Node] ERROR: hello 世界\n\x1b[m".into(),
            WARN => "\x1b[33m[Node] WARN: hello 世界\n\x1b[m".into(),
            _ => format!("[Node] {label}hello 世界\n"),
        };
        assert_eq!(message.terminal_output(true), expected);
    }
    let buffer = Buffer::default();
    let logger = Logger::with_writer(
        "mmx_node",
        LogOptions {
            log_level: WARN,
            use_ansi_escape: false,
            ..Default::default()
        },
        buffer.clone(),
    )
    .unwrap();
    // Module display level expands global visibility.
    logger.module("Node").info("visible").unwrap();
    logger.module("Node").debug("hidden").unwrap();
    logger
        .module("Router")
        .display_level(DEBUG)
        .debug("visible debug")
        .unwrap();
    assert_eq!(
        buffer.text(),
        "[Node] INFO: visible\n[Router] DEBUG: visible debug\n"
    );
    logger.grep_log("Node").unwrap();
    logger
        .module("Node")
        .info("grep hides INFO above global")
        .unwrap();
    logger.module("Node").warn("matched").unwrap();
    logger.module("Router").warn("hidden by grep").unwrap();
    assert!(buffer.text().ends_with("[Node] WARN: matched\n"));
    logger.pause_log().unwrap();
    logger.pause_log().unwrap();
    logger.resume_log().unwrap();
    logger.module("Node").error("paused").unwrap();
    assert!(!buffer.text().contains("paused"));
    logger.resume_log().unwrap();
    logger.module("Node").error("resumed").unwrap();
    assert!(buffer.text().contains("resumed"));
    assert_eq!(logger.history().unwrap().len(), 8);
}

#[test]
fn logging_retains_history_publishes_outside_lock_and_rotates_files() {
    let temp = TempDir::new();
    let buffer = Buffer::default();
    let options = LogOptions {
        use_ansi_escape: false,
        log_history_size: 2,
        error_history_size: 1,
        log_file_name: "mmx".into(),
        log_file_path: temp.0.clone(),
        log_file_name_suffix: "_%Y_%m_%d.txt".into(),
        ..Default::default()
    };
    let logger = Logger::with_writer("mmx_node", options, buffer.clone()).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let output = seen.clone();
    let callback_logger = logger.clone();
    logger
        .set_publisher(Arc::new(move |message| {
            output.lock().unwrap().push(message.message.clone());
            let _ = callback_logger.history().unwrap();
        }))
        .unwrap();
    logger.module("Node").error("first").unwrap();
    logger.module("Node").error("second").unwrap();
    logger.module("Node").info("last").unwrap();
    assert_eq!(*seen.lock().unwrap(), ["first", "second", "last"]);
    assert_eq!(
        logger
            .history()
            .unwrap()
            .iter()
            .map(|m| m.message.as_str())
            .collect::<Vec<_>>(),
        ["second", "last"]
    );
    assert_eq!(
        logger
            .error_history()
            .unwrap()
            .iter()
            .map(|m| m.message.as_str())
            .collect::<Vec<_>>(),
        ["second"]
    );
    buffer.clear();
    logger.show_error_log().unwrap();
    assert_eq!(buffer.text(), "[Node] ERROR: second\n");
    buffer.clear();
    logger.journal_log("last").unwrap();
    assert_eq!(buffer.text(), "[Node] INFO: last\n");
    // External receipt must not publish back to the same runtime topic.
    logger
        .receive(LogMsg::new("other", "Other", WARN, "received"))
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), 3);
    // Force different day independent of machine timezone.
    let mut historical = LogMsg::new("mmx_node", "Node", WARN, "rotated warning");
    historical.time = 0;
    logger.receive_at(historical, 946_771_200_000_000).unwrap();
    let logs = fs::read_dir(&temp.0)
        .unwrap()
        .map(|e| fs::read_to_string(e.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(logs.len(), 2);
    let combined = logs.join("");
    assert!(combined.contains("[@] Started mmx_node"));
    assert!(combined.contains("[Node] ERROR: first"));
    assert!(!combined.contains("[Node] INFO: last"));
    assert!(combined.contains("[Node] WARN: rotated warning"));
    logger.shutdown().unwrap();
    logger.clear_publisher().unwrap(); // release callback's deliberately cloned logger
}

#[test]
fn file_io_is_durable_bounded_and_preserves_json_integer_precision() {
    let temp = TempDir::new();
    let path = temp.0.join("value.json");
    let value: Value =
        serde_json::from_str(r#"{"n":340282366920938463463374607431768211455,"text":"世界"}"#)
            .unwrap();
    util::write_json_file(&path, &value).unwrap();
    assert_eq!(util::read_json_file::<Value>(&path).unwrap(), value);
    assert!(util::read_file_bounded(&path, 1).is_err());
    util::atomic_write(&path, b"replacement").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"replacement");
    assert_eq!(fs::read_dir(&temp.0).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn hex_random_sha_and_clock_calculations_match_vnx() {
    assert_eq!(
        util::to_hex_string(&[0x01, 0xab, 0xff], false, false),
        "01ABFF"
    );
    assert_eq!(
        util::to_hex_string(&[0x01, 0xab, 0xff], true, true),
        "ffab01"
    );
    assert_eq!(util::from_hex_string("0x01aBFF").unwrap(), [1, 171, 255]);
    for invalid in ["0", "xy", "a🦀"] {
        assert!(util::from_hex_string(invalid).is_err());
    }
    assert_eq!(
        util::to_hex_string(&util::sha256(b"abc"), false, true),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_ne!(
        util::random_bytes::<32>().unwrap(),
        util::random_bytes::<32>().unwrap()
    );
    assert_ne!(util::random_nonzero_u64().unwrap(), 0);
    let sync = util::TimeSync {
        offset: 50,
        ..Default::default()
    };
    assert_eq!(sync.time_at(100), 150);
    let mut control = util::TimeControl {
        is_realtime: false,
        time_start: 100,
        time_offset: 10,
        time_speed: 2.,
        ..Default::default()
    };
    assert_eq!(control.time_at(120), 150);
    control.is_paused = true;
    assert_eq!(control.time_at(999), 110);
    control.is_realtime = true;
    assert_eq!(control.time_at(999), 999);
}

#[test]
fn native_file_io_restores_schema_and_json_auto_detection() {
    use vnx::{code::CODE_UINT64, Field, Registry, TypeCode};
    let temp = TempDir::new();
    let mut registry = Registry::new();
    registry
        .register(TypeCode {
            name: "test.Stored".into(),
            type_hash: 10,
            code_hash: 11,
            is_class: true,
            fields: vec![Field {
                name: "height".into(),
                code: vec![CODE_UINT64],
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
    let value = json!({"__type":"test.Stored","height":18446744073709551615u64});
    let path = temp.0.join("state.dat");
    util::write_to_file(&path, &value, &registry).unwrap();
    assert!(fs::read(&path)
        .unwrap()
        .starts_with(&[0xff, 0xff, 0x13, 0x37]));
    let (actual, restored) = util::read_from_file_with_registry(&path).unwrap();
    assert_eq!(actual, value);
    assert_eq!(restored.by_name("test.Stored").unwrap().code_hash, 11);
    util::write_to_file(temp.0.join("state.json"), &value, &registry).unwrap();
    assert_eq!(
        util::read_from_file(temp.0.join("state.json")).unwrap(),
        value
    );
    // Content sniffing also accepts JSON copied to a legacy .dat filename.
    fs::copy(temp.0.join("state.json"), temp.0.join("legacy.dat")).unwrap();
    assert_eq!(
        util::read_from_file(temp.0.join("legacy.dat")).unwrap(),
        value
    );
}

#[test]
fn module_log_levels_follow_vnx_namespace() {
    let config = Config::new();
    config.set("log_level", json!(WARN)).unwrap();
    config.set("Node.vnx_log_level", json!(DEBUG)).unwrap();
    let buffer = Buffer::default();
    let logger = Logger::with_writer(
        "mmx_node",
        LogOptions {
            log_level: WARN,
            use_ansi_escape: false,
            ..Default::default()
        },
        buffer.clone(),
    )
    .unwrap();
    logger
        .module_from_config("Node", &config)
        .unwrap()
        .debug("visible")
        .unwrap();
    logger
        .module_from_config("Router", &config)
        .unwrap()
        .info("hidden")
        .unwrap();
    assert_eq!(buffer.text(), "[Node] DEBUG: visible\n");
}

#[test]
fn dates_and_endian_helpers_match_vnx() {
    assert_eq!(
        util::get_date_string(true, 0).unwrap(),
        "1970-01-01_00:00:00"
    );
    assert_eq!(
        util::get_date_string_ex("%Y_%m_%d", true, 946684800).unwrap(),
        "2000_01_01"
    );
    assert!(util::get_date_string_ex("%Q", true, 0).is_err());
    assert_eq!(util::flip_bytes(0x12345678u32), 0x78563412);
    assert_eq!(util::flip_bytes(util::flip_bytes(123.456f64)), 123.456);
    assert_eq!(util::is_little_endian(), u16::from_ne_bytes([1, 0]) == 1);
}

#[test]
fn custom_log_publisher_panics_are_contained() {
    let logger = Logger::with_writer(
        "test",
        LogOptions {
            use_ansi_escape: false,
            ..Default::default()
        },
        Buffer::default(),
    )
    .unwrap();
    logger
        .set_publisher(Arc::new(|_| panic!("test publisher failure")))
        .unwrap();
    assert!(logger
        .module("Node")
        .info("retained before callback")
        .unwrap_err()
        .to_string()
        .contains("publisher panicked"));
    assert_eq!(
        logger.history().unwrap()[0].message,
        "retained before callback"
    );
    logger.clear_publisher().unwrap();
    logger.module("Node").info("continues").unwrap();
}
