//! 大文档的快速梳理（纯规则，不调模型）。
//!
//! 本地小模型读不完十万字的文档：8B 模型在 8GB 显存上只能开 8192 的上下文窗口，
//! 逐块让模型过一遍要十几分钟，而且照样会漏。所以"读薄"这一步不能交给模型。
//!
//! 这里的做法是纯规则、O(n)、对 10 万字也是毫秒级：
//!
//! ```text
//! 按标题切段 → 给每段打"离需求有多近"的分 → 选出真正的需求内容 → 恢复原始顺序
//! ```
//!
//! 产出是一份 ≤ 目标字数的"梳理稿"，模型只在它上面做一次规格化。这样整个预处理
//! 阶段只有一次模型调用，与文档长度基本无关。

use super::clean::{is_structural, looks_like_heading, take_head};

/// 梳理稿的目标长度：保证一次模型调用就能吃下（含输出预留）。
///
/// 这不是个可以随便调大的值：它会连同系统提示词一起进 8192 的上下文窗口，而窗口里还要
/// 留出四成给模型输出。系统提示词每次加规则，这里就得相应让一点 —— 由
/// `context_budget_fits_a_distilled_draft` 这个测试守着。
pub const DISTILL_TARGET_CHARS: usize = 5_800;

/// 数据附录的字数上限。
///
/// 词库、字表、取值清单这类内容不是"背景资料"，而是生成逻辑唯一的数据源 —— 丢了它们，
/// 模型就只能自己编。实测一份 1.7 万字的 PRD，第 15 章的种子词库（6 个风格的前置/后置
/// 字表，1057 字）因为归在"数据"小节、评分又不如"数据模型"那几节，被预算整个挤掉了，
/// 于是模型把"生成名字"实现成从场景名里拆字（产出「网网名名」）。所以数据块单独留一份
/// 预算，不参与小节之间的竞争。
pub const DATA_APPENDIX_CHARS: usize = 1_500;

/// 各小节预算的总量（`Bucket::budget()` 之和）。数据附录占用后按比例缩放。
const TOTAL_BUCKET_BUDGET: usize = 5_300;

/// 缩放到最小也要给每个小节留这么多，否则等于把整类内容关掉。
const MIN_BUCKET_BUDGET: usize = 240;

/// 说明性开头段最多保留多少字（通常是产品一句话介绍，值得优先保住）。
const PREAMBLE_KEEP_CHARS: usize = 600;

/// 单个章节参与打分时按多少字封顶（避免超长章节仅靠体量压过其他章节）。
const SCORE_LENGTH_CAP: usize = 1_200;

/// 没有标题结构时，按段落合并成多大的块。
const FALLBACK_BLOCK_CHARS: usize = 1_000;

/// 正向关键词：出现越多，这一段越像"要做的东西"。
const POSITIVE: [(&str, i64); 40] = [
    ("功能", 8),
    ("需求", 8),
    ("页面", 6),
    ("界面", 5),
    ("字段", 7),
    ("数据", 4),
    ("交互", 6),
    ("操作", 4),
    ("支持", 4),
    ("筛选", 5),
    ("搜索", 5),
    ("导出", 5),
    ("上传", 4),
    ("下载", 4),
    ("列表", 4),
    ("表单", 5),
    ("点击", 4),
    ("按钮", 4),
    ("展示", 4),
    ("统计", 4),
    ("风格", 5),
    ("视觉", 5),
    ("配色", 6),
    ("颜色", 5),
    ("色值", 6),
    ("字体", 4),
    ("圆角", 4),
    ("响应式", 5),
    ("布局", 4),
    ("约束", 4),
    ("限制", 3),
    ("单文件", 6),
    ("规则", 3),
    ("校验", 4),
    ("提示", 4),
    ("模块", 3),
    ("流程", 3),
    ("权限", 3),
    ("接口", 3),
    ("实时", 3),
];

/// 负向关键词：这类章节与"生成一个网页"无关，看到就整段丢掉。
const NEGATIVE: [(&str, i64); 26] = [
    ("目录", 40),
    ("修订记录", 40),
    ("版本历史", 40),
    ("变更记录", 40),
    ("版本说明", 30),
    ("参考文献", 35),
    ("致谢", 30),
    ("术语表", 25),
    ("索引", 25),
    ("保密", 30),
    ("免责", 30),
    ("法律声明", 30),
    ("版权", 25),
    ("审批", 25),
    ("签署", 25),
    ("盖章", 25),
    ("干系人", 20),
    ("报价", 30),
    ("商务", 25),
    ("付款", 25),
    ("发票", 25),
    ("合同条款", 25),
    ("售后", 20),
    ("培训", 15),
    ("测试用例", 12),
    ("验收流程", 10),
];

/// 一段（章节）及其打分结果。
struct Section {
    heading: Option<String>,
    text: String,
    score: i64,
    /// 说明性开头，无论分数高低都优先保留。
    pinned: bool,
}

/// 梳理结果，附带用于日志展示的统计。
pub struct DistillResult {
    pub text: String,
    pub source_chars: usize,
    pub kept_chars: usize,
    pub source_sections: usize,
    pub kept_sections: usize,
    /// 没能挤进预算的章节标题，用于告诉用户"哪些内容没被采用"。
    pub dropped_headings: Vec<String>,
    /// 被单独保下来的数据附录字数（词库/字表/取值清单）。
    pub data_appendix_chars: usize,
}

/// 梳理结果里最多列出多少个被丢弃的章节。
const MAX_DROPPED_HEADINGS: usize = 12;

