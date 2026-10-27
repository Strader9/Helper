//! 视频生成（异步任务链路：提交 → 轮询 → 下载 → 核对）。
//!
//! 视频不可能像出图那样一次请求拿结果——服务商侧要排队、渲染几分钟。
//! 所以这里必须有自己的任务生命周期：
//!
//! 1. `POST {base}/videos` 提交，拿远端任务 id
//! 2. 写入 `media_generations` 台账（跨进程重启后还能接着查）
//! 3. `GET {base}/videos/{id}` 轮询状态
//! 4. `completed` 后 `GET {base}/videos/{id}/content` 取字节并落盘
//! 5. 用 ffprobe（装了才有）核对真实时长/分辨率，不一致如实报告
//!
//! 提交时先试 multipart（OpenAI 官方 `/v1/videos` 的形状），被 400/415/422
//! 拒掉再退一次 JSON——大量兼容中转只收 JSON。两次都失败才报错，
//! 并且把两次的响应体都带回去，不猜"大概是什么问题"。

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::media_provider::{
    endpoint, find_by_task_id, load_video_config, mask, missing_result, record_generation,
    truncate, update_generation, VideoConfig,
};
use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

/// 单个视频文件的体积上限。4K/60s 的片子可能到几百 MB，
/// 但一个"助手生成视频"的场景不该把磁盘吃掉一整块，给 512 MB 封顶。
const MAX_VIDEO_BYTES: usize = 512 * 1024 * 1024;
/// 轮询间隔：服务商侧排队时间通常是分钟级，5 秒一次既不浪费请求也够及时。
const POLL_INTERVAL_SEC: u64 = 5;

fn default_video_dir() -> PathBuf {
    dirs::video_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Videos")))
        .unwrap_or_else(std::env::temp_dir)
        .join("EAssisant")
}

fn resolve_dir(requested: Option<&str>) -> AppResult<PathBuf> {
    let default = default_video_dir();
    let dir = match requested.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => {
            let path = PathBuf::from(raw);
            if path.is_absolute() {
                path
            } else {
                default.join(path)
            }
        }
        None => default,
    };
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::Internal(format!("创建输出目录失败 {}: {e}", dir.display())))?;
    Ok(dir)
}

fn build_client(timeout_secs: u64) -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs.max(30)))
        .user_agent("e-assisant/0.1")
        .build()
        .map_err(|e| AppError::Internal(format!("HTTP 客户端构造失败: {e}")))
}

/// 远端状态归一到少数几个词，避免把各家五花八门的 status 直接透传给上层。
fn normalize_status(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "completed" | "succeeded" | "success" | "done" | "finished" => "completed".to_string(),
        "failed" | "error" | "cancelled" | "canceled" | "expired" => "failed".to_string(),
        "queued" | "pending" | "submitted" => "queued".to_string(),
        "in_progress" | "processing" | "running" | "generating" => "in_progress".to_string(),
        other => other.to_string(),
    }
}

/// 从任意形态的响应里挖出任务 id 与状态。
///
/// 兼容 `{id,status}`、`{data:{id,status}}`、`{task_id:...}` 三种常见包装——
/// 它们之间的差别只是外壳，语义完全一致。
fn extract_task(raw: &Value) -> Option<(String, String)> {
    let candidates = [
        raw,
        raw.get("data").unwrap_or(&Value::Null),
        raw.get("result").unwrap_or(&Value::Null),
    ];
    for node in candidates {
        let id = ["id", "task_id", "video_id", "request_id"]
            .iter()
            .find_map(|k| node.get(*k).and_then(|v| v.as_str()))
            .map(|s| s.trim().to_string());
        if let Some(id) = id.filter(|s| !s.is_empty()) {
            let status = node
                .get("status")
                .or_else(|| node.get("state"))
                .and_then(|v| v.as_str())
                .map(normalize_status)
                .unwrap_or_else(|| "queued".to_string());
            return Some((id, status));
        }
    }
    None
}

/// 找可以直连下载的视频地址（部分服务商在状态里直接给 URL）。
fn extract_download_url(raw: &Value) -> Option<String> {
    let candidates = [
        raw,
        raw.get("data").unwrap_or(&Value::Null),
        raw.get("result").unwrap_or(&Value::Null),
        raw.get("output").unwrap_or(&Value::Null),
        // 火山方舟把成品地址放在 content.video_url
        raw.get("content").unwrap_or(&Value::Null),
    ];
    for node in candidates {
        for key in ["url", "video_url", "download_url"] {
            if let Some(url) = node.get(key).and_then(|v| v.as_str()) {
                let url = url.trim();
                if url.starts_with("http://") || url.starts_with("https://") {
                    return Some(url.to_string());
                }
            }
        }
    }
    None
}

