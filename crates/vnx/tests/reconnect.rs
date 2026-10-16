use serde_json::{json, Value};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use vnx::net::{ClientOptions, ConnectionStatus, NetworkClient, Server, ServerOptions};
use vnx::runtime::{OverflowPolicy, Runtime, Session};
use vnx::{Error, Registry, Result};
fn options() -> ClientOptions {
    ClientOptions {
        reconnect_initial_delay: Duration::from_millis(10),
        reconnect_max_delay: Duration::from_millis(30),
        connect_timeout: Duration::from_millis(50),
        heartbeat_interval: Duration::ZERO,
        heartbeat_timeout: Duration::from_secs(1),
        ..Default::default()
    }
}

#[test]
fn native_resend_payload_is_recovered_for_new_overlapping_subscribers() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("tcp://{}", listener.local_addr()?).parse()?;
    let exports = Arc::new(AtomicUsize::new(0));
    let sent = exports.clone();
    let peer = std::thread::spawn(move || -> Result<()> {
        let (stream, _) = listener.accept()?;
        let mut registry = Registry::new();
        vnx::net::register_types(&mut registry)?;
        let mut reader = vnx::binary::Reader::new(stream.try_clone()?, registry.clone());
        let mut writer = vnx::binary::Writer::new(stream, registry);
        while let Ok(request) = reader.read() {
            if request["method"]["__type"] == "vnx.ProxyInterface.on_remote_connect" {
                writer.write(&json!({"__type":"vnx.Request","stream":0,"flags":4,"hop_count":0,"src_mac":99,"dst_mac":vnx::hash64("vnx.remote"),"session":0,"request_id":1,"method":{"__type":"vnx.ProxyInterface.on_remote_connect","process_id":99}}))?;
                writer.flush()?;
            }
            if request["method"]["__type"] == "vnx.ProxyInterface.enable_export" {
                let index = sent.fetch_add(1, Ordering::SeqCst);
                writer.write(&json!({"__type":"vnx.Return","stream":0,"flags":0,"hop_count":0,"src_mac":request["src_mac"],"request_id":request["request_id"],"value":{"__type":"vnx.ProxyInterface.enable_export.return"}}))?;
                // Native BaseProxy omits the payload when replaying a sequence
                // already sent through another overlapping export.
                let value = if index == 0 {
                    json!({"__type":"vnx.Exception","what":"original payload"})
                } else {
                    Value::Null
                };
                writer.write(&json!({"__type":"vnx.Sample","stream":0,"flags":if index==0{2}else{2|32},"hop_count":0,"src_mac":123,"seq_num":7,"recv_time":1,"topic":"mmx.blocks.head","value":value}))?;
                writer.flush()?;
            }
        }
        Ok(())
    });
    let client = NetworkClient::connect_with_options(
        &endpoint,
        "unused",
        Registry::new(),
        Duration::from_secs(1),
        options(),
    )?;
    client.wait_on_connect()?;
    let parent = client.subscribe("mmx", 4, OverflowPolicy::Reject, true)?;
    assert_eq!(
        parent.recv_timeout(Duration::from_secs(1))?.value["what"],
        "original payload"
    );
    let child = client.subscribe("mmx.blocks", 4, OverflowPolicy::Reject, true)?;
    assert_eq!(
        child.recv_timeout(Duration::from_secs(1))?.value["what"],
        "original payload"
    );
    assert!(parent.try_recv()?.is_none());
    let shared = client.subscribe("mmx.blocks", 4, OverflowPolicy::Reject, true)?;
    let replay = shared.recv_timeout(Duration::from_secs(1))?;
    assert!(replay.replay);
    assert_eq!(replay.value["what"], "original payload");
    assert_eq!(
        exports.load(Ordering::SeqCst),
        2,
        "exact local topics must share one remote export"
    );
    client.close();
    peer.join().unwrap()?;
    Ok(())
}

#[test]
fn published_samples_are_never_replayed_after_tcp_recovery() -> Result<()> {
    let runtime = Runtime::new()?;
    let receiver = runtime.subscribe("samples", 8, OverflowPolicy::Reject, false)?;
    let server_options = ServerOptions {
        session: Session::new("peer", ["vnx.permission_e.PUBLISH".into()]),
        publish_topics: ["samples".into()].into(),
        ..Default::default()
    };
    let mut server = Server::bind(
        "127.0.0.1:0".parse()?,
        runtime.clone(),
        Registry::new(),
        server_options.clone(),
    )?;
    let endpoint = server.endpoint().clone();
    let client = NetworkClient::connect_with_options(
        &endpoint,
        "unused",
        Registry::new(),
        Duration::from_secs(1),
        options(),
    )?;
    client.wait_on_connect()?;
    client.publish(
        "samples.test",
        json!({"__type":"vnx.Exception","what":"first"}),
    )?;
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1))?.value["what"],
        "first"
    );
    server.shutdown();
    let until = Instant::now() + Duration::from_secs(1);
    while client.connection_status() == ConnectionStatus::Ready {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut restarted = Server::bind(endpoint, runtime.clone(), Registry::new(), server_options)?;
    client.wait_on_connect()?;
    assert!(matches!(
        receiver.recv_timeout(Duration::from_millis(50)),
        Err(Error::Timeout)
    ));
    client.publish(
        "samples.test",
        json!({"__type":"vnx.Exception","what":"second"}),
    )?;
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1))?.value["what"],
        "second"
    );
    assert!(receiver.try_recv()?.is_none());
    client.close();
    restarted.shutdown();
    runtime.shutdown();
    Ok(())
}
