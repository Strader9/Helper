//! 媒体工具（media 域）
//!
//! 图像处理走系统自带的 `System.Drawing`：不引 image crate（那会把编译时间
//! 和体积一起拉起来），也不需要用户装任何东西。
//!
//! 音视频只做"能不能做"的探测：ffmpeg 不是系统组件，替用户静默下载一个
//! 几十兆的二进制不合适。装了就用，没装就如实说没装。
//!
//! 判据统一是**重新从磁盘打开输出文件读尺寸**，不是内存里那个 Bitmap 对象的
//! 属性——后者在 Save 失败时照样是对的。

use async_trait::async_trait;
use serde_json::{Value, json};

use super::os_extra::{literal, run_powershell_b64};
use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

// ============================================================
// 公共辅助
// ============================================================

/// 图片扩展名对应的 System.Drawing 格式表达式。
fn format_expr(path_var: &str) -> String {
    format!(
        r#"$ext = [System.IO.Path]::GetExtension({p}).ToLower()
switch ($ext) {{
  '.png'  {{ $fmt = [System.Drawing.Imaging.ImageFormat]::Png }}
  '.jpg'  {{ $fmt = [System.Drawing.Imaging.ImageFormat]::Jpeg }}
  '.jpeg' {{ $fmt = [System.Drawing.Imaging.ImageFormat]::Jpeg }}
  '.bmp'  {{ $fmt = [System.Drawing.Imaging.ImageFormat]::Bmp }}
  '.gif'  {{ $fmt = [System.Drawing.Imaging.ImageFormat]::Gif }}
  '.tif'  {{ $fmt = [System.Drawing.Imaging.ImageFormat]::Tiff }}
  '.tiff' {{ $fmt = [System.Drawing.Imaging.ImageFormat]::Tiff }}
  default {{ throw "不支持的输出格式: $ext" }}
}}"#,
        p = path_var
    )
}

/// 把脚本输出解析成 JSON；失败时把原文带回去，别只说"解析失败"。
fn parse_ps_json(stdout: &str) -> Result<Value, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err("脚本没有输出任何结果".to_string());
    }
    serde_json::from_str(trimmed).map_err(|e| format!("解析脚本输出失败：{}（原文：{}）", e, trimmed))
}

/// 输出路径：给了就用，没给就在原文件旁加后缀。
fn resolve_output(source: &str, output: Option<&str>, suffix: &str, ext: &str) -> String {
    if let Some(out) = output.map(str::trim).filter(|s| !s.is_empty()) {
        return out.to_string();
    }
    let path = std::path::Path::new(source);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".to_string());
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    parent
        .join(format!("{}_{}.{}", stem, suffix, ext))
        .to_string_lossy()
        .to_string()
}

/// 输出文件已存在时是否放行。
fn output_guard(output: &str, overwrite: bool) -> Option<ToolResult> {
    if std::path::Path::new(output).exists() && !overwrite {
        return Some(ToolResult::err(
            "OUTPUT_EXISTS",
            &format!(
                "输出文件已存在：{}。覆盖需要显式传 overwrite=true——上一版结果可能还要用。",
                output
            ),
        ));
    }
    None
}

/// 读源图尺寸（用于按比例缩放时的计算与参数校验）。
fn read_image_size(source: &str) -> AppResult<(i64, i64)> {
    let script = r#"
Add-Type -AssemblyName System.Drawing
$src = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__SRC__'))
if (-not (Test-Path -LiteralPath $src)) { throw "文件不存在" }
$img = [System.Drawing.Image]::FromFile($src)
$w = $img.Width; $h = $img.Height
$img.Dispose()
@{ width = $w; height = $h } | ConvertTo-Json -Compress
"#
    .replace("__SRC__", &literal(source));

    let (ok, stdout, stderr) = run_powershell_b64(&script)?;
    if !ok {
        return Err(AppError::ToolExecution(format!(
            "读取图片尺寸失败：{}",
            stderr.trim()
        )));
    }
    let value = parse_ps_json(&stdout).map_err(AppError::ToolExecution)?;
    let width = value["width"].as_i64().unwrap_or(0);
    let height = value["height"].as_i64().unwrap_or(0);
    Ok((width, height))
}

