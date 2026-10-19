//! 系统设置与系统信息工具
//!
//! 补充 `system.rs`：那边是"跑命令、截屏"，这边是剪贴板、通知、
//! 网络与磁盘现状、环境变量、锁屏。
//!
//! 实现上统一走 PowerShell，但**参数一律经 base64 传递**——把用户文本
//! 直接拼进脚本字符串等于给自己开一个命令注入口，而剪贴板内容、通知
//! 正文恰好都是用户可控的。

use std::process::Command;

use async_trait::async_trait;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

use base64::{engine::general_purpose::STANDARD as B64, Engine};

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

/// 环境变量里这些关键字出现时，值不回传。
///
/// 助手读环境变量是为了回答"我的 PATH 里有什么""JAVA_HOME 指向哪"，
/// 不是为了把用户的云密钥打印到聊天记录里。
const SECRET_HINTS: &[&str] = &[
    "KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "AUTH", "SESSION", "COOKIE",
    "PRIVATE",
];

fn require_str<'a>(args: &'a serde_json::Value, key: &str) -> AppResult<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::InvalidArgument(format!("{} is required", key)))
}

/// 执行一段 PowerShell，返回 (是否成功, stdout, stderr)。
pub(crate) fn run_powershell(script: &str) -> AppResult<(bool, String, String)> {
    #[cfg(windows)]
    {
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                script,
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| AppError::ToolExecution(format!("无法调用 PowerShell: {}", e)))?;
        Ok((
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }

    #[cfg(not(windows))]
    {
        let _ = script;
        Err(AppError::ToolExecution(
            "该能力目前只在 Windows 上实现".to_string(),
        ))
    }
}

/// 执行 PowerShell 并把 stdout 当作 base64 解回来。
///
/// 不能直接用 `run_powershell` 的 stdout：Windows PowerShell 的输出编码跟着
/// 控制台代码页走（中文机器是 GBK），按 UTF-8 解码会把中文变成替换字符——
/// 不仅内容错，**字符数也会跟着变**，于是剪贴板回读比对会误报"没写成"。
/// 让脚本自己把结果 base64 化，是唯一不依赖代码页的做法。
pub(crate) fn run_powershell_b64(body: &str) -> AppResult<(bool, String, String)> {
    let script = format!(
        "$out = @(& {{ {} }}) -join \"`n\"\n[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($out))",
        body
    );
    let (ok, stdout, stderr) = run_powershell(&script)?;
    if !ok {
        return Ok((false, String::new(), stderr));
    }
    let decoded = B64
        .decode(stdout.trim())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or(stdout);
    Ok((true, decoded, String::new()))
}

/// 把文本编码成 base64，供脚本内解码。脚本里只出现 `[A-Za-z0-9+/=]`。
pub(crate) fn literal(text: &str) -> String {
    B64.encode(text.as_bytes())
}

/// 统一换行符：剪贴板往返会把 `\n` 变成 `\r\n`，
/// 直接逐字节比对会把"写对了"误判成"没写成"。
fn normalize_newline(text: &str) -> String {
    text.replace("\r\n", "\n")
}

// ============================================================
// 剪贴板
// ============================================================

/// 读取剪贴板文本
pub struct ReadClipboardTool;

#[async_trait]
impl AgentTool for ReadClipboardTool {
    fn name(&self) -> &'static str {
        "read_clipboard"
    }

    fn description(&self) -> &'static str {
        "读取系统剪贴板里的文本。仅在用户明确要求查看剪贴板内容时使用，不要用它猜测用户意图。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "max_chars": { "type": "integer", "description": "最多返回多少字符，默认 4000" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let max_chars = args["max_chars"].as_u64().unwrap_or(4000).min(50000) as usize;
        let (ok, stdout, stderr) = run_powershell_b64(
            "$t = Get-Clipboard -Raw; if ($null -eq $t) { '' } else { $t }",
        )?;
        if !ok {
            return Ok(ToolResult::err("CLIPBOARD_READ_FAILED", stderr.trim()));
        }
        let text = stdout.trim_end_matches(['\r', '\n']).to_string();
        let total = text.chars().count();
        let clipped: String = text.chars().take(max_chars).collect();
        Ok(ToolResult::ok(serde_json::json!({
            "text": clipped,
            "chars": total,
            "truncated": total > max_chars,
            "empty": total == 0,
        })))
    }
}

