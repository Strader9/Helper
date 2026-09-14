//! 程序操作工具
//!
//! 提供程序启动和搜索能力。

use std::fs;
use std::path::Path;
use std::process::Command;

use async_trait::async_trait;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

// ============================================================
// 启动程序工具
// ============================================================

/// 启动指定的程序
pub struct LaunchProgramTool;

#[async_trait]
impl AgentTool for LaunchProgramTool {
    fn name(&self) -> &'static str {
        "launch_program"
    }

    fn description(&self) -> &'static str {
        "启动指定的程序。当用户说'打开QQ'、'启动记事本'、'打开微信'时使用此工具。支持程序名或完整路径。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name_or_path": {
                    "type": "string",
                    "description": "程序名称（如'notepad'、'qq'）或完整路径"
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "传递给程序的参数（可选）"
                },
                "wait_for_start": {
                    "type": "boolean",
                    "description": "是否等待应用启动完成（等待窗口出现），默认 false。V16 新增。"
                },
                "wait_timeout": {
                    "type": "integer",
                    "description": "等待启动超时秒数，默认 10。仅 wait_for_start=true 时生效。"
                }
            },
            "required": ["name_or_path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let name_or_path = args["name_or_path"].as_str()
            .ok_or_else(|| AppError::InvalidArgument("name_or_path is required".to_string()))?;
        let program_args: Vec<String> = args["args"].as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();

        // 安全校验：拒绝 shell 元字符，防止命令注入
        if contains_shell_metachars(name_or_path) {
            return Ok(ToolResult::err("INVALID_INPUT", "程序名或路径包含非法字符"));
        }
        for arg in &program_args {
            if contains_shell_metachars(arg) {
                return Ok(ToolResult::err("INVALID_INPUT", "程序参数包含非法字符"));
            }
        }

        // 解析程序路径
        let program_path = if Path::new(name_or_path).exists() {
            name_or_path.to_string()
        } else {
            match find_program_path(name_or_path) {
                Ok(Some(path)) => path,
                Ok(None) => name_or_path.to_string(),
                Err(e) => return Ok(ToolResult::err("SEARCH_FAILED", &e.to_string())),
            }
        };

        #[cfg(target_os = "windows")]
        {
            let path_lower = program_path.to_lowercase();
            let is_lnk = path_lower.ends_with(".lnk") || path_lower.ends_with(".url");
            let path_exists = Path::new(&program_path).exists();

            let child = if is_lnk && path_exists {
                // .lnk/.url 快捷方式必须通过 shell 启动（CreateProcess 无法解析）
                let mut cmd = Command::new("cmd");
                cmd.arg("/c").arg("start").arg("").arg(&program_path);
                if !program_args.is_empty() {
                    cmd.args(&program_args);
                }
                cmd.spawn()
            } else if path_exists {
                // 已验证存在的可执行文件：直接 CreateProcess 启动
                let mut cmd = Command::new(&program_path);
                if !program_args.is_empty() {
                    cmd.args(&program_args);
                }
                cmd.spawn()
            } else {
                // 路径不存在且 find_program 未找到 → 直接返回失败
                // 不再回退 cmd start（会返回 cmd 进程的虚假 PID）
                return Ok(ToolResult::err(
                    "PROGRAM_NOT_FOUND",
                    &format!("未找到程序: {}（请检查程序名或路径是否正确）", name_or_path)
                ));
            };

            let child = child.map_err(|e| AppError::ToolExecution(format!("无法启动程序: {}", e)))?;
            let pid = child.id();

            // V16: 等待应用启动完成（等待窗口出现）
            let wait_for_start = args["wait_for_start"].as_bool().unwrap_or(false);
            let wait_timeout = args["wait_timeout"].as_i64().unwrap_or(10) as u64;
            let mut started = true;
            let mut window_title: Option<String> = None;

            if wait_for_start {
                let start_time = std::time::Instant::now();
                while start_time.elapsed().as_secs() < wait_timeout {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    // 检查进程是否有窗口
                    if let Ok(output) = std::process::Command::new("powershell")
                        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &format!(
                            "Get-Process -Id {} -ErrorAction SilentlyContinue | Where-Object {{ $_.MainWindowTitle -ne '' }} | Select-Object -First 1 -ExpandProperty MainWindowTitle",
                            pid
                        )])
                        .creation_flags(CREATE_NO_WINDOW)
                        .output()
                    {
                        let title = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if !title.is_empty() {
                            window_title = Some(title);
                            break;
                        }
                    }
                    // 检查进程是否还在运行
                    if let Ok(output) = std::process::Command::new("powershell")
                        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &format!("Get-Process -Id {} -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Id", pid)])
                        .creation_flags(CREATE_NO_WINDOW)
                        .output()
                    {
                        let check = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if check.is_empty() {
                            started = false;
                            break;
                        }
                    }
                }
                if window_title.is_none() && started {
                    // 超时但进程还在，视为启动中
                    started = true;
                }
            }

            let mut result_data = serde_json::json!({
                "program": name_or_path,
                "path": program_path,
                "pid": pid,
                "message": format!("已启动: {}", name_or_path)
            });
            if wait_for_start {
                result_data["started"] = serde_json::json!(started);
                if let Some(ref title) = window_title {
                    result_data["window_title"] = serde_json::json!(title);
                }
                result_data["message"] = serde_json::json!(if started {
                    if let Some(ref t) = window_title {
                        format!("已启动: {}（窗口: {}）", name_or_path, t)
                    } else {
                        format!("已启动: {}（进程运行中，窗口未出现）", name_or_path)
                    }
                } else {
                    format!("启动失败: {}（进程已退出）", name_or_path)
                });
            }

            if wait_for_start && !started {
                return Ok(ToolResult::err("LAUNCH_FAILED", &format!("{} 启动失败（进程已退出）", name_or_path)));
            }
            return Ok(ToolResult::ok(result_data));
        }

        #[cfg(not(target_os = "windows"))]
        {
            let mut cmd = Command::new(&program_path);
            if !program_args.is_empty() {
                cmd.args(&program_args);
            }
            let child = cmd.spawn()
                .map_err(|e| AppError::ToolExecution(format!("无法启动程序: {}", e)))?;
            Ok(ToolResult::ok(serde_json::json!({
                "program": name_or_path,
                "path": program_path,
                "pid": child.id(),
                "message": format!("已启动: {}", name_or_path)
            })))
        }
    }
}