/// 重新打开磁盘上的输出文件，确认它真的是宽×高。
///
/// 这是这一域唯一的真判据：`Save` 不抛异常不代表写成了这份尺寸。
fn verify_output(output: &str, expect_width: i64, expect_height: i64) -> AppResult<Value> {
    let (width, height) = read_image_size(output)?;
    let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    Ok(json!({
        "path": output,
        "width": width,
        "height": height,
        "bytes": bytes,
        "dimensions_match": width == expect_width && height == expect_height,
    }))
}

// ============================================================
// image_info
// ============================================================

pub struct ImageInfoTool;

#[async_trait]
impl AgentTool for ImageInfoTool {
    fn name(&self) -> &'static str {
        "image_info"
    }

    fn description(&self) -> &'static str {
        "读取图片的基本信息：宽高、像素格式、DPI、文件大小。回答「这张图多大」或缩放前确认原尺寸时用。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "图片绝对路径" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let path = args["path"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;

        if !std::path::Path::new(path).is_file() {
            return Ok(ToolResult::err(
                "FILE_NOT_FOUND",
                &format!("文件不存在：{}", path),
            ));
        }

        let script = r#"
Add-Type -AssemblyName System.Drawing
$src = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__SRC__'))
$img = [System.Drawing.Image]::FromFile($src)
$result = @{
  width = $img.Width
  height = $img.Height
  pixel_format = $img.PixelFormat.ToString()
  horizontal_dpi = [math]::Round($img.HorizontalResolution, 1)
  vertical_dpi = [math]::Round($img.VerticalResolution, 1)
  raw_format = $img.RawFormat.Guid.ToString()
  bytes = (Get-Item -LiteralPath $src).Length
}
$img.Dispose()
$result | ConvertTo-Json -Compress
"#
        .replace("__SRC__", &literal(path));

        let (ok, stdout, stderr) = run_powershell_b64(&script)?;
        if !ok {
            return Ok(ToolResult::err(
                "IMAGE_READ_FAILED",
                &format!("读不出图片信息：{}", stderr.trim()),
            ));
        }
        match parse_ps_json(&stdout) {
            Ok(mut value) => {
                value["path"] = json!(path);
                Ok(ToolResult::ok(value))
            }
            Err(message) => Ok(ToolResult::err("IMAGE_READ_FAILED", &message)),
        }
    }
}

// ============================================================
// image_resize
// ============================================================

pub struct ImageResizeTool;

