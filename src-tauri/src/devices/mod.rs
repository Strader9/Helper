//! 多端（设备）网关 —— 让手机/平板/另一台电脑连上同一个助手
//!
//! 三件套：
//! 1. **配对**：桌面端生成一次性 6 位配对码（默认 10 分钟有效），远端拿码换
//!    一个长期令牌。配对码本身就是授权凭据，所以只能由用户主动"要一个"，
//!    不会被任何自动流程打印出来。
//! 2. **令牌**：只存 SHA-256，明文只在配对成功那一次返回给设备。库被读走
//!    不等于设备被接管。
//! 3. **本地 HTTP 服务**：只开 5 个端点，手写报文，不引框架。
//!    默认绑 `127.0.0.1`——手机要连就必须由用户显式把 bind 改成 `0.0.0.0`，
//!    这是刻意的：默认暴露到局域网才是真正危险的设计。
//!
//! 为什么不用 sidecar 那套 Socket.IO：sidecar 是 Rust 的**下游**（Rust 调它），
//! 而设备网关必须能反向驱动 Agent（远端一句话要真的跑起来），放在 Rust 里
//! 才能直接拿到 `AppState`，不必再绕一层进程间协议。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify};

/// 配对码有效期（秒）
pub const PAIRING_CODE_TTL_SECS: i64 = 600;
/// 单次请求体上限，防止一个畸形请求把内存吃满
const MAX_BODY_BYTES: usize = 1_048_576;

// ============================================================
// 令牌
// ============================================================

/// 生成一个 64 位十六进制令牌（两个 uuid 拼起来，够长且无需额外随机源依赖）。
pub fn generate_token() -> String {
    let a = uuid::Uuid::new_v4().simple().to_string();
    let b = uuid::Uuid::new_v4().simple().to_string();
    format!("{}{}", a, b)
}

/// SHA-256 十六进制摘要。库里只存这个值。
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

/// 令牌末 4 位，用于界面上人工分辨"这是哪台设备的令牌"
pub fn token_hint(token: &str) -> String {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() < 8 {
        return String::new();
    }
    chars[chars.len() - 4..].iter().collect()
}

// ============================================================
// 数据模型
// ============================================================

#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: Option<String>,
    pub token_hint: Option<String>,
    pub paired_at: String,
    pub last_seen_at: Option<String>,
    pub revoked: bool,
}

fn row_to_device(row: &rusqlite::Row<'_>) -> rusqlite::Result<Device> {
    Ok(Device {
        id: row.get(0)?,
        name: row.get(1)?,
        platform: row.get(2)?,
        token_hint: row.get(3)?,
        paired_at: row.get(4)?,
        last_seen_at: row.get(5)?,
        revoked: row.get::<_, i64>(6)? != 0,
    })
}

const DEVICE_COLUMNS: &str = "id, name, platform, token_hint, paired_at, last_seen_at, revoked";

// ============================================================
// 数据库操作
// ============================================================

pub fn list_devices(conn: &rusqlite::Connection) -> Result<Vec<Device>, String> {
    let sql = format!(
        "SELECT {} FROM devices ORDER BY revoked ASC, paired_at DESC",
        DEVICE_COLUMNS
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], row_to_device)
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

fn find_device_by_hash(
    conn: &rusqlite::Connection,
    token_hash: &str,
) -> Result<Option<Device>, String> {
    let sql = format!(
        "SELECT {} FROM devices WHERE token_hash = ?1 AND revoked = 0",
        DEVICE_COLUMNS
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut rows = stmt.query([token_hash]).map_err(|e| e.to_string())?;
    match rows.next().map_err(|e| e.to_string())? {
        Some(row) => Ok(Some(row_to_device(row).map_err(|e| e.to_string())?)),
        None => Ok(None),
    }
}

/// 校验令牌并顺手更新 last_seen_at。
pub fn authenticate(conn: &rusqlite::Connection, token: &str) -> Result<Device, String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err("缺少令牌".to_string());
    }

    let hash = hash_token(trimmed);
    let device =
        find_device_by_hash(conn, &hash)?.ok_or_else(|| "令牌无效或设备已撤销".to_string())?;

    let _ = conn.execute(
        "UPDATE devices SET last_seen_at = datetime('now') WHERE id = ?1",
        [&device.id],
    );

    Ok(device)
}

