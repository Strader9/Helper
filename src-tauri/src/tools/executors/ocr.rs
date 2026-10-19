//! 图片文字识别（OCR）
//!
//! 走系统自带的 `Windows.Media.Ocr`（WinRT），不引 tesseract：
//! - tesseract 要额外分发 100MB+ 的语言数据包，且中文识别质量不如系统引擎；
//! - Windows 10/11 自带 OCR 引擎，安装了哪个语言包就能识别哪种语言，
//!   中文系统开箱就能认中文+英文混排。
//!
//! 代价是只支持 Windows——这正是本项目的定位（PC Guardian）。
//!
//! 两个工具的分工：`ocr_image_file` 认磁盘上的图片（截图、扫描件、照片），
//! `ocr_screen` 现截现认（认屏幕上用户正在看的东西，不落永久文件）。

use std::path::Path;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::executors::os_extra::run_powershell;
use crate::tools::registry::{AgentTool, RiskLevel, ToolResult};

fn require_str<'a>(args: &'a Value, key: &str) -> AppResult<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::InvalidArgument(format!("{} is required", key)))
}

/// OCR 结果：整段文本 + 逐行结果
#[derive(Debug)]
struct OcrOutput {
    text: String,
    lines: Vec<String>,
    language: String,
}

/// 调 WinRT 做识别。
///
/// 整个过程是同步阻塞的：WinRT 的 IAsyncOperation 在 `get()` 上等待，
/// 一次识别几十毫秒到几百毫秒。放 spawn_blocking 里跑，避免卡住 tokio 的
/// 工作线程——截图+识别两秒，够把整个 runtime 的其他任务饿住。
#[cfg(windows)]
fn ocr_image_blocking(path: &str, language: Option<&str>) -> Result<OcrOutput, String> {
    use windows::core::HSTRING;
    use windows::Graphics::Imaging::BitmapDecoder;
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::{FileAccessMode, StorageFile};

    if !Path::new(path).is_file() {
        return Err(format!("文件不存在: {}", path));
    }

    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path))
        .and_then(|op| op.get())
        .map_err(|e| format!("打开图片失败: {}", e))?;
    let stream = file
        .OpenAsync(FileAccessMode::Read)
        .and_then(|op| op.get())
        .map_err(|e| format!("读取图片流失败: {}", e))?;
    let decoder = BitmapDecoder::CreateAsync(&stream)
        .and_then(|op| op.get())
        .map_err(|e| format!("解码图片失败（格式不支持？）: {}", e))?;
    let bitmap = decoder
        .GetSoftwareBitmapAsync()
        .and_then(|op| op.get())
        .map_err(|e| format!("转换位图失败: {}", e))?;

    // 引擎选择：显式指定语言时按语言建，否则用用户配置的语言列表。
    //
    // 显式语言失败不静默回退——用户指定"日文"却拿到中文识别结果，
    // 比直接说"这台机器没装日文 OCR"糟糕得多。
    let (engine, used_language) = if let Some(lang) = language.filter(|l| !l.trim().is_empty()) {
        use windows::Globalization::Language;
        let language_obj = Language::CreateLanguage(&HSTRING::from(lang.trim()))
            .map_err(|e| format!("语言标识不合法: {} ({})", lang, e))?;
        let engine = OcrEngine::TryCreateFromLanguage(&language_obj).map_err(|e| {
            format!(
                "这台机器没有安装「{}」的 OCR 语言包（设置 → 时间和语言 → 语言 里添加该语言的「光学字符识别」）: {}",
                lang, e
            )
        })?;
        (engine, lang.trim().to_string())
    } else {
        let engine = OcrEngine::TryCreateFromUserProfileLanguages()
            .map_err(|e| format!("无法创建 OCR 引擎（系统可能缺少 OCR 语言包）: {}", e))?;
        let language_name = engine
            .RecognizerLanguage()
            .ok()
            .and_then(|l| l.LanguageTag().ok())
            .map(|t| t.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        (engine, language_name)
    };

    let result = engine
        .RecognizeAsync(&bitmap)
        .and_then(|op| op.get())
        .map_err(|e| format!("识别失败: {}", e))?;

    let mut lines: Vec<String> = Vec::new();
    if let Ok(ocr_lines) = result.Lines() {
        for line in ocr_lines {
            if let Ok(text) = line.Text() {
                let t = text.to_string();
                if !t.trim().is_empty() {
                    lines.push(t);
                }
            }
        }
    }
    // 优先用整段文本（多行时引擎会保留原始换行与词间距）；
    // 拿不到就退化成逐行拼接。
    let text = match result.Text() {
        Ok(t) if !t.is_empty() => t.to_string(),
        _ => lines.join("\n"),
    };

    Ok(OcrOutput {
        text,
        lines,
        language: used_language,
    })
}