fn extract_error_message(raw: &Value) -> Option<String> {
    raw.get("error")
        .and_then(|e| {
            e.get("message")
                .and_then(|m| m.as_str())
                .or_else(|| e.as_str())
        })
        .or_else(|| raw.get("message").and_then(|m| m.as_str()))
        .map(|s| s.to_string())
}

/// 火山方舟（Ark）系列走自有任务协议，不能用 OpenAI 的 `/videos`：
/// 创建 `POST /contents/generations/tasks`（prompt 包在 `content` 数组里），
/// 查询 `GET /contents/generations/tasks/{id}`，成品地址在 `content.video_url`。
///
/// 判定依据用域名而非配置开关：用户把 base_url 换到别的兼容服务时，
/// 不该因为忘改一个开关就让整条链路失灵。
fn is_ark(base_url: &str) -> bool {
    base_url.to_ascii_lowercase().contains("volces.com")
}

/// 把 `1280x720` 这类像素尺寸翻成 Ark 要的 (resolution, ratio)。
///
/// Ark 只认 480p/720p/1080p 与固定宽高比，直接丢像素串会 400，
/// 所以必须换算；认不出的尺寸退回 720p 横屏，交给服务端再校验。
fn ark_resolution_ratio(size: &str) -> (String, String) {
    let parsed = size
        .split_once(|c: char| c == 'x' || c == 'X' || c == '*')
        .and_then(|(w, h)| Some((w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?)))
        .filter(|(w, h)| *w > 0 && *h > 0);

    let (w, h) = match parsed {
        Some(v) => v,
        None => return ("720p".to_string(), "16:9".to_string()),
    };

    let resolution = match w.max(h) {
        n if n >= 1080 => "1080p",
        n if n >= 720 => "720p",
        _ => "480p",
    };

    // 用交叉相乘判比例，避免浮点误差。
    let ratio = if w == h {
        "1:1"
    } else if w * 9 == h * 16 {
        "16:9"
    } else if w * 16 == h * 9 {
        "9:16"
    } else if w * 3 == h * 4 {
        "4:3"
    } else if w * 4 == h * 3 {
        "3:4"
    } else if w * 9 == h * 21 {
        "21:9"
    } else if (w as f64 / h as f64) > 1.0 {
        "16:9"
    } else {
        "9:16"
    };

    (resolution.to_string(), ratio.to_string())
}

/// Ark 提交：`content` 数组 + 顶层 resolution/ratio/duration。
async fn submit_task_ark(
    client: &reqwest::Client,
    cfg: &VideoConfig,
    model: &str,
    prompt: &str,
    size: &str,
    duration: u64,
) -> Result<(String, String, Value), String> {
    let url = endpoint(&cfg.base_url, "contents/generations/tasks");
    let (resolution, ratio) = ark_resolution_ratio(size);
    let body = json!({
        "model": model,
        "content": [{ "type": "text", "text": prompt }],
        "resolution": resolution,
        "ratio": ratio,
        "duration": duration,
        "watermark": false,
    });

    let resp = client
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("提交视频任务失败：{e}"))?;

    let code = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !code.is_success() {
        return Err(format!(
            "提交视频任务返回 HTTP {}：{}",
            code.as_u16(),
            truncate(&text, 300)
        ));
    }

    let parsed: Value = serde_json::from_str(&text)
        .map_err(|e| format!("提交响应不是 JSON（{e}）：{}", truncate(&text, 300)))?;

    match extract_task(&parsed) {
        Some((id, status)) => Ok((id, status, parsed)),
        None => Err(format!(
            "提交成功但响应里没有任务 id：{}",
            truncate(&text, 300)
        )),
    }
}