// ============================================================
// 查找程序工具
// ============================================================

/// 搜索程序安装位置
pub struct FindProgramTool;

#[async_trait]
impl AgentTool for FindProgramTool {
    fn name(&self) -> &'static str {
        "find_program"
    }

    fn description(&self) -> &'static str {
        "搜索程序在系统中的安装位置。返回找到的可执行文件路径列表。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "要搜索的程序名称（如'qq'、'wechat'、'notepad'）"
                }
            },
            "required": ["name"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let name = args["name"].as_str()
            .ok_or_else(|| AppError::InvalidArgument("name is required".to_string()))?;

        let paths = find_all_program_paths(name)?;

        Ok(ToolResult::ok(serde_json::json!({
            "name": name,
            "paths": paths,
            "found": !paths.is_empty(),
            "message": if paths.is_empty() {
                format!("未找到程序: {}", name)
            } else {
                format!("找到 {} 个结果", paths.len())
            }
        })))
    }
}

// ============================================================
// 关闭程序工具
// ============================================================

/// 关闭指定的程序（通过进程名）
pub struct CloseProgramTool;

#[async_trait]
impl AgentTool for CloseProgramTool {
    fn name(&self) -> &'static str {
        "close_program"
    }

    fn description(&self) -> &'static str {
        "关闭指定的程序。当用户说'关闭QQ'、'关掉Chrome'、'退出微信'时使用此工具。通过进程名匹配并强制结束进程。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "程序名称（如'QQ'、'chrome'、'WeChat'），会自动匹配对应进程名"
                }
            },
            "required": ["name"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let name = args["name"].as_str()
            .ok_or_else(|| AppError::InvalidArgument("name is required".to_string()))?;

        // 安全校验
        if contains_shell_metachars(name) {
            return Ok(ToolResult::err("INVALID_INPUT", "程序名包含非法字符"));
        }

        // 将用户输入的程序名映射为实际进程名
        let process_name = resolve_process_name(name);

        #[cfg(target_os = "windows")]
        {
            // 使用 taskkill 强制结束进程
            let output = Command::new("taskkill")
                .args(&["/IM", &process_name, "/F"])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map_err(|e| AppError::ToolExecution(format!("执行 taskkill 失败: {}", e)))?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            if output.status.success() {
                Ok(ToolResult::ok(serde_json::json!({
                    "program": name,
                    "process": process_name,
                    "message": format!("已关闭: {}", name),
                    "output": stdout.trim()
                })))
            } else {
                // 进程可能不存在，返回明确错误
                let error_msg = if stderr.contains("没有找到") || stderr.contains("not found") {
                    format!("未找到运行中的程序: {}", name)
                } else {
                    format!("关闭失败: {}", stderr.trim())
                };
                Ok(ToolResult::err("PROCESS_NOT_FOUND", &error_msg))
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "close_program 仅在 Windows 上可用"))
        }
    }
}

