//! PDF 家族：读取、抽取文本、信息、合并、生成、转换
//!
//! 两条技术路线，按"要不要写汉字"分工：
//!
//! - **读 / 合并**走 `lopdf`（纯 Rust，不依赖任何外部程序）。读文本依赖 PDF 里
//!   自带的 ToUnicode 映射表；扫描件和图片型 PDF 天生抽不出文字，只能走 OCR。
//! - **写走 headless 浏览器**，不自己拼 PDF 字节。原因很实际：PDF 里的
//!   Helvetica 等标准字体只有 Latin 字形，要写中文就得嵌入 TrueType 子集、
//!   自己生成 CIDToGIDMap 和 widths 数组——四百行代码只为了一行中文。
//!   交给系统里本来就有的 Edge/Chrome 排版打印，中文、表格、分页、CSS 全免费。
//!
//! 浏览器 headless 打印有个必须处理的坑：**如果用户已经开着 Edge，新起的
//! 进程会把命令行转交给已有实例然后立刻退出**，`--print-to-pdf` 根本不会执行，
//! 表现为"命令成功但文件不存在"。所以每次都指定独立的 `--user-data-dir`。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::executors::os_extra::{literal, run_powershell};
use crate::tools::registry::{AgentTool, RiskLevel, ToolResult};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 文本抽取上限，防止几百页的 PDF 把上下文撑爆。
const MAX_TEXT_CHARS: usize = 80_000;

fn require_str<'a>(args: &'a Value, key: &str) -> AppResult<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::InvalidArgument(format!("{} is required", key)))
}

fn ensure_ext(path: &str, expect: &str) -> AppResult<()> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if ext == expect {
        Ok(())
    } else {
        Err(AppError::InvalidArgument(format!(
            "扩展名应为 .{}，实际: {}",
            expect,
            if ext.is_empty() { "(无)" } else { &ext }
        )))
    }
}

fn load_pdf(path: &str) -> AppResult<lopdf::Document> {
    if !Path::new(path).is_file() {
        return Err(AppError::InvalidArgument(format!("文件不存在: {}", path)));
    }
    lopdf::Document::load(path).map_err(|e| {
        AppError::ToolExecution(format!(
            "无法解析 PDF（可能已加密或文件损坏）: {}",
            e
        ))
    })
}

/// 把 "1-5,8,10-" 这类页码范围解析成具体页号列表。
///
/// 范围写法支持三种：单页 `3`、闭区间 `2-6`、开区间 `5-`（到末页）。
fn parse_pages(spec: &str, total: u32) -> AppResult<Vec<u32>> {
    let mut pages: Vec<u32> = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            let start: u32 = a.trim().parse().map_err(|_| {
                AppError::InvalidArgument(format!("页码格式不对: {}", part))
            })?;
            let end: u32 = if b.trim().is_empty() {
                total
            } else {
                b.trim().parse().map_err(|_| {
                    AppError::InvalidArgument(format!("页码格式不对: {}", part))
                })?
            };
            if start == 0 || end == 0 {
                return Err(AppError::InvalidArgument(
                    "页码从 1 开始，不能是 0".to_string(),
                ));
            }
            for p in start..=end.min(total).max(start.min(total)) {
                if p >= 1 && p <= total {
                    pages.push(p);
                }
            }
        } else {
            let p: u32 = part.parse().map_err(|_| {
                AppError::InvalidArgument(format!("页码格式不对: {}", part))
            })?;
            if p == 0 {
                return Err(AppError::InvalidArgument(
                    "页码从 1 开始，不能是 0".to_string(),
                ));
            }
            if p <= total {
                pages.push(p);
            }
        }
    }
    pages.sort_unstable();
    pages.dedup();
    if pages.is_empty() {
        return Err(AppError::InvalidArgument(format!(
            "页码范围没有命中任何页（文档共 {} 页）",
            total
        )));
    }
    Ok(pages)
}

/// 取页面尺寸（pt）。取不到就返回 None，不编造默认值。
fn page_size(doc: &lopdf::Document, page_id: lopdf::ObjectId) -> Option<(f32, f32)> {
    let dict = doc.get_dictionary(page_id).ok()?;
    let media = dict.get(b"MediaBox").ok()?;
    let media = doc.dereference(media).ok()?.1;
    let arr = media.as_array().ok()?;
    if arr.len() < 4 {
        return None;
    }
    let nums: Vec<f32> = arr
        .iter()
        .filter_map(|o| match o {
            lopdf::Object::Integer(i) => Some(*i as f32),
            lopdf::Object::Real(r) => Some(*r),
            _ => None,
        })
        .collect();
    if nums.len() < 4 {
        return None;
    }
    Some((
        (nums[2] - nums[0]).abs(),
        (nums[3] - nums[1]).abs(),
    ))
}

// ============================================================
// read_pdf / pdf_to_text
// ============================================================

/// 读取 PDF 文本
pub struct ReadPdfTool;

