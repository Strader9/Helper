//! 窗口管理工具
//!
//! 提供 get_active_window、list_windows、focus_window、minimize_window、close_window 等工具。
//! 通过 PowerShell Add-Type 调用 user32.dll Windows API。

use std::process::Command;
use crate::tools::{AgentTool, RiskLevel, ToolResult};
use async_trait::async_trait;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// PowerShell 脚本：定义 Win32 API 类型并执行操作
/// 所有窗口工具共用这个 API 定义前缀
/// 注意：必须设置输出编码为 UTF-8，否则中文窗口标题会乱码
const WIN32_API_DEF: &str = r#"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class Win32 {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, StringBuilder lpString, int nMaxCount);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);
    public const int SW_MINIMIZE = 6;
    public const int SW_RESTORE = 9;
    public const uint WM_CLOSE = 0x0010;
}
"@
"#;

/// 获取当前活动窗口
pub struct GetActiveWindowTool;

#[async_trait]
impl AgentTool for GetActiveWindowTool {
    fn name(&self) -> &'static str {
        "get_active_window"
    }

    fn description(&self) -> &'static str {
        "获取当前前台活动窗口的标题和所属进程。用于了解用户当前正在操作什么程序。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: serde_json::Value) -> crate::error::AppResult<ToolResult> {
        #[cfg(target_os = "windows")]
        {
            let ps_script = format!(
                "{}\n$h = [Win32]::GetForegroundWindow()\n$sb = New-Object System.Text.StringBuilder 256\n[Win32]::GetWindowText($h, $sb, 256) | Out-Null\n$procId = 0\n[Win32]::GetWindowThreadProcessId($h, [ref]$procId) | Out-Null\n$proc = Get-Process -Id $procId -ErrorAction SilentlyContinue\n[PSCustomObject]@{{hwnd=$h.ToInt64(); title=$sb.ToString(); processName=$proc.ProcessName; pid=$procId}} | ConvertTo-Json -Compress",
                WIN32_API_DEF
            );
            run_ps_json(&ps_script, "获取活动窗口失败")
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "仅 Windows 可用"))
        }
    }
}

/// 列出所有可见窗口
pub struct ListWindowsTool;

#[async_trait]
impl AgentTool for ListWindowsTool {
    fn name(&self) -> &'static str {
        "list_windows"
    }

    fn description(&self) -> &'static str {
        "列出所有可见的顶层窗口，返回窗口标题、所属进程和句柄。用于了解当前打开了哪些窗口。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "description": "返回窗口数量上限，默认 30"}
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> crate::error::AppResult<ToolResult> {
        let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(30);

        #[cfg(target_os = "windows")]
        {
            let ps_script = format!(
                "{}\n$windows = New-Object System.Collections.ArrayList\n$callback = [Win32+EnumWindowsProc]{{param($h,$l) if([Win32]::IsWindowVisible($h)){{$sb=New-Object System.Text.StringBuilder 256;[Win32]::GetWindowText($h,$sb,256)|Out-Null;$t=$sb.ToString();if($t){{$pid=0;[Win32]::GetWindowThreadProcessId($h,[ref]$pid)|Out-Null;$p=Get-Process -Id $pid -ErrorAction SilentlyContinue;[void]$windows.Add([PSCustomObject]@{{hwnd=$h.ToInt64();title=$t;processName=$p.ProcessName;pid=$pid}})}}}};$true}}\n[Win32]::EnumWindows($callback,[IntPtr]::Zero)|Out-Null\n$windows | Select-Object -First {} | ConvertTo-Json -Compress",
                WIN32_API_DEF, limit
            );
            let output = run_ps_raw(&ps_script)?;
            let windows: serde_json::Value = serde_json::from_str(&output).unwrap_or(serde_json::json!([]));
            let count = windows.as_array().map(|a| a.len()).unwrap_or(0);
            Ok(ToolResult::ok(serde_json::json!({
                "windows": windows,
                "count": count,
                "message": format!("共 {} 个可见窗口", count)
            })))
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "仅 Windows 可用"))
        }
    }
}

