//! VNX binary streaming over TCP and Unix sockets. No extra framing is used.
use crate::runtime::{RpcCall, Runtime, Session};
use crate::{error::invalid, Error, Field, Registry, Result, TypeCode};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Canonical VNX metadata, transcribed from vnxcppcodegen generated schemas.
/// Inherited fields are flattened exactly as they are in the C++ wire format.
pub fn register_types(registry: &mut Registry) -> Result<()> {
    registry.register(TypeCode {
        name: "vnx.Message".into(),
        type_hash: 0xff12a5be92ae4daf,
        code_hash: 0x67c5a7c806d720dc,
        is_class: true,
        fields: vec![
            Field {
                name: "stream".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "flags".into(),
                code: vec![2],
                is_extended: false,
                data_size: 2,
                ..Default::default()
            },
            Field {
                name: "hop_count".into(),
                code: vec![2],
                is_extended: false,
                data_size: 2,
                ..Default::default()
            },
        ],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.Request".into(),
        type_hash: 0x2838ef2ae539a080,
        code_hash: 0x439e72318518915d,
        is_class: true,
        parents: vec![0x67c5a7c806d720dc],
        alias_map: [("arguments".into(), "method".into())].into(),
        fields: vec![
            Field {
                name: "stream".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "flags".into(),
                code: vec![2],
                is_extended: false,
                data_size: 2,
                ..Default::default()
            },
            Field {
                name: "hop_count".into(),
                code: vec![2],
                is_extended: false,
                data_size: 2,
                ..Default::default()
            },
            Field {
                name: "src_mac".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "dst_mac".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "session".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "request_id".into(),
                code: vec![4],
                is_extended: false,
                data_size: 8,
                ..Default::default()
            },
            Field {
                name: "method".into(),
                code: vec![16],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
        ],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.Return".into(),
        type_hash: 0x941aec28b3f42d7d,
        code_hash: 0x1d5aacfdf12abb93,
        is_class: true,
        parents: vec![0x67c5a7c806d720dc],
        fields: vec![
            Field {
                name: "stream".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "flags".into(),
                code: vec![2],
                is_extended: false,
                data_size: 2,
                ..Default::default()
            },
            Field {
                name: "hop_count".into(),
                code: vec![2],
                is_extended: false,
                data_size: 2,
                ..Default::default()
            },
            Field {
                name: "request_id".into(),
                code: vec![4],
                is_extended: false,
                data_size: 8,
                ..Default::default()
            },
            Field {
                name: "src_mac".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "value".into(),
                code: vec![16],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
        ],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.Sample".into(),
        type_hash: 0x82bffc4ec21d2408,
        code_hash: 0x83bca399538b39fd,
        is_class: true,
        parents: vec![0x67c5a7c806d720dc],
        fields: vec![
            Field {
                name: "stream".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "flags".into(),
                code: vec![2],
                is_extended: false,
                data_size: 2,
                ..Default::default()
            },
            Field {
                name: "hop_count".into(),
                code: vec![2],
                is_extended: false,
                data_size: 2,
                ..Default::default()
            },
            Field {
                name: "src_mac".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "seq_num".into(),
                code: vec![4],
                is_extended: false,
                data_size: 8,
                ..Default::default()
            },
            Field {
                name: "recv_time".into(),
                code: vec![8],
                is_extended: false,
                data_size: 8,
                ..Default::default()
            },
            Field {
                name: "topic".into(),
                code: vec![12, 5],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "value".into(),
                code: vec![16],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
        ],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.FlowMessage".into(),
        type_hash: 0x496b887fb39119c2,
        code_hash: 0x6a4eaa76764b3c3b,
        is_class: true,
        parents: vec![0x67c5a7c806d720dc],
        fields: vec![
            Field {
                name: "stream".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "flags".into(),
                code: vec![2],
                is_extended: false,
                data_size: 2,
                ..Default::default()
            },
            Field {
                name: "hop_count".into(),
                code: vec![2],
                is_extended: false,
                data_size: 2,
                ..Default::default()
            },
            Field {
                name: "src_mac".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "dst_mac".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "flow_code".into(),
                code: vec![2],
                is_extended: false,
                data_size: 2,
                ..Default::default()
            },
        ],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.Session".into(),
        type_hash: 0xe7e70e14f9c9db05,
        code_hash: 0x3834b6a95f3c09ee,
        is_class: true,
        fields: vec![
            Field {
                name: "id".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "user".into(),
                code: vec![32],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "permissions".into(),
                code: vec![12, 32],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "login_time".into(),
                code: vec![8],
                is_extended: false,
                data_size: 8,
                ..Default::default()
            },
        ],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.Exception".into(),
        type_hash: 0x5cd32aac8621de11,
        code_hash: 0xfeaec8ece4910323,
        is_class: true,
        fields: vec![Field {
            name: "what".into(),
            code: vec![32],
            is_extended: true,
            data_size: 0,
            ..Default::default()
        }],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.NoSuchService".into(),
        type_hash: 0x2e35fc01cf168b79,
        code_hash: 0x76a2fa845aacc576,
        is_class: true,
        parents: vec![0xfeaec8ece4910323],
        fields: vec![
            Field {
                name: "what".into(),
                code: vec![32],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "dst_mac".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
        ],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.NoSuchMethod".into(),
        type_hash: 0x6e811245e86c2629,
        code_hash: 0x858eed7d12425930,
        is_class: true,
        parents: vec![0x76a2fa845aacc576, 0xfeaec8ece4910323],
        fields: vec![
            Field {
                name: "what".into(),
                code: vec![32],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "dst_mac".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "method".into(),
                code: vec![32],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
        ],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.PermissionDenied".into(),
        type_hash: 0x3785e66bbcd39a78,
        code_hash: 0x77cb68dd2ed47979,
        is_class: true,
        parents: vec![0xfeaec8ece4910323],
        fields: vec![
            Field {
                name: "what".into(),
                code: vec![32],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "dst_mac".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "method".into(),
                code: vec![32],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "permission".into(),
                code: vec![32],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
        ],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.OverflowException".into(),
        type_hash: 0xba120fc8b6c0e004,
        code_hash: 0xd54e083b4364da6e,
        is_class: true,
        parents: vec![0xfeaec8ece4910323],
        fields: vec![
            Field {
                name: "what".into(),
                code: vec![32],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "dst_mac".into(),
                code: vec![4],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
        ],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.InternalError".into(),
        type_hash: 0xe3a5c8a7193b8c49,
        code_hash: 0x1571a7137b8d21a2,
        is_class: true,
        parents: vec![0xfeaec8ece4910323],
        fields: vec![Field {
            name: "what".into(),
            code: vec![32],
            is_extended: true,
            data_size: 0,
            ..Default::default()
        }],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.ProxyInterface.on_login.return".into(),
        type_hash: 0x546414b99444670a,
        code_hash: 0x34b12b123e1e67de,
        is_class: true,
        is_return: true,
        fields: vec![],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.ProxyInterface.on_login".into(),
        type_hash: 0x5df07f9c3a42bf5a,
        code_hash: 0x122b54c12852ae7a,
        is_class: true,
        is_method: true,
        return_type: 0x34b12b123e1e67de,
        permission: "vnx.permission_e.PROXY_INTERNAL".into(),
        fields: vec![
            Field {
                name: "name".into(),
                code: vec![32],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
            Field {
                name: "password".into(),
                code: vec![32],
                is_extended: true,
                data_size: 0,
                ..Default::default()
            },
        ],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.ProxyInterface.on_remote_login.return".into(),
        type_hash: 0x5d370b9c467678da,
        code_hash: 0xf75bf2934fa5b0cb,
        is_class: true,
        is_return: true,
        fields: vec![],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.ProxyInterface.on_remote_login".into(),
        type_hash: 0xfdef02ccd8b73be4,
        code_hash: 0x5800b78d9cca5c39,
        is_class: true,
        is_method: true,
        return_type: 0xf75bf2934fa5b0cb,
        permission: "vnx.permission_e.PROXY_INTERNAL".into(),
        fields: vec![Field {
            name: "remote_session".into(),
            code: vec![16],
            is_extended: true,
            data_size: 0,
            ..Default::default()
        }],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.ProxyInterface.on_remote_connect.return".into(),
        type_hash: 0x4e4469ad1a22bfc4,
        code_hash: 0x5dfdbc7330d70b86,
        is_class: true,
        is_return: true,
        fields: vec![],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.ProxyInterface.on_remote_connect".into(),
        type_hash: 0x58d0fc825fa3d477,
        code_hash: 0x35a35ba812e6b172,
        is_class: true,
        is_method: true,
        return_type: 0x5dfdbc7330d70b86,
        permission: "vnx.permission_e.PROXY_INTERNAL".into(),
        fields: vec![Field {
            name: "process_id".into(),
            code: vec![4],
            is_extended: true,
            data_size: 0,
            ..Default::default()
        }],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.ProxyInterface.get_session.return".into(),
        type_hash: 0x98cdb953e8233d03,
        code_hash: 0x9f0452f1c6890e72,
        is_class: true,
        is_return: true,
        fields: vec![Field {
            name: "_ret_0".into(),
            code: vec![16],
            is_extended: true,
            data_size: 0,
            ..Default::default()
        }],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.ProxyInterface.get_session".into(),
        type_hash: 0x60cf186ca96171f6,
        code_hash: 0xf1929a65d93c4d72,
        is_class: true,
        is_method: true,
        is_const: true,
        return_type: 0x9f0452f1c6890e72,
        fields: vec![],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.ProxyInterface.enable_export.return".into(),
        type_hash: 0x9718d8624c9536d3,
        code_hash: 0x9a0ad890f37f474a,
        is_class: true,
        is_return: true,
        fields: vec![],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.ProxyInterface.enable_export".into(),
        type_hash: 0xbca6b569c4b73ff7,
        code_hash: 0xf3616ec2935f79c1,
        is_class: true,
        is_method: true,
        return_type: 0x9a0ad890f37f474a,
        permission: "vnx.permission_e.PROXY_EXPORT".into(),
        fields: vec![Field {
            name: "topic_name".into(),
            code: vec![32],
            is_extended: true,
            data_size: 0,
            ..Default::default()
        }],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.ProxyInterface.disable_export.return".into(),
        type_hash: 0x3c5a282d7e151b56,
        code_hash: 0x5ce60c33caf59999,
        is_class: true,
        is_return: true,
        fields: vec![],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.ProxyInterface.disable_export".into(),
        type_hash: 0x255ec29037ffaac4,
        code_hash: 0x644c2598f6f6e281,
        is_class: true,
        is_method: true,
        return_type: 0x5ce60c33caf59999,
        permission: "vnx.permission_e.PROXY_EXPORT".into(),
        fields: vec![Field {
            name: "topic_name".into(),
            code: vec![32],
            is_extended: true,
            data_size: 0,
            ..Default::default()
        }],
        ..Default::default()
    })?;
    registry.register(TypeCode {
        name: "vnx.Heartbeat".into(),
        type_hash: 0x4b8135749ac4e859,
        code_hash: 0xac8a3abc200dc446,
        is_class: true,
        fields: vec![Field {
            name: "source".into(),
            code: vec![32],
            is_extended: true,
            data_size: 0,
            ..Default::default()
        }],
        ..Default::default()
    })?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Tcp(String),
    Unix(PathBuf),
}
impl std::str::FromStr for Endpoint {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        if let Some(value) = value.strip_prefix("tcp://") {
            if value.is_empty() {
                return Err(invalid("empty TCP endpoint"));
            }
            return Ok(Self::Tcp(value.into()));
        }
        if let Some(value) = value.strip_prefix("unix://") {
            if value.is_empty() {
                return Err(invalid("empty Unix endpoint"));
            }
            return Ok(Self::Unix(value.into()));
        }
        if value.is_empty() {
            return Err(invalid("empty endpoint"));
        }
        // Native VNX configuration uses host[:port] or a .sock pathname.
        if value.ends_with(".sock") {
            return Ok(Self::Unix(value.into()));
        }
        if let Some(port) = value.strip_prefix(':') {
            return Ok(Self::Tcp(format!("localhost:{port}")));
        }
        if value.contains("://") {
            return Err(invalid("unknown endpoint scheme"));
        }
        if value.contains(':') {
            Ok(Self::Tcp(value.into()))
        } else {
            Ok(Self::Tcp(format!("{value}:4444")))
        }
    }
}
impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp(value) => write!(f, "tcp://{value}"),
            Self::Unix(value) => write!(f, "unix://{}", value.display()),
        }
    }
}
enum Stream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}
impl Stream {
    fn try_clone(&self) -> std::io::Result<Self> {
        match self {
            Self::Tcp(value) => Ok(Self::Tcp(value.try_clone()?)),
            #[cfg(unix)]
            Self::Unix(value) => Ok(Self::Unix(value.try_clone()?)),
        }
    }
    fn shutdown(&self) {
        match self {
            Self::Tcp(value) => {
                let _ = value.shutdown(Shutdown::Both);
            }
            #[cfg(unix)]
            Self::Unix(value) => {
                let _ = value.shutdown(Shutdown::Both);
            }
        }
    }
    fn set_write_timeout(&self, timeout: Duration) -> std::io::Result<()> {
        match self {
            Self::Tcp(value) => value.set_write_timeout(Some(timeout)),
            #[cfg(unix)]
            Self::Unix(value) => value.set_write_timeout(Some(timeout)),
        }
    }
    fn connect(endpoint: &Endpoint, timeout: Duration) -> Result<Self> {
        match endpoint {
            Endpoint::Tcp(value) => {
                use std::net::ToSocketAddrs;
                let deadline = Instant::now() + timeout;
                let mut last = None;
                for addr in value.to_socket_addrs()? {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(Error::Timeout);
                    }
                    match TcpStream::connect_timeout(&addr, left) {
                        Ok(stream) => {
                            stream.set_nodelay(true)?;
                            return Ok(Self::Tcp(stream));
                        }
                        Err(error) => last = Some(error),
                    }
                }
                Err(last
                    .map(Error::Io)
                    .unwrap_or_else(|| invalid("TCP address resolved to no endpoints")))
            }
            #[cfg(unix)]
            Endpoint::Unix(value) => {
                Ok(Self::Unix(std::os::unix::net::UnixStream::connect(value)?))
            }
            #[cfg(not(unix))]
            Endpoint::Unix(_) => Err(invalid("Unix endpoints are unavailable on this platform")),
        }
    }
}
impl Read for Stream {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(value) => value.read(output),
            #[cfg(unix)]
            Self::Unix(value) => value.read(output),
        }
    }
}
impl Write for Stream {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(value) => value.write(input),
            #[cfg(unix)]
            Self::Unix(value) => value.write(input),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tcp(value) => value.flush(),
            #[cfg(unix)]
            Self::Unix(value) => value.flush(),
        }
    }
}