/// 提交任务。返回 (task_id, 归一状态, 原始响应)。
async fn submit_task(
    client: &reqwest::Client,
    cfg: &VideoConfig,
    model: &str,
    prompt: &str,
    size: &str,
    duration: u64,
) -> Result<(String, String, Value), String> {
    // 火山方舟协议与 OpenAI 不兼容，先分流，后面的两次提交尝试只服务 OpenAI 形状。
    if is_ark(&cfg.base_url) {
        return submit_task_ark(client, cfg, model, prompt, size, duration).await;
    }

    let url = endpoint(&cfg.base_url, "videos");

    // 第一枪：multipart（OpenAI 官方形状）
    let form = reqwest::multipart::Form::new()
        .text("model", model.to_string())
        .text("prompt", prompt.to_string())
        .text("size", size.to_string())
        .text("seconds", duration.to_string());

    let multipart_resp = client
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .multipart(form)
        .send()
        .await;

    let multipart_outcome = match multipart_resp {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            Ok((status.as_u16(), body))
        }
        Err(e) => Err(e.to_string()),
    };

    let first_error: String = match multipart_outcome {
        Ok((code, body)) if (200..300).contains(&code) => {
            let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            if let Some((id, status)) = extract_task(&parsed) {
                return Ok((id, status, parsed));
            }
            format!("提交成功但响应里没有任务 id：{}", truncate(&body, 300))
        }
        Ok((code, body)) => format!("multipart 提交返回 HTTP {code}：{}", truncate(&body, 300)),
        Err(e) => format!("multipart 提交失败：{e}"),
    };

    // 第二枪：JSON（兼容中转常见形状）
    let json_body = json!({
        "model": model,
        "prompt": prompt,
        "size": size,
        "seconds": duration,
    });
    let resp = client
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .json(&json_body)
        .send()
        .await
        .map_err(|e| format!("{first_error}；JSON 提交也失败：{e}"))?;

    let code = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !code.is_success() {
        return Err(format!(
            "{first_error}；JSON 提交返回 HTTP {}：{}",
            code.as_u16(),
            truncate(&body, 300)
        ));
    }

    let parsed: Value = serde_json::from_str(&body).map_err(|e| {
        format!(
            "{first_error}；JSON 提交的响应不是 JSON（{e}）：{}",
            truncate(&body, 300)
        )
    })?;

    match extract_task(&parsed) {
        Some((id, status)) => Ok((id, status, parsed)),
        None => Err(format!(
            "{first_error}；JSON 提交成功但响应里没有任务 id：{}",
            truncate(&body, 300)
        )),
    }
}

struct RemoteState {
    status: String,
    raw: Value,
    download_url: Option<String>,
    error: Option<String>,
}

async fn fetch_state(
    client: &reqwest::Client,
    cfg: &VideoConfig,
    task_id: &str,
) -> Result<RemoteState, String> {
    // Ark 的查询路径与 OpenAI 不同，先按服务商分流再拼 URL。
    let path = if is_ark(&cfg.base_url) {
        format!("contents/generations/tasks/{task_id}")
    } else {
        format!("videos/{task_id}")
    };
    let url = endpoint(&cfg.base_url, &path);
    let resp = client
        .get(&url)
        .bearer_auth(&cfg.api_key)
        .send()
        .await
        .map_err(|e| format!("查询任务 {task_id} 失败：{e}"))?;

    let code = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !code.is_success() {
        return Err(format!(
            "查询任务 {task_id} 返回 HTTP {}：{}",
            code.as_u16(),
            truncate(&body, 300)
        ));
    }

    let raw: Value = serde_json::from_str(&body)
        .map_err(|e| format!("查询响应不是 JSON（{e}）：{}", truncate(&body, 300)))?;

    // 状态可能在外层，也可能在 data/result 里
    let status = ["status", "state"]
        .iter()
        .find_map(|k| raw.get(*k).and_then(|v| v.as_str()))
        .or_else(|| {
            raw.get("data")
                .and_then(|d| d.get("status").and_then(|v| v.as_str()))
        })
        .map(normalize_status)
        .unwrap_or_else(|| "in_progress".to_string());

    Ok(RemoteState {
        status,
        download_url: extract_download_url(&raw),
        error: extract_error_message(&raw),
        raw,
    })
}

