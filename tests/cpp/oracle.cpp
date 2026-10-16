// Optional C++ conformance oracle only. Never linked into the Rust crates.
#include <vnx/vnx.h>
#include <vnx/LogMsg.hxx>
#include <vnx/Request.hxx>
#include <mmx/KeyFile.hxx>
#include <mmx/Node_get_height.hxx>
#include <mmx/Transaction.hxx>
#include <mmx/ChainParams.hxx>
#include <mmx/BlockHeader.hxx>
#include <fstream>
#include <iostream>
#include <filesystem>

int main(int argc, char** argv) {
    if(argc != 3) return 1;
    const std::string mode = argv[1];
    if(mode == "read") {
        vnx::File file(argv[2]); file.open("rb");
        auto value = vnx::read(file.in);
        if(!value) return 2;
        std::cout << value->to_object() << '\n';
        return 0;
    }
    if(mode != "fixtures") return 1;
    const std::string dir = std::string(argv[2]) + "/";
    std::filesystem::create_directories(dir);
    auto save = [&](const std::string& name, std::shared_ptr<vnx::Value> value) {
        vnx::write_to_file(dir + name + ".dat", value);
        std::ofstream out(dir + name + ".json");
        out << value->to_object() << '\n';
    };
    auto log = vnx::LogMsg::create();
    log->time = 1700000000123456; log->level = vnx::LogMsg::WARN;
    log->module = "Node"; log->process = "mmx_node"; log->message = "Hello 世界\nnext line";
    save("log", log);
    auto key = mmx::KeyFile::create();
    for(size_t i = 0; i < 32; ++i) key->seed_value[i] = i + 1;
    key->finger_print = std::string("1234567890"); save("key", key);
    auto request = vnx::Request::create();
    request->src_mac = vnx::Hash64(0x0123456789abcdefULL);
    request->dst_mac = vnx::Hash64("Node"); request->request_id = 42;
    request->method = mmx::Node_get_height::create(); save("request", request);
    auto params = mmx::ChainParams::create(); params->network = "mainnet"; save("params", params);
    auto block = mmx::BlockHeader::create(); block->height = 5050001; save("block", block);
    auto object = vnx::Object::create();
    (*object)["uint64"] = uint64_t(-1); (*object)["int64"] = int64_t(-9223372036854775807LL-1);
    (*object)["bool"] = true; (*object)["utf8"] = "世界";
    (*object)["none"] = vnx::Variant(); (*object)["float"] = 1.25;
    save("object", object);
    return 0;
}
