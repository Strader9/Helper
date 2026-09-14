//! 系统信息工具（V15 新增）
//!
//! 提供 `get_system_context` 工具，返回当前系统状态摘要。
//! 风险等级：SAFE（纯只读，无副作用）

use crate::error::AppResult;
use crate::monitoring::get_system_metrics;
use crate::tools::{AgentTool, RiskLevel, ToolResult};
use std::process::Command;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// 获取系统上下文工具
///
/// 返回 CPU/内存/磁盘/活动窗口/进程数等系统状态摘要。
pub struct GetSystemContextTool;

#[async_trait::async_trait]
impl AgentTool for GetSystemContextTool {
    fn name(&self) -> &'static str {
        "get_system_context"
    }

    fn description(&self) -> &'static str {
        "获取当前系统状态摘要，包括 CPU 使用率、内存使用、磁盘空间、活动窗口、前台进程和进程数。用于了解当前电脑运行状态。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        let metrics = get_system_metrics();
        let (active_window, foreground_process, process_count) = get_window_and_process_info();

        let disk_summary: Vec<serde_json::Value> = metrics
            .disks
            .iter()
            .map(|d| {
                serde_json::json!({
                    "drive": d.drive,
                    "total_gb": d.total_gb,
                    "used_gb": d.used_gb,
                    "free_gb": (d.total_gb - d.used_gb).max(0.0),
                    "usage_percent": d.usage_percent
                })
            })
            .collect();

        let data = serde_json::json!({
            "cpu_usage": metrics.cpu_usage,
            "memory": {
                "total_gb": metrics.memory_total_gb,
                "used_gb": metrics.memory_used_gb,
                "usage_percent": metrics.memory_usage_percent
            },
            "disks": disk_summary,
            "active_window": active_window,
            "foreground_process": foreground_process,
            "process_count": process_count,
            "network_ok": metrics.network_ok,
            "gpu": {
                "name": metrics.gpu_name,
                "usage": metrics.gpu_usage
            },
            "timestamp": chrono::Utc::now().timestamp_millis()
        });

        // 生成人类可读摘要
        let disk_str = if metrics.disks.is_empty() {
            "无".to_string()
        } else {
            metrics
                .disks
                .iter()
                .map(|d| format!("{}:{:.0}%", d.drive, d.usage_percent))
                .collect::<Vec<_>>()
                .join(" ")
        };

        let summary = format!(
            "CPU:{:.0}% 内存:{:.0}%({:.1}/{:.1}GB) 磁盘:[{}] 窗口:{} 进程:{} 网络:{}",
            metrics.cpu_usage,
            metrics.memory_usage_percent,
            metrics.memory_used_gb,
            metrics.memory_total_gb,
            disk_str,
            active_window.as_deref().unwrap_or("未知"),
            process_count,
            if metrics.network_ok { "正常" } else { "异常" }
        );

        Ok(ToolResult::ok(serde_json::json!({
            "summary": summary,
            "data": data
        })))
    }
}

/// 获取活动窗口和进程信息（Windows）
#[cfg(target_os = "windows")]
fn get_window_and_process_info() -> (Option<String>, Option<String>, usize) {
    let active_window = get_active_window_title();
    let foreground_process = get_foreground_process();
    let process_count = get_process_count();
    (active_window, foreground_process, process_count)
}

#[cfg(not(target_os = "windows"))]
fn get_window_and_process_info() -> (Option<String>, Option<String>, usize) {
    (None, None, 0)
}

/// 获取活动窗口标题（Windows PowerShell）
#[cfg(target_os = "windows")]
fn get_active_window_title() -> Option<String> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle", "Hidden",
            "-Command",
            "Get-Process | Where-Object { $_.MainWindowTitle -ne '' } | Sort-Object StartTime -Descending | Select-Object -First 1 -ExpandProperty MainWindowTitle",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 获取前台进程名（Windows PowerShell）
#[cfg(target_os = "windows")]
fn get_foreground_process() -> Option<String> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle", "Hidden",
            "-Command",
            r"Get-Process | Where-Object { $_.MainWindowTitle -ne '' } | Sort-Object StartTime -Descending | Select-Object -First 1 -ExpandProperty ProcessName",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 获取进程总数（Windows PowerShell）
#[cfg(target_os = "windows")]
fn get_process_count() -> usize {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle", "Hidden",
            "-Command",
            "(Get-Process).Count",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok();

    match output {
        Some(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            text.trim().parse::<usize>().unwrap_or(0)
        }
        None => 0,
    }
}
