//! 验证引擎
//!
//! 工具执行成功 ≠ 任务完成。Verification 负责验证工具执行的真实结果。
//!
//! 例如：
//! - launch_program 返回 success，但进程可能没真正启动
//! - close_program 返回 success，但进程可能还在运行
//! - write_file 返回 success，但文件可能没真正写入

use crate::tools::ToolResult;

/// 验证结果
#[derive(Debug, Clone)]
pub enum VerificationResult {
    /// 验证通过
    Pass { summary: String },
    /// 验证失败
    Fail { reason: String },
}

/// 验证引擎
pub struct VerificationEngine;

impl VerificationEngine {
    pub fn new() -> Self {
        Self
    }

    /// 验证工具执行结果
    ///
    /// # Arguments
    /// * `tool_name` - 工具名称
    /// * `arguments` - 工具参数
    /// * `result` - 工具执行结果
    ///
    /// # Returns
    /// VerificationResult
    pub fn verify(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        result: &ToolResult,
    ) -> VerificationResult {
        // 如果工具本身返回失败，直接验证失败
        if !result.success {
            return VerificationResult::Fail {
                reason: result
                    .error
                    .clone()
                    .unwrap_or_else(|| "工具执行失败".to_string()),
            };
        }

        match tool_name {
            "launch_program" => self.verify_launch_program(arguments),
            "close_program" => self.verify_close_program(arguments),
            "write_file" => self.verify_write_file(arguments),
            "read_file" => self.verify_read_file(result),
            "list_directory" => self.verify_list_directory(result),
            "execute_command" => self.verify_execute_command(result),
            "take_screenshot" => self.verify_screenshot(result),
            // V12 Computer State 工具
            "list_processes" => self.verify_list_processes(result),
            "kill_process" => self.verify_kill_process(arguments),
            "get_active_window" => self.verify_get_active_window(result),
            "list_windows" => self.verify_list_windows(result),
            "focus_window" => self.verify_focus_window(arguments, result),
            "minimize_window" => self.verify_minimize_window(arguments, result),
            "close_window" => self.verify_close_window(arguments, result),
            // 文件操作工具
            "create_directory" => self.verify_create_directory(arguments),
            "delete_file" => self.verify_delete_file(arguments),
            "copy_file" => self.verify_copy_file(arguments, result),
            "move_file" => self.verify_move_file(arguments, result),
            "open_file" => self.verify_open_file(result),
            "find_program" => self.verify_find_program(result),
            _ => {
                // 默认信任工具返回的 success
                VerificationResult::Pass {
                    summary: "工具执行成功".to_string(),
                }
            }
        }
    }

    /// 验证 launch_program：检查进程是否真正启动
    fn verify_launch_program(&self, arguments: &serde_json::Value) -> VerificationResult {
        let name_or_path = arguments
            .get("name_or_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if name_or_path.is_empty() {
            return VerificationResult::Fail {
                reason: "无法获取程序名".to_string(),
            };
        }

        // 从路径中提取进程名（如 C:\...\QQ.exe -> QQ.exe）
        let process_name = std::path::Path::new(name_or_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| name_or_path.to_string());

        // 如果进程名不包含 .exe，尝试添加
        let process_name = if process_name.to_lowercase().ends_with(".exe") {
            process_name
        } else {
            format!("{}.exe", process_name)
        };

        // 用 tasklist 检查进程是否存在
        match check_process_exists(&process_name) {
            Ok(true) => VerificationResult::Pass {
                summary: format!("进程 {} 已启动", process_name),
            },
            Ok(false) => VerificationResult::Fail {
                reason: format!("进程 {} 未在运行，启动可能失败", process_name),
            },
            Err(e) => {
                // 无法检查时，信任工具返回值
                eprintln!("[Verification] 检查进程失败: {}", e);
                VerificationResult::Pass {
                    summary: "工具返回启动成功（进程状态无法确认）".to_string(),
                }
            }
        }
    }