#[async_trait]
impl AgentTool for ReadPdfTool {
    fn name(&self) -> &'static str {
        "read_pdf"
    }

    fn description(&self) -> &'static str {
        "读取 PDF 文件里的文字内容，返回按页分组的文本与总页数。适用于文字型 PDF；扫描件、拍照转的 PDF 里没有文字层，抽出来会是空的，那种需要 ocr_image_file。pages 可指定页码范围，如 \"1-5,9\"。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "PDF 文件路径" },
                "pages": { "type": "string", "description": "页码范围，如 \"1-5,9\"；省略表示全部" },
                "max_chars": { "type": "integer", "description": "最多返回多少字符，默认 40000" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, "pdf")?;
        let doc = load_pdf(path)?;

        let total = doc.get_pages().len() as u32;
        let wanted = match args["pages"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(spec) => parse_pages(spec, total)?,
            None => (1..=total).collect(),
        };
        let max_chars = args["max_chars"]
            .as_u64()
            .unwrap_or(40_000)
            .min(MAX_TEXT_CHARS as u64) as usize;

        // extract_text 一次处理多页，比逐页调用快一个量级
        let raw = doc.extract_text(&wanted).unwrap_or_default();
        let mut used = 0usize;
        let mut pages_out: Vec<Value> = Vec::new();
        let mut truncated = false;

        for page in &wanted {
            let page_doc = {
                // 逐页取是为了给出页码边界：整篇抽出来的文本里没有分页标记，
                // 模型引用时说不清"这句话在第几页"。
                let single = doc.clone();
                match single.extract_text(&[*page]) {
                    Ok(t) => t,
                    Err(_) => String::new(),
                }
            };
            let chars = page_doc.chars().count();
            if used + chars > max_chars {
                let remain = max_chars.saturating_sub(used);
                let cut: String = page_doc.chars().take(remain).collect();
                if !cut.trim().is_empty() {
                    pages_out.push(json!({ "page": page, "text": cut.trim() }));
                }
                truncated = true;
                break;
            }
            used += chars;
            pages_out.push(json!({ "page": page, "text": page_doc.trim() }));
        }

        let has_text = pages_out
            .iter()
            .any(|p| p["text"].as_str().map(|t| !t.is_empty()).unwrap_or(false));

        Ok(ToolResult::ok(json!({
            "path": path,
            "total_pages": total,
            "pages_read": pages_out.len(),
            "chars": used,
            "truncated": truncated,
            "has_text_layer": has_text,
            "hint": if has_text {
                Value::Null
            } else {
                json!("这份 PDF 没有可抽取的文字层（扫描件/图片型）。请改用截图或 ocr_image_file 识别。")
            },
            "pages": pages_out,
            "text_preview_chars": raw.chars().count(),
        })))
    }
}

/// 抽取纯文本
pub struct PdfToTextTool;

#[async_trait]
impl AgentTool for PdfToTextTool {
    fn name(&self) -> &'static str {
        "pdf_to_text"
    }

    fn description(&self) -> &'static str {
        "把 PDF 的文字提取出来写成 .txt 纯文本文件（可指定页码范围）。需要把整份 PDF 内容落到文件里、或交给别的工具处理时用它；只想当场看内容用 read_pdf。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "源 PDF 路径" },
                "output": { "type": "string", "description": "输出的 .txt 路径；省略时与源文件同目录同名" },
                "pages": { "type": "string", "description": "页码范围，如 \"1-10\"；省略表示全部" },
                "keep_page_breaks": { "type": "boolean", "description": "是否保留分页标记（--- 第 N 页 ---），默认 true" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, "pdf")?;
        let doc = load_pdf(path)?;
        let total = doc.get_pages().len() as u32;
        let wanted = match args["pages"].as_str().filter(|s| !s.trim().is_empty()) {
            Some(spec) => parse_pages(spec, total)?,
            None => (1..=total).collect(),
        };

        let keep_breaks = args["keep_page_breaks"].as_bool().unwrap_or(true);
        let mut out = String::new();
        for page in &wanted {
            let single = doc.clone();
            let text = single.extract_text(&[*page]).unwrap_or_default();
            if keep_breaks {
                out.push_str(&format!("--- 第 {} 页 ---\n", page));
            }
            out.push_str(text.trim());
            out.push_str("\n\n");
        }

        let output = args["output"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let p = Path::new(path);
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
                let dir = p.parent().unwrap_or(Path::new("."));
                dir.join(format!("{}.txt", stem)).display().to_string()
            });
        ensure_ext(&output, "txt")?;

        std::fs::write(&output, &out)?;
        let written = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
        let chars = out.chars().count();

        Ok(ToolResult::ok(json!({
            "source": path,
            "output": output,
            "pages": wanted.len(),
            "chars": chars,
            "bytes": written,
            "empty": chars < 10,
        })))
    }
}

// ============================================================
// pdf_info
// ============================================================

/// PDF 结构信息
pub struct PdfInfoTool;

#[async_trait]
impl AgentTool for PdfInfoTool {
    fn name(&self) -> &'static str {
        "pdf_info"
    }

    fn description(&self) -> &'static str {
        "查看 PDF 的基本信息：页数、PDF 版本、是否加密、每页尺寸（pt）、文件大小。处理 PDF 之前先看一眼，能提前发现加密/超页数这类问题。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "PDF 文件路径" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, "pdf")?;
        let doc = load_pdf(path)?;

        let pages = doc.get_pages();
        let sizes: Vec<Value> = pages
            .iter()
            .map(|(no, id)| {
                let (w, h) = page_size(&doc, *id).unwrap_or((0.0, 0.0));
                json!({
                    "page": no,
                    "width_pt": (w * 10.0).round() / 10.0,
                    "height_pt": (h * 10.0).round() / 10.0,
                    // 595x842 是 A4，612x792 是 Letter
                    "paper": if (w - 595.0).abs() < 6.0 && (h - 842.0).abs() < 6.0 {
                        "A4"
                    } else if (w - 612.0).abs() < 6.0 && (h - 792.0).abs() < 6.0 {
                        "Letter"
                    } else {
                        "custom"
                    },
                })
            })
            .collect();

        let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        Ok(ToolResult::ok(json!({
            "path": path,
            "pdf_version": doc.version.clone(),
            "page_count": pages.len(),
            "encrypted": doc.is_encrypted(),
            "size_bytes": bytes,
            "pages": sizes,
        })))
    }
}

