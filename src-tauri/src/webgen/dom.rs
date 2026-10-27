//! 生成页面的 DOM 级小工具：把 `<script>` 块单独拎出来，以及把改好的脚本塞回去。
//!
//! 为什么需要"只改脚本"这条路径：8B 模型漏掉核心逻辑时（比如把"生成名字"写成随机字母
//! 拼串），要修的是 `<script>` 里的几十行，而不是整页。若让模型重写整份源码，一是慢
//! （一万多字符，8GB 显存下得动用到溢出到 CPU 的大窗口），二是它顺手就会把已经正确的
//! 配色和布局改坏。所以定向修复只重写脚本块，再由程序确定性地替换回去 —— 视觉部分
//! 一个字节都不会动。

/// 取出每个 `<script>` 的正文（不含标签）。
pub fn script_bodies(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (body_start, body_end) in script_spans(html) {
        out.push(html[body_start..body_end].to_string());
    }
    out
}

/// 所有 `<script>` 的正文拼起来，供静态检查使用。
pub fn merged_script(html: &str) -> String {
    script_bodies(html).join("\n")
}

/// 整块脚本区域（含首尾标签），第一个 `<script` 到最后一个 `</script>`。
///
/// 生成出来的页面通常只有一个脚本块；即便有多个，合并成一块回传也不会丢信息，
/// 因为模型要做的本来就是给出一个完整的、自洽的脚本。
pub fn script_block(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<script")?;
    let end = lower[start..].rfind("</script")? + start + "</script".len();
    let end = lower[end..]
        .find('>')
        .map(|offset| end + offset + 1)
        .unwrap_or(end);
    Some(html[start..end].to_string())
}

/// 用新脚本块替换掉页面里的脚本。
///
/// - 页面已有脚本：整段替换（第一个 `<script` 到最后一个 `</script>`）；
/// - 页面没有脚本：插到 `</body>` 之前，没有 `</body>` 就追加到末尾。
pub fn splice_script(html: &str, block: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(start) = lower.find("<script") else {
        return insert_before_body_close(html, block);
    };
    let Some(relative_end) = lower[start..].rfind("</script") else {
        return insert_before_body_close(html, block);
    };
    let mut end = start + relative_end + "</script".len();
    if let Some(offset) = lower[end..].find('>') {
        end += offset + 1;
    }
    format!("{}{}{}", &html[..start], block, &html[end..])
}

/// 用新的**脚本正文**替换页面里的脚本（自动补回 `<script>` 标签）。
///
/// 存在的理由：`splice_script` 要的是完整的 `<script>…</script>` 块，而各处机械修复
/// （`fix_null_storage_reads` / `fix_placeholder_alerts` / `fix_regex_properties` / …）
/// 算出来的是脚本正文。把正文直接交给 `splice_script` 会把开闭标签一起吃掉 ——
/// 实测产出过一个 17918 字符、按钮 8 个、**`<script>` 标签 0 个**的页面：脚本内容还在，
/// 却被当成 HTML 正文渲染，自检报 `NO_SCRIPT`，所有按钮点了都不动。
pub fn splice_script_body(html: &str, body: &str) -> String {
    splice_script(html, &format!("<script>\n{}\n</script>", body.trim_end()))
}

/// 页面上的交互元素清单：所有 `id` 与按钮文案。
///
/// 只重写脚本时，模型看不见 HTML，光凭需求猜不出要绑哪些 `getElementById`。
/// 把这份清单给它，绑定的选择器才对得上现有的 DOM。
pub fn element_outline(html: &str) -> String {
    const LIMIT: usize = 1_400;
    let mut ids: Vec<String> = Vec::new();
    let mut buttons: Vec<String> = Vec::new();

    for attr in ["id=\"", "id='"] {
        let quote = attr.chars().next_back().unwrap_or('"');
        let mut from = 0usize;
        while let Some(relative) = html[from..].find(attr) {
            let start = from + relative + attr.len();
            let Some(end_offset) = html[start..].find(quote) else {
                break;
            };
            let value = &html[start..start + end_offset];
            let value = value.trim();
            if !value.is_empty() && value.chars().count() <= 40 && !ids.iter().any(|id| id == value)
            {
                ids.push(value.to_string());
            }
            from = start + end_offset;
            if from >= html.len() {
                break;
            }
        }
    }

    let lower = html.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(relative) = lower[from..].find("<button") {
        let open = from + relative;
        let Some(gt) = lower[open..].find('>') else {
            break;
        };
        let text_start = open + gt + 1;
        let Some(close) = lower[text_start..].find("</button") else {
            break;
        };
        let label = strip_tags(&html[text_start..text_start + close.min(80)]);
        let label = label.trim();
        if !label.is_empty() && !buttons.iter().any(|item| item == label) {
            buttons.push(label.to_string());
        }
        from = text_start + close;
        if from >= lower.len() {
            break;
        }
    }

    let mut out = String::new();
    if !buttons.is_empty() {
        out.push_str("按钮文案（按出现顺序）：");
        out.push_str(&buttons.join(" / "));
        out.push('\n');
    }
    if !ids.is_empty() {
        out.push_str("可用的元素 id：");
        out.push_str(&ids.join(" / "));
        out.push('\n');
    }
    out.retain(|_| true);
    if out.chars().count() > LIMIT {
        out = out.chars().take(LIMIT).collect();
    }
    out
}

