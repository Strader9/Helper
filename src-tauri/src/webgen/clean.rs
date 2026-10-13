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

/// 把一个锚点特征展开成它在源码里**可能出现的多种字面写法**。
///
/// 背景：预览里的点选脚本拿到的是元素并生成 CSS 选择器（如 `button#gen`、
/// `div.card:nth-of-type(2)`），而我们要匹配的是 HTML 源码文本（`<button id="gen">`）。
/// 选择器与源码不同构，所以要把选择器拆成属性形态再试。
///
/// 返回顺序 = 优先级：
///   1. 原串本身（`HTML：<button id="gen">生成</button>` 这种直接回传的片段必定命中）
///   2. 去掉伪类/伪元素后的选择器主体
///   3. 复合选择器的最后一段（`main > button` 取 `button`）
///   4. `#id` → `id="gen"` / `id='gen'`
///   5. `.class` → `class="card"` / 类名裸串
fn anchor_variants(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |out: &mut Vec<String>, value: String| {
        let value = value.trim().to_string();
        if !value.is_empty() && !out.contains(&value) {
            out.push(value);
        }
    };

    push(&mut out, raw.to_string());

    // 去掉伪类与属性选择器里的参数，只留「元素#id.class」这种骨架。
    let skeleton: String = raw
        .split(|ch| matches!(ch, ':' | '[' | ']' | '(' | ')' | ','))
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    push(&mut out, skeleton.clone());

    // 复合选择器（`main > button`、`body .card`）取最后一段。
    if let Some(last) = skeleton.split_whitespace().last() {
        push(&mut out, last.to_string());
    }

    // `#id` 与 `.class` → 源码里的属性写法 / 裸串。
    for token in skeleton.split(['#', '.']) {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        // `button#gen` 拆出来的是 `button` 与 `gen`；`button` 单独做锚点太弱，
        // 所以只在它带引号形态时才用。这里主要产出 id/class 的高置信写法。
        if !skeleton.contains('#') && !skeleton.contains('.') {
            break;
        }
        push(&mut out, format!("id=\"{token}\""));
        push(&mut out, format!("id='{token}'"));
        push(&mut out, format!("class=\"{token}\""));
    }

    out
}

/// 按锚点摘一段上下文：找到 `anchor` 之后，向前后各取若干字符。
///
/// 用途：整页源码塞不进窗口时，定向修改至少要让模型**看见它要改的那一处**。
/// 早先的实现只回传开头 `count` 个字符，落在后面的目标元素模型根本看不到 ——
/// 于是"只改这一处"变成盲改，实测出现过模型整份照抄、什么都没改的情况。
///
/// `anchor` 取不到时退回开头片段：**宁可给开头，也不能什么都不给** ——
/// 有原文在，模型至少知道页面的视觉风格与结构；没有原文，它会凭需求重写一份，
/// 那等于把用户的页面推倒重来。
///
/// **返回的文本是纯粹的片段，不含任何「省略」标记。**
/// 早先在片段两端加「（前面省略）」「（后面省略）」，本意是让人知道这不是完整文件，
/// 但这个字符串会被直接拼进 `<current_html>` 代码块 —— 模型把它当成代码的一部分，
/// 实测原样抄进了产物（生成的 HTML 末尾真留着一句「（后面省略）」，文件就此截断）。
/// 省略提示属于**给模型看的自然语言**，由调用方写在代码块外面，不能混进代码。
pub fn take_excerpt(text: &str, anchor: &str, count: usize) -> (String, bool) {
    take_excerpt_with_span(text, anchor, count).map_or_else(
        || (take_head(text, count), false),
        |span| (span.text, true),
    )
}