/// 聚焦指定窗口
pub struct FocusWindowTool;

#[async_trait]
impl AgentTool for FocusWindowTool {
    fn name(&self) -> &'static str {
        "focus_window"
    }

    fn description(&self) -> &'static str {
        "将指定窗口切换到前台。可通过窗口标题关键词或 PID 定位窗口。用于切换到某个应用。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "窗口标题关键词（模糊匹配）"},
                "pid": {"type": "integer", "description": "进程 PID（与 title 二选一）"}
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> crate::error::AppResult<ToolResult> {
        let title = args.get("title").and_then(|v| v.as_str());
        let pid = args.get("pid").and_then(|v| v.as_i64());

        #[cfg(target_os = "windows")]
        {
            let filter = match build_window_filter(title, pid) {
                Ok(f) => f,
                Err(r) => return Ok(r),
            };

            let ps_script = format!(
                "{}\n$windows = New-Object System.Collections.ArrayList\n$callback = [Win32+EnumWindowsProc]{{param($h,$l) if([Win32]::IsWindowVisible($h)){{$sb=New-Object System.Text.StringBuilder 256;[Win32]::GetWindowText($h,$sb,256)|Out-Null;$t=$sb.ToString();if($t){{$pid2=0;[Win32]::GetWindowThreadProcessId($h,[ref]$pid2)|Out-Null;$p=Get-Process -Id $pid2 -ErrorAction SilentlyContinue;[void]$windows.Add([PSCustomObject]@{{hwnd=$h;title=$t;processName=$p.ProcessName;pid=$pid2}})}}}};$true}}\n[Win32]::EnumWindows($callback,[IntPtr]::Zero)|Out-Null\n$target = $windows | Where-Object {{ {} }} | Select-Object -First 1\nif($target){{[Win32]::ShowWindow($target.hwnd,[Win32]::SW_RESTORE)|Out-Null;[Win32]::SetForegroundWindow($target.hwnd)|Out-Null;[PSCustomObject]@{{success=$true;title=$target.title;processName=$target.processName;pid=$target.pid}}|ConvertTo-Json -Compress}}else{{[PSCustomObject]@{{success=$false;reason='未找到匹配窗口'}}|ConvertTo-Json -Compress}}",
                WIN32_API_DEF, filter
            );
            run_ps_json(&ps_script, "聚焦窗口失败")
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "仅 Windows 可用"))
        }
    }
}

/// 最小化指定窗口
pub struct MinimizeWindowTool;

#[async_trait]
impl AgentTool for MinimizeWindowTool {
    fn name(&self) -> &'static str {
        "minimize_window"
    }

    fn description(&self) -> &'static str {
        "最小化指定窗口。可通过窗口标题关键词或 PID 定位窗口。用于把某个应用最小化到任务栏。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "窗口标题关键词（模糊匹配）"},
                "pid": {"type": "integer", "description": "进程 PID（与 title 二选一）"}
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> crate::error::AppResult<ToolResult> {
        let title = args.get("title").and_then(|v| v.as_str());
        let pid = args.get("pid").and_then(|v| v.as_i64());

        #[cfg(target_os = "windows")]
        {
            let filter = match build_window_filter(title, pid) {
                Ok(f) => f,
                Err(r) => return Ok(r),
            };

            let ps_script = format!(
                "{}\n$windows = New-Object System.Collections.ArrayList\n$callback = [Win32+EnumWindowsProc]{{param($h,$l) if([Win32]::IsWindowVisible($h)){{$sb=New-Object System.Text.StringBuilder 256;[Win32]::GetWindowText($h,$sb,256)|Out-Null;$t=$sb.ToString();if($t){{$pid2=0;[Win32]::GetWindowThreadProcessId($h,[ref]$pid2)|Out-Null;$p=Get-Process -Id $pid2 -ErrorAction SilentlyContinue;[void]$windows.Add([PSCustomObject]@{{hwnd=$h;title=$t;processName=$p.ProcessName;pid=$pid2}})}}}};$true}}\n[Win32]::EnumWindows($callback,[IntPtr]::Zero)|Out-Null\n$target = $windows | Where-Object {{ {} }} | Select-Object -First 1\nif($target){{[Win32]::ShowWindow($target.hwnd,[Win32]::SW_MINIMIZE)|Out-Null;[PSCustomObject]@{{success=$true;title=$target.title;processName=$target.processName}}|ConvertTo-Json -Compress}}else{{[PSCustomObject]@{{success=$false;reason='未找到匹配窗口'}}|ConvertTo-Json -Compress}}",
                WIN32_API_DEF, filter
            );
            run_ps_json(&ps_script, "最小化窗口失败")
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "仅 Windows 可用"))
        }
    }
}