/// 下载并落盘，返回 (路径, 字节数, ffprobe 摘要)。
async fn download_video(
    client: &reqwest::Client,
    cfg: &VideoConfig,
    task_id: &str,
    state: &RemoteState,
    out_dir: &PathBuf,
) -> Result<(String, i64, Option<Value>), String> {
    let bytes = match &state.download_url {
        Some(url) => {
            let resp = client
                .get(url)
                .send()
                .await
                .map_err(|e| format!("下载视频失败：{e}"))?;
            let code = resp.status();
            if !code.is_success() {
                return Err(format!("下载地址返回 HTTP {}", code.as_u16()));
            }
            resp.bytes()
                .await
                .map_err(|e| format!("读取视频字节失败：{e}"))?
                .to_vec()
        }
        None => {
            // Ark 没有 OpenAI 那种 `/videos/{id}/content` 取流端点，成品只从
            // `content.video_url` 拿。这里若为空说明是"任务声称成功但没给地址"，
            // 直接如实报错，不要拼一个不存在的路径去请求。
            if is_ark(&cfg.base_url) {
                return Err(format!(
                    "任务 {task_id} 已完成但响应里没有 content.video_url，无法下载"
                ));
            }
            let url = endpoint(&cfg.base_url, &format!("videos/{task_id}/content"));
            let resp = client
                .get(&url)
                .bearer_auth(&cfg.api_key)
                .send()
                .await
                .map_err(|e| format!("下载视频失败：{e}"))?;
            let code = resp.status();
            if !code.is_success() {
                return Err(format!("下载视频返回 HTTP {}（端点 {url}）", code.as_u16()));
            }
            resp.bytes()
                .await
                .map_err(|e| format!("读取视频字节失败：{e}"))?
                .to_vec()
        }
    };

    if bytes.is_empty() {
        return Err("下载到的视频是 0 字节".to_string());
    }
    if bytes.len() > MAX_VIDEO_BYTES {
        return Err(format!(
            "视频超过 {} MB 上限",
            MAX_VIDEO_BYTES / 1024 / 1024
        ));
    }

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let path = out_dir.join(format!("video-{stamp}-{task_id}.mp4"));
    std::fs::write(&path, &bytes).map_err(|e| format!("写入 {} 失败：{e}", path.display()))?;

    // 落盘校验：字节数对得上、文件头是 MP4/MOV 家族（ftyp 在第 4..8 字节）。
    let meta = std::fs::metadata(&path).map_err(|e| format!("读取文件属性失败：{e}"))?;
    if meta.len() as usize != bytes.len() {
        return Err(format!(
            "落盘字节数不一致：内存 {} / 磁盘 {}",
            bytes.len(),
            meta.len()
        ));
    }
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        // 不是致命错误（个别服务商发 WebM/AVI），但必须在返回值里说清，
        // 否则用户双击打不开时无从判断。
        let head: Vec<String> = bytes.iter().take(8).map(|b| format!("{b:02x}")).collect();
        let probe = probe_video(&path);
        return Ok((
            path.display().to_string(),
            meta.len() as i64,
            Some(json!({
                "container_hint": "非 MP4（ftyp 缺失），文件按 .mp4 保存，可能需要改扩展名",
                "head_hex": head.join(" "),
                "ffprobe": probe,
            })),
        ));
    }

    let probe = probe_video(&path);
    Ok((path.display().to_string(), meta.len() as i64, probe))
}

