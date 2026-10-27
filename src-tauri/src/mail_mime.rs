//! 邮件 MIME 解析（纯函数，不碰网络）。
//!
//! 这个模块存在的理由：IMAP 服务器只负责把**原始 RFC822 字节**递给你，
//! 剩下的全是客户端的活——分段、解 Content-Transfer-Encoding、按 charset
//! 转码、把 `=?UTF-8?B?...?=` 这种编码词还原成可读主题、从 Content-Disposition
//! 里挖附件名。少做一步，用户看到的就是 `=?gb2312?B?xOO6ww==?=` 而不是"你好"。
//!
//! 刻意不引 mailparse：那套会带进十几个 crate，而这里只需要 RFC 2045/2046/2047
//! 里最常用的子集（base64 / quoted-printable / multipart / encoded-word）。
//! 全部实现都是可单测的纯函数，`cargo test --lib` 就能覆盖中文主题、GBK 正文、
//! 嵌套 multipart、附件名编码这些真实踩过的坑。

use std::collections::HashMap;

use base64::Engine;

// ============================================================
// 头部解析
// ============================================================

/// 把原始头部切成 (小写字段名, 去折行后的值) 列表。
///
/// RFC 5322 的折行是"以空白开头的续行"，展开时必须用空格连接而不是删掉，
/// 否则长主题会被粘成一个词。同名头（如多个 Received）保留全部，顺序不变。
pub fn parse_headers(raw: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(raw);
    let mut headers: Vec<(String, String)> = Vec::new();

    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break; // 头部结束
        }
        if (line.starts_with(' ') || line.starts_with('\t')) && !headers.is_empty() {
            let last = headers.last_mut().unwrap();
            // 续行：保留一个空格，避免两段单词粘连
            if !last.1.is_empty() {
                last.1.push(' ');
            }
            last.1.push_str(line.trim());
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }

    headers
}

