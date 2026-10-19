//! 图像生成（真调用服务商，不造假图）。
//!
//! 协议：OpenAI 兼容 `POST {base_url}/images/generations`。
//! 请求体只放最小公约数 `{model, prompt, n, size}`——`response_format`
//! 有的服务接受、有的接受但忽略、`gpt-image-1` 系列直接报错，所以不传，
//! 返回里 `b64_json` 和 `url` 两种形态都兼容。
//!
//! 判据是**重新从磁盘打开生成的文件读尺寸**：HTTP 200 只说明服务端答应了，
//! 不等于写下来的东西是一张能打开的图。字节数对不上、尺寸读不出来，
//! 都算失败并如实报告，不把"可能有文件"说成"已生成"。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Value};

use super::media_provider::{
    endpoint, load_image_config, mask, missing_result, record_generation, truncate,
    update_generation,
};
use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

/// 单张图的响应体积上限。base64 会让体积膨胀约 4/3，1024×1024 PNG 通常在
/// 2–4 MB，给到 48 MB 足够覆盖 4K 出图，又能挡住"服务端返回一整页 HTML 错误页"。
const MAX_IMAGE_BYTES: usize = 48 * 1024 * 1024;

// ============================================================
// 图片头嗅探：不引 image crate，只读到尺寸为止
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageMeta {
    pub format: &'static str,
    pub width: u32,
    pub height: u32,
}

pub fn sniff_image(bytes: &[u8]) -> Option<ImageMeta> {
    if bytes.len() < 12 {
        return None;
    }

    // PNG: 89 50 4E 47 0D 0A 1A 0A，IHDR 宽高位于 16..24（大端）
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        if bytes.len() < 24 {
            return None;
        }
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return Some(ImageMeta { format: "png", width, height });
    }

    // GIF: "GIF87a" / "GIF89a"，宽高小端位于 6..10
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        let width = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let height = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return Some(ImageMeta { format: "gif", width, height });
    }

    // WebP: RIFF....WEBP
    if bytes.starts_with(b"RIFF") && bytes.len() >= 30 && &bytes[8..12] == b"WEBP" {
        let chunk = &bytes[12..16];
        if chunk == b"VP8X" {
            let width = 1 + u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]);
            let height = 1 + u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]);
            return Some(ImageMeta { format: "webp", width, height });
        }
        if chunk == b"VP8 " && bytes.len() >= 30 {
            let width = u16::from_le_bytes([bytes[26], bytes[27]]) as u32 & 0x3FFF;
            let height = u16::from_le_bytes([bytes[28], bytes[29]]) as u32 & 0x3FFF;
            return Some(ImageMeta { format: "webp", width, height });
        }
        if chunk == b"VP8L" && bytes.len() >= 25 {
            let bits = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
            let width = (bits & 0x3FFF) + 1;
            let height = ((bits >> 14) & 0x3FFF) + 1;
            return Some(ImageMeta { format: "webp", width, height });
        }
        return None;
    }

    // BMP: "BM"，宽高小端位于 18..26
    if bytes.starts_with(b"BM") && bytes.len() >= 26 {
        let width = i32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
        let height = i32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
        return Some(ImageMeta {
            format: "bmp",
            width: width.unsigned_abs(),
            height: height.unsigned_abs(),
        });
    }

    // JPEG: 逐段找 SOFn（C0..CF，排除 C4/C8/CC）
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2usize;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                i += 2;
                continue;
            }
            let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            if len < 2 {
                return None;
            }
            let is_sof = (0xC0..=0xCF).contains(&marker)
                && marker != 0xC4
                && marker != 0xC8
                && marker != 0xCC;
            if is_sof {
                if i + 9 >= bytes.len() {
                    return None;
                }
                let height = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let width = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return Some(ImageMeta { format: "jpeg", width, height });
            }
            i += 2 + len;
        }
        return None;
    }

    None
}

/// 默认落点：`<图片目录>/PCGuardian`；没有图片目录的老系统退到用户目录。
pub fn default_image_dir() -> PathBuf {
    dirs::picture_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Pictures")))
        .unwrap_or_else(std::env::temp_dir)
        .join("PCGuardian")
}

fn resolve_dir(requested: Option<&str>, default: PathBuf) -> AppResult<PathBuf> {
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

fn ext_for_format(format: &str) -> &'static str {
    match format {
        "jpeg" => "jpg",
        _ => "png",
    }
}

// ============================================================
// image_generate
// ============================================================

pub struct ImageGenerateTool;

