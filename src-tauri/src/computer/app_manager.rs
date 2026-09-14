//! 应用管理器（V16 新增）
//!
//! 提供应用生命周期管理：状态查询、详细信息、重启、已安装应用枚举、批量关闭、工作区启动。
//! 所有工具通过 ApplicationManager 封装核心逻辑，工具层为薄包装。

use std::process::Command;
use std::time::Duration;

use async_trait::async_trait;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

use crate::error::AppResult;
use crate::tools::{AgentTool, RiskLevel, ToolResult};

// ============================================================
// ApplicationManager — 核心逻辑封装
// ============================================================

/// 应用管理器
///
/// 封装应用状态检测、生命周期管理、批量操作等核心逻辑。
/// 工具层调用此模块的方法，保持工具实现简洁。
pub struct ApplicationManager;

impl ApplicationManager {
    /// 检查应用是否在运行，返回 PID 列表
    pub fn find_processes_by_name(name: &str) -> AppResult<Vec<(u32, String)>> {
        let process_name = resolve_to_process_name(name);
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle", "Hidden",
                "-Command",
                &format!(
                    "Get-Process -Name '{}' -ErrorAction SilentlyContinue | Select-Object Id, ProcessName, Responding | ConvertTo-Json -Compress",
                    process_name.replace(".exe", "")
                ),
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| crate::error::AppError::ToolExecution(format!("PowerShell 执行失败: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let data: serde_json::Value = serde_json::from_str(trimmed).unwrap_or(serde_json::json!([]));
        let mut results = Vec::new();
        if let Some(arr) = data.as_array() {
            for p in arr {
                if let (Some(pid), Some(pname)) = (p.get("Id").and_then(|v| v.as_u64()), p.get("ProcessName").and_then(|v| v.as_str())) {
                    results.push((pid as u32, pname.to_string()));
                }
            }
        } else if let (Some(pid), Some(pname)) = (data.get("Id").and_then(|v| v.as_u64()), data.get("ProcessName").and_then(|v| v.as_str())) {
            results.push((pid as u32, pname.to_string()));
        }
        Ok(results)
    }

    /// 获取应用详细信息（PID、路径、版本、启动时间、内存、CPU、窗口标题、响应性）
    pub fn get_application_detail(name: &str) -> AppResult<Vec<serde_json::Value>> {
        let process_name = resolve_to_process_name(name);
        let ps_script = format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8\nGet-Process -Name '{}' -ErrorAction SilentlyContinue | ForEach-Object {{\n  $mainWindow = $_.MainWindowTitle\n  $path = $_.Path\n  $version = if ($path) {{ (Get-Item $path -ErrorAction SilentlyContinue).VersionInfo.ProductVersion }}\n  [PSCustomObject]@{{\n    pid=$_.Id; processName=$_.ProcessName; responding=$_.Responding;\n    memoryMB=[math]::Round($_.WorkingSet64/1MB,1); cpuSeconds=[math]::Round($_.CPU,1);\n    startTime=$_.StartTime.ToString('yyyy-MM-dd HH:mm:ss');\n    windowTitle=$mainWindow; path=$path; version=$version\n  }}\n}} | ConvertTo-Json -Compress",
            process_name.replace(".exe", "")
        );

        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &ps_script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| crate::error::AppError::ToolExecution(format!("PowerShell 执行失败: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let data: serde_json::Value = serde_json::from_str(trimmed).unwrap_or(serde_json::json!([]));
        if let Some(arr) = data.as_array() {
            Ok(arr.clone())
        } else {
            Ok(vec![data])
        }
    }

    /// 优雅关闭应用（先 WM_CLOSE，等待退出，超时则强制 kill）
    pub async fn graceful_close(name: &str, timeout_secs: u64) -> AppResult<bool> {
        let processes = Self::find_processes_by_name(name)?;
        if processes.is_empty() {
            return Ok(true); // 已经没在运行，视为关闭成功
        }

        // 尝试通过 close_window 优雅关闭（发送 WM_CLOSE）
        let process_name = resolve_to_process_name(name);
        let _ = Command::new("taskkill")
            .args(["/IM", &process_name])
            .creation_flags(CREATE_NO_WINDOW)
            .output(); // 不带 /F 是优雅关闭

        // 等待进程退出
        let start = std::time::Instant::now();
        while start.elapsed().as_secs() < timeout_secs {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let remaining = Self::find_processes_by_name(name)?;
            if remaining.is_empty() {
                return Ok(true);
            }
        }

        // 超时，强制结束
        let _ = Command::new("taskkill")
            .args(["/IM", &process_name, "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let remaining = Self::find_processes_by_name(name)?;
        Ok(remaining.is_empty())
    }

    /// 列出已安装应用（从注册表 Uninstall 键读取）
    pub fn list_installed_applications(filter: Option<&str>) -> AppResult<Vec<serde_json::Value>> {
        let filter_lower = filter.map(|f| f.to_lowercase());
        let ps_script = r#"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$apps = @()
$paths = @(
  'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
  'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
  'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*'
)
foreach ($path in $paths) {
  Get-ItemProperty $path -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName } | ForEach-Object {
    $apps += [PSCustomObject]@{
      name=$_.DisplayName; publisher=$_.Publisher; version=$_.DisplayVersion;
      installLocation=$_.InstallLocation; uninstallString=$_.UninstallString;
      installDate=$_.InstallDate
    }
  }
}
$apps | Sort-Object name -Unique | ConvertTo-Json -Compress
"#;
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", ps_script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| crate::error::AppError::ToolExecution(format!("PowerShell 执行失败: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let data: serde_json::Value = serde_json::from_str(trimmed).unwrap_or(serde_json::json!([]));
        let mut apps: Vec<serde_json::Value> = if let Some(arr) = data.as_array() {
            arr.clone()
        } else {
            vec![data]
        };

        // 按名称过滤
        if let Some(ref f) = filter_lower {
            apps.retain(|app| {
                app.get("name")
                    .and_then(|v| v.as_str())
                    .map(|n| n.to_lowercase().contains(f))
                    .unwrap_or(false)
            });
        }
        Ok(apps)
    }

    /// 关闭所有用户级应用窗口（排除系统进程和 PC Guardian 自身）
    pub async fn close_all_user_applications() -> AppResult<Vec<String>> {
        let excluded = [
            "explorer", "sihost", "taskhostw", "dwm", "winlogon", "csrss",
            "smss", "lsass", "services", "svchost", "fontdrvhost", "pc-guardian",
            "msedgewebview2", "conhost", "SearchHost", "StartMenuExperienceHost",
            "TextInputHost", "ShellExperienceHost", "RuntimeBroker", "SecurityHealthService",
            "SecurityHealthSystray", "WavesSvc", "RtkAudioService",
        ];

        let ps_script = r#"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
Get-Process | Where-Object { $_.MainWindowTitle -ne '' -and $_.Responding } | Select-Object Id, ProcessName, MainWindowTitle | ConvertTo-Json -Compress
"#;
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", ps_script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| crate::error::AppError::ToolExecution(format!("PowerShell 执行失败: {}", e)))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        let data: serde_json::Value = serde_json::from_str(trimmed).unwrap_or(serde_json::json!([]));
        let windows: Vec<&serde_json::Value> = if let Some(arr) = data.as_array() {
            arr.iter().collect()
        } else {
            vec![&data]
        };

        let mut closed = Vec::new();
        for w in windows {
            let pname = w.get("ProcessName").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
            if excluded.contains(&pname.as_str()) {
                continue;
            }
            if let Some(pid) = w.get("Id").and_then(|v| v.as_u64()) {
                let _ = Command::new("taskkill").args(["/PID", &pid.to_string()]).creation_flags(CREATE_NO_WINDOW).output();
                if let Some(title) = w.get("MainWindowTitle").and_then(|v| v.as_str()) {
                    if !title.is_empty() {
                        closed.push(title.to_string());
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
        Ok(closed)
    }
}

/// 将用户输入的应用名解析为进程名（.exe）
fn resolve_to_process_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let mapped = match lower.as_str() {
        "qq" => "QQ.exe",
        "wechat" | "微信" => "WeChat.exe",
        "chrome" | "google chrome" | "谷歌浏览器" => "chrome.exe",
        "edge" | "microsoft edge" | "edge浏览器" => "msedge.exe",
        "firefox" | "火狐" => "firefox.exe",
        "vscode" | "vs code" | "visual studio code" | "代码编辑器" => "Code.exe",
        "notepad" | "记事本" => "notepad.exe",
        "calc" | "计算器" => "calc.exe",
        "spotify" => "Spotify.exe",
        "discord" => "Discord.exe",
        "steam" => "steam.exe",
        "obs" | "obs studio" => "obs64.exe",
        "钉钉" | "dingtalk" => "DingTalk.exe",
        "飞书" | "feishu" | "lark" => "Feishu.exe",
        "网易云" | "网易云音乐" | "cloudmusic" => "cloudmusic.exe",
        "qq音乐" | "qqmusic" => "QQMusic.exe",
        "豆包" | "doubao" => "Doubao.exe",
        "watt" | "watt toolkit" | "steam++" => "Watt Toolkit.exe",
        "idea" | "intellij" | "intellij idea" => "idea64.exe",
        "pycharm" => "pycharm64.exe",
        "goland" => "goland64.exe",
        "webstorm" => "webstorm64.exe",
        "clion" => "clion64.exe",
        "rustrover" => "rustrover64.exe",
        "postman" => "Postman.exe",
        "docker" | "docker desktop" => "Docker Desktop.exe",
        "git" | "git bash" => "git-bash.exe",
        "terminal" | "windows terminal" | "wt" => "WindowsTerminal.exe",
        "powershell" => "powershell.exe",
        "cmd" | "命令提示符" => "cmd.exe",
        "excel" => "EXCEL.EXE",
        "word" => "WINWORD.EXE",
        "powerpoint" | "ppt" => "POWERPNT.EXE",
        "outlook" => "OUTLOOK.EXE",
        "onenote" => "ONENOTE.EXE",
        "photoshop" | "ps" => "Photoshop.exe",
        "illustrator" | "ai" => "Illustrator.exe",
        "premiere" | "pr" => "Premiere Pro.exe",
        "after effects" | "ae" => "AfterFX.exe",
        "blender" => "blender.exe",
        "figma" => "Figma.exe",
        "xmind" => "Xmind.exe",
        "typora" => "Typora.exe",
        "notion" => "Notion.exe",
        "evernote" | "印象笔记" => "Evernote.exe",
        "todoist" => "Todoist.exe",
        "todesk" => "ToDesk.exe",
        "向日葵" | "sunlogin" => "SunloginClient.exe",
        "teamviewer" => "TeamViewer.exe",
        "迅雷" | "xunlei" => "Thunder.exe",
        "百度网盘" | "baidunetdisk" => "baidunetdisk.exe",
        "阿里云盘" | "aliyundrive" => "AliyunPan.exe",
        "epic" | "epic games" => "EpicGamesLauncher.exe",
        "origin" => "Origin.exe",
        "uplay" | "ubisoft connect" => "upc.exe",
        "battle.net" | "战网" => "Battle.net.exe",
        "wegame" => "wegame.exe",
        "原神" | "genshin" | "genshin impact" => "GenshinImpact.exe",
        "星穹铁道" | "hsr" | "honkai star rail" => "StarRail.exe",
        "绝区零" | "zzz" | "zenless zone zero" => "ZenlessZoneZero.exe",
        "英雄联盟" | "lol" | "league of legends" => "LeagueClient.exe",
        "王者荣耀" => "Honor of Kings.exe",
        "和平精英" => "AndroidEmulator.exe",
        _ => "",
    };

    if !mapped.is_empty() {
        return mapped.to_string();
    }
    if lower.ends_with(".exe") {
        return name.to_string();
    }
    format!("{}.exe", name)
}

// ============================================================
// 工具实现
// ============================================================

/// 检查应用是否在运行（简化版，返回 bool + PID 列表）
pub struct IsApplicationRunningTool;

#[async_trait]
impl AgentTool for IsApplicationRunningTool {
    fn name(&self) -> &'static str { "is_application_running" }
    fn description(&self) -> &'static str {
        "检查指定应用是否正在运行。返回是否运行及 PID 列表。用于确认某个程序是否已打开。"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "应用名称（如'QQ'、'chrome'、'微信'）" }
            },
            "required": ["name"]
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Safe }
    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let name = args["name"].as_str().unwrap_or("");
        if name.is_empty() {
            return Ok(ToolResult::err("INVALID_ARGUMENT", "必须提供 name 参数"));
        }
        let processes = ApplicationManager::find_processes_by_name(name)?;
        let pids: Vec<u32> = processes.iter().map(|(pid, _)| *pid).collect();
        Ok(ToolResult::ok(serde_json::json!({
            "name": name,
            "running": !processes.is_empty(),
            "pids": pids,
            "count": processes.len(),
            "message": if processes.is_empty() {
                format!("{} 未在运行", name)
            } else {
                format!("{} 正在运行（{} 个进程，PID: {:?}）", name, processes.len(), pids)
            }
        })))
    }
}

/// 查询应用运行状态（运行中/响应中/无响应/已关闭）
pub struct GetApplicationStatusTool;

#[async_trait]
impl AgentTool for GetApplicationStatusTool {
    fn name(&self) -> &'static str { "get_application_status" }
    fn description(&self) -> &'static str {
        "查询指定应用的运行状态：运行中且响应、运行中但无响应、已关闭。通过进程存在性和窗口响应性检测。"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "应用名称（如'QQ'、'chrome'）" }
            },
            "required": ["name"]
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Safe }
    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let name = args["name"].as_str().unwrap_or("");
        if name.is_empty() {
            return Ok(ToolResult::err("INVALID_ARGUMENT", "必须提供 name 参数"));
        }
        let details = ApplicationManager::get_application_detail(name)?;
        if details.is_empty() {
            return Ok(ToolResult::ok(serde_json::json!({
                "name": name, "status": "closed", "running": false,
                "message": format!("{} 已关闭（未找到进程）", name)
            })));
        }
        let all_responding = details.iter().all(|d| d.get("responding").and_then(|v| v.as_bool()).unwrap_or(true));
        let status = if all_responding { "running" } else { "not_responding" };
        let status_text = if all_responding { "运行中（响应正常）" } else { "运行中（无响应）" };
        Ok(ToolResult::ok(serde_json::json!({
            "name": name, "status": status, "running": true,
            "process_count": details.len(),
            "all_responding": all_responding,
            "message": format!("{} {}", name, status_text)
        })))
    }
}