/// 把清洗后的长文档梳理成一份能一次送进模型的"梳理稿"。
pub fn distill(cleaned: &str, target_chars: usize) -> DistillResult {
    let source_chars = cleaned.chars().count();
    let mut sections = split_sections(cleaned);

    if sections.is_empty() {
        return DistillResult {
            text: take_head(cleaned, target_chars),
            source_chars,
            kept_chars: source_chars.min(target_chars),
            source_sections: 0,
            kept_sections: 0,
            dropped_headings: Vec::new(),
            data_appendix_chars: 0,
        };
    }

    let source_sections = sections.len();
    for section in sections.iter_mut() {
        section.score = score_section(section);
    }

    // 数据附录优先占位：词库/字表/取值清单排在预算竞争之外。
    let appendix = collect_data_appendix(cleaned, DATA_APPENDIX_CHARS.min(target_chars / 4));
    let appendix_chars = appendix
        .as_ref()
        .map(|text| text.chars().count())
        .unwrap_or(0);

    let mut selected = vec![false; sections.len()];
    // 预留一点开销给小节标题与分隔空行。
    let mut used = BUCKET_TITLE_RESERVE + appendix_chars;
    let mut preamble: Option<String> = None;

    // 文档开头通常是"一句话说明 + 模块总览"，单独取出、不占任何小节的预算。
    if let Some(index) = sections.iter().position(|section| section.pinned) {
        let heading = sections[index].heading.as_deref().unwrap_or("").to_string();
        let text = take_head(&sections[index].text, PREAMBLE_KEEP_CHARS);
        let block = format_document_head(&heading, &text);
        used += block.chars().count();
        preamble = Some(block);
        selected[index] = true;
    }

    // 数据附录与开头段吃掉的预算，从小节份额里按比例扣 —— 总量仍卡在 target 之内。
    let bucket_pool = target_chars.saturating_sub(used).min(TOTAL_BUCKET_BUDGET);
    let budget_of = |bucket: Bucket| -> usize {
        let base = bucket.budget();
        if bucket_pool >= TOTAL_BUCKET_BUDGET {
            base
        } else {
            (base * bucket_pool / TOTAL_BUCKET_BUDGET)
                .max(MIN_BUCKET_BUDGET)
                .min(base)
        }
    };

    // 第一轮：每个小节在自己的预算内取分最高的章节。
    // 这是「不整类丢失」的关键 —— 全局取分会把整个"视觉规范"一次性丢掉。
    for bucket in Bucket::ORDER {
        let budget = budget_of(bucket);
        let mut candidates: Vec<usize> = (0..sections.len())
            .filter(|index| {
                !selected[*index]
                    && sections[*index].score > 0
                    && classify(&sections[*index]) == bucket
            })
            .collect();
        candidates.sort_by(|left, right| sections[*right].score.cmp(&sections[*left].score));

        let mut bucket_used = 0usize;
        for index in candidates {
            if selected[index] {
                continue;
            }
            let cost = section_cost(&sections[index], &sections[index].text);
            if bucket_used + cost <= budget {
                bucket_used += cost;
                used += cost;
                selected[index] = true;
            } else {
                // 放不下就按行裁剪而不是整段丢弃 —— 直接取开头会把段落中部的色值、
                // 优先级标记这些"实现必需"的信息切掉。裁完继续尝试后面的候选。
                let overhead = cost.saturating_sub(sections[index].text.chars().count());
                let room = budget.saturating_sub(bucket_used + overhead);
                if room > MIN_TAIL_CHARS {
                    sections[index].text = trim_lines(&sections[index].text, room);
                    let cost = section_cost(&sections[index], &sections[index].text);
                    if bucket_used + cost <= budget {
                        bucket_used += cost;
                        used += cost;
                        selected[index] = true;
                    }
                }
            }
        }
    }

    // 第二轮：把小节没用完的预算补给全局分数最高的剩余章节。
    let mut rest: Vec<usize> = (0..sections.len())
        .filter(|index| {
            !selected[*index]
                && sections[*index].score > 0
                && classify(&sections[*index]) != Bucket::Other
        })
        .collect();
    rest.sort_by(|left, right| sections[*right].score.cmp(&sections[*left].score));
    for index in rest {
        let cost = section_cost(&sections[index], &sections[index].text);
        if used + cost <= target_chars {
            used += cost;
            selected[index] = true;
        }
    }

    let mut out = String::new();
    if let Some(preamble) = preamble.as_ref() {
        out.push_str(preamble);
        out.push_str("\n\n");
    }
    // 数据附录紧跟在开头段之后：万一最后仍超出预算要截断，从尾部截也不会伤到它。
    if let Some(appendix) = appendix.as_ref() {
        out.push_str(appendix);
        out.push_str("\n\n");
    }
    for bucket in Bucket::ORDER {
        let picked: Vec<usize> = (0..sections.len())
            .filter(|index| {
                selected[*index]
                    && !sections[*index].pinned
                    && classify(&sections[*index]) == bucket
            })
            .collect();
        if picked.is_empty() {
            continue;
        }
        out.push_str("## ");
        out.push_str(bucket.title());
        out.push('\n');
        for index in picked {
            let section = &sections[index];
            if let Some(heading) = section.heading.as_ref() {
                let heading = heading.trim();
                if !heading.trim_start_matches('#').trim().is_empty() {
                    out.push_str(heading);
                    out.push('\n');
                }
            }
            out.push_str(section.text.trim());
            out.push_str("\n\n");
        }
    }

    // 兜底：如果文档几乎全是"其他"类内容（没有命中任何小节关键词），
    // 分桶会产出过短的梳理稿 —— 这时退回按分数贪心填充，避免空手而归。
    let assembled = out.trim().to_string();
    if assembled.chars().count() < target_chars / 4 {
        let mut rest: Vec<usize> = (0..sections.len())
            .filter(|index| !selected[*index] && sections[*index].score > 0)
            .collect();
        rest.sort_by(|left, right| sections[*right].score.cmp(&sections[*left].score));
        for index in rest {
            let cost = section_cost(&sections[index], &sections[index].text);
            if used + cost > target_chars {
                continue;
            }
            used += cost;
            selected[index] = true;
        }
        let mut rebuilt = String::new();
        if let Some(preamble) = preamble.as_ref() {
            rebuilt.push_str(preamble);
            rebuilt.push_str("\n\n");
        }
        if let Some(appendix) = appendix.as_ref() {
            rebuilt.push_str(appendix);
            rebuilt.push_str("\n\n");
        }
        for (index, section) in sections.iter().enumerate() {
            if !selected[index] || section.pinned {
                continue;
            }
            if let Some(heading) = section.heading.as_ref() {
                rebuilt.push_str(heading.trim());
                rebuilt.push('\n');
            }
            rebuilt.push_str(section.text.trim());
            rebuilt.push_str("\n\n");
        }
        out = rebuilt;
    }

    let mut dropped_headings: Vec<String> = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        if selected[index] || section.score <= 0 {
            continue;
        }
        if dropped_headings.len() >= MAX_DROPPED_HEADINGS {
            break;
        }
        let label = match section.heading.as_ref() {
            Some(heading) => heading.trim().to_string(),
            None => take_head(section.text.trim(), 20),
        };
        if !label.is_empty() {
            dropped_headings.push(label);
        }
    }

    let text = out.trim();
    // 兜底：保证一定不超预算。
    let text = if text.chars().count() > target_chars {
        take_head(text, target_chars)
    } else {
        text.to_string()
    };
    DistillResult {
        kept_chars: text.chars().count(),
        text,
        source_chars,
        source_sections,
        kept_sections: selected.iter().filter(|chosen| **chosen).count(),
        dropped_headings,
        data_appendix_chars: appendix_chars,
    }
}