// ============================================================
// merge_pdf
// ============================================================

/// 合并多个 PDF
pub struct MergePdfTool;

#[async_trait]
impl AgentTool for MergePdfTool {
    fn name(&self) -> &'static str {
        "merge_pdf"
    }

    fn description(&self) -> &'static str {
        "按给定顺序把多个 PDF 合并成一个新文件。源文件不会被修改。输入文件必须都未加密。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "inputs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "要合并的 PDF 路径列表，按数组顺序拼接（至少 2 个）"
                },
                "output": { "type": "string", "description": "输出的 PDF 路径" }
            },
            "required": ["inputs", "output"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let inputs: Vec<String> = args["inputs"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
        if inputs.len() < 2 {
            return Err(AppError::InvalidArgument(
                "至少需要 2 个输入文件".to_string(),
            ));
        }
        let output = require_str(&args, "output")?;
        ensure_ext(output, "pdf")?;
        // 输出路径不能和任一输入重合：写进去就把源文件覆盖了
        for input in &inputs {
            if Path::new(input) == Path::new(output) {
                return Err(AppError::InvalidArgument(
                    "输出路径不能与输入文件相同".to_string(),
                ));
            }
        }

        let mut target = lopdf::Document::with_version("1.5");
        let mut max_id: u32 = 1;
        let mut page_ids: Vec<lopdf::ObjectId> = Vec::new();

        for input in &inputs {
            ensure_ext(input, "pdf")?;
            let mut doc = load_pdf(input)?;
            if doc.is_encrypted() {
                return Ok(ToolResult::err(
                    "PDF_ENCRYPTED",
                    &format!("{} 已加密，无法合并。请先解除加密。", input),
                ));
            }
            doc.renumber_objects_with(max_id);
            max_id = doc.max_id + 1;
            page_ids.extend(doc.get_pages().values().copied());
            target.objects.extend(doc.objects);
        }

        // 新建 Catalog 与 Pages 树：源文档各自的根节点不能直接复用，
        // 否则合并结果里会有多个 Catalog，阅读器只认第一个。
        //
        // 这里手工构造 Dictionary 而不用 `dictionary!` 宏：lopdf 0.34 的宏在
        // 带尾随逗号时会递归调用未限定路径的 `dictionary!`，在调用方作用域里
        // 直接编译失败（宏自身的 bug）。
        let pages_id = (max_id, 0u16);
        let catalog_id = (max_id + 1, 0u16);

        let mut catalog = lopdf::Dictionary::new();
        catalog.set("Type", "Catalog");
        catalog.set("Pages", pages_id);
        target
            .objects
            .insert(catalog_id, lopdf::Object::Dictionary(catalog));

        let kids: Vec<lopdf::Object> = page_ids
            .iter()
            .map(|id| lopdf::Object::Reference(*id))
            .collect();
        let count = kids.len() as i64;
        let mut pages_node = lopdf::Dictionary::new();
        pages_node.set("Type", "Pages");
        pages_node.set("Kids", lopdf::Object::Array(kids));
        pages_node.set("Count", count);
        target
            .objects
            .insert(pages_id, lopdf::Object::Dictionary(pages_node));
        target.trailer.set("Root", catalog_id);
        // max_id 必须跟上，否则 save 时编号会重排错乱
        target.max_id = max_id + 1;
        target.compress();

        target
            .save(output)
            .map_err(|e| AppError::ToolExecution(format!("写入合并后的 PDF 失败: {}", e)))?;

        let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
        // 落盘校验：save 返回 Ok 但文件为 0 字节，说明写流中途断了
        if bytes == 0 {
            return Ok(ToolResult::err(
                "PDF_MERGE_EMPTY",
                "合并后文件为 0 字节，写入失败",
            ));
        }

        Ok(ToolResult::ok(json!({
            "output": output,
            "inputs": inputs,
            "merged_files": inputs.len(),
            "total_pages": page_ids.len(),
            "size_bytes": bytes,
        })))
    }
}

// ============================================================
// split_pdf —— 抽取指定页码范围到新文件
// ============================================================

pub struct SplitPdfTool;

