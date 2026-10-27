//! 邮件收件（IMAP）。
//!
//! 发信是 `messaging.rs` 里的 SMTP；收信是另一条协议，必须单独实现。
//! 选 IMAP 而不是某家厂商的 HTTP API：只有它是通用标准，换服务商时
//! 用户只需要改 `mail.imap_host` 和密码（授权码），代码一行不用动。
//!
//! 三个工具的分层是刻意分开的：
//! - `recent_emails` 只取头部字段（列表页），**不下载正文**，几十封信也就几十 KB
//! - `read_email` 取一整封，正文按字符上限截断，附件只给清单
//! - `read_email_attachments` 才真正把附件字节落盘
//!
//! 这样做是因为"看看 inbox 有什么"和"把附件存下来"的成本差了两个数量级，
//! 合并成一个工具会让每次看列表都可能拖回几十 MB。
//!
//! 凭据只从 settings 读，不进返回值（掩码除外）。IMAP 是同步阻塞协议，
//! 所有网络动作都放在 `spawn_blocking` 里，不占 tokio 的工作线程。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::media_provider::{load_mail_read_config, missing_result, MailReadConfig};
use crate::error::{AppError, AppResult};
use crate::mail_mime;
use crate::tools::{AgentTool, RiskLevel, ToolResult};

/// 单次读取的邮件体积上限（读整封时用）。超过就不下载正文，
/// 只报大小——一封信拖 200 MB 会把内存和等待时间一起毁掉。
const MAX_MESSAGE_BYTES: u32 = 32 * 1024 * 1024;
/// 单封邮件正文默认返回字符数。
const DEFAULT_MAX_CHARS: usize = 8000;
/// 单个附件落盘上限。
const MAX_ATTACHMENT_BYTES: usize = 64 * 1024 * 1024;

// ============================================================
// 连接
// ============================================================

enum Inner {
    /// tls 与 starttls 最终都是 TLS 流，归一类
    Tls(imap::Session<native_tls::TlsStream<TcpStream>>),
    Plain(imap::Session<TcpStream>),
}

pub struct MailSession {
    inner: Inner,
    /// 邮箱里的邮件总数（SELECT 的 EXISTS）
    pub exists: u32,
}

impl MailSession {
    fn uid_search(&mut self, query: &str) -> Result<Vec<u32>, String> {
        let result = match &mut self.inner {
            Inner::Tls(s) => s.uid_search(query),
            Inner::Plain(s) => s.uid_search(query),
        };
        result
            .map(|set| {
                let mut v: Vec<u32> = set.into_iter().collect();
                v.sort_unstable();
                v
            })
            .map_err(|e| format!("UID SEARCH 失败：{e}"))
    }

    fn uid_fetch(&mut self, set: &str, query: &str) -> Result<Vec<Fetched>, String> {
        let result = match &mut self.inner {
            Inner::Tls(s) => s.uid_fetch(set, query),
            Inner::Plain(s) => s.uid_fetch(set, query),
        };
        let fetches = result.map_err(|e| format!("UID FETCH 失败：{e}"))?;
        Ok(fetches.iter().map(to_owned).collect())
    }

    pub fn logout(&mut self) {
        match &mut self.inner {
            Inner::Tls(s) => {
                let _ = s.logout();
            }
            Inner::Plain(s) => {
                let _ = s.logout();
            }
        }
    }
}

/// 把带生命周期的 `Fetch` 拷成自持结构——零拷贝缓冲区一 drop，
/// 里面的 `&[u8]` 就失效了，不能带出闭包。
#[derive(Debug, Default)]
pub struct Fetched {
    pub uid: Option<u32>,
    pub size: Option<u32>,
    pub flags: Vec<String>,
    pub internal_date: Option<String>,
    pub header: Option<Vec<u8>>,
    pub body: Option<Vec<u8>>,
}

