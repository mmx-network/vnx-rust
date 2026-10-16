use serde_json::json;
use std::io::{self, Cursor, Write};
use std::sync::{Arc, Mutex};
use vnx::{
    config::Config,
    logging::{DEBUG, WARN},
    process::{Process, ProcessOptions},
    runtime::Methods,
    terminal::Terminal,
    Field, Registry, TypeCode,
};
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
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn setup() -> (Process, Terminal, Buffer) {
    let config = Config::new();
    config.set("use_ansi_escape", json!(false)).unwrap();
    let logs = Buffer::default();
    let process = Process::with_writer(
        "test_node",
        &[],
        ProcessOptions {
            config,
            ..Default::default()
        },
        logs.clone(),
    )
    .unwrap();
    let terminal = Terminal::new(process.clone(), Registry::new());
    (process, terminal, logs)
}
#[test]
fn terminal_debug_history_filtering_and_pause_use_process_logger() {
    let (process, terminal, logs) = setup();
    assert!(terminal.execute("help").unwrap().contains("journal"));
    assert_eq!(
        terminal.execute("debug").unwrap(),
        "[Terminal] INFO: process.set_debug(4)\n"
    );
    assert_eq!(process.logger().options().unwrap().log_level, DEBUG);
    assert!(terminal.execute("debug nope").is_err());
    terminal.execute("debug 2").unwrap();
    assert_eq!(process.logger().options().unwrap().log_level, WARN);
    terminal.execute("grep Node").unwrap();
    process
        .module_logger("Router")
        .unwrap()
        .warn("hidden")
        .unwrap();
    process
        .module_logger("Node")
        .unwrap()
        .warn("visible")
        .unwrap();
    assert_eq!(logs.text(), "[Node] WARN: visible\n");
    terminal.execute("").unwrap();
    process
        .module_logger("Router")
        .unwrap()
        .error("error history")
        .unwrap();
    logs.clear();
    terminal.execute("errors").unwrap();
    assert_eq!(logs.text(), "[Router] ERROR: error history\n");
    logs.clear();
    terminal.execute("journal visible").unwrap();
    assert_eq!(logs.text(), "[Node] WARN: visible\n");
    terminal.execute("pause").unwrap();
    process
        .module_logger("Node")
        .unwrap()
        .error("visible paused")
        .unwrap();
    assert!(!logs.text().contains("paused"));
    terminal.execute("resume").unwrap();
    assert!(terminal.execute("unknown-command").is_err());
    process.shutdown().unwrap();
}
#[test]
fn terminal_config_omits_protected_values_and_lists_services_topics() {
    let (process, terminal, _) = setup();
    process.config().set("Node.port", json!(11330)).unwrap();
    process
        .config()
        .set("secret.key", json!("never print this"))
        .unwrap();
    process.config().protect("secret").unwrap();
    let text = terminal.execute("config").unwrap();
    assert!(text.contains("Node.port = 11330"));
    assert!(!text.contains("secret"));
    assert!(!text.contains("never print this"));
    assert_eq!(
        terminal.execute("config Node.port").unwrap(),
        "Node.port = 11330\n"
    );
    process.runtime().register("Node", Methods::new()).unwrap();
    process
        .runtime()
        .publish("mmx.block", json!(1), true)
        .unwrap();
    assert_eq!(terminal.execute("module Node").unwrap(), "Node\n");
    assert_eq!(terminal.execute("topic mmx").unwrap(), "mmx.block\n");
    process.shutdown().unwrap();
}
#[test]
fn terminal_generic_calls_parse_named_positional_and_default_arguments() {
    let (process, _, _) = setup();
    let mut methods = Methods::new();
    methods.add("test.Echo.add",None,|args,_|Ok(json!({"__type":"test.Echo.add.return","_ret_0":args["a"].as_i64().unwrap()+args["b"].as_i64().unwrap()})));
    methods.add("test.Echo.reflect", None, |args, _| {
        Ok(args["value"].clone())
    });
    process.runtime().register("Echo", methods).unwrap();
    let mut registry = Registry::new();
    registry
        .register(TypeCode {
            name: "test.Echo.add".into(),
            type_hash: 1,
            code_hash: 2,
            is_method: true,
            is_class: true,
            fields: vec![
                Field {
                    name: "a".into(),
                    code: vec![7],
                    ..Default::default()
                },
                Field {
                    name: "b".into(),
                    code: vec![7],
                    value: "10".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        })
        .unwrap();
    registry
        .register(TypeCode {
            name: "test.Echo.reflect".into(),
            type_hash: 3,
            code_hash: 4,
            is_method: true,
            is_class: true,
            fields: vec![Field {
                name: "value".into(),
                code: vec![17],
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
    let terminal = Terminal::new(process.clone(), registry);
    assert!(terminal
        .execute("exec Echo")
        .unwrap()
        .contains("test.Echo.add"));
    assert_eq!(terminal.execute("call Echo.add 3 4").unwrap(), "7\n");
    assert_eq!(terminal.execute("exec Echo.add 3").unwrap(), "13\n");
    assert_eq!(terminal.execute("call Echo.add {a:5,b:7}").unwrap(), "12\n");
    assert_eq!(
        terminal
            .execute("call Echo.reflect \"hello world\"")
            .unwrap(),
        "\"hello world\"\n"
    );
    assert!(terminal
        .execute("call Echo.reflect [1, {key:2}]")
        .unwrap()
        .contains("key"));
    assert!(terminal.execute("call Echo.add 1 2 3").is_err());
    assert!(terminal.execute("call Echo.reflect [1").is_err());
    assert!(terminal.execute("call Echo.unknown").is_err());
    process.shutdown().unwrap();
}
#[test]
fn terminal_run_recovers_from_bad_commands_and_quit_requests_shutdown() {
    let (process, terminal, _) = setup();
    let mut output = Vec::new();
    terminal
        .run(
            Cursor::new(b"bad-command\nhelp\nquit\ndebug 1\n"),
            &mut output,
        )
        .unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("[Terminal] WARN:"));
    assert!(output.contains("Invalid command"));
    assert!(!process.do_run());
    assert_ne!(process.logger().options().unwrap().log_level, 1);
    process.wait().unwrap();
}
#[test]
fn terminal_eof_leaves_process_running() {
    let (process, terminal, _) = setup();
    terminal
        .run(Cursor::new(Vec::<u8>::new()), Vec::new())
        .unwrap();
    assert!(process.do_run());
    process.shutdown().unwrap();
}
