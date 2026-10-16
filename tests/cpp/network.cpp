// Optional interoperability oracle. This executable is never linked into Rust.
// Exchanges native generated C++ Request/Return messages with the Rust transport.
#include <vnx/vnx.h>
#include <vnx/Endpoint.hxx>
#include <vnx/Request.hxx>
#include <vnx/Return.hxx>
#include <vnx/NoSuchService.hxx>
#include <vnx/Proxy.h>
#include <vnx/ProxyInterfaceClient.hxx>
#include <vnx/ProxyInterface_on_remote_connect.hxx>
#include <vnx/Exception.hxx>
#include <vnx/Sample.hxx>
#include <mmx/NodeClient.hxx>
#include <chrono>
#include <thread>
#include <mmx/Node_get_height.hxx>
#include <mmx/Node_get_height_return.hxx>
#include <sys/socket.h>
#include <netinet/in.h>
#include <unistd.h>
#include <iostream>

class Receiver : public vnx::Node {
public:
    Receiver() { pipe = vnx::open_pipe(vnx::Hash64::rand(), this, 1000, 10); vnx::get_topic("test.visible")->subscribe(pipe); }
    ~Receiver() { vnx::get_topic("test.visible")->unsubscribe(pipe); }
    std::shared_ptr<const vnx::Sample> receive() {
        const auto deadline=std::chrono::steady_clock::now()+std::chrono::seconds(5);
        while(std::chrono::steady_clock::now()<deadline){if(auto sample=std::dynamic_pointer_cast<const vnx::Sample>(read_blocking(500000))){return sample;}}
        return nullptr;
    }
private: std::shared_ptr<vnx::Pipe> pipe;
};
int proxy_peer(const char* address) {
    ::alarm(10);
    int argc=1;char name[]="vnx_cpp_proxy_test";char* argv[]={name,nullptr};
    vnx::init(name,argc,argv);
    std::string url=address;if(url.rfind("tcp://",0)==0){url.erase(0,6);}
    vnx::Handle<vnx::Proxy> proxy=new vnx::Proxy("Proxy",vnx::Endpoint::from_url(url));
    proxy->forward_list={"Node"};proxy->heartbeat_ms=20;proxy->heartbeat_timeout=3;proxy->no_retry=true;
    proxy.start();
    vnx::ProxyInterfaceClient control("Proxy");
    if(!uint64_t(control.wait_on_connect())) return 10;
    const auto session=control.login("operator","correct password");
    if(!session||session->user!="operator") return 11;
    mmx::NodeClient node("Node");
    if(node.get_height()!=5050001) return 12;
    Receiver receiver;control.enable_import("test.visible");
    const auto sample=receiver.receive();
    if(!sample||sample->topic->get_name()!="test.visible.child") return 13;
    const auto value=std::dynamic_pointer_cast<const vnx::Exception>(sample->value);
    if(!value||value->what!="proxy sample") return 14;
    // More than three configured heartbeat periods: missing/invalid heartbeats
    // would disconnect the native Proxy before this second normal RPC call.
    std::this_thread::sleep_for(std::chrono::milliseconds(250));
    if(node.get_height()!=5050001) return 15;
    control.disable_import("test.visible");proxy.close();vnx::close();
    std::cout<<"native vnx::Proxy greeting/login/subscription/heartbeat accepted"<<std::endl;
    ::alarm(0);return 0;
}