/// 摘取片段并回报它**在原文中的字符区间**（`start` 含、`end` 不含）。
///
/// 区间是给「片段内改 + 程序合并」用的：模型只输出被改的那一小块，
/// 程序要把它贴回原文的对应位置，就必须知道这一段原来占哪儿。
pub fn take_excerpt_with_span(text: &str, anchor: &str, count: usize) -> Option<ExcerptSpan> {
    let anchor = anchor.trim();
    if anchor.is_empty() {
        return None;
    }
    // 锚点通常是"元素：button / 选择器：main > button:nth-of-type(1)"这样的多行描述，
    // 整串在源码里必然找不到。逐行拆成候选特征，再逐个去源码里找。
    //
    // 关键坑：点选脚本回传的是 **CSS 选择器**（`button#gen`），而源码是 **HTML 文本**
    // （`<button id="gen">`）。两者不同构，直接 `contains` 必然落空。所以每个候选
    // 还要展开成它在源码里可能的写法（`#gen` → `id="gen"`、`.card` → `class="card"`）。
    //
    // **为什么不做成"按行顺序取第一个命中"**：那是本次踩的真坑。点选描述里
    // 「当前文字：生成名字」这种短文本在页面里往往**先出现在别处**（标题、说明、注释），
    // 按行顺序会挑中那个错位置，于是片段根本没包住用户点选的元素 ——
    // 实测片段没覆盖真实目标（目标在 11359 字符，片段却在别处），合并时把不属于目标的
    // 一大段内容顶替掉了，18918 字的页面掉了 5906 字。
    //
    // 现在按**候选强度**排序：`HTML：` 那行的 outerHTML 是原文照抄，最硬；
    // 其次是长选择器路径（`main > section > button`，越长越准）；
    // 最短的"当前文字"排最后 —— 它最容易撞上别处的同名文本。
    let mut candidates = anchor
        .lines()
        .map(|line| {
            // 去掉「元素：」「选择器：」「当前文字：」这类标签，只留特征本身。
            let raw = line.split_once('：').map(|(_, value)| value).unwrap_or(line).trim();
            // outerHTML 行最硬：它逐字来自源码。
            let hardness = if line.trim_start().starts_with("HTML") { 3 } else { 0 };
            (hardness, raw.to_string())
        })
        .flat_map(|(hardness, raw)| {
            anchor_variants(&raw)
                .into_iter()
                .enumerate()
                .map(move |(rank, value)| (hardness, rank, value))
        })
        .filter(|(_, _, value)| value.chars().count() >= 2)
        .collect::<Vec<_>>();
    // 先比硬度（HTML 行优先），再比展开顺序（越靠前的写法越准），最后比长度（越长越具体）。
    candidates.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(b.2.chars().count().cmp(&a.2.chars().count()))
    });
    let needle = candidates
        .into_iter()
        .map(|(_, _, value)| value)
        .find(|value| text.contains(value.as_str()))?;

    let byte_at = text.find(needle.as_str())?;
    let needle_chars = needle.chars().count();

    // 以命中位置为中心，往前留 1/3、往后留 2/3 —— 要改的元素后面通常还有它的兄弟节点
    // 与相关脚本，往后多留一些更有助于模型理解"这一处在整体里的位置"。
    let char_at = text[..byte_at].chars().count();
    let chars: Vec<char> = text.chars().collect();
    let lead = count / 3;
    let start = char_at.saturating_sub(lead).min(chars.len());
    let end = (start + count).min(chars.len());

    // 锚点自身必须**完整**落在片段里。窗口比锚点还短时（`count` 被算错，或锚点本身就是
    // 长属性），宁可把窗口撑到装得下它，也不要切出一个"看不见目标"的片段 ——
    // 那样的片段对定向修改毫无价值。注意锚点是「元素#id.class」这类特征，命中处在元素
    // 内部（`id="…"`），所以撑宽要以**锚点区间**为中心，两边一起让。
    let anchor_end = (char_at + needle_chars).min(chars.len());
    let deficit = anchor_end.saturating_sub(end);
    let start = start.saturating_sub(deficit).min(char_at);
    let end = end.max(anchor_end);

    let text_span: String = chars[start..end].iter().collect();

    Some(ExcerptSpan {
        text: text_span,
        start,
        end,
    })
}

