//! 桌面 AI 目标（desktop-ai 域）。
//!
//! 对齐早期原型的 desktop_ai_tools：把「本机其它 AI 工具（WorkBuddy / Cursor /
//! Codex / 本地 LLM 应用）」登记成可寻址目标，并提供统一的「提问 → 提交 →
//! 读屏收集答案」编排。底层复用 e-assisant 已有的原子能力：
//! - 聚焦窗口：PowerShell + user32 `SetForegroundWindow`
//! - 写剪贴板：`os_extra::run_powershell`（base64 传参，规避 GBK 编码）
//! - 粘贴 / 提交：`computer::input` 的 `key_down` / `key_up` / `parse_key`
//! - 读屏：`ocr::capture_screen_to_png` + `ocr::ocr_image_blocking`
//!
//! 边界：`desktop_ai_ask` 只证明「快捷键被按下」，**不证明**目标已收到或已生成
//! 答案；必须用 `desktop_ai_collect_answer` 读回可见答案才算。注册只记本地清单，
//! 不证明目标已安装 / 已登录 / 可控制。

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

const TARGETS_KEY: &str = "desktop_ai.targets";

fn db() -> AppResult<crate::db::Database> {
    let path = crate::memory::get_db_path()
        .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;
    crate::db::Database::new(path)
}

/// 内置目标（无需注册即可寻址）。
fn builtin_targets() -> Vec<Value> {
    vec![
        json!({ "id": "workbuddy", "label": "WorkBuddy", "aliases": ["workbuddy"], "openTargets": ["WorkBuddy"], "surface": "desktop_app", "source": "built_in" }),
        json!({ "id": "cursor", "label": "Cursor", "aliases": ["cursor"], "openTargets": ["Cursor", "Cursor.exe"], "surface": "desktop_app", "source": "built_in" }),
        json!({ "id": "codex", "label": "Codex", "aliases": ["codex"], "openTargets": ["Codex"], "surface": "desktop_app", "source": "built_in" }),
        json!({ "id": "windsurf", "label": "Windsurf", "aliases": ["windsurf"], "openTargets": ["Windsurf"], "surface": "desktop_app", "source": "built_in" }),
        json!({ "id": "open-webui", "label": "Open WebUI", "aliases": ["openwebui", "open webui"], "openTargets": ["open-webui", "Open WebUI"], "surface": "browser_app", "source": "built_in" }),
        json!({ "id": "ollama", "label": "Ollama", "aliases": ["ollama"], "openTargets": ["ollama"], "surface": "local_runtime", "source": "built_in" }),
    ]
}

fn read_registered() -> AppResult<Vec<Value>> {
    let db = db()?;
    let raw = db.get_setting(TARGETS_KEY)?;
    Ok(raw
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default())
}

fn write_registered(targets: &[Value]) -> AppResult<()> {
    let db = db()?;
    db.connection().execute(
        "INSERT INTO settings (key, value, value_type) VALUES (?1, ?2, 'string')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
        rusqlite::params![TARGETS_KEY, Value::Array(targets.to_vec()).to_string()],
    )?;
    Ok(())
}

fn str_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ============================================================
// 底层复用：聚焦窗口
// ============================================================

/// Win32 P/Invoke 定义（作为值插入，避免在 format! 里转义 C# 花括号）。
const DAWIN_DEF: &str = r#"Add-Type @"
using System; using System.Runtime.InteropServices; using System.Text;
public class DAWin {
  public delegate bool EnumWindowsProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc p, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
}
"@"#;