/// 从全文里挑出"数据行"，拼成一份始终保留的数据附录。
///
/// 分**两遍**收，顺序很重要：
///
/// 1. 先收**形态上就是数据**的行（`looks_like_data_line`）—— 一行里塞着六个以上 1–2 字的
///    中文词，那就是一份字表，不管它写在哪一节。
/// 2. 再用剩下的预算补"线索词"命中的行（前置/后置/权重/黑名单……）。
///
/// 为什么要分两遍：线索词很宽，"首版全部基于本地词库"这种散文句也能命中。实测它们排在
/// 文档前面、又刚好把 1400 字的预算吃光，真正在文档末尾的字表反而一个字都没进来 ——
/// 分两遍之后，字表先占位，散文句只能捡剩下的。
pub fn collect_data_appendix(cleaned: &str, budget: usize) -> Option<String> {
    // 线索词只收"数据形状"的词：像"词库"这种写在正文里也毫无违和感的词不能要。
    const HINTS: [&str; 13] = [
        "前置",
        "后置",
        "单字池",
        "字池",
        "字表",
        "字根",
        "词素",
        "黑名单",
        "权重",
        "词库",
        "词条",
        "模板库",
        "规则库",
    ];

    let mut data_lines: Vec<(usize, &str)> = Vec::new();
    let mut hinted: Vec<(usize, &str)> = Vec::new();

    for (index, line) in cleaned.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 400 {
            continue;
        }
        if looks_like_data_line(trimmed) {
            data_lines.push((index, trimmed));
        } else if HINTS.iter().any(|hint| trimmed.contains(hint)) {
            hinted.push((index, trimmed));
        }
    }

    let mut picked: Vec<(usize, &str)> = Vec::new();
    let mut used = 0usize;
    for (index, line) in data_lines.iter().chain(hinted.iter()) {
        if picked.iter().any(|(_, existing)| existing == line) {
            continue;
        }
        let cost = line.chars().count() + 1;
        if used + cost > budget {
            continue;
        }
        used += cost;
        picked.push((*index, line));
    }
    // 恢复原文顺序，读起来还是文档本来的样子。
    picked.sort_by_key(|(index, _)| *index);

    if picked.is_empty() {
        return None;
    }
    Some(format!(
        "## 数据附录（生成逻辑的数据源，必须原样使用）\n{}",
        picked
            .iter()
            .map(|(_, line)| *line)
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

/// 一行是不是"数据清单"。
///
/// 三种形状都算数据：
/// 1. **短词表** —— `前置：墨 云 霜 月 青 白 霜 影`：1–2 字的中文短词占六成；
/// 2. **顿号清单** —— `抛硬币决定、原地转三圈、问一只路过的猫、…`：`、` 分隔的长词条；
/// 3. **槽位模板** —— `把「{D}」交给{obj}决定`：含两个以上 `{槽位}` 的句子骨架。
///
/// 2、3 是后补的。早先只认短词表，可"词库写在 markdown 表格里、条目是四五个字的长词"
/// 才是更常见的写法 —— 实测「人生选择题」那份需求的整份词库（抛硬币决定 / 外星人 /
/// 凌晨三点 / 广场舞阿姨…）被判成普通散文丢在规格之外，生成阶段拿不到词库只能自己编，
/// 这正是"名字瞎生成"的病根。
pub fn looks_like_data_line(line: &str) -> bool {
    looks_like_slot_template(line) || looks_like_enumerated_list(line) || short_word_table(line)
}

/// 槽位模板：`把「{D}」交给{obj}决定`。
/// 要求两个以上 `{槽位}`，把句子骨架和"代码里偶带一个大括号"区分开。
fn looks_like_slot_template(line: &str) -> bool {
    let mut slots = 0usize;
    let mut rest = line;
    while let Some(open) = rest.find('{') {
        let Some(relative) = rest[open..].find('}') else {
            break;
        };
        let inner = &rest[open + 1..open + relative];
        if !inner.is_empty() && inner.chars().count() <= 16 {
            slots += 1;
        }
        rest = &rest[open + relative + 1..];
    }
    slots >= 2
}

/// 顿号清单：`抛硬币决定、原地转三圈、…` —— 四项以上、每项都是中文短语。
///
/// 只认**表格行/列表行**。正文里偶带的顿号句（"功能点：支持列表、新增、编辑、删除…"）
/// 形状上很像清单，但一旦收进附录就会白占预算、还会把真正的小节挤出去。
fn looks_like_enumerated_list(line: &str) -> bool {
    let trimmed = line.trim_start();
    let first = trimmed.chars().next().unwrap_or(' ');
    if !(first == '|' || matches!(first, '-' | '*' | '·' | '•')) {
        return false;
    }
    let items: Vec<&str> = line.split('、').collect();
    if items.len() < 4 {
        return false;
    }
    let phrases = items
        .iter()
        .filter(|item| {
            let plain: String = item
                .chars()
                .filter(|ch| {
                    !ch.is_ascii_punctuation()
                        && !ch.is_whitespace()
                        && !matches!(ch, '｜' | '「' | '」' | '『' | '』' | '“' | '”')
                })
                .collect();
            let length = plain.chars().count();
            length >= 2
                && length <= 16
                && plain.chars().filter(|ch| is_cjk(*ch)).count() * 2 >= length
        })
        .count();
    phrases >= 4
}

/// 短词表：`前置：墨 云 霜 月 青 白 霜 影`。
///
/// 先把引号、括号、逗号、冒号这类包装符换成空格，再看 tokens：如果"词"里有六成以上是
/// 1–2 字的中文，且这样的短词至少 6 个，就认定是字表/词表。纯数字与纯标点不计入分母，
/// 这样 `"front": { "暗": 4, "血": 3, … }` 这种 JSON 池也认得出来。
fn short_word_table(line: &str) -> bool {
    const WRAPPERS: &[char] = &[
        '"', '\'', '`', '{', '}', '[', ']', ',', ':', '：', ';', '；', '|', '-', '*', '·', '（',
        '）', '(', ')', '、',
    ];
    let normalized: String = line
        .chars()
        .map(|ch| if WRAPPERS.contains(&ch) { ' ' } else { ch })
        .collect();

    let tokens: Vec<&str> = normalized
        .split_whitespace()
        .filter(|token| {
            !token
                .chars()
                .all(|ch| ch.is_ascii_digit() || ch.is_ascii_punctuation())
        })
        .collect();
    if tokens.len() < 6 {
        return false;
    }
    let short = tokens
        .iter()
        .filter(|token| {
            let length = token.chars().count();
            length >= 1 && length <= 2 && token.chars().all(is_cjk)
        })
        .count();
    short >= 6 && short * 10 >= tokens.len() * 6
}

fn is_cjk(ch: char) -> bool {
    matches!(ch, '\u{4E00}'..='\u{9FFF}')
}

/// 标准格式的小节分类：梳理稿按这套分类组织，保证每个方面都有代表内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bucket {
    /// 页面结构 / 布局 / 路由
    Page,
    /// 功能需求 / 模块 / 用户场景
    Feature,
    /// 数据模型与字段
    Data,
    /// 交互与状态
    Interaction,
    /// 视觉规范
    Visual,
    /// 技术约束
    Constraint,
    /// 未命中以上任何分类
    Other,
}

impl Bucket {
    /// 顺序与标准格式一致，生成时按这个顺序拼接。
    const ORDER: [Bucket; 6] = [
        Bucket::Page,
        Bucket::Feature,
        Bucket::Data,
        Bucket::Interaction,
        Bucket::Visual,
        Bucket::Constraint,
    ];

    fn title(self) -> &'static str {
        match self {
            Bucket::Page => "页面结构",
            Bucket::Feature => "功能需求",
            Bucket::Data => "数据与字段",
            Bucket::Interaction => "交互与状态",
            Bucket::Visual => "视觉规范",
            Bucket::Constraint => "约束",
            Bucket::Other => "其他",
        }
    }

    /// 各小节的字符预算（合计 5300，另留开头段与标题开销）。
    /// 视觉规范给得最多：配色和圆角是决定"页面像不像样"的第一要素。
    fn budget(self) -> usize {
        match self {
            Bucket::Page => 650,
            Bucket::Feature => 1750,
            Bucket::Data => 700,
            Bucket::Interaction => 700,
            Bucket::Visual => 1050,
            Bucket::Constraint => 450,
            Bucket::Other => 0,
        }
    }
}

