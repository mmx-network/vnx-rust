use serde_json::{json, Value};
use std::time::Duration;
use vnx::net::{Endpoint, NetworkClient, Server, ServerOptions};
use vnx::runtime::{Methods, OverflowPolicy, Runtime, Session};
use vnx::{Error, Field, Registry, Result, TypeCode};
fn registry() -> Result<Registry> {
    let mut registry = Registry::new();
    for (name, fields) in [
        (
            "example.Echo.call",
            vec![Field {
                name: "value".into(),
                code: vec![32],
                is_extended: true,
                ..Default::default()
            }],
        ),
        (
            "example.Echo.call.return",
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
            code_hash: vnx::hash64(format!("{name}:string")),
            is_class: true,
            is_method: !name.ends_with(".return"),
            is_return: name.ends_with(".return"),
            fields,
            ..Default::default()
        })?;
    }
    Ok(registry)
}
fn runtime() -> Result<Runtime> {
    let runtime = Runtime::new()?;
    let mut methods = Methods::new();
    methods.add("example.Echo.call", Some("echo".into()), |value, _| {
        if value["value"] == "error" {
            Err(Error::Remote("requested failure".into()))
        } else {
            Ok(Value::String(format!(
                "echo:{}",
                value["value"].as_str().unwrap()
            )))
        }
    });
    runtime.register("echo", methods)?;
    Ok(runtime)
}
fn exercise(endpoint: Endpoint) -> Result<()> {
    let runtime = runtime()?;
    let options = ServerOptions {
        services: ["echo".into()].into(),
        session: Session::new("peer", ["echo".into(), "vnx.permission_e.PUBLISH".into()]),
        publish_topics: ["test.allowed".into()].into(),
        ..Default::default()
    };
    let mut server = Server::bind(endpoint, runtime.clone(), registry()?, options)?;
    let client = NetworkClient::connect(
        server.endpoint(),
        "echo",
        registry()?,
        Duration::from_secs(1),
    )?;
    assert_ne!(client.wait_on_connect()?, 0);
    let a = client.call_async_value("example.Echo.call", json!({"value":"a"}))?;
    let b = client.call_async_value("example.Echo.call", json!({"value":"b"}))?;
    assert_ne!(a.id(), b.id());
    assert_eq!(
        a.wait()?,
        json!({"__type":"example.Echo.call.return","_ret_0":"echo:a"})
    );
    assert_eq!(b.wait()?["_ret_0"], "echo:b");
    assert!(
        matches!(client.call_value("example.Echo.call",json!({"value":"error"})),Err(Error::Remote(s)) if s=="requested failure")
    );
    let sub = runtime.subscribe("test", 4, OverflowPolicy::Reject, false)?;
    client.publish(
        "test.allowed.child",
        json!({"__type":"vnx.Exception","what":"sample"}),
    )?;
    assert_eq!(
        sub.recv_timeout(Duration::from_secs(1))?.value["what"],
        "sample"
    );
    let denied = NetworkClient::connect(
        server.endpoint(),
        "private",
        registry()?,
        Duration::from_secs(1),
    )?;
    assert!(matches!(
        denied.call_value("example.Echo.call", json!({"value":"x"})),
        Err(Error::Remote(_))
    ));
    let stopped = std::time::Instant::now();
    server.shutdown();
    assert!(
        stopped.elapsed() < Duration::from_secs(5),
        "heartbeat timers delayed shutdown"
    );
    client.close();
    assert!(client.wait_on_connect().is_err());
    assert!(client
        .call_value("example.Echo.call", json!({"value":"x"}))
        .is_err());
    runtime.shutdown();
    Ok(())
}
#[test]
fn tcp_vnx_streaming_ids_errors_samples_and_shutdown() -> Result<()> {
    exercise("tcp://127.0.0.1:0".parse()?)
}
#[test]
fn server_receive_limits_reject_large_public_samples_before_queueing() -> Result<()> {
    let runtime = runtime()?;
    let sub = runtime.subscribe("test", 4, OverflowPolicy::Reject, false)?;
    let mut server = Server::bind(
        "tcp://127.0.0.1:0".parse()?,
        runtime.clone(),
        registry()?,
        ServerOptions {
            services: ["echo".into()].into(),
            session: Session::new("peer", ["echo".into(), "vnx.permission_e.PUBLISH".into()]),
            publish_topics: ["test".into()].into(),
            receive_limits: vnx::binary::Limits {
                max_string_bytes: 1024,
                max_value_bytes: 8192,
                ..Default::default()
            },
            ..Default::default()
        },
    )?;
    let client = NetworkClient::connect(
        server.endpoint(),
        "echo",
        registry()?,
        Duration::from_secs(1),
    )?;
    assert_eq!(
        client.call_value("example.Echo.call", json!({"value":"small"}))?["_ret_0"],
        "echo:small"
    );
    client.publish(
        "test",
        json!({"__type":"vnx.Exception","what":"x".repeat(4096)}),
    )?;
    // A subsequent request observes the closed stream. No decoded large value
    // ever reaches the runtime's otherwise item-bounded subscription queue.
    assert!(client
        .call_value("example.Echo.call", json!({"value":"after"}))
        .is_err());
    assert!(sub.try_recv()?.is_none());
    server.shutdown();
    runtime.shutdown();
    Ok(())
}
#[cfg(unix)]
#[test]
fn unix_vnx_streaming_and_socket_cleanup() -> Result<()> {
    let path = std::env::temp_dir().join(format!(
        "vnx-rust-{}-{}.sock",
        std::process::id(),
        vnx::time::now_micros()
    ));
    exercise(Endpoint::Unix(path.clone()))?;
    assert!(!path.exists());
    Ok(())
}
#[test]
fn exported_service_still_requires_method_permission() -> Result<()> {
    let runtime = runtime()?;
    let options = ServerOptions {
        services: ["echo".into()].into(),
        ..Default::default()
    };
    let mut server = Server::bind(
        "tcp://127.0.0.1:0".parse()?,
        runtime.clone(),
        registry()?,
        options,
    )?;
    let client = NetworkClient::connect(
        server.endpoint(),
        "echo",
        registry()?,
        Duration::from_secs(1),
    )?;
    assert!(matches!(
        client.call_value("example.Echo.call", json!({"value":"x"})),
        Err(Error::PermissionDenied(_))
    ));
    client.close();
    server.shutdown();
    runtime.shutdown();
    Ok(())
}