/// 聚焦一个目标：按标题子串或进程名匹配可见窗口并置前。返回 (是否聚焦, 诊断信息)。
pub(crate) fn focus_window(open_targets: &[String]) -> AppResult<(bool, String)> {
    #[cfg(not(windows))]
    {
        let _ = open_targets;
        return Ok((false, "仅 Windows 可用".to_string()));
    }
    #[cfg(windows)]
    {
        for target in open_targets {
            let t = target.trim();
            if t.is_empty() {
                continue;
            }
            // 先按标题子串，再按进程名（忽略大小写）
            if let (true, msg) = focus_by(t, true)? {
                return Ok((true, msg));
            }
            if let (true, msg) = focus_by(t, false)? {
                return Ok((true, msg));
            }
        }
        Ok((
            false,
            format!("未找到目标窗口（尝试过 {}）", open_targets.join(" / ")),
        ))
    }
}

#[cfg(windows)]
fn focus_by(needle: &str, as_title: bool) -> AppResult<(bool, String)> {
    let needle_b64 = crate::tools::executors::os_extra::literal(needle);
    let script = format!(
        r#"[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8
$needle = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{needle_b64}'))
{DAWIN_DEF}
$found = [IntPtr]::Zero
$cb = [DAWin+EnumWindowsProc]{{ param($h,$l)
  if ($found -ne [IntPtr]::Zero) {{ return $true }}
  if (-not [DAWin]::IsWindowVisible($h)) {{ return $true }}
  $sb = New-Object System.Text.StringBuilder 256
  [DAWin]::GetWindowText($h,$sb,256) | Out-Null
  $t = $sb.ToString()
  if (-not $t) {{ return $true }}
  $pid2 = 0; [DAWin]::GetWindowThreadProcessId($h,[ref]$pid2) | Out-Null
  $p = Get-Process -Id $pid2 -ErrorAction SilentlyContinue
  $pn = if ($p) {{ $p.ProcessName }} else {{ '' }}
  $hit = if ({as_title}) {{ $t -like ('*' + $needle + '*') }} else {{ $pn -ieq $needle }}
  if ($hit) {{ $found = $h }}
  return $true
}}
[DAWin]::EnumWindows($cb,[IntPtr]::Zero) | Out-Null
if ($found -ne [IntPtr]::Zero) {{
  [DAWin]::ShowWindow($found, 9) | Out-Null
  [DAWin]::SetForegroundWindow($found) | Out-Null
  [PSCustomObject]@{{focused=$true; hwnd=$found.ToInt64()}} | ConvertTo-Json -Compress
}} else {{
  [PSCustomObject]@{{focused=$false; hwnd=0}} | ConvertTo-Json -Compress
}}"#,
        needle_b64 = needle_b64,
        DAWIN_DEF = DAWIN_DEF,
        as_title = if as_title { "$true" } else { "$false" },
    );
    let (ok, stdout, stderr) = crate::tools::executors::os_extra::run_powershell(&script)?;
    if !ok {
        return Ok((false, stderr.trim().to_string()));
    }
    let v: Value = serde_json::from_str(stdout.trim()).unwrap_or(json!({ "focused": false }));
    Ok((v["focused"].as_bool().unwrap_or(false), String::new()))
}

/// 写剪贴板（复用 os_extra 的 PowerShell + base64 传参）。
fn set_clipboard(text: &str) -> AppResult<bool> {
    let script = format!(
        "$t = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}')); Set-Clipboard -Value $t; 'ok'",
        crate::tools::executors::os_extra::literal(text)
    );
    let (ok, _out, _stderr) = crate::tools::executors::os_extra::run_powershell(&script)?;
    Ok(ok)
}

/// Ctrl+V 粘贴。
#[cfg(windows)]
fn paste() -> Result<(), String> {
    crate::computer::input::win::key_down(0x11)?;
    crate::computer::input::win::key_down(0x56)?;
    crate::computer::input::win::key_up(0x56)?;
    crate::computer::input::win::key_up(0x11)?;
    Ok(())
}

#[cfg(not(windows))]
fn paste() -> Result<(), String> {
    Err("桌面控制仅在 Windows 可用".to_string())
}

