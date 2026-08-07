//! MCP server management NAPI bindings.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use super::guard;

/// 扫描所有 MCP servers
#[napi]
pub async fn scan_mcp_servers() -> Result<Vec<crate::mcp::McpServerInfo>> {
    guard();
    tokio::task::spawn_blocking(crate::mcp::scan_mcp_servers)
        .await
        .map_err(|e| Error::from_reason(format!("task panicked: {}", e)))?
}

/// 获取指定 MCP server 的配置
#[napi]
pub async fn get_mcp_server_config(name: String) -> Result<Option<crate::mcp::McpServerInfo>> {
    guard();
    tokio::task::spawn_blocking(move || crate::mcp::get_mcp_server_config(name))
        .await
        .map_err(|e| Error::from_reason(format!("task panicked: {}", e)))?
}

/// 检查内置服务状态
#[napi]
pub async fn check_builtin_services() -> Result<Vec<crate::mcp::BuiltinServiceInfo>> {
    guard();
    tokio::task::spawn_blocking(crate::mcp::check_builtin_services)
        .await
        .map_err(|e| Error::from_reason(format!("task panicked: {}", e)))?
}

/// 启动 MCP server
#[napi]
pub async fn start_mcp_server(name: String) -> Result<String> {
    guard();
    crate::mcp::start_mcp_server(name)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))
}

/// 停止 MCP server
#[napi]
pub async fn stop_mcp_server(name: String) -> Result<()> {
    guard();
    crate::mcp::stop_mcp_server(name)
        .await
        .map_err(|e| Error::from_reason(e.to_string()))
}