/// 用 ffprobe 核对真实时长与分辨率；没装就返回 None，不假装核对过。
fn probe_video(path: &std::path::Path) -> Option<Value> {
    let text = path.to_string_lossy().to_string();
    let probe = super::media::run_ffprobe_json(&[
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height,duration,codec_name",
        "-show_entries",
        "format=duration,size,format_name",
        "-of",
        "json",
        &text,
    ])
    .ok()?;

    let stream = probe.get("streams").and_then(|s| s.as_array())?.first()?;
    let format = probe.get("format");
    Some(json!({
        "width": stream.get("width").and_then(|v| v.as_u64()),
        "height": stream.get("height").and_then(|v| v.as_u64()),
        "duration_sec": format
            .and_then(|f| f.get("duration"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .or_else(|| stream.get("duration").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok())),
        "format_name": format.and_then(|f| f.get("format_name").and_then(|v| v.as_str())),
        "codec": stream.get("codec_name").and_then(|v| v.as_str()),
    }))
}

// ============================================================
// video_generate
// ============================================================

pub struct VideoGenerateTool;

#[async_trait]
impl AgentTool for VideoGenerateTool {
    fn name(&self) -> &'static str {
        "video_generate"
    }

    fn description(&self) -> &'static str {
        "按文字描述生成视频（调用设置里配置的 OpenAI 兼容视频服务）。默认等到完成并保存成品；wait=false 时只提交并返回 task_id，之后用 video_generation_status 续查。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "视频内容描述" },
                "size": { "type": "string", "description": "可选，如 1280x720 / 720x1280；留空用设置默认值" },
                "duration_sec": { "type": "integer", "description": "可选，视频时长（秒），留空用设置默认值" },
                "model": { "type": "string", "description": "可选，覆盖设置里的模型名" },
                "wait": { "type": "boolean", "description": "是否等到生成完成，默认 true" },
                "max_wait_sec": { "type": "integer", "description": "可选，本次最长等待秒数，覆盖设置里的 video.poll_timeout_ms" },
                "output_dir": { "type": "string", "description": "可选，保存目录；相对路径基于默认视频目录，默认 <视频目录>/EAssisant" }
            },
            "required": ["prompt"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let prompt = args["prompt"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("prompt 不能为空".to_string()))?
            .to_string();

        let cfg = match load_video_config() {
            Ok(cfg) => cfg,
            Err(missing) => return Ok(missing_result("视频生成", &missing)),
        };

        let model = args["model"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&cfg.model)
            .to_string();
        let size = args["size"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&cfg.size)
            .to_string();
        let duration = args["duration_sec"]
            .as_u64()
            .map(|v| v.clamp(1, 60))
            .unwrap_or(cfg.duration_sec);
        let wait = args["wait"].as_bool().unwrap_or(true);
        let out_dir = resolve_dir(args["output_dir"].as_str())?;

        let max_wait_sec = args["max_wait_sec"]
            .as_u64()
            .map(|v| v.clamp(30, 3600))
            .unwrap_or(cfg.poll_timeout_ms / 1000);

        let submit_client = build_client(180)?;

        let (task_id, initial_status, raw) =
            match submit_task(&submit_client, &cfg, &model, &prompt, &size, duration).await {
                Ok(v) => v,
                Err(e) => return Ok(ToolResult::err("VIDEO_SUBMIT_FAILED", &e)),
            };

        let request_json = json!({
            "model": model,
            "prompt": prompt,
            "size": size,
            "seconds": duration,
        });
        let ledger = record_generation(
            "video",
            &cfg.base_url,
            &model,
            Some(&task_id),
            &prompt,
            Some(&size),
            Some(duration as i64),
            &initial_status,
            &request_json,
        );

        if !wait {
            return Ok(ToolResult::ok(json!({
                "task_id": task_id,
                "status": initial_status,
                "waiting": false,
                "provider": cfg.base_url,
                "model": model,
                "size": size,
                "duration_sec": duration,
                "ledger_id": ledger,
                "next": "用 video_generation_status 传 task_id 续查；完成后会自动下载并校验落盘。",
                "raw_status": truncate(&raw.to_string(), 400),
            })));
        }

        // 轮询：先查一次再睡，避免刚好已完成的任务白等一轮。
        let poll_client = build_client(60)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(max_wait_sec);
        let mut last_status = initial_status.clone();
        let mut last_error: Option<String> = None;

        loop {
            let state = match fetch_state(&poll_client, &cfg, &task_id).await {
                Ok(s) => s,
                Err(e) => {
                    // 单次查询失败不立刻放弃：网络抖动很常见，等下一轮。
                    last_error = Some(e);
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SEC)).await;
                    continue;
                }
            };

            last_status = state.status.clone();

            match state.status.as_str() {
                "completed" => {
                    let download_client = build_client(600)?;
                    match download_video(&download_client, &cfg, &task_id, &state, &out_dir).await {
                        Ok((path, bytes, probe)) => {
                            if let Some(id) = &ledger {
                                update_generation(
                                    id,
                                    "downloaded",
                                    Some("completed"),
                                    Some(&path),
                                    Some(bytes),
                                    None,
                                );
                            }
                            return Ok(ToolResult::ok(json!({
                                "task_id": task_id,
                                "status": "completed",
                                "waiting": true,
                                "path": path,
                                "bytes": bytes,
                                "video_probe": probe,
                                "provider": cfg.base_url,
                                "model": model,
                                "size_requested": size,
                                "duration_requested_sec": duration,
                                "duration_actual_sec": probe
                                    .as_ref()
                                    .and_then(|p| p.get("duration_sec").cloned()),
                                "api_key": mask(&cfg.api_key),
                                "ledger_id": ledger,
                                "raw_status": truncate(&state.raw.to_string(), 400),
                            })));
                        }
                        Err(e) => {
                            if let Some(id) = &ledger {
                                update_generation(
                                    id,
                                    "completed",
                                    Some("completed"),
                                    None,
                                    None,
                                    Some(&e),
                                );
                            }
                            return Ok(ToolResult::err(
                                "VIDEO_DOWNLOAD_FAILED",
                                &format!("任务 {task_id} 已完成，但下载/落盘失败：{e}"),
                            ));
                        }
                    }
                }
                "failed" => {
                    let reason = state
                        .error
                        .clone()
                        .unwrap_or_else(|| truncate(&state.raw.to_string(), 300));
                    if let Some(id) = &ledger {
                        update_generation(id, "failed", Some("failed"), None, None, Some(&reason));
                    }
                    return Ok(ToolResult::err(
                        "VIDEO_GENERATION_FAILED",
                        &format!("任务 {task_id} 在服务商侧失败：{reason}"),
                    ));
                }
                other => {
                    if let Some(id) = &ledger {
                        update_generation(id, "pending", Some(other), None, None, None);
                    }
                }
            }

            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SEC)).await;
        }

        // 超时：任务还在服务商那边跑，台账里留着 task_id，别把结果丢掉。
        if let Some(id) = &ledger {
            update_generation(
                id,
                "pending",
                Some(&last_status),
                None,
                None,
                last_error.as_deref(),
            );
        }
        Ok(ToolResult::err(
            "VIDEO_TIMEOUT",
            &format!(
                "等了 {max_wait_sec} 秒仍未完成（最后状态 {last_status}）。任务 id 是 {task_id}，已记入台账；稍后用 video_generation_status 继续查，不用重新提交（重新提交会再花一份钱）。{}",
                last_error
                    .map(|e| format!("最近一次查询错误：{e}"))
                    .unwrap_or_default()
            ),
        ))
    }
}