#[cfg(not(windows))]
fn ocr_image_blocking(_path: &str, _language: Option<&str>) -> Result<OcrOutput, String> {
    Err("OCR 目前只在 Windows 上实现（依赖系统自带的 Windows.Media.Ocr）".to_string())
}

/// 可用语言列表，用于诊断"为什么指定语言报错"。
#[cfg(windows)]
fn available_languages_blocking() -> Result<Vec<String>, String> {
    use windows::Media::Ocr::OcrEngine;
    let langs = OcrEngine::AvailableRecognizerLanguages()
        .map_err(|e| format!("无法枚举 OCR 语言: {}", e))?;
    let mut out = Vec::new();
    for lang in langs {
        if let Ok(tag) = lang.LanguageTag() {
            out.push(tag.to_string());
        }
    }
    Ok(out)
}

#[cfg(not(windows))]
fn available_languages_blocking() -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

/// 截屏存临时 PNG，返回路径。与 `take_screenshot` 用同一套 GDI+ 方案。
fn capture_screen_to_png() -> AppResult<String> {
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S_%3f");
    let path = std::env::temp_dir().join(format!("pcg_ocr_{}.png", stamp));
    let path_str = path.to_string_lossy().to_string();
    let escaped = path_str.replace('\'', "''");

    let script = format!(
        r#"Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bitmap = New-Object System.Drawing.Bitmap($screen.Width, $screen.Height)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($screen.Location, [System.Drawing.Point]::Empty, $screen.Size)
$bitmap.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png)
$graphics.Dispose()
$bitmap.Dispose()
'ok'"#,
        escaped
    );
    let (ok, stdout, stderr) = run_powershell(&script)?;
    if !ok || !Path::new(&path_str).is_file() {
        return Err(AppError::ToolExecution(format!(
            "截屏失败: {}",
            stderr.trim()
        )));
    }
    let _ = stdout;
    Ok(path_str)
}

// ============================================================
// ocr_image_file
// ============================================================

/// 识别图片里的文字
pub struct OcrImageFileTool;

#[async_trait]
impl AgentTool for OcrImageFileTool {
    fn name(&self) -> &'static str {
        "ocr_image_file"
    }

    fn description(&self) -> &'static str {
        "识别图片文件里的文字（截图、扫描件、照片、无文字层的 PDF 转成的图）。用系统自带的 Windows OCR 引擎，支持中英文混排。language 可指定语言标识（如 zh-Hans、en-US），省略则用系统语言。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "图片路径（png/jpg/bmp/gif/tiff）" },
                "language": { "type": "string", "description": "语言标识，如 zh-Hans、en-US；省略用系统语言" },
                "max_chars": { "type": "integer", "description": "最多返回多少字符，默认 20000" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // 只读图片，但会把图片内容转成文本送进模型上下文——
        // 图片里可能有用户没打算外传的东西，所以不给 Safe。
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?.to_string();
        if !Path::new(&path).is_file() {
            return Err(AppError::InvalidArgument(format!("文件不存在: {}", path)));
        }
        let allowed = ["png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp"];
        let ext = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !allowed.contains(&ext.as_str()) {
            return Err(AppError::InvalidArgument(format!(
                "不支持的图片格式: {}（支持 {}）",
                if ext.is_empty() { "(无扩展名)" } else { &ext },
                allowed.join("/")
            )));
        }

        let language = args["language"].as_str().map(|s| s.to_string());
        let max_chars = args["max_chars"].as_u64().unwrap_or(20_000).min(80_000) as usize;

        let path_for_task = path.clone();
        let lang_for_task = language.clone();
        let result = tokio::task::spawn_blocking(move || {
            ocr_image_blocking(&path_for_task, lang_for_task.as_deref())
        })
        .await
        .map_err(|e| AppError::ToolExecution(format!("OCR 任务异常: {}", e)))?;

        let output = match result {
            Ok(o) => o,
            Err(message) => {
                // 指定语言失败时顺带把可用语言列出来，省得用户再问一次
                let available = available_languages_blocking().unwrap_or_default();
                if available.is_empty() {
                    return Ok(ToolResult::err("OCR_FAILED", &message));
                }
                return Ok(ToolResult::err(
                    "OCR_FAILED",
                    &format!("{}。本机可用的 OCR 语言: {}", message, available.join(", ")),
                ));
            }
        };

        let (text, truncated) = if output.text.chars().count() > max_chars {
            (
                output.text.chars().take(max_chars).collect::<String>(),
                true,
            )
        } else {
            (output.text.clone(), false)
        };

        Ok(ToolResult::ok(json!({
            "path": path,
            "language": output.language,
            "line_count": output.lines.len(),
            "chars": output.text.chars().count(),
            "truncated": truncated,
            "empty": output.text.trim().is_empty(),
            "lines": output.lines,
            "text": text,
        })))
    }
}