/// 关闭指定窗口（发送 WM_CLOSE，优雅关闭）
pub struct CloseWindowTool;

#[async_trait]
impl AgentTool for CloseWindowTool {
    fn name(&self) -> &'static str {
        "close_window"
    }

    fn description(&self) -> &'static str {
        "关闭指定窗口（发送 WM_CLOSE 消息，程序可提示保存）。可通过窗口标题关键词或 PID 定位。比 kill_process 更温和。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "title": {"type": "string", "description": "窗口标题关键词（模糊匹配）"},
                "pid": {"type": "integer", "description": "进程 PID（与 title 二选一）"}
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> crate::error::AppResult<ToolResult> {
        let title = args.get("title").and_then(|v| v.as_str());
        let pid = args.get("pid").and_then(|v| v.as_i64());

        #[cfg(target_os = "windows")]
        {
            let filter = match build_window_filter(title, pid) {
                Ok(f) => f,
                Err(r) => return Ok(r),
            };

            let ps_script = format!(
                "{}\n$windows = New-Object System.Collections.ArrayList\n$callback = [Win32+EnumWindowsProc]{{param($h,$l) if([Win32]::IsWindowVisible($h)){{$sb=New-Object System.Text.StringBuilder 256;[Win32]::GetWindowText($h,$sb,256)|Out-Null;$t=$sb.ToString();if($t){{$pid2=0;[Win32]::GetWindowThreadProcessId($h,[ref]$pid2)|Out-Null;$p=Get-Process -Id $pid2 -ErrorAction SilentlyContinue;[void]$windows.Add([PSCustomObject]@{{hwnd=$h;title=$t;processName=$p.ProcessName;pid=$pid2}})}}}};$true}}\n[Win32]::EnumWindows($callback,[IntPtr]::Zero)|Out-Null\n$target = $windows | Where-Object {{ {} }} | Select-Object -First 1\nif($target){{[Win32]::PostMessage($target.hwnd,[Win32]::WM_CLOSE,[IntPtr]::Zero,[IntPtr]::Zero)|Out-Null;[PSCustomObject]@{{success=$true;title=$target.title;processName=$target.processName}}|ConvertTo-Json -Compress}}else{{[PSCustomObject]@{{success=$false;reason='未找到匹配窗口'}}|ConvertTo-Json -Compress}}",
                WIN32_API_DEF, filter
            );
            run_ps_json(&ps_script, "关闭窗口失败")
        }
        #[cfg(not(target_os = "windows"))]
        {
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "仅 Windows 可用"))
        }
    }
}

// ============================================================
// 辅助函数
// ============================================================