#[test]
fn native_proxy_login_subscriptions_replay_and_revocation() -> Result<()> {
    use std::sync::Arc;
    use vnx::auth::{final_hash, password_hash, AuthenticationServer, User};
    let runtime = runtime()?;
    let authority = AuthenticationServer::new();
    authority.add_user(User {
        name: "operator".into(),
        hashed_password: final_hash(&password_hash("correct password")),
        permissions: ["echo".into(), "vnx.permission_e.VIEW".into()].into(),
        ..Default::default()
    })?;
    let login = authority.clone();
    let validator = authority.clone();
    let options = ServerOptions {
        services: ["echo".into()].into(),
        export_topics: ["test.visible".into()].into(),
        authenticator: Some(Arc::new(move |name, password| {
            login.authenticate_hash(name, password)
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
    let client = NetworkClient::connect(
        server.endpoint(),
        "echo",
        registry()?,
        Duration::from_secs(1),
    )?;
    assert!(client.remote_session()?.is_anonymous());
    assert!(matches!(
        client.call_value("example.Echo.call", json!({"value":"before"})),
        Err(Error::PermissionDenied(_))
    ));
    assert!(matches!(
        client.login("operator", "incorrect"),
        Err(Error::PermissionDenied(_))
    ));
    let session = client.login("operator", "correct password")?;
    assert_eq!(session.user, "operator");
    assert_eq!(client.remote_session()?.id, session.id);
    assert_eq!(
        client.call_value("example.Echo.call", json!({"value":"after"}))?["_ret_0"],
        "echo:after"
    );
    assert!(matches!(
        client.subscribe("test.private", 1, OverflowPolicy::Reject, true),
        Err(Error::PermissionDenied(_))
    ));
    runtime.publish(
        "test.visible.child",
        json!({"__type":"vnx.Exception","what":"retained"}),
        true,
    )?;
    let sub = client.subscribe("test.visible", 1, OverflowPolicy::DropOldest, true)?;
    let replay = sub.recv_timeout(Duration::from_secs(1))?;
    assert!(replay.replay);
    assert_eq!(replay.value["what"], "retained");
    runtime.publish(
        "test.visible.child",
        json!({"__type":"vnx.Exception","what":"live"}),
        false,
    )?;
    assert_eq!(
        sub.recv_timeout(Duration::from_secs(1))?.value["what"],
        "live"
    );
    authority.logout(session.id)?;
    assert!(matches!(
        client.call_value("example.Echo.call", json!({"value":"revoked"})),
        Err(Error::PermissionDenied(_))
    ));
    assert!(client.remote_session()?.is_anonymous());
    runtime.publish(
        "test.visible.child",
        json!({"__type":"vnx.Exception","what":"revoked"}),
        false,
    )?;
    assert!(matches!(
        sub.recv_timeout(Duration::from_millis(20)),
        Err(Error::Timeout)
    ));
    sub.close();
    client.close();
    server.shutdown();
    runtime.shutdown();
    Ok(())
}

#[test]
fn native_endpoint_names_remain_accepted() {
    assert_eq!(
        "localhost".parse::<Endpoint>().unwrap(),
        Endpoint::Tcp("localhost:4444".into())
    );
    assert_eq!(
        ":1234".parse::<Endpoint>().unwrap(),
        Endpoint::Tcp("localhost:1234".into())
    );
    assert_eq!(
        "node.sock".parse::<Endpoint>().unwrap(),
        Endpoint::Unix("node.sock".into())
    );
    assert!("".parse::<Endpoint>().is_err());
    assert!("http://node".parse::<Endpoint>().is_err());
}

#[test]
fn canonical_wire_metadata_matches_cpp_request_fixture() -> Result<()> {
    let encoded = include_str!("fixtures/request.hex");
    let bytes = encoded
        .trim()
        .as_bytes()
        .chunks_exact(2)
        .map(|s| u8::from_str_radix(std::str::from_utf8(s).unwrap(), 16).unwrap())
        .collect::<Vec<_>>();
    let (_, registry) = vnx::binary::decode(&bytes)?;
    let mut builtin = Registry::new();
    vnx::net::register_types(&mut builtin)?;
    for native in registry.iter() {
        if let Ok(ours) = builtin.get(native.code_hash) {
            assert_eq!(ours, native, "{}", native.name);
        }
    }
    Ok(())
}