fn to_owned(fetch: &imap::types::Fetch) -> Fetched {
    Fetched {
        uid: fetch.uid,
        size: fetch.size,
        flags: fetch.flags().iter().map(|f| f.to_string()).collect(),
        internal_date: fetch.internal_date().map(|d| d.to_rfc3339()),
        header: fetch.header().map(|h| h.to_vec()),
        body: fetch.body().map(|b| b.to_vec()),
    }
}

/// 建连接 + 登录 + SELECT。`mailbox` 传 INBOX 之类。
pub fn connect_session(cfg: &MailReadConfig, mailbox: &str) -> Result<MailSession, String> {
    let security = cfg.security.trim().to_ascii_lowercase();
    let addr = (cfg.host.as_str(), cfg.port);

    let (inner, exists) = match security.as_str() {
        "tls" | "ssl" => {
            let tls = native_tls::TlsConnector::builder()
                .build()
                .map_err(|e| format!("TLS 初始化失败：{e}"))?;
            let client = imap::connect(addr, cfg.host.as_str(), &tls)
                .map_err(|e| format!("连接 {}:{} 失败：{e}", cfg.host, cfg.port))?;
            let mut session = login(client, cfg)?;
            let exists = select(&mut session, mailbox)?;
            (Inner::Tls(session), exists)
        }
        "starttls" => {
            let tls = native_tls::TlsConnector::builder()
                .build()
                .map_err(|e| format!("TLS 初始化失败：{e}"))?;
            let client = imap::connect_starttls(addr, cfg.host.as_str(), &tls)
                .map_err(|e| format!("STARTTLS 连接失败：{e}"))?;
            let mut session = login(client, cfg)?;
            let exists = select(&mut session, mailbox)?;
            (Inner::Tls(session), exists)
        }
        "none" | "plain" => {
            let tcp = TcpStream::connect(addr)
                .map_err(|e| format!("连接 {}:{} 失败：{e}", cfg.host, cfg.port))?;
            // 明文连接必须有超时，否则网络异常时会永久挂住一个阻塞线程
            let _ = tcp.set_read_timeout(Some(Duration::from_secs(60)));
            let _ = tcp.set_write_timeout(Some(Duration::from_secs(60)));
            let client = imap::Client::new(tcp);
            let mut session = login(client, cfg)?;
            let exists = select(&mut session, mailbox)?;
            (Inner::Plain(session), exists)
        }
        other => {
            return Err(format!(
                "不认识的 mail.imap_security={other}，只接受 tls / starttls / none"
            ))
        }
    };

    Ok(MailSession { inner, exists })
}

fn login<T: Read + Write>(
    client: imap::Client<T>,
    cfg: &MailReadConfig,
) -> Result<imap::Session<T>, String> {
    client.login(&cfg.user, &cfg.password).map_err(|(e, _)| {
        format!(
            "登录 {} 失败：{e}（多数邮箱要求用「授权码」而不是登录密码，且需先在网页端开启 IMAP）",
            cfg.user
        )
    })
}

fn select<T: Read + Write>(session: &mut imap::Session<T>, mailbox: &str) -> Result<u32, String> {
    session
        .select(mailbox)
        .map(|m| m.exists)
        .map_err(|e| format!("选择邮箱 {mailbox} 失败：{e}"))
}

async fn with_session<T>(
    cfg: MailReadConfig,
    mailbox: String,
    f: impl FnOnce(&mut MailSession) -> Result<T, String> + Send + 'static,
) -> Result<T, String>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut session = connect_session(&cfg, &mailbox)?;
        let result = f(&mut session);
        session.logout();
        result
    })
    .await
    .map_err(|e| format!("IMAP 任务异常：{e}"))?
}

fn not_configured(missing: Vec<&'static str>) -> ToolResult {
    missing_result("邮件收件（IMAP）", &missing)
}

// ============================================================
// recent_emails
// ============================================================

pub struct RecentEmailsTool;