/// 写入剪贴板文本
pub struct WriteClipboardTool;

#[async_trait]
impl AgentTool for WriteClipboardTool {
    fn name(&self) -> &'static str {
        "write_clipboard"
    }

    fn description(&self) -> &'static str {
        "把文本写入系统剪贴板，方便用户直接粘贴。适合交付代码片段、链接、整理好的文本。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "要写入的文本" }
            },
            "required": ["text"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let text = require_str(&args, "text")?;
        let script = format!(
            "$t = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}')); Set-Clipboard -Value $t; 'ok'",
            literal(text)
        );
        let (ok, _stdout, stderr) = run_powershell(&script)?;
        if !ok {
            return Ok(ToolResult::err("CLIPBOARD_WRITE_FAILED", stderr.trim()));
        }

        // 写完立刻回读比对。`Set-Clipboard` 不返回是否生效，
        // 而剪贴板会同时被别的程序改写——只有读回来一致才算写成了。
        let (read_ok, echo, _) = run_powershell_b64(
            "$t = Get-Clipboard -Raw; if ($null -eq $t) { '' } else { $t }",
        )?;
        let expected = normalize_newline(text);
        let actual = normalize_newline(echo.trim_end_matches(['\r', '\n']));
        let matched = read_ok && actual == expected;

        // 回读对不上就是没写成——不管 `Set-Clipboard` 有没有报错。
        // 超长文本会被剪贴板本身截断，那种情况不下"失败"的结论。
        if !matched && text.chars().count() <= 100_000 {
            return Ok(ToolResult::err(
                "CLIPBOARD_READBACK_MISMATCH",
                &format!(
                    "写入后回读不一致（期望 {} 字符，实际 {} 字符）",
                    expected.chars().count(),
                    actual.chars().count()
                ),
            ));
        }

        Ok(ToolResult::ok(serde_json::json!({
            "chars": text.chars().count(),
            "written": true,
            "readback_matched": matched,
            "readback_chars": actual.chars().count(),
        })))
    }
}

// ============================================================
// 系统通知
// ============================================================

/// 弹一条系统通知
pub struct SendNotificationTool;

#[async_trait]
impl AgentTool for SendNotificationTool {
    fn name(&self) -> &'static str {
        "send_notification"
    }

    fn description(&self) -> &'static str {
        "弹出一条 Windows 系统通知（标题 + 正文）。适合任务跑完提醒用户，或把长结果摘要推到眼前。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "通知标题" },
                "body": { "type": "string", "description": "通知正文" }
            },
            "required": ["title", "body"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let title = require_str(&args, "title")?;
        let body = require_str(&args, "body")?;

        let script = format!(
            r#"[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
$title = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{title}'))
$body  = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{body}'))
$xml = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02)
$nodes = $xml.GetElementsByTagName('text')
$nodes.Item(0).AppendChild($xml.CreateTextNode($title)) | Out-Null
$nodes.Item(1).AppendChild($xml.CreateTextNode($body)) | Out-Null
$toast = [Windows.UI.Notifications.ToastNotification]::new($xml)
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('PC Guardian').Show($toast)
'ok'"#,
            title = literal(title),
            body = literal(body)
        );

        let (ok, _stdout, stderr) = run_powershell(&script)?;
        if !ok {
            return Ok(ToolResult::err("NOTIFICATION_FAILED", stderr.trim()));
        }
        Ok(ToolResult::ok(serde_json::json!({
            "title": title,
            "shown": true,
        })))
    }
}

// ============================================================
// 网络与磁盘
// ============================================================

/// 查询本机网络状态
pub struct GetNetworkStatusTool;