    /// 验证 close_program：检查进程是否真正关闭
    fn verify_close_program(&self, arguments: &serde_json::Value) -> VerificationResult {
        let name = arguments
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if name.is_empty() {
            return VerificationResult::Fail {
                reason: "无法获取程序名".to_string(),
            };
        }

        // 映射到进程名
        let process_name = resolve_process_name_for_check(name);

        // 等待一小段时间让进程退出
        std::thread::sleep(std::time::Duration::from_millis(500));

        match check_process_exists(&process_name) {
            Ok(false) => VerificationResult::Pass {
                summary: format!("进程 {} 已关闭", process_name),
            },
            Ok(true) => VerificationResult::Fail {
                reason: format!("进程 {} 仍在运行，关闭可能失败", process_name),
            },
            Err(e) => {
                eprintln!("[Verification] 检查进程失败: {}", e);
                VerificationResult::Pass {
                    summary: "工具返回关闭成功（进程状态无法确认）".to_string(),
                }
            }
        }
    }

    /// 验证 write_file：检查文件是否存在
    fn verify_write_file(&self, arguments: &serde_json::Value) -> VerificationResult {
        let path = arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if path.is_empty() {
            return VerificationResult::Fail {
                reason: "无法获取文件路径".to_string(),
            };
        }

        if std::path::Path::new(path).exists() {
            VerificationResult::Pass {
                summary: format!("文件 {} 已写入", path),
            }
        } else {
            VerificationResult::Fail {
                reason: format!("文件 {} 不存在，写入可能失败", path),
            }
        }
    }

    /// 验证 read_file：检查返回内容
    fn verify_read_file(&self, result: &ToolResult) -> VerificationResult {
        if let Some(data) = &result.data {
            let content = data.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if !content.is_empty() {
                VerificationResult::Pass {
                    summary: format!("读取成功，共 {} 字符", content.len()),
                }
            } else {
                VerificationResult::Pass {
                    summary: "文件为空".to_string(),
                }
            }
        } else {
            VerificationResult::Fail {
                reason: "读取结果为空".to_string(),
            }
        }
    }

    /// 验证 list_directory
    fn verify_list_directory(&self, result: &ToolResult) -> VerificationResult {
        if let Some(data) = &result.data {
            let entries = data.get("entries").and_then(|v| v.as_array());
            let count = entries.map(|e| e.len()).unwrap_or(0);
            VerificationResult::Pass {
                summary: format!("目录列出成功，共 {} 项", count),
            }
        } else {
            VerificationResult::Fail {
                reason: "目录列表为空".to_string(),
            }
        }
    }

    /// 验证 execute_command：检查 exit_code
    fn verify_execute_command(&self, result: &ToolResult) -> VerificationResult {
        if let Some(data) = &result.data {
            let exit_code = data.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(-1);
            if exit_code == 0 {
                VerificationResult::Pass {
                    summary: "命令执行成功（exit_code=0）".to_string(),
                }
            } else {
                VerificationResult::Fail {
                    reason: format!("命令退出码非零: {}", exit_code),
                }
            }
        } else {
            VerificationResult::Pass {
                summary: "命令执行完成".to_string(),
            }
        }
    }

    /// 验证截图
    fn verify_screenshot(&self, result: &ToolResult) -> VerificationResult {
        if let Some(data) = &result.data {
            let path = data.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if !path.is_empty() && std::path::Path::new(path).exists() {
                VerificationResult::Pass {
                    summary: format!("截图已保存: {}", path),
                }
            } else {
                VerificationResult::Fail {
                    reason: "截图文件不存在".to_string(),
                }
            }
        } else {
            VerificationResult::Fail {
                reason: "截图结果为空".to_string(),
            }
        }
    }

    // ============================================================
    // V12 Computer State 工具验证
    // ============================================================

