//! 需求文档的确定性清洗。
//!
//! 从 PDF / DOCX 抽出来的文本通常带着页眉页脚、页码、硬换行、全角字符、零宽字符、
//! 超长 base64 等内容。这些东西不会让程序报错，但会污染提示词、吃掉上下文预算，
//! 是「文档格式导致生成失败」的主要来源。这里先用纯规则把它们处理干净，不做任何模型调用。

use std::collections::HashMap;

/// 清洗结果的长度上限，防止异常文档把后续流程拖垮。
/// 清洗结果的长度上限，防止异常文档把后续流程拖垮。
/// 清洗是 O(n) 的纯规则处理，十万字也是毫秒级，这里只做兜底。
pub const MAX_CLEAN_CHARS: usize = 400_000;

/// 段落合并时要断开的句末标点。
const SENTENCE_END: [char; 13] = [
    '。', '！', '？', '；', '：', '.', '!', '?', ';', ':', '）', ')', '】',
];

/// 长于该值的连续非空白串会被视作编码/二进制内容并折叠。
const MAX_TOKEN_CHARS: usize = 200;

/// 参与「重复行」判定的最大行长度。
const REPEAT_MAX_LINE_CHARS: usize = 24;

/// 一行重复到该次数以上就被判定为页眉页脚。
const REPEAT_MIN_COUNT: usize = 3;

/// 取字符串开头的若干字符。
pub fn take_head(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}

/// 取字符串末尾的若干字符。
pub fn take_tail(text: &str, count: usize) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let start = chars.len().saturating_sub(count);
    chars[start..].iter().collect()
}

/// 按字符切分文本，保证不会切断 UTF-8 字符。
pub fn split_into_chunks(text: &str, chunk_chars: usize) -> Vec<String> {
    text.chars()
        .collect::<Vec<_>>()
        .chunks(chunk_chars.max(1))
        .map(|chunk| chunk.iter().collect::<String>())
        .collect()
}

/// 把原始文本清洗成「结构尽可能保留、噪音尽可能少」的纯文本。
pub fn clean_document(raw: &str) -> String {
    let normalized = normalize_chars(raw);
    let unified = normalized.replace("\r\n", "\n").replace('\r', "\n");

    let mut lines: Vec<String> = unified
        .lines()
        .map(|line| shorten_long_tokens(line.trim_end()))
        .collect();

    drop_page_furniture(&mut lines);
    drop_repeated_short_lines(&mut lines);
    let merged = merge_wrapped_lines(&lines);
    let collapsed = collapse_blank_lines(&merged);

    take_head(collapsed.trim(), MAX_CLEAN_CHARS)
}

