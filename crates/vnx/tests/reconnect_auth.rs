#![cfg(unix)]
//! Authentication and import lifecycle across native VNX socket recovery.
use serde_json::{json, Value};
use std::io;
use std::time::{Duration, Instant};
use vnx::{
    auth::{final_hash, password_hash, User},
    config::Config,
    net::{ClientOptions, ConnectionStatus, Endpoint, NetworkClient, Server, ServerOptions},
    process::{Process, ProcessOptions},
    runtime::OverflowPolicy,
    Error, Field, Registry, Result, TypeCode,
};

const METHOD: &str = "test.Recovery.identity";
const USER: &str = "operator";
const PASSWORD: &str = "public-reconnect-test-password";

fn registry() -> Result<Registry> {
    let mut registry = Registry::new();
    for (name, fields) in [
        (METHOD, vec![]),
        (
            "test.Recovery.identity.return",
            vec![Field {
                name: "_ret_0".into(),
                code: vec![32],
                is_extended: true,
                ..Default::default()
            }],
        ),
    ] {
        registry.register(TypeCode {
            name: name.into(),
            type_hash: vnx::hash64(name),
            code_hash: vnx::hash64(format!("{name}:reconnect-test")),
            is_class: true,
            is_method: !name.ends_with(".return"),
            is_return: name.ends_with(".return"),
            fields,
            ..Default::default()
        })?;
    }
    Ok(registry)
}
struct Peer {
    process: Process,
    server: Option<Server>,
    endpoint: Endpoint,
    options: ServerOptions,
}
impl Peer {
    fn new() -> Result<Self> {
        let config = Config::new();
        config.set("use_ansi_escape", json!(false))?;
        let process = Process::with_writer(
            "reconnect_test_peer",
            &[],
            ProcessOptions {
                config,
                ..Default::default()
            },
            io::sink(),
        )?;
        process.auth().add_user(user(PASSWORD))?;
        let mut methods = vnx::runtime::Methods::new();
        methods.add(
            METHOD,
            Some("mmx.permission_e.TEST_RECOVERY".into()),
            |_, context| Ok(json!(context.session.user)),
        );
        process.runtime().register("node", methods)?;
        let mut options = process.network_options(["node".to_owned()])?;
        options.export_topics = ["mmx".into()].into();
        options.request_timeout = Duration::from_millis(300);
        options.heartbeat_interval = Duration::from_millis(20);
        let path = std::env::temp_dir().join(format!(
            "vnx-recovery-auth-{}-{}.sock",
            std::process::id(),
            vnx::util::random_u64()?
        ));
        let endpoint = Endpoint::Unix(path);
        let server = Some(Server::bind(
            endpoint.clone(),
            process.runtime().clone(),
            registry()?,
            options.clone(),
        )?);
        Ok(Self {
            process,
            server,
            endpoint,
            options,
        })
    }
    fn stop(&mut self) {
        if let Some(mut server) = self.server.take() {
            server.shutdown();
        }
    }
    fn restart(&mut self) -> Result<()> {
        self.server = Some(Server::bind(
            self.endpoint.clone(),
            self.process.runtime().clone(),
            registry()?,
            self.options.clone(),
        )?);
        Ok(())
    }
    fn client(&self) -> Result<NetworkClient> {
        NetworkClient::connect_with_options(
            &self.endpoint,
            "node",
            registry()?,
            Duration::from_millis(300),
            ClientOptions {
                reconnect: true,
                reconnect_initial_delay: Duration::from_millis(10),
                reconnect_max_delay: Duration::from_millis(40),
                connect_timeout: Duration::from_millis(100),
                heartbeat_interval: Duration::from_millis(20),
                ..Default::default()
            },
        )
    }
    fn publish(&self, text: &str, retain: bool) -> Result<()> {
        self.process.runtime().publish(
            "mmx.block",
            json!({"__type":"vnx.Exception","what":text}),
            retain,
        )
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop();
        let _ = self.process.shutdown();
    }
}
fn user(password: &str) -> User {
    User {
        name: USER.into(),
        hashed_password: final_hash(&password_hash(password)),
        permissions: [
            "vnx.permission_e.VIEW".into(),
            "mmx.permission_e.TEST_RECOVERY".into(),
        ]
        .into(),
        ..Default::default()
    }
}
fn until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !predicate() {
        assert!(Instant::now() < deadline, "reconnect condition timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn expect_identity(client: &NetworkClient) -> Result<()> {
    assert_eq!(client.call_value(METHOD, json!({}))?["_ret_0"], USER);
    Ok(())
}

#[test]
fn successful_login_is_revoked_on_replacement_and_disconnect() -> Result<()> {
    let mut peer = Peer::new()?;
    let client = peer.client()?;
    let first = client.login(USER, PASSWORD)?;
    expect_identity(&client)?;
    assert!(matches!(
        client.login(USER, "wrong password"),
        Err(Error::PermissionDenied(_))
    ));
    assert!(
        peer.process.auth().get_session(first.id)?.is_none(),
        "replaced authenticated session stayed valid"
    );
    assert!(matches!(
        client.call_value(METHOD, json!({})),
        Err(Error::PermissionDenied(_))
    ));
    let anonymous = client.remote_session()?;
    assert!(anonymous.is_anonymous());
    assert!(!anonymous.has_permission("mmx.permission_e.TEST_RECOVERY"));
    let second = client.login(USER, PASSWORD)?;
    assert_ne!(first.id, second.id);
    client.close();
    until(|| {
        peer.process
            .auth()
            .get_session(second.id)
            .unwrap()
            .is_none()
    });
    assert_eq!(
        peer.process.auth().session_count()?,
        1,
        "only the preassigned anonymous session should remain"
    );
    peer.stop();
    Ok(())
}

#[test]
fn reconnect_restores_auth_and_imports_without_duplicate_export_references() -> Result<()> {
    let mut peer = Peer::new()?;
    peer.publish("retained", true)?;
    let client = peer.client()?;
    let initial = client.login(USER, PASSWORD)?;
    let replaying = client.subscribe("mmx.block", 16, OverflowPolicy::Reject, true)?;
    assert_eq!(
        replaying.recv_timeout(Duration::from_secs(1))?.value["what"],
        "retained"
    );
    let live_only = client.subscribe("mmx.block", 16, OverflowPolicy::Reject, false)?;
    peer.stop();
    until(|| client.connection_status() != ConnectionStatus::Ready);
    assert!(peer.process.auth().get_session(initial.id)?.is_none());
    peer.restart()?;
    until(|| client.connection_status() == ConnectionStatus::Ready);
    expect_identity(&client)?;
    let recovered = client.remote_session()?;
    assert_eq!(recovered.user, USER);
    assert_ne!(recovered.id, initial.id);
    let replay = replaying.recv_timeout(Duration::from_secs(1))?;
    assert!(replay.replay);
    assert_eq!(replay.value["what"], "retained");
    assert!(matches!(
        live_only.recv_timeout(Duration::from_millis(30)),
        Err(Error::Timeout)
    ));
    // Removing one local reference must not stop the other's restored import.
    replaying.close();
    expect_identity(&client)?;
    peer.publish("after first close", false)?;
    assert_eq!(
        live_only.recv_timeout(Duration::from_secs(1))?.value["what"],
        "after first close"
    );
    assert!(matches!(
        live_only.recv_timeout(Duration::from_millis(30)),
        Err(Error::Timeout)
    ));
    live_only.close();
    expect_identity(&client)?;
    peer.publish("new retained", true)?;
    // If the last close leaked a remote reference, this new import would reuse
    // its old export and never receive a retained replay.
    let next = client.subscribe("mmx.block", 16, OverflowPolicy::Reject, true)?;
    let sample = next.recv_timeout(Duration::from_secs(1))?;
    assert!(sample.replay);
    assert_eq!(sample.value["what"], "new retained");
    next.close();
    client.close();
    until(|| peer.process.auth().session_count().unwrap() == 1);
    Ok(())
}

#[test]
fn failed_reauthentication_never_announces_ready_or_uses_anonymous_grants() -> Result<()> {
    let mut peer = Peer::new()?;
    let client = peer.client()?;
    let session = client.login(USER, PASSWORD)?;
    let imported = client.subscribe("mmx.block", 16, OverflowPolicy::Reject, true)?;
    peer.stop();
    until(|| client.connection_status() != ConnectionStatus::Ready);
    assert!(peer.process.auth().get_session(session.id)?.is_none());
    peer.process
        .auth()
        .add_user(user("changed server password"))?;
    peer.publish("requires valid login", true)?;
    peer.restart()?;
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        assert_ne!(
            client.connection_status(),
            ConnectionStatus::Ready,
            "failed reauthentication exposed an anonymous connection as ready"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(matches!(
        imported.recv_timeout(Duration::from_millis(20)),
        Err(Error::Timeout)
    ));
    assert_eq!(
        peer.process.auth().session_count()?,
        1,
        "failed recovery attempts leaked sessions"
    );
    // Restoring the expected credential lets the same client/import recover.
    peer.process.auth().add_user(user(PASSWORD))?;
    until(|| client.connection_status() == ConnectionStatus::Ready);
    expect_identity(&client)?;
    let sample = imported.recv_timeout(Duration::from_secs(1))?;
    assert!(sample.replay);
    assert_eq!(sample.value["what"], "requires valid login");
    imported.close();
    client.close();
    Ok(())
}

#[test]
fn overlapping_imports_fan_out_one_sample_per_local_subscriber_after_restart() -> Result<()> {
    let mut peer = Peer::new()?;
    let client = peer.client()?;
    client.login(USER, PASSWORD)?;
    let parent = client.subscribe("mmx", 16, OverflowPolicy::Reject, true)?;
    let child = client.subscribe("mmx.block", 16, OverflowPolicy::Reject, true)?;
    for iteration in 0..2 {
        if iteration == 1 {
            peer.stop();
            until(|| client.connection_status() != ConnectionStatus::Ready);
            peer.restart()?;
            until(|| client.connection_status() == ConnectionStatus::Ready);
        }
        let message = format!("sample {iteration}");
        peer.publish(&message, false)?;
        for subscription in [&parent, &child] {
            assert_eq!(
                subscription.recv_timeout(Duration::from_secs(1))?.value["what"],
                Value::String(message.clone())
            );
            assert!(
                matches!(
                    subscription.recv_timeout(Duration::from_millis(30)),
                    Err(Error::Timeout)
                ),
                "overlapping exports duplicated delivery"
            );
        }
    }
    parent.close();
    child.close();
    client.close();
    Ok(())
}

#[test]
fn delayed_export_validation_cannot_overwrite_a_newer_login() -> Result<()> {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    };
    let mut peer = Peer::new()?;
    peer.stop();
    let mut second = user(PASSWORD);
    second.name = "second operator".into();
    peer.process.auth().add_user(second)?;
    let armed = Arc::new(AtomicBool::new(false));
    let validator_armed = armed.clone();
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    let resume_rx = Mutex::new(resume_rx);
    let authority = peer.process.auth().clone();
    peer.options.session_validator = Some(Arc::new(move |id| {
        let snapshot = authority.get_session(id);
        if std::thread::current().name() == Some("vnx-export")
            && validator_armed.swap(false, Ordering::AcqRel)
        {
            let _ = entered_tx.send(id);
            let _ = resume_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2));
        }
        snapshot
    }));
    peer.restart()?;
    let client = peer.client()?;
    let first = client.login(USER, PASSWORD)?;
    let subscription = client.subscribe("mmx.block", 16, OverflowPolicy::Reject, true)?;
    armed.store(true, Ordering::Release);
    peer.publish("validation race", false)?;
    assert_eq!(
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        first.id
    );
    let second = client.login("second operator", PASSWORD)?;
    assert_ne!(first.id, second.id);
    resume_tx.send(()).unwrap();
    let _ = subscription.recv_timeout(Duration::from_secs(1))?;
    assert_eq!(
        client.remote_session()?.id,
        second.id,
        "stale export validation replaced the newer login"
    );
    subscription.close();
    client.close();
    Ok(())
}