/// 从模型输出里取出脚本块。
///
/// 定向修复要求模型"只输出 `<script>` 块"，但它时常会包上代码围栏、或者干脆只给裸 JS。
/// 这里统一收拾：去围栏；有 `<script>` 标签就截取标签之间；确实只给了裸 JS 就补上标签。
/// 判断不出来（比如它又输出了整篇 HTML）时返回 `None`，调用方保留原版不出错。
pub fn tidy_script_block(output: &str) -> Option<String> {
    let trimmed = output.trim();
    let without_fence = ["```html", "```HTML", "```javascript", "```js", "```"]
        .iter()
        .find_map(|fence| trimmed.strip_prefix(*fence))
        .map(|rest| rest.trim_start())
        .unwrap_or(trimmed);
    let without_fence = without_fence
        .strip_suffix("```")
        .unwrap_or(without_fence)
        .trim();

    let lower = without_fence.to_ascii_lowercase();
    if let (Some(start), Some(close)) = (lower.find("<script"), lower.rfind("</script")) {
        let mut end = close + "</script".len();
        if let Some(offset) = lower[end..].find('>') {
            end += offset + 1;
        }
        if end > start {
            return Some(without_fence[start..end].to_string());
        }
    }

    // 裸 JS：不能是整篇 HTML，且要有一定体量，否则大概率是模型没按要求输出。
    let looks_like_page =
        lower.contains("<html") || lower.contains("<body") || lower.contains("<style");
    if !looks_like_page && without_fence.chars().count() >= 200 {
        Some(format!("<script>\n{without_fence}\n</script>"))
    } else {
        None
    }
}

/// 把一段"只有函数定义"的源码**追加**到页面脚本块的末尾。
///
/// 用于"只补缺失函数"这条修复路线：函数声明会提升，所以补上的定义放在脚本末尾即时生效，
/// 不用碰原有代码一个字符。这比让 8B 模型重写整份脚本现实得多。
pub fn append_to_script(html: &str, extra: &str) -> String {
    let extra = extra.trim();
    if extra.is_empty() {
        return html.to_string();
    }
    match script_block(html) {
        Some(block) => {
            let body_end = block.rfind("</script").unwrap_or(block.len());
            let head = block[..body_end].trim_end();
            let tail = &block[body_end..];
            splice_script(html, &format!("{head}\n\n{extra}\n{tail}"))
        }
        None => splice_script(html, &format!("<script>\n{extra}\n</script>")),
    }
}

/// 从模型输出里取出"函数定义"片段。
///
/// 与 `tidy_script_block` 的区别：这里**不要** `<script>` 标签，只要定义本身 —— 调用方会
/// 把它追加进已有脚本。模型若又给了一整块 `<script>`，就把标签剥掉取内容。
pub fn tidy_function_appendix(output: &str) -> Option<String> {
    let trimmed = output.trim();
    let without_fence = ["```html", "```HTML", "```javascript", "```js", "```"]
        .iter()
        .find_map(|fence| trimmed.strip_prefix(*fence))
        .map(|rest| rest.trim_start())
        .unwrap_or(trimmed);
    let without_fence = without_fence
        .strip_suffix("```")
        .unwrap_or(without_fence)
        .trim();

    let lower = without_fence.to_ascii_lowercase();
    // 整页 HTML 说明模型跑偏了，拒收。
    if lower.contains("<html") || lower.contains("<body") || lower.contains("<style") {
        return None;
    }
    let body = match (lower.find("<script"), lower.rfind("</script")) {
        (Some(start), Some(close)) if close > start => {
            let body_start = without_fence[start..]
                .find('>')
                .map(|o| start + o + 1)
                .unwrap_or(start);
            without_fence[body_start..close].trim()
        }
        _ => without_fence,
    };
    if body.chars().count() < 40 {
        return None;
    }
    Some(body.to_string())
}