/// 统一字符：全角字母数字 → 半角、表意空格 → 普通空格、去掉零宽与控制字符。
///
/// 只转换全角字母和数字：全角标点（，！？：；（））是中文排版的正常写法，
/// 一律转成半角会把中文文本改得不像中文。
fn normalize_chars(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\u{3000}' | '\t' => out.push(' '),
            '\u{FEFF}' | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' => {}
            '\n' => out.push('\n'),
            c if ('\u{FF10}'..='\u{FF19}').contains(&c)
                || ('\u{FF21}'..='\u{FF3A}').contains(&c)
                || ('\u{FF41}'..='\u{FF5A}').contains(&c) =>
            {
                out.push(char::from_u32(c as u32 - 0xFEE0).unwrap_or(c));
            }
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// 把一行里超长的连续串（base64、十六进制串等）折叠成占位符。
fn shorten_long_tokens(line: &str) -> String {
    let flush = |token: &mut String, out: &mut String| {
        if token.chars().count() > MAX_TOKEN_CHARS {
            out.push_str("[已省略超长内容]");
        } else {
            out.push_str(token);
        }
        token.clear();
    };

    let mut out = String::with_capacity(line.len());
    let mut token = String::new();
    for ch in line.chars() {
        if ch == ' ' {
            flush(&mut token, &mut out);
            out.push(' ');
        } else {
            token.push(ch);
        }
    }
    flush(&mut token, &mut out);
    out
}

/// 是否是 Markdown 表格行（`| a | b |` 或分隔行 `| --- | --- |`）。
///
/// 表格行必须逐行保留：一旦被段落合并逻辑拼成一行，列结构就彻底丢了。
pub(crate) fn is_table_row(line: &str) -> bool {
    let text = line.trim();
    if text.chars().count() < 2 {
        return false;
    }
    text.starts_with('|') || text.ends_with('|')
}

/// 是否是水平分隔线（`---` / `***` / `___`）。
pub(crate) fn is_horizontal_rule(line: &str) -> bool {
    let text = line.trim();
    if text.chars().count() < 3 {
        return false;
    }
    ['-', '*', '_'].iter().any(|mark| {
        text.chars().all(|c| c == *mark || c == ' ')
            && text.chars().filter(|c| c == mark).count() >= 3
    })
}

/// 是否是标题 / 列表项这类有结构含义的行（这类行不参与「重复行」判定）。
pub(crate) fn is_structural(line: &str) -> bool {
    let text = line.trim_start();
    if text.starts_with('#') || text.starts_with("- ") || text.starts_with("* " ) {
        return true;
    }
    if text.starts_with("· ") || text.starts_with("•") || text.starts_with("●") {
        return true;
    }
    // 表格行与分隔线都是结构，绝不能参与段落合并。
    if is_table_row(text) || is_horizontal_rule(text) {
        return true;
    }
    // 引用块（`> 说明`）单独成行。
    if text.starts_with('>') {
        return true;
    }
    // "1. xxx" / "1、xxx" / "一、xxx"
    let mut chars = text.chars();
    if let Some(first) = chars.next() {
        if first.is_ascii_digit() {
            let rest: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
            let after = text.chars().nth(rest.chars().count());
            if matches!(after, Some('.') | Some('、') | Some(')') | Some('）')) {
                return true;
            }
        }
        if "一二三四五六七八九十".contains(first) {
            let after = text.chars().nth(1);
            if matches!(after, Some('、') | Some('.')) {
                return true;
            }
        }
    }
    false
}

/// 句末标点。
fn ends_sentence(line: &str) -> bool {
    line.chars()
        .last()
        .map(|c| SENTENCE_END.contains(&c))
        .unwrap_or(false)
}

/// 是否像一个小标题：短、且完全不含标点。
///
/// PDF 里标题常常不带任何标记，如果直接参与段落合并会被吸进下一段正文，所以这里单独排除。
pub(crate) fn looks_like_heading(line: &str) -> bool {
    const MAX_HEADING_CHARS: usize = 24;
    let text = line.trim();
    if text.chars().count() > MAX_HEADING_CHARS {
        return false;
    }
    // 表格行（`| 维度 | 现有做法 | 本产品 |`）短且不含标点，天然会被误判成标题 ——
    // 那会把每一行表格都切成一个章节，必须显式排除。
    if is_table_row(text) || is_horizontal_rule(text) {
        return false;
    }
    if text.starts_with('>') {
        return false;
    }
    !text
        .chars()
        .any(|c| SENTENCE_END.contains(&c) || c == '，' || c == '、')
}

/// 是否是「新段落」的起头（章节词、结构性行）。
fn starts_new_block(line: &str) -> bool {
    if is_structural(line) {
        return true;
    }
    const HEADS: [&str; 10] = [
        "摘要", "目录", "前言", "背景", "目标", "范围", "附录", "参考", "需求", "说明",
    ];
    HEADS
        .iter()
        .any(|head| line.starts_with(head) && line.chars().count() <= head.chars().count() + 6)
}

/// 判断合并两行时中间是否需要补空格（拉丁文之间要，中文之间不要）。
fn needs_space(previous: &str, next: &str) -> bool {
    let left = previous.chars().last();
    let right = next.chars().next();
    match (left, right) {
        (Some(a), Some(b)) => a.is_ascii_alphanumeric() && b.is_ascii_alphanumeric(),
        _ => false,
    }
}

/// 是否是页码、页眉页脚、点线目录行。
fn is_page_furniture(line: &str, drop_bare_digits: bool) -> bool {
    let text = line.trim();
    if text.is_empty() {
        return false;
    }
    if drop_bare_digits && text.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if text.chars().count() > 40 {
        return false;
    }

    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    // 第 3 页 / 共 12 页 / 3/12
    if (compact.starts_with('第') && compact.contains('页'))
        || (compact.contains('/') && compact.chars().all(|c| c.is_ascii_digit() || c == '/'))
    {
        return true;
    }
    // Page 3 / page 3 of 12
    if compact.len() >= 4
        && compact.is_char_boundary(4)
        && compact[..4].eq_ignore_ascii_case("page")
    {
        return true;
    }
    // - 3 -
    if compact.starts_with('-') && compact.ends_with('-') && compact.chars().count() <= 8 {
        return true;
    }
    // 点线目录行
    if text.contains("......") || text.chars().filter(|c| *c == '…').count() >= 4 {
        return true;
    }
    false
}

/// 去掉页码、页眉页脚、点线目录行。
fn drop_page_furniture(lines: &mut Vec<String>) {
    let bare_digits = lines
        .iter()
        .filter(|line| {
            let text = line.trim();
            !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
        })
        .count();
    // 偶尔一个纯数字行可能是正文数字，成批出现才判定为页码。
    let drop_bare_digits = bare_digits >= REPEAT_MIN_COUNT;
    lines.retain(|line| !is_page_furniture(line, drop_bare_digits));
}

/// 去掉反复出现的短行（典型的页眉页脚），但不动有结构含义的行。
fn drop_repeated_short_lines(lines: &mut Vec<String>) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in lines.iter() {
        let text = line.trim();
        if text.is_empty() || text.chars().count() > REPEAT_MAX_LINE_CHARS || is_structural(text) {
            continue;
        }
        *counts.entry(text.to_string()).or_insert(0) += 1;
    }

    lines.retain(|line| {
        let text = line.trim();
        if text.is_empty() || text.chars().count() > REPEAT_MAX_LINE_CHARS || is_structural(text) {
            return true;
        }
        counts
            .get(text)
            .map(|count| *count < REPEAT_MIN_COUNT)
            .unwrap_or(true)
    });
}