#[async_trait]
impl AgentTool for ImageResizeTool {
    fn name(&self) -> &'static str {
        "image_resize"
    }

    fn description(&self) -> &'static str {
        "缩放图片并另存为新文件。只给 width 或只给 height 时按原比例算另一边；两个都给就按给定尺寸拉伸。不覆盖已有文件，除非显式 overwrite=true。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "源图片路径" },
                "output": { "type": "string", "description": "输出路径（可选，默认源文件旁加尺寸后缀）" },
                "width": { "type": "integer", "minimum": 1, "description": "目标宽度（像素）" },
                "height": { "type": "integer", "minimum": 1, "description": "目标高度（像素）" },
                "overwrite": { "type": "boolean", "description": "允许覆盖已有输出（默认 false）" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let path = args["path"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;
        if !std::path::Path::new(path).is_file() {
            return Ok(ToolResult::err("FILE_NOT_FOUND", &format!("文件不存在：{}", path)));
        }

        let want_width = args["width"].as_i64().filter(|w| *w > 0);
        let want_height = args["height"].as_i64().filter(|h| *h > 0);
        if want_width.is_none() && want_height.is_none() {
            return Ok(ToolResult::err(
                "INVALID_PARAMS",
                "width 与 height 至少要给一个",
            ));
        }

        let (src_width, src_height) = match read_image_size(path) {
            Ok(size) => size,
            Err(e) => return Ok(ToolResult::err("IMAGE_READ_FAILED", &e.to_string())),
        };
        if src_width <= 0 || src_height <= 0 {
            return Ok(ToolResult::err("IMAGE_READ_FAILED", "源图尺寸无效"));
        }

        let (target_width, target_height) = match (want_width, want_height) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => {
                let h = ((w as f64) * (src_height as f64) / (src_width as f64)).round() as i64;
                (w, h.max(1))
            }
            (None, Some(h)) => {
                let w = ((h as f64) * (src_width as f64) / (src_height as f64)).round() as i64;
                (w.max(1), h)
            }
            (None, None) => unreachable!(),
        };

        let ext = std::path::Path::new(path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "png".to_string());
        let output = resolve_output(
            path,
            args["output"].as_str(),
            &format!("{}x{}", target_width, target_height),
            &ext,
        );
        if let Some(rejection) = output_guard(&output, args["overwrite"].as_bool().unwrap_or(false)) {
            return Ok(rejection);
        }

        let script = r#"
Add-Type -AssemblyName System.Drawing
$src = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__SRC__'))
$dst = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__DST__'))
$w = __W__; $h = __H__
$img = [System.Drawing.Image]::FromFile($src)
$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
$g.DrawImage($img, 0, 0, $w, $h)
$g.Dispose()
__FORMAT__
$bmp.Save($dst, $fmt)
$bmp.Dispose()
$img.Dispose()
@{ saved = $true } | ConvertTo-Json -Compress
"#
        .replace("__SRC__", &literal(path))
        .replace("__DST__", &literal(&output))
        .replace("__W__", &target_width.to_string())
        .replace("__H__", &target_height.to_string())
        .replace("__FORMAT__", &format_expr("$dst"));

        let (ok, _stdout, stderr) = run_powershell_b64(&script)?;
        if !ok {
            return Ok(ToolResult::err(
                "IMAGE_RESIZE_FAILED",
                &format!("缩放失败：{}", stderr.trim()),
            ));
        }

        let evidence = verify_output(&output, target_width, target_height)?;
        if evidence["dimensions_match"] != json!(true) {
            return Ok(ToolResult::err(
                "OUTPUT_DIMENSION_MISMATCH",
                &format!(
                    "输出尺寸与预期不符：期望 {}x{}，实际 {}x{}",
                    target_width,
                    target_height,
                    evidence["width"],
                    evidence["height"]
                ),
            ));
        }

        Ok(ToolResult::ok(json!({
            "source": path,
            "source_size": { "width": src_width, "height": src_height },
            "output": evidence,
        })))
    }
}

// ============================================================
// image_crop
// ============================================================

pub struct ImageCropTool;

