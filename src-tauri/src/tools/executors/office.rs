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
use crate::tools::executors::os_extra::{literal, run_powershell_b64};
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
        let Some(end) = after[body_start..].find(&close) else { break };
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

        let Some((pos, needle, kind)) = next else { break };
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
                let Some(end) = body.find("</w:t>") else { break };
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
                    .and_then(|file| file.trim_start_matches("slide").trim_end_matches(".xml").parse::<u32>().ok())
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
                    .and_then(|f| f.trim_start_matches("sheet").trim_end_matches(".xml").parse::<u32>().ok())
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
                &format!("只有 {} 张工作表，请求的是第 {} 张", sheet_count, sheet_index),
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
        let shared_xml = r#"<sst><si><t>姓名</t></si><si><t>年龄</t></si><si><t>张三</t></si></sst>"#;
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
}
