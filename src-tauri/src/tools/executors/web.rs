//! 联网检索工具
//!
//! 两个能力：
//! - `web_search`：走 DuckDuckGo 的 HTML 端点，**不需要 API key**，开箱可用
//! - `url_fetch`：抓一个 URL 并把 HTML 转成纯文本
//!
//! 为什么自己做 HTML 提取而不引 `scraper`：这里只需要"标签去掉、段落留住、
//! 实体解开"三件事，手写状态机约一百行、零新依赖，而 html5ever 那套会带进
//! 一大串 crate。等真需要 CSS 选择器时再换。

use serde_json::json;

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};
use async_trait::async_trait;

/// 单次抓取的响应体上限。超过就截断——抓到一个 50MB 的页面把内存吃光，
/// 对一个"助手抓网页"的工具来说是事故而不是功能。
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/125.0 Safari/537.36 pc-guardian/0.1",
        )
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

/// 只接受 http/https。`file://`、`data:`、`ftp:` 一律拒——
/// 抓取工具不该变成读本地文件的后门（那有专门的文件工具，且受路径策略约束）。
fn ensure_http_url(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::InvalidArgument("url 不能为空".to_string()));
    }
    let lower = trimmed.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(AppError::InvalidArgument(
            "只支持 http/https 链接".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

/// 块级标签：遇到它们要断行，否则整页会被压成一行没法读
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "br"
            | "li"
            | "ul"
            | "ol"
            | "tr"
            | "table"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "main"
            | "aside"
            | "blockquote"
            | "pre"
            | "hr"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "dt"
            | "dd"
            | "figure"
            | "figcaption"
    )
}

/// 内容整块丢掉的标签（不是断行，是连里面的文字都不要）
fn is_dropped_tag(name: &str) -> bool {
    matches!(
        name,
        "script" | "style" | "noscript" | "svg" | "head" | "iframe" | "template"
    )
}

/// HTML → 纯文本。
///
/// 手写状态机而非正则：正则处理嵌套的 `<script>` 和属性里带 `>` 的标签
/// 会出错，而这类页面在真实互联网上到处都是。
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let bytes = html.as_bytes();
    let mut i = 0usize;
    // 当前是否处于某个"整块丢弃"标签内部，以及是哪个标签
    let mut dropping: Option<String> = None;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            // 注释直接跳过
            if html[i..].starts_with("<!--") {
                match html[i..].find("-->") {
                    Some(end) => {
                        i += end + 3;
                        continue;
                    }
                    None => break,
                }
            }

            let Some(close_rel) = html[i..].find('>') else {
                break;
            };
            let inner = &html[i + 1..i + close_rel];

            let is_close = inner.starts_with('/');
            let name_part = inner
                .trim_start_matches('/')
                .trim_start_matches('!')
                .trim_start_matches('?');
            // 标签名只取到第一个空白或 '/' 为止
            let name: String = name_part
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase();

            if !name.is_empty() {
                if is_close {
                    if dropping.as_deref() == Some(name.as_str()) {
                        dropping = None;
                    }
                    if is_block_tag(&name) {
                        out.push('\n');
                    }
                } else if is_dropped_tag(&name) {
                    // 自闭合的 <svg/> 之类没有内容可丢
                    let self_closing = inner.trim_end().ends_with('/');
                    if !self_closing {
                        dropping = Some(name.clone());
                    }
                } else if is_block_tag(&name) {
                    out.push('\n');
                }
            }

            i += close_rel + 1;
            continue;
        }

        // 丢弃块内部的内容整段跳过
        if let Some(tag) = &dropping {
            let needle = format!("</{tag}");
            match html[i..].to_ascii_lowercase().find(&needle) {
                Some(rel) => {
                    i += rel;
                    continue;
                }
                None => break,
            }
        }

        // 普通字符：按 UTF-8 边界整字符拷贝
        let ch = html[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }

    decode_entities(&normalize_whitespace(&out))
}