#[async_trait]
impl AgentTool for RecentEmailsTool {
    fn name(&self) -> &'static str {
        "recent_emails"
    }

    fn description(&self) -> &'static str {
        "列出邮箱里最近若干封邮件的概要（主题/发件人/收件人/日期/大小/未读状态），只取头部字段、不下载正文。用于回答「最近有什么邮件」。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "description": "条数，默认 10，最多 50" },
                "mailbox": { "type": "string", "description": "邮箱名，默认 INBOX" },
                "unread_only": { "type": "boolean", "description": "只看未读，默认 false" },
                "since_days": { "type": "integer", "description": "可选，只看最近 N 天" }
            },
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let cfg = match load_mail_read_config() {
            Ok(cfg) => cfg,
            Err(missing) => return Ok(not_configured(missing)),
        };

        let limit = args["limit"].as_u64().unwrap_or(10).clamp(1, 50) as usize;
        let mailbox = args["mailbox"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("INBOX")
            .to_string();
        let unread_only = args["unread_only"].as_bool().unwrap_or(false);
        let since_days = args["since_days"].as_u64();

        // IMAP SEARCH 的日期格式是 DD-Mon-YYYY（英文月份缩写），且是"当天及以后"
        let mut criteria: Vec<String> = Vec::new();
        if unread_only {
            criteria.push("UNSEEN".to_string());
        }
        if let Some(days) = since_days {
            let days = days.clamp(1, 3650) as i64;
            let since = chrono::Local::now() - chrono::Duration::days(days);
            criteria.push(format!("SINCE {}", since.format("%d-%b-%Y")));
        }
        if criteria.is_empty() {
            criteria.push("ALL".to_string());
        }
        let query = criteria.join(" ");
        let mailbox_for_task = mailbox.clone();

        let outcome = with_session(cfg.clone(), mailbox.clone(), move |session| {
            let mut uids = session.uid_search(&query)?;
            uids.sort_unstable();
            // 取最新的 limit 条
            let selected: Vec<u32> = uids.iter().rev().take(limit).copied().collect();
            if selected.is_empty() {
                return Ok((0u32, Vec::<Value>::new()));
            }
            let set = selected
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let fetches = session.uid_fetch(
                &set,
                "(UID FLAGS RFC822.SIZE INTERNALDATE BODY.PEEK[HEADER.FIELDS (SUBJECT FROM TO DATE MESSAGE-ID)])",
            )?;

            let mut emails: Vec<Value> = Vec::new();
            for item in fetches {
                let header_bytes = item.header.unwrap_or_default();
                let parsed = mail_mime::parse_header_only(&header_bytes);
                let seen = item.flags.iter().any(|f| f.eq_ignore_ascii_case("\\Seen"));
                emails.push(json!({
                    "uid": item.uid,
                    "subject": parsed.subject,
                    "from": parsed.from,
                    "to": parsed.to,
                    "date": parsed.date,
                    "internal_date": item.internal_date,
                    "size_bytes": item.size,
                    "seen": seen,
                    "flags": item.flags,
                    "message_id": parsed.message_id,
                }));
            }
            // 新的在前
            emails.sort_by_key(|e| {
                std::cmp::Reverse(e["internal_date"].as_str().unwrap_or("").to_string())
            });
            Ok((session.exists, emails))
        })
        .await;

        match outcome {
            Ok((exists, emails)) => Ok(ToolResult::ok(json!({
                "mailbox": mailbox_for_task,
                "count": emails.len(),
                "mailbox_total": exists,
                "emails": emails,
                "imap_host": cfg.host,
                "user": cfg.user,
                "note": "只取了头部字段，正文与附件未下载；读某一封用 read_email(uid)。",
            }))),
            Err(e) => Ok(ToolResult::err("MAIL_READ_FAILED", &e)),
        }
    }
}

// ============================================================
// read_email
// ============================================================

pub struct ReadEmailTool;

