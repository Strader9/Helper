//! 文件操作工具
//!
//! 提供文件系统操作能力：打开文件、读取内容、写入内容、列出目录。

use std::fs;
use std::path::Path;
use std::process::Command;

use async_trait::async_trait;

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

// ============================================================
// 打开文件工具
// ============================================================

/// 用系统默认程序打开指定文件
pub struct OpenFileTool;

#[async_trait]
impl AgentTool for OpenFileTool {
    fn name(&self) -> &'static str {
        "open_file"
    }

    fn description(&self) -> &'static str {
        "用系统默认程序打开指定文件。当用户说'打开这个文件'、'打开桌面的xxx.txt'时使用此工具。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "要打开的文件的完整路径"
                }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;

        if !Path::new(path).exists() {
            return Ok(ToolResult::err(
                "FILE_NOT_FOUND",
                &format!("文件不存在: {}", path),
            ));
        }

        #[cfg(target_os = "windows")]
        {
            let _ = Command::new("cmd")
                .args(&["/c", "start", "", path])
                .spawn()
                .map_err(|e| AppError::ToolExecution(format!("无法打开文件: {}", e)))?;
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = Command::new("xdg-open")
                .arg(path)
                .spawn()
                .map_err(|e| AppError::ToolExecution(format!("无法打开文件: {}", e)))?;
        }

        Ok(ToolResult::ok(serde_json::json!({
            "message": format!("已打开文件: {}", path)
        })))
    }
}

// ============================================================
// 读取文件工具
// ============================================================

/// 读取指定文本文件的内容
pub struct ReadFileTool;

#[async_trait]
impl AgentTool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "读取指定文本文件的内容。适用于读取日志、配置文件、文档等。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "要读取的文件路径"
                },
                "max_lines": {
                    "type": "integer",
                    "description": "最大读取行数（可选，默认全部）"
                }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;
        let max_lines = args["max_lines"].as_u64();

        if !Path::new(path).exists() {
            return Ok(ToolResult::err(
                "FILE_NOT_FOUND",
                &format!("文件不存在: {}", path),
            ));
        }

        let content = fs::read_to_string(path).map_err(|e| AppError::Io(e))?;

        let final_content = if let Some(max) = max_lines {
            content
                .lines()
                .take(max as usize)
                .collect::<Vec<_>>()
                .join("\n")
        } else if content.chars().count() > 10240 {
            let total_chars = content.chars().count();
            format!(
                "{}\n... (文件过大，已截断，共 {} 字符)",
                content.chars().take(10240).collect::<String>(),
                total_chars
            )
        } else {
            content
        };

        Ok(ToolResult::ok(serde_json::json!({
            "path": path,
            "content": final_content
        })))
    }
}

// ============================================================
// read_files_batch
// ============================================================

/// 批量读取多个文本文件
pub struct ReadFilesBatchTool;

#[async_trait]
impl AgentTool for ReadFilesBatchTool {
    fn name(&self) -> &'static str {
        "read_files_batch"
    }

    fn description(&self) -> &'static str {
        "一次读取多个文本文件，返回每个文件的内容（单文件超 10240 字符截断）。适合同时对照多份日志、配置、文档。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "文件路径列表"
                },
                "max_files": { "type": "integer", "description": "最多读几个，默认 20" }
            },
            "required": ["paths"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let max_files = args["max_files"].as_u64().unwrap_or(20).min(50) as usize;
        let paths: Vec<String> = args["paths"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        if paths.is_empty() {
            return Ok(ToolResult::err("NO_PATHS", "paths 不能为空"));
        }

        let mut files: Vec<serde_json::Value> = Vec::new();
        let mut errors: Vec<serde_json::Value> = Vec::new();
        for path in paths.iter().take(max_files) {
            if !Path::new(path).exists() {
                errors.push(serde_json::json!({ "path": path, "error": "文件不存在" }));
                continue;
            }
            match fs::read_to_string(path) {
                Ok(content) => {
                    let total_chars = content.chars().count();
                    let truncated = total_chars > 10240;
                    let shown = if truncated {
                        content.chars().take(10240).collect::<String>()
                    } else {
                        content
                    };
                    files.push(serde_json::json!({
                        "path": path,
                        "chars": total_chars,
                        "truncated": truncated,
                        "content": shown,
                    }));
                }
                Err(e) => {
                    errors.push(serde_json::json!({ "path": path, "error": e.to_string() }));
                }
            }
        }

        Ok(ToolResult::ok(serde_json::json!({
            "file_count": files.len(),
            "error_count": errors.len(),
            "files": files,
            "errors": errors,
        })))
    }
}

// ============================================================
// 写入文件工具
// ============================================================

/// 将文本内容写入指定文件
pub struct WriteFileTool;

#[async_trait]
impl AgentTool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "将文本内容写入指定文件。如果文件不存在则创建，如果存在则覆盖。适用于创建笔记、保存配置等。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "要写入的文件路径"
                },
                "content": {
                    "type": "string",
                    "description": "要写入的文本内容"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;
        let content = args["content"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("content is required".to_string()))?;

        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(path, content)?;

        Ok(ToolResult::ok(serde_json::json!({
            "path": path,
            "bytes_written": content.len(),
            "message": format!("已写入文件: {}", path)
        })))
    }
}

// ============================================================
// 列出目录工具
// ============================================================

/// 列出指定目录中的文件和子目录
pub struct ListDirectoryTool;

#[async_trait]
impl AgentTool for ListDirectoryTool {
    fn name(&self) -> &'static str {
        "list_directory"
    }

    fn description(&self) -> &'static str {
        "列出指定目录中的文件和子目录。适用于查看文件夹内容。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "要列出的目录路径"
                }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let raw_path = args["path"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;
        // 解析环境变量（如 %USERPROFILE%\Desktop）
        let path = expand_env_vars(raw_path);

        if !Path::new(&path).exists() {
            return Ok(ToolResult::err(
                "DIR_NOT_FOUND",
                &format!("目录不存在: {}", path),
            ));
        }

        let entries = fs::read_dir(&path)?
            .filter_map(|entry| entry.ok())
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                let metadata = entry.metadata().ok();
                let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                serde_json::json!({
                    "name": name,
                    "is_directory": is_dir,
                    "size": size
                })
            })
            .collect::<Vec<_>>();

        Ok(ToolResult::ok(serde_json::json!({
            "path": path,
            "entries": entries,
            "count": entries.len()
        })))
    }
}

/// 解析 Windows 环境变量（如 %USERPROFILE% → C:\Users\xxx）
fn expand_env_vars(path: &str) -> String {
    let mut result = path.to_string();
    let mut start = 0;
    while let Some(pct1) = result[start..].find('%') {
        let abs_pct1 = start + pct1;
        if let Some(pct2) = result[abs_pct1 + 1..].find('%') {
            let abs_pct2 = abs_pct1 + 1 + pct2;
            let var_name = &result[abs_pct1 + 1..abs_pct2];
            if let Ok(val) = std::env::var(var_name) {
                result.replace_range(abs_pct1..=abs_pct2, &val);
                start = abs_pct1 + val.len();
            } else {
                start = abs_pct2 + 1;
            }
        } else {
            break;
        }
    }
    result
}