// Each socket gets fresh VNX schema state, including after a reconnect.
void serve_session(int socket, uint64_t process_id, size_t calls, uint32_t height) {
    timeval timeout = {}; timeout.tv_sec = 5;
    ::setsockopt(socket, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    ::setsockopt(socket, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
    vnx::SocketInputStream input_stream(socket);
    vnx::SocketOutputStream output_stream(socket);
    vnx::TypeInput input(&input_stream); vnx::TypeOutput output(&output_stream);
    for(size_t i = 0; i < calls; ++i) {
        std::shared_ptr<vnx::Request> request;
        while(true) {
            request = std::dynamic_pointer_cast<vnx::Request>(vnx::read(input));
            if(!request) continue;
            if(std::dynamic_pointer_cast<const vnx::ProxyInterface_on_remote_connect>(request->method)) {
                auto greeting = vnx::Request::create();
                greeting->flags = 4;
                greeting->src_mac = vnx::Hash64(process_id);
                greeting->dst_mac = vnx::Hash64("vnx.remote");
                auto method = vnx::ProxyInterface_on_remote_connect::create();
                method->process_id = vnx::Hash64(process_id);
                greeting->method = method;
                vnx::write(output, greeting); output.flush();
            }
            if(std::dynamic_pointer_cast<const mmx::Node_get_height>(request->method)) break;
        }
        auto response = vnx::Return::create();
        response->request_id = request->request_id; response->src_mac = request->src_mac;
        auto value = mmx::Node_get_height_return::create(); value->_ret_0 = height + i;
        response->value = value; vnx::write(output, response); output.flush();
    }
}

int main(int argc, char** argv) {
    if(argc < 2) return 1;
    if(std::string(argv[1])=="proxy" && argc==3){return proxy_peer(argv[2]);}
    const bool reconnect = std::string(argv[1]) == "reconnect";
    const bool server = reconnect || std::string(argv[1]) == "server";
    int socket = -1;
    if(server) {
        ::alarm(15);
        const int listener = ::socket(AF_INET, SOCK_STREAM, 0);
        sockaddr_in addr = {}; addr.sin_family = AF_INET;
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        if(::bind(listener, reinterpret_cast<sockaddr*>(&addr), sizeof(addr))) return 2;
        socklen_t size = sizeof(addr);
        if(::getsockname(listener, reinterpret_cast<sockaddr*>(&addr), &size)) return 3;
        if(::listen(listener, 1)) return 4;
        std::cout << "tcp://127.0.0.1:" << ntohs(addr.sin_port) << std::endl;
        for(size_t i = 0; i < (reconnect ? 3u : 1u); ++i) {
            socket = ::accept(listener, nullptr, nullptr);
            if(socket < 0) return 5;
            serve_session(socket, 5051000 + i, reconnect ? 1 : 2, 5050001 + i);
            // Deliver the final Return before disconnecting, even if the peer
            // has an outgoing heartbeat queued. Closing unread TCP input could
            // otherwise reset the connection and discard that response.
            ::shutdown(socket, SHUT_WR);
            char discard[1024];
            while(::recv(socket, discard, sizeof(discard), 0) > 0) {}
            ::close(socket);
        }
        ::close(listener);
        ::alarm(0);
        return 0;
    } else {
        if(argc != 3) return 1;
        std::string address = argv[2];
        if(address.rfind("tcp://", 0) == 0) address.erase(0, 6);
        const auto endpoint = vnx::Endpoint::from_url(address);
        socket = endpoint->open(); endpoint->connect(socket);
    }
    timeval timeout = {}; timeout.tv_sec = 5;
    ::setsockopt(socket, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    ::setsockopt(socket, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
    vnx::SocketInputStream input_stream(socket);
    vnx::SocketOutputStream output_stream(socket);
    vnx::TypeInput input(&input_stream); vnx::TypeOutput output(&output_stream);
    {
        for(size_t i = 0; i < 2; ++i) {
            auto request = vnx::Request::create(); request->request_id = i + 1;
            request->src_mac = vnx::Hash64(0x1234567890ULL); request->dst_mac = vnx::Hash64("Node");
            request->method = mmx::Node_get_height::create();
            vnx::write(output, request); output.flush();
            auto response = std::dynamic_pointer_cast<vnx::Return>(vnx::read(input));
            if(!response || response->request_id != i + 1 || response->src_mac != request->src_mac) return 6;
            auto value = std::dynamic_pointer_cast<const mmx::Node_get_height_return>(response->value);
            if(!value || value->_ret_0 != 5050001) return 7;
        }
        std::cout << "native C++ client accepted Rust returns" << std::endl;
    }
    ::close(socket);
    return 0;
}