#[async_trait]
impl AgentTool for ReadEmailTool {
    fn name(&self) -> &'static str {
        "read_email"
    }

    fn description(&self) -> &'static str {
        "按 UID 读一整封邮件：主题、收发人、日期、正文（超出上限截断）、附件清单（不落盘）。uid 从 recent_emails 的返回值里拿。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "uid": { "type": "integer", "description": "邮件 UID（recent_emails 返回值里的 uid）" },
                "mailbox": { "type": "string", "description": "邮箱名，默认 INBOX" },
                "max_chars": { "type": "integer", "description": "正文返回的最大字符数，默认 8000，最多 50000" }
            },
            "required": ["uid"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let uid = args["uid"]
            .as_u64()
            .ok_or_else(|| AppError::InvalidArgument("uid 必须是正整数".to_string()))?
            as u32;

        let cfg = match load_mail_read_config() {
            Ok(cfg) => cfg,
            Err(missing) => return Ok(not_configured(missing)),
        };

        let mailbox = args["mailbox"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("INBOX")
            .to_string();
        let max_chars = (args["max_chars"]
            .as_u64()
            .unwrap_or(DEFAULT_MAX_CHARS as u64) as usize)
            .clamp(500, 50_000);

        let outcome = with_session(cfg.clone(), mailbox.clone(), move |session| {
            let fetches =
                session.uid_fetch(&uid.to_string(), "(UID FLAGS RFC822.SIZE BODY.PEEK[])")?;
            let item = fetches
                .into_iter()
                .next()
                .ok_or_else(|| format!("邮箱里没有 uid={uid} 的邮件"))?;

            if let Some(size) = item.size {
                if size > MAX_MESSAGE_BYTES {
                    return Err(format!(
                        "这封邮件有 {:.1} MB，超过 {} MB 的读取上限，已在下载正文前中止",
                        size as f64 / 1024.0 / 1024.0,
                        MAX_MESSAGE_BYTES / 1024 / 1024
                    ));
                }
            }

            let raw = item
                .body
                .ok_or_else(|| "服务端没有返回邮件正文（可能被服务端策略限制）".to_string())?;
            let parsed = mail_mime::parse_message(&raw);

            // 没有 text/plain 分区时，把 HTML 粗转成文本——否则用户只能看到空正文。
            let (body_text, body_source) = if parsed.text_body.trim().is_empty() {
                let from_html = mail_mime::html_to_text(&parsed.html_body);
                if from_html.trim().is_empty() {
                    (String::new(), "empty")
                } else {
                    (from_html, "html")
                }
            } else {
                (parsed.text_body.clone(), "text")
            };

            let total_chars = body_text.chars().count();
            let body_out = if total_chars > max_chars {
                let mut out = String::new();
                for ch in body_text.chars().take(max_chars) {
                    out.push(ch);
                }
                out.push_str(&format!("\n…（已截断，共 {total_chars} 字）"));
                out
            } else {
                body_text
            };

            Ok(json!({
                "uid": item.uid,
                "subject": parsed.subject,
                "from": parsed.from,
                "to": parsed.to,
                "date": parsed.date,
                "message_id": parsed.message_id,
                "body": body_out,
                "body_source": body_source,
                "body_total_chars": total_chars,
                "truncated": total_chars > max_chars,
                "attachments": parsed.attachments.iter().map(|a| json!({
                    "filename": a.filename,
                    "content_type": a.content_type,
                    "size_bytes": a.size,
                })).collect::<Vec<_>>(),
                "seen": item.flags.iter().any(|f| f.eq_ignore_ascii_case("\\Seen")),
                "size_bytes": item.size,
                "note": if parsed.attachments.is_empty() {
                    "".to_string()
                } else {
                    "附件只给了清单；要落盘用 read_email_attachments(uid)。".to_string()
                },
            }))
        })
        .await;

        match outcome {
            Ok(data) => Ok(ToolResult::ok(data)),
            Err(e) => Ok(ToolResult::err("MAIL_READ_FAILED", &e)),
        }
    }
}

// ============================================================
// read_email_attachments
// ============================================================

pub struct ReadEmailAttachmentsTool;