/// 小节标题与空行的预留开销。
const BUCKET_TITLE_RESERVE: usize = 120;

/// 小节内最后一个章节至少要放下这么多字，否则不截。
const MIN_TAIL_CHARS: usize = 140;

/// 按关键词把章节分到标准格式的小节里。
///
/// 标题命中权重高于正文：`### 3.2 模块 A` 靠标题就能确定属于"功能需求"，
/// 而正文里出现的"配色"多半只是顺带一提。
fn classify(section: &Section) -> Bucket {
    const RULES: [(Bucket, &[&str]); 6] = [
        (
            Bucket::Visual,
            &[
                "配色",
                "色值",
                "色彩",
                "字体",
                "圆角",
                "间距",
                "视觉",
                "响应式",
                "断点",
                "设计基调",
                "主题",
                "体验规范",
            ],
        ),
        (
            Bucket::Interaction,
            &[
                "交互",
                "状态",
                "反馈",
                "空状态",
                "错误",
                "键盘",
                "无障碍",
                "拖拽",
                "动效",
                "二次确认",
                "校验提示",
                "操作说明",
            ],
        ),
        (
            Bucket::Data,
            &[
                "数据模型",
                "数据结构",
                "字段",
                "表结构",
                "存储",
                "localstorage",
                "实体",
                "数据类型",
                "词库",
                "持久化",
                "schema",
                "键名",
            ],
        ),
        (
            Bucket::Page,
            &[
                "页面结构",
                "布局",
                "路由",
                "信息架构",
                "首屏",
                "导航",
                "区块",
                "页面组成",
                "界面结构",
                "信息层级",
                "站点",
                "页面",
            ],
        ),
        (
            Bucket::Constraint,
            &[
                "技术方案",
                "技术栈",
                "约束",
                "非功能",
                "性能",
                "兼容",
                "隐私",
                "安全",
                "不做",
                "限制",
                "打包",
                "seo",
            ],
        ),
        (
            Bucket::Feature,
            &[
                "功能",
                "需求",
                "模块",
                "场景",
                "story",
                "故事",
                "算法",
                "生成",
                "质量控制",
                "验收",
                "埋点",
                "词条",
                "流程",
            ],
        ),
    ];

    let heading = section.heading.as_deref().unwrap_or("").to_lowercase();
    let body = section.text.to_lowercase();

    let mut best = (Bucket::Other, 0i64);
    for (bucket, keywords) in RULES {
        let mut score = 0i64;
        for keyword in keywords.iter().copied() {
            if heading.contains(keyword) {
                score += 3;
            }
            if body.contains(keyword) {
                score += 1;
            }
        }
        if score > best.1 {
            best = (bucket, score);
        }
    }
    best.0
}