#[async_trait]
impl AgentTool for GetNetworkStatusTool {
    fn name(&self) -> &'static str {
        "get_network_status"
    }

    fn description(&self) -> &'static str {
        "查询本机网络状态：各网卡的 IPv4 地址、网关、DNS，以及外网是否可达。用户问「网通不通」「IP 是多少」时用它。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        let script = r#"$r = [ordered]@{}
$r['hostname'] = $env:COMPUTERNAME
$r['interfaces'] = @(Get-NetIPConfiguration | Where-Object { $_.IPv4Address } | ForEach-Object {
  [ordered]@{ name = $_.InterfaceAlias; ip = $_.IPv4Address.IPAddress; gateway = $_.IPv4DefaultGateway.NextHop }
})
$r['dns'] = @(Get-DnsClientServerAddress -AddressFamily IPv4 | Where-Object { $_.ServerAddresses } | ForEach-Object {
  [ordered]@{ name = $_.InterfaceAlias; servers = @($_.ServerAddresses) }
})
$r | ConvertTo-Json -Depth 5 -Compress"#;

        let (ok, stdout, stderr) = run_powershell_b64(script)?;
        if !ok {
            return Ok(ToolResult::err("NETWORK_QUERY_FAILED", stderr.trim()));
        }
        let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|_| {
            serde_json::json!({ "raw": stdout.trim() })
        });

        // 「能不能上网」测的是 IP 连通，不是 DNS 通不通：
        // 用 IP 直连可以区分「网卡没连」和「DNS 挂了」这两种完全不同的故障。
        // 第一个目标是公共 DNS 的 IP，第二个是域名——两个都失败才算断网。
        let mut online = false;
        if let Ok(client) = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .no_proxy()
            .build()
        {
            for probe in ["http://223.5.5.5/", "http://www.baidu.com/"] {
                if let Ok(resp) = client.get(probe).send().await {
                    // 4xx 也说明网是通的，只是路径不对；5xx 和超时才可疑
                    if resp.status().as_u16() < 500 {
                        online = true;
                        break;
                    }
                }
            }
        }

        Ok(ToolResult::ok(serde_json::json!({
            "hostname": parsed.get("hostname").cloned().unwrap_or(serde_json::Value::Null),
            "interfaces": parsed.get("interfaces").cloned().unwrap_or(serde_json::json!([])),
            "dns": parsed.get("dns").cloned().unwrap_or(serde_json::json!([])),
            "reachable": online,
        })))
    }
}

/// 查询磁盘占用
pub struct GetDiskUsageTool;

#[async_trait]
impl AgentTool for GetDiskUsageTool {
    fn name(&self) -> &'static str {
        "get_disk_usage"
    }

    fn description(&self) -> &'static str {
        "列出所有本地磁盘的总容量、剩余空间与使用率。用户问「哪个盘还有空间」「C 盘满了没」时用它。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "min_free_gb": { "type": "integer", "description": "只返回剩余空间低于该值的盘（可选）" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let script = r#"Get-CimInstance Win32_LogicalDisk -Filter "DriveType=3" | ForEach-Object {
  [ordered]@{ drive = $_.DeviceID; label = $_.VolumeName; total = [int64]$_.Size; free = [int64]$_.FreeSpace }
} | ConvertTo-Json -Compress"#;

        let (ok, stdout, stderr) = run_powershell_b64(script)?;
        if !ok {
            return Ok(ToolResult::err("DISK_QUERY_FAILED", stderr.trim()));
        }
        let raw: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or(serde_json::json!([]));
        // 只有一个盘时 ConvertTo-Json 返回对象而不是数组
        let list = match raw {
            serde_json::Value::Array(items) => items,
            other if other.is_object() => vec![other],
            _ => vec![],
        };

        let min_free_gb = args["min_free_gb"].as_u64().unwrap_or(0) as f64;
        let mut drives = Vec::new();
        let mut total_edge: f64 = 0.0;
        let mut free_edge: f64 = 0.0;

        for item in list {
            let bytes_total = item["total"].as_f64().unwrap_or(0.0);
            let bytes_free = item["free"].as_f64().unwrap_or(0.0);
            let gb_total = bytes_total / 1024.0 / 1024.0 / 1024.0;
            let gb_free = bytes_free / 1024.0 / 1024.0 / 1024.0;
            total_edge += gb_total;
            free_edge += gb_free;
            if min_free_gb > 0.0 && gb_free >= min_free_gb {
                continue;
            }
            let used_pct = if gb_total > 0.0 {
                ((gb_total - gb_free) / gb_total * 100.0).round()
            } else {
                0.0
            };
            drives.push(serde_json::json!({
                "drive": item["drive"].as_str().unwrap_or(""),
                "label": item["label"].as_str().unwrap_or(""),
                "total_gb": (gb_total * 10.0).round() / 10.0,
                "free_gb": (gb_free * 10.0).round() / 10.0,
                "used_percent": used_pct,
                "critical": gb_free < 5.0,
            }));
        }

        Ok(ToolResult::ok(serde_json::json!({
            "drives": drives,
            "count": drives.len(),
            "total_gb": (total_edge * 10.0).round() / 10.0,
            "free_gb": (free_edge * 10.0).round() / 10.0,
        })))
    }
}