#[async_trait]
impl AgentTool for ReadEmailAttachmentsTool {
    fn name(&self) -> &'static str {
        "read_email_attachments"
    }

    fn description(&self) -> &'static str {
        "把指定邮件的附件保存到本地目录，返回每个文件的路径与字节数（落盘后重新核对大小）。可用 names 只取指定文件名。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "uid": { "type": "integer", "description": "邮件 UID" },
                "mailbox": { "type": "string", "description": "邮箱名，默认 INBOX" },
                "output_dir": { "type": "string", "description": "保存目录，默认 <下载目录>/EAssisant-mail" },
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "可选，只保存这些文件名（精确匹配，不区分大小写）"
                },
                "overwrite": { "type": "boolean", "description": "同名文件是否覆盖，默认 false（自动加序号）" }
            },
            "required": ["uid"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // 会把外部邮件的字节写到本地磁盘——MEDIUM，让人有机会确认。
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let uid = args["uid"]
            .as_u64()
            .ok_or_else(|| AppError::InvalidArgument("uid 必须是正整数".to_string()))?
            as u32;

        let cfg = match load_mail_read_config() {
            Ok(cfg) => cfg,
            Err(missing) => return Ok(not_configured(missing)),
        };

        let mailbox = args["mailbox"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("INBOX")
            .to_string();

        let filter: Option<Vec<String>> = args["names"].as_array().map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        });
        let overwrite = args["overwrite"].as_bool().unwrap_or(false);

        let out_dir = match args["output_dir"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(raw) => {
                let path = std::path::PathBuf::from(raw);
                if path.is_absolute() {
                    path
                } else {
                    default_mail_dir().join(path)
                }
            }
            None => default_mail_dir(),
        };
        if let Err(e) = std::fs::create_dir_all(&out_dir) {
            return Ok(ToolResult::err(
                "MAIL_ATTACHMENT_DIR_FAILED",
                &format!("创建目录 {} 失败：{e}", out_dir.display()),
            ));
        }

        let outcome = with_session(cfg.clone(), mailbox.clone(), move |session| {
            let fetches = session.uid_fetch(
                &uid.to_string(),
                "(UID FLAGS RFC822.SIZE BODY.PEEK[])",
            )?;
            let item = fetches
                .into_iter()
                .next()
                .ok_or_else(|| format!("邮箱里没有 uid={uid} 的邮件"))?;
            if let Some(size) = item.size {
                if size > MAX_MESSAGE_BYTES {
                    return Err(format!(
                        "这封邮件有 {:.1} MB，超过 {} MB 的读取上限",
                        size as f64 / 1024.0 / 1024.0,
                        MAX_MESSAGE_BYTES / 1024 / 1024
                    ));
                }
            }
            let raw = item
                .body
                .ok_or_else(|| "服务端没有返回邮件正文".to_string())?;

            let attachments = mail_mime::extract_attachments(&raw);
            if attachments.is_empty() {
                return Ok(json!({
                    "uid": uid,
                    "saved": Vec::<Value>::new(),
                    "skipped": Vec::<Value>::new(),
                    "count": 0,
                    "note": "这封邮件没有附件。",
                }));
            }

            let mut saved: Vec<Value> = Vec::new();
            let mut skipped: Vec<Value> = Vec::new();

            for attachment in attachments {
                if let Some(filter) = &filter {
                    if !filter.contains(&attachment.filename.to_ascii_lowercase()) {
                        skipped.push(json!({
                            "filename": attachment.filename,
                            "reason": "不在 names 过滤列表里",
                        }));
                        continue;
                    }
                }
                if attachment.bytes.len() > MAX_ATTACHMENT_BYTES {
                    skipped.push(json!({
                        "filename": attachment.filename,
                        "size_bytes": attachment.bytes.len(),
                        "reason": format!("超过 {} MB 单文件上限", MAX_ATTACHMENT_BYTES / 1024 / 1024),
                    }));
                    continue;
                }

                let safe_name = sanitize_filename(&attachment.filename);
                let target = unique_path(&out_dir, &safe_name, overwrite);
                match std::fs::write(&target, &attachment.bytes) {
                    Ok(()) => {
                        // 落盘校验：重新读属性比对字节数
                        let disk = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(0);
                        if disk as usize != attachment.bytes.len() {
                            skipped.push(json!({
                                "filename": attachment.filename,
                                "path": target.display().to_string(),
                                "reason": format!("落盘字节数不一致：内存 {} / 磁盘 {disk}", attachment.bytes.len()),
                            }));
                            continue;
                        }
                        saved.push(json!({
                            "filename": attachment.filename,
                            "path": target.display().to_string(),
                            "bytes": disk,
                            "content_type": attachment.content_type,
                        }));
                    }
                    Err(e) => skipped.push(json!({
                        "filename": attachment.filename,
                        "reason": format!("写入失败：{e}"),
                    })),
                }
            }

            Ok(json!({
                "uid": uid,
                "count": saved.len(),
                "saved": saved,
                "skipped": skipped,
                "output_dir": out_dir.display().to_string(),
            }))
        })
        .await;

        match outcome {
            Ok(data) => Ok(ToolResult::ok(data)),
            Err(e) => Ok(ToolResult::err("MAIL_ATTACHMENT_FAILED", &e)),
        }
    }
}