/// 生成一次性配对码。同一时刻可以有多个有效码（用户可能给两台设备配对）。
pub fn issue_pairing_code(
    conn: &rusqlite::Connection,
    ttl_secs: i64,
) -> Result<(String, String), String> {
    let ttl = ttl_secs.clamp(60, 3600);
    // 6 位数字：由 uuid 的低位取模得到，避免额外随机源
    let code = format!("{:06}", uuid::Uuid::new_v4().as_u128() % 1_000_000);

    let now = chrono::Local::now();
    let expires = now + chrono::Duration::seconds(ttl);

    conn.execute(
        "INSERT OR REPLACE INTO device_pairing_codes (code, created_at, expires_at, used) \
         VALUES (?1, ?2, ?3, 0)",
        rusqlite::params![
            code,
            now.format(crate::scheduler::TIME_FMT).to_string(),
            expires.format(crate::scheduler::TIME_FMT).to_string(),
        ],
    )
    .map_err(|e| e.to_string())?;

    Ok((code, expires.format(crate::scheduler::TIME_FMT).to_string()))
}

/// 消费配对码。成功即标记已用，失败说明原因（不存在/已用/过期）。
pub fn consume_pairing_code(conn: &rusqlite::Connection, code: &str) -> Result<(), String> {
    let code = code.trim();
    if code.is_empty() {
        return Err("缺少配对码".to_string());
    }

    let row: Option<(i64, String)> = {
        let mut stmt = conn
            .prepare("SELECT used, expires_at FROM device_pairing_codes WHERE code = ?1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query([code]).map_err(|e| e.to_string())?;
        match rows.next().map_err(|e| e.to_string())? {
            Some(row) => Some((
                row.get::<_, i64>(0).map_err(|e| e.to_string())?,
                row.get::<_, String>(1).map_err(|e| e.to_string())?,
            )),
            None => None,
        }
    };

    let Some((used, expires_at)) = row else {
        return Err("配对码不存在".to_string());
    };
    if used != 0 {
        return Err("配对码已被使用".to_string());
    }

    let expires = chrono::NaiveDateTime::parse_from_str(&expires_at, crate::scheduler::TIME_FMT)
        .map_err(|_| "配对码记录损坏".to_string())?;
    if chrono::Local::now().naive_local() > expires {
        return Err("配对码已过期，请在电脑上重新生成".to_string());
    }

    conn.execute(
        "UPDATE device_pairing_codes SET used = 1 WHERE code = ?1",
        [code],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// 完成配对：建设备记录，返回（设备，明文令牌）。
///
/// 明文令牌只在这一刻出现一次，之后库里只有哈希。
pub fn pair_device(
    conn: &rusqlite::Connection,
    name: &str,
    platform: Option<&str>,
) -> Result<(Device, String), String> {
    let name = name.trim();
    let name = if name.is_empty() {
        "未命名设备"
    } else {
        name
    };

    let token = generate_token();
    let id = uuid::Uuid::new_v4().to_string();

    conn.execute(
        "INSERT INTO devices (id, name, platform, token_hash, token_hint, paired_at, revoked) \
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'), 0)",
        rusqlite::params![
            id,
            name,
            platform.map(str::trim).filter(|p| !p.is_empty()),
            hash_token(&token),
            token_hint(&token),
        ],
    )
    .map_err(|e| e.to_string())?;

    let sql = format!("SELECT {} FROM devices WHERE id = ?1", DEVICE_COLUMNS);
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut rows = stmt.query([&id]).map_err(|e| e.to_string())?;
    let device = match rows.next().map_err(|e| e.to_string())? {
        Some(row) => row_to_device(row).map_err(|e| e.to_string())?,
        None => return Err("设备写入后读不回来".to_string()),
    };

    Ok((device, token))
}

pub fn revoke_device(conn: &rusqlite::Connection, id: &str) -> Result<bool, String> {
    let affected = conn
        .execute("UPDATE devices SET revoked = 1 WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(affected > 0)
}

/// 清掉过期/已用的配对码，避免表无限长
pub fn prune_pairing_codes(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute(
        "DELETE FROM device_pairing_codes WHERE used = 1 OR expires_at < ?1",
        [chrono::Local::now()
            .format(crate::scheduler::TIME_FMT)
            .to_string()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ============================================================
// HTTP 报文：解析与响应
// ============================================================

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(&name.to_lowercase()).map(String::as_str)
    }

    /// 从 `Authorization: Bearer xxx` 里取令牌
    pub fn bearer_token(&self) -> Option<&str> {
        let value = self.header("authorization")?;
        let rest = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))?;
        let token = rest.trim();
        if token.is_empty() {
            None
        } else {
            Some(token)
        }
    }

    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query.get(name).map(String::as_str)
    }

    pub fn body_json(&self) -> Result<Value, String> {
        if self.body.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&self.body).map_err(|e| format!("请求体不是合法 JSON：{}", e))
    }
}

fn parse_query(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.insert(percent_decode(key), percent_decode(value));
    }
    out
}