/// 获取应用详细信息
pub struct GetApplicationInfoTool;

#[async_trait]
impl AgentTool for GetApplicationInfoTool {
    fn name(&self) -> &'static str { "get_application_info" }
    fn description(&self) -> &'static str {
        "返回应用详细信息：PID、可执行路径、版本号、启动时间、内存占用(MB)、CPU占用(秒)、窗口标题、是否响应。"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "应用名称" }
            },
            "required": ["name"]
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Safe }
    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let name = args["name"].as_str().unwrap_or("");
        if name.is_empty() {
            return Ok(ToolResult::err("INVALID_ARGUMENT", "必须提供 name 参数"));
        }
        let details = ApplicationManager::get_application_detail(name)?;
        if details.is_empty() {
            return Ok(ToolResult::err("APP_NOT_FOUND", &format!("未找到运行中的应用: {}", name)));
        }
        Ok(ToolResult::ok(serde_json::json!({
            "name": name,
            "processes": details,
            "count": details.len(),
            "message": format!("找到 {} 个 {} 进程", details.len(), name)
        })))
    }
}

/// 重启应用（优雅关闭后重新启动）
pub struct RestartApplicationTool;

#[async_trait]
impl AgentTool for RestartApplicationTool {
    fn name(&self) -> &'static str { "restart_application" }
    fn description(&self) -> &'static str {
        "重启指定应用：先优雅关闭（等待进程退出，超时则强制结束），再重新启动。用于应用卡死或需要刷新时。"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "应用名称（如'QQ'、'chrome'）" },
                "wait_timeout": { "type": "integer", "description": "等待关闭超时秒数，默认 10" }
            },
            "required": ["name"]
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Medium }
    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let name = args["name"].as_str().unwrap_or("");
        if name.is_empty() {
            return Ok(ToolResult::err("INVALID_ARGUMENT", "必须提供 name 参数"));
        }
        let timeout_secs = args["wait_timeout"].as_i64().unwrap_or(10) as u64;

        // 记录原始 PID（用于确认重启后是新进程）
        let original_pids: Vec<u32> = ApplicationManager::find_processes_by_name(name)?
            .iter().map(|(pid, _)| *pid).collect();

        // 优雅关闭
        let closed = ApplicationManager::graceful_close(name, timeout_secs).await?;
        if !closed {
            return Ok(ToolResult::err("CLOSE_FAILED", &format!("关闭 {} 失败（超时 {} 秒后仍有进程）", name, timeout_secs)));
        }

        // 重新启动
        let launch_result = crate::tools::executors::program::LaunchProgramTool;
        let launch_args = serde_json::json!({ "name_or_path": name });
        let result = launch_result.execute(launch_args).await?;

        if result.success {
            // 等待新进程出现
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let new_pids: Vec<u32> = ApplicationManager::find_processes_by_name(name)?
                .iter().map(|(pid, _)| *pid).collect();
            let is_new = !new_pids.is_empty() && new_pids.iter().any(|p| !original_pids.contains(p));
            Ok(ToolResult::ok(serde_json::json!({
                "name": name,
                "restarted": is_new,
                "original_pids": original_pids,
                "new_pids": new_pids,
                "message": if is_new {
                    format!("{} 已重启（新 PID: {:?}）", name, new_pids)
                } else {
                    format!("{} 已发送启动命令，请确认是否成功启动", name)
                }
            })))
        } else {
            Ok(result)
        }
    }
}

