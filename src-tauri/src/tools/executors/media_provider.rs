//! 外部 AI 服务商的连接配置与自测（供图像 / 视频生成共用）。
//!
//! 设计要点：**服务商不是写死的枚举**。这里只认一种协议——OpenAI 兼容的
//! `{base_url}/images/generations`、`{base_url}/videos`，于是"换一家"这件事
//! 退化成在设置里改三行：base_url、api_key、model。真实存在的服务商里，
//! 官方 OpenAI、各类中转、以及绝大多数国内平台的兼容端点都符合这套形状；
//! 硬编码一串厂商名只会变成一张填不完的表，而且一旦厂商改接口就全废。
//!
//! 凭据只从 settings 表读，**不写日志、不进工具返回值**。状态查询会把
//! api_key 掩码后回显（末 4 位），让用户能确认"我填的是哪一个"，同时
//! 不足以还原凭据。

use async_trait::async_trait;
use rusqlite::params;
use serde_json::{json, Value};

use crate::error::AppResult;
use crate::tools::{AgentTool, RiskLevel, ToolResult};

// ============================================================
// 配置读取
// ============================================================

/// 读一条 settings；空串视为未配置（前端输入框清空 = 关掉这个能力）。
pub fn setting(key: &str) -> Option<String> {
    let conn = crate::memory::open_connection()?;
    let value: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .ok();
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn setting_or(key: &str, fallback: &str) -> String {
    setting(key).unwrap_or_else(|| fallback.to_string())
}

fn setting_u64(key: &str, fallback: u64) -> u64 {
    setting(key)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(fallback)
}

/// 掩码：空值保持空串（前端据此区分"未配置/已配置"），非空只留末 4 位。
pub fn mask(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        return "••••••••".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("••••••••{tail}")
}

/// 去掉末尾多余的 `/`，再拼上接口路径。
///
/// 用户填 `https://x/v1/` 和 `https://x/v1` 都是常态，拼接时不能靠猜。
pub fn endpoint(base_url: &str, path: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    let path = path.trim_start_matches('/');
    format!("{base}/{path}")
}

#[derive(Debug, Clone)]
pub struct ImageConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub size: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct VideoConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub size: String,
    pub duration_sec: u64,
    pub poll_timeout_ms: u64,
}

/// 缺哪些键就报哪些键，不拿默认值硬凑一个"看起来能用"的配置。
pub fn load_image_config() -> Result<ImageConfig, Vec<&'static str>> {
    let api_key = setting("image.api_key");
    let model = setting("image.model");
    let mut missing = Vec::new();
    if api_key.is_none() {
        missing.push("image.api_key");
    }
    if model.is_none() {
        missing.push("image.model");
    }
    if !missing.is_empty() {
        // api_key 没填时 base_url/model 的缺省没有意义，但仍一起报，
        // 免得用户填完 key 又撞一次"缺 model"。
        return Err(missing);
    }

    Ok(ImageConfig {
        base_url: setting_or("image.base_url", "https://api.openai.com/v1"),
        api_key: api_key.unwrap_or_default(),
        model: model.unwrap_or_else(|| "gpt-image-1".to_string()),
        size: setting_or("image.size", "1024x1024"),
        timeout_ms: setting_u64("image.timeout_ms", 180_000),
    })
}

pub fn load_video_config() -> Result<VideoConfig, Vec<&'static str>> {
    let api_key = setting("video.api_key");
    let model = setting("video.model");
    let mut missing = Vec::new();
    if api_key.is_none() {
        missing.push("video.api_key");
    }
    if model.is_none() {
        missing.push("video.model");
    }
    if !missing.is_empty() {
        return Err(missing);
    }

    Ok(VideoConfig {
        base_url: setting_or("video.base_url", "https://api.openai.com/v1"),
        api_key: api_key.unwrap_or_default(),
        model: model.unwrap_or_else(|| "sora-2".to_string()),
        size: setting_or("video.size", "1280x720"),
        duration_sec: setting_u64("video.duration_sec", 4),
        poll_timeout_ms: setting_u64("video.poll_timeout_ms", 600_000),
    })
}

/// 邮件收件配置。user/password 留空时复用 SMTP 那组——同一个邮箱的
/// 授权码发信收信通常通用，强制填两遍只会让人填错。
#[derive(Debug, Clone)]
pub struct MailReadConfig {
    pub host: String,
    pub port: u16,
    pub security: String,
    pub user: String,
    pub password: String,
}