/// 把连续空白压成单个空格，但保留换行（段落结构是纯文本唯一的版式信息）
fn normalize_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = false;
    let mut newline_run = 0usize;
    for ch in text.chars() {
        match ch {
            '\n' => {
                // 先去掉行尾空格
                while out.ends_with(' ') {
                    out.pop();
                }
                newline_run += 1;
                if newline_run == 1 {
                    out.push('\n');
                }
            }
            '\r' => {}
            ' ' | '\t' | '\u{0c}' | '\u{a0}' => {
                if newline_run > 0 {
                    continue;
                }
                if !last_was_space && !out.is_empty() {
                    out.push(' ');
                    last_was_space = true;
                }
            }
            other => {
                newline_run = 0;
                last_was_space = false;
                out.push(other);
            }
        }
    }
    out.trim().to_string()
}

/// 解开常见 HTML 实体。只处理会实际影响可读性的那些，
/// 不追求完整映射表（完整表要用 html-escape 那类 crate）。
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        // 实体最长一般不超过 10 个字符（&#x1F600; 这类）
        let window_end = tail.len().min(12);
        let window = &tail[..window_end];
        let semi = window.find(';');

        let mut handled = false;
        if let Some(semi) = semi {
            let entity = &window[1..semi];
            let replacement: Option<String> = match entity {
                "amp" => Some("&".to_string()),
                "lt" => Some("<".to_string()),
                "gt" => Some(">".to_string()),
                "quot" => Some("\"".to_string()),
                "apos" => Some("'".to_string()),
                "nbsp" => Some(" ".to_string()),
                "mdash" => Some("—".to_string()),
                "ndash" => Some("–".to_string()),
                "hellip" => Some("…".to_string()),
                "middot" => Some("·".to_string()),
                "ldquo" => Some("\u{201c}".to_string()),
                "rdquo" => Some("\u{201d}".to_string()),
                "lsquo" => Some("\u{2018}".to_string()),
                "rsquo" => Some("\u{2019}".to_string()),
                _ => {
                    if let Some(num) = entity.strip_prefix('#') {
                        let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                            u32::from_str_radix(hex, 16).ok()
                        } else {
                            num.parse::<u32>().ok()
                        };
                        code.and_then(char::from_u32).map(|c| c.to_string())
                    } else {
                        None
                    }
                }
            };
            if let Some(rep) = replacement {
                out.push_str(&rep);
                rest = &tail[semi + 1..];
                handled = true;
            }
        }
        if !handled {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// 一条搜索结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// 从一段标签文本里取属性值。不处理无引号的属性——DDG 的输出一直带引号，
/// 而容忍无引号会把 `<a href=foo bar=baz>` 解错。
fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let double = format!("{name}=\"");
    let single = format!("{name}='");
    if let Some(pos) = tag.find(&double) {
        let start = pos + double.len();
        let end = tag[start..].find('"')? + start;
        return Some(tag[start..end].to_string());
    }
    if let Some(pos) = tag.find(&single) {
        let start = pos + single.len();
        let end = tag[start..].find('\'')? + start;
        return Some(tag[start..end].to_string());
    }
    None
}

/// DDG 的结果链接是 `//duckduckgo.com/l/?uddg=<百分号编码>&rut=...` 形式，
/// 要把真正的目标 URL 从 uddg 参数里解出来。
pub fn unwrap_ddg_redirect(href: &str) -> String {
    let Some(idx) = href.find("uddg=") else {
        // 不是重定向形式：补上协议头就用
        if href.starts_with("//") {
            return format!("https:{href}");
        }
        return href.to_string();
    };
    let tail = &href[idx + 5..];
    let encoded = tail.split('&').next().unwrap_or("");
    match urlencoding::decode(encoded) {
        Ok(decoded) => decoded.to_string(),
        Err(_) => encoded.to_string(),
    }
}