/// 中文程序名 → 英文进程名关键词映射
/// 用于窗口匹配时，将"记事本"等中文名称映射到 notepad 等进程名
fn resolve_process_keywords(title: &str) -> Vec<String> {
    let mut keywords = vec![title.to_string()];
    let lower = title.to_lowercase();
    let mappings: &[(&str, &[&str])] = &[
        // 系统工具
        ("记事本", &["notepad"]),
        ("浏览器", &["chrome", "msedge", "firefox"]),
        ("资源管理器", &["explorer"]),
        ("文件管理器", &["explorer"]),
        ("任务管理器", &["taskmgr"]),
        ("计算器", &["calc"]),
        ("画图", &["mspaint"]),
        ("终端", &["powershell", "cmd", "WindowsTerminal", "wt"]),
        ("命令行", &["powershell", "cmd"]),
        ("设置", &["SystemSettings"]),
        ("截图", &["SnippingTool", "ScreenClippingHost"]),
        ("录音机", &["SoundRecorder"]),
        ("相机", &["WindowsCamera"]),
        ("邮件", &["HxOutlook", "OUTLOOK"]),
        ("日历", &["HxCalendar", "OUTLOOK"]),
        // 通讯社交
        ("微信", &["WeChat", "Weixin"]),
        ("qq", &["QQ"]),
        ("钉钉", &["DingTalk"]),
        ("飞书", &["Feishu", "Lark"]),
        ("腾讯会议", &["wemeetapp", "WeMeet"]),
        ("zoom", &["Zoom"]),
        ("discord", &["Discord"]),
        ("telegram", &["Telegram"]),
        ("微博", &["weibo"]),
        ("知乎", &["zhihu"]),
        ("小红书", &["xhs"]),
        ("b站", &["bilibili"]),
        ("哔哩哔哩", &["bilibili"]),
        // 开发工具
        ("vscode", &["Code"]),
        ("代码", &["Code"]),
        ("编辑器", &["Code", "notepad"]),
        ("idea", &["idea64", "IntelliJIdea"]),
        ("pycharm", &["pycharm64"]),
        ("goland", &["goland64"]),
        ("webstorm", &["webstorm64"]),
        ("clion", &["clion64"]),
        ("rustrover", &["rustrover64"]),
        ("eclipse", &["eclipse"]),
        ("git", &["git-bash", "git-gui", "GitKraken"]),
        ("docker", &["Docker Desktop", "docker"]),
        ("postman", &["Postman"]),
        ("navicat", &["navicat"]),
        ("dbeaver", &["dbeaver"]),
        ("xshell", &["Xshell"]),
        ("winscp", &["WinSCP"]),
        ("终端模拟器", &["MobaXterm", "PuTTY", "Xshell"]),
        // 设计创意
        ("photoshop", &["Photoshop"]),
        ("ps", &["Photoshop"]),
        ("illustrator", &["Illustrator"]),
        ("ai", &["Illustrator"]),
        ("premiere", &["Premiere Pro"]),
        ("pr", &["Premiere Pro"]),
        ("after effects", &["AfterFX"]),
        ("ae", &["AfterFX"]),
        ("blender", &["blender"]),
        ("figma", &["Figma"]),
        ("sketch", &["Sketch"]),
        ("cad", &["acad", "AutoCAD"]),
        ("3dmax", &["3dsmax"]),
        ("maya", &["maya"]),
        // 办公软件
        ("word", &["WINWORD"]),
        ("excel", &["EXCEL"]),
        ("powerpoint", &["POWERPNT"]),
        ("ppt", &["POWERPNT"]),
        ("outlook", &["OUTLOOK"]),
        ("onenote", &["ONENOTE"]),
        ("visio", &["VISIO"]),
        ("project", &["WINPROJ"]),
        ("wps", &["wps", "et", "wpp"]),
        ("永中", &["YozoOffice"]),
        ("pdf", &["Acrobat", "FoxitReader", "SumatraPDF"]),
        ("acrobat", &["Acrobat"]),
        // 笔记知识
        ("notion", &["Notion"]),
        ("obsidian", &["Obsidian"]),
        ("typora", &["Typora"]),
        ("xmind", &["Xmind"]),
        ("印象笔记", &["Evernote"]),
        ("有道云", &["YoudaoNote"]),
        ("为知笔记", &["Wiz"]),
        ("语雀", &["yuque"]),
        // 娱乐游戏
        ("steam", &["steam"]),
        ("epic", &["EpicGamesLauncher"]),
        ("origin", &["Origin"]),
        ("uplay", &["upc"]),
        ("战网", &["Battle.net"]),
        ("wegame", &["wegame"]),
        ("原神", &["GenshinImpact", "YuanShen"]),
        ("星穹铁道", &["StarRail"]),
        ("绝区零", &["ZenlessZoneZero"]),
        ("英雄联盟", &["LeagueClient", "League of Legends"]),
        ("网易云", &["cloudmusic"]),
        ("音乐", &["cloudmusic", "QQMusic"]),
        ("qq音乐", &["QQMusic"]),
        ("spotify", &["Spotify"]),
        ("视频", &["PotPlayer", "vlc", "mpc-hc"]),
        ("potplayer", &["PotPlayer"]),
        ("vlc", &["vlc"]),
        // 下载工具
        ("迅雷", &["Thunder"]),
        ("idm", &["IDMan"]),
        ("motrix", &["Motrix"]),
        ("百度网盘", &["baidunetdisk"]),
        ("阿里云盘", &["AliyunPan"]),
        ("夸克", &["Quark"]),
        // 远程控制
        ("todesk", &["ToDesk"]),
        ("向日葵", &["SunloginClient"]),
        ("teamviewer", &["TeamViewer"]),
        ("anydesk", &["AnyDesk"]),
        // AI 工具
        ("豆包", &["Doubao"]),
        ("chatgpt", &["ChatGPT"]),
        ("claude", &["Claude"]),
        ("cursor", &["cursor"]),
        ("windsurf", &["Windsurf"]),
        ("ollama", &["ollama"]),
        // 系统优化
        ("360", &["360tray", "360safe"]),
        ("火绒", &["HipsTray", "wsctrl"]),
        ("geek", &["geek"]),
        ("ccleaner", &["CCleaner"]),
        ("鲁大师", &["ComputerZ"]),
        ("aida64", &["aida64"]),
        ("hwinfo", &["HWiNFO64"]),
        ("任务管理器", &["taskmgr"]),
        ("资源监视器", &["resmon"]),
        ("性能监视器", &["perfmon"]),
    ];
    for (cn, en_list) in mappings {
        if lower.contains(cn) {
            for en in *en_list {
                keywords.push(en.to_string());
            }
        }
    }
    keywords
}