pub fn load_mail_read_config() -> Result<MailReadConfig, Vec<&'static str>> {
    let host = setting("mail.imap_host");
    let user = setting("mail.imap_user").or_else(|| setting("mail.smtp_user"));
    let password = setting("mail.imap_password").or_else(|| setting("mail.smtp_password"));

    let mut missing = Vec::new();
    if host.is_none() {
        missing.push("mail.imap_host");
    }
    if user.is_none() {
        missing.push("mail.imap_user（或 mail.smtp_user）");
    }
    if password.is_none() {
        missing.push("mail.imap_password（或 mail.smtp_password）");
    }
    if !missing.is_empty() {
        return Err(missing);
    }

    let port = setting("mail.imap_port")
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(993);
    let security = setting("mail.imap_security")
        .map(|v| v.trim().to_lowercase())
        .unwrap_or_else(|| {
            if port == 143 {
                "starttls".into()
            } else {
                "tls".into()
            }
        });

    Ok(MailReadConfig {
        host: host.unwrap(),
        port,
        security,
        user: user.unwrap(),
        password: password.unwrap(),
    })
}

/// 缺配置的统一回话：把键名报回去，别猜用户想用哪家。
pub fn missing_result(kind: &str, missing: &[&str]) -> ToolResult {
    ToolResult::err(
        "PROVIDER_NOT_CONFIGURED",
        &format!(
            "{} 未配置，缺少：{}。请在「设置 → AI 能力」里填入服务地址与 API Key；本工具不会用猜测的地址或凭据代跑。",
            kind,
            missing.join(" / ")
        ),
    )
}

// ============================================================
// 生成台账（image / video 共用）
// ============================================================

/// 写一条生成记录。失败不阻断主流程——台账是旁路，不是真相来源，
/// 但要让调用方在返回值里看到"没记上"，否则排查时两眼一抹黑。
pub fn record_generation(
    kind: &str,
    provider: &str,
    model: &str,
    task_id: Option<&str>,
    prompt: &str,
    size: Option<&str>,
    duration_sec: Option<i64>,
    status: &str,
    request_json: &Value,
) -> Option<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let conn = crate::memory::open_connection()?;
    conn.execute(
        "INSERT INTO media_generations
           (id, kind, provider, model, task_id, prompt, size, duration_sec, status, request_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            kind,
            provider,
            model,
            task_id,
            prompt,
            size,
            duration_sec,
            status,
            request_json.to_string()
        ],
    )
    .ok()?;
    Some(id)
}

/// 更新台账。`artifact_path` 传 None 表示这次不改产物路径（不是清空）。
pub fn update_generation(
    id: &str,
    status: &str,
    remote_status: Option<&str>,
    artifact_path: Option<&str>,
    artifact_bytes: Option<i64>,
    error: Option<&str>,
) {
    let Some(conn) = crate::memory::open_connection() else {
        return;
    };
    let _ = conn.execute(
        "UPDATE media_generations SET
            status = ?2,
            remote_status = COALESCE(?3, remote_status),
            artifact_path = COALESCE(?4, artifact_path),
            artifact_bytes = COALESCE(?5, artifact_bytes),
            error = COALESCE(?6, error),
            updated_at = datetime('now')
         WHERE id = ?1",
        params![
            id,
            status,
            remote_status,
            artifact_path,
            artifact_bytes,
            error
        ],
    );
}