#[test]
fn later_overlapping_subscription_receives_retained_data_without_repeating_old_delivery(
) -> Result<()> {
    let peer = Peer::new()?;
    peer.publish("retained before imports", true)?;
    let client = peer.client()?;
    client.login(USER, PASSWORD)?;
    let parent = client.subscribe("mmx", 16, OverflowPolicy::Reject, true)?;
    assert!(parent.recv_timeout(Duration::from_secs(1))?.replay);
    let later = client.subscribe("mmx.block", 16, OverflowPolicy::Reject, true)?;
    let replay = later.recv_timeout(Duration::from_secs(1))?;
    assert!(replay.replay);
    assert_eq!(replay.value["what"], "retained before imports");
    assert!(
        matches!(
            parent.recv_timeout(Duration::from_millis(30)),
            Err(Error::Timeout)
        ),
        "late subscriber replay repeated delivery to an existing receiver"
    );
    parent.close();
    later.close();
    client.close();
    Ok(())
}

#[test]
fn session_release_callback_panics_are_contained_and_default_session_is_preserved() -> Result<()> {
    use std::sync::{Arc, Mutex};
    let mut peer = Peer::new()?;
    peer.stop();
    let authority = peer.process.auth().clone();
    let released = Arc::new(Mutex::new(Vec::new()));
    let calls = released.clone();
    peer.options.session_releaser = Some(Arc::new(move |id| {
        calls.lock().unwrap().push(id);
        authority.logout(id)?;
        panic!("intentional test session release callback panic");
    }));
    let default_id = peer.options.session.id;
    peer.restart()?;
    let client = peer.client()?;
    let first = client.login(USER, PASSWORD)?;
    assert!(client.login(USER, "wrong replacement").is_err());
    assert!(peer.process.auth().get_session(first.id)?.is_none());
    let next = client.login(USER, PASSWORD)?;
    expect_identity(&client)?;
    client.close();
    until(|| peer.process.auth().get_session(next.id).unwrap().is_none());
    peer.stop();
    assert_eq!(*released.lock().unwrap(), [first.id, next.id]);
    assert!(
        peer.process.auth().get_session(default_id)?.is_some(),
        "preassigned anonymous session was released by a connection"
    );
    Ok(())
}

#[test]
fn explicit_login_during_recovery_is_not_immediately_replaced_by_restore() -> Result<()> {
    let mut peer = Peer::new()?;
    let client = peer.client()?;
    client.login(USER, PASSWORD)?;
    peer.stop();
    until(|| client.connection_status() != ConnectionStatus::Ready);
    let pending = client.clone();
    let login = std::thread::spawn(move || pending.login(USER, PASSWORD));
    // Login waits for the next socket while holding the login mutex. Recovery
    // must reuse that successful exchange instead of performing it twice.
    std::thread::sleep(Duration::from_millis(20));
    peer.restart()?;
    let session = login.join().unwrap()?;
    until(|| client.connection_status() == ConnectionStatus::Ready);
    assert_eq!(
        client.remote_session()?.id,
        session.id,
        "automatic restore replaced the explicit recovered login"
    );
    assert!(peer.process.auth().get_session(session.id)?.is_some());
    expect_identity(&client)?;
    client.close();
    Ok(())
}