    /// 验证 list_processes
    fn verify_list_processes(&self, result: &ToolResult) -> VerificationResult {
        if let Some(data) = &result.data {
            let processes = data.get("processes").and_then(|v| v.as_array());
            let count = processes.map(|p| p.len()).unwrap_or(0);
            if count > 0 {
                VerificationResult::Pass {
                    summary: format!("获取到 {} 个进程", count),
                }
            } else {
                VerificationResult::Fail {
                    reason: "进程列表为空".to_string(),
                }
            }
        } else {
            VerificationResult::Fail {
                reason: "进程查询结果为空".to_string(),
            }
        }
    }

    /// 验证 kill_process：检查进程是否真正终止
    fn verify_kill_process(&self, arguments: &serde_json::Value) -> VerificationResult {
        let pid = arguments.get("pid").and_then(|v| v.as_i64());
        let name = arguments.get("name").and_then(|v| v.as_str());

        // 等待进程退出
        std::thread::sleep(std::time::Duration::from_millis(500));

        if let Some(pid) = pid {
            // 用 tasklist 检查指定 PID 是否存在
            match check_pid_exists(pid as u32) {
                Ok(false) => VerificationResult::Pass {
                    summary: format!("进程 PID {} 已终止", pid),
                },
                Ok(true) => VerificationResult::Fail {
                    reason: format!("进程 PID {} 仍在运行", pid),
                },
                Err(e) => {
                    eprintln!("[Verification] 检查进程失败: {}", e);
                    VerificationResult::Pass {
                        summary: "工具返回终止成功（进程状态无法确认）".to_string(),
                    }
                }
            }
        } else if let Some(name) = name {
            let process_name = resolve_process_name_for_check(name);
            match check_process_exists(&process_name) {
                Ok(false) => VerificationResult::Pass {
                    summary: format!("进程 {} 已终止", process_name),
                },
                Ok(true) => VerificationResult::Fail {
                    reason: format!("进程 {} 仍在运行", process_name),
                },
                Err(e) => {
                    eprintln!("[Verification] 检查进程失败: {}", e);
                    VerificationResult::Pass {
                        summary: "工具返回终止成功（进程状态无法确认）".to_string(),
                    }
                }
            }
        } else {
            VerificationResult::Fail {
                reason: "无法获取进程标识".to_string(),
            }
        }
    }

    /// 验证 get_active_window
    fn verify_get_active_window(&self, result: &ToolResult) -> VerificationResult {
        if let Some(data) = &result.data {
            let title = data.get("title").and_then(|v| v.as_str()).unwrap_or("");
            if !title.is_empty() {
                VerificationResult::Pass {
                    summary: format!("当前活动窗口: {}", title),
                }
            } else {
                VerificationResult::Fail {
                    reason: "无法获取活动窗口标题".to_string(),
                }
            }
        } else {
            VerificationResult::Fail {
                reason: "活动窗口查询结果为空".to_string(),
            }
        }
    }

    /// 验证 list_windows
    fn verify_list_windows(&self, result: &ToolResult) -> VerificationResult {
        if let Some(data) = &result.data {
            let windows = data.get("windows").and_then(|v| v.as_array());
            let count = windows.map(|w| w.len()).unwrap_or(0);
            if count > 0 {
                VerificationResult::Pass {
                    summary: format!("获取到 {} 个窗口", count),
                }
            } else {
                VerificationResult::Fail {
                    reason: "窗口列表为空".to_string(),
                }
            }
        } else {
            VerificationResult::Fail {
                reason: "窗口查询结果为空".to_string(),
            }
        }
    }