/// 将用户输入的程序名映射为实际进程名（.exe）
fn resolve_process_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let mapped = match lower.as_str() {
        "qq" => "QQ.exe",
        "wechat" | "微信" => "WeChat.exe",
        "chrome" | "google chrome" => "chrome.exe",
        "edge" | "microsoft edge" => "msedge.exe",
        "firefox" => "firefox.exe",
        "vscode" | "vs code" | "visual studio code" => "Code.exe",
        "notepad" => "notepad.exe",
        "calc" | "计算器" => "calc.exe",
        "spotify" => "Spotify.exe",
        "discord" => "Discord.exe",
        "steam" => "steam.exe",
        "obs" => "obs64.exe",
        _ => "",
    };

    if !mapped.is_empty() {
        return mapped.to_string();
    }

    // 如果用户已经输入了 .exe 后缀，直接使用
    if lower.ends_with(".exe") {
        return name.to_string();
    }

    // 否则自动添加 .exe
    format!("{}.exe", name)
}

// ============================================================
// 内部辅助函数
// ============================================================

/// 查找单个程序路径（从所有结果中选择最佳匹配）
fn find_program_path(name: &str) -> AppResult<Option<String>> {
    let mut paths = find_all_program_paths(name)?;
    if paths.is_empty() {
        return Ok(None);
    }

    // 按优先级排序：分数高的排前面
    let name_lower = name.to_lowercase();
    paths.sort_by(|a, b| {
        let score_a = score_program_path(a, &name_lower);
        let score_b = score_program_path(b, &name_lower);
        score_b.cmp(&score_a)
    });

    Ok(paths.into_iter().next())
}