/// 从模型输出里切出"每个函数的完整新定义"。
///
/// 约定模型用 `// === FUNCTION: 名字 ===` 分隔，只输出需要改的那几个函数。相比"重写整段
/// 脚本"，这条路线是为**局部缺陷**准备的：实测脚本 8827 字符要 4200 token，而重写轮的输出
/// 上限只有 3928 —— 装不下就必然少函数（实测丢了 `createCard`，整页点不动）。
///
/// 解析宽容：大小写、空格差异都认；每个片段必须是完整的 `function 名字(...) { ... }`，
/// 否则丢弃（由调用方再按括号配平复核一道）。
pub fn tidy_function_patch(output: &str) -> Vec<(String, String)> {
    let trimmed = output.trim();
    let without_fence = ["```html", "```HTML", "```javascript", "```js", "```"]
        .iter()
        .find_map(|fence| trimmed.strip_prefix(*fence))
        .map(|rest| rest.trim_start())
        .unwrap_or(trimmed);
    let text = without_fence
        .strip_suffix("```")
        .unwrap_or(without_fence)
        .trim();

    // 找出所有标记：`// === FUNCTION: 名字 ===`。记录"标记行首"与"正文起"。
    let lower = text.to_ascii_lowercase();
    let mut marks: Vec<(usize, usize, String)> = Vec::new();
    let mut from = 0usize;
    while let Some(relative) = lower[from..].find("function:") {
        let at = from + relative;
        let name: String = text[at + "function:".len()..]
            .trim_start()
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '$')
            .collect();
        if !name.is_empty() {
            let line_start = text[..at].rfind('\n').map(|offset| offset + 1).unwrap_or(0);
            let line_end = text[at..]
                .find('\n')
                .map(|offset| at + offset + 1)
                .unwrap_or(text.len());
            marks.push((line_start, line_end, name));
        }
        from = at + "function:".len();
        if from >= text.len() {
            break;
        }
    }
    if marks.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for (index, (_, content_start, name)) in marks.iter().enumerate() {
        let end = marks
            .get(index + 1)
            .map(|(line_start, _, _)| *line_start)
            .unwrap_or(text.len());
        if *content_start >= end {
            continue;
        }
        let definition = text[*content_start..end].trim();
        // 必须是完整的函数定义，且确实定义的是这个名字。
        if !definition.starts_with("function") || !definition.ends_with('}') {
            continue;
        }
        let declares = definition["function".len()..]
            .trim_start()
            .starts_with(name.as_str());
        if !declares {
            continue;
        }
        out.push((name.clone(), definition.to_string()));
    }
    out
}

/// 拿新的完整定义替换掉页面脚本里同名的函数。一个都没替换成时返回 `None`。
///
/// 只认 `function 名字(...) { ... }` 这种声明形式（`const f = () => {}` 不在覆盖范围内）——
/// 定位和替换都由 `verify::function_spans` 给出，从后往前替换，避免前面的改动挪动后面的下标。
pub fn replace_functions(html: &str, replacements: &[(String, String)]) -> Option<String> {
    let block = script_block(html)?;
    let open_end = block.find('>')? + 1;
    let body_end = block.rfind("</script")?;
    let body = &block[open_end..body_end];

    let spans = crate::webgen::verify::function_spans(body);
    let mut hits: Vec<(usize, usize, String)> = Vec::new();
    for (name, definition) in replacements {
        if let Some((_, start, end)) = spans.iter().find(|(found, _, _)| found == name) {
            hits.push((*start, *end, definition.clone()));
        }
    }
    if hits.is_empty() {
        return None;
    }
    hits.sort_by_key(|(start, _, _)| *start);

    let chars: Vec<char> = body.chars().collect();
    let mut out = String::new();
    let mut cursor = 0usize;
    for (start, end, definition) in &hits {
        if *start < cursor {
            // 重叠（同名函数被重复声明）——跳过，交给自检继续报。
            continue;
        }
        out.extend(chars[cursor..*start].iter());
        out.push_str(definition.trim());
        cursor = *end;
    }
    out.extend(chars[cursor.min(chars.len())..].iter());

    // 注意：`splice_script` 要的是**整块含标签**的脚本。早先这里只传了脚本体，
    // 于是 `<script>` / `</script>` 一起丢掉，页面变成"有按钮、没脚本"（NO_SCRIPT）。
    // 保留原有的开标签，属性（若有）也一并留着。
    let rebuilt = format!("{}{out}</script>", &block[..open_end]);
    Some(splice_script(html, &rebuilt))
}

