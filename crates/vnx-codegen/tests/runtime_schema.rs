// The checked-in VNI declarations are the runtime wire API, compiled through
// the same public compiler used by MMX. No C++ build is needed for this check.
use vnx_codegen::Compiler;
#[test]
fn builtin_runtime_metadata_matches_generated_vni() {
    let mut compiler = Compiler::new();
    compiler
        .add_source(
            "package.vni",
            "package vnx; extern Hash64 = {4}; extern TopicPtr = {12, 5}; enum permission_e { VIEW, LOCAL, PROXY_IMPORT, PROXY_EXPORT, PROXY_FORWARD, INTERNAL, PROXY_INTERNAL }",
        )
        .unwrap();
    compiler
        .add_source(
            "Message.vni",
            r#"package vnx;

class Message {

	static const ushort BLOCKING = 1;
	static const ushort REPLAY = 2;
	static const ushort NO_RETURN = 4;
	static const ushort NON_BLOCKING = 8;		// throw exception if blocked / not connected / etc
	static const ushort SHUTDOWN = 16;			// shutdown node
	static const ushort RESEND = 32;			// same data as previous (same sequence number)
	static const ushort FORWARD = 64;			// only reply when forwarding fails

	Hash64 stream;			// stream id (tunnel mac, or proxy mac, or pipe mac)

	ushort flags;
	ushort hop_count;

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "Request.vni",
            r#"package vnx;

class Request extends Message {

	Hash64 src_mac;			// unique id
	Hash64 dst_mac;			// unique id

	Hash64 session;			// session id

	ulong request_id;		// sequence counter (starting with 1)

	Value* method;

	alias arguments method;

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "Return.vni",
            r#"package vnx;

class Return extends Message {

	ulong request_id;

	Hash64 src_mac;

	Value* value;

	static Return* create_overflow_return(Request* request);

	static Return* create_no_such_service_return(Request* request);

	static Return* create_permission_denied_return(Request *request);

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "Sample.vni",
            r#"package vnx;

class Sample extends Message {

	Hash64 src_mac;				// source identity (unique for each publisher)

	ulong seq_num;				// first sequence number is at least 1

	long recv_time;				// timestamp when it was received in process (sync time) [usec]

	TopicPtr topic;				// topic

	Value* value;				// the actual data

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "FlowMessage.vni",
            r#"package vnx;

class FlowMessage extends Message {

	static const ushort OPEN = 1;
	static const ushort PING = 2;
	static const ushort CLOSE = 3;

	Hash64 src_mac;
	Hash64 dst_mac;

	ushort flow_code;

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "Session.vni",
            r#"package vnx;

class Session {

	Hash64 id;						// unique session id (local to a process)

	string user;					// user name (empty = anonymous)

	set<string> permissions;		// granted permissions

	long login_time;				// posix timestamp [usec]


	bool is_anonymous() const;

	bool has_permission(string perm) const;

	bool has_permission_vnx(permission_e perm) const;

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "Exception.vni",
            r#"package vnx;

class Exception {

	string what;


	static Exception* from_what(string what);

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "NoSuchService.vni",
            r#"package vnx;

class NoSuchService extends Exception {

	Hash64 dst_mac;

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "NoSuchMethod.vni",
            r#"package vnx;

class NoSuchMethod extends NoSuchService {

	string method;

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "PermissionDenied.vni",
            r#"package vnx;

class PermissionDenied extends Exception {

	Hash64 dst_mac;

	string method;

	string permission;			// required permission

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "OverflowException.vni",
            r#"package vnx;

class OverflowException extends Exception {

	Hash64 dst_mac;

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "InternalError.vni",
            r#"package vnx;

class InternalError extends Exception {

	static InternalError* from_what(string what_);

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "ProxyInterface.vni",
            r#"package vnx;

interface ProxyInterface {

	@Permission(LOCAL)
	Session* login(string name, string password) const async;

	@Permission(PROXY_IMPORT)
	void enable_import(string topic_name);

	@Permission(PROXY_IMPORT)
	void disable_import(string topic_name);

	@Permission(PROXY_EXPORT)
	void enable_export(string topic_name);

	@Permission(PROXY_EXPORT)
	void disable_export(string topic_name);

	@Permission(PROXY_FORWARD)
	void enable_forward(string service_name, int max_queue_ms, int max_queue_size);

	@Permission(PROXY_FORWARD)
	void disable_forward(string service_name);

	@Permission(PROXY_FORWARD)
	void enable_tunnel(Hash64 tunnel_addr, int max_queue_ms, int max_queue_size);

	@Permission(PROXY_FORWARD)
	void disable_tunnel(Hash64 tunnel_addr);

	Session* get_session() const;

	Hash64 wait_on_connect() const async;		// returns remote process id when connected

	Hash64 wait_on_disconnect() const async;	// returns remote process id of lost connection (if any)

	@Permission(INTERNAL)
	void on_connect();					// internal use only

	@Permission(INTERNAL)
	void on_disconnect();				// internal use only

	@Permission(PROXY_INTERNAL)
	void on_remote_connect(Hash64 process_id);			// internal use only, called from other end

	@Permission(PROXY_INTERNAL)
	void on_login(string name, string password);		// internal use only, called from other end

	@Permission(PROXY_INTERNAL)
	void on_remote_login(Session* remote_session);		// internal use only, called from other end

}
"#,
        )
        .unwrap();
    compiler
        .add_source(
            "Heartbeat.vni",
            r#"package vnx;

class Heartbeat {

	string source;

}
"#,
        )
        .unwrap();
    let mut generated = compiler.compile().unwrap().registry;
    vnx::net::register_types(&mut generated).unwrap();
    // The same complete generated registry must be accepted by both transports.
    let runtime = vnx::runtime::Runtime::new().unwrap();
    let mut server = vnx::net::Server::bind(
        "tcp://127.0.0.1:0".parse().unwrap(),
        runtime.clone(),
        generated.clone(),
        Default::default(),
    )
    .unwrap();
    let client = vnx::net::NetworkClient::connect(
        server.endpoint(),
        "missing",
        generated,
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    client.close();
    server.shutdown();
    runtime.shutdown();
}
