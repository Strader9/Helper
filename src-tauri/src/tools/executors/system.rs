//! 系统操作工具
//!
//! 提供系统级操作：执行 shell 命令、截取屏幕。

use std::process::{Command, Stdio};

use async_trait::async_trait;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};
use crate::security::command_policy::CommandPolicy;

// ============================================================
// 执行命令工具
// ============================================================

/// 执行 shell 命令并返回输出
pub struct ExecuteCommandTool;

#[async_trait]
impl AgentTool for ExecuteCommandTool {
    fn name(&self) -> &'static str {
        "execute_command"
    }

    fn description(&self) -> &'static str {
        "执行 shell 命令并返回输出。当需要运行系统命令、获取系统信息时使用此工具。注意：此工具风险等级为 HIGH，需要用户确认。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "要执行的命令（Windows 下会自动添加 cmd /c 前缀）"
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "命令参数（可选）"
                },
                "cwd": {
                    "type": "string",
                    "description": "工作目录（可选）"
                }
            },
            "required": ["command"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::High
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let command = args["command"].as_str()
            .ok_or_else(|| AppError::InvalidArgument("command is required".to_string()))?;
        let cmd_args: Vec<String> = args["args"].as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let cwd = args["cwd"].as_str();

        // Command Policy：危险命令拦截
        match CommandPolicy::check(command, &cmd_args) {
            crate::security::command_policy::CommandPolicyResult::Deny { reason, code } => {
                return Ok(ToolResult::err(&code, &reason));
            }
            crate::security::command_policy::CommandPolicyResult::Allow => {}
        }

        let mut cmd = Command::new("cmd");
        cmd.arg("/c").arg(command);

        if !cmd_args.is_empty() {
            cmd.args(&cmd_args);
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        // 隐藏控制台窗口，防止闪烁
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        // 使用 spawn_blocking 避免阻塞异步运行时
        let output = tokio::task::spawn_blocking(move || cmd.output())
            .await
            .map_err(|e| AppError::Internal(format!("命令执行失败: {}", e)))??;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        // 限制输出大小，防止超大输出
        let max_len = 5000;
        let final_stdout = if stdout.len() > max_len {
            format!("{}\n... (stdout 已截断，共 {} 字符)", &stdout[..max_len], stdout.len())
        } else {
            stdout.to_string()
        };
        let final_stderr = if stderr.len() > max_len {
            format!("{}\n... (stderr 已截断，共 {} 字符)", &stderr[..max_len], stderr.len())
        } else {
            stderr.to_string()
        };

        Ok(ToolResult::ok(serde_json::json!({
            "stdout": final_stdout,
            "stderr": final_stderr,
            "exit_code": exit_code,
            "success": output.status.success()
        })))
    }
}

// ============================================================
// 截屏工具
// ============================================================

/// 截取屏幕并保存到临时目录
pub struct TakeScreenshotTool;

#[async_trait]
impl AgentTool for TakeScreenshotTool {
    fn name(&self) -> &'static str {
        "take_screenshot"
    }

    fn description(&self) -> &'static str {
        "截取当前屏幕并保存为图片。返回保存的图片路径。适用于用户需要查看屏幕内容时。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        let temp_dir = std::env::temp_dir();
        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let filename = format!("screenshot_{}.png", timestamp);
        let path = temp_dir.join(&filename);

        #[cfg(target_os = "windows")]
        {
            let path_str = path.to_string_lossy().replace("'", "''");
            let ps_script = format!(
                r#"Add-Type -AssemblyName System.Windows.Forms,System.Drawing
$screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bitmap = New-Object System.Drawing.Bitmap($screen.Width, $screen.Height)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($screen.Location, [System.Drawing.Point]::Empty, $screen.Size)
$bitmap.Save('{}')
$bitmap.Dispose()
$graphics.Dispose()
"#,
                path_str
            );

            let output = Command::new("powershell")
                .args(&["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-ExecutionPolicy", "Bypass", "-Command", &ps_script])
                .creation_flags(CREATE_NO_WINDOW)
                .output()?;

            if output.status.success() && path.exists() {
                Ok(ToolResult::ok(serde_json::json!({
                    "path": path.to_string_lossy().to_string(),
                    "message": format!("截图已保存: {}", path.display())
                })))
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Ok(ToolResult::err("SCREENSHOT_FAILED", &format!("截图失败: {}", stderr)))
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "截图功能仅在 Windows 上可用"))
        }
    }
}