/// 构建窗口匹配的 PowerShell filter 表达式
/// 同时匹配窗口标题和进程名，支持中文程序名映射
fn build_window_filter(title: Option<&str>, pid: Option<i64>) -> Result<String, ToolResult> {
    if let Some(pid) = pid {
        return Ok(format!("$_.pid -eq {}", pid));
    }
    if let Some(title) = title {
        let escaped = title.replace("'", "''");
        let keywords = resolve_process_keywords(title);
        let mut conditions = vec![format!("$_.title -like '*{}*'", escaped)];
        for kw in &keywords {
            let kw_escaped = kw.replace("'", "''");
            conditions.push(format!("$_.processName -like '*{}*'", kw_escaped));
        }
        return Ok(conditions.join(" -or "));
    }
    Err(ToolResult::err("INVALID_ARGUMENT", "必须提供 title 或 pid"))
}

/// 运行 PowerShell 脚本并解析 JSON 输出
fn run_ps_json(script: &str, error_msg: &str) -> crate::error::AppResult<ToolResult> {
    let output = Command::new("powershell")
        .args(&["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| crate::error::AppError::ToolExecution(format!("{}: {}", error_msg, e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Ok(ToolResult::err("POWERSHELL_ERROR", &format!("{}: {}", error_msg, stderr)));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(ToolResult::err("EMPTY_OUTPUT", error_msg));
    }

    let data: serde_json::Value = serde_json::from_str(trimmed)
        .unwrap_or_else(|_| serde_json::json!({"raw": trimmed}));

    // 如果返回的对象有 success=false，返回错误
    if let Some(success) = data.get("success").and_then(|v| v.as_bool()) {
        if !success {
            let reason = data.get("reason").and_then(|v| v.as_str()).unwrap_or("操作失败");
            return Ok(ToolResult::err("OPERATION_FAILED", reason));
        }
    }

    Ok(ToolResult::ok(data))
}

/// 运行 PowerShell 脚本并返回原始输出
fn run_ps_raw(script: &str) -> crate::error::AppResult<String> {
    let output = Command::new("powershell")
        .args(&["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| crate::error::AppError::ToolExecution(format!("PowerShell 执行失败: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(crate::error::AppError::ToolExecution(format!("PowerShell 错误: {}", stderr)));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