/// 去掉标签，留下纯文本（用于取按钮文案）。
fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for ch in text.chars() {
        match ch {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(ch),
            _ => {}
        }
    }
    out
}

/// 位置区间：每个 `<script>` 正文的起止下标。
fn script_spans(html: &str) -> Vec<(usize, usize)> {
    let lower = html.to_ascii_lowercase();
    let mut spans = Vec::new();
    let mut from = 0usize;
    while let Some(relative) = lower[from..].find("<script") {
        let open = from + relative;
        let Some(gt) = lower[open..].find('>') else {
            break;
        };
        let body_start = open + gt + 1;
        match lower[body_start..].find("</script") {
            Some(relative_end) => {
                let end = body_start + relative_end;
                spans.push((body_start, end));
                from = end + 1;
            }
            None => {
                spans.push((body_start, html.len()));
                break;
            }
        }
    }
    spans
}

/// 删除 `<script>` 正文里混进来的 Markdown 代码围栏行。
///
/// 实测故障（端到端第 4 轮）：脚本第 59 行是 ```` ```}); ```` —— 上一轮生成到半句就被
/// `num_predict` 截断，续写的下一轮又从"开一个代码块"写起，围栏就缝进了脚本中间。
/// 反引号落进 JS 里立刻是 `SyntaxError: Unexpected template string`，**整段脚本一行都不执行**，
/// 页面所有按钮集体失灵。`tidy()` 只清整篇首尾的围栏，中间的它管不到，所以必须按脚本块逐个清。
///
/// 只删"整行 trim 后以 ``` 开头"的行（可带 `html` / `js` 语言标记）。在正常的 JS 里，
/// 行首出现 ``` 本身就是非法写法，误删风险接近零。
pub fn strip_script_fences(html: &str) -> Option<String> {
    let mut changed = false;
    let mut out = html.to_string();
    for (start, end) in script_spans(html).into_iter().rev() {
        let body = out[start..end].to_string();
        if !body.lines().any(|line| peel_fence(line).is_some()) {
            continue;
        }
        let mut kept: Vec<String> = Vec::new();
        for line in body.lines() {
            match peel_fence(line) {
                // 纯围栏行（```、```html）整行丢掉。
                Some(rest) if rest.is_empty() => {}
                // 围栏后面还粘着代码（实测是 ` ```}); `），剥掉围栏保留代码。
                Some(rest) => kept.push(rest),
                None => kept.push(line.to_string()),
            }
        }
        // 逐行重组会丢掉末尾换行，补一个，避免把 `</script>` 粘到上一行行尾。
        out.replace_range(start..end, &format!("{}\n", kept.join("\n")));
        changed = true;
    }
    if changed {
        Some(out)
    } else {
        None
    }
}

/// 剥掉行首的 Markdown 围栏，返回剩下的内容。
///
/// `None` = 这行本来就不是围栏，原样保留；`Some("")` = 纯围栏行，整行可删；
/// `Some(代码)` = 围栏后面粘着代码，把代码留下。
fn peel_fence(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let backticks = trimmed.chars().take_while(|ch| *ch == '`').count();
    if backticks < 3 {
        return None;
    }
    let rest = trimmed[backticks..].trim();
    // ```` ```html ```` 这类语言标记也算纯围栏。
    let is_tag = [
        "html",
        "HTML",
        "js",
        "javascript",
        "css",
        "json",
        "vue",
        "ts",
        "tsx",
        "jsx",
    ]
    .iter()
    .any(|tag| rest == *tag);
    if rest.is_empty() || is_tag {
        Some(String::new())
    } else {
        Some(rest.to_string())
    }
}