/// 一次片段摘取的结果：片段本体 + 它在原文里的字符区间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcerptSpan {
    pub text: String,
    /// 片段首字符在原文中的下标（含）。
    pub start: usize,
    /// 片段末字符在原文中的下标（不含）。
    pub end: usize,
}

/// 合并一次「片段内改」的结果：把模型改后的片段贴回原文。
///
/// 为什么需要它：整页超过窗口预算时，让模型"看着片段输出完整 HTML"是**不可能完成的任务** ——
/// 它只看到 1/6 的源码，却要交出 6/6 的成品，只能凭空造出剩下 5/6。
/// 实测就是这么坏的：18918 字的页面回传 3006 字片段，模型把整页替换成那 3006 字，
/// `<body>` 与 `<script>` 全部丢失，产物在 CSS 中途被自己写的「（后面省略）」截断。
///
/// 所以定向修改改成两次分工：**模型只负责改它看得见的那一块，程序负责把这一块放回原位**。
///
/// 贴回的位置不是"整个片段"，而是**锚点所在的那个元素**：片段是上下文窗口（可能 6000 字），
/// 模型返回的是它改的那个元素（可能 50 字）。用整个片段做替换位会把上下文一起换掉，
/// 所以这里先从片段里抠出目标元素的字符区间，再把改后内容贴到那个区间上。
///
/// 抠不出元素区间时退回"整片段替换"：锚点常常直接命中元素中间（`id="gen"`），
/// 这时找不到元素边界，但整片段替换仍然比整页重写安全得多 —— 至少丢失的内容
/// 被限制在这一个窗口内。
///
/// 返回 `None` 表示"这次改动没法安全合并"，调用方必须**保留原文**而不是接受产物：
/// 宁可告诉用户"这次没改成"，也不能把一份残缺的页面交出去。
pub fn merge_patch(original: &str, anchor: &str, count: usize, patched: &str) -> Option<MergeOutcome> {
    let span = take_excerpt_with_span(original, anchor, count)?;
    let patch = strip_code_fence(patched);

    // 模型经常会把片段原样抄回来（表示"无需改动"）或者连省略提示一起抄。
    let patch = patch.trim();
    if patch.is_empty() {
        return None;
    }
    let patch = drop_ellipsis_marks(patch);
    if patch.is_empty() {
        return None;
    }

    // 校验 1：patch 自身必须是**结构自洽**的一小块。
    //
    // 这里**不能**拿"片段里有 `<script>`、patch 里没有"去判 —— 那是个设计错误：
    // 片段是**上下文窗口**，里面天然带 `<html>`/`<body>`/`<script>`；而模型在片段模式下
    // 本来就只该返回它改的那个元素（实测返回的正是 `<button …>再摇一次</button>`，94 字符）。
    // 早先按"整片段的标签必须在 patch 里保留"来判，把每一次正确的局部修改都拒了。
    //
    // 真正该拦的是 patch **自己**缺胳膊少腿：开了 `<script>` 没关、标签半截断在那里。
    for (open, close) in [
        ("<html", "</html>"),
        ("<body", "</body>"),
        ("<script", "</script>"),
        ("<style", "</style>"),
        ("<button", "</button>"),
        ("<div", "</div>"),
        ("<span", "</span>"),
    ] {
        let lower = patch.to_lowercase();
        if lower.contains(open) && !lower.contains(close) {
            return None;
        }
    }

    // 校验 2：改后片段不能**比它看到的上下文还长**。
    //
    // 刻意不设"下限"：片段是上下文窗口（可能 6000 字），模型该返回的是它改的那个
    // 元素（可能只有 50 字）—— 两者本来就不该同量级。早先拿片段长度当基准算倍率，
    // 结果把"精准只改一处"判成了异常，反而放过了"把上下文原样抄回来"。
    let window = span.text.chars().count().max(1);
    if patch.chars().count() > window {
        return None;
    }

    // 校验 3：改后的内容要还认得出来是"同一个元素"。
    // 抠得出元素区间时，比该元素的 id/class；抠不出时退而比整个片段的身份特征。
    let element = locate_element(span.text.as_str(), anchor);
    let reference = element
        .as_ref()
        .map_or(span.text.as_str(), |(start, end)| &span.text[*start..*end]);
    if !keeps_anchor_signal(reference, patch) {
        return None;
    }

    // 定位贴回位置：优先"元素区间"，退而"整个片段"。
    let (from, to) = match element {
        Some((start, end)) => (span.start + start, span.start + end),
        None => (span.start, span.end),
    };

    let chars: Vec<char> = original.chars().collect();
    let from = from.min(chars.len());
    let to = to.min(chars.len()).max(from);
    let mut merged = String::with_capacity(original.len() + patch.len());
    merged.extend(chars[..from].iter());
    merged.push_str(patch);
    merged.extend(chars[to..].iter());

    let replaced: String = chars[from..to].iter().collect();
    let changed = patch.trim() != replaced.trim();
    Some(MergeOutcome {
        html: merged,
        changed,
    })
}