/// 把文档开头整理成 `# 标题` + 说明段。
fn format_document_head(heading: &str, text: &str) -> String {
    let title = heading.trim().trim_start_matches('#').trim();
    let mut out = String::new();
    if !title.is_empty() {
        out.push_str("# ");
        out.push_str(title);
        out.push('\n');
    }
    let text = text.trim();
    if !text.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(text);
    }
    out
}

/// 按标题把文档切成章节。
fn split_sections(cleaned: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut heading: Option<String> = None;
    let mut body = String::new();
    let mut heading_count = 0usize;

    for line in cleaned.lines() {
        if is_heading(line) {
            heading_count += 1;
            if !body.trim().is_empty() || heading.is_some() {
                let pinned = sections.is_empty();
                sections.push(Section {
                    heading: heading.take(),
                    text: body.trim().to_string(),
                    score: 0,
                    pinned,
                });
            }
            body.clear();
            heading = Some(line.trim().to_string());
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    if !body.trim().is_empty() || heading.is_some() {
        let pinned = sections.is_empty();
        sections.push(Section {
            heading,
            text: body.trim().to_string(),
            score: 0,
            pinned,
        });
    }

    // 标题太少说明文档没有结构，改用"段落块"切分，否则整篇只变成一两个巨型章节。
    if heading_count < 3 {
        return split_blocks(cleaned);
    }
    sections
}

/// 没有标题结构时的兜底：按空行分段，再把小段合并成固定大小的块。
fn split_blocks(cleaned: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut buffer = String::new();

    for block in cleaned.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        if !buffer.is_empty()
            && buffer.chars().count() + block.chars().count() > FALLBACK_BLOCK_CHARS
        {
            let pinned = sections.is_empty();
            sections.push(Section {
                heading: None,
                text: buffer.trim().to_string(),
                score: 0,
                pinned,
            });
            buffer.clear();
        }
        buffer.push_str(block);
        buffer.push_str("\n\n");
    }
    if !buffer.trim().is_empty() {
        let pinned = sections.is_empty();
        sections.push(Section {
            heading: None,
            text: buffer.trim().to_string(),
            score: 0,
            pinned,
        });
    }
    sections
}

/// 一行是否像章节标题。
fn is_heading(line: &str) -> bool {
    let text = line.trim();
    if text.is_empty() || text.chars().count() > 40 {
        return false;
    }
    // Markdown 标题优先。
    if text.starts_with('#') {
        return true;
    }
    // 列表项、编号条目不是标题，避免把文档切得过碎。
    if is_structural(text) {
        return false;
    }
    // "一、xxx" / "（一）xxx"
    let first = text.chars().next().unwrap_or(' ');
    if "一二三四五六七八九十".contains(first)
        && matches!(
            text.chars().nth(1),
            Some('、') | Some('.') | Some('）') | Some(')')
        )
    {
        return true;
    }
    // "第X章 xxx"
    if text.starts_with('第') {
        if let Some(position) = text.chars().position(|c| "章节部分篇".contains(c)) {
            if position <= 4 {
                return true;
            }
        }
    }
    // "2.1 xxx" 这类多级编号
    if is_multi_level_number(text) {
        return true;
    }
    // 短且完全不含标点，当成小标题。
    looks_like_heading(text)
}

/// 形如 `2.1` / `3.2.1` 的多级编号开头。
fn is_multi_level_number(text: &str) -> bool {
    let mut parts = 0usize;
    let mut expect_digit = true;
    for ch in text.chars() {
        if expect_digit {
            if ch.is_ascii_digit() {
                expect_digit = false;
                continue;
            }
            return false;
        }
        match ch {
            '.' => {
                parts += 1;
                expect_digit = true;
            }
            ' ' | '\t' => return parts >= 1,
            _ => return false,
        }
    }
    false
}

/// 一个章节在最终梳理稿里占用的字数（含标题与分隔用的空行）。
fn section_cost(section: &Section, text: &str) -> usize {
    let heading = section
        .heading
        .as_ref()
        .map(|heading| heading.chars().count() + 4)
        .unwrap_or(0);
    heading + text.chars().count()
}

/// 一行信息对"照着做出网页"的价值：数值越大越该保住。
fn line_priority(line: &str) -> i64 {
    let text = line.trim();
    if text.is_empty() {
        return 0;
    }
    let mut priority = 0i64;
    if contains_color_value(text) {
        priority += 10;
    }
    if text.contains("P0") {
        priority += 6;
    }
    if text.contains("P1") {
        priority += 3;
    }
    if has_field_line(text) {
        priority += 4;
    }
    if has_list_line(text) {
        priority += 2;
    }
    if text.starts_with('|') {
        priority += 3;
    }
    if text.chars().count() > 60 {
        priority -= 1;
    }
    priority
}

/// 预算不够时按行裁剪：优先保住色值、优先级标记、字段与表格行这类"实现必需"的行，
/// 剩下的空间再按原文顺序补上下文。直接用 take_head 会把段落中部的关键信息切掉。
fn trim_lines(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.to_string();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![false; lines.len()];
    let mut used = 0usize;

    // 开头两行通常是标题与主旨，先占住。
    for (index, line) in lines.iter().enumerate().take(2) {
        used += line.chars().count() + 1;
        keep[index] = true;
    }
    // 再捞高价值行。
    for (index, line) in lines.iter().enumerate() {
        if keep[index] || used >= room {
            continue;
        }
        if line_priority(line) <= 0 {
            continue;
        }
        let cost = line.chars().count() + 1;
        if used + cost <= room {
            used += cost;
            keep[index] = true;
        }
    }
    // 还有空间就按原文顺序补，保持行文连贯。
    for (index, line) in lines.iter().enumerate() {
        if used >= room {
            break;
        }
        if keep[index] {
            continue;
        }
        let cost = line.chars().count() + 1;
        if used + cost > room {
            break;
        }
        used += cost;
        keep[index] = true;
    }

    lines
        .iter()
        .enumerate()
        .filter(|(index, _)| keep[*index])
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 给一个章节打分。
fn score_section(section: &Section) -> i64 {
    let haystack = format!(
        "{}\n{}",
        section.heading.as_deref().unwrap_or(""),
        section.text
    );
    let heading = section.heading.as_deref().unwrap_or("");

    // 标题命中负向词，直接判死（"修订记录""参考文献"这类整段没有价值）。
    for (word, weight) in NEGATIVE.iter() {
        if heading.contains(word) {
            return -(*weight);
        }
    }

    let mut score: i64 = 0;
    for (word, weight) in POSITIVE.iter() {
        let hits = haystack.matches(word).count().min(4) as i64;
        score += hits * weight;
    }
    for (word, weight) in NEGATIVE.iter() {
        if haystack.contains(word) {
            score -= weight / 4;
        }
    }

    // 结构化内容更有价值：列表、色值、字段式排版。
    if has_list_line(&section.text) {
        score += 4;
    }
    if contains_color_value(&haystack) {
        score += 8;
    }
    if has_field_line(&section.text) {
        score += 4;
    }

    // 按长度归一：短章节不该因为"没写够"而吃亏，长章节也不该只靠体量取胜。
    let length = section.text.chars().count().min(SCORE_LENGTH_CAP) as i64;
    let length_factor = 30 + length * 70 / SCORE_LENGTH_CAP as i64;
    score * length_factor / 100
}

/// 是否包含列表行。
fn has_list_line(text: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed.starts_with("· ")
            || trimmed.starts_with("（")
            || trimmed.starts_with('(')
            || is_multi_level_number(trimmed)
    })
}