#[async_trait]
impl AgentTool for SplitPdfTool {
    fn name(&self) -> &'static str {
        "split_pdf"
    }

    fn description(&self) -> &'static str {
        "从 PDF 里抽取指定页码范围，另存为新文件（源文件不改动）。页码从 1 开始，支持 1-5,8,10- 这类写法。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "input": { "type": "string", "description": "源 PDF 路径" },
                "output": { "type": "string", "description": "输出 PDF 路径" },
                "pages": { "type": "string", "description": "页码范围，如 1-3 或 2,4,6-8" }
            },
            "required": ["input", "output", "pages"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let input = require_str(&args, "input")?;
        let output = require_str(&args, "output")?;
        let pages_spec = require_str(&args, "pages")?;
        ensure_ext(input, "pdf")?;
        ensure_ext(output, "pdf")?;
        if Path::new(input) == Path::new(output) {
            return Err(AppError::InvalidArgument("输出路径不能与输入文件相同".to_string()));
        }

        let mut doc = load_pdf(input)?;
        if doc.is_encrypted() {
            return Ok(ToolResult::err("PDF_ENCRYPTED", "源文件已加密，无法拆分。请先解除加密。"));
        }

        // get_pages 返回的是 (ObjectId -> page ObjectId) 的映射，按 ObjectId 排序即为页序。
        let all_pages: Vec<lopdf::ObjectId> = doc.get_pages().values().copied().collect();
        let total = all_pages.len() as u32;
        if total == 0 {
            return Ok(ToolResult::err("PDF_NO_PAGES", "源文件没有页面"));
        }
        let page_nums = parse_pages(pages_spec, total)?;
        if page_nums.is_empty() {
            return Ok(ToolResult::err("PDF_NO_PAGES", "页码范围没有匹配到任何页"));
        }
        let selected: Vec<lopdf::ObjectId> = page_nums.iter().map(|p| all_pages[(*p - 1) as usize]).collect();

        // 手术式改 Pages 树：保留整个 objects（含字体/图片等共享资源），
        // 只把根 Pages 节点的 Kids / Count 替换成选中的页。
        let root_id = doc
            .trailer
            .get(b"Root")
            .map_err(|_| AppError::ToolExecution("PDF 缺少 Root".into()))?
            .as_reference()
            .map_err(|_| AppError::ToolExecution("Root 不是引用".into()))?;
        let catalog = doc
            .get_object(root_id)
            .map_err(|_| AppError::ToolExecution("找不到 Catalog".into()))?;
        let pages_id = catalog
            .as_dict()
            .map_err(|_| AppError::ToolExecution("Catalog 不是字典".into()))?
            .get(b"Pages")
            .map_err(|_| AppError::ToolExecution("Catalog 缺少 Pages".into()))?
            .as_reference()
            .map_err(|_| AppError::ToolExecution("Pages 不是引用".into()))?;

        let mut pages_dict = doc
            .get_object(pages_id)
            .map_err(|_| AppError::ToolExecution("找不到 Pages 节点".into()))?
            .as_dict()
            .map_err(|_| AppError::ToolExecution("Pages 不是字典".into()))?
            .clone();
        let kids: Vec<lopdf::Object> = selected.iter().map(|id| lopdf::Object::Reference(*id)).collect();
        pages_dict.set("Kids", lopdf::Object::Array(kids));
        pages_dict.set("Count", selected.len() as i64);
        doc.objects.insert(pages_id, lopdf::Object::Dictionary(pages_dict));

        doc.compress();
        doc.save(output)
            .map_err(|e| AppError::ToolExecution(format!("写入拆分后的 PDF 失败: {}", e)))?;

        let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
        if bytes == 0 {
            return Ok(ToolResult::err("PDF_SPLIT_EMPTY", "拆分后文件为 0 字节，写入失败"));
        }

        Ok(ToolResult::ok(json!({
            "output": output,
            "source_pages": total,
            "extracted_pages": selected.len(),
            "size_bytes": bytes,
        })))
    }
}

// ============================================================
// 浏览器探测（create_pdf / convert_to_pdf 共用）
// ============================================================

