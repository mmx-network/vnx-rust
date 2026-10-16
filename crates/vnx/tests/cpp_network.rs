//! Optional live C++ peer tests. Set VNX_CPP_NETWORK_ORACLE to the compiled
//! tests/cpp/network.cpp executable; normal builds never require a C++ toolchain.
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;
use vnx::net::{NetworkClient, Server, ServerOptions};
use vnx::runtime::{Methods, Runtime, Session};
use vnx::{Field, Registry, Result, TypeCode};
fn registry() -> Result<Registry> {
    let mut registry = Registry::new();
    registry.register(TypeCode {
        name: "mmx.Node.get_height.return".into(),
        type_hash: 0x5fb0ff1e11add033,
        code_hash: 0xbcdb9cb9df806e51,
        is_class: true,
        is_return: true,
        fields: vec![Field {
            name: "_ret_0".into(),
            code: vec![3],
            data_size: 4,
            ..Default::default()
        }],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "mmx.Node.get_height".into(),
        type_hash: 0x55f0d4ef7a117716,
        code_hash: 0x335c00dfad151370,
        is_class: true,
        is_method: true,
        return_type: 0xbcdb9cb9df806e51,
        permission: "mmx.permission_e.PUBLIC".into(),
        ..Default::default()
    })?;
    Ok(registry)
}
#[test]
#[ignore = "requires tests/cpp/network.cpp compiled and VNX_CPP_NETWORK_ORACLE"]
fn cpp_client_calls_native_rust_service() -> Result<()> {
    let oracle = std::env::var("VNX_CPP_NETWORK_ORACLE").map_err(|_| {
        vnx::error::invalid("set VNX_CPP_NETWORK_ORACLE to the optional C++ peer executable")
    })?;
    let runtime = Runtime::new()?;
    let mut methods = Methods::new();
    methods.add(
        "mmx.Node.get_height",
        Some("mmx.permission_e.PUBLIC".into()),
        |_, _| Ok(json!(5050001)),
    );
    runtime.register("Node", methods)?;
    let options = ServerOptions {
        services: ["Node".into()].into(),
        session: Session::new("peer", ["mmx.permission_e.PUBLIC".into()]),
        ..Default::default()
    };
    let mut server = Server::bind(
        "tcp://127.0.0.1:0".parse()?,
        runtime.clone(),
        registry()?,
        options,
    )?;
    let output = Command::new(oracle)
        .arg("client")
        .arg(server.endpoint().to_string())
        .output()?;
    assert!(
        output.status.success(),
        "C++ client: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.shutdown();
    runtime.shutdown();
    Ok(())
}
#[test]
#[ignore = "requires tests/cpp/network.cpp compiled and VNX_CPP_NETWORK_ORACLE"]
fn rust_client_calls_native_cpp_service() -> Result<()> {
    let oracle = std::env::var("VNX_CPP_NETWORK_ORACLE").map_err(|_| {
        vnx::error::invalid("set VNX_CPP_NETWORK_ORACLE to the optional C++ peer executable")
    })?;
    let mut child = Command::new(oracle)
        .arg("server")
        .stdout(Stdio::piped())
        .spawn()?;
    let mut endpoint = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut endpoint)?;
    let client = NetworkClient::connect(
        &endpoint.trim().parse()?,
        "Node",
        registry()?,
        Duration::from_secs(5),
    )?;
    for expected in [5050001, 5050002] {
        assert_eq!(
            client.call_value("mmx.Node.get_height", json!({}))?["_ret_0"],
            expected
        );
    }
    assert!(child.wait()?.success());
    client.close();
    Ok(())
}

#[test]
#[ignore = "requires tests/cpp/network.cpp compiled and VNX_CPP_NETWORK_ORACLE"]
fn rust_client_reconnects_to_native_cpp_streams() -> Result<()> {
    struct Peer(std::process::Child);
    impl Drop for Peer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let oracle = std::env::var("VNX_CPP_NETWORK_ORACLE")
        .map_err(|_| vnx::error::invalid("set VNX_CPP_NETWORK_ORACLE"))?;
    let mut peer = Peer(
        Command::new(oracle)
            .arg("reconnect")
            .stdout(Stdio::piped())
            .spawn()?,
    );
    let mut endpoint = String::new();
    BufReader::new(peer.0.stdout.take().unwrap()).read_line(&mut endpoint)?;
    let client = NetworkClient::connect(
        &endpoint.trim().parse()?,
        "Node",
        registry()?,
        Duration::from_secs(3),
    )?;
    for round in 0..3 {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while client.wait_on_connect().map_err(|error| {
            vnx::error::invalid(format!("connection identity in round {round}: {error}"))
        })? != 5051000 + round
        {
            assert!(std::time::Instant::now() < deadline, "reconnect stalled");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            client
                .call_value("mmx.Node.get_height", json!({}))
                .map_err(|error| {
                    vnx::error::invalid(format!("native RPC in round {round}: {error}"))
                })?["_ret_0"],
            5050001 + round
        );
    }
    assert!(peer.0.wait()?.success());
    client.close();
    Ok(())
}

#[test]
#[ignore = "requires tests/cpp/network.cpp compiled and VNX_CPP_NETWORK_ORACLE"]
fn native_cpp_proxy_bootstrap_login_subscriptions_and_heartbeats() -> Result<()> {
    use std::sync::Arc;
    use vnx::auth::{final_hash, password_hash, AuthenticationServer, User};
    let oracle = std::env::var("VNX_CPP_NETWORK_ORACLE")
        .map_err(|_| vnx::error::invalid("set VNX_CPP_NETWORK_ORACLE"))?;
    let runtime = Runtime::new()?;
    let mut methods = Methods::new();
    methods.add(
        "mmx.Node.get_height",
        Some("mmx.permission_e.PUBLIC".into()),
        |_, _| Ok(json!(5050001)),
    );
    runtime.register("Node", methods)?;
    runtime.publish(
        "test.visible.child",
        json!({"__type":"vnx.Exception","what":"proxy sample"}),
        true,
    )?;
    let auth = AuthenticationServer::new();
    auth.add_user(User {
        name: "operator".into(),
        hashed_password: final_hash(&password_hash("correct password")),
        permissions: [
            "mmx.permission_e.PUBLIC".into(),
            "vnx.permission_e.VIEW".into(),
        ]
        .into(),
        ..Default::default()
    })?;
    let validator = auth.clone();
    let login = auth;
    let options = ServerOptions {
        services: ["Node".into()].into(),
        export_topics: ["test.visible".into()].into(),
        heartbeat_interval: Duration::from_millis(20),
        authenticator: Some(Arc::new(move |user, password| {
            login.authenticate_hash(user, password)
        })),
        session_validator: Some(Arc::new(move |id| validator.get_session(id))),
        ..Default::default()
    };
    let mut server = Server::bind(
        "127.0.0.1:0".parse()?,
        runtime.clone(),
        registry()?,
        options,
    )?;
    let output = Command::new(oracle)
        .arg("proxy")
        .arg(server.endpoint().to_string())
        .output()?;
    assert!(
        output.status.success(),
        "native Proxy status {:?}: {}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    server.shutdown();
    runtime.shutdown();
    Ok(())
}