/// 从片段里抠出"锚点命中处所属的那个元素"的字符区间（相对片段，`end` 不含）。
///
/// 只处理**配对标签**（`<tag ...>…</tag>`）这种最常见也最好判的情况：
/// 从锚点命中位置往前找到最近的 `<tag`，再从那里往后配平地数到对应的 `</tag>`。
/// 找不到配对的闭合标签（自闭合标签、锚点落在 CSS/JS 里）就返回 `None`，
/// 由调用方退回整片段替换。
fn locate_element(excerpt: &str, anchor: &str) -> Option<(usize, usize)> {
    // 锚点在片段里最终命中的那个特征（与 take_excerpt 里的挑选逻辑一致）。
    let needle = anchor
        .lines()
        .flat_map(|line| {
            let raw = line.split_once('：').map(|(_, value)| value).unwrap_or(line).trim();
            anchor_variants(raw)
        })
        .filter(|value| value.chars().count() >= 2)
        .find(|value| excerpt.contains(value.as_str()))?;

    let byte_at = excerpt.find(needle.as_str())?;
    let hit = excerpt[..byte_at].chars().count();

    // 往前找最近的起始标签。
    let before: Vec<char> = excerpt.chars().collect();
    let open_at = (0..=hit.min(before.len().saturating_sub(1)))
        .rev()
        .find(|&index| before.get(index) == Some(&'<'))?;
    let name: String = before[open_at + 1..]
        .iter()
        .take_while(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if name.is_empty() {
        return None;
    }

    // 从起始标签的 `>` 之后，按嵌套深度配平找闭合标签。
    let open_close = before[open_at..].iter().position(|&ch| ch == '>')? + open_at;
    // 自闭合标签没有闭合对，直接当作它自己。
    if before.get(open_close.saturating_sub(1)) == Some(&'/') {
        return Some((open_at, open_close + 1));
    }

    let lowercase: Vec<char> = excerpt.to_lowercase().chars().collect();
    let name_lower: Vec<char> = name.to_lowercase().chars().collect();
    let mut depth = 1usize;
    let mut cursor = open_close + 1;
    while cursor < lowercase.len() {
        if lowercase[cursor] != '<' {
            cursor += 1;
            continue;
        }
        if lowercase[cursor..].starts_with(&name_lower)
            && matches!(lowercase.get(cursor + name_lower.len()), Some(' ') | Some('>') | Some('\n') | Some('\t') | Some('/'))
        {
            depth += 1;
        } else if lowercase[cursor..].starts_with(&['<', '/'])
            && lowercase[cursor + 2..].starts_with(&name_lower)
        {
            depth -= 1;
            if depth == 0 {
                let close_end = lowercase[cursor..].iter().position(|&ch| ch == '>')? + cursor + 1;
                return Some((open_at, close_end));
            }
        }
        cursor += 1;
    }
    None
}

/// 一次片段合并的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    pub html: String,
    /// 改后片段与原文片段是否真的不同。为 `false` 时说明模型没做任何改动。
    pub changed: bool,
}