/// 数一数脚本"代码区"里的反引号个数（忽略字符串与注释里的）。
///
/// 判据的依据是**奇偶性**：每个模板字符串贡献 2 个反引号，正常脚本里反引号必为偶数。
/// 奇数说明有模板字符串没闭合 —— 这会直接抛 SyntaxError，整段脚本不执行，
/// 而括号配平检查看它是"正常"的（实测就是这么漏过去的）。
pub fn stray_backticks(script: &str) -> usize {
    let mut count = 0usize;
    let mut chars = script.chars().peekable();
    #[derive(PartialEq)]
    enum State {
        Code,
        Line,
        Block,
        Single,
        Double,
    }
    let mut state = State::Code;
    while let Some(ch) = chars.next() {
        match state {
            State::Code => match ch {
                '`' => count += 1,
                '\'' => state = State::Single,
                '"' => state = State::Double,
                '/' => match chars.peek() {
                    Some('/') => {
                        chars.next();
                        state = State::Line;
                    }
                    Some('*') => {
                        chars.next();
                        state = State::Block;
                    }
                    _ => {}
                },
                _ => {}
            },
            State::Line => {
                if ch == '\n' {
                    state = State::Code;
                }
            }
            State::Block => {
                if ch == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    state = State::Code;
                }
            }
            State::Single => match ch {
                '\\' => {
                    chars.next();
                }
                '\'' => state = State::Code,
                _ => {}
            },
            State::Double => match ch {
                '\\' => {
                    chars.next();
                }
                '"' => state = State::Code,
                _ => {}
            },
        }
    }
    count
}