/// 按下组合键（如 "enter"、"ctrl+enter"）。
#[cfg(windows)]
fn press_combo(spec: &str) -> Result<(), String> {
    let parts: Vec<&str> = spec
        .split('+')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        return Ok(());
    }
    let mut vks = Vec::with_capacity(parts.len());
    for p in &parts {
        vks.push(crate::computer::input::win::parse_key(p)?);
    }
    for &vk in &vks {
        crate::computer::input::win::key_down(vk)?;
    }
    for &vk in vks.iter().rev() {
        crate::computer::input::win::key_up(vk)?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn press_combo(_spec: &str) -> Result<(), String> {
    Err("桌面控制仅在 Windows 可用".to_string())
}

/// 解析一个目标：按 id / label / alias 匹配，返回 openTargets。
fn resolve_target(target_ref: &str, registered: &[Value], custom: &[Value]) -> Option<Vec<String>> {
    let needle = target_ref.trim().to_lowercase();
    let builtin = builtin_targets();
    for t in builtin.iter().chain(registered.iter()).chain(custom.iter()) {
        let id = t["id"].as_str().unwrap_or("").to_lowercase();
        let label = t["label"].as_str().unwrap_or("").to_lowercase();
        let aliases: Vec<String> = t["aliases"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                    .collect()
            })
            .unwrap_or_default();
        if id == needle || label == needle || aliases.iter().any(|a| a == &needle) {
            return Some(str_array(&t["openTargets"]));
        }
    }
    None
}

fn target_json(t: &Value, source: &str) -> Value {
    json!({
        "id": t["id"].as_str().unwrap_or(""),
        "label": t["label"].as_str().unwrap_or(""),
        "aliases": t["aliases"].as_array().cloned().unwrap_or_default(),
        "openTargets": t["openTargets"].as_array().cloned().unwrap_or_default(),
        "surface": t["surface"].as_str().unwrap_or("desktop_app"),
        "source": source,
    })
}

// ============================================================
// 1. desktop_ai_list_targets
// ============================================================

pub struct DesktopAiListTargetsTool;

#[async_trait]
impl AgentTool for DesktopAiListTargetsTool {
    fn name(&self) -> &'static str {
        "desktop_ai_list_targets"
    }

    fn description(&self) -> &'static str {
        "列出可寻址的本地 AI 工具目标（WorkBuddy / Cursor / Codex 等内置目标 + 用户注册目标）。要控制某个桌面 AI 工具前先查它，拿到 openTargets。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "includeStored": { "type": "boolean", "description": "是否包含用户已注册目标，默认 true" },
                "customTargets": { "type": "array", "description": "可选的一次性目标" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let include_stored = args
            .get("includeStored")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let custom = args
            .get("customTargets")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let registered = if include_stored {
            read_registered()?
        } else {
            Vec::new()
        };

        let mut targets: Vec<Value> = builtin_targets()
            .iter()
            .map(|t| target_json(t, "built_in"))
            .collect();
        for t in &registered {
            targets.push(target_json(t, "registered"));
        }
        for t in &custom {
            targets.push(target_json(t, "custom"));
        }

        Ok(ToolResult::ok(json!({
            "targets": targets,
            "discovery": "用 desktop_ai_discovery_plan 研究缺失的桌面 AI 工具，确认后用 desktop_ai_register_target 注册。",
            "boundary": "桌面目标通过可见窗口控制，答案收集需要截图/OCR 证据。",
        })))
    }
}

// ============================================================
// 2. desktop_ai_discovery_plan
// ============================================================

pub struct DesktopAiDiscoveryPlanTool;

