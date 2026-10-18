use serde_json::json;
use std::io::Read;
use std::net::TcpListener;
#[cfg(unix)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use vnx::net::{ClientOptions, ConnectionStatus, Endpoint, NetworkClient, Server, ServerOptions};
use vnx::runtime::{Methods, OverflowPolicy, Runtime, Session};
use vnx::{Error, Field, Registry, Result, TypeCode};

const METHOD: &str = "lifecycle.Echo.call";
fn registry() -> Result<Registry> {
    let mut out = Registry::new();
    for (name, field) in [(METHOD, "value"), ("lifecycle.Echo.call.return", "_ret_0")] {
        out.register(TypeCode {
            name: name.into(),
            type_hash: vnx::hash64(name),
            code_hash: vnx::hash64(format!("{name}:string")),
            fields: vec![Field {
                name: field.into(),
                code: vec![32],
                is_extended: true,
                ..Default::default()
            }],
            is_class: true,
            is_method: name == METHOD,
            is_return: name != METHOD,
            ..Default::default()
        })?;
    }
    Ok(out)
}
fn options() -> ClientOptions {
    ClientOptions {
        reconnect: true,
        reconnect_initial_delay: Duration::from_millis(20),
        reconnect_max_delay: Duration::from_millis(80),
        connect_timeout: Duration::from_millis(100),
        heartbeat_interval: Duration::ZERO,
        heartbeat_timeout: Duration::ZERO,
    }
}
fn server_options() -> ServerOptions {
    ServerOptions {
        services: ["echo".to_owned()].into(),
        session: Session::internal(),
        export_topics: ["lifecycle".to_owned()].into(),
        heartbeat_interval: Duration::ZERO,
        ..Default::default()
    }
}
fn connect(endpoint: &Endpoint, options: ClientOptions) -> Result<NetworkClient> {
    NetworkClient::connect_with_options(
        endpoint,
        "echo",
        registry()?,
        Duration::from_secs(3),
        options,
    )
}
#[cfg(unix)]
fn await_status(client: &NetworkClient, expected: ConnectionStatus) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while client.connection_status() != expected {
        assert!(
            Instant::now() < deadline,
            "expected {expected:?}, found {:?}",
            client.connection_status()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
struct Release(mpsc::Sender<()>);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[test]
fn dropping_one_clone_keeps_connection_and_last_clone_closes_peer_socket() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = Endpoint::Tcp(listener.local_addr()?.to_string());
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (closed_tx, closed_rx) = mpsc::channel();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut schemas = registry().unwrap();
        vnx::net::register_types(&mut schemas).unwrap();
        let mut writer = vnx::binary::Writer::new(socket.try_clone().unwrap(), schemas);
        writer.write(&json!({"__type":"vnx.Request","stream":0,"flags":4,"hop_count":0,"src_mac":99,"dst_mac":vnx::hash64("vnx.remote"),"session":0,"request_id":1,"method":{"__type":"vnx.ProxyInterface.on_remote_connect","process_id":99}})).unwrap();
        writer.flush().unwrap();
        accepted_tx.send(()).unwrap();
        let mut buffer = [0_u8; 1024];
        loop {
            match socket.read(&mut buffer) {
                Ok(0) => {
                    let _ = closed_tx.send(true);
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = closed_tx.send(false);
                    break;
                }
            }
        }
    });
    let client = connect(&endpoint, options())?;
    accepted_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    client.wait_on_connect()?;
    let clone = client.clone();
    drop(client);
    clone.publish(
        "lifecycle.alive",
        json!({"__type":"vnx.Exception","what":"last clone owns transport"}),
    )?;
    assert!(matches!(
        closed_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let start = Instant::now();
    drop(clone);
    assert!(
        closed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        "last clone did not shut down its peer socket"
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    peer.join().unwrap();
    Ok(())
}

#[test]
fn pending_calls_and_subscriptions_do_not_keep_last_client_alive() -> Result<()> {
    let runtime = Runtime::new()?;
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let mut methods = Methods::new();
    methods.add(METHOD, None, move |request, _| {
        let _ = started_tx.send(());
        let _ = release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3));
        Ok(request["value"].clone())
    });
    runtime.register("echo", methods)?;
    let mut server = Server::bind(
        "127.0.0.1:0".parse()?,
        runtime.clone(),
        registry()?,
        server_options(),
    )?;
    let client = connect(server.endpoint(), options())?;
    client.wait_on_connect()?;
    let subscription = client.subscribe("lifecycle", 8, OverflowPolicy::Reject, false)?;
    let release = Release(release_tx);
    let pending = client.call_async_value(METHOD, json!({"value":"pending"}))?;
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let start = Instant::now();
    drop(client);
    assert!(matches!(
        pending.wait_timeout(Duration::from_secs(1)),
        Err(Error::Closed)
    ));
    assert!(matches!(
        subscription.recv_timeout(Duration::from_secs(1)),
        Err(Error::Closed)
    ));
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "drop waited for the pending RPC timeout"
    );
    drop(release);
    server.shutdown();
    runtime.shutdown();
    Ok(())
}