#[async_trait]
impl AgentTool for ImageCropTool {
    fn name(&self) -> &'static str {
        "image_crop"
    }

    fn description(&self) -> &'static str {
        "裁剪图片的一块矩形区域并另存。坐标从左上角 (0,0) 起算，超出源图范围会被拒绝而不是自动裁掉。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "源图片路径" },
                "x": { "type": "integer", "minimum": 0, "description": "左上角 x" },
                "y": { "type": "integer", "minimum": 0, "description": "左上角 y" },
                "width": { "type": "integer", "minimum": 1, "description": "裁剪宽度" },
                "height": { "type": "integer", "minimum": 1, "description": "裁剪高度" },
                "output": { "type": "string", "description": "输出路径（可选）" },
                "overwrite": { "type": "boolean", "description": "允许覆盖（默认 false）" }
            },
            "required": ["path", "x", "y", "width", "height"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let path = args["path"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;
        if !std::path::Path::new(path).is_file() {
            return Ok(ToolResult::err("FILE_NOT_FOUND", &format!("文件不存在：{}", path)));
        }

        let x = args["x"].as_i64().unwrap_or(0);
        let y = args["y"].as_i64().unwrap_or(0);
        let width = args["width"].as_i64().unwrap_or(0);
        let height = args["height"].as_i64().unwrap_or(0);
        if width <= 0 || height <= 0 || x < 0 || y < 0 {
            return Ok(ToolResult::err(
                "INVALID_PARAMS",
                "x/y 不能为负，width/height 必须大于 0",
            ));
        }

        let (src_width, src_height) = match read_image_size(path) {
            Ok(size) => size,
            Err(e) => return Ok(ToolResult::err("IMAGE_READ_FAILED", &e.to_string())),
        };
        // 越界直接拒：自动夹到边界会悄悄产出"看起来成功了但内容不对"的图
        if x + width > src_width || y + height > src_height {
            return Ok(ToolResult::err(
                "CROP_OUT_OF_BOUNDS",
                &format!(
                    "裁剪区域 ({},{}) {}x{} 超出源图 {}x{}",
                    x, y, width, height, src_width, src_height
                ),
            ));
        }

        let ext = std::path::Path::new(path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "png".to_string());
        let output = resolve_output(
            path,
            args["output"].as_str(),
            &format!("crop_{}x{}_{}_{}", width, height, x, y),
            &ext,
        );
        if let Some(rejection) = output_guard(&output, args["overwrite"].as_bool().unwrap_or(false)) {
            return Ok(rejection);
        }

        let script = r#"
Add-Type -AssemblyName System.Drawing
$src = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__SRC__'))
$dst = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__DST__'))
$img = [System.Drawing.Image]::FromFile($src)
$rect = New-Object System.Drawing.Rectangle(__X__, __Y__, __W__, __H__)
$bmp = $img.Clone($rect, $img.PixelFormat)
__FORMAT__
$bmp.Save($dst, $fmt)
$bmp.Dispose()
$img.Dispose()
@{ saved = $true } | ConvertTo-Json -Compress
"#
        .replace("__SRC__", &literal(path))
        .replace("__DST__", &literal(&output))
        .replace("__X__", &x.to_string())
        .replace("__Y__", &y.to_string())
        .replace("__W__", &width.to_string())
        .replace("__H__", &height.to_string())
        .replace("__FORMAT__", &format_expr("$dst"));

        let (ok, _stdout, stderr) = run_powershell_b64(&script)?;
        if !ok {
            return Ok(ToolResult::err(
                "IMAGE_CROP_FAILED",
                &format!("裁剪失败：{}", stderr.trim()),
            ));
        }

        let evidence = verify_output(&output, width, height)?;
        if evidence["dimensions_match"] != json!(true) {
            return Ok(ToolResult::err(
                "OUTPUT_DIMENSION_MISMATCH",
                &format!(
                    "输出尺寸与预期不符：期望 {}x{}，实际 {}x{}",
                    width, height, evidence["width"], evidence["height"]
                ),
            ));
        }

        Ok(ToolResult::ok(json!({
            "source": path,
            "crop": { "x": x, "y": y, "width": width, "height": height },
            "output": evidence,
        })))
    }
}

// ============================================================
// image_convert
// ============================================================

pub struct ImageConvertTool;

