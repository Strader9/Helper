//! 消息类工具（messaging 域）
//!
//! 邮件是目前唯一能真正"对外送达"的通道：IM 类需要平台侧授权与绑定，
//! 桌面端硬控微信窗口既不稳也越界，所以这一域只做邮件，加一条
//! "把内容交给本机邮件客户端"的兜底路径。
//!
//! 凭据取自 `settings` 表（`mail.*` 键），**不落日志、不进返回值**。
//! 没配就是把配置项名字报回去，让用户自己填，而不是偷偷退回某个默认服务器。

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

// ============================================================
// 配置读取
// ============================================================

/// 读一条 settings；读不到就是 None，不抛错（缺配置是常态，不是故障）。
fn setting(key: &str) -> Option<String> {
    let conn = crate::memory::open_connection()?;
    let value: Option<String> = conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .ok();
    value.filter(|v| !v.trim().is_empty())
}

/// 邮件配置快照。`password` 只用于发信，绝不出现在返回值里。
struct MailConfig {
    host: String,
    port: u16,
    security: String,
    user: String,
    password: String,
    from: String,
    from_name: Option<String>,
}

fn load_config() -> Result<MailConfig, Vec<&'static str>> {
    let host = setting("mail.smtp_host");
    let user = setting("mail.smtp_user");
    let password = setting("mail.smtp_password");
    let from = setting("mail.from");

    let mut missing = Vec::new();
    if host.is_none() {
        missing.push("mail.smtp_host");
    }
    if user.is_none() {
        missing.push("mail.smtp_user");
    }
    if password.is_none() {
        missing.push("mail.smtp_password");
    }
    if from.is_none() {
        missing.push("mail.from");
    }
    if !missing.is_empty() {
        return Err(missing);
    }

    let port = setting("mail.smtp_port")
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(587);
    let security = setting("mail.smtp_security")
        .map(|v| v.trim().to_lowercase())
        .unwrap_or_else(|| if port == 465 { "tls".into() } else { "starttls".into() });

    Ok(MailConfig {
        host: host.unwrap(),
        port,
        security,
        user: user.unwrap(),
        password: password.unwrap(),
        from: from.unwrap(),
        from_name: setting("mail.from_name"),
    })
}

/// 缺配置时的统一回话：给出键名与说明，不猜服务器。
fn missing_config_result(missing: &[&str]) -> ToolResult {
    ToolResult::err(
        "MAIL_NOT_CONFIGURED",
        &format!(
            "邮件未配置，缺少：{}。请在设置里补上这些项（mail.smtp_host / mail.smtp_port / mail.smtp_security / mail.smtp_user / mail.smtp_password / mail.from），不要用猜测的服务器发信。",
            missing.join(" / ")
        ),
    )
}

/// 把收件人参数统一成列表。
///
/// 支持字符串（`a@x.com, b@y.com`）和数组两种写法——模型两种都会写，
/// 只认一种就会莫名其妙报参数错误。
fn parse_recipients(raw: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match raw {
        Value::String(s) => {
            for part in s.split([',', ';', ' ', '\n']) {
                let part = part.trim();
                if !part.is_empty() {
                    out.push(part.to_string());
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    let s = s.trim();
                    if !s.is_empty() {
                        out.push(s.to_string());
                    }
                }
            }
        }
        _ => {}
    }
    out
}

// ============================================================
// send_email — 真发一封信
// ============================================================

pub struct SendEmailTool;