// ============================================================
// video_generation_status
// ============================================================

pub struct VideoGenerationStatusTool;

#[async_trait]
impl AgentTool for VideoGenerationStatusTool {
    fn name(&self) -> &'static str {
        "video_generation_status"
    }

    fn description(&self) -> &'static str {
        "查询视频生成任务：传 task_id 则向服务商查最新状态，完成时自动下载并校验落盘；不传则列出本地台账里最近的视频任务（不发起网络请求）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "description": "可选，video_generate 返回的任务 id" },
                "limit": { "type": "integer", "description": "不传 task_id 时列出的条数，默认 5，最多 20" },
                "save": { "type": "boolean", "description": "完成时是否下载保存，默认 true" },
                "output_dir": { "type": "string", "description": "可选，保存目录" }
            },
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // 带 save=true 会写文件；查询本身只读，但风险等级按最重的那条路径定。
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let task_id = args["task_id"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let Some(task_id) = task_id else {
            return Ok(list_recent(args["limit"].as_u64().unwrap_or(5) as usize));
        };

        let cfg = match load_video_config() {
            Ok(cfg) => cfg,
            Err(missing) => return Ok(missing_result("视频生成", &missing)),
        };

        let ledger = find_by_task_id(task_id);
        let client = build_client(60)?;
        let state = match fetch_state(&client, &cfg, task_id).await {
            Ok(s) => s,
            Err(e) => return Ok(ToolResult::err("VIDEO_STATUS_QUERY_FAILED", &e)),
        };

        let mut result = json!({
            "task_id": task_id,
            "status": state.status,
            "provider": cfg.base_url,
            "raw_status": truncate(&state.raw.to_string(), 400),
            "ledger": ledger.as_ref().map(|(id, kind, st)| json!({
                "id": id, "kind": kind, "status": st
            })),
        });

        if state.status == "completed" {
            let save = args["save"].as_bool().unwrap_or(true);
            if !save {
                result["note"] = json!(
                    "任务已完成；save=false 所以没有下载。需要成品时再调一次并保持 save=true。"
                );
                return Ok(ToolResult::ok(result));
            }
            let out_dir = resolve_dir(args["output_dir"].as_str())?;
            let download_client = build_client(600)?;
            match download_video(&download_client, &cfg, task_id, &state, &out_dir).await {
                Ok((path, bytes, probe)) => {
                    if let Some((id, _, _)) = &ledger {
                        update_generation(
                            id,
                            "downloaded",
                            Some("completed"),
                            Some(&path),
                            Some(bytes),
                            None,
                        );
                    }
                    result["path"] = json!(path);
                    result["bytes"] = json!(bytes);
                    result["video_probe"] = probe.unwrap_or(Value::Null);
                }
                Err(e) => {
                    if let Some((id, _, _)) = &ledger {
                        update_generation(id, "completed", Some("completed"), None, None, Some(&e));
                    }
                    result["download_error"] = json!(e);
                }
            }
        } else if state.status == "failed" {
            if let Some((id, _, _)) = &ledger {
                update_generation(
                    id,
                    "failed",
                    Some("failed"),
                    None,
                    None,
                    state.error.as_deref(),
                );
            }
            result["error"] = json!(state.error);
        } else if let Some((id, _, _)) = &ledger {
            update_generation(id, "pending", Some(&state.status), None, None, None);
        }

        Ok(ToolResult::ok(result))
    }
}