/// 找一个能 headless 打印的 Chromium 内核浏览器。
///
/// 优先 Edge：Windows 10/11 自带，不需要用户另外装东西。
fn find_browser() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let env = |k: &str| std::env::var(k).ok();

    let roots: Vec<String> = [
        env("ProgramFiles"),
        env("ProgramFiles(x86)"),
        env("LOCALAPPDATA"),
    ]
    .into_iter()
    .flatten()
    .collect();

    for root in &roots {
        candidates.push(Path::new(root).join(r"Microsoft\Edge\Application\msedge.exe"));
        candidates.push(Path::new(root).join(r"Google\Chrome\Application\chrome.exe"));
    }
    // 只装在 32 位路径下的 Edge（少见但要兜住）
    candidates.push(PathBuf::from(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"));
    candidates.push(PathBuf::from(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"));

    candidates.into_iter().find(|p| p.is_file())
}

/// HTML 转义（只用于把用户文本塞进 <pre>）
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 默认样式：A4、中文字体、合理行距。
///
/// 不写这个的话，浏览器默认是用英文字体 Times New Roman 打中文，
/// 字距会散得很难看。
const BASE_CSS: &str = r#"@page { size: A4; margin: 18mm 16mm; }
body { font-family: "Microsoft YaHei", "PingFang SC", "Noto Sans CJK SC", sans-serif;
       font-size: 12pt; line-height: 1.7; color: #111; }
h1 { font-size: 20pt; margin: 0 0 12pt; }
h2 { font-size: 16pt; margin: 16pt 0 8pt; }
pre { white-space: pre-wrap; word-wrap: break-word; font-family: inherit; margin: 0; }
table { border-collapse: collapse; width: 100%; }
th, td { border: 1px solid #999; padding: 4pt 6pt; text-align: left; }
img { max-width: 100%; }"#;

/// 用 headless 浏览器把 HTML 打印成 PDF。
fn html_to_pdf(browser: &Path, html: &str, output: &str, landscape: bool) -> AppResult<()> {
    // 临时目录：每次一个新目录，既是 --user-data-dir 也是 html 的落点
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let temp_dir = std::env::temp_dir().join(format!("pcg-pdf-{}-{}", std::process::id(), stamp));
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| AppError::ToolExecution(format!("无法创建临时目录: {}", e)))?;

    let html_path = temp_dir.join("doc.html");
    {
        let mut f = std::fs::File::create(&html_path)
            .map_err(|e| AppError::ToolExecution(format!("无法写临时 HTML: {}", e)))?;
        f.write_all(html.as_bytes())?;
    }

    let output_abs = if Path::new(output).is_absolute() {
        PathBuf::from(output)
    } else {
        std::env::current_dir()
            .map(|d| d.join(output))
            .unwrap_or_else(|_| PathBuf::from(output))
    };
    if let Some(parent) = output_abs.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    // file:/// 需要正斜杠；路径里的空格要转成 %20，否则 Edge 会把空格后
    // 半截当参数丢掉。
    let url = format!(
        "file:///{}",
        html_path
            .to_string_lossy()
            .replace('\\', "/")
            .replace(' ', "%20")
    );

    // 先渲染到临时目录，成功后再搬到目标路径。
    //
    // 直接写目标路径有两个问题：① 目标已存在旧文件时，轮询会看到上一次的
    // 产物而误判成功；② 渲染失败时会把用户原有的文件删掉/写坏。
    let render_path = temp_dir.join("render.pdf");
    let print_flag = format!("--print-to-pdf={}", render_path.to_string_lossy());
    let profile_flag = format!("--user-data-dir={}", temp_dir.join("profile").to_string_lossy());

    let mut cmd = Command::new(browser);
    cmd.args([
        "--headless=new",
        "--disable-gpu",
        "--no-sandbox",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        "--disable-background-networking",
        "--virtual-time-budget=3000", // 等页面 JS/字体加载完
        "--no-pdf-header-footer",
    ])
    .arg(&profile_flag)
    .arg(&print_flag);
    if landscape {
        cmd.arg("--print-to-pdf-landscape");
    }
    cmd.arg(&url);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let result = cmd.output();

    // 关键：浏览器**不是在退出前写完 PDF 的**。
    // headless Edge 的主进程把渲染任务交给子进程后立刻返回（退出码 0），
    // 文件要再过几百毫秒才出现。只看退出码会得到"成功但文件不存在"，
    // 只看一次 metadata 会得到"文件不存在→判定失败"。
    // 正确做法是轮询等文件出现并稳定下来。
    let mut waited = std::time::Duration::ZERO;
    let step = std::time::Duration::from_millis(120);
    let limit = std::time::Duration::from_secs(25);
    let mut last_size = u64::MAX;
    let mut stable_rounds = 0;
    let mut size_now = 0u64;
    while waited < limit {
        if let Ok(meta) = std::fs::metadata(&render_path) {
            size_now = meta.len();
            if size_now > 0 && size_now == last_size {
                stable_rounds += 1;
                // 连续 3 轮（约 360ms）大小不变，认为写完了
                if stable_rounds >= 3 {
                    break;
                }
            } else {
                last_size = size_now;
                stable_rounds = 0;
            }
        }
        std::thread::sleep(step);
        waited += step;
    }

    let cleanup = || {
        let _ = std::fs::remove_dir_all(&temp_dir);
    };

    if let Err(e) = result {
        cleanup();
        return Err(AppError::ToolExecution(format!(
            "无法启动浏览器做 PDF 渲染: {}",
            e
        )));
    }
    let out = result.unwrap();
    let code = out.status.code().unwrap_or(-1);
    if !render_path.is_file() || size_now == 0 {
        let stderr = String::from_utf8_lossy(&out.stderr);
        cleanup();
        return Err(AppError::ToolExecution(format!(
            "浏览器没有生成 PDF（退出码 {}，等待 {}ms）: {}",
            code,
            waited.as_millis(),
            stderr.chars().take(400).collect::<String>()
        )));
    }

    // 文件头校验：有些浏览器在渲染失败时会写出一个 0 页的占位文件，
    // 光看大小看不出来，必须确认它真的是 PDF。
    let bytes = std::fs::read(&render_path).unwrap_or_default();
    if bytes.len() < 5 || &bytes[..5] != b"%PDF-" {
        cleanup();
        return Err(AppError::ToolExecution(
            "生成的文件不是 PDF（文件头校验失败）".to_string(),
        ));
    }

    // 搬到位：跨盘时 rename 会失败，直接 copy（PDF 通常几 MB，代价可接受）
    if let Err(e) = std::fs::copy(&render_path, &output_abs) {
        cleanup();
        return Err(AppError::ToolExecution(format!(
            "无法把生成的 PDF 写到 {}: {}",
            output_abs.display(),
            e
        )));
    }
    cleanup();
    Ok(())
}

// ============================================================
// create_pdf
// ============================================================

/// 从 HTML 或纯文本生成 PDF
pub struct CreatePdfTool;

#[async_trait]
impl AgentTool for CreatePdfTool {
    fn name(&self) -> &'static str {
        "create_pdf"
    }

    fn description(&self) -> &'static str {
        "把 HTML 或纯文本生成为 PDF 文件（用系统自带的 Edge/Chrome 排版打印，中文、表格、图片、分页都正常）。html 参数按原样渲染，适合要排版效果的场景；text 参数按段落渲染，适合纯文字。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "输出的 .pdf 路径" },
                "html": { "type": "string", "description": "完整 HTML 内容（与 text 二选一）" },
                "text": { "type": "string", "description": "纯文本，按换行分段（与 html 二选一）" },
                "title": { "type": "string", "description": "文档标题，会作为一级标题显示在开头（仅 text 模式）" },
                "landscape": { "type": "boolean", "description": "是否横向，默认 false" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, "pdf")?;

        let body = if let Some(html) = args["html"].as_str().filter(|s| !s.trim().is_empty()) {
            html.to_string()
        } else if let Some(text) = args["text"].as_str().filter(|s| !s.trim().is_empty()) {
            let title = args["title"].as_str().unwrap_or("");
            let heading = if title.trim().is_empty() {
                String::new()
            } else {
                format!("<h1>{}</h1>", escape_html(title))
            };
            format!(
                "<h1 style=\"display:none\"></h1>{}<pre>{}</pre>",
                heading,
                escape_html(text)
            )
        } else {
            return Err(AppError::InvalidArgument(
                "html 或 text 至少提供一个".to_string(),
            ));
        };

        let browser = find_browser().ok_or_else(|| {
            AppError::ToolExecution(
                "未找到 Edge 或 Chrome。生成 PDF 需要 Chromium 内核浏览器做排版。".to_string(),
            )
        })?;

        let doc = format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>{}</style></head><body>{}</body></html>",
            BASE_CSS, body
        );
        html_to_pdf(
            &browser,
            &doc,
            path,
            args["landscape"].as_bool().unwrap_or(false),
        )?;

        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        // 回读页数：这是"PDF 真的能用"的硬证据，只报文件大小连是不是
        // 空文档都看不出来。
        let pages = lopdf::Document::load(path)
            .map(|d| d.get_pages().len())
            .unwrap_or(0);

        Ok(ToolResult::ok(json!({
            "path": path,
            "size_bytes": size,
            "pages": pages,
            "renderer": browser.to_string_lossy(),
        })))
    }
}

// ============================================================
// convert_to_pdf
// ============================================================

/// 把办公文档/图片/文本转成 PDF
pub struct ConvertToPdfTool;

#[async_trait]
impl AgentTool for ConvertToPdfTool {
    fn name(&self) -> &'static str {
        "convert_to_pdf"
    }

    fn description(&self) -> &'static str {
        "把已有文件转换成 PDF。docx/xlsx/pptx 优先调用本机 Office 导出（保真度最高），没有 Office 时回退 headless 浏览器渲染；txt/md/html/图片直接用浏览器渲染。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "源文件路径（docx/xlsx/pptx/txt/md/html/图片）" },
                "output": { "type": "string", "description": "输出的 .pdf 路径；省略时与源文件同目录同名" },
                "landscape": { "type": "boolean", "description": "是否横向，默认 false" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let src = require_str(&args, "path")?;
        if !Path::new(src).is_file() {
            return Err(AppError::InvalidArgument(format!("文件不存在: {}", src)));
        }
        let ext = Path::new(src)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let output = args["output"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                let p = Path::new(src);
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
                p.parent()
                    .unwrap_or(Path::new("."))
                    .join(format!("{}.pdf", stem))
                    .display()
                    .to_string()
            });
        ensure_ext(&output, "pdf")?;
        if Path::new(&output) == Path::new(src) {
            return Err(AppError::InvalidArgument(
                "输出路径不能与源文件相同".to_string(),
            ));
        }

        let landscape = args["landscape"].as_bool().unwrap_or(false);

        // ---- Office 文档：先试本机 Office 导出 ----
        if matches!(ext.as_str(), "docx" | "xlsx" | "pptx") {
            match office_export_pdf(src, &output, &ext) {
                Ok(true) => {
                    let pages = lopdf::Document::load(&output)
                        .map(|d| d.get_pages().len())
                        .unwrap_or(0);
                    return Ok(ToolResult::ok(json!({
                        "source": src,
                        "output": output,
                        "engine": "office",
                        "pages": pages,
                        "size_bytes": std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0),
                    })));
                }
                Ok(false) => { /* 没装 Office，往下走浏览器 */ }
                Err(e) => return Err(e),
            }
        }

        // ---- 其余：交给浏览器渲染 ----
        let browser = find_browser().ok_or_else(|| {
            AppError::ToolExecution(
                "未找到 Edge 或 Chrome，无法渲染 PDF。docx/xlsx/pptx 需要本机安装 Office。"
                    .to_string(),
            )
        })?;

        let html = match ext.as_str() {
            "html" | "htm" => std::fs::read_to_string(src)?,
            "md" | "markdown" => md_to_html(&std::fs::read_to_string(src)?),
            "txt" | "log" | "csv" | "json" => {
                let text = std::fs::read_to_string(src)
                    .map_err(|e| AppError::ToolExecution(format!("读取文本失败: {}", e)))?;
                format!("<pre>{}</pre>", escape_html(&text))
            }
            "png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp" => {
                // 图片按页面宽度铺开并居中
                let file_url = format!(
                    "file:///{}",
                    Path::new(src)
                        .canonicalize()
                        .unwrap_or_else(|_| PathBuf::from(src))
                        .to_string_lossy()
                        .replace('\\', "/")
                        .replace(' ', "%20")
                );
                format!(
                    "<div style=\"text-align:center\"><img src=\"{}\" style=\"max-width:100%;\"></div>",
                    file_url
                )
            }
            other => {
                return Err(AppError::InvalidArgument(format!(
                    "不支持转换为 PDF 的文件类型: .{}",
                    other
                )))
            }
        };

        let doc = format!(
            "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>{}</style></head><body>{}</body></html>",
            BASE_CSS, html
        );
        html_to_pdf(&browser, &doc, &output, landscape)?;

        let pages = lopdf::Document::load(&output)
            .map(|d| d.get_pages().len())
            .unwrap_or(0);
        Ok(ToolResult::ok(json!({
            "source": src,
            "output": output,
            "engine": "browser",
            "pages": pages,
            "size_bytes": std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0),
        })))
    }
}