// ============================================================
// 环境变量
// ============================================================

/// 读取环境变量
pub struct GetEnvironmentTool;

#[async_trait]
impl AgentTool for GetEnvironmentTool {
    fn name(&self) -> &'static str {
        "get_environment"
    }

    fn description(&self) -> &'static str {
        "读取环境变量。不传 name 时列出全部变量名；传 name 时返回该变量的值。名字里带 KEY/TOKEN/SECRET 等字样的一律打码。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "变量名（可选）。不传则只列变量名" },
                "reveal_secret": { "type": "boolean", "description": "是否允许返回疑似密钥的值，默认 false" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let name = args["name"].as_str().filter(|s| !s.is_empty());
        let reveal_secret = args["reveal_secret"].as_bool().unwrap_or(false);

        let is_secret = |key: &str| {
            let upper = key.to_uppercase();
            SECRET_HINTS.iter().any(|hint| upper.contains(hint))
        };

        if let Some(key) = name {
            return Ok(match std::env::var(key) {
                Ok(value) => {
                    if is_secret(key) && !reveal_secret {
                        ToolResult::ok(serde_json::json!({
                            "name": key,
                            "value": null,
                            "masked": true,
                            "chars": value.chars().count(),
                        }))
                    } else {
                        ToolResult::ok(serde_json::json!({
                            "name": key,
                            "value": value,
                            "masked": false,
                        }))
                    }
                }
                Err(_) => ToolResult::err(
                    "ENV_NOT_FOUND",
                    &format!("环境变量不存在: {}", key),
                ),
            });
        }

        let mut names: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
        names.sort();
        Ok(ToolResult::ok(serde_json::json!({
            "count": names.len(),
            "names": names,
            "note": "只返回变量名。要看具体值请用 name 参数，疑似密钥会被打码。",
        })))
    }
}

// ============================================================
// 锁屏 / 打开链接
// ============================================================

/// 锁定屏幕
pub struct LockScreenTool;

#[async_trait]
impl AgentTool for LockScreenTool {
    fn name(&self) -> &'static str {
        "lock_screen"
    }

    fn description(&self) -> &'static str {
        "立即锁定 Windows 屏幕。用户说「锁屏」「我要离开一下」时使用。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        run_powershell("rundll32.exe user32.dll,LockWorkStation; 'ok'")?;
        Ok(ToolResult::ok(serde_json::json!({
            "locked": true,
        })))
    }
}

/// 用默认浏览器打开链接
pub struct OpenUrlTool;