#[async_trait]
impl AgentTool for ImageConvertTool {
    fn name(&self) -> &'static str {
        "image_convert"
    }

    fn description(&self) -> &'static str {
        "转换图片格式（png / jpg / bmp / gif / tiff）。输出格式由 output 的扩展名决定。透明背景转 jpg 会被填黑——需要白底请先说明。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "源图片路径" },
                "output": { "type": "string", "description": "输出路径（扩展名决定格式，必填）" },
                "overwrite": { "type": "boolean", "description": "允许覆盖（默认 false）" }
            },
            "required": ["path", "output"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let path = args["path"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;
        let output = args["output"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("output is required".to_string()))?;

        if !std::path::Path::new(path).is_file() {
            return Ok(ToolResult::err("FILE_NOT_FOUND", &format!("文件不存在：{}", path)));
        }
        if let Some(rejection) = output_guard(output, args["overwrite"].as_bool().unwrap_or(false)) {
            return Ok(rejection);
        }

        let (src_width, src_height) = match read_image_size(path) {
            Ok(size) => size,
            Err(e) => return Ok(ToolResult::err("IMAGE_READ_FAILED", &e.to_string())),
        };

        let script = r#"
Add-Type -AssemblyName System.Drawing
$src = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__SRC__'))
$dst = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__DST__'))
$img = [System.Drawing.Image]::FromFile($src)
__FORMAT__
try {
  $img.Save($dst, $fmt)
} finally {
  $img.Dispose()
}
@{ saved = $true } | ConvertTo-Json -Compress
"#
        .replace("__SRC__", &literal(path))
        .replace("__DST__", &literal(output))
        .replace("__FORMAT__", &format_expr("$dst"));

        let (ok, _stdout, stderr) = run_powershell_b64(&script)?;
        if !ok {
            return Ok(ToolResult::err(
                "IMAGE_CONVERT_FAILED",
                &format!("转换失败：{}", stderr.trim()),
            ));
        }

        // 格式转换不该改变尺寸；尺寸变了说明源被裁/被缩放，这不符合预期
        let evidence = verify_output(output, src_width, src_height)?;

        Ok(ToolResult::ok(json!({
            "source": path,
            "output": evidence,
            "dimensions_preserved": evidence["dimensions_match"],
        })))
    }
}

// ============================================================
// image_rotate
// ============================================================

pub struct ImageRotateTool;

#[async_trait]
impl AgentTool for ImageRotateTool {
    fn name(&self) -> &'static str {
        "image_rotate"
    }

    fn description(&self) -> &'static str {
        "旋转或翻转图片并另存。rotate90/rotate180/rotate270 是顺时针角度；flip_horizontal / flip_vertical 是镜像。旋转 90/270 会交换宽高。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "源图片路径" },
                "operation": {
                    "type": "string",
                    "enum": ["rotate90", "rotate180", "rotate270", "flip_horizontal", "flip_vertical"],
                    "description": "旋转/翻转方式"
                },
                "output": { "type": "string", "description": "输出路径（可选）" },
                "overwrite": { "type": "boolean", "description": "允许覆盖（默认 false）" }
            },
            "required": ["path", "operation"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let path = args["path"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;
        let operation = args["operation"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("operation is required".to_string()))?;

        let rotate_flip = match operation {
            "rotate90" => "Rotate90FlipNone",
            "rotate180" => "Rotate180FlipNone",
            "rotate270" => "Rotate270FlipNone",
            "flip_horizontal" => "RotateNoneFlipX",
            "flip_vertical" => "RotateNoneFlipY",
            other => {
                return Ok(ToolResult::err(
                    "INVALID_PARAMS",
                    &format!("不支持的 operation：{}", other),
                ))
            }
        };

        if !std::path::Path::new(path).is_file() {
            return Ok(ToolResult::err("FILE_NOT_FOUND", &format!("文件不存在：{}", path)));
        }

        let (src_width, src_height) = match read_image_size(path) {
            Ok(size) => size,
            Err(e) => return Ok(ToolResult::err("IMAGE_READ_FAILED", &e.to_string())),
        };
        let swaps = matches!(operation, "rotate90" | "rotate270");
        let (expect_width, expect_height) = if swaps {
            (src_height, src_width)
        } else {
            (src_width, src_height)
        };

        let ext = std::path::Path::new(path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "png".to_string());
        let output = resolve_output(path, args["output"].as_str(), operation, &ext);
        if let Some(rejection) = output_guard(&output, args["overwrite"].as_bool().unwrap_or(false)) {
            return Ok(rejection);
        }

        let script = r#"
Add-Type -AssemblyName System.Drawing
$src = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__SRC__'))
$dst = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__DST__'))
$img = [System.Drawing.Image]::FromFile($src)
$img.RotateFlip([System.Drawing.RotateFlipType]::__OP__)
__FORMAT__
$img.Save($dst, $fmt)
$img.Dispose()
@{ saved = $true } | ConvertTo-Json -Compress
"#
        .replace("__SRC__", &literal(path))
        .replace("__DST__", &literal(&output))
        .replace("__OP__", rotate_flip)
        .replace("__FORMAT__", &format_expr("$dst"));

        let (ok, _stdout, stderr) = run_powershell_b64(&script)?;
        if !ok {
            return Ok(ToolResult::err(
                "IMAGE_ROTATE_FAILED",
                &format!("旋转失败：{}", stderr.trim()),
            ));
        }

        let evidence = verify_output(&output, expect_width, expect_height)?;
        if evidence["dimensions_match"] != json!(true) {
            return Ok(ToolResult::err(
                "OUTPUT_DIMENSION_MISMATCH",
                &format!(
                    "输出尺寸与预期不符：期望 {}x{}，实际 {}x{}",
                    expect_width, expect_height, evidence["width"], evidence["height"]
                ),
            ));
        }

        Ok(ToolResult::ok(json!({
            "source": path,
            "operation": operation,
            "source_size": { "width": src_width, "height": src_height },
            "output": evidence,
        })))
    }
}

