//! MCP 客户端工具（mcp 域，转发到 sidecar）
//!
//! 架构分工：sidecar（Node）用官方 `@modelcontextprotocol/sdk` 承担 MCP 协议
//! 细节（stdio / SSE 传输、tool/list、tool/call），Rust 侧只保留两个薄转发工具：
//! - `mcp_list_tools`：列出 sidecar 已连接的所有 MCP 工具
//! - `mcp_call`：调用某个 MCP 工具
//!
//! 这样 agent 的工具清单保持稳定，任意 MCP server 的工具都能通过这两个入口触达，
//! 而不必把远端工具逐个动态注册成 AgentTool。

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

/// sidecar 地址。优先 settings `sidecar.url`，默认本机 4770。
fn sidecar_base() -> String {
    if let Some(conn) = crate::memory::open_connection() {
        if let Ok(v) = conn.query_row(
            "SELECT value FROM settings WHERE key = 'sidecar.url'",
            [],
            |row| row.get::<_, Option<String>>(0),
        ) {
            if let Some(u) = v.filter(|s| !s.trim().is_empty()) {
                return u.trim_end_matches('/').to_string();
            }
        }
    }
    "http://127.0.0.1:4770".to_string()
}

/// sidecar 不可达时的统一回话。
fn sidecar_unreachable() -> ToolResult {
    ToolResult::err(
        "SIDECAR_UNREACHABLE",
        "本地 sidecar 服务未运行或不可达。MCP 能力依赖 sidecar 常驻进程，请确认它已启动（默认 127.0.0.1:4770）。",
    )
}

// ============================================================
// mcp_list_tools
// ============================================================

pub struct McpListToolsTool;

#[async_trait]
impl AgentTool for McpListToolsTool {
    fn name(&self) -> &'static str {
        "mcp_list_tools"
    }

    fn description(&self) -> &'static str {
        "列出通过 MCP（Model Context Protocol）接入的所有外部工具。回答「有哪些外部工具可用」时用。这些工具来自用户配置的 MCP server，调用时用 mcp_call。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: Value) -> AppResult<ToolResult> {
        let url = format!("{}/mcp/tools", sidecar_base());
        let resp = match reqwest::Client::new().get(&url).send().await {
            Ok(r) => r,
            Err(_) => return Ok(sidecar_unreachable()),
        };

        if !resp.status().is_success() {
            return Ok(ToolResult::err(
                "MCP_FAILED",
                &format!("sidecar 返回 {}", resp.status()),
            ));
        }

        match resp.json::<Value>().await {
            Ok(data) => Ok(ToolResult::ok(data)),
            Err(e) => Ok(ToolResult::err(
                "MCP_FAILED",
                &format!("解析 sidecar 响应失败：{}", e),
            )),
        }
    }
}

// ============================================================
// mcp_call
// ============================================================

pub struct McpCallTool;

#[async_trait]
impl AgentTool for McpCallTool {
    fn name(&self) -> &'static str {
        "mcp_call"
    }

    fn description(&self) -> &'static str {
        "调用一个通过 MCP 接入的外部工具。先 mcp_list_tools 查可用工具，再用 name 指定工具名、arguments 传参数。外部工具的副作用不受本机契约管控，调用前确认用户意图。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "工具名（从 mcp_list_tools 得到）" },
                "arguments": { "type": "object", "description": "工具参数（JSON 对象）" },
                "server": { "type": "string", "description": "可选：指定 MCP server id。多个 server 提供同名工具时必填" }
            },
            "required": ["name"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::High
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let name = args["name"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("name is required".to_string()))?;
        let arguments = args["arguments"].clone();
        let server = args["server"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let url = format!("{}/mcp/call", sidecar_base());
        let mut payload = json!({ "name": name, "arguments": arguments });
        if let Some(s) = server {
            payload["server"] = json!(s);
        }
        let resp = match reqwest::Client::new()
            .post(&url)
            .json(&payload)
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => return Ok(sidecar_unreachable()),
        };

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        let data: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => {
                return Ok(ToolResult::err(
                    "MCP_FAILED",
                    &format!("sidecar 返回非 JSON（{}）：{}", status, body),
                ))
            }
        };

        if !status.is_success() {
            let message = data["error"].as_str().unwrap_or("MCP 工具调用失败");
            return Ok(ToolResult::err("MCP_CALL_FAILED", message));
        }

        Ok(ToolResult::ok(data))
    }
}