/// 给程序路径打分，分数越高越可能是用户想要的主程序
fn score_program_path(path: &str, query: &str) -> i32 {
    let file_name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let mut score = 0;

    // === 排除项（大幅扣分）===
    let exclude_patterns = [
        "ext.", "extension.", "update.", "uninstall.", "helper.", "crash.",
        "report.", "setup.", "installer.", "patch.", "launcher.", "daemon.",
        "service.", "agent.", "background.", "renderer.", "webview.",
    ];
    for pat in &exclude_patterns {
        if file_name.contains(pat) {
            score -= 200;
        }
    }

    // === 加分项 ===

    // .lnk 快捷方式通常指向正确的主程序，且不会被193错误困扰（因为启动时已用 start 命令）
    if file_name.ends_with(".lnk") {
        score += 80;
    }

    // 完全匹配查询名称（如 wechat.exe 匹配 "wechat"）
    let base_name = file_name.replace(".exe", "").replace(".lnk", "");
    if base_name == query {
        score += 100;
    }

    // 特定程序的精确匹配规则
    match query {
        // 微信：WeChat.exe 是主程序，WeixinExt.exe 是扩展
        "微信" | "wechat" => {
            if file_name.contains("wechat") && !file_name.contains("ext") && !file_name.contains("xin") {
                score += 150; // WeChat.exe 最优先
            }
            if file_name.contains("weixin") && file_name.contains("ext") {
                score -= 100; // WeixinExt.exe 是扩展程序
            }
        }
        // QQ：QQ.exe / QQNT.exe 优先
        "qq" => {
            if file_name == "qq.exe" || file_name == "qqnt.exe" {
                score += 100;
            }
        }
        // Docker Desktop：优先包含 Desktop 的
        "docker desktop" | "docker" => {
            if file_name.contains("desktop") {
                score += 150; // Docker Desktop.exe
            }
            if file_name == "docker.exe" && path.contains("resources\\bin") {
                score -= 100; // resources\bin\docker.exe 是 CLI 工具
            }
            if file_name == "dockercli.exe" {
                score += 50;
            }
        }
        // Watt Toolkit / Steam++
        "watt toolkit" | "watt" | "steam++" => {
            if file_name.contains("watt") || file_name.contains("steam++") {
                score += 100;
            }
        }
        _ => {}
    }

    // 路径越深（版本号目录中）越不优先
    let depth = path.matches("\\").count();
    if depth >= 5 {
        score -= 20;
    }

    score
}

/// 常见程序别名映射：用户可能说的名称 -> 搜索时额外匹配的名称
fn get_program_aliases(name: &str) -> Vec<String> {
    let name_lower = name.to_lowercase();
    let mut aliases = vec![name_lower.clone()];

    // 常见程序别名
    match name_lower.as_str() {
        "qq" => { aliases.push("qq".to_string()); aliases.push("tencent".to_string()); }
        "wechat" | "微信" => { aliases.push("wechat".to_string()); aliases.push("weixin".to_string()); }
        "docker" | "docker desktop" => { aliases.push("docker".to_string()); aliases.push("docker desktop".to_string()); }
        "vscode" | "vs code" | "visual studio code" => { aliases.push("code".to_string()); aliases.push("vscode".to_string()); aliases.push("visual studio code".to_string()); }
        "chrome" | "google chrome" => { aliases.push("chrome".to_string()); aliases.push("google chrome".to_string()); }
        "edge" | "microsoft edge" => { aliases.push("msedge".to_string()); aliases.push("microsoft edge".to_string()); }
        "firefox" => { aliases.push("firefox".to_string()); }
        "notepad++" => { aliases.push("notepad++".to_string()); }
        "idea" | "intellij" => { aliases.push("idea".to_string()); aliases.push("intellij".to_string()); }
        "steam" => { aliases.push("steam".to_string()); }
        "spotify" => { aliases.push("spotify".to_string()); }
        "obs" => { aliases.push("obs".to_string()); aliases.push("obs studio".to_string()); }
        "watt toolkit" | "watt" | "steam++" => {
            aliases.push("watt toolkit".to_string());
            aliases.push("watt".to_string());
            aliases.push("steam++".to_string());
        }
        _ => {}
    }

    aliases.sort();
    aliases.dedup();
    aliases
}