/// 从 DDG 的 HTML 结果页里抽搜索条目。
///
/// 容错优先：抽不满就返回抽到的部分，页面改版导致一条都抽不到时返回空表，
/// 由调用方给出"可能被限流/页面结构变了"的提示，而不是报错中断整个任务。
pub fn extract_ddg_results(html: &str, max: usize) -> Vec<SearchHit> {
    let mut hits: Vec<SearchHit> = Vec::new();
    let mut cursor = 0usize;

    while hits.len() < max {
        let rest = &html[cursor..];
        let Some(rel) = rest.find("result__a") else {
            break;
        };
        let abs = cursor + rel;

        // 定位这一条结果的 <a> 标签
        let Some(tag_start_rel) = html[..abs].rfind("<a") else {
            cursor = abs + 1;
            continue;
        };
        let tag_start = tag_start_rel;
        let Some(tag_end_rel) = html[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + tag_end_rel;
        let tag = &html[tag_start..tag_end];

        let href = extract_attr(tag, "href").unwrap_or_default();
        let text_start = tag_end + 1;
        let Some(text_end_rel) = html[text_start..].find("</a>") else {
            cursor = abs + 1;
            continue;
        };
        let text_end = text_start + text_end_rel;
        let title = html_to_text(&html[text_start..text_end]);

        // 摘要：从这一条结束位置往后找 result__snippet
        let after = text_end;
        let snippet = html[after..]
            .find("result__snippet")
            .and_then(|srel| {
                let sabs = after + srel;
                let end = html[sabs..].find("</a>").map(|e| sabs + e)?;
                Some(html_to_text(&html[sabs..end]))
            })
            .unwrap_or_default();

        let url = unwrap_ddg_redirect(&href);
        if !url.is_empty() && !title.is_empty() {
            hits.push(SearchHit {
                title,
                url,
                snippet,
            });
        }

        cursor = abs + 10;
    }

    hits
}

// ============================================================
// web_search
// ============================================================

pub struct WebSearchTool;

#[async_trait]
impl AgentTool for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "在互联网上搜索。返回若干条结果的标题、链接和摘要。\
         查资料、找最新信息、确认事实时先用它，比直接爬页面快得多；\
         需要正文再用 url_fetch 抓具体链接。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "搜索关键词"},
                "max_results": {
                    "type": "integer",
                    "description": "返回条数，默认 8，上限 20"
                }
            },
            "required": ["query"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("缺少参数 query".to_string()))?;
        let max = args
            .get("max_results")
            .and_then(|v| v.as_i64())
            .unwrap_or(8)
            .clamp(1, 20) as usize;

        let encoded = urlencoding::encode(query);
        let endpoint = format!("https://html.duckduckgo.com/html/?q={encoded}");

        let resp = match build_client().get(&endpoint).send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult::err(
                    "NETWORK_ERROR",
                    &format!("搜索请求失败：{e}"),
                ))
            }
        };

        let status = resp.status();
        if !status.is_success() {
            return Ok(ToolResult::err(
                "NETWORK_ERROR",
                &format!("搜索端点返回 HTTP {status}"),
            ));
        }

        let body = match resp.text().await {
            Ok(t) => t,
            Err(e) => {
                return Ok(ToolResult::err(
                    "NETWORK_ERROR",
                    &format!("读取搜索响应失败：{e}"),
                ))
            }
        };

        let hits = extract_ddg_results(&body, max);
        if hits.is_empty() {
            // 空结果有两种可能：真的没结果，或页面结构变了/被限流。
            // 不报错，把原因写清楚让模型自己决定要不要换个查法。
            return Ok(ToolResult::ok(json!({
                "query": query,
                "results": [],
                "note": "没有解析到结果：可能是关键词没有匹配，也可能检索端点临时改了页面结构或被限流。可以换个关键词，或直接用 url_fetch 访问目标站点。"
            })));
        }

        Ok(ToolResult::ok(json!({
            "query": query,
            "count": hits.len(),
            "results": hits.iter().map(|h| json!({
                "title": h.title,
                "url": h.url,
                "snippet": h.snippet,
            })).collect::<Vec<_>>(),
        })))
    }
}

// ============================================================
// url_fetch
// ============================================================

pub struct UrlFetchTool;