#[async_trait]
impl AgentTool for SendEmailTool {
    fn name(&self) -> &'static str {
        "send_email"
    }

    fn description(&self) -> &'static str {
        "通过已配置的 SMTP 账号发送一封邮件，可带附件。这是对外发送、发出后无法撤回的动作：只有在用户明确要求「发邮件给某某」时才用，不要替用户决定收件人。发送成功仅表示邮件服务器已接受（250），不代表对方已读。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to": {
                    "type": "string",
                    "description": "收件人，多个用逗号分隔"
                },
                "subject": { "type": "string", "description": "主题" },
                "body": { "type": "string", "description": "正文（纯文本）" },
                "cc": { "type": "string", "description": "抄送（可选，逗号分隔）" },
                "attachments": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "附件绝对路径列表（可选，单个不超过 20MB）"
                }
            },
            "required": ["to", "subject", "body"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // 对外发送、不可撤回，按 High 处理：必须走确认
        RiskLevel::High
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let to = parse_recipients(&args["to"]);
        if to.is_empty() {
            return Ok(ToolResult::err("INVALID_PARAMS", "至少要给一个收件人"));
        }
        let subject = args["subject"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("subject is required".to_string()))?;
        let body = args["body"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("body is required".to_string()))?;
        let cc = parse_recipients(&args["cc"]);

        let config = match load_config() {
            Ok(config) => config,
            Err(missing) => return Ok(missing_config_result(&missing)),
        };

        // 附件先在这里读进来：读不到就别开 SMTP 会话，
        // 免得连上服务器、握手完了才发现本地文件不存在。
        let mut attachments: Vec<(String, Vec<u8>)> = Vec::new();
        if let Some(items) = args["attachments"].as_array() {
            for item in items {
                let Some(path) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
                    continue;
                };
                let path = std::path::Path::new(path);
                if !path.is_file() {
                    return Ok(ToolResult::err(
                        "ATTACHMENT_NOT_FOUND",
                        &format!("附件不存在：{}", path.display()),
                    ));
                }
                let meta = std::fs::metadata(path)?;
                const LIMIT: u64 = 20 * 1024 * 1024;
                if meta.len() > LIMIT {
                    return Ok(ToolResult::err(
                        "ATTACHMENT_TOO_LARGE",
                        &format!(
                            "附件 {} 有 {} MB，超过 20MB 上限",
                            path.display(),
                            meta.len() / 1024 / 1024
                        ),
                    ));
                }
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "attachment".to_string());
                let bytes = std::fs::read(path)?;
                attachments.push((name, bytes));
            }
        }

        let attachment_count = attachments.len();
        let to_list = to.clone();
        let cc_list = cc.clone();
        let subject_owned = subject.to_string();
        let body_owned = body.to_string();
        let config_ref = MailConfig {
            host: config.host.clone(),
            port: config.port,
            security: config.security.clone(),
            user: config.user.clone(),
            password: config.password.clone(),
            from: config.from.clone(),
            from_name: config.from_name.clone(),
        };

        // lettre 的同步 Transport 会阻塞线程，放进 spawn_blocking，
        // 否则 SMTP 握手期间整个 runtime 的定时器都会卡住。
        let send_result = tokio::task::spawn_blocking(move || {
            send_via_smtp(
                &config_ref,
                &to_list,
                &cc_list,
                &subject_owned,
                &body_owned,
                &attachments,
            )
        })
        .await
        .map_err(|e| AppError::Internal(format!("发送任务异常结束：{}", e)))?;

        match send_result {
            Ok(response) => Ok(ToolResult::ok(json!({
                "sent": true,
                "to": to,
                "cc": cc,
                "subject": subject,
                "attachment_count": attachment_count,
                "smtp_code": response.0,
                "smtp_response": response.1,
                "note": "服务器已接受该邮件；不代表收件人已读",
            }))),
            Err(message) => Ok(ToolResult::err("SMTP_SEND_FAILED", &message)),
        }
    }
}

/// 真正发信。返回 (SMTP 响应码, 响应文本)。
fn send_via_smtp(
    config: &MailConfig,
    to: &[String],
    cc: &[String],
    subject: &str,
    body: &str,
    attachments: &[(String, Vec<u8>)],
) -> Result<(u16, String), String> {
    use lettre::message::{header::ContentType, Attachment, MultiPart, SinglePart};
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{Message, SmtpTransport, Transport};

    let from_address = config
        .from
        .parse::<lettre::Address>()
        .map_err(|e| format!("发件地址 {} 不合法：{}", config.from, e))?;

    let display_name = config
        .from_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    let from_mailbox = lettre::message::Mailbox::new(display_name, from_address);

    let mut builder = Message::builder()
        .from(from_mailbox)
        .subject(subject.to_string());

    for addr in to {
        let mailbox = addr
            .parse()
            .map_err(|e| format!("收件人 {} 不合法：{}", addr, e))?;
        builder = builder.to(mailbox);
    }
    for addr in cc {
        let mailbox = addr
            .parse()
            .map_err(|e| format!("抄送 {} 不合法：{}", addr, e))?;
        builder = builder.cc(mailbox);
    }

    let message = if attachments.is_empty() {
        builder
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_string())
            .map_err(|e| format!("构造邮件失败：{}", e))?
    } else {
        let mut multipart = MultiPart::mixed().singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(body.to_string()),
        );
        for (name, bytes) in attachments {
            multipart =
                multipart.singlepart(Attachment::new(name.clone()).body(bytes.clone(), "application/octet-stream".parse().unwrap()));
        }
        builder
            .multipart(multipart)
            .map_err(|e| format!("构造邮件失败：{}", e))?
    };

    let credentials = Credentials::new(config.user.clone(), config.password.clone());

    let mailer = match config.security.as_str() {
        // 465：连上就是 TLS
        "tls" | "ssl" => SmtpTransport::relay(&config.host)
            .map_err(|e| format!("SMTP 配置不合法：{}", e))?
            .port(config.port)
            .credentials(credentials)
            .build(),
        // 587/25：明文连上再 STARTTLS 升级
        "starttls" => SmtpTransport::starttls_relay(&config.host)
            .map_err(|e| format!("SMTP 配置不合法：{}", e))?
            .port(config.port)
            .credentials(credentials)
            .build(),
        "none" => {
            // 明文发信会把凭据暴露在链路上，这里允许但明确告知调用方。
            eprintln!("[Mail] 警告：mail.smtp_security=none，凭据将以明文发送");
            SmtpTransport::builder_dangerous(&config.host)
                .port(config.port)
                .credentials(credentials)
                .build()
        }
        other => {
            return Err(format!(
                "不认识的 mail.smtp_security={}，只接受 tls / starttls / none",
                other
            ))
        }
    };

    let response = mailer
        .send(&message)
        .map_err(|e| format!("SMTP 发送失败：{}", e))?;

    let code: u16 = response.code().into();
    let text: String = response.message().collect::<Vec<_>>().join(" ");
    if !response.is_positive() {
        return Err(format!("SMTP 返回 {} {}", code, text));
    }
    Ok((code, text))
}

