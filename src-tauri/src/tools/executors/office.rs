//! Office 文档读取工具（Word / Excel / PowerPoint）
//!
//! 不引第三方解析库：docx/xlsx/pptx 本质是装了一堆 XML 的 zip，
//! 用系统自带的 `System.IO.Compression` 取出条目，再自己把标签剥成文本。
//! 这条路的代价是只能取"文字"，取不到样式与图片——对助手场景足够了。
//!
//! 解析逻辑全部是纯函数，因此可以在没有 Office、没有真文件的情况下测试。

use std::path::Path;

use async_trait::async_trait;

use crate::error::{AppError, AppResult};
use crate::tools::executors::os_extra::{literal, run_powershell, run_powershell_b64};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

/// 单个条目解压后的上限。再大就不是办公文档了。
const MAX_ENTRY_CHARS: usize = 12 * 1024 * 1024;

/// 从 zip 里取出若干条目，返回 (条目名, XML 文本)。
///
/// 一次 PowerShell 调用取多条：pptx 有几十个 slide，
/// 每个都起一次进程会让"读一份 PPT"变成十几秒。
fn read_zip_entries(path: &str, patterns: &[String]) -> AppResult<Vec<(String, String)>> {
    let pattern_list = patterns
        .iter()
        .map(|p| format!("'{}'", p.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");

    let script = format!(
        r#"Add-Type -AssemblyName System.IO.Compression.FileSystem
$p = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{path}'))
$patterns = @({patterns})
$zip = [System.IO.Compression.ZipFile]::OpenRead($p)
$sb = New-Object Text.StringBuilder
foreach ($e in @($zip.Entries)) {{
  # 条目名分隔符取决于打包工具：.NET Framework 写出的是反斜杠，
  # 而 zip 规范是正斜杠。不归一化就会出现"文件明明在包里却匹配不到"。
  $name = $e.FullName -replace '\\', '/'
  $hit = $false
  foreach ($pat in $patterns) {{ if ($name -like $pat) {{ $hit = $true; break }} }}
  if (-not $hit) {{ continue }}
  $sr = New-Object System.IO.StreamReader($e.Open(), [Text.Encoding]::UTF8)
  $xml = $sr.ReadToEnd()
  $sr.Close()
  [void]$sb.Append($name).Append([char]1).Append($xml).Append([char]0)
}}
$zip.Dispose()
$sb.ToString()"#,
        path = literal(path),
        patterns = pattern_list
    );

    let (ok, payload, stderr) = run_powershell_b64(&script)?;
    if !ok {
        return Err(AppError::ToolExecution(format!(
            "读取文档失败: {}",
            stderr.trim()
        )));
    }

    let mut entries = Vec::new();
    for chunk in payload.split('\0') {
        if chunk.is_empty() {
            continue;
        }
        if let Some((name, xml)) = chunk.split_once('\u{1}') {
            if xml.len() > MAX_ENTRY_CHARS {
                return Err(AppError::InvalidArgument(
                    "文档内部条目过大，已中止（不是普通办公文档）".to_string(),
                ));
            }
            entries.push((name.to_string(), xml.to_string()));
        }
    }
    Ok(entries)
}

fn require_str<'a>(args: &'a serde_json::Value, key: &str) -> AppResult<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::InvalidArgument(format!("{} is required", key)))
}

fn ensure_ext(path: &str, expect: &[&str]) -> AppResult<()> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if expect.contains(&ext.as_str()) {
        Ok(())
    } else {
        Err(AppError::InvalidArgument(format!(
            "扩展名应为 {}，实际: {}",
            expect.join("/"),
            ext
        )))
    }
}