#[async_trait]
impl AgentTool for ImageGenerateTool {
    fn name(&self) -> &'static str {
        "image_generate"
    }

    fn description(&self) -> &'static str {
        "按文字描述生成图片并保存到本地（调用设置里配置的 OpenAI 兼容图像服务）。返回每张图的落盘路径、字节数与真实尺寸；文件读不出尺寸即视为失败。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "图片描述，越具体越好" },
                "size": { "type": "string", "description": "可选，如 1024x1024 / 1792x1024；留空用设置里的默认值" },
                "n": { "type": "integer", "description": "生成张数，1-4，默认 1" },
                "model": { "type": "string", "description": "可选，覆盖设置里的模型名" },
                "output_dir": { "type": "string", "description": "可选，保存目录；相对路径基于默认图片目录，默认 <图片目录>/PCGuardian" }
            },
            "required": ["prompt"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // 会产生真实的服务商计费，并往磁盘写文件——MEDIUM 让人有机会拦一下。
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let prompt = args["prompt"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("prompt 不能为空".to_string()))?
            .to_string();

        let cfg = match load_image_config() {
            Ok(cfg) => cfg,
            Err(missing) => return Ok(missing_result("图像生成", &missing)),
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
        let n = args["n"].as_u64().unwrap_or(1).clamp(1, 4);
        let out_dir = resolve_dir(args["output_dir"].as_str(), default_image_dir())?;

        let req_body = json!({
            "model": model,
            "prompt": prompt,
            "n": n,
            "size": size,
        });

        let url = endpoint(&cfg.base_url, "images/generations");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(cfg.timeout_ms))
            .user_agent("pc-guardian/0.1")
            .build()
            .map_err(|e| AppError::Internal(format!("HTTP 客户端构造失败: {e}")))?;

        let resp = client
            .post(&url)
            .bearer_auth(&cfg.api_key)
            .json(&req_body)
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult::err(
                    "IMAGE_REQUEST_FAILED",
                    &format!("请求 {url} 失败：{e}"),
                ))
            }
        };

        let http_status = resp.status();
        let body_bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolResult::err(
                    "IMAGE_RESPONSE_READ_FAILED",
                    &format!("读取响应体失败：{e}"),
                ))
            }
        };

        if !http_status.is_success() {
            let body = String::from_utf8_lossy(&body_bytes);
            return Ok(ToolResult::err(
                "IMAGE_PROVIDER_ERROR",
                &format!(
                    "服务商返回 HTTP {}：{}",
                    http_status.as_u16(),
                    truncate(&body, 500)
                ),
            ));
        }

        let parsed: Value = serde_json::from_slice(&body_bytes).map_err(|e| {
            AppError::Internal(format!(
                "服务商返回的不是 JSON（{e}）：{}",
                truncate(&String::from_utf8_lossy(&body_bytes), 300)
            ))
        })?;

        let items = match parsed.get("data").and_then(|v| v.as_array()) {
            Some(items) if !items.is_empty() => items.clone(),
            _ => {
                return Ok(ToolResult::err(
                    "IMAGE_EMPTY_RESULT",
                    &format!(
                        "服务商没有返回任何图片（data 为空）：{}",
                        truncate(&parsed.to_string(), 400)
                    ),
                ))
            }
        };

        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let ledger = record_generation(
            "image",
            &cfg.base_url,
            &model,
            None,
            &prompt,
            Some(&size),
            None,
            "generating",
            &req_body,
        );

        let mut saved: Vec<Value> = Vec::new();
        let mut failures: Vec<Value> = Vec::new();

        for (idx, item) in items.iter().enumerate() {
            let bytes = match image_bytes_from_item(&client, item).await {
                Ok(Some(b)) => b,
                Ok(None) => {
                    failures.push(json!({
                        "index": idx,
                        "error": "该条既没有 b64_json 也没有 url",
                    }));
                    continue;
                }
                Err(e) => {
                    failures.push(json!({ "index": idx, "error": e }));
                    continue;
                }
            };

            if bytes.len() > MAX_IMAGE_BYTES {
                failures.push(json!({
                    "index": idx,
                    "error": format!("单张图片超过 {} MB 上限", MAX_IMAGE_BYTES / 1024 / 1024),
                }));
                continue;
            }

            let Some(meta) = sniff_image(&bytes) else {
                failures.push(json!({
                    "index": idx,
                    "error": "返回内容不是可识别的图片格式（PNG/JPEG/WebP/GIF/BMP）",
                    "first_bytes": bytes.iter().take(8).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "),
                }));
                continue;
            };

            let path: PathBuf = out_dir.join(format!(
                "image-{stamp}-{}.{}",
                idx + 1,
                ext_for_format(meta.format)
            ));

            if let Err(e) = std::fs::write(&path, &bytes) {
                failures.push(json!({
                    "index": idx,
                    "error": format!("写入 {} 失败: {e}", path.display()),
                }));
                continue;
            }

            // 落盘校验：重新打开文件，字节数与尺寸都要对得上。
            match verify_saved_image(&path) {
                Ok((disk_bytes, disk_meta)) if disk_bytes as usize == bytes.len() => {
                    saved.push(json!({
                        "index": idx,
                        "path": path.display().to_string(),
                        "bytes": disk_bytes,
                        "width": disk_meta.width,
                        "height": disk_meta.height,
                        "format": disk_meta.format,
                        "revised_prompt": item.get("revised_prompt").cloned(),
                    }));
                }
                Ok((disk_bytes, _)) => {
                    failures.push(json!({
                        "index": idx,
                        "error": format!("落盘字节数不一致：内存 {} / 磁盘 {}", bytes.len(), disk_bytes),
                    }));
                }
                Err(e) => failures.push(json!({ "index": idx, "error": e })),
            }
        }

        if let Some(id) = &ledger {
            if saved.is_empty() {
                update_generation(
                    id,
                    "failed",
                    None,
                    None,
                    None,
                    Some(&format!("{} 张全部失败", failures.len())),
                );
            } else {
                let first_path = saved[0]["path"].as_str().unwrap_or_default();
                let total: i64 = saved.iter().filter_map(|v| v["bytes"].as_i64()).sum();
                update_generation(
                    id,
                    "completed",
                    None,
                    Some(first_path),
                    Some(total),
                    None,
                );
            }
        }

        if saved.is_empty() {
            return Ok(ToolResult::err(
                "IMAGE_ALL_FAILED",
                &format!(
                    "{} 张图片全部失败：{}",
                    failures.len(),
                    truncate(&Value::Array(failures).to_string(), 600)
                ),
            ));
        }

        Ok(ToolResult::ok(json!({
            "image_count": saved.len(),
            "images": saved,
            "failed": failures,
            "provider": cfg.base_url,
            "model": model,
            "size_requested": size,
            "output_dir": out_dir.display().to_string(),
            "api_key": mask(&cfg.api_key),
            "ledger_id": ledger,
        })))
    }
}