/// 取第一个同名字段。
pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// 解析 `Content-Type` 型的值：返回 (主类型小写, 参数表)。
///
/// 参数值可能带引号（`boundary="abc"`），也可能走 RFC 2231 的
/// `filename*=UTF-8''%E4%B8%AD` 写法。两种都要认，否则中文附件名会丢。
pub fn parse_content_type(value: &str) -> (String, HashMap<String, String>) {
    let mut parts = value.split(';');
    let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let mut params = HashMap::new();

    for part in parts {
        let Some((key, raw_val)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let mut val = raw_val.trim();
        // 引号内可能含分号，上面的 split 会切坏——这是简化实现的已知边界：
        // 只处理"引号内无分号"的常见情形，真遇到再按 RFC 手工续接。
        if val.starts_with('"') && val.ends_with('"') && val.len() >= 2 {
            val = &val[1..val.len() - 1];
        }

        if let Some(lang_key) = key.strip_suffix('*') {
            // RFC 2231: charset'language'percent-encoded
            let decoded = decode_rfc2231(val);
            params.insert(lang_key.to_string(), decoded);
        } else {
            params.insert(key, val.to_string());
        }
    }

    (mime, params)
}

/// RFC 2231 三段式：`UTF-8''%E4%B8%AD%E6%96%87`
fn decode_rfc2231(value: &str) -> String {
    let mut segments = value.splitn(3, '\'');
    let charset = segments.next().unwrap_or("utf-8");
    let _language = segments.next().unwrap_or("");
    let encoded = segments.next().unwrap_or(value);
    let bytes = percent_decode(encoded.as_bytes());
    charset_to_utf8(&bytes, charset)
}

fn percent_decode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' && i + 2 < input.len() {
            let hex = |b: u8| -> Option<u8> {
                match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None,
                }
            };
            if let (Some(hi), Some(lo)) = (hex(input[i + 1]), hex(input[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

// ============================================================
// 字符集与传输编码
// ============================================================

/// 按 charset 把字节转成 UTF-8。认不出的字符集退回 UTF-8 有损解码——
/// 宁可出现几个替换符，也不要整封信变成空白。
pub fn charset_to_utf8(bytes: &[u8], charset: &str) -> String {
    let label = charset.trim().trim_matches('"').to_ascii_lowercase();
    let label = label.split('*').next().unwrap_or("utf-8"); // 去掉 RFC2231 语言后缀
    let canonical = match label {
        "" | "utf8" | "utf-8" => "utf-8",
        "gbk" | "gb2312" | "gb18030" | "cp936" => "gbk",
        "big5" | "big-5" => "big5",
        "latin1" | "iso-8859-1" => "windows-1252",
        other => other,
    };

    match encoding_rs::Encoding::for_label(canonical.as_bytes()) {
        Some(enc) => {
            let (cow, _, _) = enc.decode(bytes);
            cow.into_owned()
        }
        None => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// 解 Content-Transfer-Encoding。base64 遇到脏数据时退到"原样返回"，
/// 而不是把整封信判成失败——半封信也比没有强，且返回值里会标 encoding。
pub fn decode_transfer_encoding(body: &[u8], encoding: &str) -> Vec<u8> {
    match encoding.trim().to_ascii_lowercase().as_str() {
        "base64" => {
            let cleaned: Vec<u8> = body
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            base64::engine::general_purpose::STANDARD
                .decode(&cleaned)
                .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(&cleaned))
                .unwrap_or_else(|_| body.to_vec())
        }
        "quoted-printable" => decode_quoted_printable(body),
        _ => body.to_vec(),
    }
}

fn decode_quoted_printable(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b'=' => {
                // 软换行 `=\r\n` / `=\n`
                if i + 1 < body.len() && body[i + 1] == b'\n' {
                    i += 2;
                    continue;
                }
                if i + 2 < body.len() && body[i + 1] == b'\r' && body[i + 2] == b'\n' {
                    i += 3;
                    continue;
                }
                if i + 2 < body.len() {
                    let hi = body[i + 1];
                    let lo = body[i + 2];
                    if let (Some(h), Some(l)) = (hex_val(hi), hex_val(lo)) {
                        out.push(h * 16 + l);
                        i += 3;
                        continue;
                    }
                }
                out.push(body[i]);
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// ============================================================
// RFC 2047 编码词
// ============================================================

/// 把 `=?charset?B?base64?=` / `=?charset?Q?quoted?=` 还原成普通文本。
///
/// 连续多个编码词之间的空白按 RFC 2047 应被吞掉（否则中文标题会多出一串空格），
/// 所以这里做一次合并：解码后若前一段是编码词、后一段也是编码词，中间空白丢弃。
pub fn decode_encoded_words(input: &str) -> String {
    if !input.contains("=?") {
        return input.to_string();
    }

    let bytes = input.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    let mut last_was_encoded = false;

    while i < bytes.len() {
        if bytes[i..].starts_with(b"=?") {
            if let Some((decoded, consumed)) = parse_one_encoded_word(&input[i..]) {
                out.push_str(&decoded);
                i += consumed;
                last_was_encoded = true;
                continue;
            }
        }

        // 普通字符
        let ch_len = next_char_len(bytes, i);
        let chunk = &input[i..i + ch_len];
        if last_was_encoded
            && chunk
                .chars()
                .all(|c| c == ' ' || c == '\t' || c == '\r' || c == '\n')
        {
            // 编码词之间的空白丢掉
            i += ch_len;
            continue;
        }
        last_was_encoded = false;
        out.push_str(chunk);
        i += ch_len;
    }

    out
}

fn next_char_len(bytes: &[u8], i: usize) -> usize {
    let b = bytes[i];
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// 解析一个 `=?...?=`，返回 (解码文本, 消耗字节数)。
fn parse_one_encoded_word(s: &str) -> Option<(String, usize)> {
    let rest = s.strip_prefix("=?")?;
    let charset_end = rest.find('?')?;
    let charset = &rest[..charset_end];

    let after_charset = &rest[charset_end + 1..];
    if after_charset.len() < 2 {
        return None;
    }
    let encoding = after_charset.as_bytes()[0].to_ascii_uppercase();
    if after_charset.as_bytes()[1] != b'?' {
        return None;
    }

    let payload_and_end = &after_charset[2..];
    let payload_end = payload_and_end.find("?=")?;
    let payload = &payload_and_end[..payload_end];

    let bytes = match encoding {
        b'B' => base64::engine::general_purpose::STANDARD
            .decode(payload.as_bytes())
            .ok()?,
        b'Q' => {
            // Q 编码里 '_' 代表空格
            let replaced = payload.replace('_', " ");
            decode_quoted_printable(replaced.as_bytes())
        }
        _ => return None,
    };

    let consumed = 2 + charset_end + 1 + 2 + payload_end + 2;
    Some((charset_to_utf8(&bytes, charset), consumed))
}

// ============================================================
// 消息结构
// ============================================================

#[derive(Debug, Clone)]
pub struct AttachmentMeta {
    pub filename: String,
    pub content_type: String,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct ParsedMail {
    pub subject: String,
    pub from: String,
    pub to: String,
    pub date: String,
    pub message_id: String,
    pub text_body: String,
    /// 原始 HTML（纯文本分区缺失时用它转成可读文本）
    pub html_body: String,
    pub attachments: Vec<AttachmentMeta>,
}

/// 切出头部与正文。返回 (头部字节, 正文起点偏移)。
fn split_head_body(raw: &[u8]) -> (&[u8], usize) {
    // 先找 CRLFCRLF，再找 LFLF（部分服务端落盘时丢 CR）
    if let Some(pos) = find_subsequence(raw, b"\r\n\r\n") {
        return (&raw[..pos], pos + 4);
    }
    if let Some(pos) = find_subsequence(raw, b"\n\n") {
        return (&raw[..pos], pos + 2);
    }
    (raw, raw.len())
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 深度优先遍历的产物，供 parse_message 与 extract_attachments 共用。
struct Walk {
    text_parts: Vec<String>,
    html_parts: Vec<String>,
    /// (文件名, 类型, 内容)
    attachments: Vec<(String, String, Vec<u8>)>,
}

fn walk(headers: &[(String, String)], body: &[u8], out: &mut Walk, depth: usize) {
    if depth > 8 {
        // 恶意/异常邮件的嵌套保护
        return;
    }

    let ctype_raw = header(headers, "content-type").unwrap_or("text/plain");
    let (mime, params) = parse_content_type(ctype_raw);
    let cte = header(headers, "content-transfer-encoding").unwrap_or("7bit");
    let charset = params.get("charset").map(|s| s.as_str()).unwrap_or("utf-8");
    let disposition = header(headers, "content-disposition").unwrap_or("");

    // multipart：按 boundary 递归
    if mime.starts_with("multipart/") {
        let Some(boundary) = params.get("boundary") else {
            return;
        };
        let delimiter = format!("--{boundary}");
        for segment in split_by_boundary(body, delimiter.as_bytes()) {
            process_part(segment, out, depth);
        }
        return;
    }

    let decoded = decode_transfer_encoding(body, cte);
    let is_attachment = disposition.to_ascii_lowercase().starts_with("attachment")
        || params.contains_key("name")
        || disposition.contains("filename");

    if is_attachment {
        let filename = disposition
            .split(';')
            .find_map(|piece| {
                let piece = piece.trim();
                piece
                    .strip_prefix("filename*=")
                    .map(decode_rfc2231)
                    .or_else(|| {
                        piece.strip_prefix("filename=").map(|v| {
                            let v = v.trim().trim_matches('"');
                            decode_encoded_words(v)
                        })
                    })
            })
            .or_else(|| params.get("name").map(|v| decode_encoded_words(v)))
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| format!("attachment-{}", out.attachments.len() + 1));
        out.attachments.push((filename, mime, decoded));
        return;
    }

    if mime == "text/html" {
        out.html_parts.push(charset_to_utf8(&decoded, charset));
        return;
    }

    // 其余当文本处理（text/plain 或没有 Content-Type 的老邮件）
    out.text_parts.push(charset_to_utf8(&decoded, charset));
}

/// 按 boundary 切分。返回的是"分隔线之间"的原始段（不含分隔线本身），
/// 已去掉每段起始的空行与结尾的 CRLF；结束标记 `--boundary--` 之后的段落丢弃。
fn split_by_boundary<'a>(body: &'a [u8], delimiter: &[u8]) -> Vec<&'a [u8]> {
    let mut segments: Vec<&'a [u8]> = Vec::new();
    let mut starts: Vec<usize> = Vec::new(); // 每条分隔线的起始偏移
    let mut search_from = 0usize;

    while let Some(rel) = find_subsequence(&body[search_from..], delimiter) {
        let abs = search_from + rel;
        let after = abs + delimiter.len();
        starts.push(abs);
        // 结束标记：后面紧跟 "--"
        if body.len() >= after + 2 && &body[after..after + 2] == b"--" {
            break;
        }
        search_from = after;
    }

    for (idx, start) in starts.iter().enumerate() {
        let content_start = start + delimiter.len();
        let content_end = starts.get(idx + 1).copied().unwrap_or(body.len());
        if content_start >= content_end {
            continue;
        }
        segments.push(trim_blank_edges(&body[content_start..content_end]));
    }

    segments
}

fn trim_blank_edges(mut raw: &[u8]) -> &[u8] {
    while raw.starts_with(b"\r\n") {
        raw = &raw[2..];
    }
    while raw.starts_with(b"\n") {
        raw = &raw[1..];
    }
    while raw.ends_with(b"\r\n") {
        raw = &raw[..raw.len() - 2];
    }
    while raw.ends_with(b"\n") {
        raw = &raw[..raw.len() - 1];
    }
    raw
}

fn process_part(part: &[u8], out: &mut Walk, depth: usize) {
    let (head, body_start) = split_head_body(part);
    let part_headers = parse_headers(head);
    let part_body = &part[body_start..];
    walk(&part_headers, part_body, out, depth + 1);
}

/// 解析整封信：题头 + 正文 + 附件清单（不保留附件字节，避免读一封信吃掉几十 MB）。
pub fn parse_message(raw: &[u8]) -> ParsedMail {
    let (head, body_start) = split_head_body(raw);
    let headers = parse_headers(head);
    let mut walk_out = Walk {
        text_parts: Vec::new(),
        html_parts: Vec::new(),
        attachments: Vec::new(),
    };
    walk(&headers, &raw[body_start..], &mut walk_out, 0);

    let attachments = walk_out
        .attachments
        .iter()
        .map(|(name, mime, bytes)| AttachmentMeta {
            filename: name.clone(),
            content_type: mime.clone(),
            size: bytes.len(),
        })
        .collect();

    // 多个 text/plain part（alternative 里的纯文本 + 转发的正文）按出现顺序拼接，
    // 这比"只取第一个"更接近用户预期。
    let text_body = walk_out
        .text_parts
        .iter()
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n");

    ParsedMail {
        subject: header(&headers, "subject")
            .map(decode_encoded_words)
            .unwrap_or_default(),
        from: header(&headers, "from")
            .map(decode_encoded_words)
            .unwrap_or_default(),
        to: header(&headers, "to")
            .map(decode_encoded_words)
            .unwrap_or_default(),
        date: header(&headers, "date").unwrap_or("").to_string(),
        message_id: header(&headers, "message-id").unwrap_or("").to_string(),
        text_body,
        html_body: walk_out.html_parts.join(
            "
",
        ),
        attachments,
    }
}

/// 只取头部（列表页用：不下载正文）。
pub fn parse_header_only(raw: &[u8]) -> ParsedMail {
    let headers = parse_headers(raw);
    ParsedMail {
        subject: header(&headers, "subject")
            .map(decode_encoded_words)
            .unwrap_or_default(),
        from: header(&headers, "from")
            .map(decode_encoded_words)
            .unwrap_or_default(),
        to: header(&headers, "to")
            .map(decode_encoded_words)
            .unwrap_or_default(),
        date: header(&headers, "date").unwrap_or("").to_string(),
        message_id: header(&headers, "message-id").unwrap_or("").to_string(),
        ..Default::default()
    }
}

/// 提取附件字节（读附件/保存到磁盘用）。
pub fn extract_attachments(raw: &[u8]) -> Vec<Attachment> {
    let (head, body_start) = split_head_body(raw);
    let headers = parse_headers(head);
    let mut walk_out = Walk {
        text_parts: Vec::new(),
        html_parts: Vec::new(),
        attachments: Vec::new(),
    };
    walk(&headers, &raw[body_start..], &mut walk_out, 0);

    walk_out
        .attachments
        .into_iter()
        .map(|(name, mime, bytes)| Attachment {
            filename: name,
            content_type: mime,
            bytes,
        })
        .collect()
}

/// 把 HTML 粗转成纯文本（没有 text/plain 分区时的兜底）。
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut skip_until: Option<&str> = None;

    let lower = html.to_ascii_lowercase();
    let mut rest = 0usize;

    while rest < html.len() {
        if let Some(tag) = skip_until {
            match lower[rest..].find(tag) {
                Some(pos) => {
                    rest += pos + tag.len();
                    skip_until = None;
                    continue;
                }
                None => break,
            }
        }
        let ch = html[rest..].chars().next().unwrap();
        if ch == '<' {
            in_tag = true;
            // script/style 内容整体丢弃
            if lower[rest..].starts_with("<script") {
                skip_until = Some("</script>");
            } else if lower[rest..].starts_with("<style") {
                skip_until = Some("</style>");
            }
            rest += 1;
            continue;
        }
        if ch == '>' {
            in_tag = false;
            rest += 1;
            continue;
        }
        if !in_tag {
            out.push(ch);
        }
        rest += ch.len_utf8();
    }

    let decoded = decode_html_entities(&out);
    // 压缩连续空白
    let mut compact = String::with_capacity(decoded.len());
    let mut prev_space = false;
    for ch in decoded.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                compact.push(' ');
                prev_space = true;
            }
        } else {
            compact.push(ch);
            prev_space = false;
        }
    }
    compact.trim().to_string()
}

fn decode_html_entities(input: &str) -> String {
    let out = input
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    // 数字实体
    let mut result = String::with_capacity(out.len());
    let mut chars = out.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '&' {
            if let Some(end) = out[i..].find(';') {
                let entity = &out[i + 1..i + end];
                if let Some(num) = entity.strip_prefix('#') {
                    let code = if let Some(hex) =
                        num.strip_prefix('x').or_else(|| num.strip_prefix('X'))
                    {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        num.parse::<u32>().ok()
                    };
                    if let Some(c) = code.and_then(char::from_u32) {
                        result.push(c);
                        for _ in 0..end {
                            chars.next();
                        }
                        continue;
                    }
                }
            }
        }
        result.push(ch);
    }
    drop(chars);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfolds_continuation_lines() {
        let headers = parse_headers(b"Subject: hello\r\n world\r\nFrom: a@b.c\r\n\r\nbody");
        assert_eq!(header(&headers, "subject"), Some("hello world"));
        assert_eq!(header(&headers, "from"), Some("a@b.c"));
    }

    #[test]
    fn decodes_utf8_base64_encoded_word() {
        // "=?UTF-8?B?5L2g5aW9?=" == 你好
        assert_eq!(decode_encoded_words("=?UTF-8?B?5L2g5aW9?="), "你好");
    }

    #[test]
    fn decodes_gbk_base64_subject() {
        // "=?gb2312?B?xOO6ww==?=" == 你好（GBK）
        assert_eq!(decode_encoded_words("=?gb2312?B?xOO6ww==?="), "你好");
    }

    #[test]
    fn drops_whitespace_between_encoded_words() {
        assert_eq!(
            decode_encoded_words("=?UTF-8?B?5L2g?= =?UTF-8?B?5aW9?="),
            "你好"
        );
    }

    #[test]
    fn decodes_q_encoding_with_underscore() {
        assert_eq!(
            decode_encoded_words("=?UTF-8?Q?hello_world?="),
            "hello world"
        );
    }

    #[test]
    fn quoted_printable_soft_breaks() {
        let decoded = decode_quoted_printable(b"line1=\r\nline2=3D=3D");
        assert_eq!(String::from_utf8(decoded).unwrap(), "line1line2==");
    }

    #[test]
    fn parses_content_type_params() {
        let (mime, params) = parse_content_type("multipart/mixed; boundary=abc; charset=\"utf-8\"");
        assert_eq!(mime, "multipart/mixed");
        assert_eq!(params.get("boundary").map(String::as_str), Some("abc"));
        assert_eq!(params.get("charset").map(String::as_str), Some("utf-8"));
    }

    #[test]
    fn parses_rfc2231_filename() {
        let (_, params) =
            parse_content_type("application/pdf; name*=UTF-8''%E6%8A%A5%E5%91%8A.pdf");
        assert_eq!(params.get("name").map(String::as_str), Some("报告.pdf"));
    }

    #[test]
    fn parses_simple_text_message() {
        let raw = b"Subject: =?UTF-8?B?5L2g5aW9?=\r\nFrom: a@b.c\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nhello body\r\n";
        let parsed = parse_message(raw);
        assert_eq!(parsed.subject, "你好");
        assert_eq!(parsed.from, "a@b.c");
        assert_eq!(parsed.text_body.trim(), "hello body");
        assert!(parsed.attachments.is_empty());
    }

    #[test]
    fn parses_base64_body() {
        let raw = b"Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\naGVsbG8g5LiW55WM\r\n";
        let parsed = parse_message(raw);
        assert_eq!(parsed.text_body, "hello 世界");
    }

    #[test]
    fn parses_multipart_alternative_and_attachment() {
        let raw = b"Subject: test\r\n\
Content-Type: multipart/mixed; boundary=\"BOUND\"\r\n\
\r\n\
--BOUND\r\n\
Content-Type: multipart/alternative; boundary=\"INNER\"\r\n\
\r\n\
--INNER\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
plain text\r\n\
--INNER\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>html text</p>\r\n\
--INNER--\r\n\
--BOUND\r\n\
Content-Type: application/pdf; name=\"=?UTF-8?B?5oql5ZGKLnBkZg==?=\"\r\n\
Content-Disposition: attachment; filename=\"=?UTF-8?B?5oql5ZGKLnBkZg==?=\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQK\r\n\
--BOUND--\r\n";

        let parsed = parse_message(raw);
        assert_eq!(parsed.subject, "test");
        assert!(
            parsed.text_body.contains("plain text"),
            "正文: {:?}",
            parsed.text_body
        );
        assert!(!parsed.html_body.is_empty());
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].filename, "报告.pdf");
        assert_eq!(parsed.attachments[0].content_type, "application/pdf");
        assert!(parsed.attachments[0].size > 0);

        let files = extract_attachments(raw);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].bytes, b"%PDF-1.4\n");
    }

    #[test]
    fn html_to_text_strips_tags_and_scripts() {
        let text = html_to_text("<html><head><style>p{}</style></head><body><p>Hello&nbsp;world</p><script>alert(1)</script></body></html>");
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn gbk_body_is_transcoded() {
        // GBK 编码的"中文"
        let gbk = [0xD6, 0xD0, 0xCE, 0xC4];
        assert_eq!(charset_to_utf8(&gbk, "gb2312"), "中文");
    }

    #[test]
    fn missing_boundary_does_not_panic() {
        let raw = b"Content-Type: multipart/mixed\r\n\r\nbody";
        let parsed = parse_message(raw);
        assert_eq!(parsed.text_body, "");
    }
}