/// 是否包含色值。
fn contains_color_value(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index + 3 < bytes.len() {
        if bytes[index] == b'#' {
            let hex = bytes[index + 1..]
                .iter()
                .take_while(|byte| byte.is_ascii_hexdigit())
                .count();
            if hex == 3 || hex == 6 || hex == 8 {
                return true;
            }
        }
        index += 1;
    }
    false
}

/// 是否是"字段：说明"式的排版（需求文档里很常见）。
fn has_field_line(text: &str) -> bool {
    text.lines()
        .filter(|line| line.contains('：') || line.contains(':'))
        .count()
        >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_document() -> String {
        let mut doc = String::from(
            "# 订单系统 需求说明书\n本文档描述订单系统的功能设计，供研发团队实现。\n\n",
        );
        doc.push_str("## 修订记录\nv1.0 初稿 张三\nv1.1 修改 李四\nv1.2 补充 王五\n\n");
        doc.push_str("## 一、项目背景\n我们希望通过这个系统提升效率，本模块需要与相关方充分沟通，确保方案在长期演进中保持一致性、可维护性与良好的用户体验。\n\n");
        doc.push_str("## 二、功能需求\n- 订单列表支持按状态筛选、按时间排序\n- 新建订单填写客户、金额、备注\n- 支持导出 CSV\n- 逾期订单用红色标记\n\n");
        doc.push_str("## 三、视觉规范\n深色背景 #12141A，卡片 #1C1F27，强调色 #FF7A1A，圆角 12px，移动端单列。\n\n");
        doc.push_str("## 四、参考文献\n[1] 某某规范\n[2] 某某标准\n\n");
        doc.push_str("## 五、术语表\n订单：指一次交易记录。\n");
        doc
    }

    #[test]
    fn drops_boilerplate_sections() {
        let cleaned = super::super::clean::clean_document(&long_document());
        let result = distill(&cleaned, DISTILL_TARGET_CHARS);

        assert!(result.text.contains("订单列表支持按状态筛选"));
        assert!(result.text.contains("#12141A"));
        assert!(!result.text.contains("v1.2 补充 王五"), "修订记录应被剔除");
        assert!(!result.text.contains("某某标准"), "参考文献应被剔除");
        assert!(!result.text.contains("术语表"), "术语表应被剔除");
    }

    #[test]
    fn reports_dropped_headings() {
        let cleaned = super::super::clean::clean_document(&long_document());
        let result = distill(&cleaned, DISTILL_TARGET_CHARS);
        // 与网页实现无关的章节（修订记录这类）由负向规则直接排除，不该出现在梳理稿里，
        // 也不该混进"被预算挤掉"的清单 —— 那份清单只用来提示"内容可能不够用"。
        assert!(!result.text.contains("修订记录"), "无关章节不应进入梳理稿");
        assert!(
            !result
                .dropped_headings
                .iter()
                .any(|heading| heading.contains("功能需求")),
            "保留的章节不该出现在丢弃清单里"
        );
        assert!(result.kept_chars <= DISTILL_TARGET_CHARS);
    }

    #[test]
    fn reports_sections_dropped_for_budget() {
        let mut doc =
            String::from("# 甲系统需求\n面向内部团队的甲系统，用于集中管理日常业务数据。\n\n");
        for index in 1..=40 {
            doc.push_str(&format!(
                "## 模块 {index}\n功能点：支持列表、新增、编辑、删除、关键词搜索与导出，字段包括名称、金额、备注。\n\n"
            ));
        }
        let cleaned = super::super::clean::clean_document(&doc);
        let result = distill(&cleaned, 1_200);
        assert!(
            result.kept_chars <= 1_200,
            "梳理稿 {} 字超过预算",
            result.kept_chars
        );
        assert!(
            !result.dropped_headings.is_empty(),
            "预算不够时必须报告被挤掉的章节"
        );
    }

    #[test]
    fn keeps_document_order() {
        let cleaned = super::super::clean::clean_document(&long_document());
        let result = distill(&cleaned, DISTILL_TARGET_CHARS);
        let functions = result.text.find("功能需求").unwrap();
        let visual = result.text.find("视觉规范").unwrap();
        assert!(functions < visual, "章节应保持原文顺序");
    }

    #[test]
    fn respects_target_length() {
        let mut doc = String::new();
        for index in 1..=60 {
            doc.push_str(&format!(
                "## 模块 {index}\n本模块支持列表展示、条件筛选、导出与实时更新，字段包括名称、金额、"
            ));
            doc.push_str(
                "备注、创建时间、负责人与状态；支持按时间范围筛选与关键词搜索，异常时给出明确提示。\n\n",
            );
        }
        let cleaned = super::super::clean::clean_document(&doc);
        let result = distill(&cleaned, 3_000);
        assert!(
            result.kept_chars <= 3_000,
            "梳理稿 {} 字，超过目标",
            result.kept_chars
        );
        assert!(
            result.kept_sections < result.source_sections,
            "应发生截取：保留 {} / {} 个章节",
            result.kept_sections,
            result.source_sections
        );
    }

    #[test]
    fn handles_documents_without_headings() {
        let mut doc = String::new();
        for _ in 0..40 {
            doc.push_str("这一段描述系统的功能与页面布局，包含字段、筛选、导出与配色要求。\n\n");
        }
        let result = distill(&doc, 2_000);
        assert!(!result.text.is_empty());
        assert!(result.kept_chars <= 2_000);
    }

    #[test]
    fn detects_character_lists() {
        assert!(looks_like_data_line(
            "- 前置：墨 云 霜 月 青 白 疏 空 浅 薄 寒 澄"
        ));
        assert!(looks_like_data_line(
            r#""front": { "暗": 4, "血": 3, "终": 3, "弑": 2, "冥": 3, "极": 3 }"#
        ));
        assert!(!looks_like_data_line(
            "- 生成结果需优先包含该字，或使用含该字的词素。允许为空。"
        ));
        assert!(!looks_like_data_line(
            "| 情侣名 | 2–3 字 | 任意 | 必须成对输出 |"
        ));
        assert!(!looks_like_data_line(
            "打开页面后 3 秒内可点击生成，无需任何说明"
        ));
    }

    #[test]
    fn treats_markdown_word_banks_and_templates_as_data() {
        // 词库写在 markdown 表格里、条目是四五个字的长词 —— 最常见的写法，也是"瞎生成"的根因。
        assert!(looks_like_data_line(
            "| `act` | 离谱动作 | 抛硬币决定、原地转三圈、问一只路过的猫、把决定权交给外卖小哥、掷骰子、反向执行 |"
        ));
        assert!(looks_like_data_line(
            "| `obj` | 离谱对象 | 外星人、小区保安、楼下便利店老板、未来穿越回来的自己、一只柯基、广场舞阿姨 |"
        ));
        // 槽位模板：句子骨架也算数据。
        assert!(looks_like_data_line(
            "| T01 | 把「{D}」交给{obj}决定 | D=困境, obj |"
        ));
        assert!(looks_like_data_line(
            "| T02 | {time}，{act}，然后{conseq_ridiculous} | time, act |"
        ));
        // 反例：正文里的普通列举不是数据，别去挤数据附录的预算。
        assert!(!looks_like_data_line(
            "本系统支持订单管理、客户管理、报表统计等模块，面向中小团队。"
        ));
        assert!(!looks_like_data_line(
            "功能点：支持列表、新增、编辑、删除、关键词搜索与导出，字段包括名称、金额、备注。"
        ));
        assert!(!looks_like_data_line(
            "| 情侣名 | 2–3 字 | 任意 | 必须成对输出 |"
        ));
    }

    #[test]
    fn keeps_a_table_shaped_word_bank_in_the_digest() {
        // 「人生选择题」那份需求的词库长这样：全在表格里，条目是长词条。
        let doc = String::from(
            "# 人生选择题 需求文档\n帮助用户在纠结时获得启发，主打替你纠结。\n\n\
             ## 5.1 词库设计\n词库是生成效果的地基，按类别组织，每类至少 20 条。\n\n\
             | 类别 key | 含义 | 示例条目 |\n\
             |----------|------|----------|\n\
             | `act` | 离谱动作 | 抛硬币决定、原地转三圈、问一只路过的猫、把决定权交给外卖小哥、掷骰子 |\n\
             | `obj` | 离谱对象 | 外星人、小区保安、楼下便利店老板、未来穿越回来的自己、一只柯基、广场舞阿姨 |\n\
             | `time` | 时间修饰 | 凌晨三点、下暴雨时、周一早高峰、吃夜宵时、宿醉醒来时、年会当天 |\n\n\
             ## 5.2 模板库设计\n模板即句子骨架，用槽位占位，至少 20 个离谱模板。\n\n\
             | 编号 | 模板 | 槽位类型 |\n\
             |------|------|----------|\n\
             | T01 | 把「{D}」交给{obj}决定 | D=困境, obj |\n\
             | T04 | 让{obj}帮你选「{D}」，选完{conseq_ridiculous} | obj, D |\n",
        );
        let cleaned = super::super::clean::clean_document(&doc);
        let result = distill(&cleaned, DISTILL_TARGET_CHARS);

        for word in [
            "抛硬币决定",
            "广场舞阿姨",
            "凌晨三点",
            "把决定权交给外卖小哥",
        ] {
            assert!(result.text.contains(word), "词库条目「{word}」应进入梳理稿");
        }
        assert!(result.text.contains("{obj}"), "槽位模板应进入梳理稿");
        assert!(result.data_appendix_chars > 0, "应当单独保出数据附录");
        assert!(result.kept_chars <= DISTILL_TARGET_CHARS);
    }

    #[test]
    fn keeps_the_word_bank_out_of_budget_competition() {
        let mut doc =
            String::from("# 起名工具 需求说明\n面向大众的起名工具，打开就能生成名字。\n\n");
        // 十几个更"高分"的数据类章节，足以把预算吃光 —— 词库不能再靠分数去抢。
        for index in 1..=12 {
            doc.push_str(&format!(
                "## 数据模型 {index}\n字段：名称、金额、备注、创建时间、负责人、状态；持久化到 localStorage，键名 nlp:{index}。\n\n"
            ));
        }
        doc.push_str(
            "## 15. 附录：可直接使用的种子词库\n以下为起手词库。\n\n### 古风\n\
             - 前置：墨 云 霜 月 青 白 疏 空 浅 薄 寒 澄 江 竹 松 鹤 兰 砚 舟 灯\n\
             - 后置：衣 舟 鹤 弦 辞 生 客 子 娘 君 卿 书 尘 雪 阁 台 语 影 年 序 山 川\n",
        );

        let cleaned = super::super::clean::clean_document(&doc);
        let result = distill(&cleaned, 2_400);
        assert!(result.data_appendix_chars > 0, "应当单独保出数据附录");
        assert!(result.text.contains("墨 云 霜"), "前置字表必须留在梳理稿里");
        assert!(result.text.contains("衣 舟 鹤"), "后置字表必须留在梳理稿里");
        assert!(
            result.kept_chars <= 2_400,
            "梳理稿 {} 字超预算",
            result.kept_chars
        );
    }

    #[test]
    fn real_character_lists_win_over_prose_that_mentions_data() {
        let mut doc = String::from("# 起名工具 需求说明\n\n## 1. 背景\n");
        // 一堆提到"词库"的散文句，排在字表前面 —— 它们不该把数据附录的预算吃光。
        for index in 1..=40 {
            doc.push_str(&format!(
                "- 第 {index} 条说明：首版全部基于本地词库，保证零后端、零调用成本、零延迟，与本次实现相关。\n"
            ));
        }
        doc.push_str(
            "\n## 15. 附录：种子词库\n\n### 古风\n\
             - 前置：墨 云 霜 月 青 白 疏 空 浅 薄 寒 澄\n\
             - 后置：衣 舟 鹤 弦 辞 生 客 子 娘 君 卿 书 尘\n",
        );
        let cleaned = super::super::clean::clean_document(&doc);
        let result = distill(&cleaned, 4_000);
        assert!(result.text.contains("墨 云 霜"), "字表必须挤进数据附录");
        assert!(result.text.contains("衣 舟 鹤"), "后置字表必须挤进数据附录");
    }

    #[test]
    fn data_appendix_is_absent_when_the_document_has_none() {
        let cleaned = super::super::clean::clean_document(&long_document());
        let result = distill(&cleaned, DISTILL_TARGET_CHARS);
        assert_eq!(result.data_appendix_chars, 0);
        assert!(!result.text.contains("数据附录"));
    }

    #[test]
    fn handles_hundred_thousand_char_document() {
        let mut doc =
            String::from("# 大型业务系统 需求说明书\n本文档描述一个大型业务系统的完整需求。\n\n");
        // 大量与网页实现无关的章节，用来撑到十万字以上。
        for index in 1..=900 {
            doc.push_str(&format!(
                "## 附录 {index} 参考资料\n本附录列举相关规范、引用文献与历史记录，供查阅使用，与本次网页实现无关。\n本附录需要与相关方充分沟通，确保文档在长期演进中保持一致性、可维护性与良好的用户体验。\n\n"
            ));
        }
        // 真正要保留的功能章节。
        for index in 1..=200 {
            doc.push_str(&format!(
                "## 功能模块 {index}\n支持列表展示、条件筛选、导出与实时更新；字段包括名称、金额、备注、创建时间、负责人、状态。\n配色 #12141A，圆角 12px，移动端单列。\n\n"
            ));
        }

        let total = doc.chars().count();
        assert!(total >= 100_000, "测试文档只有 {total} 字");

        let cleaned = super::super::clean::clean_document(&doc);
        let started = std::time::Instant::now();
        let result = distill(&cleaned, DISTILL_TARGET_CHARS);
        let elapsed = started.elapsed();
        eprintln!(
            "十万字文档：原始 {total} 字 → 清洗 {} 字 → 梳理稿 {} 字，梳理耗时 {elapsed:?}",
            cleaned.chars().count(),
            result.kept_chars
        );
        assert!(
            elapsed.as_millis() < 1_000,
            "梳理耗时 {elapsed:?}，超出预期（应为毫秒级）"
        );

        assert!(
            result.kept_chars <= DISTILL_TARGET_CHARS,
            "十万字文档的梳理稿 {} 字，超过预算",
            result.kept_chars
        );
        assert!(
            result.kept_chars > 1_000,
            "梳理稿过短，功能章节可能被丢光了"
        );
        assert!(result.text.contains("支持列表展示"), "功能内容应被保留");
        assert!(result.text.contains("功能模块 1"), "应保留靠前的功能章节");
        assert!(
            result.kept_sections < result.source_sections / 2,
            "应只保留少数章节：{} / {}",
            result.kept_sections,
            result.source_sections
        );
    }
}