/// 查找所有匹配的程序路径
fn find_all_program_paths(name: &str) -> AppResult<Vec<String>> {
    let mut results = Vec::new();
    let aliases = get_program_aliases(name);

    // 1. 使用 where 命令搜索 PATH
    #[cfg(target_os = "windows")]
    {
        for alias in &aliases {
            let search_names = [
                format!("{}.exe", alias),
                format!("{}.lnk", alias),
                alias.clone(),
            ];

            for search_name in &search_names {
                if let Ok(output) = Command::new("cmd").args(&["/c", "where", search_name]).creation_flags(CREATE_NO_WINDOW).output() {
                    if output.status.success() {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        for line in stdout.lines() {
                            let path = line.trim();
                            if !path.is_empty() && !results.contains(&path.to_string()) {
                                results.push(path.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // 2. 在常见安装目录中搜索（递归深度3）
    let search_dirs = get_common_program_dirs();
    for dir in search_dirs {
        search_directory_recursive(&dir, &aliases, &mut results, 0, 3);
    }

    Ok(results)
}

/// 获取常见程序安装目录
fn get_common_program_dirs() -> Vec<String> {
    let mut dirs = Vec::new();

    #[cfg(target_os = "windows")]
    {
        if let Ok(pf) = std::env::var("ProgramFiles") {
            dirs.push(pf);
        }
        if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
            dirs.push(pf86);
        }
        if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
            dirs.push(local_appdata.clone());
            // 增加 Start Menu 和 Programs 目录
            dirs.push(format!("{}\\Microsoft\\Windows\\Start Menu\\Programs", local_appdata));
        }
        if let Ok(appdata) = std::env::var("APPDATA") {
            dirs.push(format!("{}\\Microsoft\\Windows\\Start Menu\\Programs", appdata));
        }
        if let Ok(user_profile) = std::env::var("USERPROFILE") {
            dirs.push(user_profile.clone());
            dirs.push(format!("{}\\Desktop", user_profile));
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        dirs.push("/usr/bin".to_string());
        dirs.push("/usr/local/bin".to_string());
        dirs.push("/opt".to_string());
        dirs.push("/Applications".to_string());
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(format!("{}/.local/bin", home));
            dirs.push(format!("{}/Applications", home));
        }
    }

    dirs
}

/// 在目录中递归搜索程序（支持深度限制）
fn search_directory_recursive(
    dir: &str,
    aliases: &[String],
    results: &mut Vec<String>,
    current_depth: usize,
    max_depth: usize,
) {
    if current_depth > max_depth {
        return;
    }

    let path = Path::new(dir);
    if !path.exists() || !path.is_dir() {
        return;
    }

    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let entry_path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_lowercase();

        // 文件匹配
        if entry_path.is_file() {
            for alias in aliases {
                let alias_lower = alias.to_lowercase();
                let exact_names = [
                    format!("{}.exe", alias_lower),
                    format!("{}.lnk", alias_lower),
                    alias_lower.clone(),
                ];

                // 直接匹配
                for ename in &exact_names {
                    if &file_name == ename {
                        let p = entry_path.to_string_lossy().to_string();
                        if !results.contains(&p) {
                            results.push(p);
                        }
                        break;
                    }
                }

                // 包含匹配（如 QQ.exe 匹配别名 qq）
                if file_name.contains(&alias_lower)
                    && (file_name.ends_with(".exe") || file_name.ends_with(".lnk"))
                {
                    let p = entry_path.to_string_lossy().to_string();
                    if !results.contains(&p) {
                        results.push(p);
                    }
                }
            }
        }

        // 递归进入子目录
        if entry_path.is_dir() {
            search_directory_recursive(
                &entry_path.to_string_lossy(),
                aliases,
                results,
                current_depth + 1,
                max_depth,
            );
        }
    }
}

/// 检查字符串是否包含 shell 元字符（防止命令注入）
fn contains_shell_metachars(s: &str) -> bool {
    s.contains('&') || s.contains('|') || s.contains('>') ||
    s.contains('<') || s.contains('^') || s.contains(';') ||
    s.contains('"') || s.contains('`')
}