#[cfg(unix)]
fn unix_endpoint(label: &str) -> Endpoint {
    Endpoint::Unix(std::env::temp_dir().join(format!(
        "vnx-reconnect-{label}-{}-{}.sock",
        std::process::id(),
        vnx::time::now_micros()
    )))
}
#[cfg(unix)]
#[test]
fn close_interrupts_connect_waiter_and_backoff_and_stays_closed() -> Result<()> {
    let runtime = Runtime::new()?;
    let endpoint = unix_endpoint("close");
    let mut server = Server::bind(
        endpoint.clone(),
        runtime.clone(),
        registry()?,
        server_options(),
    )?;
    let mut config = options();
    config.reconnect_initial_delay = Duration::from_secs(2);
    config.reconnect_max_delay = Duration::from_secs(2);
    let client = connect(&endpoint, config)?;
    client.wait_on_connect()?;
    server.shutdown();
    await_status(&client, ConnectionStatus::Disconnected);
    let waiter = client.clone();
    let (done_tx, done_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let _ = done_tx.send(waiter.wait_on_connect());
    });
    let start = Instant::now();
    client.close();
    assert!(matches!(
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(Error::Closed)
    ));
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "close waited for reconnect backoff"
    );
    thread.join().unwrap();
    let mut replacement = Server::bind(endpoint, runtime.clone(), registry()?, server_options())?;
    assert_eq!(client.connection_status(), ConnectionStatus::Closed);
    assert!(matches!(
        client.call_value(METHOD, json!({"value":"after close"})),
        Err(Error::Closed)
    ));
    assert!(matches!(client.wait_on_connect(), Err(Error::Closed)));
    replacement.shutdown();
    runtime.shutdown();
    Ok(())
}

#[cfg(unix)]
#[test]
fn repeated_recovery_does_not_retransmit_uncertain_pending_requests() -> Result<()> {
    let runtime = Runtime::new()?;
    let endpoint = unix_endpoint("repeat");
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let mut methods = Methods::new();
    methods.add(METHOD, None, move |request, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        if request["value"] == "uncertain" {
            let _ = started_tx.send(());
            let _ = release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3));
        }
        Ok(request["value"].clone())
    });
    runtime.register("echo", methods)?;
    let mut server = Server::bind(
        endpoint.clone(),
        runtime.clone(),
        registry()?,
        server_options(),
    )?;
    let client = connect(&endpoint, options())?;
    client.wait_on_connect()?;
    for cycle in 0..3 {
        let release = Release(release_tx.clone());
        let pending = client.call_async_value(METHOD, json!({"value":"uncertain"}))?;
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let stop = std::thread::spawn(move || {
            server.shutdown();
        });
        assert!(matches!(
            pending.wait_timeout(Duration::from_secs(1)),
            Err(Error::Closed)
        ));
        drop(release);
        stop.join().unwrap();
        server = Server::bind(
            endpoint.clone(),
            runtime.clone(),
            registry()?,
            server_options(),
        )?;
        client.wait_on_connect()?;
        let marker = format!("fresh-{cycle}");
        assert_eq!(
            client.call_value(METHOD, json!({"value":marker}))?["_ret_0"],
            marker
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            (cycle + 1) * 2,
            "an uncertain request was replayed after reconnect"
        );
    }
    client.close();
    server.shutdown();
    runtime.shutdown();
    Ok(())
}