/// 去掉模型可能会顺手抄回来的代码围栏。
fn strip_code_fence(text: &str) -> String {
    let trimmed = text.trim();
    let trimmed = trimmed
        .strip_prefix("```html")
        .or_else(|| trimmed.strip_prefix("```HTML"))
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    trimmed.strip_suffix("```").unwrap_or(trimmed).to_string()
}

/// 去掉片段两端模型抄回来的「省略」提示。
///
/// 这些提示是**给模型看的自然语言**，本该写在代码块外面；模型却常常把它当代码抄回来，
/// 一旦合并进产物，产物就会被这句中文截断。
fn drop_ellipsis_marks(text: &str) -> &str {
    let mut value = text.trim();
    loop {
        let before = value;
        for marker in ["（前面省略）", "（后面省略）", "(前面省略)", "(后面省略)"] {
            value = value.trim();
            value = value.strip_prefix(marker).unwrap_or(value).trim();
            value = value.strip_suffix(marker).unwrap_or(value).trim();
        }
        if value == before {
            return value;
        }
    }
}

/// 判断改后片段是否还留着原片段的"身份特征"（id / class 名或标签名）。
///
/// 只用来拦住"模型改的压根不是这一处"这种错位，不做严格比对 ——
/// 用户让它把 `class="cta"` 换成别的样式是合理需求。
fn keeps_anchor_signal(before: &str, after: &str) -> bool {
    let tokens: Vec<String> = ["id=\"", "class=\"", "id='", "class='"]
        .iter()
        .flat_map(|prefix| {
            before
                .match_indices(prefix)
                .filter_map(move |(at, _)| {
                    let rest = &before[at + prefix.len()..];
                    let end = rest.find(['"', '\''])?;
                    Some(rest[..end].to_string())
                })
        })
        .filter(|value| !value.trim().is_empty())
        .collect();

    if tokens.is_empty() {
        return true;
    }
    // 只要还剩至少一个身份特征就算对得上 —— 元素被保留下来了。
    tokens.iter().any(|token| after.contains(token.as_str()))
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

    /// 摘片段必须把目标元素包进来 —— 这是"只改这一处"能成立的前提。
    #[test]
    fn excerpt_centers_on_the_anchor() {
        let head = "A".repeat(2000);
        let target = "<button id=\"gen\">生成</button>";
        let tail = "B".repeat(2000);
        let page = format!("{head}{target}{tail}");

        // 锚点用点选脚本给出的那套描述格式：CSS 选择器在源码里没有对应字面写法，
        // 靠 `HTML：` 那一行的 outerHTML 精确定位。
        let anchor = "元素：button\n选择器：main > button#gen\n当前文字：生成\nHTML：<button id=\"gen\">生成</button>";
        let (excerpt, located) = take_excerpt(&page, anchor, 600);
        assert!(located, "应当能定位到锚点");
        assert!(
            excerpt.contains(target),
            "片段的中心必须是目标元素本身：{excerpt}"
        );
        // 片段必须是**纯代码**：省略提示曾经混在这里，被模型原样抄进了产物。
        assert!(!excerpt.contains("省略"), "片段里不能出现给模型看的省略提示：{excerpt}");
        assert_eq!(excerpt.chars().count(), 600, "按要求的长度摘取");
    }

    /// 片段必须回报它在原文中的位置 —— 「片段内改 + 程序合并」要靠它贴回去。
    #[test]
    fn excerpt_reports_where_it_came_from() {
        let head = "A".repeat(2000);
        let target = "<button id=\"gen\">生成</button>";
        let page = format!("{head}{target}{}", "B".repeat(2000));

        let span = take_excerpt_with_span(&page, "选择器：main > button#gen", 600)
            .expect("应当能定位到锚点");
        let chars: Vec<char> = page.chars().collect();
        // 区间必须精确切回原片段，否则合并会把改动贴错位置。
        assert_eq!(chars[span.start..span.end].iter().collect::<String>(), span.text);
        assert!(span.text.contains(target), "区间要覆盖目标元素");
    }

    /// 页面前半段的目标元素也要能摘出来（不能只往后看）。
    #[test]
    fn excerpt_handles_an_anchor_near_the_start() {
        let target = "<button id=\"early\">早</button>";
        let page = format!("{target}{}", "B".repeat(3000));
        let (excerpt, located) = take_excerpt(&page, "选择器：body > button#early", 500);
        assert!(located);
        assert!(excerpt.contains(target), "{excerpt}");
        assert!(excerpt.starts_with(target), "靠开头就该从 0 开始，不留前置内容");
    }

    /// 窗口比锚点还短时，片段要撑到能装下**锚点本身** —— 看不见目标的片段毫无价值。
    ///
    /// 注意守的是"锚点可见"而不是"整个元素可见"：锚点（`id="very-long-id"`）落在元素
    /// 内部，`take_excerpt` 只承诺让模型看得见它要改的那个特征，不负责把标签补全。
    #[test]
    fn excerpt_widens_the_window_to_fit_a_long_anchor() {
        let head = "A".repeat(500);
        let target = "<button id=\"very-long-id\">很长的一段按钮文字</button>";
        let page = format!("{head}{target}{}", "B".repeat(500));

        let needle = "id=\"very-long-id\"";
        let (excerpt, located) = take_excerpt(&page, "选择器：button#very-long-id", 10);
        assert!(located);
        assert!(
            excerpt.contains(needle),
            "窗口再小也不能把锚点切掉：{excerpt}"
        );
        assert!(excerpt.chars().count() >= needle.chars().count());

        // 区间同样要以锚点为中心。
        let span = take_excerpt_with_span(&page, "选择器：button#very-long-id", 10).unwrap();
        assert!(span.text.contains(needle));
    }

    /// 只有 CSS 选择器、没有 `HTML：` 行时，选择器要能展开成 `id="…"` 再命中源码。
    #[test]
    fn excerpt_expands_a_selector_into_its_attribute_form() {
        let head = "A".repeat(1000);
        let target = "<button id=\"gen\" class=\"cta\">生成</button>";
        let page = format!("{head}{target}{}", "B".repeat(1000));

        let (excerpt, located) = take_excerpt(&page, "选择器：main > button#gen", 400);
        assert!(located, "选择器 button#gen 应当能展开成 id=\"gen\" 命中：{excerpt}");
        assert!(excerpt.contains(target), "{excerpt}");
    }

    /// 定位要挑**最硬的锚点**，而不是"按行顺序第一个能命中的"。
    ///
    /// 这是端到端实测踩出来的坑：点选描述里「当前文字：生成名字」这种短文本在页面上
    /// 往往**先出现在别处**（标题、说明），按行顺序会挑中错位置，片段根本没包住
    /// 用户点选的元素 —— 实测目标在 11359 字符、片段却在别处，合并时顶掉了 5906 字。
    /// 现在 `HTML：` 那行（逐字来自源码）优先于「当前文字」。
    #[test]
    fn excerpt_prefers_the_outer_html_line_over_short_text() {
        // "生成名字" 先在标题里出现一次，真正的按钮在后面。
        let page = format!(
            "<h1>生成名字</h1>{}<button onclick=\"generateNames()\">生成名字</button>",
            "填".repeat(800)
        );
        let anchor = "元素：button\n选择器：main > button:nth-of-type(1)\n当前文字：生成名字\nHTML：<button onclick=\"generateNames()\">生成名字</button>";

        let span = take_excerpt_with_span(&page, anchor, 400).expect("应当能定位");
        let target = "<button onclick=\"generateNames()\">生成名字</button>";
        let target_at = page.find(target).map(|b| page[..b].chars().count()).unwrap();
        assert!(
            span.start <= target_at && target_at < span.end,
            "片段必须包住真实的按钮（目标在 {target_at}，片段是 {}..{}）",
            span.start,
            span.end
        );
    }

    /// 锚点找不到时退回开头片段，而不是返回空串。
    ///
    /// 为什么要退回而不是放弃：有原文在，模型至少知道页面的视觉风格与结构；
    /// 什么都不给，它会凭需求重写一份，等于把用户的页面推倒重来。
    #[test]
    fn excerpt_falls_back_to_the_head_when_the_anchor_is_missing() {
        let page = format!("{}END", "X".repeat(1000));
        let (excerpt, located) = take_excerpt(&page, "元素：button#不存在", 200);
        assert!(!located, "找不到就该如实报告");
        assert_eq!(excerpt, "X".repeat(200), "必须退回开头片段，不能给空");
        assert!(!excerpt.contains("省略"));
    }

    /// 空锚点（用户没点选，但走了这条分支）同样退回开头。
    #[test]
    fn excerpt_with_an_empty_anchor_is_just_the_head() {
        let page = "Y".repeat(500);
        let (excerpt, located) = take_excerpt(&page, "   ", 100);
        assert!(!located);
        assert_eq!(excerpt, "Y".repeat(100));
    }

    /// 锚点描述里的标签前缀（「元素：」）要去掉，用特征本身去匹配。
    #[test]
    fn excerpt_strips_the_label_prefix_before_matching() {
        let page = format!("{}<div class=\"card\">内容</div>{}", "a".repeat(300), "b".repeat(300));
        // 只有"当前文字"那一行的特征能命中。
        let anchor = "元素：span#nope\n选择器：div#nope\n当前文字：内容";
        let (excerpt, located) = take_excerpt(&page, anchor, 400);
        assert!(located, "应当用「当前文字」那一行命中：{excerpt}");
        assert!(excerpt.contains("内容"));
    }

    // ---------- 片段合并 ----------

    /// 合并要把改动贴到正确位置，且**只动那一处**。
    #[test]
    fn merge_patch_splices_the_edited_fragment_back_in() {
        let head = "A".repeat(1000);
        let target = "<button id=\"gen\">生成</button>";
        let tail = "B".repeat(1000);
        let page = format!("{head}{target}{tail}");

        let patched = "<button id=\"gen\" style=\"background:#16a34a\">再摇一次</button>";
        let outcome = merge_patch(&page, "选择器：main > button#gen", 600, patched)
            .expect("片段可定位就该能合并");

        assert!(outcome.changed);
        assert!(outcome.html.contains(patched));
        assert!(!outcome.html.contains(target), "旧片段必须被替换掉");
        // 前后文一字不动。
        assert!(outcome.html.starts_with(&head));
        assert!(outcome.html.ends_with(&tail));
        assert_eq!(outcome.html.chars().count(), 1000 + patched.chars().count() + 1000);
    }

    /// 模型原样抄回片段 = 没改动，要如实报告，不能谎称改过。
    #[test]
    fn merge_patch_reports_when_nothing_actually_changed() {
        let page = format!("{}<button id=\"gen\">生成</button>{}", "A".repeat(500), "B".repeat(500));
        let outcome = merge_patch(
            &page,
            "选择器：button#gen",
            500,
            "<button id=\"gen\">生成</button>",
        )
        .expect("抄回来的片段也能合并");
        assert!(!outcome.changed, "一模一样就该说没变化");
        assert_eq!(outcome.html, page);
    }

    /// 模型把「省略」提示抄进代码时，合并前要替它摘掉 —— 否则产物会被这句中文截断。
    #[test]
    fn merge_patch_strips_ellipsis_marks_the_model_copied_in() {
        let page = format!("{}<button id=\"gen\">生成</button>{}", "A".repeat(500), "B".repeat(500));
        let outcome = merge_patch(
            &page,
            "选择器：button#gen",
            500,
            "（后面省略）<button id=\"gen\">再摇一次</button>（后面省略）",
        )
        .expect("应当能合并");
        assert!(outcome.html.contains("再摇一次"));
        assert!(!outcome.html.contains("省略"), "提示不能进产物：{}", outcome.html);
    }

    /// patch 自身缺胳膊少腿（开了标签没关）→ 拒绝合并。
    ///
    /// 注意**不能**用"片段里有 `<script>`、patch 里没有"来判：片段是上下文窗口，
    /// 天然带 `<html>`/`<body>`/`<script>`；模型在片段模式下只该返回它改的那个元素。
    /// 早先按这个判，把每一次正确的局部修改都拒了。
    #[test]
    fn merge_patch_refuses_a_fragment_that_is_itself_unbalanced() {
        let head = "A".repeat(400);
        let target = "<button id=\"gen\">生成</button>";
        let tail = "B".repeat(400);
        let page = format!("{head}{target}{tail}");

        // 只返回了半个 button —— 不合法。
        assert!(
            merge_patch(&page, "选择器：button#gen", 500, "<button id=\"gen\">再摇一次").is_none(),
            "半截标签必须拒绝"
        );
        // 完整的一小块 —— 放行。
        assert!(
            merge_patch(&page, "选择器：button#gen", 500, "<button id=\"gen\">再摇一次</button>")
                .is_some()
        );
    }

    /// 片段里带 `<html>/<body>/<script>` 上下文时，模型只返回一个元素也必须能合并。
    ///
    /// 这是端到端实测暴露的真坑：18918 字的页面，6000 字片段恰好覆盖了整份文档，
    /// 模型精准返回了一个 `<button>`（94 字符），却被"结构标签消失"的校验拦下，
    /// 于是每次定向修改都被判失败、原页原样保留 —— 功能等于没用。
    #[test]
    fn merge_patch_accepts_a_single_element_patch_over_a_document_wide_excerpt() {
        let page = format!(
            "<html><body><div>{}</div><script>function go(){{}}</script></body></html>",
            "A".repeat(600)
        );
        let patch = "<button id=\"gen\" style=\"background-color:#16a34a; color:white;\">再摇一次</button>";
        // 造一个锚点指向片段里的某个位置。
        let outcome = merge_patch(&page, "选择器：script", 2000, patch)
            .expect("单个元素 patch 必须能合并");
        assert!(outcome.html.contains("再摇一次"));
        assert!(outcome.html.starts_with("<html><body><div>"));
    }

    /// 改后片段体量暴涨数倍 → 说明模型偷着重写了别的东西，拒绝合并。
    #[test]
    fn merge_patch_refuses_a_fragment_that_balloons() {
        let page = format!("{}<button id=\"gen\">生成</button>{}", "A".repeat(500), "B".repeat(500));
        let huge = format!("<button id=\"gen\">{}</button>", "改".repeat(4000));
        assert!(merge_patch(&page, "选择器：button#gen", 500, &huge).is_none());
    }

    /// 改后片段把目标元素的 id/class 全换掉 → 可能改错了地方，拒绝合并。
    #[test]
    fn merge_patch_refuses_a_fragment_that_lost_the_target_identity() {
        let page = format!(
            "{}<button id=\"gen\" class=\"cta\">生成</button>{}",
            "A".repeat(400),
            "B".repeat(400)
        );
        // 锚点只命中 "gen"，改后连 gen 都没有了。
        assert!(
            merge_patch(&page, "选择器：button#gen", 200, "<button>生成</button>").is_none(),
            "目标身份全丢必须拒绝"
        );
    }

    /// 代码围栏要剥掉，不能把 ``` 也贴进产物。
    #[test]
    fn merge_patch_strips_a_code_fence() {
        let page = format!("{}<button id=\"gen\">生成</button>{}", "A".repeat(500), "B".repeat(500));
        let outcome = merge_patch(
            &page,
            "选择器：button#gen",
            500,
            "```html\n<button id=\"gen\">再摇一次</button>\n```",
        )
        .expect("应当能合并");
        assert!(!outcome.html.contains("```"), "围栏不能进产物");
        assert!(outcome.html.contains("再摇一次"));
    }

    /// 锚点定位不到 → 没有可贴回的位置，必须拒绝而不是瞎猜。
    #[test]
    fn merge_patch_refuses_when_the_anchor_cannot_be_found() {
        let page = format!("{}<button id=\"gen\">生成</button>", "A".repeat(500));
        assert!(merge_patch(&page, "选择器：button#不存在", 500, "<button>改了</button>").is_none());
    }
}