#[async_trait]
impl AgentTool for UrlFetchTool {
    fn name(&self) -> &'static str {
        "url_fetch"
    }

    fn description(&self) -> &'static str {
        "抓取一个网页并把内容转成纯文本。返回页面标题和正文，超长会被截断。\
         只取静态 HTML——需要登录、需要执行 JavaScript 才能看到内容的页面，\
         用浏览器工具（browser_navigate）而不是它。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "description": "要抓取的 http/https 链接"},
                "max_chars": {
                    "type": "integer",
                    "description": "正文最多返回多少字符，默认 8000，上限 50000"
                }
            },
            "required": ["url"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let raw = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::InvalidArgument("缺少参数 url".to_string()))?;
        let url = ensure_http_url(raw)?;
        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_i64())
            .unwrap_or(8000)
            .clamp(200, 50_000) as usize;

        let resp = match build_client().get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult::err(
                    "NETWORK_ERROR",
                    &format!("请求失败：{e}"),
                ))
            }
        };

        let status = resp.status();
        let final_url = resp.url().to_string();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolResult::err(
                    "NETWORK_ERROR",
                    &format!("读取响应体失败：{e}"),
                ))
            }
        };

        let truncated_bytes = bytes.len() > MAX_BODY_BYTES;
        let slice = if truncated_bytes {
            &bytes[..MAX_BODY_BYTES]
        } else {
            &bytes[..]
        };

        // 非文本类型（PDF、图片、二进制）不要硬转字符串——那会产出乱码，
        // 还会让模型以为抓到了内容。直接告诉它换工具。
        if !content_type.is_empty()
            && !content_type.contains("text")
            && !content_type.contains("json")
            && !content_type.contains("xml")
        {
            return Ok(ToolResult::ok(json!({
                "url": final_url,
                "status": status.as_u16(),
                "content_type": content_type,
                "note": "这个链接不是文本内容，url_fetch 无法直接读。PDF 用 read_pdf，图片用 OCR 工具。",
            })));
        }

        // lossy：网页里偶发坏字节不该让整次抓取失败
        let raw_text = String::from_utf8_lossy(slice).to_string();
        let looks_html = content_type.contains("html")
            || raw_text.trim_start().starts_with("<!DOCTYPE")
            || raw_text.trim_start().starts_with("<html")
            || raw_text.contains("<body");

        let title = extract_title(&raw_text);
        let text = if looks_html {
            html_to_text(&raw_text)
        } else {
            raw_text.trim().to_string()
        };

        let total_chars = text.chars().count();
        let body: String = if total_chars > max_chars {
            text.chars().take(max_chars).collect()
        } else {
            text
        };

        Ok(ToolResult::ok(json!({
            "url": final_url,
            "status": status.as_u16(),
            "title": title,
            "text": body,
            "chars": body.chars().count(),
            "truncated": total_chars > max_chars || truncated_bytes,
        })))
    }
}