/// 最小的百分号解码（只处理 %XX 与 '+'），够读 URL 里的中文会话名
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 解析完整报文（头 + 体）。测试直接喂字符串，服务端喂 socket 读到的字节。
pub fn parse_http(raw: &str) -> Result<HttpRequest, String> {
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .ok_or_else(|| "请求头不完整（缺少空行）".to_string())?;

    let mut lines = head.split("\r\n");
    let request_line = lines.next().ok_or_else(|| "缺少请求行".to_string())?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| "缺少方法".to_string())?
        .to_uppercase();
    let target = parts.next().ok_or_else(|| "缺少请求路径".to_string())?;

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), parse_query(q)),
        None => (target.to_string(), HashMap::new()),
    };

    let mut headers = HashMap::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_lowercase(), value.trim().to_string());
        }
    }

    Ok(HttpRequest {
        method,
        path,
        query,
        headers,
        body: body.to_string(),
    })
}

pub fn http_response(status: u16, payload: &Value) -> String {
    let body = payload.to_string();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "OK",
    };
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{}",
        status,
        reason,
        body.as_bytes().len(),
        body
    )
}

/// 从 socket 里读一个完整请求。
///
/// 先读到空行为止拿头，再按 `Content-Length` 补体。上限 1MB，
/// 超了直接 413——本网关没有上传大文件的需求。
async fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];

    loop {
        let read =
            tokio::time::timeout(std::time::Duration::from_secs(15), stream.read(&mut chunk))
                .await
                .map_err(|_| "读取请求超时".to_string())?
                .map_err(|e| format!("读取失败：{}", e))?;

        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);

        if buffer.len() > MAX_BODY_BYTES {
            return Err("请求体过大".to_string());
        }

        if let Some(head_end) = find_head_end(&buffer) {
            let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
            let content_length = head
                .split("\r\n")
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    if key.trim().eq_ignore_ascii_case("content-length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);

            let body_have = buffer.len().saturating_sub(head_end + 4);
            if body_have >= content_length {
                break;
            }
        }
    }

    let raw = String::from_utf8_lossy(&buffer).to_string();
    parse_http(&raw)
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

// ============================================================
// 路由与处理
// ============================================================

/// 一个端点的定位结果：要么是已知路由，要么是"不认识"。
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Health,
    Pair,
    Me,
    AgentMessage,
    AgentMessages,
    SchedulerJobs,
    NotFound,
}