fn default_mail_dir() -> std::path::PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .unwrap_or_else(std::env::temp_dir)
        .join("EAssisant-mail")
}

/// 附件名来自外部邮件，**不可信**：路径分隔符、`..`、Windows 保留字符都要处理，
/// 否则一个叫 `..\..\Windows\System32\x.dll` 的附件就能写到目录外面去。
pub fn sanitize_filename(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("attachment")
        .trim();
    let mut cleaned: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\0' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    cleaned = cleaned.trim_matches('.').trim().to_string();
    if cleaned.is_empty() {
        cleaned = "attachment".to_string();
    }
    // Windows 保留设备名
    let stem_upper = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(
        stem_upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "COM2" | "COM3" | "LPT1" | "LPT2" | "LPT3"
    ) {
        cleaned = format!("_{cleaned}");
    }
    cleaned
}

/// 同名不覆盖时加 `-1`、`-2` 后缀，保留扩展名。
fn unique_path(dir: &std::path::Path, name: &str, overwrite: bool) -> std::path::PathBuf {
    let candidate = dir.join(name);
    if overwrite || !candidate.exists() {
        return candidate;
    }
    let path = std::path::Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "attachment".to_string());
    let ext = path
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    for i in 1..1000 {
        let next = dir.join(format!("{stem}-{i}{ext}"));
        if !next.exists() {
            return next;
        }
    }
    dir.join(format!("{stem}-{}{ext}", uuid::Uuid::new_v4()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_path_traversal() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename(r"..\..\Windows\System32\x.dll"), "x.dll");
        assert_eq!(sanitize_filename("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitize_filename("   "), "attachment");
        assert_eq!(sanitize_filename("..."), "attachment");
    }

    #[test]
    fn sanitize_guards_reserved_device_names() {
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_filename("con"), "_con");
        assert_eq!(sanitize_filename("report.pdf"), "report.pdf");
    }

    #[test]
    fn sanitize_keeps_chinese_names() {
        assert_eq!(sanitize_filename("季度报告.pdf"), "季度报告.pdf");
    }

    #[test]
    fn unique_path_appends_counter() {
        let dir = std::env::temp_dir().join(format!("pcg-mail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "a.txt", false);
        assert_eq!(first.file_name().unwrap(), "a.txt");
        std::fs::write(&first, b"x").unwrap();

        let second = unique_path(&dir, "a.txt", false);
        assert_eq!(second.file_name().unwrap(), "a-1.txt");

        let over = unique_path(&dir, "a.txt", true);
        assert_eq!(over.file_name().unwrap(), "a.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