/// 用本机 Office 导出 PDF。返回 `Ok(false)` 表示没装 Office（调用方回退）。
///
/// 只认 Word/Excel/PowerPoint 的 COM 自动化：这是保真度最高的路线，
/// 分页、页码、页眉页脚、图表都跟用户屏幕上一致。
fn office_export_pdf(src: &str, output: &str, ext: &str) -> AppResult<bool> {
    // 17 = wdExportFormatPDF / xlTypePDF / ppSaveAsPDF
    let (prog_id, open_expr, export_expr) = match ext {
        "docx" => (
            "Word.Application",
            "$app.Documents.Open($src, $false, $true)",
            "$doc.ExportAsFixedFormat($dst, 17)",
        ),
        "xlsx" => (
            "Excel.Application",
            "$app.Workbooks.Open($src, 0, $true)",
            "$book.ExportAsFixedFormat(0, $dst)",
        ),
        _ => (
            "PowerPoint.Application",
            "$app.Presentations.Open($src, $true, $false, $false)",
            "$pres.SaveAs($dst, 32)",
        ),
    };
    // PowerPoint 的 Application.Visible 不允许设 false（会抛异常），单独跳过
    let visible_setup = if prog_id.starts_with("PowerPoint") {
        ""
    } else {
        "$app.Visible = $false\n"
    };

    let script = format!(
        r#"$ErrorActionPreference = 'Stop'
$src = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{src}'))
$dst = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{dst}'))
$app = $null
try {{
  $app = New-Object -ComObject {prog_id}
  {visible}
  $app.DisplayAlerts = 0
  $doc = {open_expr}
  {export_expr}
  $doc.Close($false)
  $app.Quit()
  'ok'
}} catch {{
  if ($app -ne $null) {{ try {{ $app.Quit() }} catch {{}} }}
  # 没装对应的 Office 组件时 New-Object 会报 800401F3 / 80080005
  if ($_.Exception.Message -match '800401F3|80080005|0x800401F3|0x80080005|Retrieving the COM class factory') {{
    'NO_OFFICE'
  }} else {{
    'ERR: ' + $_.Exception.Message
  }}
}}"#,
        src = literal(src),
        dst = literal(output),
        prog_id = prog_id,
        visible = visible_setup,
        open_expr = open_expr,
        export_expr = export_expr,
    );

    let (ok, stdout, stderr) = run_powershell(&script)?;
    let out = stdout.trim();
    if out.contains("NO_OFFICE") {
        return Ok(false);
    }
    if !ok || out.starts_with("ERR") {
        // 有 Office 但导出失败（文件损坏、被占用、加密）：这是真错误，不能静默回退
        return Err(AppError::ToolExecution(format!(
            "Office 导出 PDF 失败: {} {}",
            out,
            stderr.trim()
        )));
    }
    if !Path::new(output).is_file() {
        return Err(AppError::ToolExecution(
            "Office 报告导出成功，但目标文件不存在".to_string(),
        ));
    }
    Ok(true)
}