    /// 验证 focus_window
    fn verify_focus_window(
        &self,
        arguments: &serde_json::Value,
        result: &ToolResult,
    ) -> VerificationResult {
        if let Some(data) = &result.data {
            if data.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
                let title = data.get("title").and_then(|v| v.as_str()).unwrap_or("");
                VerificationResult::Pass {
                    summary: format!("已聚焦窗口: {}", title),
                }
            } else {
                let reason = data
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("聚焦失败");
                VerificationResult::Fail {
                    reason: reason.to_string(),
                }
            }
        } else {
            let _ = arguments;
            VerificationResult::Fail {
                reason: "聚焦窗口结果为空".to_string(),
            }
        }
    }

    /// 验证 minimize_window
    fn verify_minimize_window(
        &self,
        _arguments: &serde_json::Value,
        result: &ToolResult,
    ) -> VerificationResult {
        if let Some(data) = &result.data {
            if data.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
                let title = data.get("title").and_then(|v| v.as_str()).unwrap_or("");
                VerificationResult::Pass {
                    summary: format!("已最小化窗口: {}", title),
                }
            } else {
                let reason = data
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("最小化失败");
                VerificationResult::Fail {
                    reason: reason.to_string(),
                }
            }
        } else {
            VerificationResult::Fail {
                reason: "最小化窗口结果为空".to_string(),
            }
        }
    }

    /// 验证 close_window
    fn verify_close_window(
        &self,
        _arguments: &serde_json::Value,
        result: &ToolResult,
    ) -> VerificationResult {
        if let Some(data) = &result.data {
            if data.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
                let title = data.get("title").and_then(|v| v.as_str()).unwrap_or("");
                VerificationResult::Pass {
                    summary: format!("已发送关闭消息: {}", title),
                }
            } else {
                let reason = data
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("关闭失败");
                VerificationResult::Fail {
                    reason: reason.to_string(),
                }
            }
        } else {
            VerificationResult::Fail {
                reason: "关闭窗口结果为空".to_string(),
            }
        }
    }

    // ============================================================
    // 文件操作工具验证
    // ============================================================

    /// 验证 create_directory
    fn verify_create_directory(&self, arguments: &serde_json::Value) -> VerificationResult {
        let path = arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if path.is_empty() {
            return VerificationResult::Fail {
                reason: "无法获取目录路径".to_string(),
            };
        }
        if std::path::Path::new(path).is_dir() {
            VerificationResult::Pass {
                summary: format!("目录已创建: {}", path),
            }
        } else {
            VerificationResult::Fail {
                reason: format!("目录不存在，创建可能失败: {}", path),
            }
        }
    }

    /// 验证 delete_file
    fn verify_delete_file(&self, arguments: &serde_json::Value) -> VerificationResult {
        let path = arguments
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if path.is_empty() {
            return VerificationResult::Fail {
                reason: "无法获取文件路径".to_string(),
            };
        }
        // 等待文件系统操作完成
        std::thread::sleep(std::time::Duration::from_millis(200));
        if !std::path::Path::new(path).exists() {
            VerificationResult::Pass {
                summary: format!("文件已删除: {}", path),
            }
        } else {
            VerificationResult::Fail {
                reason: format!("文件仍存在，删除可能失败: {}", path),
            }
        }
    }

    /// 验证 copy_file
    fn verify_copy_file(
        &self,
        arguments: &serde_json::Value,
        _result: &ToolResult,
    ) -> VerificationResult {
        let dest = arguments
            .get("destination")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if dest.is_empty() {
            return VerificationResult::Fail {
                reason: "无法获取目标路径".to_string(),
            };
        }
        if std::path::Path::new(dest).exists() {
            VerificationResult::Pass {
                summary: format!("文件已复制到: {}", dest),
            }
        } else {
            VerificationResult::Fail {
                reason: format!("目标文件不存在，复制可能失败: {}", dest),
            }
        }
    }

    /// 验证 move_file
    fn verify_move_file(
        &self,
        arguments: &serde_json::Value,
        _result: &ToolResult,
    ) -> VerificationResult {
        let src = arguments.get("src").and_then(|v| v.as_str()).unwrap_or("");
        let dest = arguments
            .get("destination")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if src.is_empty() || dest.is_empty() {
            return VerificationResult::Fail {
                reason: "无法获取源或目标路径".to_string(),
            };
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        let src_exists = std::path::Path::new(src).exists();
        let dest_exists = std::path::Path::new(dest).exists();
        if !src_exists && dest_exists {
            VerificationResult::Pass {
                summary: format!("文件已从 {} 移动到 {}", src, dest),
            }
        } else if src_exists && dest_exists {
            VerificationResult::Fail {
                reason: format!("源文件仍存在，移动可能失败: {}", src),
            }
        } else {
            VerificationResult::Fail {
                reason: "移动结果异常".to_string(),
            }
        }
    }

    /// 验证 open_file
    fn verify_open_file(&self, result: &ToolResult) -> VerificationResult {
        if let Some(data) = &result.data {
            let path = data.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if !path.is_empty() {
                VerificationResult::Pass {
                    summary: format!("已打开文件: {}", path),
                }
            } else {
                VerificationResult::Fail {
                    reason: "打开文件结果为空".to_string(),
                }
            }
        } else {
            VerificationResult::Fail {
                reason: "打开文件结果为空".to_string(),
            }
        }
    }

    /// 验证 find_program
    fn verify_find_program(&self, result: &ToolResult) -> VerificationResult {
        if let Some(data) = &result.data {
            let found = data.get("found").and_then(|v| v.as_bool()).unwrap_or(false);
            let path = data.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if found && !path.is_empty() {
                VerificationResult::Pass {
                    summary: format!("找到程序: {}", path),
                }
            } else if found {
                VerificationResult::Pass {
                    summary: "程序已找到".to_string(),
                }
            } else {
                VerificationResult::Fail {
                    reason: "未找到程序".to_string(),
                }
            }
        } else {
            VerificationResult::Fail {
                reason: "程序查找结果为空".to_string(),
            }
        }
    }
}