/// XML 实体还原。
///
/// 不处理的话，正文里的 `&amp;` 会原样出现在结果里，
/// 用户会以为是文档本身写错了。
fn unescape_xml(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        // 实体最长也就 10 来个字符，找不到分号就当普通 & 处理
        match tail.find(';').filter(|&i| i <= 10) {
            Some(semi) => {
                let entity = &tail[1..semi];
                let decoded = match entity {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => entity
                        .strip_prefix("#x")
                        .or_else(|| entity.strip_prefix("#X"))
                        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                        .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse::<u32>().ok()))
                        .and_then(char::from_u32),
                };
                match decoded {
                    Some(c) => {
                        out.push(c);
                        rest = &tail[semi + 1..];
                    }
                    None => {
                        out.push('&');
                        rest = &tail[1..];
                    }
                }
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// 取出指定标签内的全部文本（同名标签可能出现多次）。
fn collect_tag(segment: &str, tag: &str) -> String {
    let open_prefix = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut text = String::new();
    let mut rest = segment;

    while let Some(pos) = rest.find(&open_prefix) {
        let after = &rest[pos..];
        // 前缀会撞车：`<w:t` 也匹配 `<w:tbl>` / `<w:tc>` / `<w:tr>`，
        // `<a:t` 也匹配 `<a:tabLst>`。必须确认标签名在这里就结束了。
        let name_end = open_prefix.len();
        let boundary_ok = matches!(
            after[name_end..].chars().next(),
            Some('>') | Some(' ') | Some('/') | Some('\t') | Some('\n')
        );
        if !boundary_ok {
            rest = &after[1..];
            continue;
        }

        let Some(gt) = after.find('>') else { break };
        // <w:t/> 这类自闭合标签没有内容
        if after[..gt].ends_with('/') {
            rest = &after[gt + 1..];
            continue;
        }
        let body_start = gt + 1;
        let Some(end) = after[body_start..].find(&close) else {
            break;
        };
        text.push_str(&unescape_xml(&after[body_start..body_start + end]));
        rest = &after[body_start + end + close.len()..];
    }
    text
}

/// 读标签的第一个值（用于 `<v>123</v>`）。
fn first_tag_value(segment: &str, tag: &str) -> Option<String> {
    let open_prefix = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let pos = segment.find(&open_prefix)?;
    let after = &segment[pos..];
    let name_end = open_prefix.len();
    if !matches!(
        after[name_end..].chars().next(),
        Some('>') | Some(' ') | Some('/')
    ) {
        return None;
    }
    let gt = after.find('>')?;
    if after[..gt].ends_with('/') {
        return Some(String::new());
    }
    let end = after[gt + 1..].find(&close)?;
    Some(unescape_xml(&after[gt + 1..gt + 1 + end]))
}

/// 从标签头部取属性值，例如 `<c r="A1" t="s">` 里的 `t`。
fn attr_value(segment: &str, name: &str) -> Option<String> {
    let head_end = segment.find('>')?;
    let head = &segment[..head_end];
    let needle = format!("{}=\"", name);
    let start = head.find(&needle)? + needle.len();
    let rest = &head[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// docx 正文转纯文本。
///
/// 流式扫描而不是"按段落切开再逐段提取"：表格里每个单元格自带 `<w:p>`，
/// 先分段会让同一行的单元格散成多行。这里按标记逐点推进，
/// 并记录"是否在单元格内"，从而让 `</w:p>` 在单元格里不换行。
pub fn docx_to_text(xml: &str) -> String {
    enum Marker {
        Ignore,
        EnterCell,
        LeaveCell,
        Paragraph,
        Row,
        Tab,
        Break,
        Text,
    }
    use Marker::*;

    // 顺序有意义：同一个位置有多个候选时取靠前的那个，
    // 所以更长的标签名必须排在它的前缀之前（<w:tbl 先于 <w:t）。
    let markers: [(&str, Marker); 9] = [
        ("<w:tbl", Ignore),
        ("<w:tc", EnterCell),
        ("</w:tc>", LeaveCell),
        ("<w:tr", Ignore),
        ("</w:tr>", Row),
        ("</w:p>", Paragraph),
        ("<w:tab/>", Tab),
        ("<w:br/>", Break),
        ("<w:t", Text),
    ];

    let mut out = String::new();
    let mut rest = xml;
    let mut in_cell = false;

    loop {
        let next = markers
            .iter()
            .filter_map(|(needle, kind)| rest.find(needle).map(|pos| (pos, *needle, kind)))
            .min_by_key(|(pos, _, _)| *pos);

        let Some((pos, needle, kind)) = next else {
            break;
        };
        rest = &rest[pos + needle.len()..];

        match kind {
            Ignore => {}
            EnterCell => in_cell = true,
            LeaveCell => {
                in_cell = false;
                if !out.ends_with('\t') {
                    out.push('\t');
                }
            }
            Paragraph => {
                if !in_cell && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Row => {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            Tab => out.push('\t'),
            Break => out.push('\n'),
            Text => {
                // 标签名边界：`<w:t` 不能把 `<w:tbl>` 当成文本节点
                if !matches!(rest.chars().next(), Some('>') | Some(' ') | Some('/')) {
                    continue;
                }
                let Some(gt) = rest.find('>') else { break };
                if rest[..gt].ends_with('/') {
                    rest = &rest[gt + 1..];
                    continue;
                }
                let body = &rest[gt + 1..];
                let Some(end) = body.find("</w:t>") else {
                    break;
                };
                out.push_str(&unescape_xml(&body[..end]));
                rest = &body[end + 6..];
            }
        }
    }

    out.lines()
        .map(|line| line.trim_end())
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// pptx 单页转纯文本。
pub fn pptx_slide_to_text(xml: &str) -> String {
    let mut out = String::new();
    for para in xml.split("</a:p>") {
        let text = collect_tag(para, "a:t");
        let trimmed = text.trim_end();
        if !trimmed.trim().is_empty() {
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

/// xlsx 共享字符串表。
pub fn xlsx_shared_strings(xml: &str) -> Vec<String> {
    xml.split("<si")
        .skip(1)
        .map(|si| collect_tag(si, "t"))
        .collect()
}

/// xlsx 工作表转二维文本。
///
/// 只处理三种取值方式：共享字符串（`t="s"`）、内联字符串（`t="inlineStr"`）、
/// 其余按字面值。公式单元格取缓存值——重算公式是 Excel 的事。
pub fn xlsx_sheet_rows(xml: &str, shared: &[String], max_rows: usize) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();

    for row_seg in xml.split("<row").skip(1) {
        if rows.len() >= max_rows {
            break;
        }
        let mut cells: Vec<String> = Vec::new();
        for cell_seg in row_seg.split("<c ").skip(1) {
            let ty = attr_value(cell_seg, "t");
            let value = match ty.as_deref() {
                Some("s") => first_tag_value(cell_seg, "v")
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .and_then(|idx| shared.get(idx).cloned())
                    .unwrap_or_default(),
                Some("inlineStr") => collect_tag(cell_seg, "t"),
                _ => first_tag_value(cell_seg, "v").unwrap_or_default(),
            };
            cells.push(value);
        }
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    rows
}

/// 把二维表渲染成 TSV，前端与模型都更好读。
fn rows_to_tsv(rows: &[Vec<String>]) -> String {
    rows.iter()
        .map(|row| row.join("\t"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 把二维表渲染成标准 CSV（逗号分隔，含引号与逗号的字段用双引号包裹并转义）。
fn rows_to_csv(rows: &[Vec<String>]) -> String {
    rows.iter()
        .map(|row| {
            row.iter()
                .map(|cell| {
                    if cell.contains(',') || cell.contains('"') || cell.contains('\n') {
                        format!("\"{}\"", cell.replace('"', "\"\""))
                    } else {
                        cell.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(",")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================
// read_docx
// ============================================================

/// 读取 Word 文档正文
pub struct ReadDocxTool;

#[async_trait]
impl AgentTool for ReadDocxTool {
    fn name(&self) -> &'static str {
        "read_docx"
    }

    fn description(&self) -> &'static str {
        "读取 .docx 文档的正文文字（含表格文字，表格按制表符分隔）。适合总结、摘录、改错别字。取不到图片与样式。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": ".docx 文件路径" },
                "max_chars": { "type": "integer", "description": "最多返回多少字符，默认 20000" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, &["docx"])?;
        if !Path::new(path).is_file() {
            return Ok(ToolResult::err(
                "FILE_NOT_FOUND",
                &format!("文件不存在: {}", path),
            ));
        }
        let max_chars = args["max_chars"].as_u64().unwrap_or(20000).min(200000) as usize;

        let entries = read_zip_entries(path, &["word/document.xml".to_string()])?;
        let Some((_, xml)) = entries.first() else {
            return Ok(ToolResult::err(
                "NOT_A_DOCX",
                "压缩包里没有 word/document.xml，可能不是 .docx",
            ));
        };

        let text = docx_to_text(xml);
        let total = text.chars().count();
        let clipped: String = text.chars().take(max_chars).collect();

        Ok(ToolResult::ok(serde_json::json!({
            "path": path,
            "text": clipped,
            "chars": total,
            "truncated": total > max_chars,
            "paragraphs": text.lines().count(),
        })))
    }
}

// ============================================================
// read_pptx
// ============================================================

/// 读取 PowerPoint 每页文字
pub struct ReadPptxTool;

#[async_trait]
impl AgentTool for ReadPptxTool {
    fn name(&self) -> &'static str {
        "read_pptx"
    }

    fn description(&self) -> &'static str {
        "读取 .pptx 每一页的文字，按页返回。适合总结汇报内容、提取讲稿要点。取不到图片与排版。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": ".pptx 文件路径" },
                "max_slides": { "type": "integer", "description": "最多读多少页，默认 30" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, &["pptx"])?;
        if !Path::new(path).is_file() {
            return Ok(ToolResult::err(
                "FILE_NOT_FOUND",
                &format!("文件不存在: {}", path),
            ));
        }
        let max_slides = args["max_slides"].as_u64().unwrap_or(30).min(300) as usize;

        let entries = read_zip_entries(path, &["ppt/slides/slide*.xml".to_string()])?;
        if entries.is_empty() {
            return Ok(ToolResult::err(
                "NOT_A_PPTX",
                "压缩包里没有 ppt/slides/slide*.xml，可能不是 .pptx",
            ));
        }

        // 条目顺序由压缩包决定，页码必须从文件名里取，否则页序会乱
        let mut slides: Vec<(u32, String)> = entries
            .iter()
            .map(|(name, xml)| {
                let number = name
                    .rsplit('/')
                    .next()
                    .and_then(|file| {
                        file.trim_start_matches("slide")
                            .trim_end_matches(".xml")
                            .parse::<u32>()
                            .ok()
                    })
                    .unwrap_or(0);
                (number, pptx_slide_to_text(xml))
            })
            .collect();
        slides.sort_by_key(|(n, _)| *n);
        slides.truncate(max_slides);

        let payload: Vec<serde_json::Value> = slides
            .iter()
            .map(|(n, text)| {
                serde_json::json!({
                    "slide": n,
                    "text": text,
                    "chars": text.chars().count(),
                })
            })
            .collect();

        Ok(ToolResult::ok(serde_json::json!({
            "path": path,
            "slides": payload,
            "count": payload.len(),
        })))
    }
}

// ============================================================
// read_xlsx
// ============================================================

/// 读取 Excel 表格
pub struct ReadXlsxTool;

#[async_trait]
impl AgentTool for ReadXlsxTool {
    fn name(&self) -> &'static str {
        "read_xlsx"
    }

    fn description(&self) -> &'static str {
        "读取 .xlsx 工作表内容，按行返回（制表符分隔）。默认读第一张表，可用 sheet_index 指定。公式取上次保存时的缓存值。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": ".xlsx 文件路径" },
                "sheet_index": { "type": "integer", "description": "第几张表，从 1 开始，默认 1" },
                "max_rows": { "type": "integer", "description": "最多读多少行，默认 100" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, &["xlsx"])?;
        if !Path::new(path).is_file() {
            return Ok(ToolResult::err(
                "FILE_NOT_FOUND",
                &format!("文件不存在: {}", path),
            ));
        }
        let sheet_index = args["sheet_index"].as_u64().unwrap_or(1).max(1) as usize;
        let max_rows = args["max_rows"].as_u64().unwrap_or(100).min(5000) as usize;

        // 共享字符串先取，sheet 里 t="s" 的单元格只是索引
        let mut entries = read_zip_entries(
            path,
            &[
                "xl/sharedStrings.xml".to_string(),
                "xl/worksheets/sheet*.xml".to_string(),
            ],
        )?;

        let shared = entries
            .iter()
            .find(|(name, _)| name.ends_with("sharedStrings.xml"))
            .map(|(_, xml)| xlsx_shared_strings(xml))
            .unwrap_or_default();

        let mut sheets: Vec<(u32, String)> = entries
            .iter()
            .filter(|(name, _)| name.contains("/worksheets/"))
            .map(|(name, xml)| {
                let number = name
                    .rsplit('/')
                    .next()
                    .and_then(|f| {
                        f.trim_start_matches("sheet")
                            .trim_end_matches(".xml")
                            .parse::<u32>()
                            .ok()
                    })
                    .unwrap_or(0);
                (number, xml.clone())
            })
            .collect();
        sheets.sort_by_key(|(n, _)| *n);
        entries.clear();

        if sheets.is_empty() {
            return Ok(ToolResult::err(
                "NOT_A_XLSX",
                "压缩包里没有工作表，可能不是 .xlsx",
            ));
        }
        let sheet_count = sheets.len();
        if sheet_index > sheet_count {
            return Ok(ToolResult::err(
                "SHEET_NOT_FOUND",
                &format!(
                    "只有 {} 张工作表，请求的是第 {} 张",
                    sheet_count, sheet_index
                ),
            ));
        }

        let (number, xml) = &sheets[sheet_index - 1];
        let rows = xlsx_sheet_rows(xml, &shared, max_rows);

        Ok(ToolResult::ok(serde_json::json!({
            "path": path,
            "sheet_index": sheet_index,
            "sheet_file": format!("sheet{}.xml", number),
            "sheet_count": sheet_count,
            "rows": rows.iter().take(max_rows).collect::<Vec<_>>(),
            "row_count": rows.len(),
            "text": rows_to_tsv(&rows),
        })))
    }
}

// ============================================================
// xlsx_to_csv
// ============================================================

/// 把 .xlsx 转成 .csv 文件
pub struct XlsxToCsvTool;

#[async_trait]
impl AgentTool for XlsxToCsvTool {
    fn name(&self) -> &'static str {
        "xlsx_to_csv"
    }

    fn description(&self) -> &'static str {
        "把 .xlsx 工作表转成 .csv 文件（逗号分隔，含引号转义）。默认转第一张表，可用 sheet_index 指定，产出写到 output_path。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": ".xlsx 文件路径" },
                "output_path": { "type": "string", "description": "输出 .csv 路径；省略则与源文件同目录同名换 .csv 后缀" },
                "sheet_index": { "type": "integer", "description": "第几张表，从 1 开始，默认 1" },
                "max_rows": { "type": "integer", "description": "最多转多少行，默认 100000" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, &["xlsx"])?;
        if !Path::new(path).is_file() {
            return Ok(ToolResult::err(
                "FILE_NOT_FOUND",
                &format!("文件不存在: {}", path),
            ));
        }
        let sheet_index = args["sheet_index"].as_u64().unwrap_or(1).max(1) as usize;
        let max_rows = args["max_rows"].as_u64().unwrap_or(100_000).min(1_000_000) as usize;

        let mut entries = read_zip_entries(
            path,
            &[
                "xl/sharedStrings.xml".to_string(),
                "xl/worksheets/sheet*.xml".to_string(),
            ],
        )?;
        let shared = entries
            .iter()
            .find(|(name, _)| name.ends_with("sharedStrings.xml"))
            .map(|(_, xml)| xlsx_shared_strings(xml))
            .unwrap_or_default();
        let mut sheets: Vec<(u32, String)> = entries
            .iter()
            .filter(|(name, _)| name.contains("/worksheets/"))
            .map(|(name, xml)| {
                let number = name
                    .rsplit('/')
                    .next()
                    .and_then(|f| {
                        f.trim_start_matches("sheet")
                            .trim_end_matches(".xml")
                            .parse::<u32>()
                            .ok()
                    })
                    .unwrap_or(0);
                (number, xml.clone())
            })
            .collect();
        sheets.sort_by_key(|(n, _)| *n);
        entries.clear();

        if sheets.is_empty() {
            return Ok(ToolResult::err(
                "NOT_A_XLSX",
                "压缩包里没有工作表，可能不是 .xlsx",
            ));
        }
        if sheet_index > sheets.len() {
            return Ok(ToolResult::err(
                "SHEET_NOT_FOUND",
                &format!("只有 {} 张工作表", sheets.len()),
            ));
        }

        let (_, xml) = &sheets[sheet_index - 1];
        let rows = xlsx_sheet_rows(xml, &shared, max_rows);
        let csv = rows_to_csv(&rows);

        let output_path = match args["output_path"].as_str() {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => {
                let p = Path::new(path);
                let mut out = p.to_path_buf();
                out.set_extension("csv");
                out.to_string_lossy().to_string()
            }
        };
        std::fs::write(&output_path, csv.as_bytes())
            .map_err(|e| AppError::Internal(format!("写入 CSV 失败: {}", e)))?;

        Ok(ToolResult::ok(serde_json::json!({
            "ok": true,
            "path": path,
            "output_path": output_path,
            "sheet_index": sheet_index,
            "row_count": rows.len(),
            "size_bytes": csv.len(),
        })))
    }
}

// ============================================================
// 写入（反向：纯文本 / 表格 → OOXML → zip）
// ============================================================

/// XML 转义。与上面的 `unescape_xml` 互为逆操作：
/// 写出的正文含 `<`、`&` 时必须转义，否则 Word 会把它们当标签。
fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// 0 基列号 → Excel 列名（0→A，25→Z，26→AA）。
fn column_name(index: usize) -> String {
    let mut n = index;
    let mut name = String::new();
    loop {
        name.push((b'A' + (n % 26) as u8) as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    name.chars().rev().collect()
}

/// 生成 word/document.xml：每段包成 `<w:p><w:r><w:t>`。
///
/// `xml:space="preserve"` 必须写，否则 Word 会折叠连续空格与首尾空格。
fn docx_document_xml(paragraphs: &[String]) -> String {
    let mut body = String::new();
    for p in paragraphs {
        let text = p.trim_end();
        if text.is_empty() {
            body.push_str("<w:p/>");
        } else {
            body.push_str(&format!(
                "<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
                escape_xml(text)
            ));
        }
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{}<w:sectPr/></w:body></w:document>",
        body
    )
}

/// 生成 xl/worksheets/sheet1.xml：全部用内联字符串（`t="inlineStr"`），
/// 绕开 sharedStrings 表，让最小 xlsx 也能被 Excel 正常打开。
fn xlsx_sheet_xml(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for (ri, row) in rows.iter().enumerate() {
        out.push_str(&format!("<row r=\"{}\">", ri + 1));
        for (ci, cell) in row.iter().enumerate() {
            let cell_ref = format!("{}{}", column_name(ci), ri + 1);
            out.push_str(&format!(
                "<c r=\"{}\" t=\"inlineStr\"><is><t>{}</t></is></c>",
                cell_ref,
                escape_xml(cell)
            ));
        }
        out.push_str("</row>");
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
         <sheetData>{}</sheetData></worksheet>",
        out
    )
}

/// 生成单页 slide XML：每行一个文本框段落。
fn pptx_slide_xml(text: &str) -> String {
    let mut paras = String::new();
    for line in text.lines() {
        let t = line.trim_end();
        if t.is_empty() {
            paras.push_str("<a:p/>");
        } else {
            paras.push_str(&format!(
                "<a:p><a:r><a:t>{}</a:t></a:r></a:p>",
                escape_xml(t)
            ));
        }
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <p:sld xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
         xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\">\
         <p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id=\"1\" name=\"text\"/>\
         <p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/>{}</p:txBody>\
         </p:sp></p:spTree></p:cSld></p:sld>",
        paras
    )
}

/// 用 System.IO.Compression 把若干 (条目名, 内容) 打进一个 zip，写到 `path`。
///
/// 与 `read_zip_entries` 相反：这里条目内容经 JSON → base64 传给脚本，
/// 脚本用 `ZipArchive.CreateEntry` 直接写流，不落中间临时文件。
/// 条目名统一用正斜杠（zip 规范）。
fn write_zip_entries(path: &str, entries: &[(String, String)]) -> AppResult<()> {
    let payload: Vec<serde_json::Value> = entries
        .iter()
        .map(|(name, content)| serde_json::json!({ "n": name, "c": content }))
        .collect();
    let json = serde_json::to_string(&payload).unwrap_or_else(|_| "[]".to_string());

    let script = format!(
        r#"Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$p = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{path}'))
$entries = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{json}')) | ConvertFrom-Json
$zip = [System.IO.Compression.ZipFile]::Open($p, [System.IO.Compression.ZipArchiveMode]::Create)
foreach ($e in $entries) {{
  $entry = $zip.CreateEntry($e.n)
  $sw = New-Object System.IO.StreamWriter($entry.Open())
  $sw.Write($e.c)
  $sw.Close()
}}
$zip.Dispose()
'ok'"#,
        path = literal(path),
        json = literal(&json),
    );

    let (ok, _stdout, stderr) = run_powershell(&script)?;
    // 双重校验：进程成功 ≠ 文件真写出来了。脚本里的非终止错误不会让
    // powershell 返回非零，所以再确认目标文件确实落盘。
    if !ok || !Path::new(path).is_file() {
        return Err(AppError::ToolExecution(format!(
            "写入文档失败: {}",
            stderr.trim()
        )));
    }
    Ok(())
}

// ============================================================
// write_docx
// ============================================================

/// 生成 Word 文档
pub struct WriteDocxTool;

#[async_trait]
impl AgentTool for WriteDocxTool {
    fn name(&self) -> &'static str {
        "write_docx"
    }

    fn description(&self) -> &'static str {
        "把纯文本写成 .docx Word 文档。用 text（按换行分段）或 paragraphs（字符串数组）指定内容，写入指定路径。生成的是纯文字文档，不含样式与图片。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "输出的 .docx 文件路径" },
                "text": { "type": "string", "description": "文档正文，按换行符分段（与 paragraphs 二选一）" },
                "paragraphs": { "type": "array", "items": { "type": "string" }, "description": "段落数组（与 text 二选一）" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, &["docx"])?;

        let paragraphs: Vec<String> = if let Some(arr) = args["paragraphs"].as_array() {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        } else if let Some(text) = args["text"].as_str() {
            text.lines().map(|s| s.to_string()).collect()
        } else {
            return Err(AppError::InvalidArgument(
                "text 或 paragraphs 至少提供一个".to_string(),
            ));
        };
        if paragraphs.is_empty() {
            return Err(AppError::InvalidArgument(
                "内容为空，无法生成文档".to_string(),
            ));
        }

        let document_xml = docx_document_xml(&paragraphs);
        let entries = vec![
            (
                "[Content_Types].xml".to_string(),
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#
                    .to_string(),
            ),
            (
                "_rels/.rels".to_string(),
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#
                    .to_string(),
            ),
            ("word/document.xml".to_string(), document_xml),
        ];

        write_zip_entries(path, &entries)?;

        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Ok(ToolResult::ok(serde_json::json!({
            "path": path,
            "paragraphs": paragraphs.len(),
            "chars": paragraphs.iter().map(|p| p.chars().count()).sum::<usize>(),
            "size_bytes": size,
        })))
    }
}

// ============================================================
// write_xlsx
// ============================================================

/// 生成 Excel 表格
pub struct WriteXlsxTool;

#[async_trait]
impl AgentTool for WriteXlsxTool {
    fn name(&self) -> &'static str {
        "write_xlsx"
    }

    fn description(&self) -> &'static str {
        "把二维表写成 .xlsx Excel 文件。用 rows（字符串二维数组）或 csv（按逗号/制表符切分）指定内容，写入指定路径。生成的是纯文本单元格，不含公式与样式。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "输出的 .xlsx 文件路径" },
                "rows": { "type": "array", "items": { "type": "array", "items": { "type": "string" } }, "description": "二维字符串数组（与 csv 二选一）" },
                "csv": { "type": "string", "description": "CSV/TSV 文本（与 rows 二选一）" },
                "sheet_name": { "type": "string", "description": "工作表名，默认 Sheet1" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, &["xlsx"])?;

        let rows: Vec<Vec<String>> = if let Some(arr) = args["rows"].as_array() {
            arr.iter()
                .map(|row| {
                    row.as_array()
                        .map(|cells| {
                            cells
                                .iter()
                                .filter_map(|c| c.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect()
        } else if let Some(csv) = args["csv"].as_str() {
            csv.lines()
                .map(|line| parse_csv_line(line))
                .filter(|r| !r.is_empty())
                .collect()
        } else {
            return Err(AppError::InvalidArgument(
                "rows 或 csv 至少提供一个".to_string(),
            ));
        };
        if rows.is_empty() {
            return Err(AppError::InvalidArgument(
                "内容为空，无法生成表格".to_string(),
            ));
        }

        let sheet_name = args["sheet_name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("Sheet1");

        let sheet_xml = xlsx_sheet_xml(&rows);
        let entries = vec![
            (
                "[Content_Types].xml".to_string(),
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#
                    .to_string(),
            ),
            (
                "_rels/.rels".to_string(),
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#
                    .to_string(),
            ),
            (
                "xl/workbook.xml".to_string(),
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
                     <workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
                     xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
                     <sheets><sheet name=\"{}\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>",
                    escape_xml(sheet_name)
                ),
            ),
            (
                "xl/_rels/workbook.xml.rels".to_string(),
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#
                    .to_string(),
            ),
            ("xl/worksheets/sheet1.xml".to_string(), sheet_xml),
        ];

        write_zip_entries(path, &entries)?;

        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Ok(ToolResult::ok(serde_json::json!({
            "path": path,
            "rows": rows.len(),
            "cols": rows.iter().map(|r| r.len()).max().unwrap_or(0),
            "size_bytes": size,
        })))
    }
}

/// 解析一行 CSV/TSV：先探测分隔符（含制表符的按 tab，否则按逗号），
/// 支持双引号包裹。不做完整 RFC 4180，够助手场景用。
fn parse_csv_line(line: &str) -> Vec<String> {
    let delim = if line.contains('\t') { '\t' } else { ',' };
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    current.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == delim {
            cells.push(current.clone());
            current.clear();
        } else {
            current.push(c);
        }
    }
    cells.push(current);
    cells
}

// ============================================================
// write_pptx
// ============================================================

/// 生成 PowerPoint 演示文稿
pub struct WritePptxTool;

#[async_trait]
impl AgentTool for WritePptxTool {
    fn name(&self) -> &'static str {
        "write_pptx"
    }

    fn description(&self) -> &'static str {
        "把文字写成 .pptx PowerPoint。用 slides（每页一个字符串，页内可用换行分段）指定内容，写入指定路径。生成的是纯文字页，不含图片与排版。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "输出的 .pptx 文件路径" },
                "slides": { "type": "array", "items": { "type": "string" }, "description": "每页文字（页内换行分段）" }
            },
            "required": ["path", "slides"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, &["pptx"])?;

        let slides: Vec<String> = args["slides"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if slides.is_empty() {
            return Err(AppError::InvalidArgument("slides 不能为空".to_string()));
        }

        // 每个 slide 一条关系，加上 presentation.xml 自身
        let mut slide_entries = Vec::new();
        let mut rels = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        );
        let mut sld_id_list = String::new();
        for (i, text) in slides.iter().enumerate() {
            let n = i + 1;
            slide_entries.push((format!("ppt/slides/slide{}.xml", n), pptx_slide_xml(text)));
            rels.push_str(&format!(
                "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide\" Target=\"slides/slide{}.xml\"/>",
                n, n
            ));
            sld_id_list.push_str(&format!("<p:sldId id=\"{}\" r:id=\"rId{}\"/>", 256 + i, n));
        }
        rels.push_str("</Relationships>");

        let presentation_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <p:presentation xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
             xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" \
             xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\">\
             <p:sldIdLst>{}</p:sldIdLst><p:sldSz cx=\"9144000\" cy=\"6858000\"/></p:presentation>",
            sld_id_list
        );

        let mut entries = vec![
            (
                "[Content_Types].xml".to_string(),
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#
                    .to_string(),
            ),
            (
                "_rels/.rels".to_string(),
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#
                    .to_string(),
            ),
            ("ppt/presentation.xml".to_string(), presentation_xml),
            ("ppt/_rels/presentation.xml.rels".to_string(), rels),
        ];
        entries.extend(slide_entries);

        write_zip_entries(path, &entries)?;

        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Ok(ToolResult::ok(serde_json::json!({
            "path": path,
            "slides": slides.len(),
            "chars": slides.iter().map(|s| s.chars().count()).sum::<usize>(),
            "size_bytes": size,
        })))
    }
}

// ============================================================
// 增量修改（modify_docx / modify_xlsx）
//
// write_docx/write_xlsx 是"从零生成"，会把目标文件整个覆盖掉。
// 用户手上已有的文档要改两个字，显然不能重写一遍——那会丢掉样式、
// 图片、页眉页脚、批注。这里的做法是打开 zip 只替换 document.xml /
// sheetN.xml 这一个条目，其余条目原封不动。
// ============================================================

/// 用 Update 模式替换 zip 里的单个条目，其余条目保持原样。
///
/// 为什么不用 read_zip_entries + write_zip_entries：那套组合是"解出来再整体
/// 重打"，图片、样式、嵌入对象都会丢（ReadToEnd 按 UTF-8 读二进制会毁数据）。
/// ZipArchiveMode::Update 是就地改，未触碰的条目字节级不变。
fn update_zip_entry(path: &str, entry: &str, content: &str) -> AppResult<()> {
    let script = format!(
        r#"Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$ErrorActionPreference = 'Stop'
$p = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{path}'))
$name = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{name}'))
$content = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{content}'))
$zip = [System.IO.Compression.ZipFile]::Open($p, [System.IO.Compression.ZipArchiveMode]::Update)
$target = $null
foreach ($e in @($zip.Entries)) {{
  if (($e.FullName -replace '\\', '/') -eq $name) {{ $target = $e; break }}
}}
if ($null -eq $target) {{ $target = $zip.CreateEntry($name) }}
$stream = $target.Open()
$stream.SetLength(0)
# 不带 BOM：OOXML 各部件都是无 BOM 的 UTF-8
$enc = New-Object System.Text.UTF8Encoding($false)
$sw = New-Object System.IO.StreamWriter($stream, $enc)
$sw.Write($content)
$sw.Close()
$stream.Close()
$zip.Dispose()
'ok'"#,
        path = literal(path),
        name = literal(entry),
        content = literal(content),
    );
    let (ok, stdout, stderr) = run_powershell(&script)?;
    if !ok || !stdout.trim().contains("ok") {
        return Err(AppError::ToolExecution(format!(
            "写入文档条目失败: {}",
            stderr.trim()
        )));
    }
    Ok(())
}

/// 找出所有段落（`<w:p>` 块）的字节区间。
///
/// 必须排除 `<w:pPr>` / `<w:pStyle>` 这类同前缀标签——直接找 `<w:p` 会
/// 把它们当成段落开头，段落边界全错。
fn paragraph_spans(xml: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = xml[cursor..].find("<w:p") {
        let start = cursor + rel;
        let after = &xml[start + 4..];
        let is_paragraph = after.is_empty()
            || after.starts_with('>')
            || after.starts_with(' ')
            || after.starts_with('\t')
            || after.starts_with('\n')
            || after.starts_with('\r')
            || after.starts_with('/');
        if !is_paragraph {
            cursor = start + 4;
            continue;
        }
        // 自闭合 <w:p/>
        if after.starts_with('/') || after.starts_with(">") && after[1..].starts_with("</w:p>") {
            let end = match xml[start..].find('>') {
                Some(gt) => start + gt + 1,
                None => break,
            };
            spans.push((start, end));
            cursor = end;
            continue;
        }
        match xml[start..].find("</w:p>") {
            Some(end_rel) => {
                let end = start + end_rel + "</w:p>".len();
                spans.push((start, end));
                cursor = end;
            }
            None => break,
        }
    }
    spans
}

/// 把整段内容换成一段新文字，保留段落属性（`<w:pPr>`：对齐、缩进、样式）。
fn rewrite_paragraph(block: &str, text: &str) -> String {
    let open_end = match block.find('>') {
        Some(i) => i + 1,
        None => {
            return format!(
                "<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
                escape_xml(text)
            )
        }
    };
    let close = "</w:p>";
    let inner_end = block.len().saturating_sub(close.len());
    if open_end > inner_end {
        return format!(
            "<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
            escape_xml(text)
        );
    }
    let open = &block[..open_end];
    let inner = &block[open_end..inner_end];
    let ppr = if inner.trim_start().starts_with("<w:pPr") {
        match inner.find("</w:pPr>") {
            Some(e) => &inner[..e + "</w:pPr>".len()],
            None => "",
        }
    } else {
        ""
    };
    if text.trim().is_empty() {
        format!("{}{}</w:p>", open, ppr)
    } else {
        format!(
            "{}{}<w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
            open,
            ppr,
            escape_xml(text)
        )
    }
}

/// 在每个 `<w:t>` 内部做文本替换，返回 (新 xml, 命中次数)。
///
/// 只能整段命中同一个 run 内的文字。Word 会把一句话按编辑历史切成多个
/// run（改过词的句子几乎必然是碎的），跨 run 的短语匹配不到——这是纯文本
/// 层面替换的固有限制，不是实现缺陷。
fn replace_in_text_runs(xml: &str, from: &str, to: &str) -> (String, usize) {
    let mut out = String::with_capacity(xml.len());
    let mut cursor = 0usize;
    let mut hits = 0usize;
    loop {
        let Some(rel) = xml[cursor..].find("<w:t") else {
            out.push_str(&xml[cursor..]);
            break;
        };
        let tag_start = cursor + rel;
        let Some(gt_rel) = xml[tag_start..].find('>') else {
            out.push_str(&xml[cursor..]);
            break;
        };
        let content_start = tag_start + gt_rel + 1;
        // 自闭合 <w:t/>：没有内容可换
        if xml[tag_start..content_start].ends_with("/>") {
            out.push_str(&xml[cursor..content_start]);
            cursor = content_start;
            continue;
        }
        let Some(end_rel) = xml[content_start..].find("</w:t>") else {
            out.push_str(&xml[cursor..]);
            break;
        };
        let content_end = content_start + end_rel;
        out.push_str(&xml[cursor..content_start]);
        let raw = &xml[content_start..content_end];
        let plain = unescape_xml(raw);
        if plain.contains(from) {
            hits += plain.matches(from).count();
            out.push_str(&escape_xml(&plain.replace(from, to)));
        } else {
            out.push_str(raw);
        }
        out.push_str("</w:t>");
        cursor = content_end + "</w:t>".len();
    }
    (out, hits)
}

/// document.xml 里段落应该插在哪：`</w:body>` 之前，且要在 `<w:sectPr>` 之前。
///
/// `w:sectPr` 定义页面尺寸/页边距，Word 要求它是 body 的最后一个子元素。
/// 插到它后面，文档会变成"新段落用默认页面设置"，打印出来页边距全变。
fn docx_body_insert_point(xml: &str) -> Option<usize> {
    let body_close = xml.rfind("</w:body>")?;
    let head = &xml[..body_close];
    if let Some(sect) = head.rfind("<w:sectPr") {
        // 只认 body 直属的 sectPr：它和 </w:body> 之间只有空白
        if head[sect..].trim_end().ends_with("</w:sectPr>") {
            return Some(sect);
        }
    }
    Some(body_close)
}

/// 修改已有的 Word 文档
pub struct ModifyDocxTool;

#[async_trait]
impl AgentTool for ModifyDocxTool {
    fn name(&self) -> &'static str {
        "modify_docx"
    }

    fn description(&self) -> &'static str {
        "在已有 .docx 文档上做局部修改，保留原有样式、图片、页眉页脚（不像 write_docx 那样整份重写）。支持三种操作，可组合：append_paragraphs 在末尾追加段落；replace_text 替换指定文字（只能命中同一个文字块内的连续文字，被 Word 拆散的长句可能匹配不到）；replace_paragraph 按段落序号整段改写（序号从 1 开始，可用 read_docx 先看内容）。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "要修改的 .docx 路径（必须是已存在的文件）" },
                "append_paragraphs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "在文档末尾追加的段落"
                },
                "replace_text": {
                    "type": "array",
                    "description": "文本替换列表",
                    "items": {
                        "type": "object",
                        "properties": {
                            "from": { "type": "string", "description": "要查找的原文" },
                            "to": { "type": "string", "description": "替换成的文字，可为空字符串（等于删除）" }
                        },
                        "required": ["from", "to"]
                    }
                },
                "replace_paragraph": {
                    "type": "object",
                    "description": "按段落序号整段改写",
                    "properties": {
                        "index": { "type": "integer", "description": "段落序号，从 1 开始" },
                        "text": { "type": "string", "description": "新的段落文字" }
                    },
                    "required": ["index", "text"]
                }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, &["docx"])?;
        if !Path::new(path).is_file() {
            return Err(AppError::InvalidArgument(format!(
                "文档不存在（modify_docx 只能改已有文件，新建请用 write_docx）: {}",
                path
            )));
        }

        let entries = read_zip_entries(path, &["word/document.xml".to_string()])?;
        let xml = entries
            .into_iter()
            .find(|(name, _)| name.ends_with("word/document.xml"))
            .map(|(_, xml)| xml)
            .ok_or_else(|| {
                AppError::ToolExecution(
                    "文件里没有 word/document.xml，可能不是有效的 docx".to_string(),
                )
            })?;

        let mut updated = xml.clone();
        let mut changed = false;
        let mut replaced_hits = 0usize;
        let mut appended = 0usize;
        let mut paragraph_replaced: Option<usize> = None;

        // 1) 文本替换
        if let Some(list) = args["replace_text"].as_array() {
            for item in list {
                let from = item["from"].as_str().unwrap_or("");
                if from.is_empty() {
                    continue;
                }
                let to = item["to"].as_str().unwrap_or("");
                let (next, hits) = replace_in_text_runs(&updated, from, to);
                if hits > 0 {
                    updated = next;
                    replaced_hits += hits;
                    changed = true;
                }
            }
        }

        // 2) 整段改写（放在文本替换之后：序号是基于原始文档的，
        //    先替换会让序号对应的段落内容变掉，但位置不变——顺序无所谓，
        //    这里选在追加之前，避免追加的段落混进序号计算）
        if let Some(spec) = args["replace_paragraph"].as_object() {
            let index = spec.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let text = spec.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if index == 0 {
                return Err(AppError::InvalidArgument(
                    "replace_paragraph.index 从 1 开始".to_string(),
                ));
            }
            let spans = paragraph_spans(&updated);
            if index > spans.len() {
                return Err(AppError::InvalidArgument(format!(
                    "段落序号超出范围：文档共 {} 段，请求第 {} 段",
                    spans.len(),
                    index
                )));
            }
            let (start, end) = spans[index - 1];
            let block = &updated[start..end];
            let fresh = rewrite_paragraph(block, text);
            updated.replace_range(start..end, &fresh);
            paragraph_replaced = Some(index);
            changed = true;
        }

        // 3) 末尾追加段落
        if let Some(list) = args["append_paragraphs"].as_array() {
            let paragraphs: Vec<String> = list
                .iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.to_string())
                .collect();
            if !paragraphs.is_empty() {
                let insert_at = docx_body_insert_point(&updated).ok_or_else(|| {
                    AppError::ToolExecution("文档结构异常：找不到 </w:body>".to_string())
                })?;
                let mut block = String::new();
                for p in &paragraphs {
                    block.push_str(&format!(
                        "<w:p><w:pPr><w:rPr><w:rFonts w:eastAsia=\"Microsoft YaHei\"/></w:rPr></w:pPr>\
                         <w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
                        escape_xml(p.trim_end())
                    ));
                }
                updated.insert_str(insert_at, &block);
                appended = paragraphs.len();
                changed = true;
            }
        }

        if !changed {
            return Ok(ToolResult::err(
                "NOTHING_TO_DO",
                "没有提供任何有效的修改操作（append_paragraphs / replace_text / replace_paragraph 至少给一个）",
            ));
        }

        update_zip_entry(path, "word/document.xml", &updated)?;

        // 回读校验：写回成功 ≠ 内容真的进去了（可能写到了别的条目）。
        // 重新打开文件抽出文字，确认改后的文字确实存在。
        let verify = read_zip_entries(path, &["word/document.xml".to_string()])?;
        let after_xml = verify
            .into_iter()
            .find(|(n, _)| n.ends_with("word/document.xml"))
            .map(|(_, x)| x)
            .unwrap_or_default();
        let text_after = docx_to_text(&after_xml);
        let paragraphs_after = paragraph_spans(&after_xml).len();
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        Ok(ToolResult::ok(serde_json::json!({
            "path": path,
            "paragraphs_after": paragraphs_after,
            "appended": appended,
            "replaced_occurrences": replaced_hits,
            "paragraph_replaced": paragraph_replaced,
            "size_bytes": size,
            "text_preview": text_after.chars().take(200).collect::<String>(),
        })))
    }
}