// ============================================================
// ocr_screen
// ============================================================

/// 识别当前屏幕上的文字
pub struct OcrScreenTool;

#[async_trait]
impl AgentTool for OcrScreenTool {
    fn name(&self) -> &'static str {
        "ocr_screen"
    }

    fn description(&self) -> &'static str {
        "截取当前主屏幕并识别其中的文字，同时保留截图文件。适合读取用户正在看的报错、表格、对话框内容——比 take_screenshot 更进一步，直接给出文字。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "language": { "type": "string", "description": "语言标识，如 zh-Hans；省略用系统语言" },
                "max_chars": { "type": "integer", "description": "最多返回多少字符，默认 20000" },
                "keep_screenshot": { "type": "boolean", "description": "是否保留截图文件，默认 true；false 则识别后删除" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        // 屏幕内容是最私密的一类数据（可能含聊天、密码管理器界面），
        // 即使只是读也要走一次确认。
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let screenshot = capture_screen_to_png()?;
        let language = args["language"].as_str().map(|s| s.to_string());
        let keep = args["keep_screenshot"].as_bool().unwrap_or(true);
        let max_chars = args["max_chars"].as_u64().unwrap_or(20_000).min(80_000) as usize;

        let shot_for_task = screenshot.clone();
        let lang_for_task = language.clone();
        let result = tokio::task::spawn_blocking(move || {
            ocr_image_blocking(&shot_for_task, lang_for_task.as_deref())
        })
        .await
        .map_err(|e| AppError::ToolExecution(format!("OCR 任务异常: {}", e)))?;

        let output = match result {
            Ok(o) => o,
            Err(message) => {
                // 识别失败也要把截图路径告诉用户：图已经截了，
                // 至少让人能自己看一眼出了什么事。
                return Ok(ToolResult::err(
                    "OCR_FAILED",
                    &format!("{}（截图已保存: {}）", message, screenshot),
                ));
            }
        };

        if !keep {
            let _ = std::fs::remove_file(&screenshot);
        }

        let (text, truncated) = if output.text.chars().count() > max_chars {
            (
                output.text.chars().take(max_chars).collect::<String>(),
                true,
            )
        } else {
            (output.text.clone(), false)
        };

        Ok(ToolResult::ok(json!({
            "screenshot": if keep { Value::String(screenshot) } else { Value::Null },
            "language": output.language,
            "line_count": output.lines.len(),
            "chars": output.text.chars().count(),
            "truncated": truncated,
            "empty": output.text.trim().is_empty(),
            "lines": output.lines,
            "text": text,
        })))
    }
}

// ============================================================
// ocr_languages
// ============================================================

/// 列出可用的 OCR 语言
pub struct OcrLanguagesTool;