impl Default for VerificationEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// 检查进程是否存在（Windows tasklist）
fn check_process_exists(process_name: &str) -> Result<bool, String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("tasklist")
            .args(&["/FI", &format!("IMAGENAME eq {}", process_name), "/NH"])
            .creation_flags(0x08000000)
            .output()
            .map_err(|e| e.to_string())?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        // 如果输出包含进程名，说明进程存在
        Ok(stdout.contains(process_name))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let output = std::process::Command::new("pgrep")
            .arg(process_name)
            .output()
            .map_err(|e| e.to_string())?;
        Ok(output.status.success())
    }
}

/// 检查指定 PID 的进程是否存在（Windows tasklist）
fn check_pid_exists(pid: u32) -> Result<bool, String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("tasklist")
            .args(&["/FI", &format!("PID eq {}", pid), "/NH"])
            .creation_flags(0x08000000)
            .output()
            .map_err(|e| e.to_string())?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains(&format!(" {}", pid)))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let output = std::process::Command::new("ps")
            .args(&["-p", &pid.to_string()])
            .output()
            .map_err(|e| e.to_string())?;
        Ok(output.status.success())
    }
}

/// 映射程序名到进程名（用于验证）
fn resolve_process_name_for_check(name: &str) -> String {
    let lower = name.to_lowercase();
    match lower.as_str() {
        "qq" => "QQ.exe".to_string(),
        "wechat" | "微信" => "WeChat.exe".to_string(),
        "chrome" | "google chrome" => "chrome.exe".to_string(),
        "edge" | "microsoft edge" => "msedge.exe".to_string(),
        "firefox" => "firefox.exe".to_string(),
        "vscode" | "vs code" | "visual studio code" => "Code.exe".to_string(),
        "notepad" => "notepad.exe".to_string(),
        _ => {
            if lower.ends_with(".exe") {
                name.to_string()
            } else {
                format!("{}.exe", name)
            }
        }
    }
}