#[test]
fn silent_peer_timeout_reconnects_even_while_outgoing_heartbeats_succeed() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = Endpoint::Tcp(listener.local_addr()?.to_string());
    let (reconnected_tx, reconnected_rx) = mpsc::channel();
    let peer = std::thread::spawn(move || -> Result<()> {
        for generation in 0..2 {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return Err(Error::Timeout);
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            // Winsock inherits the listener's nonblocking mode on accept.
            // This fake peer uses blocking VNX reads and a socket timeout.
            socket.set_nonblocking(false)?;
            socket.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut schemas = registry()?;
            vnx::net::register_types(&mut schemas)?;
            let mut writer = vnx::binary::Writer::new(socket.try_clone()?, schemas);
            writer.write(&json!({"__type":"vnx.Request","stream":0,"flags":4,"hop_count":0,"src_mac":123,"dst_mac":vnx::hash64("vnx.remote"),"session":0,"request_id":1,"method":{"__type":"vnx.ProxyInterface.on_remote_connect","process_id":123+generation}}))?;
            writer.flush()?;
            if generation == 1 {
                let _ = reconnected_tx.send(());
            }
            let mut buffer = [0_u8; 1024];
            // Consume successful client writes, but never send a heartbeat back.
            loop {
                match socket.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(_) => {}
                    // Closing the client can produce a reset instead of EOF
                    // on Windows. Both confirm that the old socket was closed.
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        ) =>
                    {
                        break;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(())
    });
    let mut config = options();
    config.heartbeat_interval = Duration::from_millis(20);
    config.heartbeat_timeout = Duration::from_millis(100);
    let client = connect(&endpoint, config)?;
    client.wait_on_connect()?;
    let recovered = reconnected_rx.recv_timeout(Duration::from_secs(2));
    client.close();
    let peer_result = peer.join().unwrap();
    recovered.expect("the silent half-open connection was not replaced");
    peer_result?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn call_budget_includes_waiting_for_recovery() -> Result<()> {
    let runtime = Runtime::new()?;
    let endpoint = unix_endpoint("budget");
    let mut server = Server::bind(
        endpoint.clone(),
        runtime.clone(),
        registry()?,
        server_options(),
    )?;
    let mut config = options();
    config.reconnect_initial_delay = Duration::from_secs(1);
    config.reconnect_max_delay = Duration::from_secs(1);
    let client = NetworkClient::connect_with_options(
        &endpoint,
        "echo",
        registry()?,
        Duration::from_millis(150),
        config,
    )?;
    client.wait_on_connect()?;
    server.shutdown();
    await_status(&client, ConnectionStatus::Disconnected);
    let start = Instant::now();
    assert!(client
        .call_value(METHOD, json!({"value":"must not outlive its budget"}))
        .is_err());
    let elapsed = start.elapsed();
    client.close();
    runtime.shutdown();
    assert!(
        elapsed < Duration::from_millis(500),
        "150ms call waited {elapsed:?} for a one-second reconnect backoff"
    );
    Ok(())
}

#[test]
fn reconnect_disabled_closes_pending_handles_and_never_dials_replacement_peer() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = Endpoint::Tcp(listener.local_addr()?.to_string());
    let (attempts_tx, attempts_rx) = mpsc::channel();
    let peer = std::thread::spawn(move || -> Result<()> {
        let (socket, _) = listener.accept()?;
        socket.set_read_timeout(Some(Duration::from_secs(3)))?;
        let mut schemas = registry()?;
        vnx::net::register_types(&mut schemas)?;
        let mut input = vnx::binary::Reader::new(socket.try_clone()?, schemas.clone());
        let mut output = vnx::binary::Writer::new(socket.try_clone()?, schemas);
        output.write(&json!({"__type":"vnx.Request","stream":0,"flags":4,"hop_count":0,"src_mac":202,"dst_mac":vnx::hash64("vnx.remote"),"session":0,"request_id":1,"method":{"__type":"vnx.ProxyInterface.on_remote_connect","process_id":202}}))?;
        output.flush()?;
        loop {
            let request = input.read()?;
            match request["method"]["__type"].as_str() {
                Some("vnx.ProxyInterface.enable_export") => {
                    output.write(&json!({"__type":"vnx.Return","stream":0,"flags":0,"hop_count":0,"request_id":request["request_id"],"src_mac":request["src_mac"],"value":{"__type":"vnx.ProxyInterface.enable_export.return"}}))?;
                    output.flush()?;
                }
                Some(METHOD) => break,
                _ => {}
            }
        }
        // Stop the established native peer with the RPC still uncertain. Keep a
        // listener at the same endpoint to observe any automatic replacement dial.
        socket.shutdown(std::net::Shutdown::Both)?;
        drop(input);
        drop(output);
        drop(socket);
        listener.set_nonblocking(true)?;
        let until = Instant::now() + Duration::from_millis(500);
        let mut attempts = 0;
        while Instant::now() < until {
            match listener.accept() {
                Ok((socket, _)) => {
                    attempts += 1;
                    drop(socket);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => return Err(error.into()),
            }
        }
        let _ = attempts_tx.send(attempts);
        Ok(())
    });
    let mut config = options();
    config.reconnect = false;
    let client = connect(&endpoint, config)?;
    assert_eq!(client.wait_on_connect()?, 202);
    let subscription = client.subscribe("lifecycle", 4, OverflowPolicy::Reject, false)?;
    let pending = client.call_async_value(METHOD, json!({"value":"uncertain"}))?;
    assert!(matches!(
        pending.wait_timeout(Duration::from_secs(1)),
        Err(Error::Closed)
    ));
    assert!(matches!(
        subscription.recv_timeout(Duration::from_secs(1)),
        Err(Error::Closed)
    ));
    let start = Instant::now();
    assert!(matches!(client.wait_on_connect(), Err(Error::Closed)));
    assert!(matches!(
        client.call_value(METHOD, json!({"value":"replacement must not receive this"})),
        Err(Error::Closed)
    ));
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "disabled reconnect kept callers waiting"
    );
    assert_eq!(
        attempts_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        0,
        "client dialed the replacement peer despite reconnect=false"
    );
    assert_eq!(client.connection_status(), ConnectionStatus::Disconnected);
    client.close();
    peer.join().unwrap()?;
    Ok(())
}