// ------------------------------------------------------------
// xlsx 单元格改写
// ------------------------------------------------------------

/// 解析单元格引用 "AB12" → (列下标 0 基, 行号 1 基)
fn parse_cell_ref(reference: &str) -> Option<(usize, u32)> {
    let reference = reference.trim();
    let mut col = 0usize;
    let mut digits = String::new();
    let mut seen_digit = false;
    for c in reference.chars() {
        if c.is_ascii_alphabetic() {
            if seen_digit {
                return None; // A1B 这种非法
            }
            col = col * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
        } else if c.is_ascii_digit() {
            seen_digit = true;
            digits.push(c);
        } else {
            return None;
        }
    }
    if !seen_digit || col == 0 {
        return None;
    }
    let row: u32 = digits.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((col - 1, row))
}

/// 生成单元格 XML：数字写成数值，其余写成内联字符串。
///
/// 用 inlineStr 而不是共享字符串表：共享表要给 sharedStrings.xml 增条目并
/// 改 count/uniqueCount，还要维护 SST 索引——为了写一个格子改四处的账，
/// 不值得。内联字符串是 OOXML 里合法的等价写法。
fn cell_xml(reference: &str, value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Number(n) => {
            format!("<c r=\"{}\"><v>{}</v></c>", reference, n)
        }
        serde_json::Value::Bool(b) => format!(
            "<c r=\"{}\" t=\"b\"><v>{}</v></c>",
            reference,
            if *b { 1 } else { 0 }
        ),
        serde_json::Value::Null => format!("<c r=\"{}\"/>", reference),
        other => {
            let text = other
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| other.to_string());
            if text.is_empty() {
                // 空字符串：写成空单元格而不是 inlineStr，Excel 才不会把它当"有内容"
                format!("<c r=\"{}\"/>", reference)
            } else {
                format!(
                    "<c r=\"{}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                    reference,
                    escape_xml(&text)
                )
            }
        }
    }
}