// ============================================================
// media_tools_status
// ============================================================

pub struct MediaToolsStatusTool;

#[async_trait]
impl AgentTool for MediaToolsStatusTool {
    fn name(&self) -> &'static str {
        "media_tools_status"
    }

    fn description(&self) -> &'static str {
        "查本机有哪些可用的媒体处理能力：图像处理（系统自带，总是可用）、ffmpeg/ffprobe（装了才有）。用户问「能不能剪视频」「能不能转音频」时先查这个，别直接答能或不能。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: Value) -> AppResult<ToolResult> {
        let ffmpeg = probe_binary("ffmpeg");
        let ffprobe = probe_binary("ffprobe");

        Ok(ToolResult::ok(json!({
            "image": {
                "available": true,
                "engine": "System.Drawing",
                "capabilities": ["info", "resize", "crop", "convert", "rotate", "flip"],
            },
            "video": {
                "available": ffmpeg.is_some(),
                "engine": "ffmpeg",
                "path": ffmpeg,
            },
            "audio": {
                "available": ffmpeg.is_some(),
                "engine": "ffmpeg",
                "path": ffmpeg,
            },
            "probe": {
                "available": ffprobe.is_some(),
                "path": ffprobe,
            },
            "note": if ffmpeg.is_some() {
                "ffmpeg 可用；音视频裁剪/转码可以走命令行"
            } else {
                "本机没有 ffmpeg，音视频处理不可用。需要的话请用户自己安装，助手不会静默下载几十兆的二进制"
            },
        })))
    }
}