/// 极简 Markdown → HTML。
///
/// 不引 pulldown-cmark：这里只需要标题、粗体、行内代码、代码块、列表、链接，
/// 五个正则量的规则。引一个完整解析器会多 200KB 体积，而排版细节本来
/// 就由浏览器的 CSS 兜着。
fn md_to_html(md: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    let mut in_list = false;

    for line in md.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if in_code {
                out.push_str("</code></pre>\n");
                in_code = false;
            } else {
                out.push_str("<pre><code>");
                in_code = true;
            }
            continue;
        }
        if in_code {
            out.push_str(&escape_html(line));
            out.push('\n');
            continue;
        }

        if trimmed.is_empty() {
            if in_list {
                out.push_str("</ul>\n");
                in_list = false;
            }
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("### ") {
            out.push_str(&format!("<h3>{}</h3>\n", inline_md(rest)));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            out.push_str(&format!("<h2>{}</h2>\n", inline_md(rest)));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            out.push_str(&format!("<h1>{}</h1>\n", inline_md(rest)));
            continue;
        }
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            if !in_list {
                out.push_str("<ul>\n");
                in_list = true;
            }
            out.push_str(&format!("<li>{}</li>\n", inline_md(&trimmed[2..])));
            continue;
        }
        if in_list {
            out.push_str("</ul>\n");
            in_list = false;
        }
        out.push_str(&format!("<p>{}</p>\n", inline_md(trimmed)));
    }
    if in_code {
        out.push_str("</code></pre>\n");
    }
    if in_list {
        out.push_str("</ul>\n");
    }
    out
}