/// 从单条结果里取出图片字节：优先 b64_json（无需二次下载），否则取 url。
async fn image_bytes_from_item(client: &reqwest::Client, item: &Value) -> Result<Option<Vec<u8>>, String> {
    use base64::Engine;

    if let Some(b64) = item.get("b64_json").and_then(|v| v.as_str()) {
        let b64 = b64.trim();
        if !b64.is_empty() {
            // 部分服务商带 data:image/png;base64, 前缀
            let payload = b64.split_once(',').map(|(_, rest)| rest).unwrap_or(b64);
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .map_err(|e| format!("b64_json 解码失败: {e}"))?;
            return Ok(Some(decoded));
        }
    }

    if let Some(url) = item.get("url").and_then(|v| v.as_str()) {
        let url = url.trim();
        if url.is_empty() {
            return Ok(None);
        }
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return Err(format!("不支持的图片地址: {url}"));
        }
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("下载图片失败: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("下载图片返回 HTTP {}", status.as_u16()));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("读取图片字节失败: {e}"))?;
        return Ok(Some(bytes.to_vec()));
    }

    Ok(None)
}

/// 重新打开文件读字节数与尺寸——这才是"图真的在那儿"的证据。
fn verify_saved_image(path: &Path) -> Result<(u64, ImageMeta), String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("读取文件属性失败: {e}"))?;
    if !meta.is_file() {
        return Err("落点不是文件".to_string());
    }
    if meta.len() == 0 {
        return Err("落点文件为 0 字节".to_string());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("读回文件失败: {e}"))?;
    let info = sniff_image(&bytes).ok_or_else(|| "读回内容无法识别为图片".to_string())?;
    if info.width == 0 || info.height == 0 {
        return Err("图片尺寸为 0".to_string());
    }
    Ok((meta.len(), info))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1×1 透明 PNG
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn sniffs_png_dimensions() {
        let meta = sniff_image(PNG_1X1).expect("应识别为 PNG");
        assert_eq!(meta.format, "png");
        assert_eq!((meta.width, meta.height), (1, 1));
    }

    #[test]
    fn sniffs_jpeg_sof0() {
        // SOI + APP0(空) + SOF0 段：高 32、宽 64
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x20, 0x00, 0x40, 0x03]);
        jpeg.extend_from_slice(&[0x01, 0x11, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
        let meta = sniff_image(&jpeg).expect("应识别为 JPEG");
        assert_eq!(meta.format, "jpeg");
        assert_eq!((meta.width, meta.height), (64, 32));
    }

    #[test]
    fn rejects_non_image_bytes() {
        assert!(sniff_image(b"<html>not an image</html>").is_none());
        assert!(sniff_image(&[]).is_none());
    }

    #[test]
    fn verify_rejects_empty_file() {
        let dir = std::env::temp_dir().join(format!("pcg-img-verify-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.png");
        std::fs::write(&path, b"").unwrap();
        assert!(verify_saved_image(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_accepts_real_png() {
        let dir = std::env::temp_dir().join(format!("pcg-img-verify-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ok.png");
        std::fs::write(&path, PNG_1X1).unwrap();
        let (bytes, meta) = verify_saved_image(&path).expect("应通过校验");
        assert_eq!(bytes, PNG_1X1.len() as u64);
        assert_eq!((meta.width, meta.height), (1, 1));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