pub fn route_for(method: &str, path: &str) -> Route {
    match (method, path) {
        ("GET", "/v1/health") => Route::Health,
        ("POST", "/v1/device/pair") => Route::Pair,
        ("GET", "/v1/device/me") => Route::Me,
        ("POST", "/v1/agent/message") => Route::AgentMessage,
        ("GET", "/v1/agent/messages") => Route::AgentMessages,
        ("GET", "/v1/scheduler/jobs") => Route::SchedulerJobs,
        _ => Route::NotFound,
    }
}

async fn handle_connection(app_handle: AppHandle, mut stream: TcpStream) {
    let request = match read_request(&mut stream).await {
        Ok(req) => req,
        Err(error) => {
            let body = json!({ "error": error });
            let _ = stream.write_all(http_response(400, &body).as_bytes()).await;
            let _ = stream.shutdown().await;
            return;
        }
    };

    let (status, payload) = dispatch(&app_handle, &request).await;

    let _ = stream
        .write_all(http_response(status, &payload).as_bytes())
        .await;
    let _ = stream.shutdown().await;
}

async fn dispatch(app_handle: &AppHandle, request: &HttpRequest) -> (u16, Value) {
    let route = route_for(&request.method, &request.path);
    if route == Route::NotFound {
        return (
            404,
            json!({ "error": format!("未知端点：{} {}", request.method, request.path) }),
        );
    }

    let Some(state) = app_handle.try_state::<crate::AppState>() else {
        return (500, json!({ "error": "应用状态不可用" }));
    };

    match route {
        Route::Health => {
            let devices = {
                let db = state.db.lock().await;
                list_devices(db.connection()).unwrap_or_default()
            };
            (
                200,
                json!({
                    "ok": true,
                    "app": "pc-guardian",
                    "version": env!("CARGO_PKG_VERSION"),
                    "devices_total": devices.len(),
                    "devices_active": devices.iter().filter(|d| !d.revoked).count(),
                }),
            )
        }
        Route::Pair => {
            let body = match request.body_json() {
                Ok(v) => v,
                Err(e) => return (400, json!({ "error": e })),
            };
            let code = body["code"].as_str().unwrap_or("");
            let name = body["name"].as_str().unwrap_or("");
            let platform = body["platform"].as_str();

            let db = state.db.lock().await;
            if let Err(error) = consume_pairing_code(db.connection(), code) {
                // 配对码错误是安全事件，值得留痕
                eprintln!("[Gateway] 配对失败：{}", error);
                return (401, json!({ "error": error }));
            }
            match pair_device(db.connection(), name, platform) {
                Ok((device, token)) => (
                    200,
                    json!({
                        "device_id": device.id,
                        "name": device.name,
                        "token": token,
                        "note": "令牌只会返回这一次，请保存在设备上",
                    }),
                ),
                Err(error) => (500, json!({ "error": error })),
            }
        }
        Route::Me => {
            let Some(token) = request.bearer_token() else {
                return (401, json!({ "error": "缺少 Authorization: Bearer 令牌" }));
            };
            let db = state.db.lock().await;
            match authenticate(db.connection(), token) {
                Ok(device) => (200, json!({ "device": device })),
                Err(error) => (401, json!({ "error": error })),
            }
        }
        Route::AgentMessage => {
            let Some(token) = request.bearer_token().map(str::to_string) else {
                return (401, json!({ "error": "缺少 Authorization: Bearer 令牌" }));
            };
            let body = match request.body_json() {
                Ok(v) => v,
                Err(e) => return (400, json!({ "error": e })),
            };
            let text = body["text"].as_str().unwrap_or("").trim().to_string();
            if text.is_empty() {
                return (400, json!({ "error": "text 不能为空" }));
            }

            // 1. 先鉴权（不要让陌生请求摸到模型）
            let device = {
                let db = state.db.lock().await;
                match authenticate(db.connection(), &token) {
                    Ok(d) => d,
                    Err(error) => return (401, json!({ "error": error })),
                }
            };

            // 2. 决定落在哪个会话
            let session_id = match body["session_id"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(id) => {
                    let db = state.db.lock().await;
                    crate::scheduler::ensure_session(
                        db.connection(),
                        id,
                        &format!("来自 {}", device.name),
                    )
                    .ok();
                    id.to_string()
                }
                None => {
                    let db = state.db.lock().await;
                    let mgr = crate::db::conversation::ConversationManager::new(db.connection());
                    match mgr.create(Some(format!("来自 {}", device.name))) {
                        Ok(conversation) => conversation.id,
                        Err(error) => {
                            return (500, json!({ "error": format!("建会话失败：{}", error) }))
                        }
                    }
                }
            };

            // 3. 跑一轮 Agent
            let run = {
                let agent = state.agent.lock().await;
                agent
                    .send_message(&session_id, &text, None, None, &state.db)
                    .await
            };

            let agent_run = match run {
                Ok(r) => r,
                Err(error) => return (500, json!({ "error": format!("发起失败：{}", error) })),
            };

            let timeout_secs = {
                let db = state.db.lock().await;
                db.get_setting("agent.gateway_reply_timeout_sec")
                    .ok()
                    .flatten()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(|v| v.clamp(30, 600))
                    .unwrap_or(180)
            };

            let outcome = crate::scheduler::drain_agent_run(agent_run.events, timeout_secs).await;

            let status = if outcome.status == "ok" { 200 } else { 500 };
            (
                status,
                json!({
                    "session_id": session_id,
                    "run_id": agent_run.run_id,
                    "device": device.name,
                    "status": outcome.status,
                    "reply": outcome.summary,
                    "error": outcome.error,
                    "steps": outcome.steps,
                    "tools_used": outcome.tools_used,
                }),
            )
        }
        Route::AgentMessages => {
            let Some(token) = request.bearer_token() else {
                return (401, json!({ "error": "缺少 Authorization: Bearer 令牌" }));
            };
            let db = state.db.lock().await;
            if let Err(error) = authenticate(db.connection(), token) {
                return (401, json!({ "error": error }));
            }

            let session_id = request.query_param("session_id").unwrap_or("").trim();
            if session_id.is_empty() {
                return (400, json!({ "error": "缺少 session_id" }));
            }
            let limit = request
                .query_param("limit")
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(30)
                .clamp(1, 200);

            let mgr = crate::db::conversation::ConversationManager::new(db.connection());
            match mgr.get_messages(session_id) {
                Ok(messages) => {
                    // 只回角色/内容/时间：tool_calls 与内部 id 是桌面端调试用的，
                    // 远端拿到也没用，反而会把实现细节暴露出去。
                    let tail: Vec<Value> = messages
                        .iter()
                        .rev()
                        .take(limit)
                        .rev()
                        .map(|m| {
                            json!({
                                "role": m.role,
                                "content": m.content,
                                "timestamp": m.timestamp,
                            })
                        })
                        .collect();
                    (200, json!({ "session_id": session_id, "messages": tail }))
                }
                Err(error) => (500, json!({ "error": error.to_string() })),
            }
        }
        Route::SchedulerJobs => {
            let Some(token) = request.bearer_token() else {
                return (401, json!({ "error": "缺少 Authorization: Bearer 令牌" }));
            };
            let db = state.db.lock().await;
            if let Err(error) = authenticate(db.connection(), token) {
                return (401, json!({ "error": error }));
            }
            match crate::scheduler::list_jobs(db.connection()) {
                Ok(jobs) => (200, json!({ "count": jobs.len(), "jobs": jobs })),
                Err(error) => (500, json!({ "error": error })),
            }
        }
        Route::NotFound => (404, json!({ "error": "未知端点" })),
    }
}