/// 按 task_id 找台账（远端任务 id 是跨进程重启后唯一的线索）。
pub fn find_by_task_id(task_id: &str) -> Option<(String, String, String)> {
    let conn = crate::memory::open_connection()?;
    conn.query_row(
        "SELECT id, kind, status FROM media_generations WHERE task_id = ?1
         ORDER BY created_at DESC LIMIT 1",
        [task_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .ok()
}

// ============================================================
// media_provider_status — 配置完整度 + 可选连通性自测
// ============================================================

pub struct MediaProviderStatusTool;

#[async_trait]
impl AgentTool for MediaProviderStatusTool {
    fn name(&self) -> &'static str {
        "media_provider_status"
    }

    fn description(&self) -> &'static str {
        "查看图像生成 / 视频生成 / 邮件收件（IMAP）三处的服务商配置：是否配齐、缺哪个键、基址与模型是什么。probe=true 时额外做一次只读连通性自测（不发真实生成请求）。凭据只回显掩码。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "probe": { "type": "boolean", "description": "是否做连通性自测，默认 false（只读本地配置）" }
            },
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let probe = args["probe"].as_bool().unwrap_or(false);

        let image = match load_image_config() {
            Ok(cfg) => {
                let reachable = if probe {
                    Some(probe_openai_base(&cfg.base_url, &cfg.api_key).await)
                } else {
                    None
                };
                json!({
                    "configured": true,
                    "base_url": cfg.base_url,
                    "model": cfg.model,
                    "default_size": cfg.size,
                    "timeout_ms": cfg.timeout_ms,
                    "api_key": mask(&cfg.api_key),
                    "probe": reachable,
                })
            }
            Err(missing) => json!({
                "configured": false,
                "missing": missing,
                "base_url": setting_or("image.base_url", "https://api.openai.com/v1"),
                "model": setting("image.model"),
                "api_key": mask(&setting("image.api_key").unwrap_or_default()),
            }),
        };

        let video = match load_video_config() {
            Ok(cfg) => {
                let reachable = if probe {
                    Some(probe_openai_base(&cfg.base_url, &cfg.api_key).await)
                } else {
                    None
                };
                json!({
                    "configured": true,
                    "base_url": cfg.base_url,
                    "model": cfg.model,
                    "default_size": cfg.size,
                    "default_duration_sec": cfg.duration_sec,
                    "poll_timeout_ms": cfg.poll_timeout_ms,
                    "api_key": mask(&cfg.api_key),
                    "probe": reachable,
                })
            }
            Err(missing) => json!({
                "configured": false,
                "missing": missing,
                "base_url": setting_or("video.base_url", "https://api.openai.com/v1"),
                "model": setting("video.model"),
                "api_key": mask(&setting("video.api_key").unwrap_or_default()),
            }),
        };

        let mail_read = match load_mail_read_config() {
            Ok(cfg) => {
                let probe_result = if probe {
                    Some(probe_imap(&cfg).await)
                } else {
                    None
                };
                json!({
                    "configured": true,
                    "imap_host": cfg.host,
                    "imap_port": cfg.port,
                    "imap_security": cfg.security,
                    "user": cfg.user,
                    "password": mask(&cfg.password),
                    "probe": probe_result,
                })
            }
            Err(missing) => json!({
                "configured": false,
                "missing": missing,
                "imap_host": setting("mail.imap_host"),
                "imap_port": setting("mail.imap_port"),
                "imap_security": setting("mail.imap_security"),
                "password": mask(&setting("mail.imap_password").unwrap_or_default()),
            }),
        };

        Ok(ToolResult::ok(json!({
            "image": image,
            "video": video,
            "mail_read": mail_read,
            "hint": "任一能力未配置时，对应工具会直接返回缺哪些键，不会静默降级到别的服务商。",
        })))
    }
}

/// 只读自测：`GET {base}/models`。有的兼容网关不实现这个端点，
/// 404 不算失败——如实说明"端点未提供自测入口"即可，别把它报成"连不上"。
async fn probe_openai_base(base_url: &str, api_key: &str) -> Value {
    let url = endpoint(base_url, "models");
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("e-assisant/0.1")
        .build()
    {
        Ok(c) => c,
        Err(e) => return json!({ "ok": false, "error": format!("HTTP 客户端构造失败: {e}") }),
    };

    match client.get(&url).bearer_auth(api_key).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                json!({ "ok": true, "http_status": status.as_u16(), "url": url })
            } else if status.as_u16() == 404 || status.as_u16() == 405 {
                json!({
                    "ok": null,
                    "http_status": status.as_u16(),
                    "url": url,
                    "note": "该端点未提供 /models，无法用只读方式自测；这不代表配置错误",
                })
            } else {
                let body = resp.text().await.unwrap_or_default();
                json!({
                    "ok": false,
                    "http_status": status.as_u16(),
                    "url": url,
                    "error": truncate(&body, 300),
                })
            }
        }
        Err(e) => json!({ "ok": false, "url": url, "error": e.to_string() }),
    }
}

async fn probe_imap(cfg: &MailReadConfig) -> Value {
    let cfg = cfg.clone();
    let joined = tokio::task::spawn_blocking(move || {
        let mut session = super::mail_read::connect_session(&cfg, "INBOX")?;
        let exists = session.exists;
        session.logout();
        Ok::<u32, String>(exists)
    })
    .await;

    match joined {
        Ok(Ok(exists)) => json!({ "ok": true, "inbox_messages": exists }),
        Ok(Err(e)) => json!({ "ok": false, "error": e }),
        Err(e) => json!({ "ok": false, "error": format!("自测任务异常: {e}") }),
    }
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_handles_trailing_slash() {
        assert_eq!(
            endpoint("https://api.openai.com/v1/", "images/generations"),
            "https://api.openai.com/v1/images/generations"
        );
        assert_eq!(
            endpoint("https://api.openai.com/v1", "/images/generations"),
            "https://api.openai.com/v1/images/generations"
        );
    }

    #[test]
    fn mask_keeps_only_tail() {
        assert_eq!(mask(""), "");
        assert_eq!(mask("short"), "••••••••");
        assert_eq!(mask("sk-1234567890abcdef"), "••••••••cdef");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("abcdef", 3), "abc…");
        assert_eq!(truncate("中文字", 2), "中文…");
        assert_eq!(truncate("ab", 5), "ab");
    }
}