/// 只读本地台账，不碰网络——"我刚才那几个视频跑到哪了"要能离线回答。
fn list_recent(limit: usize) -> ToolResult {
    let limit = limit.clamp(1, 20);
    let Some(conn) = crate::memory::open_connection() else {
        return ToolResult::err("DB_UNAVAILABLE", "数据库尚未初始化，读不到生成台账。");
    };
    let mut stmt = match conn.prepare(
        "SELECT id, task_id, model, prompt, status, remote_status, artifact_path, error, created_at
         FROM media_generations WHERE kind = 'video'
         ORDER BY created_at DESC LIMIT ?1",
    ) {
        Ok(s) => s,
        Err(e) => return ToolResult::err("DB_QUERY_FAILED", &format!("查询台账失败：{e}")),
    };

    let rows = stmt.query_map([limit as i64], |row| {
        Ok(json!({
            "ledger_id": row.get::<_, String>(0)?,
            "task_id": row.get::<_, Option<String>>(1)?,
            "model": row.get::<_, String>(2)?,
            "prompt": row.get::<_, String>(3)?,
            "status": row.get::<_, String>(4)?,
            "remote_status": row.get::<_, Option<String>>(5)?,
            "path": row.get::<_, Option<String>>(6)?,
            "error": row.get::<_, Option<String>>(7)?,
            "created_at": row.get::<_, String>(8)?,
        }))
    });

    match rows {
        Ok(iter) => {
            let items: Vec<Value> = iter.filter_map(|r| r.ok()).collect();
            ToolResult::ok(json!({
                "count": items.len(),
                "tasks": items,
                "note": "这是本地台账的最近状态。要拿服务商侧的最新状态，传 task_id 再查一次。",
            }))
        }
        Err(e) => ToolResult::err("DB_QUERY_FAILED", &format!("读取台账失败：{e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_vendor_statuses() {
        assert_eq!(normalize_status("succeeded"), "completed");
        assert_eq!(normalize_status("IN_PROGRESS"), "in_progress");
        assert_eq!(normalize_status("processing"), "in_progress");
        assert_eq!(normalize_status("cancelled"), "failed");
        assert_eq!(normalize_status("queued"), "queued");
        assert_eq!(normalize_status("weird"), "weird");
    }

    #[test]
    fn extracts_task_from_nested_shapes() {
        let flat = json!({"id": "vid_1", "status": "queued"});
        assert_eq!(extract_task(&flat).unwrap().0, "vid_1");

        let nested = json!({"data": {"task_id": "vid_2", "state": "succeeded"}});
        let (id, status) = extract_task(&nested).unwrap();
        assert_eq!(id, "vid_2");
        assert_eq!(status, "completed");

        assert!(extract_task(&json!({"foo": 1})).is_none());
    }

    #[test]
    fn extracts_download_url_from_various_fields() {
        assert_eq!(
            extract_download_url(&json!({"output": {"video_url": "https://x/y.mp4"}})).as_deref(),
            Some("https://x/y.mp4")
        );
        assert!(extract_download_url(&json!({"url": "file:///etc/passwd"})).is_none());
    }

    #[test]
    fn extracts_provider_error_message() {
        assert_eq!(
            extract_error_message(&json!({"error": {"message": "quota exceeded"}})).as_deref(),
            Some("quota exceeded")
        );
        assert_eq!(
            extract_error_message(&json!({"error": "bad request"})).as_deref(),
            Some("bad request")
        );
    }
}