// ============================================================
// 网关生命周期
// ============================================================

#[derive(Debug, Clone, Serialize)]
pub struct GatewayStatus {
    pub running: bool,
    pub bound: Option<String>,
    pub last_error: Option<String>,
    pub requests: u64,
}

pub struct DeviceGateway {
    app_handle: AppHandle,
    running: Arc<AtomicBool>,
    bound: Arc<Mutex<Option<String>>>,
    last_error: Arc<Mutex<Option<String>>>,
    requests: Arc<std::sync::atomic::AtomicU64>,
    generation: Arc<AtomicU64>,
    wake: Arc<Notify>,
}

impl DeviceGateway {
    pub fn new(app_handle: AppHandle) -> Self {
        Self {
            app_handle,
            running: Arc::new(AtomicBool::new(false)),
            bound: Arc::new(Mutex::new(None)),
            last_error: Arc::new(Mutex::new(None)),
            requests: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            generation: Arc::new(AtomicU64::new(0)),
            wake: Arc::new(Notify::new()),
        }
    }

    pub fn start(&self) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }

        let generation_id = self.generation.fetch_add(1, Ordering::SeqCst) + 1;

        let app_handle = self.app_handle.clone();
        let running = self.running.clone();
        let bound = self.bound.clone();
        let last_error = self.last_error.clone();
        let requests = self.requests.clone();
        let generation = self.generation.clone();
        let wake = self.wake.clone();

        tauri::async_runtime::spawn(async move {
            *last_error.lock().await = None;
            *bound.lock().await = None;
            let (enabled, bind, port) = {
                let state = app_handle.state::<crate::AppState>();
                let db = state.db.lock().await;
                let enabled = db
                    .get_setting("agent.gateway_enabled")
                    .ok()
                    .flatten()
                    .map(|v| !matches!(v.trim().to_lowercase().as_str(), "false" | "0" | "off"))
                    .unwrap_or(true);
                let bind = db
                    .get_setting("agent.gateway_bind")
                    .ok()
                    .flatten()
                    .filter(|v| !v.trim().is_empty())
                    .unwrap_or_else(|| "127.0.0.1".to_string());
                let port = db
                    .get_setting("agent.gateway_port")
                    .ok()
                    .flatten()
                    .and_then(|v| v.trim().parse::<u16>().ok())
                    .unwrap_or(4769);
                (enabled, bind, port)
            };

            if !enabled {
                eprintln!("[Gateway] 已禁用（agent.gateway_enabled=false）");
                if generation.load(Ordering::SeqCst) == generation_id {
                    running.store(false, Ordering::SeqCst);
                }
                return;
            }

            let address = format!("{}:{}", bind.trim(), port);
            let listener = match TcpListener::bind(&address).await {
                Ok(l) => l,
                Err(error) => {
                    eprintln!("[Gateway] 绑定 {} 失败: {}", address, error);
                    *last_error.lock().await = Some(format!("绑定 {} 失败：{}", address, error));
                    if generation.load(Ordering::SeqCst) == generation_id {
                        running.store(false, Ordering::SeqCst);
                    }
                    return;
                }
            };

            if generation.load(Ordering::SeqCst) != generation_id {
                return;
            }
            *bound.lock().await = Some(address.clone());
            eprintln!("[Gateway] listening on http://{}", address);

            while running.load(Ordering::SeqCst)
                && generation.load(Ordering::SeqCst) == generation_id
            {
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _peer)) => {
                            requests.fetch_add(1, Ordering::Relaxed);
                            let app = app_handle.clone();
                            tauri::async_runtime::spawn(async move {
                                handle_connection(app, stream).await;
                            });
                        }
                        Err(error) => {
                            eprintln!("[Gateway] accept 失败: {}", error);
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        }
                    },
                    _ = wake.notified() => {
                        if !running.load(Ordering::SeqCst)
                            || generation.load(Ordering::SeqCst) != generation_id
                        {
                            break;
                        }
                    }
                }
            }

            if generation.load(Ordering::SeqCst) == generation_id {
                running.store(false, Ordering::SeqCst);
                *bound.lock().await = None;
            }
            eprintln!("[Gateway] stopped");
        });
    }

    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
        self.wake.notify_waiters();
    }

    /// 重新读取数据库配置并重启监听器。generation 可防止旧监听任务在重启竞态中复活。
    pub async fn restart(&self) {
        self.stop();
        tokio::task::yield_now().await;
        self.start();
    }

    pub async fn status(&self) -> GatewayStatus {
        GatewayStatus {
            running: self.running.load(Ordering::SeqCst),
            bound: self.bound.lock().await.clone(),
            last_error: self.last_error.lock().await.clone(),
            requests: self.requests.load(Ordering::Relaxed),
        }
    }
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_db(tag: &str) -> (crate::db::Database, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("pcg-gateway-test-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let db = crate::db::Database::new(&dir.join("t.db")).expect("打开数据库");
        db.init_schema().expect("初始化 schema");
        (db, dir)
    }

    #[test]
    fn token_hash_is_stable_and_never_equals_plaintext() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        assert_eq!(hash_token(&token), hash_token(&token));
        assert_ne!(hash_token(&token), token, "库里不能存明文");
        assert_eq!(token_hint(&token).len(), 4);
    }

    #[test]
    fn pairing_code_round_trip_happy_path() {
        let (db, dir) = fresh_db("pair");
        let conn = db.connection();

        let (code, expires) = issue_pairing_code(conn, 600).expect("生成配对码");
        assert_eq!(code.len(), 6);
        assert!(
            expires
                > chrono::Local::now()
                    .format(crate::scheduler::TIME_FMT)
                    .to_string()
        );

        consume_pairing_code(conn, &code).expect("首次消费应当成功");
        // 一次性：第二次必须失败，否则码被截获就能反复配对
        let second = consume_pairing_code(conn, &code);
        assert!(second.is_err(), "配对码只能用一次：{:?}", second);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_and_expired_codes_are_rejected() {
        let (db, dir) = fresh_db("badcode");
        let conn = db.connection();

        assert!(consume_pairing_code(conn, "000000").is_err());

        let (code, _) = issue_pairing_code(conn, 60).expect("生成配对码");
        // 手工把它改成已过期
        conn.execute(
            "UPDATE device_pairing_codes SET expires_at = ?2 WHERE code = ?1",
            rusqlite::params![code, "2000-01-01 00:00:00"],
        )
        .expect("改过期时间");
        let result = consume_pairing_code(conn, &code);
        assert!(result.is_err(), "过期码不该能换到令牌");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn paired_device_authenticates_and_revoke_blocks_it() {
        let (db, dir) = fresh_db("auth");
        let conn = db.connection();

        let (device, token) = pair_device(conn, "我的手机", Some("android")).expect("配对");
        assert_eq!(device.name, "我的手机");
        assert_eq!(
            device.token_hint.as_deref(),
            Some(token_hint(&token).as_str())
        );

        let authed = authenticate(conn, &token).expect("有效令牌应通过");
        assert_eq!(authed.id, device.id);

        assert!(authenticate(conn, "garbage").is_err(), "乱写的令牌必须拒绝");

        assert!(revoke_device(conn, &device.id).expect("撤销"));
        assert!(
            authenticate(conn, &token).is_err(),
            "撤销后旧令牌必须立即失效"
        );

        let devices = list_devices(conn).expect("列表");
        assert_eq!(devices.len(), 1);
        assert!(devices[0].revoked);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn device_name_falls_back_when_blank() {
        let (db, dir) = fresh_db("noname");
        let conn = db.connection();
        let (device, _) = pair_device(conn, "   ", None).expect("配对");
        assert_eq!(device.name, "未命名设备");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn http_request_parses_method_path_query_and_bearer() {
        let raw = "GET /v1/agent/messages?session_id=abc&limit=5 HTTP/1.1\r\n\
                   Host: 127.0.0.1:4769\r\n\
                   Authorization: Bearer tok123\r\n\
                   \r\n";
        let req = parse_http(raw).expect("解析");
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/v1/agent/messages");
        assert_eq!(req.query_param("session_id"), Some("abc"));
        assert_eq!(req.query_param("limit"), Some("5"));
        assert_eq!(req.bearer_token(), Some("tok123"));
    }

    #[test]
    fn http_request_parses_json_body() {
        let raw = "POST /v1/agent/message HTTP/1.1\r\n\
                   Content-Type: application/json\r\n\
                   \r\n\
                   {\"text\":\"你好\",\"session_id\":\"s1\"}";
        let req = parse_http(raw).expect("解析");
        let body = req.body_json().expect("JSON");
        assert_eq!(body["text"], "你好");
        assert_eq!(body["session_id"], "s1");
    }

    #[test]
    fn http_request_without_header_terminator_is_rejected() {
        assert!(parse_http("GET /v1/health HTTP/1.1\r\nHost: x").is_err());
    }

    #[test]
    fn empty_body_decodes_as_empty_object() {
        let raw = "POST /v1/device/pair HTTP/1.1\r\n\r\n";
        let req = parse_http(raw).expect("解析");
        assert_eq!(req.body_json().expect("JSON"), json!({}));
    }

    #[test]
    fn route_table_maps_known_endpoints() {
        assert_eq!(route_for("GET", "/v1/health"), Route::Health);
        assert_eq!(route_for("POST", "/v1/device/pair"), Route::Pair);
        assert_eq!(route_for("GET", "/v1/device/me"), Route::Me);
        assert_eq!(route_for("POST", "/v1/agent/message"), Route::AgentMessage);
        assert_eq!(route_for("GET", "/v1/agent/messages"), Route::AgentMessages);
        assert_eq!(route_for("GET", "/v1/scheduler/jobs"), Route::SchedulerJobs);
        // 方法不匹配也算不认识，避免 GET 打到会改状态的端点
        assert_eq!(route_for("GET", "/v1/agent/message"), Route::NotFound);
        assert_eq!(route_for("POST", "/v1/health"), Route::NotFound);
    }

    #[test]
    fn response_has_correct_content_length() {
        let response = http_response(200, &json!({"ok": true}));
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        // {"ok":true} 是 11 字节
        assert!(response.contains("Content-Length: 11\r\n"), "{}", response);
        assert!(response.ends_with("\r\n\r\n{\"ok\":true}"));
    }

    #[test]
    fn query_values_are_percent_decoded() {
        let parsed = parse_query("q=%E4%BD%A0%E5%A5%BD&name=a+b");
        assert_eq!(parsed.get("q").map(String::as_str), Some("你好"));
        assert_eq!(parsed.get("name").map(String::as_str), Some("a b"));
    }

    #[test]
    fn head_end_is_found_across_chunk_boundaries() {
        let buffer = b"GET / HTTP/1.1\r\nHost: x\r\n\r\nbody";
        assert_eq!(find_head_end(buffer), Some(23));
        assert_eq!(find_head_end(b"partial\r\n"), None);
    }

    /// 真开一个 socket，把请求拆成两段发过去，验证"读到 Content-Length 满足
    /// 才停"的逻辑——TCP 拆包是常态，一口气 read 一次就 parse 的写法会随机失败。
    #[tokio::test]
    async fn read_request_waits_for_the_full_body() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("监听");
        let address = listener.local_addr().expect("地址");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("接受连接");
            let request = read_request(&mut stream).await.expect("读请求");
            assert_eq!(request.path, "/v1/agent/message");
            assert_eq!(request.method, "POST");
            assert_eq!(request.body, "{\"text\":\"hi\"}");
            let response = http_response(200, &json!({ "ok": true }));
            stream.write_all(response.as_bytes()).await.expect("写响应");
            let _ = stream.shutdown().await;
        });

        let mut client = TcpStream::connect(address).await.expect("连接");
        // 第一段只给头（含 Content-Length: 13）
        client
            .write_all(b"POST /v1/agent/message HTTP/1.1\r\nContent-Length: 13\r\n")
            .await
            .expect("写第一段");
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        // 第二段补空行体
        client
            .write_all(b"\r\n{\"text\":\"hi\"}")
            .await
            .expect("写第二段");

        let mut response = String::new();
        client.read_to_string(&mut response).await.expect("读响应");

        assert!(response.starts_with("HTTP/1.1 200 OK"), "{}", response);
        assert!(response.contains("\"ok\":true"));
        server.await.expect("服务端任务");
    }
}