#[async_trait]
impl AgentTool for DesktopAiDiscoveryPlanTool {
    fn name(&self) -> &'static str {
        "desktop_ai_discovery_plan"
    }

    fn description(&self) -> &'static str {
        "为新的桌面 AI / 桌面工具目标生成一份有据可查的发现计划（公开来源调研 schema + 查询建议）。只出计划，不安装、不登录、不激活。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "focus": { "type": "string", "description": "发现焦点，如桌面 AI 工具 / 编码 agent / 本地 LLM 应用" },
                "includeKnownTargets": { "type": "boolean", "description": "是否包含当前已知目标，默认 true" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let focus = args["focus"]
            .as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "桌面 AI 与桌面自动化工具".to_string());
        let include_known = args
            .get("includeKnownTargets")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let known = if include_known {
            builtin_targets()
                .iter()
                .map(|t| target_json(t, "built_in"))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "planned",
            "focus": focus,
            "knownTargets": known,
            "suggestedQueries": [
                format!("{} 官方下载", focus),
                format!("{} 桌面版 API/CLI", focus),
                format!("{} 自动化 快捷键", focus),
            ],
            "candidateSchema": {
                "id": "stable-lowercase-id",
                "label": "人类可读名称",
                "aliases": ["窗口标题别名", "品牌别名"],
                "openTargets": ["应用名 / 可执行文件 / localhost URL / 官网 URL"],
                "surface": "desktop_app | browser_app | local_runtime | developer_tool",
                "sourceUrls": ["官方产品页或文档 URL"],
                "notes": "它是什么、怎么打开、登录/API 限制、证据来源"
            },
            "evaluationChecklist": [
                "优先官方产品页、文档、GitHub 仓库、厂商下载页",
                "记录目标是桌面应用 / 浏览器应用 / 本地运行时 / 开发者工具",
                "找到稳定的打开目标：应用名 / 可执行名 / localhost URL / 官网 URL",
                "把登录、付费、验证码、账号切换、安装要求记为阻塞项，而非已完成能力",
                "用户确认后调用 desktop_ai_register_target 注册",
            ],
            "boundary": "发现只是公开来源调研，不安装软件、不登录账号、不绕过验证、不激活目标。",
        })))
    }
}

// ============================================================
// 3. desktop_ai_register_target
// ============================================================

pub struct DesktopAiRegisterTargetTool;

#[async_trait]
impl AgentTool for DesktopAiRegisterTargetTool {
    fn name(&self) -> &'static str {
        "desktop_ai_register_target"
    }

    fn description(&self) -> &'static str {
        "注册 / 更新一个经用户确认的桌面 AI 目标，供后续 desktop_ai_ask / collect_answer 按 id、label、别名寻址。只记本地清单，不证明目标已安装或可控制。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "稳定小写 id，如 raycast-ai" },
                "label": { "type": "string" },
                "aliases": { "type": "array", "items": { "type": "string" } },
                "openTargets": { "type": "array", "items": { "type": "string" }, "description": "应用名 / 可执行名 / URL，按顺序尝试" },
                "surface": { "type": "string", "enum": ["desktop_app", "browser_app", "local_runtime", "developer_tool"] },
                "sourceUrls": { "type": "array", "items": { "type": "string" } },
                "notes": { "type": "string" },
                "enabled": { "type": "boolean" }
            },
            "required": ["label", "openTargets"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let label = args["label"].as_str().unwrap_or("").trim().to_string();
        let open_targets = str_array(&args["openTargets"]);
        if label.is_empty() || open_targets.is_empty() {
            return Ok(ToolResult::err(
                "INVALID_TARGET",
                "label 与 openTargets 不能为空",
            ));
        }
        let id = args["id"]
            .as_str()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| label.to_lowercase().replace(' ', "-"));

        let target = json!({
            "id": id,
            "label": label,
            "aliases": args["aliases"].as_array().cloned().unwrap_or_default(),
            "openTargets": open_targets,
            "surface": args["surface"].as_str().unwrap_or("desktop_app"),
            "sourceUrls": args["sourceUrls"].as_array().cloned().unwrap_or_default(),
            "notes": args["notes"].as_str().unwrap_or(""),
            "enabled": args["enabled"].as_bool().unwrap_or(true),
            "updatedAt": chrono::Local::now().to_rfc3339(),
        });

        let mut registered = read_registered()?;
        let idx = registered
            .iter()
            .position(|t| t["id"].as_str() == Some(target["id"].as_str().unwrap_or("")));
        match idx {
            Some(i) => registered[i] = target.clone(),
            None => registered.push(target.clone()),
        }
        write_registered(&registered)?;

        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "registered",
            "persisted": true,
            "registered": true,
            "target": target,
            "note": "已注册到本地清单。后续 desktop_ai_list_targets / ask / collect_answer 可按 id、label、别名寻址。",
        })))
    }
}