/// 列出已安装应用
pub struct ListInstalledApplicationsTool;

#[async_trait]
impl AgentTool for ListInstalledApplicationsTool {
    fn name(&self) -> &'static str { "list_installed_applications" }
    fn description(&self) -> &'static str {
        "列出系统已安装的应用（从注册表读取），返回名称、发布者、版本、安装路径、卸载命令。支持按名称搜索过滤。"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "search": { "type": "string", "description": "按名称搜索过滤（可选）" },
                "limit": { "type": "integer", "description": "返回数量上限，默认 50" }
            }
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Safe }
    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let search = args["search"].as_str();
        let limit = args["limit"].as_i64().unwrap_or(50) as usize;

        let apps = ApplicationManager::list_installed_applications(search)?;
        let total = apps.len();
        let limited: Vec<&serde_json::Value> = apps.iter().take(limit).collect();

        Ok(ToolResult::ok(serde_json::json!({
            "applications": limited,
            "total": total,
            "returned": limited.len(),
            "search": search,
            "message": if let Some(s) = search {
                format!("搜索 '{}' 找到 {} 个已安装应用（显示前 {} 个）", s, total, limited.len())
            } else {
                format!("系统共安装 {} 个应用（显示前 {} 个）", total, limited.len())
            }
        })))
    }
}