// ============================================================
// compose_mail — 交给本机邮件客户端
// ============================================================

pub struct ComposeMailTool;

#[async_trait]
impl AgentTool for ComposeMailTool {
    fn name(&self) -> &'static str {
        "compose_mail"
    }

    fn description(&self) -> &'static str {
        "在默认邮件客户端里打开一封已填好收件人/主题/正文的草稿，由用户自己点发送。适用场景：没配 SMTP 账号，或用户想先看一眼再发。这个工具不会真的发出邮件。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "收件人（可为空）" },
                "subject": { "type": "string", "description": "主题" },
                "body": { "type": "string", "description": "正文" }
            },
            "required": ["subject"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // 只是打开客户端，发不发由用户决定
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let to = args["to"].as_str().unwrap_or("").trim();
        let subject = args["subject"].as_str().unwrap_or("").trim();
        let body = args["body"].as_str().unwrap_or("");

        let mut url = String::from("mailto:");
        // mailto 的参数分隔符是 '?' 和 '&'，收件人里若有这些字符必须编码，
        // 否则会被解释成额外参数（曾经出现过标题被当成收件人的情况）。
        if !to.is_empty() {
            url.push_str(&urlencoding::encode(to));
        }
        let mut params = Vec::new();
        if !subject.is_empty() {
            params.push(format!("subject={}", urlencoding::encode(subject)));
        }
        if !body.is_empty() {
            params.push(format!("body={}", urlencoding::encode(body)));
        }
        if !params.is_empty() {
            url.push('?');
            url.push_str(&params.join("&"));
        }

        let mut command = std::process::Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", &url]);

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        match command.spawn() {
            Ok(_) => Ok(ToolResult::ok(json!({
                "opened": true,
                "to": to,
                "subject": subject,
                "note": "已在默认邮件客户端打开草稿，需要用户自己点发送",
            }))),
            Err(e) => Ok(ToolResult::err(
                "CLIENT_LAUNCH_FAILED",
                &format!("打不开默认邮件客户端：{}", e),
            )),
        }
    }
}

// ============================================================
// check_mail_config — 配置体检
// ============================================================

pub struct CheckMailConfigTool;

#[async_trait]
impl AgentTool for CheckMailConfigTool {
    fn name(&self) -> &'static str {
        "check_mail_config"
    }

    fn description(&self) -> &'static str {
        "检查邮件配置是否齐全、SMTP 端口能否连通。回答「能不能发邮件」「为什么发不出去」时先调这个。只做连通性探测，不发送任何邮件、不回显密码。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: Value) -> AppResult<ToolResult> {
        let config = match load_config() {
            Ok(config) => config,
            Err(missing) => {
                return Ok(ToolResult::ok(json!({
                    "configured": false,
                    "missing": missing,
                    "hint": "在设置里补齐 mail.* 配置项后才能发信",
                })))
            }
        };

        // 只测 TCP 是否可达：这一步能区分"地址写错了/被墙了"和
        // "账号密码不对"，两者排查方向完全不同。
        let address = format!("{}:{}", config.host, config.port);
        let reachable = {
            use std::net::{TcpStream, ToSocketAddrs};
            use std::time::Duration;
            match address.to_socket_addrs() {
                Ok(mut addrs) => match addrs.next() {
                    Some(addr) => TcpStream::connect_timeout(&addr, Duration::from_secs(4)).is_ok(),
                    None => false,
                },
                Err(_) => false,
            }
        };

        Ok(ToolResult::ok(json!({
            "configured": true,
            "host": config.host,
            "port": config.port,
            "security": config.security,
            "user": config.user,
            "from": config.from,
            "has_from_name": config.from_name.is_some(),
            "tcp_reachable": reachable,
            "note": if reachable {
                "端口可达；账号密码是否正确只有发一次才知道"
            } else {
                "端口连不上：检查地址/端口/网络，或换 security 类型"
            },
        })))
    }
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipients_accept_string_and_array() {
        assert_eq!(
            parse_recipients(&json!("a@x.com, b@y.com;c@z.com")),
            vec!["a@x.com", "b@y.com", "c@z.com"]
        );
        assert_eq!(
            parse_recipients(&json!(["a@x.com", " ", "b@y.com"])),
            vec!["a@x.com", "b@y.com"]
        );
        assert!(parse_recipients(&json!(null)).is_empty());
    }

    #[test]
    fn missing_config_lists_every_key_not_just_the_first() {
        // 缺配置时一次把缺的全列出来：让用户补三轮会让人直接放弃
        let result = missing_config_result(&["mail.smtp_host", "mail.from"]);
        assert!(!result.success);
        let message = result.error.unwrap();
        assert!(message.contains("mail.smtp_host"));
        assert!(message.contains("mail.from"));
        assert!(message.contains("未配置"));
    }
}