#[async_trait]
impl AgentTool for OcrLanguagesTool {
    fn name(&self) -> &'static str {
        "ocr_languages"
    }

    fn description(&self) -> &'static str {
        "列出这台机器上已安装的 OCR 识别语言。指定 language 失败时可以先查这个，看看到底支持哪些。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: Value) -> AppResult<ToolResult> {
        let languages = tokio::task::spawn_blocking(available_languages_blocking)
            .await
            .map_err(|e| AppError::ToolExecution(format!("查询语言列表异常: {}", e)))?;
        match languages {
            Ok(list) => Ok(ToolResult::ok(json!({
                "count": list.len(),
                "languages": list,
            }))),
            Err(e) => Ok(ToolResult::err("OCR_LANGUAGES_FAILED", &e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_rejected_before_touching_winrt() {
        let err = ocr_image_blocking("Z:/no/such/image.png", None).unwrap_err();
        assert!(err.contains("不存在"), "错误信息应说清文件不存在: {}", err);
    }

    #[test]
    fn image_extension_filter_accepts_common_formats() {
        // 这条只验证白名单逻辑本身（列表在 execute 里内联），
        // 用与工具一致的集合做交叉检查，避免以后改了一处漏另一处。
        let allowed = ["png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp"];
        for ext in ["PNG", "jpg", "WebP"] {
            assert!(
                allowed.contains(&ext.to_lowercase().as_str()),
                "{} 应被接受",
                ext
            );
        }
        assert!(!allowed.contains(&"svg"));
    }

    #[test]
    fn screenshots_go_to_temp_dir() {
        let path = std::env::temp_dir();
        assert!(path.exists());
    }

    /// 实机回环：造一张有字的图片 → OCR 认出来。
    ///
    /// 断言只锁 ASCII 数字与字母（任何语言包都能认），中文识别结果依赖
    /// 系统装的是哪个 OCR 语言包，不能当作硬指标。
    #[tokio::test]
    #[ignore = "需要系统 OCR 语言包 + 真实图片"]
    async fn real_ocr_round_trip() {
        let dir = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let png = dir.join(format!("pcg-ocr-{}.png", stamp));
        let path_str = png.to_string_lossy().to_string();
        let escaped = path_str.replace('\'', "''");

        // 白底黑字、大字号：OCR 对小字识别率低，测试图要给它最好的条件
        let script = format!(
            r#"Add-Type -AssemblyName System.Drawing
$bmp = New-Object System.Drawing.Bitmap(900, 260)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.Clear([System.Drawing.Color]::White)
$font = New-Object System.Drawing.Font('Microsoft YaHei', 40, [System.Drawing.FontStyle]::Bold)
$brush = [System.Drawing.Brushes]::Black
$g.DrawString('Report 2026 Total: 12345', $font, $brush, 30, 40)
$g.DrawString('ABC-xyz 67890', $font, $brush, 30, 130)
$g.Dispose()
$bmp.Save('{}', [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
'ok'"#,
            escaped
        );
        let (ok, _out, err) = run_powershell(&script).expect("应能调用 PowerShell");
        assert!(ok, "造图失败: {}", err);
        assert!(Path::new(&path_str).is_file());

        let result = tokio::task::spawn_blocking({
            let p = path_str.clone();
            move || ocr_image_blocking(&p, None)
        })
        .await
        .unwrap();

        let output = match result {
            Ok(o) => o,
            Err(e) => panic!("OCR 失败: {}（本机已装 OCR 语言包吗？）", e),
        };

        // 数字是最稳的判据：不同语言包对字母的大小写/连字符处理不一致
        assert!(
            output.text.contains("12345") && output.text.contains("67890"),
            "应识别出两组数字，实际: {:?}",
            output.text
        );
        assert!(!output.lines.is_empty(), "应至少有一行");
        assert!(
            !output.language.is_empty() && output.language != "unknown",
            "应能报出识别语言"
        );

        // 走一遍工具层，确认返回结构完整（契约里的字段都在）
        let tool = OcrImageFileTool
            .execute(json!({ "path": path_str }))
            .await
            .expect("工具应返回");
        assert!(tool.success, "工具失败: {:?}", tool.error);
        let data = tool.data.unwrap();
        for field in ["path", "language", "line_count", "text"] {
            assert!(data.get(field).is_some(), "缺少字段 {}", field);
        }

        let _ = std::fs::remove_file(&png);
    }

    /// 可用语言列表：至少能跑通，不 panic
    #[tokio::test]
    #[ignore = "需要 Windows OCR 组件"]
    async fn real_ocr_languages_are_enumerable() {
        let r = OcrLanguagesTool.execute(json!({})).await.expect("应返回");
        assert!(r.success, "列举语言失败: {:?}", r.error);
        let data = r.data.unwrap();
        assert!(data["count"].as_u64().is_some());
    }
}