/// 把 PDF 常见的硬换行拼回段落。
fn merge_wrapped_lines(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for line in lines {
        let text = line.trim();
        let append = match out.last() {
            Some(previous) => {
                let previous = previous.trim();
                !previous.is_empty()
                    && !text.is_empty()
                    && !is_structural(previous)
                    && !is_structural(text)
                    && !looks_like_heading(previous)
                    && !ends_sentence(previous)
                    && !starts_new_block(text)
            }
            None => false,
        };

        if append {
            if let Some(previous) = out.last_mut() {
                if needs_space(previous, text) {
                    previous.push(' ');
                }
                previous.push_str(text);
            }
        } else {
            out.push(line.clone());
        }
    }
    out
}

/// 连续空行折叠成一个。
fn collapse_blank_lines(lines: &[String]) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut blank = false;
    for line in lines {
        if line.trim().is_empty() {
            if blank {
                continue;
            }
            blank = true;
            out.push("");
        } else {
            blank = false;
            out.push(line);
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_page_numbers_and_running_headers() {
        let raw = "第 1 页\n某某产品需求说明\n第一章 概述\n这是正文的第一段内容，讲清楚产品要解决的问题。\n第 2 页\n某某产品需求说明\n第二章 功能\n- 支持记一笔账\n- 支持按月统计\n第 3 页\n某某产品需求说明\n第三章 收尾\n结束语在此。\n";
        let cleaned = clean_document(raw);

        assert!(!cleaned.contains("第 1 页"));
        assert!(!cleaned.contains("第 2 页"));
        assert!(!cleaned.contains("第 3 页"));
        // 反复出现的页眉被识别成页脚噪音；只出现一次的第三章正文保留下来。
        assert!(!cleaned.contains("某某产品需求说明"));
        assert!(cleaned.contains("第一章 概述"));
        assert!(cleaned.contains("- 支持记一笔账"));
        assert!(cleaned.contains("- 支持按月统计"));
        assert!(cleaned.contains("结束语在此。"));
    }

    #[test]
    fn joins_hard_wrapped_lines_but_keeps_headings_separate() {
        let raw = "视觉规范\n本产品的整体风格以深色为主，\n强调色使用青绿色，卡片统一圆角。\n";
        let cleaned = clean_document(raw);

        assert!(
            cleaned.contains("本产品的整体风格以深色为主，强调色使用青绿色，卡片统一圆角。"),
            "硬换行没有拼接：{cleaned:?}"
        );
        // 「视觉规范」是一个不含标点的短标题，不应被并入正文。
        assert!(
            cleaned.lines().any(|line| line.trim() == "视觉规范"),
            "短标题被误并入正文：{cleaned:?}"
        );
    }

    #[test]
    fn normalizes_full_width_and_invisible_characters() {
        let cleaned = clean_document("ＡＢＣ１２３\u{FEFF}\u{200B}测试\u{3000}完成");
        assert_eq!(cleaned.trim(), "ABC123测试 完成", "实际输出：{cleaned:?}");
        assert!(!cleaned.contains('\u{FEFF}'));
        assert!(!cleaned.contains('\u{200B}'));
    }

    #[test]
    fn keeps_chinese_punctuation() {
        let cleaned = clean_document("第一步：导入文档！然后（可选）配置参数；最后导出。");
        assert_eq!(
            cleaned, "第一步：导入文档！然后（可选）配置参数；最后导出。",
            "中文标点被改动了：{cleaned:?}"
        );
    }

    #[test]
    fn folds_very_long_tokens() {
        let blob = "A".repeat(500);
        let cleaned = clean_document(&format!("图片数据：{blob}"));
        assert!(cleaned.contains("[已省略超长内容]"));
        assert!(cleaned.chars().count() < 100);
    }

    #[test]
    fn collapses_blank_runs() {
        let cleaned = clean_document("第一段。\n\n\n\n第二段。");
        assert_eq!(cleaned, "第一段。\n\n第二段。");
    }

    #[test]
    fn take_head_and_tail_are_char_safe() {
        assert_eq!(take_head("中文测试", 2), "中文");
        assert_eq!(take_tail("中文测试", 2), "测试");
        assert_eq!(split_into_chunks("中文测试", 2), vec!["中文", "测试"]);
    }
}
