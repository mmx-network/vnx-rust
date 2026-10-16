//! Common transport interface used by generated VNI clients.
use crate::{runtime::RpcCall, Result};
use serde_json::Value;
/// Generated clients work with in-process services and native VNX sockets.
/// Implementations retain their own session, destination, timeout and cancellation policy.
pub trait RpcClient: Clone + Send + Sync {
    fn call_value(&self, method: &str, args: Value) -> Result<Value>;
    fn call_async_value(&self, method: &str, args: Value) -> Result<RpcCall>;
}
impl RpcClient for crate::runtime::Client {
    fn call_value(&self, method: &str, args: Value) -> Result<Value> {
        crate::runtime::Client::call_value(self, method, args)
    }
    fn call_async_value(&self, method: &str, args: Value) -> Result<RpcCall> {
        crate::runtime::Client::call_async_value(self, method, args)
    }
}
impl RpcClient for crate::net::NetworkClient {
    fn call_value(&self, method: &str, args: Value) -> Result<Value> {
        crate::net::NetworkClient::call_value(self, method, args)
    }
    fn call_async_value(&self, method: &str, args: Value) -> Result<RpcCall> {
        crate::net::NetworkClient::call_async_value(self, method, args)
    }
}