/// 探测可执行文件是否在 PATH 上。
fn probe_binary(name: &str) -> Option<String> {
    let mut command = std::process::Command::new(name);
    command.arg("-version");
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    match command.status() {
        Ok(status) if status.success() => Some(name.to_string()),
        _ => None,
    }
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn tiny_path(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("pcg-media-{}-{}.png", tag, std::process::id()))
            .to_string_lossy()
            .to_string()
    }

    /// 造一张纯色 PNG（最小合法文件），用于走真实链路。
    fn write_png(path: &str, width: u32, height: u32) {
        // 1x1 的 PNG 是固定字节；缩放/裁剪测试不需要真实像素内容，
        // 但尺寸必须是真的，所以用 System.Drawing 生成。
        let script = r#"
Add-Type -AssemblyName System.Drawing
$dst = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__DST__'))
$bmp = New-Object System.Drawing.Bitmap(__W__, __H__)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.Clear([System.Drawing.Color]::CornflowerBlue)
$g.Dispose()
$bmp.Save($dst, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
'ok'
"#
        .replace("__DST__", &literal(path))
        .replace("__W__", &width.to_string())
        .replace("__H__", &height.to_string());

        let (ok, _out, err) = run_powershell_b64(&script).expect("生成测试图");
        assert!(ok, "生成测试图失败: {}", err);
        assert!(std::path::Path::new(path).is_file());
    }

    #[test]
    fn output_default_keeps_directory_and_extension() {
        let resolved = resolve_output("C:/imgs/a.jpg", None, "800x600", "jpg");
        assert!(resolved.ends_with("a_800x600.jpg"), "实际：{}", resolved);
        assert!(resolved.contains("imgs"));
    }

    #[test]
    fn existing_output_is_rejected_without_overwrite() {
        let path = tiny_path("guard");
        std::fs::write(&path, b"x").unwrap();
        assert!(output_guard(&path, false).is_some(), "已存在时必须拒绝");
        assert!(output_guard(&path, true).is_none(), "显式覆盖应放行");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn base64_literal_is_url_safe_for_paths_with_spaces() {
        // 路径里带空格/中文时，只有 base64 传参才不会被命令行拆开
        let encoded = literal("C:\\测试 目录\\图 1.png");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("可解码");
        assert_eq!(
            String::from_utf8(decoded).unwrap(),
            "C:\\测试 目录\\图 1.png"
        );
    }

    /// 实机：缩放 + 裁剪 + 转换 + 旋转，全部以"重开输出文件读尺寸"为判据。
    #[tokio::test]
    #[ignore = "需要真实 System.Drawing，手动跑"]
    async fn real_image_pipeline_round_trip() {
        let source = tiny_path("src");
        write_png(&source, 400, 200);

        let resized_out = tiny_path("resized");
        let resized = ImageResizeTool
            .execute(json!({
                "path": source,
                "output": resized_out,
                "width": 100
            }))
            .await
            .expect("resize");
        assert!(resized.success, "{:?}", resized.error);
        let data = resized.data.expect("resize 数据");
        // 只给宽度，高度应按 400:200 折半
        assert_eq!(data["output"]["width"], 100);
        assert_eq!(data["output"]["height"], 50);

        let crop_out = tiny_path("crop");
        let cropped = ImageCropTool
            .execute(json!({
                "path": source,
                "x": 10, "y": 10, "width": 120, "height": 60,
                "output": crop_out
            }))
            .await
            .expect("crop");
        assert!(cropped.success, "{:?}", cropped.error);
        assert_eq!(cropped.data.expect("crop 数据")["output"]["width"], 120);

        // 越界裁剪必须被拒，而不是悄悄裁到边界
        let oob = ImageCropTool
            .execute(json!({
                "path": source,
                "x": 350, "y": 0, "width": 200, "height": 50,
                "output": tiny_path("oob")
            }))
            .await
            .expect("crop oob");
        assert!(!oob.success);
        assert_eq!(oob.error_code.as_deref(), Some("CROP_OUT_OF_BOUNDS"));

        let jpg_out = tiny_path("converted").replace(".png", ".jpg");
        let converted = ImageConvertTool
            .execute(json!({ "path": source, "output": jpg_out.clone() }))
            .await
            .expect("convert");
        assert!(converted.success, "{:?}", converted.error);
        assert_eq!(
            converted.data.expect("convert 数据")["dimensions_preserved"],
            true,
            "格式转换不该改变尺寸"
        );

        let rot_out = tiny_path("rotated");
        let rotated = ImageRotateTool
            .execute(json!({
                "path": source,
                "operation": "rotate90",
                "output": rot_out
            }))
            .await
            .expect("rotate");
        assert!(rotated.success, "{:?}", rotated.error);
        let rdata = rotated.data.expect("rotate 数据");
        assert_eq!(rdata["output"]["width"], 200, "旋转 90 度宽高应互换");
        assert_eq!(rdata["output"]["height"], 400);

        for path in [source, resized_out, crop_out, jpg_out, rot_out] {
            let _ = std::fs::remove_file(path);
        }
    }
}