fn extract_title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(start) = lower.find("<title") else {
        return String::new();
    };
    let Some(open_end) = lower[start..].find('>') else {
        return String::new();
    };
    let content_start = start + open_end + 1;
    let Some(close_rel) = lower[content_start..].find("</title>") else {
        return String::new();
    };
    let raw = &html[content_start..content_start + close_rel];
    html_to_text(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_and_keeps_paragraphs() {
        let html = "<html><body><h1>标题</h1><p>第一段</p><p>第二段</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("标题"));
        assert!(text.contains("第一段"));
        // 段落之间必须有换行，否则整页挤成一行
        assert!(text.contains("第一段\n") || text.contains("第一段\n\n"));
    }

    #[test]
    fn drops_script_and_style_contents() {
        let html = r#"<div>可见<script>var secret = "不该出现";</script><style>.a{color:red}</style>文字</div>"#;
        let text = html_to_text(html);
        assert!(text.contains("可见"));
        assert!(text.contains("文字"));
        assert!(!text.contains("不该出现"), "script 内容漏出来了：{text}");
        assert!(!text.contains("color:red"), "style 内容漏出来了：{text}");
    }

    #[test]
    fn decodes_common_entities() {
        assert_eq!(decode_entities("a &amp; b"), "a & b");
        assert_eq!(decode_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(decode_entities("&#65;&#x42;"), "AB");
        // 不认识又没有分号的裸 & 必须原样保留，不能被吃掉
        assert_eq!(decode_entities("Tom & Jerry"), "Tom & Jerry");
    }

    #[test]
    fn collapses_whitespace_but_keeps_newlines() {
        let text = normalize_whitespace("a    b\n\n\n\nc   ");
        assert_eq!(text, "a b\nc");
    }

    #[test]
    fn unwraps_ddg_redirect_links() {
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa%3Fb%3D1&rut=xyz";
        assert_eq!(unwrap_ddg_redirect(href), "https://example.com/a?b=1");
        // 不是重定向形式就原样返回
        assert_eq!(
            unwrap_ddg_redirect("https://example.com/x"),
            "https://example.com/x"
        );
    }

    #[test]
    fn extracts_title() {
        assert_eq!(
            extract_title("<html><head><title>页面 &amp; 标题</title></head></html>"),
            "页面 & 标题"
        );
        // 没有 title 时返回空串，不能 panic
        assert_eq!(extract_title("<html></html>"), "");
    }

    #[test]
    fn rejects_non_http_urls() {
        assert!(ensure_http_url("file:///C:/Windows/win.ini").is_err());
        assert!(ensure_http_url("javascript:alert(1)").is_err());
        assert!(ensure_http_url("").is_err());
        assert!(ensure_http_url("https://example.com").is_ok());
    }

    #[test]
    fn parses_ddg_result_block() {
        let html = r#"
        <div class="result">
          <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc">示例<strong>标题</strong></a>
          <a class="result__snippet" href="https://example.com/page">这是<b>摘要</b>内容</a>
        </div>
        "#;
        let hits = extract_ddg_results(html, 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].url, "https://example.com/page");
        assert_eq!(hits[0].title, "示例标题");
        assert!(hits[0].snippet.contains("摘要"));
    }

    #[test]
    fn empty_page_yields_no_hits_instead_of_panicking() {
        assert!(extract_ddg_results("<html><body>no results</body></html>", 5).is_empty());
        assert!(extract_ddg_results("", 5).is_empty());
    }

    /// 实机：真的搜一次，验证检索端点仍然返回可解析的结构。
    ///
    /// 跑法：`cargo test --lib real_web_search -- --ignored --nocapture`
    ///
    /// 这个测试的价值不在"代码有没有 bug"，而在**发现外部页面改版**：
    /// 解析器全是按当前 DDG 的 HTML 结构写的，哪天它改了类名，
    /// 单元测试仍然全绿，只有这里的实机会红。
    #[tokio::test]
    #[ignore]
    async fn real_web_search_returns_results() {
        let out = WebSearchTool
            .execute(json!({"query": "rust programming language", "max_results": 5}))
            .await
            .expect("搜索执行失败");
        assert!(out.success, "搜索返回失败：{:?}", out.error);
        let data = out.data.expect("没有 data");
        let results = data["results"].as_array().expect("results 不是数组");
        for r in results {
            println!(
                "- {} | {}",
                r["title"].as_str().unwrap_or(""),
                r["url"].as_str().unwrap_or("")
            );
        }
        assert!(
            !results.is_empty(),
            "一条结果都没解析出来——检索端点的页面结构可能变了。原始响应片段：{:?}",
            data["note"]
        );
        // 解出来的链接必须是完整 http(s)，不能残留 DDG 的重定向外壳
        for r in results {
            let url = r["url"].as_str().unwrap_or("");
            assert!(
                url.starts_with("http://") || url.starts_with("https://"),
                "链接没被正确还原：{url}"
            );
        }
    }

    /// 实机：抓一个真实网页，验证 HTML→文本 在真实页面上可用。
    ///
    /// 断言只锁**结构性**特征（状态码、标题、正文非空、标签清干净），
    /// 不锁具体正文——example.com 的正文已经改过一次版，锁文字会让这个测试
    /// 因为别人改站而变红，那样的红是噪音。
    #[tokio::test]
    #[ignore]
    async fn real_url_fetch_extracts_text() {
        let out = UrlFetchTool
            .execute(json!({"url": "https://example.com", "max_chars": 2000}))
            .await
            .expect("抓取执行失败");
        assert!(out.success, "抓取返回失败：{:?}", out.error);
        let data = out.data.expect("没有 data");
        println!("status={} title={}", data["status"], data["title"]);
        println!("text={}", data["text"].as_str().unwrap_or(""));

        assert_eq!(data["status"].as_u64(), Some(200));
        assert!(
            data["title"].as_str().unwrap_or("").contains("Example"),
            "标题没抽出来：{:?}",
            data["title"]
        );

        let text = data["text"].as_str().unwrap_or("");
        assert!(!text.trim().is_empty(), "正文是空的");
        assert!(
            !text.contains('<') && !text.contains("text-align"),
            "正文里还残留 HTML 标签或样式：{text}"
        );
    }
}