/// 行内规则：先转义，再按固定顺序处理粗体/行内代码/链接。
fn inline_md(text: &str) -> String {
    let escaped = escape_html(text);
    let mut result = escaped.clone();

    // 行内代码先处理：里面的内容不该再被粗体规则碰
    let mut pieces: Vec<String> = Vec::new();
    let mut rest = result.as_str();
    while let Some(start) = rest.find('`') {
        if let Some(end_rel) = rest[start + 1..].find('`') {
            let end = start + 1 + end_rel;
            pieces.push(rest[..start].to_string());
            pieces.push(format!(
                "<code>{}</code>",
                &rest[start + 1..end]
            ));
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }
    pieces.push(rest.to_string());
    // 行内代码块之外的片段才做粗体替换
    result = pieces
        .iter()
        .map(|p| {
            if p.starts_with("<code>") {
                p.clone()
            } else {
                bolds(p)
            }
        })
        .collect();

    // [文字](链接) → <a>
    while let Some(open) = result.find('[') {
        let Some(close_rel) = result[open..].find("](") else {
            break;
        };
        let close = open + close_rel;
        let Some(end_rel) = result[close + 2..].find(')') else {
            break;
        };
        let end = close + 2 + end_rel;
        let label = result[open + 1..close].to_string();
        let url = result[close + 2..end].to_string();
        if url.starts_with("http://") || url.starts_with("https://") {
            result.replace_range(
                open..=end,
                &format!("<a href=\"{}\">{}</a>", url, label),
            );
        } else {
            break;
        }
    }
    result
}

/// `**粗体**` → `<strong>`。用扫描而不是正则，避免引 regex 依赖。
fn bolds(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    loop {
        let Some(start) = rest.find("**") else {
            out.push_str(rest);
            break;
        };
        let Some(end_rel) = rest[start + 2..].find("**") else {
            out.push_str(rest);
            break;
        };
        let end = start + 2 + end_rel;
        out.push_str(&rest[..start]);
        out.push_str("<strong>");
        out.push_str(&rest[start + 2..end]);
        out.push_str("</strong>");
        rest = &rest[end + 2..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_range_supports_comma_and_open_end() {
        assert_eq!(parse_pages("1-3,5", 10).unwrap(), vec![1, 2, 3, 5]);
        assert_eq!(parse_pages("8-", 10).unwrap(), vec![8, 9, 10]);
        assert_eq!(parse_pages(" 2 , 4 ", 5).unwrap(), vec![2, 4]);
    }

    #[test]
    fn page_range_clamps_to_total() {
        // 超出总页数的部分被忽略，而不是报错
        assert_eq!(parse_pages("9-20", 10).unwrap(), vec![9, 10]);
    }

    #[test]
    fn page_range_rejects_zero_and_garbage() {
        assert!(parse_pages("0", 5).is_err());
        assert!(parse_pages("abc", 5).is_err());
        // 全部落在范围外时不能返回空列表假装成功
        assert!(parse_pages("20-30", 5).is_err());
    }

    #[test]
    fn html_escape_covers_angle_and_quote() {
        assert_eq!(escape_html("<a & \"b\">"), "&lt;a &amp; &quot;b&quot;&gt;");
    }

    #[test]
    fn markdown_handles_headings_lists_and_code() {
        let html = md_to_html("# 标题\n\n- 甲\n- 乙\n\n```\nlet x = 1 < 2;\n```");
        assert!(html.contains("<h1>标题</h1>"));
        assert!(html.contains("<li>甲</li>"));
        assert!(html.contains("&lt; 2"), "代码块内容应转义");
    }

    #[test]
    fn markdown_bold_and_inline_code() {
        assert!(inline_md("这是**重点**").contains("<strong>重点</strong>"));
        assert!(inline_md("用 `a**b` 表示").contains("<code>a**b</code>"));
    }

    #[test]
    fn markdown_keeps_unsafe_links_as_text() {
        // javascript: 链接不能变成可点的 <a>
        assert!(!inline_md("[点我](javascript:alert(1))").contains("<a "));
    }

    #[test]
    fn browser_probe_returns_none_or_existing_path() {
        // 这条机器上一定有 Edge 或 Chrome；若真的都没有，
        // 返回 None 也是正确行为，所以只校验"返回的路径必须真实存在"。
        if let Some(p) = find_browser() {
            assert!(p.is_file(), "探测到的浏览器路径必须存在: {:?}", p);
        }
    }

    /// 实机回环：生成 → 读回 → 合并 → 再读回。
    ///
    /// 需要真实浏览器，所以标 ignore。断言只锁结构特征（页数、字节数、
    /// 是否抽得出字），不锁具体文字——文字层能不能抽出来取决于浏览器
    /// 嵌入的字体子集，不同版本 Edge 行为不同。
    #[tokio::test]
    #[ignore = "需要真实浏览器（Edge/Chrome）做 PDF 渲染"]
    async fn real_pdf_pipeline_round_trip() {
        let dir = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let a = dir.join(format!("pcg-pdf-a-{}.pdf", stamp));
        let b = dir.join(format!("pcg-pdf-b-{}.pdf", stamp));
        let merged = dir.join(format!("pcg-pdf-m-{}.pdf", stamp));

        // 1) 生成第一份（含中文，验证字体路径）
        let r1 = CreatePdfTool
            .execute(json!({
                "path": a.to_string_lossy(),
                "title": "测试文档",
                "text": "第一段：这是中文正文。\n第二段：包含数字 12345 与符号 & < >。"
            }))
            .await
            .expect("create_pdf 应返回");
        assert!(r1.success, "create_pdf 失败: {:?}", r1.error);
        let d1 = r1.data.unwrap();
        assert!(d1["pages"].as_u64().unwrap_or(0) >= 1, "至少一页: {}", d1);
        assert!(d1["size_bytes"].as_u64().unwrap_or(0) > 1000, "不能是空壳: {}", d1);

        // 2) 生成第二份
        let r2 = CreatePdfTool
            .execute(json!({ "path": b.to_string_lossy(), "text": "second document" }))
            .await
            .expect("create_pdf 应返回");
        assert!(r2.success, "第二份生成失败: {:?}", r2.error);

        // 3) 读回：页数必须与 info 一致
        let info = PdfInfoTool
            .execute(json!({ "path": a.to_string_lossy() }))
            .await
            .expect("pdf_info 应返回");
        assert!(info.success, "pdf_info 失败: {:?}", info.error);
        let page_count = info.data.unwrap()["page_count"].as_u64().unwrap();
        assert!(page_count >= 1);

        let read = ReadPdfTool
            .execute(json!({ "path": a.to_string_lossy() }))
            .await
            .expect("read_pdf 应返回");
        assert!(read.success, "read_pdf 失败: {:?}", read.error);
        let rd = read.data.unwrap();
        assert_eq!(rd["total_pages"].as_u64().unwrap(), page_count);

        // 4) 合并
        let m = MergePdfTool
            .execute(json!({
                "inputs": [a.to_string_lossy(), b.to_string_lossy()],
                "output": merged.to_string_lossy()
            }))
            .await
            .expect("merge_pdf 应返回");
        assert!(m.success, "merge_pdf 失败: {:?}", m.error);
        let md = m.data.unwrap();
        let merged_info = PdfInfoTool
            .execute(json!({ "path": merged.to_string_lossy() }))
            .await
            .expect("合并后 pdf_info 应返回");
        assert!(merged_info.success);
        let merged_pages = merged_info.data.unwrap()["page_count"].as_u64().unwrap();
        assert_eq!(
            merged_pages,
            md["total_pages"].as_u64().unwrap(),
            "合并后实际页数必须等于报表页数"
        );
        assert!(merged_pages >= 2, "两份合并后至少 2 页");

        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
        let _ = std::fs::remove_file(&merged);
    }
}
