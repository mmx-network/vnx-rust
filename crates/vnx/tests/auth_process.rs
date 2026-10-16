use serde_json::json;
use std::collections::BTreeSet;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vnx::{
    auth::{final_hash, password_hash, AuthenticationServer, User},
    config::Config,
    logging::{DEBUG, INFO},
    process::{Process, ProcessOptions},
    runtime::{OverflowPolicy, Session},
};

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);
impl Buffer {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
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
fn auth_config() -> Config {
    let config = Config::new();
    config.set("vnx.authentication",json!({"users":[{"name":"admin","access_roles":["ADMIN"]},{"name":"viewer","access_roles":["VIEWER"]}],"passwd":{"admin":final_hash(&password_hash("public-test-password")),"viewer":final_hash(&password_hash("public-viewer-password"))},"permissions":{"ADMIN":["!PROTECTED_CONFIG","mmx.permission_e.SPENDING"],"REMOTE":["VIEW","PUBLISH","mmx.permission_e.PUBLIC"]}})).unwrap();
    config
}
fn test_process(buffer: Buffer) -> Process {
    let config = auth_config();
    config.set("use_ansi_escape", json!(false)).unwrap();
    Process::with_writer(
        "mmx_node",
        &[],
        ProcessOptions {
            config,
            ..Default::default()
        },
        buffer,
    )
    .unwrap()
}
fn until(mut check: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(2);
    while !check() {
        assert!(Instant::now() < end, "condition timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn native_password_hashes_roles_and_sessions() {
    assert_eq!(
        password_hash("public-test-password"),
        "4c66c496fe3eb899ef929aef151ef7ab4972a09294f97158c5fa005ee6e54dd5"
    );
    assert_eq!(
        final_hash(&password_hash("public-test-password")),
        "8900d23b58a2e90a3cfd469e9e1529d1abe4c58454cb2a81522f1c02364f86be"
    );
    let auth = AuthenticationServer::from_config(&auth_config()).unwrap();
    let session = auth
        .authenticate_password("admin", "public-test-password")
        .unwrap();
    assert!(!session.is_anonymous());
    assert_ne!(session.id, 0);
    assert_eq!(session.user, "admin");
    assert!(session.has_permission("vnx.permission_e.VIEW"));
    assert!(session.has_permission("mmx.permission_e.SPENDING"));
    assert!(!session.has_permission("vnx.permission_e.PROTECTED_CONFIG"));
    assert!(!session.has_permission("arbitrary.permission"));
    assert!(auth.is_allowed(session.id, "VIEW").unwrap());
    assert!(!auth.is_allowed(session.id, "PROTECTED_CONFIG").unwrap());
    let failed = auth.login("admin", "wrong", "REMOTE").unwrap();
    assert!(failed.is_anonymous());
    assert!(failed.has_permission("vnx.permission_e.PUBLISH"));
    assert!(!failed.has_permission("mmx.permission_e.SPENDING"));
    assert!(auth.authenticate_hash("admin", "wrong").is_err());
    assert!(auth.authenticate_hash("unknown", "wrong").is_err());
    auth.logout(session.id).unwrap();
    assert!(auth.get_session(session.id).unwrap().is_none());
    assert!(!auth.is_allowed(session.id, "VIEW").unwrap());
    let anonymous = auth.login_anonymous("UNAUTHENTICATED").unwrap();
    assert_ne!(anonymous.id, 0);
    assert!(anonymous.has_permission("vnx.permission_e.PROXY_EXPORT"));
    assert!(!anonymous.has_permission("vnx.permission_e.REQUEST"));
    assert!(auth.is_allowed(0, "anything").unwrap());
    assert!(!format!("{:?}", auth.get_user("admin").unwrap().unwrap()).contains("8900d23b"));
}
#[test]
fn crypto_session_ids_are_unique_and_builtins_are_not_admin_wildcards() {
    let auth = AuthenticationServer::new();
    let mut ids = BTreeSet::new();
    for _ in 0..100 {
        assert!(ids.insert(auth.login_anonymous("DEFAULT").unwrap().id));
    }
    let admin = auth.login_anonymous("ADMIN").unwrap();
    assert!(admin.has_permission("vnx.permission_e.LOCAL"));
    assert!(!admin.has_permission("mmx.permission_e.SPENDING"));
    auth.revoke_all().unwrap();
    assert_eq!(auth.session_count().unwrap(), 0);
    auth.add_user(User {
        name: "disabled".into(),
        access_roles: vec!["ADMIN".into()],
        ..Default::default()
    })
    .unwrap();
    assert!(auth.authenticate_password("disabled", "").is_err());
}
#[test]
fn process_bootstraps_logs_config_rpc_and_shutdown() {
    let output = Buffer::default();
    let process = test_process(output.clone());
    assert!(process.do_run());
    assert_eq!(process.name(), "mmx_node");
    assert_ne!(process.id(), 0);
    let subscription = process
        .runtime()
        .subscribe("vnx.log_out", 16, OverflowPolicy::Reject, false)
        .unwrap();
    process
        .module_logger("Node")
        .unwrap()
        .info("started")
        .unwrap();
    let log = subscription.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(log.value["message"], "started");
    assert_eq!(log.value["__type"], "vnx.LogMsg");
    // Origin marker prevents the Process topic subscription from printing twice.
    std::thread::sleep(Duration::from_millis(10));
    assert_eq!(output.text(), "[Node] INFO: started\n");
    process.runtime().publish("vnx.log_out",json!({"time":0,"level":INFO,"display_level":INFO,"process":"other","module":"Router","message":"remote log"}),false).unwrap();
    until(|| output.text().contains("[Router] INFO: remote log"));
    let client = process.runtime().client("vnx.process");
    assert_eq!(
        client.call_value("get_name", json!({})).unwrap(),
        "mmx_node"
    );
    assert_eq!(
        client.call_value("get_process_info", json!({})).unwrap()["id"],
        process.id()
    );
    client
        .call_value("set_debug", json!({"level":DEBUG}))
        .unwrap();
    assert_eq!(process.logger().options().unwrap().log_level, DEBUG);
    assert!(!process.wait_timeout(Duration::from_millis(1)).unwrap());
    client.call_value("trigger_shutdown", json!({})).unwrap();
    assert!(!process.do_run());
    process.wait().unwrap();
    assert!(process.runtime().is_shutdown());
    assert!(process.is_stopped());
    process.shutdown().unwrap();
}
#[test]
fn process_permissions_protect_secrets_and_failed_login_resets_grants() {
    let process = test_process(Buffer::default());
    process
        .config()
        .set("secret.key", json!("sensitive"))
        .unwrap();
    process.config().protect("secret").unwrap();
    let mut client = process.runtime().client("vnx.process");
    client.set_session(Session::anonymous());
    assert!(client.call_value("get_name", json!({})).is_err());
    let admin = process
        .auth()
        .authenticate_password("admin", "public-test-password")
        .unwrap();
    client.set_session(admin);
    assert_eq!(
        client.call_value("get_name", json!({})).unwrap(),
        "mmx_node"
    );
    assert!(client
        .call_value("get_global_config", json!({"key":"secret.key"}))
        .is_err());
    assert!(client
        .call_value(
            "set_global_config",
            json!({"key":"secret.key","value":"changed"})
        )
        .is_err());
    assert_eq!(
        process.config().get("secret.key").unwrap(),
        Some(json!("sensitive"))
    );
    let trusted = process.runtime().client("vnx.process");
    assert_eq!(
        trusted
            .call_value("get_global_config", json!({"key":"secret.key"}))
            .unwrap(),
        "sensitive"
    );
    client
        .call_value(
            "set_global_config",
            json!({"key":"log_level","value":DEBUG}),
        )
        .unwrap();
    assert_eq!(process.logger().options().unwrap().log_level, DEBUG);
    process.shutdown().unwrap();
}
#[test]
fn optional_log_subscriber_overflow_is_diagnostic_and_shutdown_drains_modules() {
    let process = test_process(Buffer::default());
    let _full = process
        .runtime()
        .subscribe("vnx.log_out", 1, OverflowPolicy::Reject, false)
        .unwrap();
    process
        .module_logger("Node")
        .unwrap()
        .info("first")
        .unwrap();
    process
        .module_logger("Node")
        .unwrap()
        .info("second")
        .unwrap();
    assert!(process
        .diagnostics()
        .iter()
        .any(|message| message.contains("log publication failed")));
    let module = vnx::runtime::Module::new("owned", 16).unwrap();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = count.clone();
    module
        .post(move || {
            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
        .unwrap();
    process.own_module(module).unwrap();
    process.shutdown().unwrap();
    assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(process.start_module("late", 1).is_err());
}

#[test]
fn process_network_policy_uses_same_authority_and_revokes_sessions() {
    let process = test_process(Buffer::default());
    let options = process.network_options(["vnx.process".to_owned()]).unwrap();
    assert_eq!(options.services, BTreeSet::from(["vnx.process".to_owned()]));
    assert!(options
        .session
        .has_permission("vnx.permission_e.PROXY_INTERNAL"));
    assert!(!options
        .session
        .has_permission("vnx.permission_e.READ_CONFIG"));
    let authenticate = options.authenticator.unwrap();
    let validate = options.session_validator.unwrap();
    let session = authenticate("viewer", &password_hash("public-viewer-password")).unwrap();
    assert_eq!(validate(session.id).unwrap().unwrap().user, "viewer");
    assert!(authenticate("admin", &password_hash("incorrect")).is_err());
    process.auth().logout(session.id).unwrap();
    assert!(validate(session.id).unwrap().is_none());
    let qualified = process
        .runtime()
        .client("vnx.process")
        .call_value("vnx.Process.get_name", json!({}))
        .unwrap();
    assert_eq!(
        qualified,
        json!({"__type":"vnx.Process.get_name.return","_ret_0":"mmx_node"})
    );
    process.shutdown().unwrap();
}