/// 找出 sheetData 内所有行的 (行号, 起始, 结束)
fn row_spans(sheet_xml: &str) -> Vec<(u32, usize, usize)> {
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = sheet_xml[cursor..].find("<row") {
        let start = cursor + rel;
        let after = &sheet_xml[start + 4..];
        if !(after.starts_with(' ')
            || after.starts_with('>')
            || after.starts_with('/')
            || after.starts_with('\t')
            || after.starts_with('\n')
            || after.starts_with('\r'))
        {
            cursor = start + 4;
            continue;
        }
        let Some(gt_rel) = sheet_xml[start..].find('>') else {
            break;
        };
        let tag = &sheet_xml[start..start + gt_rel];
        let row_no = tag
            .split("r=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        if tag.trim_end().ends_with('/') {
            let end = start + gt_rel + 1;
            spans.push((row_no, start, end));
            cursor = end;
            continue;
        }
        match sheet_xml[start..].find("</row>") {
            Some(end_rel) => {
                let end = start + end_rel + "</row>".len();
                spans.push((row_no, start, end));
                cursor = end;
            }
            None => break,
        }
    }
    spans
}

/// 找 row 内所有单元格的 (列下标, 起始, 结束)
fn cell_spans(row_xml: &str) -> Vec<(usize, usize, usize)> {
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = row_xml[cursor..].find("<c ") {
        let start = cursor + rel;
        let Some(gt_rel) = row_xml[start..].find('>') else {
            break;
        };
        let tag = &row_xml[start..start + gt_rel];
        let reference = tag
            .split("r=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("");
        let col = parse_cell_ref(reference).map(|(c, _)| c).unwrap_or(0);
        if tag.trim_end().ends_with('/') {
            let end = start + gt_rel + 1;
            spans.push((col, start, end));
            cursor = end;
            continue;
        }
        match row_xml[start..].find("</c>") {
            Some(end_rel) => {
                let end = start + end_rel + "</c>".len();
                spans.push((col, start, end));
                cursor = end;
            }
            None => break,
        }
    }
    spans
}

/// 在 row 内设置一个单元格：已存在则整格替换，不存在则按列序插入。
fn set_cell_in_row(
    row_xml: &str,
    col: usize,
    reference: &str,
    value: &serde_json::Value,
) -> String {
    let fresh = cell_xml(reference, value);
    let spans = cell_spans(row_xml);
    if let Some((_, start, end)) = spans.iter().find(|(c, _, _)| *c == col) {
        let mut out = String::with_capacity(row_xml.len() + fresh.len());
        out.push_str(&row_xml[..*start]);
        out.push_str(&fresh);
        out.push_str(&row_xml[*end..]);
        return out;
    }
    // 按列序插入：OpenXML 规范要求 row 内的 c 按列升序，乱序 Excel 会提示修复
    for (c, start, _) in &spans {
        if *c > col {
            let mut out = String::with_capacity(row_xml.len() + fresh.len());
            out.push_str(&row_xml[..*start]);
            out.push_str(&fresh);
            out.push_str(&row_xml[*start..]);
            return out;
        }
    }
    match row_xml.rfind("</row>") {
        Some(pos) => {
            let mut out = String::with_capacity(row_xml.len() + fresh.len());
            out.push_str(&row_xml[..pos]);
            out.push_str(&fresh);
            out.push_str(&row_xml[pos..]);
            out
        }
        None => row_xml.to_string(),
    }
}

/// 确保 sheet 里存在某一行，返回新的 sheet xml。
///
/// 自闭合的 `<row r="5"/>` 要展开成 `<row r="5"></row>` 才能往里放单元格。
fn ensure_row(sheet_xml: &str, row: u32) -> String {
    let spans = row_spans(sheet_xml);
    if let Some((_, start, end)) = spans.iter().find(|(r, _, _)| *r == row) {
        let block = &sheet_xml[*start..*end];
        if block.contains("</row>") {
            return sheet_xml.to_string();
        }
        let fresh = format!("<row r=\"{}\"></row>", row);
        let mut out = String::with_capacity(sheet_xml.len() + 16);
        out.push_str(&sheet_xml[..*start]);
        out.push_str(&fresh);
        out.push_str(&sheet_xml[*end..]);
        return out;
    }
    let fresh = format!("<row r=\"{}\"></row>", row);
    // 按行号升序插入
    for (r, start, _) in &spans {
        if *r > row {
            let mut out = String::with_capacity(sheet_xml.len() + fresh.len());
            out.push_str(&sheet_xml[..*start]);
            out.push_str(&fresh);
            out.push_str(&sheet_xml[*start..]);
            return out;
        }
    }
    match sheet_xml.rfind("</sheetData>") {
        Some(pos) => {
            let mut out = String::with_capacity(sheet_xml.len() + fresh.len());
            out.push_str(&sheet_xml[..pos]);
            out.push_str(&fresh);
            out.push_str(&sheet_xml[pos..]);
            out
        }
        // 没有 sheetData 的空表：补一个
        None => sheet_xml.replace(
            "</worksheet>",
            &format!("<sheetData>{}</sheetData></worksheet>", fresh),
        ),
    }
}

/// sheet 名 → 工作表 xml 路径。
///
/// workbook.xml 只给 `r:id`，真实文件名在 workbook.xml.rels 里，
/// 而那个 target 可能是 "worksheets/sheet1.xml"（相对 xl/）也可能是
/// "/xl/worksheets/sheet1.xml"（绝对）——两种写法 Excel 都产出过。
fn xlsx_sheet_map(entries: &[(String, String)]) -> Vec<(String, String)> {
    let workbook = entries
        .iter()
        .find(|(n, _)| n.ends_with("xl/workbook.xml"))
        .map(|(_, x)| x.as_str())
        .unwrap_or("");
    let rels = entries
        .iter()
        .find(|(n, _)| n.ends_with("xl/_rels/workbook.xml.rels"))
        .map(|(_, x)| x.as_str())
        .unwrap_or("");

    // rId → target
    let mut id_to_target: Vec<(String, String)> = Vec::new();
    let mut rest = rels;
    while let Some(pos) = rest.find("<Relationship") {
        let tag_end = match rest[pos..].find('>') {
            Some(e) => pos + e,
            None => break,
        };
        let tag = &rest[pos..tag_end];
        let id = tag
            .split("Id=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("");
        let target = tag
            .split("Target=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("");
        if !id.is_empty() && !target.is_empty() {
            id_to_target.push((id.to_string(), target.to_string()));
        }
        rest = &rest[tag_end..];
    }

    let mut result = Vec::new();
    let mut rest = workbook;
    while let Some(pos) = rest.find("<sheet ") {
        let tag_end = match rest[pos..].find('>') {
            Some(e) => pos + e,
            None => break,
        };
        let tag = &rest[pos..tag_end];
        let name = tag
            .split("name=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("");
        let rid = tag
            .split("r:id=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("");
        if let Some((_, target)) = id_to_target.iter().find(|(id, _)| id == rid) {
            let path = if target.starts_with('/') {
                target.trim_start_matches('/').to_string()
            } else {
                format!("xl/{}", target)
            };
            result.push((unescape_xml(name), path));
        }
        rest = &rest[tag_end..];
    }
    result
}

/// 修改已有的 Excel 工作簿
pub struct ModifyXlsxTool;

#[async_trait]
impl AgentTool for ModifyXlsxTool {
    fn name(&self) -> &'static str {
        "modify_xlsx"
    }

    fn description(&self) -> &'static str {
        "在已有 .xlsx 工作簿里改单元格内容，保留其他工作表、公式、样式和图表（不像 write_xlsx 那样整份重写）。set_cells 是单元格列表：sheet 省略时用第一张表，cell 如 \"B3\"，value 可以是文字或数字。行/单元格不存在会自动创建。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "要修改的 .xlsx 路径（必须是已存在的文件）" },
                "set_cells": {
                    "type": "array",
                    "description": "要写入的单元格",
                    "items": {
                        "type": "object",
                        "properties": {
                            "sheet": { "type": "string", "description": "工作表名，省略用第一张" },
                            "cell": { "type": "string", "description": "单元格地址，如 A1、B3" },
                            "value": { "description": "写入的值：文字、数字或布尔" }
                        },
                        "required": ["cell", "value"]
                    }
                }
            },
            "required": ["path", "set_cells"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = require_str(&args, "path")?;
        ensure_ext(path, &["xlsx"])?;
        if !Path::new(path).is_file() {
            return Err(AppError::InvalidArgument(format!(
                "工作簿不存在（modify_xlsx 只能改已有文件，新建请用 write_xlsx）: {}",
                path
            )));
        }
        let cells = args["set_cells"].as_array().cloned().unwrap_or_default();
        if cells.is_empty() {
            return Err(AppError::InvalidArgument(
                "set_cells 为空，没有要写入的内容".to_string(),
            ));
        }

        // 先取出 workbook 结构与所有 sheet 条目
        let meta = read_zip_entries(
            path,
            &[
                "xl/workbook.xml".to_string(),
                "xl/_rels/workbook.xml.rels".to_string(),
            ],
        )?;
        let sheet_map = xlsx_sheet_map(&meta);
        if sheet_map.is_empty() {
            return Err(AppError::ToolExecution(
                "无法解析工作表清单（xl/workbook.xml 或 rels 缺失/异常）".to_string(),
            ));
        }
        let sheet_names: Vec<String> = sheet_map.iter().map(|(n, _)| n.clone()).collect();

        // 按工作表分组：同一张表只读一次、写一次，避免多次往返
        let mut per_sheet: Vec<(String, String, Vec<(usize, u32, serde_json::Value, String)>)> =
            Vec::new();
        for item in &cells {
            let wanted_sheet = item["sheet"].as_str().unwrap_or("").trim().to_string();
            let (sheet_name, sheet_path) = if wanted_sheet.is_empty() {
                sheet_map[0].clone()
            } else {
                match sheet_map.iter().find(|(n, _)| n == &wanted_sheet) {
                    Some(pair) => pair.clone(),
                    None => {
                        return Err(AppError::InvalidArgument(format!(
                            "找不到工作表「{}」。现有工作表: {}",
                            wanted_sheet,
                            sheet_names.join(", ")
                        )))
                    }
                }
            };
            let reference = item["cell"].as_str().unwrap_or("").trim().to_string();
            let (col, row) = parse_cell_ref(&reference).ok_or_else(|| {
                AppError::InvalidArgument(format!("单元格地址不合法: {}", reference))
            })?;
            let value = item
                .get("value")
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            if let Some(slot) = per_sheet.iter_mut().find(|(n, _, _)| n == &sheet_name) {
                slot.2.push((col, row, value, reference));
            } else {
                per_sheet.push((
                    sheet_name.clone(),
                    sheet_path.clone(),
                    vec![(col, row, value, reference)],
                ));
            }
        }

        let mut written = 0usize;
        let mut touched_sheets: Vec<String> = Vec::new();

        for (sheet_name, sheet_path, items) in &per_sheet {
            let loaded = read_zip_entries(path, &[sheet_path.clone()])?;
            let xml = loaded
                .into_iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(sheet_path))
                .map(|(_, x)| x)
                .ok_or_else(|| {
                    AppError::ToolExecution(format!(
                        "工作表「{}」的 xml（{}）不在文件里",
                        sheet_name, sheet_path
                    ))
                })?;

            let mut updated = xml.clone();
            for (col, row, value, reference) in items {
                updated = ensure_row(&updated, *row);
                // ensure_row 之后重新定位行块（位置可能变了）
                let spans = row_spans(&updated);
                let Some((_, start, end)) = spans.iter().find(|(r, _, _)| r == row).copied() else {
                    return Err(AppError::ToolExecution(format!(
                        "无法在 {} 第 {} 行定位插入点",
                        sheet_name, row
                    )));
                };
                let row_block = &updated[start..end];
                let fresh_row = set_cell_in_row(row_block, *col, reference, value);
                updated.replace_range(start..end, &fresh_row);
                written += 1;
            }

            update_zip_entry(path, sheet_path, &updated)?;
            touched_sheets.push(sheet_name.clone());
        }

        // 回读校验：把改过的单元格重新读出来，确认值真的落进去了。
        // 这一步能抓住"写成功了但写到另一张表"这类错位。
        let mut verified: Vec<serde_json::Value> = Vec::new();
        for (sheet_name, sheet_path, items) in &per_sheet {
            let reloaded = read_zip_entries(path, &[sheet_path.clone()])?;
            let xml = reloaded
                .into_iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(sheet_path))
                .map(|(_, x)| x)
                .unwrap_or_default();
            for (_, row, value, reference) in items {
                let expected = match value {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => {
                        if *b {
                            "1".to_string()
                        } else {
                            "0".to_string()
                        }
                    }
                    _ => String::new(),
                };
                // 在对应行里找这个单元格的实际内容。
                // 只抽 <t>（内联字符串）和 <v>（数值）：这两个标签就是
                // 单元格里"看得见的值"，其余 <f> 公式、样式不参与比对。
                let spans = row_spans(&xml);
                let found = spans
                    .iter()
                    .find(|(r, _, _)| r == row)
                    .map(|(_, s, e)| {
                        let block = &xml[*s..*e];
                        let cells = cell_spans(block);
                        cells
                            .iter()
                            .find(|(c, _, _)| {
                                parse_cell_ref(reference)
                                    .map(|(cc, _)| cc == *c)
                                    .unwrap_or(false)
                            })
                            .map(|(_, cs, ce)| {
                                let cell_xml = &block[*cs..*ce];
                                let inline = collect_tag(cell_xml, "t");
                                if inline.is_empty() {
                                    collect_tag(cell_xml, "v")
                                } else {
                                    inline
                                }
                            })
                    })
                    .flatten()
                    .unwrap_or_default();
                verified.push(serde_json::json!({
                    "sheet": sheet_name,
                    "cell": reference,
                    "expected": expected,
                    "found": found.clone(),
                    "match": expected.is_empty() || found.contains(&expected),
                }));
            }
        }

        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let all_match = verified
            .iter()
            .all(|v| v["match"].as_bool().unwrap_or(false));

        Ok(ToolResult::ok(serde_json::json!({
            "path": path,
            "sheets_touched": touched_sheets,
            "cells_written": written,
            "all_verified": all_match,
            "size_bytes": size,
            "cells": verified,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCX_XML: &str = r#"<?xml version="1.0"?>
<w:document><w:body>
<w:p><w:r><w:t>第一段：这是正文</w:t></w:r></w:p>
<w:p><w:r><w:t>第二段含</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>制表与 &amp; 实体</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:p></w:p>
</w:body></w:document>"#;

    #[test]
    fn docx_extracts_paragraphs_and_entities() {
        let text = docx_to_text(DOCX_XML);
        assert!(text.contains("第一段：这是正文"));
        assert!(text.contains("制表与 & 实体"), "实体必须还原");
        assert!(!text.contains("<w:"), "不该残留标签");
        assert!(!text.contains("\n\n"), "空段落不该留下空行");
    }

    #[test]
    fn docx_keeps_table_cells_on_one_row() {
        let text = docx_to_text(DOCX_XML);
        let table_line = text
            .lines()
            .find(|l| l.contains("A1"))
            .expect("表格内容应存在");
        assert!(table_line.contains("B1"), "同一行的单元格应在同一行");
    }

    #[test]
    fn numeric_entities_are_decoded() {
        assert_eq!(unescape_xml("&#26085;"), "日");
        assert_eq!(unescape_xml("&#x65E5;"), "日");
        // 不完整的实体原样保留，不能把 & 吃掉
        assert_eq!(unescape_xml("a & b"), "a & b");
        assert_eq!(unescape_xml("&unknown;"), "&unknown;");
    }

    #[test]
    fn pptx_reads_each_slide_text() {
        let xml = r#"<p:sld><p:cSld><p:spTree>
            <p:sp><p:txBody>
              <a:p><a:r><a:t>标题页</a:t></a:r></a:p>
              <a:p><a:r><a:t>副标题</a:t></a:r></a:p>
            </p:txBody></p:sp>
        </p:spTree></p:cSld></p:sld>"#;
        let text = pptx_slide_to_text(xml);
        assert!(text.contains("标题页"));
        assert!(text.contains("副标题"));
        assert_eq!(text.lines().count(), 2, "每个段落一行");
    }

    #[test]
    fn xlsx_maps_shared_strings_and_numbers() {
        let shared_xml =
            r#"<sst><si><t>姓名</t></si><si><t>年龄</t></si><si><t>张三</t></si></sst>"#;
        let shared = xlsx_shared_strings(shared_xml);
        assert_eq!(shared, vec!["姓名", "年龄", "张三"]);

        let sheet_xml = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
            <row r="2"><c r="A2" t="s"><v>2</v></c><c r="B2"><v>28</v></c></row>
            <row r="3"><c r="A3" t="inlineStr"><is><t>内联</t></is></c></row>
        </sheetData></worksheet>"#;
        let rows = xlsx_sheet_rows(sheet_xml, &shared, 10);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], vec!["姓名", "年龄"]);
        assert_eq!(rows[1], vec!["张三", "28"]);
        assert_eq!(rows[2], vec!["内联"]);
    }

    #[test]
    fn xlsx_respects_max_rows() {
        let sheet_xml = r#"<worksheet><sheetData>
            <row r="1"><c r="A1"><v>1</v></c></row>
            <row r="2"><c r="A2"><v>2</v></c></row>
            <row r="3"><c r="A3"><v>3</v></c></row>
        </sheetData></worksheet>"#;
        let rows = xlsx_sheet_rows(sheet_xml, &[], 2);
        assert_eq!(rows.len(), 2, "max_rows 必须生效");
    }

    /// 实机探针：自己造一个最小 docx，走完整"解压 → 剥标签"链路。
    ///
    /// 不拿用户的真实文档做测试——那会把私人内容带进测试输出。
    /// 手动执行：`cargo test real_docx_round_trip -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "需要 PowerShell 打包临时 docx"]
    async fn real_docx_round_trip() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!("pcg-office-probe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("word")).unwrap();
        fs::write(
            dir.join("[Content_Types].xml"),
            r#"<?xml version="1.0"?><Types/>"#,
        )
        .unwrap();
        fs::write(dir.join("word/document.xml"), DOCX_XML).unwrap();

        // Compress-Archive 只认 .zip 扩展名，所以先压成 zip 再改名成 docx；
        // 目标文件必须放在源目录之外，否则打包时会把正在写的包自己也扫进去。
        let zip_path = std::env::temp_dir().join(format!("pcg-probe-{}.zip", std::process::id()));
        let docx = std::env::temp_dir().join(format!("pcg-probe-{}.docx", std::process::id()));
        let _ = fs::remove_file(&zip_path);
        let _ = fs::remove_file(&docx);

        let script = format!(
            "$src = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{src}'))\n\
             $dst = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{dst}'))\n\
             if (Test-Path $dst) {{ Remove-Item -LiteralPath $dst -Force }}\n\
             Get-ChildItem -Path $src | Compress-Archive -DestinationPath $dst -Force\n\
             'ok'",
            src = literal(&dir.to_string_lossy()),
            dst = literal(&zip_path.to_string_lossy())
        );
        let (ok, _out, stderr) = run_powershell_b64(&script).expect("调用 PowerShell");
        assert!(ok, "打包失败: {}", stderr);
        assert!(zip_path.is_file(), "临时 zip 没生成");
        fs::rename(&zip_path, &docx).expect("改名成 docx");

        let result = ReadDocxTool
            .execute(serde_json::json!({ "path": docx.to_string_lossy() }))
            .await
            .expect("读取 docx");
        assert!(result.success, "读取失败: {:?}", result.error);
        let data = result.data.unwrap();
        let text = data["text"].as_str().unwrap();
        assert!(text.contains("第一段：这是正文"), "正文缺失: {text:?}");
        assert!(text.contains("A1\tB1"), "表格应保持同一行: {text:?}");
        assert_eq!(data["truncated"], serde_json::json!(false));

        // 扩展名不对时必须拒绝，而不是读出一堆二进制乱码
        let wrong = ReadDocxTool
            .execute(serde_json::json!({ "path": docx.to_string_lossy().replace(".docx", ".txt") }))
            .await;
        assert!(wrong.is_err(), "非 docx 扩展名应报参数错误");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn escape_xml_round_trips_with_unescape() {
        let raw = "a < b & c > \"d\" 'e'";
        let escaped = escape_xml(raw);
        assert!(
            !escaped.contains('<') && !escaped.contains('>'),
            "转义后不应残留裸尖括号: {escaped}"
        );
        assert_eq!(unescape_xml(&escaped), raw, "转义后还原应得到原文");
    }

    #[test]
    fn column_name_counts_like_excel() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(27), "AB");
        assert_eq!(column_name(701), "ZZ");
    }

    #[test]
    fn docx_document_xml_wraps_paragraphs_and_escapes() {
        let xml = docx_document_xml(&[
            "第一段".to_string(),
            "含 & 与 < 符号".to_string(),
            "".to_string(),
        ]);
        assert!(
            xml.contains("<w:p><w:r><w:t xml:space=\"preserve\">第一段</w:t>"),
            "段落应被包裹"
        );
        assert!(
            xml.contains("&amp;") && xml.contains("&lt;"),
            "特殊字符应转义"
        );
        assert!(xml.contains("<w:p/>"), "空段落应生成自闭合段");
    }

    #[test]
    fn xlsx_sheet_xml_emits_inline_strings_with_cell_refs() {
        let rows = vec![
            vec!["姓名".to_string(), "年龄".to_string()],
            vec!["张三".to_string(), "28".to_string()],
        ];
        let xml = xlsx_sheet_xml(&rows);
        assert!(
            xml.contains(r#"<c r="A1" t="inlineStr"><is><t>姓名</t></is></c>"#),
            "A1 应映射正确"
        );
        assert!(
            xml.contains(r#"<c r="B2" t="inlineStr"><is><t>28</t></is></c>"#),
            "B2 应映射正确"
        );
    }

    #[test]
    fn pptx_slide_xml_makes_one_paragraph_per_line() {
        let xml = pptx_slide_xml("标题\n要点一\n要点二");
        assert_eq!(xml.matches("<a:p>").count(), 3, "三行应生成三个段落");
        assert!(xml.contains("要点一"), "内容应保留");
    }

    #[test]
    fn parse_csv_line_handles_quotes_and_tsv() {
        assert_eq!(parse_csv_line("a,b,c"), vec!["a", "b", "c"]);
        assert_eq!(parse_csv_line("a\tb"), vec!["a", "b"], "TSV 应按制表符切分");
        assert_eq!(
            parse_csv_line("\"a,b\",c"),
            vec!["a,b", "c"],
            "引号内的逗号不应切分"
        );
        assert_eq!(parse_csv_line("\"a\"\"b\""), vec!["a\"b"], "双引号应还原");
    }

    /// 实机回环：写 → 读，验证打包链路（ZipFile.CreateEntry）与 OOXML 结构能被
    /// 自己的 read 工具解析回来。手动执行：
    /// `cargo test write_then_read_round_trip -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "需要 PowerShell 打包临时 Office 文件"]
    async fn write_then_read_round_trip() {
        let tmp = std::env::temp_dir();
        let tag = std::process::id();

        // --- docx ---
        let docx = tmp.join(format!("pcg-write-{tag}.docx"));
        let _ = std::fs::remove_file(&docx);
        let w = WriteDocxTool
            .execute(serde_json::json!({
                "path": docx.to_string_lossy(),
                "text": "第一段：你好\n第二段：世界 <标签> & 实体"
            }))
            .await
            .expect("写 docx");
        assert!(w.success, "写 docx 失败: {:?}", w.error);
        let r = ReadDocxTool
            .execute(serde_json::json!({ "path": docx.to_string_lossy() }))
            .await
            .expect("读 docx");
        assert!(r.success, "读 docx 失败: {:?}", r.error);
        let text = r.data.unwrap()["text"].as_str().unwrap().to_string();
        assert!(text.contains("第一段：你好"), "docx 回环正文缺失: {text:?}");
        assert!(
            text.contains("<标签>") && text.contains("& 实体"),
            "特殊字符应还原: {text:?}"
        );

        // --- xlsx ---
        let xlsx = tmp.join(format!("pcg-write-{tag}.xlsx"));
        let _ = std::fs::remove_file(&xlsx);
        let w = WriteXlsxTool
            .execute(serde_json::json!({
                "path": xlsx.to_string_lossy(),
                "rows": [["姓名", "年龄"], ["张三", "28"]]
            }))
            .await
            .expect("写 xlsx");
        assert!(w.success, "写 xlsx 失败: {:?}", w.error);
        let r = ReadXlsxTool
            .execute(serde_json::json!({ "path": xlsx.to_string_lossy() }))
            .await
            .expect("读 xlsx");
        assert!(r.success, "读 xlsx 失败: {:?}", r.error);
        let data = r.data.unwrap();
        let rows = data["rows"].as_array().unwrap();
        assert_eq!(rows[0][0], "姓名");
        assert_eq!(rows[1][1], "28", "xlsx 回环单元格应一致");

        // --- pptx ---
        let pptx = tmp.join(format!("pcg-write-{tag}.pptx"));
        let _ = std::fs::remove_file(&pptx);
        let w = WritePptxTool
            .execute(serde_json::json!({
                "path": pptx.to_string_lossy(),
                "slides": ["标题页", "第二页：要点"]
            }))
            .await
            .expect("写 pptx");
        assert!(w.success, "写 pptx 失败: {:?}", w.error);
        let r = ReadPptxTool
            .execute(serde_json::json!({ "path": pptx.to_string_lossy() }))
            .await
            .expect("读 pptx");
        assert!(r.success, "读 pptx 失败: {:?}", r.error);
        let data = r.data.unwrap();
        let slides = data["slides"].as_array().unwrap();
        assert_eq!(slides.len(), 2, "pptx 应有两页");
        assert_eq!(slides[0]["text"], "标题页");

        // 清理
        let _ = std::fs::remove_file(&docx);
        let _ = std::fs::remove_file(&xlsx);
        let _ = std::fs::remove_file(&pptx);
    }

    // ============ 增量修改（纯函数） ============

    #[test]
    fn cell_ref_parses_columns_beyond_z() {
        assert_eq!(parse_cell_ref("A1"), Some((0, 1)));
        assert_eq!(parse_cell_ref("Z9"), Some((25, 9)));
        assert_eq!(parse_cell_ref("AA1"), Some((26, 1)));
        assert_eq!(parse_cell_ref("ab12"), Some((27, 12)));
    }

    #[test]
    fn cell_ref_rejects_malformed_input() {
        assert_eq!(parse_cell_ref("1A"), None, "列必须在行号前");
        assert_eq!(parse_cell_ref("A0"), None, "行号从 1 开始");
        assert_eq!(parse_cell_ref("A1B"), None, "字母不能夹在行号后面");
        assert_eq!(parse_cell_ref(""), None);
    }

    #[test]
    fn paragraph_spans_ignores_ppr_and_style_tags() {
        // <w:pPr> 与 <w:pStyle> 都以 `<w:p` 开头，不能被当成段落边界
        let xml = r#"<w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>A</w:t></w:r></w:p><w:p><w:r><w:t>B</w:t></w:r></w:p></w:body>"#;
        let spans = paragraph_spans(xml);
        assert_eq!(spans.len(), 2, "应识别出 2 个段落");
        assert_eq!(
            &xml[spans[0].0..spans[0].1],
            r#"<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>A</w:t></w:r></w:p>"#
        );
    }

    #[test]
    fn replace_text_only_touches_wt_content() {
        let xml = r#"<w:p><w:pPr><w:pStyle w:val="X"/></w:pPr><w:r><w:t>旧城市</w:t></w:r></w:p>"#;
        let (out, hits) = replace_in_text_runs(xml, "旧城市", "新城市");
        assert_eq!(hits, 1);
        assert!(out.contains("新城市"));
        // 属性值里的同名文字不能被改（否则样式引用会断）
        assert!(out.contains(r#"w:val="X""#));
    }

    #[test]
    fn replace_text_does_not_cross_runs() {
        // Word 把"你好世界"拆成两个 run 时，跨 run 的匹配注定失败——
        // 这是设计上的已知限制，测试把它固定下来，避免有人误以为能改。
        let xml = r#"<w:p><w:r><w:t>你好</w:t></w:r><w:r><w:t>世界</w:t></w:r></w:p>"#;
        let (_, hits) = replace_in_text_runs(xml, "你好世界", "再见");
        assert_eq!(hits, 0, "跨 run 的短语不应命中");
    }

    #[test]
    fn rewrite_paragraph_keeps_paragraph_properties() {
        let block = r#"<w:p w:rsidR="00A1"><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t>旧内容</w:t></w:r></w:p>"#;
        let fresh = rewrite_paragraph(block, "新内容");
        assert!(
            fresh.contains(r#"<w:jc w:val="center"/>"#),
            "段落对齐方式必须保留"
        );
        assert!(fresh.contains(r#"w:rsidR="00A1""#));
        assert!(fresh.contains("新内容"));
        assert!(!fresh.contains("旧内容"));
    }

    #[test]
    fn docx_insert_point_stays_before_sectpr() {
        let xml = r#"<w:body><w:p><w:r><w:t>A</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="11906"/></w:sectPr></w:body>"#;
        let at = docx_body_insert_point(xml).unwrap();
        assert!(
            xml[at..].starts_with("<w:sectPr"),
            "插入点必须在 sectPr 之前，否则页面设置会被新段落覆盖"
        );
    }

    #[test]
    fn set_cell_inserts_in_column_order() {
        let row = r#"<row r="1"><c r="A1" t="inlineStr"><is><t>甲</t></is></c><c r="C1" t="inlineStr"><is><t>丙</t></is></c></row>"#;
        let updated = set_cell_in_row(row, 1, "B1", &serde_json::json!("乙"));
        let b = updated.find(r#"r="B1""#).expect("应插入 B1");
        let c = updated.find(r#"r="C1""#).expect("C1 应还在");
        assert!(b < c, "B1 必须排在 C1 之前，否则 Excel 会提示文件需修复");
    }

    #[test]
    fn set_cell_replaces_existing_cell_in_place() {
        let row = r#"<row r="2"><c r="A2" t="inlineStr"><is><t>旧</t></is></c></row>"#;
        let updated = set_cell_in_row(row, 0, "A2", &serde_json::json!("新"));
        assert!(updated.contains("新"));
        assert!(!updated.contains("旧"));
        assert_eq!(updated.matches("<c ").count(), 1, "不能多出一个重复单元格");
    }

    #[test]
    fn nines_row_expands_self_closing_tag() {
        let sheet = r#"<worksheet><sheetData><row r="1"/></sheetData></worksheet>"#;
        let updated = ensure_row(sheet, 1);
        assert!(
            updated.contains("<row r=\"1\"></row>"),
            "自闭合行要展开才能放单元格"
        );
        // 第二次调用必须幂等
        assert_eq!(ensure_row(&updated, 1), updated);
    }

    #[test]
    fn xlsx_sheet_map_handles_relative_and_absolute_targets() {
        let entries = vec![
            (
                "xl/workbook.xml".to_string(),
                r#"<workbook><sheets><sheet name="总表" sheetId="1" r:id="rId1"/><sheet name="明细" sheetId="2" r:id="rId2"/></sheets></workbook>"#.to_string(),
            ),
            (
                "xl/_rels/workbook.xml.rels".to_string(),
                r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Target="/xl/worksheets/sheet2.xml"/></Relationships>"#.to_string(),
            ),
        ];
        let map = xlsx_sheet_map(&entries);
        assert_eq!(map.len(), 2);
        assert_eq!(
            map[0],
            ("总表".to_string(), "xl/worksheets/sheet1.xml".to_string())
        );
        assert_eq!(
            map[1],
            ("明细".to_string(), "xl/worksheets/sheet2.xml".to_string())
        );
    }

    /// 实机回环：生成 → 追加段落 → 文本替换 → 读回核对。
    ///
    /// 走真实 zip 读写（PowerShell），验证"就地改写"这条路真的没把
    /// 文档写坏——纯字符串测试证明不了 zip 层的正确性。
    #[tokio::test]
    #[ignore = "需要 PowerShell 打包临时 Office 文件"]
    async fn real_modify_docx_round_trip() {
        let dir = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let path = dir.join(format!("pcg-mod-{}.docx", stamp));
        let path_str = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);

        // 1) 先造一份
        let created = WriteDocxTool
            .execute(serde_json::json!({
                "path": path_str,
                "paragraphs": ["原第一段：旧项目名称", "原第二段：保留内容"]
            }))
            .await
            .expect("write_docx 应返回");
        assert!(created.success, "写 docx 失败: {:?}", created.error);

        // 2) 追加 + 替换
        let modified = ModifyDocxTool
            .execute(serde_json::json!({
                "path": path_str,
                "append_paragraphs": ["追加的第三段"],
                "replace_text": [{ "from": "旧项目名称", "to": "新项目名称" }]
            }))
            .await
            .expect("modify_docx 应返回");
        assert!(modified.success, "modify_docx 失败: {:?}", modified.error);
        let md = modified.data.unwrap();
        assert_eq!(md["appended"].as_u64(), Some(1));
        assert_eq!(md["replaced_occurrences"].as_u64(), Some(1));
        assert_eq!(
            md["paragraphs_after"].as_u64(),
            Some(3),
            "应有 3 段: {}",
            md
        );

        // 3) 读回核对
        let read = ReadDocxTool
            .execute(serde_json::json!({ "path": path_str }))
            .await
            .expect("read_docx 应返回");
        assert!(read.success, "读 docx 失败: {:?}", read.error);
        let text = read.data.unwrap()["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert!(text.contains("新项目名称"), "替换后的文字应存在: {}", text);
        assert!(!text.contains("旧项目名称"), "旧文字应已被替换");
        assert!(text.contains("追加的第三段"), "追加的段落应存在: {}", text);
        assert!(
            text.contains("保留内容"),
            "未改动的段落不能被弄丢: {}",
            text
        );

        // 4) 按序号改写第二段
        let by_index = ModifyDocxTool
            .execute(serde_json::json!({
                "path": path_str,
                "replace_paragraph": { "index": 2, "text": "第二段已整段改写" }
            }))
            .await
            .expect("modify_docx 应返回");
        assert!(by_index.success, "按序号改写失败: {:?}", by_index.error);
        let read2 = ReadDocxTool
            .execute(serde_json::json!({ "path": path_str }))
            .await
            .expect("read_docx 应返回");
        let text2 = read2.data.unwrap()["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        assert!(
            text2.contains("第二段已整段改写"),
            "序号改写结果: {}",
            text2
        );

        let _ = std::fs::remove_file(&path);
    }

    /// 实机回环：生成 xlsx → 改单元格（含新建行）→ 读回核对。
    #[tokio::test]
    #[ignore = "需要 PowerShell 打包临时 Office 文件"]
    async fn real_modify_xlsx_round_trip() {
        let dir = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let path = dir.join(format!("pcg-modx-{}.xlsx", stamp));
        let path_str = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);

        let created = WriteXlsxTool
            .execute(serde_json::json!({
                "path": path_str,
                "rows": [["姓名", "城市"], ["张三", "北京"]]
            }))
            .await
            .expect("write_xlsx 应返回");
        assert!(created.success, "写 xlsx 失败: {:?}", created.error);

        // 改已有的 B2，并在第 3 行写两个新单元格（行本来不存在）
        let modified = ModifyXlsxTool
            .execute(serde_json::json!({
                "path": path_str,
                "set_cells": [
                    { "cell": "B2", "value": "上海" },
                    { "cell": "A3", "value": "李四" },
                    { "cell": "B3", "value": 42 }
                ]
            }))
            .await
            .expect("modify_xlsx 应返回");
        assert!(modified.success, "modify_xlsx 失败: {:?}", modified.error);
        let md = modified.data.unwrap();
        assert_eq!(md["cells_written"].as_u64(), Some(3));
        assert_eq!(
            md["all_verified"].as_bool(),
            Some(true),
            "回读校验应全部命中: {}",
            md
        );

        let read = ReadXlsxTool
            .execute(serde_json::json!({ "path": path_str }))
            .await
            .expect("read_xlsx 应返回");
        assert!(read.success, "读 xlsx 失败: {:?}", read.error);
        let rows = read.data.unwrap()["rows"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        // 第一行表头必须还在（就地改写不能把原内容冲掉）
        assert_eq!(rows[0][0].as_str(), Some("姓名"));
        assert_eq!(rows[0][1].as_str(), Some("城市"));
        assert_eq!(rows[1][0].as_str(), Some("张三"), "未改的单元格应保留");
        assert_eq!(rows[1][1].as_str(), Some("上海"), "B2 应已改写");
        assert_eq!(rows[2][0].as_str(), Some("李四"), "新行应写入");
        assert_eq!(rows[2][1].as_str(), Some("42"), "数字应写入");

        let _ = std::fs::remove_file(&path);
    }
}
