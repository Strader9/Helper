//! 进程管理工具
//!
//! 提供 list_processes、kill_process 等工具。

use std::process::Command;
use crate::tools::{AgentTool, RiskLevel, ToolResult};
use async_trait::async_trait;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 列出系统中所有运行中的进程
pub struct ListProcessesTool;

#[async_trait]
impl AgentTool for ListProcessesTool {
    fn name(&self) -> &'static str {
        "list_processes"
    }

    fn description(&self) -> &'static str {
        "列出系统中所有运行中的进程，返回进程名、PID、内存占用等信息。可用于查找哪个程序最占内存、检查某个程序是否在运行。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "sort_by": {
                    "type": "string",
                    "description": "排序方式：memory（按内存降序）、cpu（按CPU降序）、name（按名称）。默认 memory"
                },
                "limit": {
                    "type": "integer",
                    "description": "返回进程数量上限，默认 50"
                }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> crate::error::AppResult<ToolResult> {
        let sort_by = args.get("sort_by").and_then(|v| v.as_str()).unwrap_or("memory");
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(50) as usize;

        #[cfg(target_os = "windows")]
        {
            // 使用 PowerShell 获取进程列表
            let ps_script = format!(
                "Get-Process | Sort-Object {} -Descending | Select-Object -First {} Id, ProcessName, @{{N='MemoryMB';E={{[math]::Round($_.WorkingSet64/1MB,1)}}}}, @{{N='CPU';E={{[math]::Round($_.CPU,1)}}}} | ConvertTo-Json",
                match sort_by {
                    "memory" => "WorkingSet64",
                    "cpu" => "CPU",
                    _ => "ProcessName",
                },
                limit
            );

            let output = Command::new("powershell")
                .args(&["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &ps_script])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map_err(|e| crate::error::AppError::ToolExecution(format!("执行 PowerShell 失败: {}", e)))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Ok(ToolResult::err("PROCESS_LIST_FAILED", &format!("获取进程列表失败: {}", stderr)));
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let processes: serde_json::Value = serde_json::from_str(&stdout).unwrap_or(serde_json::json!([]));
            let count = processes.as_array().map(|a| a.len()).unwrap_or(0);

            Ok(ToolResult::ok(serde_json::json!({
                "processes": processes,
                "count": count,
                "sort_by": sort_by,
                "message": format!("共 {} 个进程（按{}排序）", count, sort_by)
            })))
        }

        #[cfg(not(target_os = "windows"))]
        {
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "list_processes 仅在 Windows 上可用"))
        }
    }
}

/// 结束指定进程
pub struct KillProcessTool;

#[async_trait]
impl AgentTool for KillProcessTool {
    fn name(&self) -> &'static str {
        "kill_process"
    }

    fn description(&self) -> &'static str {
        "结束指定的进程。可通过 PID 或进程名结束进程。用于关闭无响应的程序、释放内存等。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pid": {
                    "type": "integer",
                    "description": "要结束的进程 PID（与 name 二选一，优先 PID）"
                },
                "name": {
                    "type": "string",
                    "description": "要结束的进程名（如 chrome.exe、QQ.exe），会结束所有同名进程"
                }
            },
            "required": []
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::High
    }

    async fn execute(&self, args: serde_json::Value) -> crate::error::AppResult<ToolResult> {
        let pid = args.get("pid").and_then(|v| v.as_i64());
        let name = args.get("name").and_then(|v| v.as_str());

        if pid.is_none() && name.is_none() {
            return Ok(ToolResult::err("INVALID_ARGUMENT", "必须提供 pid 或 name 参数"));
        }

        #[cfg(target_os = "windows")]
        {
            let output = if let Some(pid) = pid {
                Command::new("taskkill")
                    .args(&["/PID", &pid.to_string(), "/F"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output()
                    .map_err(|e| crate::error::AppError::ToolExecution(format!("执行 taskkill 失败: {}", e)))?
            } else if let Some(name) = name {
                // 确保进程名有 .exe 后缀
                let process_name = if name.to_lowercase().ends_with(".exe") {
                    name.to_string()
                } else {
                    format!("{}.exe", name)
                };
                Command::new("taskkill")
                    .args(&["/IM", &process_name, "/F"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output()
                    .map_err(|e| crate::error::AppError::ToolExecution(format!("执行 taskkill 失败: {}", e)))?
            } else {
                unreachable!()
            };

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if output.status.success() {
                let target = pid.map(|p| format!("PID {}", p)).unwrap_or_else(|| name.unwrap_or("unknown").to_string());
                Ok(ToolResult::ok(serde_json::json!({
                    "target": target,
                    "message": format!("已结束进程: {}", target),
                    "output": stdout.trim()
                })))
            } else {
                let reason = if stderr.contains("没有找到") || stderr.contains("not found") {
                    "进程不存在或已结束"
                } else {
                    stderr.trim()
                };
                Ok(ToolResult::err("KILL_FAILED", reason))
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "kill_process 仅在 Windows 上可用"))
        }
    }
}