fn insert_before_body_close(html: &str, content: &str) -> String {
    match html.rfind("</body>") {
        Some(index) => format!("{}\n{}\n{}", &html[..index], content, &html[index..]),
        None => format!("{html}\n{content}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_the_existing_script_block() {
        let html = "<html><head></head><body><button>生成</button><script>const a = 1;</script></body></html>";
        let out = splice_script(html, "<script>const a = 2;</script>");
        assert!(out.contains("const a = 2;"));
        assert!(!out.contains("const a = 1;"));
        assert!(
            out.contains("<button>生成</button>"),
            "HTML 部分必须原样保留"
        );
    }

    #[test]
    fn merges_multiple_script_blocks_into_one() {
        let html = "<html><head><script>const cfg = 1;</script></head><body><script>const a = 1;</script></body></html>";
        let out = splice_script(html, "<script>const merged = true;</script>");
        assert_eq!(out.matches("<script").count(), 1);
        assert!(out.contains("const merged = true;"));
    }

    #[test]
    fn inserts_script_when_page_has_none() {
        let html = "<html><body><button>生成</button></body></html>";
        let out = splice_script(html, "<script>const a = 1;</script>");
        assert!(out.contains("<script>"));
        assert!(out.find("<script>").unwrap() < out.find("</body>").unwrap());
    }

    #[test]
    fn collects_ids_and_button_labels() {
        let html = r#"<html><body><select id="scene"><option>古风</option></select>
<button id="go">生成名字</button><button>复制</button></body></html>"#;
        let outline = element_outline(html);
        assert!(outline.contains("scene"));
        assert!(outline.contains("生成名字"));
        assert!(outline.contains("复制"));
    }

    #[test]
    fn script_block_returns_full_region() {
        let html = "<html><body><script>\nconst a = 1;\n</script></body></html>";
        let block = script_block(html).expect("应当找到脚本块");
        assert!(block.starts_with("<script"));
        assert!(block.ends_with("</script>"));
    }

    #[test]
    fn tidies_fenced_and_bare_script_outputs() {
        let fenced =
            tidy_script_block("```javascript\n<script>const a = 1;</script>\n```").unwrap();
        assert!(fenced.starts_with("<script"));
        assert!(!fenced.contains("```"));

        let bare = format!("const a = 1;\n{}", "x();\n".repeat(60));
        let wrapped = tidy_script_block(&bare).expect("裸 JS 应当被补上标签");
        assert!(wrapped.starts_with("<script>"));
        assert!(wrapped.ends_with("</script>"));
    }

    #[test]
    fn pulls_the_script_out_of_a_stray_full_page() {
        // 模型没按要求"只给脚本"，又输出了一整页 —— 里面的脚本仍可取用，HTML 部分丢掉。
        let page = format!(
            "<html><body><script>const a = 1;</script>{}</body></html>",
            "<div>x</div>".repeat(40)
        );
        let block = tidy_script_block(&page).expect("能从误输出的整页里取出脚本");
        assert!(block.starts_with("<script"));
        assert!(block.contains("const a = 1;"));
        assert!(!block.contains("<div>"));
    }

    #[test]
    fn rejects_a_response_without_any_script() {
        assert!(tidy_script_block("抱歉，这部分我需要更多信息。").is_none());
        assert!(tidy_script_block("").is_none());
    }

    #[test]
    fn appends_definitions_inside_the_existing_script() {
        let html =
            "<html><body><button>生成</button><script>\nconst a = 1;\n</script></body></html>";
        let out = append_to_script(html, "function pinyin(name) {\n    return name;\n}");
        assert_eq!(out.matches("<script").count(), 1, "不能新增脚本块");
        assert!(out.contains("const a = 1;"));
        assert!(out.contains("function pinyin(name)"));
        // 追加的定义必须在 </script> 之前。
        assert!(out.find("function pinyin").unwrap() < out.find("</script>").unwrap());
    }

    #[test]
    fn tidies_bare_and_wrapped_function_definitions() {
        let bare = r#"
function pinyin(text) {
    const table = { 雨: "yu" };
    return text.split("").map((ch) => table[ch] || ch).join("");
}
"#;
        let cleaned = tidy_function_appendix(bare).expect("裸函数定义应当被接受");
        assert!(cleaned.starts_with("function pinyin"));
        assert!(!cleaned.contains("```"));

        let wrapped = format!("```javascript\n<script>{bare}\n</script>\n```");
        let cleaned = tidy_function_appendix(&wrapped).expect("带标签与围栏也要收拾干净");
        assert!(cleaned.starts_with("function pinyin"));
        assert!(!cleaned.contains("<script"));
        assert!(!cleaned.contains("```"));
    }

    #[test]
    fn refuses_a_full_page_as_a_function_appendix() {
        let page = "<html><body><script>function a() {}</script></body></html>";
        assert!(tidy_function_appendix(page).is_none());
        assert!(tidy_function_appendix("好的，我来补上。").is_none());
    }

    #[test]
    fn picks_out_labelled_functions_from_a_rewrite() {
        let output = r#"好的，改这两个函数：
// === FUNCTION: generateOptions ===
function generateOptions() {
    return prop[Math.floor(Math.random() * prop.length)];
}
// === FUNCTION: renderOptions ===
function renderOptions(list) {
    const box = document.getElementById("out");
    box.innerHTML = list.join("");
}
"#;
        let patched = tidy_function_patch(output);
        let names: Vec<&str> = patched.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["generateOptions", "renderOptions"]);
        assert!(patched[0].1.contains("const prop = [") == false);
        assert!(patched[0].1.contains("prop[Math.floor"));
        assert!(patched[1].1.ends_with('}'));
    }

    #[test]
    fn drops_a_labelled_block_that_is_not_a_whole_function() {
        // 只给半截函数（少了结尾大括号）必须被丢掉，否则替换进脚本会立刻把页面写坏。
        let output = "// === FUNCTION: broken ===\nfunction broken() {\n  return 1;\n";
        assert!(tidy_function_patch(output).is_empty());
    }

    #[test]
    fn replaces_only_the_named_function() {
        let html = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const prop = ["用左手", "穿睡衣"];
function generateOptions() {
    return "占位";
}
function other() { return 42; }
document.getElementById("go").onclick = generateOptions;
</script></body></html>"#;
        let replacement = "function generateOptions() {\n    return prop[Math.floor(Math.random() * prop.length)];\n}";
        let patched = replace_functions(
            html,
            &[("generateOptions".to_string(), replacement.to_string())],
        )
        .expect("应当替换成功");
        assert!(patched.contains("prop[Math.floor"));
        assert!(!patched.contains("\"占位\""));
        assert!(patched.contains("function other() { return 42; }"));
        // 关键：`splice_script` 要的是整块含标签的脚本。只传脚本体的话，
        // `<script>` 标签会一起丢掉，页面变成"有按钮、没脚本"。
        assert!(patched.contains("<script"), "脚本开标签不能丢：{patched}");
        assert!(patched.contains("</script>"), "脚本闭标签不能丢：{patched}");
        assert!(
            crate::webgen::dom::script_block(&patched).is_some(),
            "替换后脚本块应当仍然存在"
        );
        assert!(
            !crate::webgen::verify::verify_page(&patched, "")
                .iter()
                .any(|issue| issue.code == "NO_SCRIPT"),
            "替换后页面必须仍有脚本"
        );
    }

    #[test]
    fn returns_none_when_the_function_is_not_a_declaration() {
        // 箭头函数不在覆盖范围内：宁可交给自检继续报，也不能瞎改。
        let html = "<html><body><button id=\"go\">x</button><script>\nconst go = () => 1;\n</script></body></html>";
        assert!(replace_functions(
            html,
            &[("go".to_string(), "function go() { return 2; }".to_string())]
        )
        .is_none());
    }
}