/// 关闭所有用户级应用
pub struct CloseAllApplicationsTool;

#[async_trait]
impl AgentTool for CloseAllApplicationsTool {
    fn name(&self) -> &'static str { "close_all_applications" }
    fn description(&self) -> &'static str {
        "关闭所有用户级应用窗口（排除系统进程、资源管理器、PC Guardian 自身）。高风险操作，需二次确认。用于一键清理桌面、准备关机等场景。"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "confirm": { "type": "boolean", "description": "确认执行（必须为 true）" }
            },
            "required": ["confirm"]
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::High }
    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let confirm = args["confirm"].as_bool().unwrap_or(false);
        if !confirm {
            return Ok(ToolResult::err("CONFIRMATION_REQUIRED", "此操作会关闭所有应用，请设置 confirm=true 确认执行"));
        }
        let closed = ApplicationManager::close_all_user_applications().await?;
        Ok(ToolResult::ok(serde_json::json!({
            "closed_count": closed.len(),
            "closed_windows": closed,
            "message": format!("已关闭 {} 个应用窗口", closed.len())
        })))
    }
}

/// 启动工作区（一组预设应用）
pub struct LaunchWorkspaceTool;

#[async_trait]
impl AgentTool for LaunchWorkspaceTool {
    fn name(&self) -> &'static str { "launch_workspace" }
    fn description(&self) -> &'static str {
        "启动一组预设应用（工作区）。内置预设：开发环境(VSCode+Chrome+Terminal)、设计环境(Photoshop+Figma+Chrome)、办公环境(Word+Excel+Outlook)、娱乐环境(Steam+Discord+Spotify)。也支持自定义应用列表。"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "preset": {
                    "type": "string",
                    "description": "预设工作区名称：dev（开发）、design（设计）、office（办公）、gaming（娱乐）"
                },
                "apps": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "自定义应用名称列表（与 preset 二选一）"
                }
            }
        })
    }
    fn risk_level(&self) -> RiskLevel { RiskLevel::Medium }
    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let preset = args["preset"].as_str();
        let custom_apps: Vec<String> = args["apps"].as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        let apps: Vec<&str> = if !custom_apps.is_empty() {
            custom_apps.iter().map(|s| s.as_str()).collect()
        } else if let Some(p) = preset {
            match p.to_lowercase().as_str() {
                "dev" | "开发" => vec!["vscode", "chrome", "terminal"],
                "design" | "设计" => vec!["photoshop", "figma", "chrome"],
                "office" | "办公" => vec!["word", "excel", "outlook"],
                "gaming" | "娱乐" => vec!["steam", "discord", "spotify"],
                _ => vec![],
            }
        } else {
            vec![]
        };

        if apps.is_empty() {
            return Ok(ToolResult::err("INVALID_ARGUMENT", "必须提供 preset 或 apps 参数"));
        }

        let launcher = crate::tools::executors::program::LaunchProgramTool;
        let mut results = Vec::new();
        let mut success_count = 0;

        for app in &apps {
            let launch_args = serde_json::json!({ "name_or_path": app });
            match launcher.execute(launch_args).await {
                Ok(r) => {
                    if r.success {
                        success_count += 1;
                        results.push(serde_json::json!({ "app": app, "success": true }));
                    } else {
                        let err = r.error.unwrap_or_else(|| "启动失败".to_string());
                        results.push(serde_json::json!({ "app": app, "success": false, "error": err }));
                    }
                }
                Err(e) => {
                    results.push(serde_json::json!({ "app": app, "success": false, "error": e.to_string() }));
                }
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }

        let workspace_name = preset.unwrap_or("custom");
        Ok(ToolResult::ok(serde_json::json!({
            "workspace": workspace_name,
            "apps": apps,
            "success_count": success_count,
            "total": apps.len(),
            "results": results,
            "message": format!("工作区 '{}' 启动完成：{}/{} 成功", workspace_name, success_count, apps.len())
        })))
    }
}