// ============================================================
// 4. desktop_ai_ask
// ============================================================

pub struct DesktopAiAskTool;

#[async_trait]
impl AgentTool for DesktopAiAskTool {
    fn name(&self) -> &'static str {
        "desktop_ai_ask"
    }

    fn description(&self) -> &'static str {
        "向一个本地 AI 工具目标提交一个问题：聚焦其窗口 → 写剪贴板 → 粘贴 → 可选按提交快捷键。只证明「快捷键被按下」，不证明目标已收到；答案需用 desktop_ai_collect_answer 读回。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "question": { "type": "string" },
                "target": { "type": "string", "description": "目标 id 或名称，省略时取第一个可聚焦目标" },
                "customTargets": { "type": "array" },
                "send": { "type": "boolean", "description": "粘贴后是否按提交快捷键，默认 true" },
                "submitShortcut": { "type": "string", "description": "提交快捷键，默认 enter，可用 ctrl+enter" },
                "openIfNeeded": { "type": "boolean", "description": "目标不在前台时是否先聚焦，默认 true" }
            },
            "required": ["question"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let question = args["question"].as_str().unwrap_or("").trim().to_string();
        if question.is_empty() {
            return Ok(ToolResult::err("QUESTION_REQUIRED", "question 不能为空"));
        }
        let send = args.get("send").and_then(|v| v.as_bool()).unwrap_or(true);
        let submit_shortcut = args["submitShortcut"]
            .as_str()
            .unwrap_or("enter")
            .to_string();
        let open_if_needed = args
            .get("openIfNeeded")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let custom = args
            .get("customTargets")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let registered = read_registered()?;

        // 定位目标
        let target_ref = args["target"].as_str().unwrap_or("").trim().to_string();
        let open_targets = if !target_ref.is_empty() {
            match resolve_target(&target_ref, &registered, &custom) {
                Some(t) => t,
                None => {
                    return Ok(ToolResult::err(
                        "TARGET_NOT_FOUND",
                        &format!(
                            "未找到目标「{}」，用 desktop_ai_list_targets 查看可用目标",
                            target_ref
                        ),
                    ))
                }
            }
        } else {
            Vec::new()
        };

        // 聚焦
        let mut focused = false;
        if open_if_needed && !open_targets.is_empty() {
            let (ok, _msg) = crate::tools::executors::desktop_ai::focus_window(&open_targets)?;
            focused = ok;
        }

        // 写剪贴板 + 粘贴
        let mut prepared = false;
        if set_clipboard(&question)? {
            paste().map_err(|e| AppError::ToolExecution(format!("粘贴失败: {}", e)))?;
            prepared = true;
        }

        // 提交
        let mut submitted = false;
        if prepared && send {
            press_combo(&submit_shortcut)
                .map_err(|e| AppError::ToolExecution(format!("提交失败: {}", e)))?;
            submitted = true;
        }

        let status = if submitted {
            "submitted_unverified"
        } else if prepared {
            "prepared"
        } else {
            "blocked"
        };

        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": status,
            "question": question,
            "send": send,
            "preparedCount": if prepared { 1 } else { 0 },
            "submittedCount": if submitted { 1 } else { 0 },
            "sentCount": 0,
            "verifiedSentCount": 0,
            "focused": focused,
            "results": [],
            "note": "提交未被验证——仅证明快捷键被按下，不证明目标已收到或已生成答案。",
        })))
    }
}

// ============================================================
// 5. desktop_ai_collect_answer
// ============================================================

pub struct DesktopAiCollectAnswerTool;