/// Only named services in this allow-list are exported. Wire session IDs are
/// deliberately ignored: the session is assigned by the accepting application.
pub type SessionValidator = Arc<dyn Fn(u64) -> Result<Option<Session>> + Send + Sync>;
pub type Authenticator = Arc<dyn Fn(&str, &str) -> Result<Session> + Send + Sync>;
#[derive(Clone)]
pub struct ServerOptions {
    pub services: BTreeSet<String>,
    pub session: Session,
    pub request_timeout: Duration,
    pub max_connections: usize,
    /// Shared advertised process identity (0 chooses a cryptographic identity at bind).
    pub process_id: u64,
    pub heartbeat_interval: Duration,
    /// Hierarchical topics allowed for inbound Sample messages. Empty denies all.
    pub publish_topics: BTreeSet<String>,
    /// Topics permitted for peer subscriptions. Empty denies all.
    pub export_topics: BTreeSet<String>,
    /// Accepts the native Proxy on_login SHA256(password), never a wire session ID.
    pub authenticator: Option<Authenticator>,
    pub session_validator: Option<SessionValidator>,
}
impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            services: BTreeSet::new(),
            session: Session::anonymous(),
            request_timeout: Duration::from_secs(30),
            max_connections: 64,
            process_id: 0,
            heartbeat_interval: Duration::from_secs(10),
            publish_topics: BTreeSet::new(),
            export_topics: BTreeSet::new(),
            authenticator: None,
            session_validator: None,
        }
    }
}
struct ServerState {
    stop: AtomicBool,
    next_id: AtomicU64,
    streams: Mutex<BTreeMap<u64, Stream>>,
}
pub struct Server {
    endpoint: Endpoint,
    state: Arc<ServerState>,
    worker: Option<JoinHandle<()>>,
}
enum Listener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixListener),
}
impl Listener {
    fn accept(&self) -> std::io::Result<Stream> {
        match self {
            Self::Tcp(value) => {
                let (stream, _) = value.accept()?;
                stream.set_nodelay(true)?;
                Ok(Stream::Tcp(stream))
            }
            #[cfg(unix)]
            Self::Unix(value) => Ok(Stream::Unix(value.accept()?.0)),
        }
    }
}
impl Server {
    pub fn bind(
        endpoint: Endpoint,
        runtime: Runtime,
        mut registry: Registry,
        mut options: ServerOptions,
    ) -> Result<Self> {
        if options.max_connections == 0 || options.request_timeout.is_zero() {
            return Err(invalid(
                "connection limit and request timeout must be positive",
            ));
        }
        register_types(&mut registry)?;
        if options.process_id == 0 {
            options.process_id = crate::util::random_nonzero_u64()?;
        }
        let (listener, endpoint) = match endpoint {
            Endpoint::Tcp(value) => {
                let listener = TcpListener::bind(value)?;
                listener.set_nonblocking(true)?;
                let endpoint = Endpoint::Tcp(listener.local_addr()?.to_string());
                (Listener::Tcp(listener), endpoint)
            }
            #[cfg(unix)]
            Endpoint::Unix(value) => {
                let listener = std::os::unix::net::UnixListener::bind(&value)?;
                listener.set_nonblocking(true)?;
                (Listener::Unix(listener), Endpoint::Unix(value))
            }
            #[cfg(not(unix))]
            Endpoint::Unix(_) => {
                return Err(invalid("Unix endpoints are unavailable on this platform"))
            }
        };
        let state = Arc::new(ServerState {
            stop: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            streams: Mutex::new(BTreeMap::new()),
        });
        let worker_state = state.clone();
        let worker = thread::Builder::new()
            .name("vnx-listener".into())
            .spawn(move || {
                let mut workers: Vec<JoinHandle<()>> = Vec::new();
                while !worker_state.stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok(stream) => {
                            let mut streams = worker_state.streams.lock().unwrap();
                            if streams.len() >= options.max_connections
                                || worker_state.stop.load(Ordering::Acquire)
                            {
                                stream.shutdown();
                                continue;
                            }
                            let id = worker_state.next_id.fetch_add(1, Ordering::Relaxed);
                            let closer = match stream.try_clone() {
                                Ok(value) => value,
                                Err(_) => continue,
                            };
                            streams.insert(id, closer);
                            drop(streams);
                            let connection_state = worker_state.clone();
                            let runtime = runtime.clone();
                            let registry = registry.clone();
                            let options = options.clone();
                            match thread::Builder::new()
                                .name(format!("vnx-connection-{id}"))
                                .spawn(move || {
                                    let _ = serve_connection(stream, runtime, registry, options);
                                    if let Some(stream) =
                                        connection_state.streams.lock().unwrap().remove(&id)
                                    {
                                        stream.shutdown();
                                    }
                                }) {
                                Ok(worker) => workers.push(worker),
                                Err(_) => {
                                    worker_state.streams.lock().unwrap().remove(&id);
                                }
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::park_timeout(Duration::from_millis(10))
                        }
                        Err(_) => break,
                    }
                    let mut index = 0;
                    while index < workers.len() {
                        if workers[index].is_finished() {
                            let _ = workers.swap_remove(index).join();
                        } else {
                            index += 1;
                        }
                    }
                }
                for stream in worker_state.streams.lock().unwrap().values() {
                    stream.shutdown();
                }
                for worker in workers {
                    let _ = worker.join();
                }
            })?;
        Ok(Self {
            endpoint,
            state,
            worker: Some(worker),
        })
    }
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
    pub fn shutdown(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        for stream in self.state.streams.lock().unwrap().values() {
            stream.shutdown();
        }
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
            #[cfg(unix)]
            if let Endpoint::Unix(path) = &self.endpoint {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn refresh_session(session: &Arc<RwLock<Session>>, validator: &Option<SessionValidator>) {
    if let Some(validate) = validator {
        let id = session.read().unwrap().id;
        if id != 0 {
            let next = catch_unwind(AssertUnwindSafe(|| validate(id)))
                .ok()
                .and_then(std::result::Result::ok)
                .flatten()
                .unwrap_or_else(Session::anonymous);
            *session.write().unwrap() = next;
        }
    }
}
struct Heartbeats {
    stop: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}
impl Heartbeats {
    fn spawn(
        interval: Duration,
        mut send: impl FnMut() -> Result<()> + Send + 'static,
    ) -> Result<Self> {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let ended = stop.clone();
        let worker = thread::Builder::new()
            .name("vnx-heartbeat".into())
            .spawn(move || loop {
                if *ended.0.lock().unwrap() {
                    break;
                }
                if send().is_err() {
                    break;
                }
                let guard = ended.0.lock().unwrap();
                if *guard {
                    break;
                }
                let (guard, _) = ended
                    .1
                    .wait_timeout_while(guard, interval, |stop| !*stop)
                    .unwrap();
                if *guard {
                    break;
                }
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}
impl Drop for Heartbeats {
    fn drop(&mut self) {
        *self.stop.0.lock().unwrap() = true;
        self.stop.1.notify_all();
        if let Some(worker) = self.worker.take() {
            if worker.thread().id() != thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}
fn heartbeat(source: u64, seq: u64) -> Value {
    json!({"__type":"vnx.Sample","stream":0,"flags":0,"hop_count":0,"src_mac":source,"seq_num":seq,"recv_time":crate::time::now_micros(),"topic":"vnx.heartbeat","value":{"__type":"vnx.Heartbeat","source":"vnx-rust"}})
}
struct Export {
    subscription: Arc<crate::runtime::Subscription>,
    worker: Option<JoinHandle<()>>,
    references: usize,
}
impl Drop for Export {
    fn drop(&mut self) {
        self.subscription.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn write_message(writer: &Arc<Mutex<crate::binary::Writer<Stream>>>, value: &Value) -> Result<()> {
    let mut output = writer.lock().unwrap();
    output.write(value)?;
    output.flush()
}
fn proxy_request(source: u64, id: u64, method: &str, args: Value) -> Value {
    let mut method_value = args.as_object().cloned().unwrap_or_default();
    method_value.insert("__type".into(), method.into());
    json!({"__type":"vnx.Request","stream":0,"flags":4,"hop_count":0,"src_mac":source,"dst_mac":crate::hash::hash64("vnx.remote"),"session":0,"request_id":id,"method":method_value})
}
fn serve_connection(
    stream: Stream,
    runtime: Runtime,
    registry: Registry,
    options: ServerOptions,
) -> Result<()> {
    stream.set_write_timeout(options.request_timeout)?;
    let mut input = crate::binary::Reader::new(stream.try_clone()?, registry.clone());
    let writer = Arc::new(Mutex::new(crate::binary::Writer::new(stream, registry)));
    let session = Arc::new(RwLock::new(options.session.clone()));
    let mut exports: BTreeMap<String, Export> = BTreeMap::new();
    let mut greeted = false;
    let mut _heartbeats: Option<Heartbeats> = None;
    let services: BTreeMap<_, _> = options
        .services
        .iter()
        .map(|name| (crate::hash::hash64(name), name))
        .collect();
    loop {
        let request = input.read()?;
        // A peer can announce a new method/value schema on the same stream.
        {
            let mut output = writer.lock().unwrap();
            for schema in input.registry().iter() {
                output.registry_mut().register(schema.clone())?;
            }
        }
        match request["__type"].as_str() {
            Some("vnx.Request") => {
                refresh_session(&session, &options.session_validator);
                let request_id = request["request_id"]
                    .as_u64()
                    .ok_or_else(|| invalid("missing request ID"))?;
                let source = request["src_mac"]
                    .as_u64()
                    .ok_or_else(|| invalid("missing source identity"))?;
                let destination = request["dst_mac"]
                    .as_u64()
                    .ok_or_else(|| invalid("missing destination identity"))?;
                let method = request["method"]["__type"]
                    .as_str()
                    .ok_or_else(|| invalid("missing method type"))?;
                let mut login_reply = None;
                let value = if destination == crate::hash::hash64("vnx.remote") {
                    let result: Result<Value> = (|| match method {
                        "vnx.ProxyInterface.on_remote_connect" => {
                            if !greeted {
                                greeted = true;
                                let greeting = proxy_request(
                                    options.process_id,
                                    1,
                                    "vnx.ProxyInterface.on_remote_connect",
                                    json!({"process_id":options.process_id}),
                                );
                                write_message(&writer, &greeting)?;
                                if !options.heartbeat_interval.is_zero() {
                                    let writer = writer.clone();
                                    let source = options.process_id;
                                    let mut seq = 0;
                                    _heartbeats = Some(Heartbeats::spawn(
                                        options.heartbeat_interval,
                                        move || {
                                            seq += 1;
                                            write_message(&writer, &heartbeat(source, seq))
                                        },
                                    )?);
                                }
                            }
                            Ok(Value::Null)
                        }
                        "vnx.ProxyInterface.on_login" => {
                            let username = request["method"]["name"]
                                .as_str()
                                .ok_or_else(|| invalid("missing login name"))?;
                            let password = request["method"]["password"]
                                .as_str()
                                .ok_or_else(|| invalid("missing password hash"))?;
                            let authenticated = match &options.authenticator {
                                Some(authenticate) => catch_unwind(AssertUnwindSafe(|| {
                                    authenticate(username, password)
                                }))
                                .ok()
                                .and_then(std::result::Result::ok)
                                .unwrap_or_else(|| options.session.clone()),
                                None => options.session.clone(),
                            };
                            *session.write().unwrap() = authenticated.clone();
                            login_reply = Some(proxy_request(
                                destination,
                                request_id,
                                "vnx.ProxyInterface.on_remote_login",
                                json!({"remote_session":{"__type":"vnx.Session","id":authenticated.id,"user":authenticated.user,"permissions":authenticated.permissions,"login_time":authenticated.login_time}}),
                            ));
                            Ok(Value::Null)
                        }
                        "vnx.ProxyInterface.get_session" => {
                            let session = session.read().unwrap().clone();
                            let mut value = serde_json::to_value(session)?;
                            value
                                .as_object_mut()
                                .unwrap()
                                .insert("__type".into(), "vnx.Session".into());
                            Ok(value)
                        }
                        "vnx.ProxyInterface.enable_export" => {
                            let topic = request["method"]["topic_name"]
                                .as_str()
                                .ok_or_else(|| invalid("missing export topic"))?;
                            let allowed = options.export_topics.iter().any(|p| {
                                p.is_empty()
                                    || p == topic
                                    || topic.strip_prefix(p).is_some_and(|s| s.starts_with('.'))
                            });
                            if !allowed
                                || !session
                                    .read()
                                    .unwrap()
                                    .has_permission("vnx.permission_e.VIEW")
                            {
                                return Err(Error::PermissionDenied(format!("subscribe {topic}")));
                            }
                            if let Some(export) = exports.get_mut(topic) {
                                export.references += 1;
                            } else {
                                let subscription = Arc::new(runtime.subscribe(
                                    topic,
                                    256,
                                    crate::runtime::OverflowPolicy::DropOldest,
                                    true,
                                )?);
                                let input = subscription.clone();
                                let output = writer.clone();
                                let granted = session.clone();
                                let validator = options.session_validator.clone();
                                let worker=thread::Builder::new().name("vnx-export".into()).spawn(move||{
                                        while let Ok(sample)=input.recv(){
                                            refresh_session(&granted,&validator);
                                            if !granted.read().unwrap().has_permission("vnx.permission_e.VIEW"){continue;}
                                            let value=json!({"__type":"vnx.Sample","stream":0,"flags":if sample.replay{2}else{0},"hop_count":0,"src_mac":sample.src_mac,"seq_num":sample.seq_num,"recv_time":sample.recv_time,"topic":sample.topic,"value":sample.value});
                                            if write_message(&output,&value).is_err(){break;}
                                        }
                                    })?;
                                exports.insert(
                                    topic.into(),
                                    Export {
                                        subscription,
                                        worker: Some(worker),
                                        references: 1,
                                    },
                                );
                            }
                            Ok(Value::Null)
                        }
                        "vnx.ProxyInterface.disable_export" => {
                            let topic = request["method"]["topic_name"]
                                .as_str()
                                .ok_or_else(|| invalid("missing export topic"))?;
                            if let Some(export) = exports.get_mut(topic) {
                                export.references = export.references.saturating_sub(1);
                                if export.references == 0 {
                                    exports.remove(topic);
                                }
                            }
                            Ok(Value::Null)
                        }
                        _ => Err(Error::Remote(format!("no such method: {method}"))),
                    })();
                    match result {
                        Ok(value) => wrap_return(value, method, input.registry())?,
                        Err(error) => exception_value(&error, destination, method),
                    }
                } else if let Some(service) = services.get(&destination) {
                    let mut client = runtime.client((*service).clone());
                    client.set_timeout(options.request_timeout);
                    client.set_session(session.read().unwrap().clone());
                    match client.call_value(method, request["method"].clone()) {
                        Ok(value) => wrap_return(value, method, input.registry())?,
                        Err(error) => exception_value(&error, destination, method),
                    }
                } else {
                    json!({"__type":"vnx.NoSuchService","what":"service is not exported","dst_mac":destination})
                };
                if request["flags"].as_u64().unwrap_or(0) & 4 == 0 {
                    let response = json!({"__type":"vnx.Return","stream":request["stream"],"flags":0,"hop_count":0,"request_id":request_id,"src_mac":source,"value":value});
                    write_message(&writer, &response)?;
                }
                if let Some(reply) = login_reply {
                    write_message(&writer, &reply)?;
                }
            }
            Some("vnx.Sample") => {
                refresh_session(&session, &options.session_validator);
                let topic = request["topic"]
                    .as_str()
                    .ok_or_else(|| invalid("missing sample topic"))?;
                if topic == "vnx.heartbeat" {
                    continue;
                }
                let allowed = options.publish_topics.iter().any(|p| {
                    p.is_empty()
                        || p == topic
                        || topic.strip_prefix(p).is_some_and(|s| s.starts_with('.'))
                });
                if !allowed
                    || !session
                        .read()
                        .unwrap()
                        .has_permission("vnx.permission_e.PUBLISH")
                {
                    return Err(Error::PermissionDenied(format!("publish {topic}")));
                }
                runtime.publish(topic, request["value"].clone(), true)?;
            }
            Some("vnx.FlowMessage") => { /* Requests open return routes directly, as in BaseProxy::process(Request). */
            }
            _ => return Err(invalid("expected VNX Request, Sample or FlowMessage")),
        }
    }
}

fn wrap_return(value: Value, method: &str, registry: &Registry) -> Result<Value> {
    if value
        .get("__type")
        .and_then(Value::as_str)
        .is_some_and(|name| name == format!("{method}.return"))
    {
        return Ok(value);
    }
    let typename = format!("{method}.return");
    let schema = registry.by_name(&typename)?;
    let mut result = serde_json::Map::new();
    result.insert("__type".into(), Value::String(typename));
    if !schema.fields.is_empty() {
        result.insert("_ret_0".into(), value);
    }
    Ok(Value::Object(result))
}
fn exception_value(error: &Error, destination: u64, method: &str) -> Value {
    let typename = match error {
        Error::PermissionDenied(_) => "vnx.PermissionDenied",
        Error::Overflow => "vnx.OverflowException",
        Error::Remote(msg) if msg.starts_with("no such method:") => "vnx.NoSuchMethod",
        Error::Remote(msg) if msg.starts_with("no such service:") => "vnx.NoSuchService",
        _ => "vnx.InternalError",
    };
    json!({"__type":typename,"what":error.to_string(),"dst_mac":destination,"method":method,"permission":match error { Error::PermissionDenied(p)=>p.as_str(),_=>"" }})
}
fn response_value(value: Value, registry: &Registry) -> Result<Value> {
    if let Some(typename) = value["__type"].as_str() {
        let is_exception = typename == "vnx.Exception"
            || registry
                .by_name(typename)
                .is_ok_and(|t| t.parents.contains(&0xfeaec8ece4910323));
        if is_exception
            || matches!(
                typename,
                "vnx.PermissionDenied"
                    | "vnx.NoSuchService"
                    | "vnx.NoSuchMethod"
                    | "vnx.OverflowException"
                    | "vnx.InternalError"
            )
        {
            let message = value["what"]
                .as_str()
                .unwrap_or("remote exception")
                .to_owned();
            return Err(match typename {
                "vnx.PermissionDenied" => Error::PermissionDenied(message),
                "vnx.OverflowException" => Error::Overflow,
                _ => Error::Remote(message),
            });
        }
    }
    Ok(value)
}
struct SampleReceiver {
    topic: String,
    pipe: crate::runtime::Pipe<crate::runtime::Sample>,
    missed: Arc<AtomicU64>,
    replay: bool,
}
struct NetworkInner {
    writer: Mutex<crate::binary::Writer<Stream>>,
    pending: Mutex<BTreeMap<u64, crate::runtime::Pipe<Result<Value>>>>,
    closed: AtomicBool,
    samples: Mutex<BTreeMap<u64, SampleReceiver>>,
    remote_session: Mutex<Option<Session>>,
    session_changed: Condvar,
    login_lock: Mutex<()>,
    remote_process_id: Mutex<Option<u64>>,
    connection_changed: Condvar,
    heartbeats: Mutex<Option<Heartbeats>>,
    next_id: AtomicU64,
    source: u64,
    service: u64,
    timeout: Duration,
    closer: Stream,
    reader: Mutex<Option<JoinHandle<()>>>,
}
impl NetworkInner {
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.closer.shutdown();
        self.session_changed.notify_all();
        *self.remote_process_id.lock().unwrap() = None;
        self.connection_changed.notify_all();
        self.heartbeats.lock().unwrap().take();
        for receiver in std::mem::take(&mut *self.samples.lock().unwrap()).into_values() {
            receiver.pipe.close();
        }
        for pipe in std::mem::take(&mut *self.pending.lock().unwrap()).into_values() {
            pipe.close();
        }
    }
}
impl Drop for NetworkInner {
    fn drop(&mut self) {
        self.close();
        if let Some(reader) = self.reader.lock().unwrap().take() {
            if reader.thread().id() != thread::current().id() {
                let _ = reader.join();
            }
        }
    }
}
/// Multiplexed native VNX client. Returning from a timeout removes the outstanding
/// request, so a late return cannot satisfy a subsequent call.
#[derive(Clone)]
pub struct NetworkClient {
    inner: Arc<NetworkInner>,
}
impl NetworkClient {
    pub fn connect(
        endpoint: &Endpoint,
        service: &str,
        mut registry: Registry,
        timeout: Duration,
    ) -> Result<Self> {
        if timeout.is_zero() {
            return Err(invalid("network timeout must be positive"));
        }
        register_types(&mut registry)?;
        let stream = Stream::connect(endpoint, timeout)?;
        stream.set_write_timeout(timeout)?;
        let closer = stream.try_clone()?;
        let reader_stream = stream.try_clone()?;
        let mut random = [0_u8; 8];
        getrandom::getrandom(&mut random).map_err(|e| invalid(format!("random identity: {e}")))?;
        let source = u64::from_le_bytes(random).max(1);
        let inner = Arc::new(NetworkInner {
            writer: Mutex::new(crate::binary::Writer::new(stream, registry.clone())),
            pending: Mutex::new(BTreeMap::new()),
            closed: AtomicBool::new(false),
            samples: Mutex::new(BTreeMap::new()),
            remote_session: Mutex::new(None),
            session_changed: Condvar::new(),
            login_lock: Mutex::new(()),
            remote_process_id: Mutex::new(None),
            connection_changed: Condvar::new(),
            heartbeats: Mutex::new(None),
            next_id: AtomicU64::new(1),
            source,
            service: crate::hash::hash64(service),
            timeout,
            closer,
            reader: Mutex::new(None),
        });
        let weak = Arc::downgrade(&inner);
        let reader = thread::Builder::new()
            .name("vnx-network-client".into())
            .spawn(move || {
                let mut input = crate::binary::Reader::new(reader_stream, registry);
                while let Ok(response) = input.read() {
                    let Some(inner) = weak.upgrade() else {
                        break;
                    };
                    if response["__type"] == "vnx.Request"
                        && response["dst_mac"].as_u64() == Some(crate::hash::hash64("vnx.remote"))
                    {
                        if response["method"]["__type"] == "vnx.ProxyInterface.on_remote_connect" {
                            if let Some(process_id) = response["method"]["process_id"].as_u64() {
                                *inner.remote_process_id.lock().unwrap() = Some(process_id);
                                inner.connection_changed.notify_all();
                                let mut ticker = inner.heartbeats.lock().unwrap();
                                if ticker.is_none() {
                                    let weak = Arc::downgrade(&inner);
                                    let mut seq = 0;
                                    *ticker =
                                        Heartbeats::spawn(Duration::from_secs(10), move || {
                                            let inner = weak.upgrade().ok_or(Error::Closed)?;
                                            seq += 1;
                                            let mut writer = inner.writer.lock().unwrap();
                                            writer.write(&heartbeat(inner.source, seq))?;
                                            writer.flush()
                                        })
                                        .ok();
                                }
                            }
                        }
                        if response["method"]["__type"] == "vnx.ProxyInterface.on_remote_login" {
                            if let Ok(session) = serde_json::from_value::<Session>(
                                response["method"]["remote_session"].clone(),
                            ) {
                                *inner.remote_session.lock().unwrap() = Some(session);
                                inner.session_changed.notify_all();
                            }
                        }
                        continue;
                    }
                    if response["__type"] == "vnx.Sample" {
                        if let Some(topic) = response["topic"].as_str() {
                            let sample = crate::runtime::Sample {
                                topic: topic.into(),
                                value: response["value"].clone(),
                                src_mac: response["src_mac"].as_u64().unwrap_or(0),
                                seq_num: response["seq_num"].as_u64().unwrap_or(0),
                                recv_time: crate::time::now_micros(),
                                replay: response["flags"].as_u64().unwrap_or(0) & 2 != 0,
                            };
                            for receiver in inner.samples.lock().unwrap().values() {
                                if (receiver.topic.is_empty()
                                    || receiver.topic == topic
                                    || topic
                                        .strip_prefix(&receiver.topic)
                                        .is_some_and(|s| s.starts_with('.')))
                                    && (receiver.replay || !sample.replay)
                                    && receiver.pipe.try_send(sample.clone()).is_err()
                                {
                                    receiver.missed.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                        continue;
                    }
                    if response["__type"] == "vnx.Return"
                        && response["src_mac"].as_u64() == Some(inner.source)
                    {
                        if let Some(id) = response["request_id"].as_u64() {
                            if let Some(pipe) = inner.pending.lock().unwrap().remove(&id) {
                                let _ = pipe.try_send(response_value(
                                    response["value"].clone(),
                                    input.registry(),
                                ));
                                pipe.close();
                            }
                        }
                    }
                }
                if let Some(inner) = weak.upgrade() {
                    inner.close();
                }
            })?;
        *inner.reader.lock().unwrap() = Some(reader);
        {
            let greeting = proxy_request(
                inner.source,
                inner.next_id.fetch_add(1, Ordering::Relaxed),
                "vnx.ProxyInterface.on_remote_connect",
                json!({"process_id":inner.source}),
            );
            let mut writer = inner.writer.lock().unwrap();
            writer.write(&greeting)?;
            writer.flush()?;
        }
        Ok(Self { inner })
    }
    pub fn wait_on_connect(&self) -> Result<u64> {
        let deadline = Instant::now() + self.inner.timeout;
        let mut process_id = self.inner.remote_process_id.lock().unwrap();
        loop {
            if let Some(id) = *process_id {
                return Ok(id);
            }
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(Error::Closed);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::Timeout);
            }
            process_id = self
                .inner
                .connection_changed
                .wait_timeout(process_id, left)
                .unwrap()
                .0;
        }
    }
    pub fn call_value(&self, method: &str, args: Value) -> Result<Value> {
        self.call_async_value(method, args)?.wait()
    }
    pub fn call_async_value(&self, method: &str, args: Value) -> Result<RpcCall> {
        self.call_destination(self.inner.service, method, args)
    }
    fn call_destination(&self, destination: u64, method: &str, mut args: Value) -> Result<RpcCall> {
        if !args.is_object() {
            return Err(invalid("method arguments must be an object"));
        }
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        args.as_object_mut()
            .unwrap()
            .insert("__type".into(), Value::String(method.into()));
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            return Err(invalid("request ID exhausted"));
        }
        let (mut call, pipe) = RpcCall::pending(id, self.inner.timeout)?;
        {
            let mut pending = self.inner.pending.lock().unwrap();
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(Error::Closed);
            }
            if pending.len() >= 1024 {
                return Err(Error::Overflow);
            }
            pending.insert(id, pipe);
        }
        let weak = Arc::downgrade(&self.inner);
        call.on_cancel(move || {
            if let Some(inner) = weak.upgrade() {
                inner.pending.lock().unwrap().remove(&id);
            }
        });
        let request = json!({"__type":"vnx.Request","stream":0,"flags":1,"hop_count":0,"src_mac":self.inner.source,"dst_mac":destination,"session":0,"request_id":id,"method":args});
        let mut writer = self.inner.writer.lock().unwrap();
        writer.write(&request)?;
        writer.flush()?;
        Ok(call)
    }
    /// Authenticate using the native Proxy exchange (SHA256 of the plaintext is
    /// sent in on_login; the server responds with on_remote_login Session).
    pub fn login(&self, username: &str, password: &str) -> Result<Session> {
        use sha2::{Digest, Sha256};
        let _login = self.inner.login_lock.lock().unwrap();
        *self.inner.remote_session.lock().unwrap() = None;
        let password = Sha256::digest(password.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        self.call_destination(
            crate::hash::hash64("vnx.remote"),
            "vnx.ProxyInterface.on_login",
            json!({"name":username,"password":password}),
        )?
        .wait()?;
        let deadline = Instant::now() + self.inner.timeout;
        let mut session = self.inner.remote_session.lock().unwrap();
        loop {
            if let Some(session) = session.as_ref() {
                if session.user != username {
                    return Err(Error::PermissionDenied("login failed".into()));
                }
                return Ok(session.clone());
            }
            if self.inner.closed.load(Ordering::Acquire) {
                return Err(Error::Closed);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }
            session = self
                .inner
                .session_changed
                .wait_timeout(session, remaining)
                .unwrap()
                .0;
        }
    }
    pub fn remote_session(&self) -> Result<Session> {
        let response = self
            .call_destination(
                crate::hash::hash64("vnx.remote"),
                "vnx.ProxyInterface.get_session",
                json!({}),
            )?
            .wait()?;
        Ok(serde_json::from_value(response["_ret_0"].clone())?)
    }
    pub fn subscribe(
        &self,
        topic: &str,
        capacity: usize,
        overflow: crate::runtime::OverflowPolicy,
        replay: bool,
    ) -> Result<NetworkSubscription> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        let pipe = crate::runtime::Pipe::new(capacity, overflow)?;
        let missed = Arc::new(AtomicU64::new(0));
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut samples = self.inner.samples.lock().unwrap();
            if samples.len() >= 1024 {
                return Err(Error::Overflow);
            }
            samples.insert(
                id,
                SampleReceiver {
                    topic: topic.into(),
                    pipe: pipe.clone(),
                    missed: missed.clone(),
                    replay,
                },
            );
        }
        let subscription = NetworkSubscription {
            id,
            topic: topic.into(),
            inner: Arc::downgrade(&self.inner),
            pipe,
            missed,
        };
        self.call_destination(
            crate::hash::hash64("vnx.remote"),
            "vnx.ProxyInterface.enable_export",
            json!({"topic_name":topic}),
        )?
        .wait()?;
        Ok(subscription)
    }
    pub fn publish(&self, topic: &str, value: Value) -> Result<()> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        let sample = json!({"__type":"vnx.Sample","stream":0,"flags":0,"hop_count":0,"src_mac":self.inner.source,"seq_num":self.inner.next_id.fetch_add(1,Ordering::Relaxed),"recv_time":crate::time::now_micros(),"topic":topic,"value":value});
        let mut writer = self.inner.writer.lock().unwrap();
        writer.write(&sample)?;
        writer.flush()
    }
    pub fn close(&self) {
        self.inner.close();
    }
}

/// Stream samples share a reader with RPC replies. A full subscriber queue never
/// blocks that reader; dropped() counts rejected or overwritten samples.
pub struct NetworkSubscription {
    id: u64,
    topic: String,
    inner: std::sync::Weak<NetworkInner>,
    pipe: crate::runtime::Pipe<crate::runtime::Sample>,
    missed: Arc<AtomicU64>,
}
impl NetworkSubscription {
    pub fn recv(&self) -> Result<crate::runtime::Sample> {
        self.pipe.recv()
    }
    pub fn recv_timeout(&self, timeout: Duration) -> Result<crate::runtime::Sample> {
        self.pipe.recv_timeout(timeout)
    }
    pub fn try_recv(&self) -> Result<Option<crate::runtime::Sample>> {
        self.pipe.try_recv()
    }
    pub fn dropped(&self) -> u64 {
        self.pipe.dropped() + self.missed.load(Ordering::Relaxed)
    }
    pub fn close(&self) {
        self.pipe.close();
        if let Some(inner) = self.inner.upgrade() {
            if inner.samples.lock().unwrap().remove(&self.id).is_some()
                && !inner.closed.load(Ordering::Acquire)
            {
                let request = proxy_request(
                    inner.source,
                    inner.next_id.fetch_add(1, Ordering::Relaxed),
                    "vnx.ProxyInterface.disable_export",
                    json!({"topic_name":self.topic}),
                );
                let mut output = inner.writer.lock().unwrap();
                let _ = output.write(&request).and_then(|_| output.flush());
            }
        }
    }
}
impl Drop for NetworkSubscription {
    fn drop(&mut self) {
        self.close();
    }
}