#[async_trait]
impl AgentTool for OpenUrlTool {
    fn name(&self) -> &'static str {
        "open_url"
    }

    fn description(&self) -> &'static str {
        "用系统默认浏览器打开一个 http/https 链接。只接受 http 与 https，其他协议一律拒绝。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "要打开的链接，必须 http:// 或 https:// 开头" }
            },
            "required": ["url"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let url = require_str(&args, "url")?;
        let lower = url.to_lowercase();
        if !lower.starts_with("http://") && !lower.starts_with("https://") {
            return Ok(ToolResult::err(
                "UNSUPPORTED_SCHEME",
                "只允许 http/https 链接",
            ));
        }

        #[cfg(windows)]
        {
            let script = format!(
                "Start-Process -FilePath ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}'))); 'ok'",
                literal(url)
            );
            let (ok, _out, stderr) = run_powershell(&script)?;
            if !ok {
                return Ok(ToolResult::err("OPEN_FAILED", stderr.trim()));
            }
            Ok(ToolResult::ok(serde_json::json!({
                "url": url,
                "opened": true,
            })))
        }

        #[cfg(not(windows))]
        {
            Ok(ToolResult::err("UNSUPPORTED_PLATFORM", "仅支持 Windows"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_literal_contains_no_quote() {
        let encoded = literal("it's a \"quoted\" 中文");
        assert!(!encoded.contains('\''));
        assert!(!encoded.contains('"'));
        assert!(!encoded.contains('\n'));
    }

    #[test]
    fn secret_hint_matches_common_names() {
        let is_secret = |key: &str| {
            let upper = key.to_uppercase();
            SECRET_HINTS.iter().any(|hint| upper.contains(hint))
        };
        assert!(is_secret("OPENAI_API_KEY"));
        assert!(is_secret("GITHUB_TOKEN"));
        assert!(is_secret("AWS_SECRET_ACCESS_KEY"));
        assert!(is_secret("PATH") == false, "PATH 不该被当成密钥");
        assert!(!is_secret("JAVA_HOME"));
        assert!(!is_secret("TEMP"));
    }

    /// 实机探针：确认 PowerShell 链路在本机真的能跑通，而不是只编译过。
    ///
    /// 默认 ignore —— 它会**改写用户剪贴板**（结束时还原原文）。
    /// 手动执行：`cargo test real_system_probes -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "会改写剪贴板，需在真实 Windows 桌面会话下手动跑"]
    async fn real_system_probes_work() {
        let original = ReadClipboardTool
            .execute(serde_json::json!({}))
            .await
            .expect("读剪贴板");
        let before = original
            .data
            .as_ref()
            .and_then(|d| d["text"].as_str())
            .unwrap_or("")
            .to_string();

        let marker = format!("pc-guardian 探针 {}", std::process::id());
        let written = WriteClipboardTool
            .execute(serde_json::json!({ "text": marker }))
            .await
            .expect("写剪贴板");
        assert!(written.success, "写入失败: {:?}", written.error);
        assert_eq!(
            written.data.as_ref().unwrap()["readback_matched"],
            serde_json::json!(true),
            "回读必须与写入一致"
        );

        let echoed = ReadClipboardTool
            .execute(serde_json::json!({}))
            .await
            .expect("回读剪贴板");
        assert_eq!(
            echoed.data.as_ref().unwrap()["text"].as_str().unwrap(),
            marker
        );

        // 还原，别把用户的剪贴板留在探针状态
        let _ = WriteClipboardTool
            .execute(serde_json::json!({ "text": before }))
            .await;

        let disks = GetDiskUsageTool
            .execute(serde_json::json!({}))
            .await
            .expect("磁盘");
        assert!(disks.success, "磁盘查询失败: {:?}", disks.error);
        let count = disks.data.as_ref().unwrap()["count"].as_u64().unwrap();
        assert!(count >= 1, "至少要识别出一个本地磁盘");
        println!("磁盘: {}", disks.data.unwrap());

        let net = GetNetworkStatusTool
            .execute(serde_json::json!({}))
            .await
            .expect("网络");
        assert!(net.success, "网络查询失败: {:?}", net.error);
        println!("网络: {}", net.data.unwrap());

        let env = GetEnvironmentTool
            .execute(serde_json::json!({}))
            .await
            .expect("环境变量");
        assert!(env.success);
        assert!(env.data.as_ref().unwrap()["count"].as_u64().unwrap() > 5);
        println!("环境变量数量: {}", env.data.unwrap()["count"]);
    }
}