#[async_trait]
impl AgentTool for DesktopAiCollectAnswerTool {
    fn name(&self) -> &'static str {
        "desktop_ai_collect_answer"
    }

    fn description(&self) -> &'static str {
        "读回目标窗口的可见答案：聚焦目标 → 截图 → OCR。只读可见区域，内容在屏幕外时可能不完整；拿不到时返回 pending/blocked 而非编造答案。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target": { "type": "string", "description": "目标 id 或名称" },
                "customTargets": { "type": "array" },
                "question": { "type": "string", "description": "原始问题，用于区分答案与提示语" },
                "openIfNeeded": { "type": "boolean", "description": "目标不在前台时是否先聚焦，默认 true" },
                "waitMs": { "type": "number", "description": "截图前等待毫秒，最大 60000" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let custom = args
            .get("customTargets")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let registered = read_registered()?;
        let wait_ms = args["waitMs"].as_u64().unwrap_or(0).min(60_000);

        // 定位并聚焦
        let target_ref = args["target"].as_str().unwrap_or("").trim().to_string();
        let mut focused = false;
        if !target_ref.is_empty() {
            if let Some(open_targets) = resolve_target(&target_ref, &registered, &custom) {
                let open_if_needed = args
                    .get("openIfNeeded")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if open_if_needed {
                    let (ok, _msg) =
                        crate::tools::executors::desktop_ai::focus_window(&open_targets)?;
                    focused = ok;
                }
            }
        }

        if wait_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
        }

        // 截图 + OCR
        let screenshot = crate::tools::executors::ocr::capture_screen_to_png()?;
        let shot_for_task = screenshot.clone();
        let result = tokio::task::spawn_blocking(move || {
            crate::tools::executors::ocr::ocr_image_blocking(&shot_for_task, None)
        })
        .await
        .map_err(|e| AppError::ToolExecution(format!("OCR 任务异常: {}", e)))?;

        let output = match result {
            Ok(o) => o,
            Err(message) => {
                return Ok(ToolResult::ok(json!({
                    "status": "blocked",
                    "target": target_ref,
                    "blocker": message,
                    "answerText": "",
                })));
            }
        };

        let answer_text = output.text.trim().to_string();
        let status = if answer_text.is_empty() {
            "pending"
        } else {
            "collected"
        };

        Ok(ToolResult::ok(json!({
            "status": status,
            "target": target_ref,
            "focused": focused,
            "answerText": answer_text,
            "language": output.language,
            "screenshot": screenshot,
            "note": if status == "pending" {
                "尚未读到可见答案，等待后再收集，勿编造答案。"
            } else {
                "可见答案已读回；若答案延伸到屏幕外，可能不完整。"
            },
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_targets_are_non_empty_and_unique() {
        let all = builtin_targets();
        assert!(all.len() >= 4);
        let mut ids: Vec<&str> = all.iter().filter_map(|t| t["id"].as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "内置目标 id 不应重复");
    }

    #[test]
    fn resolve_target_matches_by_id_label_alias() {
        let registered: Vec<Value> = vec![json!({
            "id": "foo", "label": "Foo AI", "aliases": ["foobar"], "openTargets": ["Foo.exe"]
        })];
        let custom: Vec<Value> = vec![];
        assert_eq!(
            resolve_target("foo", &registered, &custom),
            Some(vec!["Foo.exe".to_string()])
        );
        assert_eq!(
            resolve_target("Foo AI", &registered, &custom),
            Some(vec!["Foo.exe".to_string()])
        );
        assert_eq!(
            resolve_target("foobar", &registered, &custom),
            Some(vec!["Foo.exe".to_string()])
        );
        assert!(resolve_target("nope", &registered, &custom).is_none());
    }

    #[test]
    fn resolve_builtin_by_label() {
        let registered: Vec<Value> = vec![];
        let custom: Vec<Value> = vec![];
        let got = resolve_target("cursor", &registered, &custom);
        assert!(got.is_some());
        assert!(got
            .unwrap()
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Cursor")));
    }
}
