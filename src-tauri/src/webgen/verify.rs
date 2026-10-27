//! 生成结果的确定性功能自检。
//!
//! 8B 模型很擅长写"看着像那么回事"的页面：布局、按钮、交互文案、提示语一应俱全，但核心
//! 业务逻辑是一根**桩**。实测最深的一次：需求里给了整份名字词库，"生成网名"却被实现成
//!
//! ```js
//! const charCode = Math.floor(Math.random() * 26) + 97;   // 随机 a-z
//! part += String.fromCharCode(charCode);
//! ```
//!
//! 于是页面上滚出一串 `qpqtmcs`、`osds`；而那份中文词库被丢在页面底部当一个装饰性的
//! `<ul>` 列表，生成函数从头到尾没读过它。应用原先只校验 `<html>` / `</html>` 是否成对，
//! 这种"跑得通、但是假的"产出会被直接判为成功 —— 用户点开看到的是一堆乱码。
//!
//! 所以这里加一道**纯规则**的功能自检（毫秒级，不调模型）：把"表面工作"识别出来，
//! 由 `lib.rs` 带着具体证据回灌给模型做定向修复。规则只做**有确凿证据**的判定，
//! 宁可漏报也不错报 —— 误报会让模型去改本来正确的地方，反而把页面改坏。
//!
//! 检查项一览：
//!
//! | 编号 | 级别 | 判据 |
//! |---|---|---|
//! | `NO_SCRIPT` | 严重 | 有按钮却完全没有脚本 |
//! | `FAKE_NOISE` | 严重 | 同一个函数体内既 `Math.random()` 又 `String.fromCharCode` —— 随机字符拼凑结果 |
//! | `PLACEHOLDER_DATA` | 严重 | 出现「模拟数据 / 示例数据 / 待实现」这类自认是桩的痕迹 |
//! | `NO_DATA_SOURCE` | 严重 | 有随机生成逻辑，但页面里找不到任何数据数组 |
//! | `GENERATOR_IGNORES_DATA` | 严重 | 有数据数组，但随机生成函数一个都没用上 —— 数据只是摆设 |
//! | `DUPLICATE_DECLARATION` | 严重 | 同一作用域里重复声明 `let`/`const` —— 续写把同一段逻辑写了两遍，脚本抛 SyntaxError |
//! | `DUPLICATE_FUNCTION` | 严重 | 同一作用域里同名函数声明两遍 —— 后一份覆盖前一份，而它往往是残缺的 |
//! | `UNDEFINED_REFERENCE` | 严重 | 调用了从未定义的函数 —— 括号配平、`node --check` 也过，但点下去是 ReferenceError |
//! | `UNUSED_DATA_POOL` | 严重 | 数据数组声明了却从未被读取 —— 词库抄进页面，生成分支却没接上它 |
//! | `OPTION_UNHANDLED` | 严重 | 下拉选项在脚本里从未被判断或查表 —— 用户选中它时不走任何分支，渲染出 `undefined` |
//! | `SCRIPT_UNBALANCED` | 严重 | 括号或引号不配平 —— 输出被截断后拼回去的残骸 |
//! | `BUTTON_NO_HANDLER` | 提示 | 有按钮但没有任何事件绑定 |
//! | `DEAD_FUNCTION` | 提示 | 函数定义了却从未被调用 |
//! | `FEATURE_MISSING` | 提示 | 需求里的功能关键词大量未出现在页面中 |

use std::collections::HashMap;

/// 问题级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// 功能不可用或数据是假的：必须进入定向修复轮次。
    Critical,
    /// 可疑但不致命：只在日志里提示，不触发修复。
    Major,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Critical => "严重",
            Severity::Major => "提示",
        }
    }
}

/// 一项自检发现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// 机器可读的编号，日志与修复指令都用它。
    pub code: &'static str,
    pub severity: Severity,
    /// 给人看的具体证据，要能定位到代码。
    pub detail: String,
    /// 给模型的修复要求：说清"改成什么样"，而不只是"这里不对"。
    pub fix: String,
}

impl Issue {
    /// 日志里的一行。
    pub fn log_line(&self) -> String {
        format!("[{}] {}：{}", self.severity.label(), self.code, self.detail)
    }
}

/// 一个问题的"致命度"。跨版本择优时不能只数问题条数 ——
/// 1 条 `UNDEFINED_REFERENCE`（点下去就抛 ReferenceError、整页交互全废）远比
/// 3 条 `DEAD_FUNCTION`（有函数没接上）更该避免。
///
/// 实测教训：第 1 轮出稿只有 1 条严重问题（`UNUSED_DATA_POOL`），整段重写之后变成
/// 2 条（`UNDEFINED_REFERENCE` + `UNUSED_DATA_POOL`）。只比条数会认为"1 < 2 该回滚"，
/// 但若按条数比较出现反向情形（比如 1 条悬空调用 vs 2 条死函数），就会选错。
/// 所以按"会不会让页面彻底不能用"分级加权。
pub fn issue_weight(code: &str) -> usize {
    match code {
        // 页面彻底不能用：没有脚本，或脚本语法就是残的。
        // SCRIPT_FENCE / UNCLOSED_TEMPLATE / REGEX_PROPERTY_INVALID 也归这一档：一个混进来的
        // ``` 或一个不存在的 Unicode 属性名就让整段脚本抛 SyntaxError，与"没有脚本"等价
        // （括号配平检查根本看不出来）。
        "NO_SCRIPT"
        | "SCRIPT_UNBALANCED"
        | "SCRIPT_FENCE"
        | "UNCLOSED_TEMPLATE"
        | "REGEX_PROPERTY_INVALID" => 10,
        // 交互全废：按钮点下去抛 ReferenceError / 运行期必抛 DOM 异常，页面上什么都不会发生。
        "UNDEFINED_REFERENCE" | "FRAGILE_DOM_SWAP" => 9,
        // 页面自己埋的死链：是单文件网页，点开必然 404。
        "DEAD_LINK" => 9,
        // 引了外部资源：单文件网页里必然取不到，用户看到破图或缺功能。
        "EXTERNAL_DEPENDENCY" => 7,
        // 残品顶掉完好的一代 / 同一作用域重复声明。
        "DUPLICATE_FUNCTION" | "DUPLICATE_DECLARATION" => 8,
        // 数据是编的：假噪声、占位数据、根本没有数据源。
        "FAKE_NOISE" | "PLACEHOLDER_DATA" | "NO_DATA_SOURCE" => 7,
        // 初始化中途抛错：后面的绑定与渲染全都不执行。
        "NULL_STORAGE_READ" => 6,
        // 用弹窗代替界面：功能"看着有"，但需求要求的页面内呈现根本不存在。
        "PLACEHOLDER_ALERT" => 6,
        // 语义错但看起来完全正常：中文输入被自己的校验挡掉。
        "NARROW_WORD_CLASS" => 6,
        // 功能缺一块：数据抄进来了没接进逻辑，或选项没有对应分支。
        "UNUSED_DATA_POOL" | "OPTION_UNHANDLED" => 3,
        // 有函数没接上：成片出现时等于整块功能是空壳（severity 也随数量升为 Critical）。
        "DEAD_FUNCTION" => 4,
        // 按钮没绑事件：整页一处绑定都没有 → 所有交互全废（severity = Critical）。
        "BUTTON_NO_HANDLER" => 5,
        // 需求成片没落地。
        "FEATURE_MISSING" => 2,
        _ => 1,
    }
}

/// 给一组问题打分，返回 `(权重和, 条数)`。
///
/// 元组按字典序比较 —— 先比总分，同分再比条数。用于回答"修复后的这一版，是不是比
/// 之前那一版更好"，从而决定要不要回滚。见 `issue_weight` 说明为什么不能只数条数。
pub fn health(issues: &[Issue]) -> (usize, usize) {
    let score = issues.iter().map(|issue| issue_weight(issue.code)).sum();
    (score, issues.len())
}

/// 打分时把"缺失函数的个数"也算进成本。
///
/// 为什么必须看页面而不只看问题清单：`check_undefined_references` 把一次缺 10 个函数和
/// 一次缺 2 个函数**压成同一条** `UNDEFINED_REFERENCE`。于是"补进 8 个、还剩 2 个"在条数上
/// 是 1 条 → 2 条（反而变多）—— 实测端到端第 6 轮就是这样：一轮补写了 10 个函数里的 8 个，
/// 却被判成"越改越差"整轮回滚，白扔掉一次有效修复，最后交付的仍是 10 个函数全缺的版本。
///
/// 每个缺失函数都是一处"点下去就抛 ReferenceError"的入口，按个数计价才反映真实收益。
pub fn health_with_html(html: &str, issues: &[Issue]) -> (usize, usize) {
    let (score, count) = health(issues);
    (score + missing_function_names(html).len(), count)
}

/// 粗估"把整段脚本重写一遍"需要多少输出 token。
///
/// 实测教训：页面脚本 8827 字符，修复轮的单轮输出上限只有 3928 token —— 根本装不下，
/// 模型写到一半就断了，交出来的重写版要么少函数（`UNDEFINED_REFERENCE`）、要么直接把
/// 好好的逻辑写坏。**装不下的重写不该开始**，宁可保留原版并如实告诉用户。
///
/// 估算按字符类型分开算：汉字 1 字 ≈ 1 token，其余（英文标识符、标点、缩进）1 token ≈ 4
/// 字符。宁可高估 —— 高估只是放弃一次自动修复，低估会交付残品。
pub fn script_token_estimate(html: &str) -> usize {
    let script = crate::webgen::dom::merged_script(html);
    let mut cjk = 0usize;
    let mut rest = 0usize;
    for ch in script.chars() {
        let code = ch as u32;
        // 基本汉字区（含扩展 A）。够覆盖页面里的中文文案与中文注释。
        if (0x4E00..=0x9FFF).contains(&code) || (0x3400..=0x4DBF).contains(&code) {
            cjk += 1;
        } else {
            rest += 1;
        }
    }
    cjk + rest.div_ceil(4)
}

/// 判断一次改写是否把页面**改退化了** —— 该回滚保留原版。
///
/// 为什么必须单独有这一道：`verify_page` 面对一个空壳文件**没有问题可报**。
/// 实测 18918 字的页面被模型替换成 3006 字片段后，`<body>` 与 `<script>` 全丢，
/// 自检却报"通过"—— 因为它按"页面上有的东西"找缺陷，而那份残品上什么都没有。
/// 于是"越改越烂"被当成了"改好了"。
///
/// 判据不用抽象分数，只看两条硬事实：
///   1. **体量骤降**：新产物不足原版的 60%（定向修改绝不该让页面瘦掉一大截）
///   2. **结构丢失**：原版有的 `<body>` / `<script>` / `</html>`，新版没了
///
/// 只用于"改写已有页面"的场景（生成首版时没有原版可比，不适用）。
pub fn looks_degraded(original: &str, candidate: &str) -> Option<String> {
    let before = original.chars().count();
    let after = candidate.chars().count();
    if before == 0 {
        return None;
    }

    // 结构标签：原版有、新版没了 —— 这是"页面被截断/重写"最硬的证据。
    for tag in ["<body", "</html>", "<script"] {
        let had = original.to_ascii_lowercase().contains(tag);
        let has = candidate.to_ascii_lowercase().contains(tag);
        if had && !has {
            return Some(format!("原页面包含 {tag} 而改写后没有了（页面结构被破坏）"));
        }
    }

    // 体量骤降：不足原版 60%。放宽到 60% 是为了给"用户要求删掉一大块内容"留余地，
    // 同时仍然能拦住"整页被替换成片段"这种 16% 的灾难。
    if after * 100 < before * 60 {
        return Some(format!(
            "改写后只剩 {after} 字，不足原页面 {before} 字的 60%（内容大量丢失）"
        ));
    }

    None
}

/// 跑完整自检。`spec` 是标准格式需求，用于功能覆盖比对。
pub fn verify_page(html: &str, spec: &str) -> Vec<Issue> {
    let mut issues = Vec::new();
    let script = crate::webgen::dom::merged_script(html);

    let button_count = count_of(&html.to_ascii_lowercase(), "<button");
    if button_count > 0 && script.trim().is_empty() {
        issues.push(Issue {
            code: "NO_SCRIPT",
            severity: Severity::Critical,
            detail: format!("页面有 {button_count} 个按钮，但完全没有任何脚本，所有交互都不会有反应。"),
            fix: "补上 <script> 并实现全部交互逻辑：按钮点击必须改变页面状态，不允许出现点了没反应的按钮。"
                .to_string(),
        });
    }

    if !script.is_empty() {
        check_balanced_delimiters(&script, &mut issues);
        check_script_fences(&script, &mut issues);
        check_unclosed_template(&script, &mut issues);
        check_regex_property(&script, &mut issues);
        check_fake_noise(&script, &mut issues);
        check_data_source(&script, &mut issues);
        check_duplicate_declarations(&script, &mut issues);
        check_duplicate_functions(&script, &mut issues);
        check_undefined_references(&script, &mut issues);
        check_unused_data_pools(&script, &mut issues);
        check_null_storage_reads(&script, &mut issues);
        check_narrow_word_class(&script, &mut issues);
        check_dead_functions(&script, html, &mut issues);
        check_alerts(&script, &mut issues);
        check_fragile_dom_swap(&script, &mut issues);
    }
    check_dead_links(html, &mut issues);
    check_external_dependencies(html, &mut issues);
    check_option_coverage(html, &script, &mut issues);
    check_placeholder(&script, &mut issues);
    check_buttons(html, &script, &mut issues);
    check_feature_coverage(html, spec, &mut issues);

    issues
}

/// 是否拦下了致命问题（决定要不要进修复轮次）。
pub fn has_critical(issues: &[Issue]) -> bool {
    issues
        .iter()
        .any(|issue| issue.severity == Severity::Critical)
}

/// "页面看着齐全、其实点不动"的功能性缺陷。
///
/// 这一类问题**不算语法错**，所以静态检查全过、括号也配平，但用户点下去什么都不会发生 ——
/// 正是"不具备功能的网页"最典型的形态。它们过去被标成 `Major`（提示级），
/// 于是"三个按钮全没绑事件"的页面会被直接交付。
///
/// 现在把它们单独归成一类，交给调用方决定要不要为它再跑一轮定向修复。
pub const FUNCTIONAL_CODES: &[&str] = &[
    // 有按钮，但脚本里一处事件绑定都没有。
    "BUTTON_NO_HANDLER",
    // 定义了却从没被调用的函数：需求里的功能只写了个空壳。
    "DEAD_FUNCTION",
    // 调用了未定义的函数（Critical，但同样属于这一类）。
    "UNDEFINED_REFERENCE",
    // 数据抄进来了没接进逻辑 / 选项没有对应分支。
    "UNUSED_DATA_POOL",
    "OPTION_UNHANDLED",
    // 需求里成片的功能在页面上找不到任何痕迹。
    "FEATURE_MISSING",
];

/// 这条问题是不是"功能没真正实现"。
pub fn is_functional_defect(issue: &Issue) -> bool {
    FUNCTIONAL_CODES.contains(&issue.code)
}

/// 这组问题里有没有功能性缺陷。
///
/// 与 `has_critical` 的区别：`has_critical` 只认"语法/运行期崩"，
/// 这一条连"按钮没绑事件""功能只写空壳"也算 —— 因为从用户视角，"点不动"和"报错"
/// 是同一件事：网页不具备它声称的功能。
pub fn has_functional_defect(issues: &[Issue]) -> bool {
    issues.iter().any(is_functional_defect)
}

/// 功能性缺陷加起来够不够格再跑一轮修复。
///
/// 不能"有一点点就修"：`DEAD_FUNCTION` 很容易因误报（比如函数只在内联 `onclick` 里被调用）
/// 而出现一条，为它多跑一轮一分钟的推理不划算。判据取**加权致命度之和**：
///
/// - 一条 `UNDEFINED_REFERENCE`（9 分）→ 够。点下去必抛 ReferenceError，没有讨论余地。
/// - 任意条 `BUTTON_NO_HANDLER`（2 分/条）→ 一条就够。这条误报率极低：只有在整份脚本
///   连一处 `onclick` / `addEventListener` 都找不到时才报，出现了就是真没绑。
/// - 其余功能性缺陷 → 按加权和 ≥ 6 判：两条 `UNUSED_DATA_POOL`(3+3) 够，
///   一条单独的 `FEATURE_MISSING`(1) 或 `DEAD_FUNCTION`(1) 不够。
pub fn functional_repair_worthwhile(issues: &[Issue]) -> bool {
    let defects: Vec<&Issue> = issues
        .iter()
        .filter(|issue| is_functional_defect(issue))
        .collect();
    if defects.is_empty() {
        return false;
    }
    if defects
        .iter()
        .any(|issue| matches!(issue.code, "UNDEFINED_REFERENCE" | "BUTTON_NO_HANDLER"))
    {
        return true;
    }
    let weight: usize = defects.iter().map(|issue| issue_weight(issue.code)).sum();
    weight >= 6
}

/// 把问题清单写成给模型看的修复要求。
///
/// 关键在"给证据 + 给目标"：只说"功能不对"，8B 模型会重写一版同样的桩；把出问题的
/// 那几行代码和"必须改成什么样"一起摆出来，它才改得动。
pub fn repair_instruction(issues: &[Issue]) -> String {
    let mut out = String::from(
        "【功能自检未通过】上面这个单文件网页已经生成过一次，但程序化自检发现它只是\"看起来像\"，核心逻辑没有真正实现：\n\n",
    );
    for (index, issue) in issues.iter().enumerate() {
        out.push_str(&format!(
            "{}. [{}] {}\n   必须改成：{}\n\n",
            index + 1,
            issue.code,
            issue.detail,
            issue.fix
        ));
    }
    out
}

// ============================================================
// 各项检查
// ============================================================

/// 随机字符拼凑：同一个函数体内既出现 `Math.random()` 又出现 `String.fromCharCode`。
///
/// 这一条是本次故障的直接指纹 —— 正常页面不会用"随机码点"造字符串。
fn check_fake_noise(script: &str, issues: &mut Vec<Issue>) {
    let mut offenders = Vec::new();
    for (name, body) in function_bodies(script) {
        let lower = body.to_ascii_lowercase();
        if lower.contains("math.random") && lower.contains("fromcharcode") {
            offenders.push(name);
        }
    }
    // 兜底：函数体识别不到（比如箭头函数）时，用邻近窗口再判一次。
    if offenders.is_empty() && appears_nearby(script, "fromcharcode", "math.random", 400) {
        offenders.push("<匿名>".to_string());
    }
    if offenders.is_empty() {
        return;
    }
    issues.push(Issue {
        code: "FAKE_NOISE",
        severity: Severity::Critical,
        detail: format!(
            "{} 用 String.fromCharCode + Math.random() 拼凑随机字符，生成出来的是一串无意义的字母（实测页面上出现了 qpqtmcs、osds 这种结果）。",
            offenders.join(" / ")
        ),
        fix: "删掉随机码点拼串的写法，改成从需求给出的词库数组里随机取词后拼接。页面上面向用户的文字必须全部是真实内容，不允许由随机字符、随机数字或空串拼出来。"
            .to_string(),
    });
}

/// 生成逻辑有没有真正的数据源。
///
/// 两个致命情形：
/// - 有随机生成逻辑，但整页没有任何数组（`NO_DATA_SOURCE`）；
/// - 有数组，但随机生成函数一个都没引用（`GENERATOR_IGNORES_DATA`）—— 数据只是摆设。
fn check_data_source(script: &str, issues: &mut Vec<Issue>) {
    let functions = function_bodies(script);
    let random_functions: Vec<(String, String)> = functions
        .iter()
        .filter(|(_, body)| body.to_ascii_lowercase().contains("math.random"))
        .cloned()
        .collect();
    if random_functions.is_empty() {
        return;
    }

    let arrays = top_level_arrays(script);
    if arrays.is_empty() {
        issues.push(Issue {
            code: "NO_DATA_SOURCE",
            severity: Severity::Critical,
            detail: format!(
                "脚本里有随机生成逻辑（{}），但整页找不到任何数据数组 —— 生成结果没有数据来源。",
                names_of(&random_functions)
            ),
            fix: "把需求中给出的数据（词库、场景、风格、字段取值等）以 JS 数组常量的形式内联进页面，并让生成函数从数组里取值组合。"
                .to_string(),
        });
        return;
    }

    // 数据是否真的流进了随机生成逻辑。
    //
    // 不能只查"函数体里有没有数组名"：常见的抽词辅助函数写成 `pick(list)`，数组是通过
    // 参数传进去的，函数体里当然看不到数组名（那是形参）。所以这里沿调用关系往下追一层
    // —— 只要随机函数拿到的是"吃过数据"的函数递过来的东西，就算数据真的用上了。
    let fed = data_fed_functions(&arrays, &functions);
    if random_functions.iter().any(|(name, _)| fed.contains(name)) {
        return;
    }

    issues.push(Issue {
        code: "GENERATOR_IGNORES_DATA",
        severity: Severity::Critical,
        detail: format!(
            "页面里有数据数组（{}），但随机生成函数（{}）一个都没用上 —— 数据只是装饰，生成逻辑凭空捏造结果。",
            arrays.join(" / "),
            names_of(&random_functions)
        ),
        fix: "让生成函数真正从数据数组取值：先从词库数组随机取词，再按需求拼接、筛选、限长。数组内容必须原样使用，不要替换成随机字符或自己另编一套示例。"
            .to_string(),
    });
}

fn names_of(functions: &[(String, String)]) -> String {
    functions
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(" / ")
}

/// 找出"吃过数据"的函数集合：直接引用数据数组的，以及被它们调用到的。
///
/// 迭代到不动点，这样 `pick()` 被 `pickOne()` 包一层也认得出来。
fn data_fed_functions(data_names: &[String], functions: &[(String, String)]) -> Vec<String> {
    let mut fed: Vec<String> = functions
        .iter()
        .filter(|(_, body)| data_names.iter().any(|name| body.contains(name.as_str())))
        .map(|(name, _)| name.clone())
        .collect();

    for _ in 0..4 {
        let mut added = Vec::new();
        for (name, body) in functions {
            if !fed.contains(name) {
                continue;
            }
            for (callee, _) in functions {
                if callee != name && !fed.contains(callee) && body.contains(&format!("{callee}(")) {
                    added.push(callee.clone());
                }
            }
        }
        if added.is_empty() {
            break;
        }
        fed.extend(added);
    }
    fed
}

/// 同一个作用域里重复声明同一个标识符。
///
/// `let` / `const` 不允许在同一作用域重复声明，重复了就是 SyntaxError，**整段脚本一行都不
/// 执行** —— 页面等于死的。最典型的来源是自动续写：单轮输出被封顶后模型接着写，写完不会
/// 停手，转头把"状态管理 / 数据源"整段再写一遍，于是脚本里出现两份 `let currentScene`。
///
/// 判据必须卡在**同一个大括号作用域**上：`function a() { let name = 1; }` 和
/// `function b() { let name = 2; }` 里的 `name` 是两份合法局部变量，按"渲染深度"去数会
/// 把它们判成重复 —— 实测就是这么误报的，结果修复轮次一直在改本来正确的代码。
fn check_duplicate_declarations(script: &str, issues: &mut Vec<Issue>) {
    const MIN_PAIRS: usize = 3;
    const MIN_NAME: usize = 4;

    let mut counts: HashMap<(usize, String), usize> = HashMap::new();
    for (scope, name, _) in let_const_declarations(script) {
        if name.chars().count() < MIN_NAME {
            continue;
        }
        *counts.entry((scope, name)).or_insert(0) += 1;
    }

    let mut duplicated: Vec<String> = counts
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|((_, name), _)| name)
        .collect();
    if duplicated.len() < MIN_PAIRS {
        return;
    }
    duplicated.sort();
    duplicated.dedup();

    issues.push(Issue {
        code: "DUPLICATE_DECLARATION",
        severity: Severity::Critical,
        detail: format!(
            "脚本里 {} 个标识符在同一作用域被重复声明（{}）—— 这是续写时把同一段逻辑又写了一遍，浏览器会抛 SyntaxError，整段脚本不执行，页面所有交互失效。",
            duplicated.len(),
            duplicated.join(" / ")
        ),
        fix: "把重复的那一份整段删掉，只保留一条完整的实现：同一批状态变量、数据源和函数各声明一次，不要出现两份。"
            .to_string(),
    });
}

/// 扫出所有 `let` / `const` 声明及其所在的**作用域**。
///
/// 作用域用"最内层未闭合大括号的编号"表示，顶层为 0。同一个函数体里的两次声明算同作用域
/// （真重复），两个不同函数里的同名声明各有各的大括号编号（合法，不算）。
///
/// 扫描时跳过字符串、模板串与注释；`for (let i = …)` 这类写在括号里的声明每次循环都是
/// 独立作用域，不算重复。
fn let_const_declarations(script: &str) -> Vec<(usize, String, usize)> {
    let chars: Vec<char> = script.chars().collect();
    let mut found = Vec::new();
    let mut braces: Vec<usize> = Vec::new();
    let mut next_brace = 1usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];

        if line_comment {
            if ch == '\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if ch == '*' && chars.get(index + 1) == Some(&'/') {
                block_comment = false;
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        if let Some(current) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == current {
                quote = None;
            }
            index += 1;
            continue;
        }

        if ch == '/' && chars.get(index + 1) == Some(&'/') {
            line_comment = true;
            index += 2;
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            block_comment = true;
            index += 2;
            continue;
        }

        match ch {
            '"' | '\'' | '`' => {
                quote = Some(ch);
                index += 1;
                continue;
            }
            '{' => {
                braces.push(next_brace);
                next_brace += 1;
            }
            '}' => {
                braces.pop();
            }
            _ => {}
        }

        let previous = previous_significant(&chars, index);
        let inside_parens = previous == Some('(') || previous == Some('.');
        if !inside_parens {
            for keyword in ["let", "const"] {
                if let Some(rest) = keyword_at(&chars, index, keyword) {
                    let name: String = chars[rest..]
                        .iter()
                        .take_while(|c| c.is_ascii_alphanumeric() || **c == '_' || **c == '$')
                        .collect();
                    if !name.is_empty() {
                        found.push((braces.last().copied().unwrap_or(0), name, index));
                    }
                }
            }
        }
        index += 1;
    }
    found
}

/// 位置 `at` 处是不是完整的关键字（前面是边界，后面跟空白），是则返回名字起始下标。
fn keyword_at(chars: &[char], at: usize, keyword: &str) -> Option<usize> {
    let word: Vec<char> = keyword.chars().collect();
    if at + word.len() >= chars.len() || chars[at..at + word.len()] != word[..] {
        return None;
    }
    if !chars[at + word.len()].is_whitespace() {
        return None;
    }
    if let Some(previous) = chars.get(at.wrapping_sub(1)) {
        if at > 0 && (previous.is_ascii_alphanumeric() || *previous == '_' || *previous == '$') {
            return None;
        }
    }
    Some(at + word.len() + 1)
}

/// 往前找第一个非空白字符。
fn previous_significant(chars: &[char], at: usize) -> Option<char> {
    let mut index = at;
    while index > 0 {
        index -= 1;
        let ch = chars[index];
        if !ch.is_whitespace() {
            return Some(ch);
        }
    }
    None
}

/// 括号与引号是否配平。
///
/// 这条抓的是"被截断后拼回去"的残骸：单轮输出到上限时，累积内容往往停在半个 token 上
/// （实测停在 `function showToast(message`），续写再从这儿接上，就得到
/// `function showToast(message}` —— 括号不配平，浏览器抛 SyntaxError，整页交互全失效。
/// 标签闭合检查看不出这种问题，因为 HTML 结构本身是完整的。
fn check_balanced_delimiters(script: &str, issues: &mut Vec<Issue>) {
    let Some(problem) = scan_delimiters(script) else {
        return;
    };
    issues.push(Issue {
        code: "SCRIPT_UNBALANCED",
        severity: Severity::Critical,
        detail: format!("脚本括号或引号不配平：{problem} —— 浏览器会抛 SyntaxError，整段脚本不执行。"),
        fix: "把被截断的那段代码补完整：括号、引号都要成对。如果某处是从半截开始的，按完整语句重写一遍。"
            .to_string(),
    });
}

/// 扫描分隔符配平；返回问题描述。
pub fn delimiter_problem(script: &str) -> Option<String> {
    scan_delimiters(script)
}

/// 脚本被截断时的**确定性抢救**：退回到最后一个自洽版本。
///
/// 换模型不如换个做法：模型在被输出上限截断时，尾部半截代码会带个未闭合的 `(` 或 `{`，
/// 整段脚本于是抛 SyntaxError、一行都不执行。与其让用户拿到一个死页面，不如把残缺的尾巴
/// 丢掉 —— 前面完整的那部分逻辑仍然可用，页面至少是活的。
///
/// 实测里还有更隐蔽的一种残法：单轮被截断后，模型从断点接着写，写着写着**把整份脚本
/// 重开了一篇**。于是同一份脚本里出现两代实现 —— 前一代完整，后一代写到一半就断了。
/// 因为 JS 里后声明的同名函数会覆盖前面的，**残破的后一代反而胜出**，页面照样是死的，
/// 而且括号还是配平的（`node --check` 都过得去，因为是运行时 ReferenceError）。
/// 所以候选切点有两个：
///
/// 1. **模型重开一篇的位置**（最早的"同名函数第二次出现"之前）——优先，因为切回去就能
///    拿回一份完整自洽的脚本；
/// 2. 最后一个括号配平的行边界 ——兜底。
///
/// 择优不数"切得长不长"，而是把每个候选放回页面跑一遍功能自检，按 `health` 的**加权致命度**
/// 比大小 —— 这样"脚本压根不执行"（重复声明）、"点了没反应"（悬空调用）这些不同性质的伤
/// 才能摆在一起比较。健康度相同时才看谁留下的代码更多。
pub fn salvage_truncated_script(html: &str) -> Option<String> {
    restore_script(html).map(|(fixed, _)| fixed)
}

/// 同 `salvage_truncated_script`，但额外返回一句"为什么这么切"，用于日志。
pub fn restore_script(html: &str) -> Option<(String, String)> {
    let block = crate::webgen::dom::script_block(html)?;
    let body_start = block.find('>')? + 1;
    let body_end = block.rfind("</script")?;
    let body = &block[body_start..body_end];

    let current_script = crate::webgen::dom::merged_script(html);
    let current_broken = delimiter_problem(&current_script).is_some();

    // 候选一：**就地收口**。脚本被写坏（括号不匹配）时，切到损坏点之前的最后一个
    // 语句边界，再把未闭合的括号按逆序补上 —— 这样能保住宿主大部分代码。
    // 只在脚本确实坏了的时候才用它，否则会给健康脚本多补一堆括号。
    let mut candidates: Vec<(String, &'static str)> = Vec::new();
    if current_broken {
        if let Some(fixed) = close_truncated_tail(body) {
            candidates.push((fixed, "在脚本损坏处就地收口（截去损坏段并补齐括号）"));
        }
    }
    // 候选二：**退回"重开一篇"之前**。最早的那个重启点就是模型重新写整份脚本的位置，
    // 切回去等于把完整的那一代整份拿回。
    for cut in restart_cuts(body) {
        candidates.push((
            body.chars().take(cut).collect(),
            "退回模型重写整份脚本之前的完整版本",
        ));
    }
    // 候选三：兜底，退回最后一个括号配平的行边界。
    if let Some(cut) = last_safe_cut(body) {
        candidates.push((
            body.chars().take(cut).collect(),
            "退回到最后一个括号配平的位置",
        ));
    }

    let floor = body.chars().count() / 4;
    // 择优用**整体健康度**，不再只看"同名函数有几个"。
    // 实测教训：某一版回滚候选带着 `DUPLICATE_DECLARATION`（`let history` 声明两遍，
    // `node --check` 报 SyntaxError，整段脚本一行都不执行）—— 旧判据只数同名函数，
    // 看不出这版和另一版的天壤之别，于是挑回一个同样跑不起来的页面。
    let score = |page: &str| -> (usize, usize) { health(&verify_page(page, "")) };
    let current_key = score(html);

    let mut best: Option<(String, &'static str, (usize, usize), usize)> = None;
    for (script, reason) in candidates {
        if script.trim().is_empty() || script.chars().count() < floor {
            continue;
        }
        let rebuilt = format!("<script>{}</script>", script.trim_end());
        let candidate = crate::webgen::dom::splice_script(html, &rebuilt);
        let merged = crate::webgen::dom::merged_script(&candidate);
        // 切完（或补完）还得是配平的，否则这个候选没有意义。
        if delimiter_problem(&merged).is_some() {
            continue;
        }
        let key = score(&candidate);
        let length = script.chars().count();
        // 健康度相同时，代码留得多的那一版更好（信息没丢）。
        let better = match &best {
            None => true,
            Some((_, _, best_key, best_len)) => {
                key < *best_key || (key == *best_key && length > *best_len)
            }
        };
        if better {
            best = Some((candidate, reason, key, length));
        }
    }

    let (fixed, reason, key, _) = best?;
    // 只有候选**确实比现状更健康**才替换 —— 没变好就别动，避免把好页面改坏。
    if key >= current_key {
        return None;
    }
    Some((fixed, reason.to_string()))
}

/// 就地收口：脚本被写坏时，切到损坏点之前的最后一个语句边界，再补齐未闭合的括号。
///
/// 为什么不能只靠"退回栈空的行首"：现在的页面常把整个脚本包在
/// `document.addEventListener("DOMContentLoaded", () => { … })` 里，括号栈几乎从不落空，
/// 按栈空退位会退到全文 20% 处、丢掉大半功能。实测就是这么失败的 ——
/// 第 264 行的模板串被模型写坏（多了一个没闭合的 `${`），而栈空点在第 84 行，
/// 候选全被拒，最后交付了一个整段脚本不执行的死页面。
///
/// 收口的代价是最后那个残缺的函数只剩前半段，但页面其余部分照常工作。
fn close_truncated_tail(script: &str) -> Option<String> {
    let chars: Vec<char> = script.chars().collect();
    let scan = scan_for_recovery(&chars);
    let cut = scan.boundary?;
    if cut >= chars.len() {
        return None;
    }
    let prefix: String = chars[..cut].iter().collect();
    if prefix.trim().is_empty() {
        return None;
    }
    let mut out = prefix.trim_end().to_string();
    for open in scan.stack_at_boundary.iter().rev() {
        out.push('\n');
        out.push(match *open {
            '(' => ')',
            '[' => ']',
            _ => '}',
        });
    }
    out.push('\n');
    Some(out)
}

/// 损坏位置与"最后一个语句边界"的扫描结果。
struct RecoveryScan {
    /// 错误之前最后一个像语句结尾的行首位置。
    boundary: Option<usize>,
    /// 在那个边界处仍未闭合的分隔符（按打开顺序）。
    stack_at_boundary: Vec<char>,
}

/// 一路扫到第一个结构性错误，顺便记下"最后一个语句边界"。
///
/// "像语句结尾" = 行首，且这一行最后一个有意义的字符是 `;` / `{` / `}`。
/// 这样切出来不会停在半句表达式中间（`a = b +` 这种，补上 `}` 就成语法错误了）。
fn scan_for_recovery(chars: &[char]) -> RecoveryScan {
    let mut result = RecoveryScan {
        boundary: None,
        stack_at_boundary: Vec::new(),
    };
    let mut stack: Vec<char> = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut last_significant = ';';
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];

        if line_comment {
            if ch == '\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if ch == '*' && chars.get(index + 1) == Some(&'/') {
                block_comment = false;
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        if let Some(current) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == current {
                quote = None;
            } else if ch == '\n' && current != '`' {
                quote = None;
            }
            index += 1;
            continue;
        }

        if ch == '/' && chars.get(index + 1) == Some(&'/') {
            line_comment = true;
            index += 2;
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            block_comment = true;
            index += 2;
            continue;
        }
        if ch == '/' && regex_position(chars, index) {
            index += 1;
            let mut in_class = false;
            while index < chars.len() {
                let current = chars[index];
                if current == '\\' {
                    index += 2;
                    continue;
                }
                if current == '[' {
                    in_class = true;
                } else if current == ']' {
                    in_class = false;
                } else if current == '/' && !in_class {
                    index += 1;
                    break;
                } else if current == '\n' {
                    break;
                }
                index += 1;
            }
            last_significant = '/';
            continue;
        }

        match ch {
            '"' | '\'' | '`' => {
                quote = Some(ch);
                last_significant = ch;
            }
            '(' | '[' | '{' => {
                stack.push(ch);
                last_significant = ch;
            }
            ')' | ']' | '}' => {
                let expected = match ch {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if stack.last().copied() != Some(expected) {
                    break;
                }
                stack.pop();
                last_significant = ch;
            }
            '\n' => {
                if matches!(last_significant, ';' | '{' | '}') {
                    result.boundary = Some(index + 1);
                    result.stack_at_boundary = stack.clone();
                }
            }
            _ => {
                if !ch.is_whitespace() {
                    last_significant = ch;
                }
            }
        }
        index += 1;
    }

    result
}

/// 最后一个安全切点（字符下标）。
fn last_safe_cut(script: &str) -> Option<usize> {
    let chars: Vec<char> = script.chars().collect();
    let mut stack = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut last_safe: Option<usize> = None;
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];

        if line_comment {
            if ch == '\n' {
                line_comment = false;
                if stack == 0 && quote.is_none() {
                    last_safe = Some(index + 1);
                }
            }
            index += 1;
            continue;
        }
        if block_comment {
            if ch == '*' && chars.get(index + 1) == Some(&'/') {
                block_comment = false;
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        if let Some(current) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == current {
                quote = None;
            }
            index += 1;
            continue;
        }

        if ch == '/' && chars.get(index + 1) == Some(&'/') {
            line_comment = true;
            index += 2;
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            block_comment = true;
            index += 2;
            continue;
        }
        if ch == '/' && regex_position(&chars, index) {
            index += 1;
            let mut in_class = false;
            while index < chars.len() {
                let current = chars[index];
                if current == '\\' {
                    index += 2;
                    continue;
                }
                if current == '[' {
                    in_class = true;
                } else if current == ']' {
                    in_class = false;
                } else if current == '/' && !in_class {
                    index += 1;
                    break;
                } else if current == '\n' {
                    break;
                }
                index += 1;
            }
            continue;
        }

        match ch {
            '"' | '\'' | '`' => quote = Some(ch),
            '(' | '[' | '{' => stack += 1,
            ')' | ']' | '}' => stack = (stack - 1).max(0),
            '\n' if stack == 0 => last_safe = Some(index + 1),
            _ => {}
        }
        index += 1;
    }
    last_safe
}

/// 扫描分隔符配平；返回问题描述。
///
/// 字符串、模板串、注释、正则字面量都要跳过，否则 `replace(/[\"']/g, '')` 这种写法里
/// 的引号会被误当成字符串开始 —— 正则的识别靠"斜杠前面是不是运算符位置"来判断。
fn scan_delimiters(script: &str) -> Option<String> {
    let chars: Vec<char> = script.chars().collect();
    let mut stack: Vec<(char, usize)> = Vec::new();
    let mut quote: Option<(char, usize)> = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];

        if line_comment {
            if ch == '\n' {
                line_comment = false;
            }
            index += 1;
            continue;
        }
        if block_comment {
            if ch == '*' && chars.get(index + 1) == Some(&'/') {
                block_comment = false;
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        if let Some((current, opened_at)) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == current {
                quote = None;
            } else if ch == '\n' && current != '`' {
                return Some(format!(
                    "第 {} 行开始的 {} 字符串跨行了也没有闭合",
                    line_of(&chars, opened_at),
                    current
                ));
            }
            index += 1;
            continue;
        }

        if ch == '/' && chars.get(index + 1) == Some(&'/') {
            line_comment = true;
            index += 2;
            continue;
        }
        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            block_comment = true;
            index += 2;
            continue;
        }
        if ch == '/' && regex_position(&chars, index) {
            index += 1;
            let mut in_class = false;
            while index < chars.len() {
                let current = chars[index];
                if current == '\\' {
                    index += 2;
                    continue;
                }
                if current == '[' {
                    in_class = true;
                } else if current == ']' {
                    in_class = false;
                } else if current == '/' && !in_class {
                    index += 1;
                    break;
                } else if current == '\n' {
                    break;
                }
                index += 1;
            }
            continue;
        }

        match ch {
            '"' | '\'' | '`' => quote = Some((ch, index)),
            '(' | '[' | '{' => stack.push((ch, index)),
            ')' | ']' | '}' => {
                let expected = match ch {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                match stack.pop() {
                    Some((open, _)) if open == expected => {}
                    Some((open, at)) => {
                        return Some(format!(
                            "第 {} 行的 `{}` 对不上第 {} 行的 `{}`",
                            line_of(&chars, index),
                            ch,
                            line_of(&chars, at),
                            open
                        ))
                    }
                    None => {
                        return Some(format!(
                            "第 {} 行多了一个 `{}`，前面没有对应的开符号",
                            line_of(&chars, index),
                            ch
                        ))
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }

    if let Some((current, at)) = quote {
        return Some(format!(
            "第 {} 行开始的 {} 字符串一直没闭合",
            line_of(&chars, at),
            current
        ));
    }
    if let Some((open, at)) = stack.last().copied() {
        return Some(format!(
            "第 {} 行的 `{}` 没有闭合，脚本像是被截断在半途",
            line_of(&chars, at),
            open
        ));
    }
    None
}

/// 斜杠是否处在"正则字面量"的位置：前面是运算符或行首，而不是标识符、数字、`)`、`]`。
fn regex_position(chars: &[char], at: usize) -> bool {
    let mut index = at;
    while index > 0 {
        index -= 1;
        let ch = chars[index];
        if ch.is_whitespace() {
            continue;
        }
        return matches!(
            ch,
            '=' | '('
                | '['
                | '{'
                | ','
                | ':'
                | ';'
                | '!'
                | '&'
                | '|'
                | '?'
                | '+'
                | '-'
                | '*'
                | '%'
                | '<'
                | '>'
                | '~'
                | '^'
        );
    }
    true
}

/// 字符下标转行号（从 1 开始）。
fn line_of(chars: &[char], at: usize) -> usize {
    chars[..at.min(chars.len())]
        .iter()
        .filter(|ch| **ch == '\n')
        .count()
        + 1
}

// ============================================================
// 标识符级扫描：把"表面完整、其实是死页面"认出来
// ============================================================
//
// 8B 模型最刁的失败模式不是"页面没写完"，而是"页面写完了，但调用的函数根本不存在"：
//
// ```js
// function generateNames() { ... generateSingleName(...) ... }
// function generateSingleName(...) { ... generateGeneralName(...) }   // ← 从没定义过
// ```
//
// 括号是配平的、`node --check` 是过的（未定义标识符属于**运行时** ReferenceError，
// 不是语法错误），点「生成」却什么都不发生。所以必须做一次标识符级的检查。

/// 把字符串字面量、注释与正则字面量的**内容**替换成空格，保留代码骨架。
///
/// 掩码是所有标识符扫描的前提：否则 `"点击生成按钮后会调用 generateNames()"` 这种界面
/// 文案里的名字会被当成真实引用。模板串里 `${...}` 属于**代码**，要保留 —— 实测就有一处
/// `${pinyin(name)}` 藏在 CSV 导出的模板串里，整段掩掉的话这个悬空调用就漏掉了。
pub fn mask_non_code(script: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Code,
        Single,
        Double,
        Template,
        Line,
        Block,
    }

    let chars: Vec<char> = script.chars().collect();
    let mut out = chars.clone();
    let mut modes: Vec<Mode> = vec![Mode::Code];
    let mut braces: Vec<i32> = vec![0];
    let mut escaped = false;
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];
        match *modes.last().unwrap() {
            Mode::Line => {
                if ch == '\n' {
                    modes.pop();
                    braces.pop();
                } else {
                    out[index] = ' ';
                }
                index += 1;
            }
            Mode::Block => {
                if ch == '*' && chars.get(index + 1) == Some(&'/') {
                    out[index] = ' ';
                    out[index + 1] = ' ';
                    modes.pop();
                    braces.pop();
                    index += 2;
                } else {
                    out[index] = ' ';
                    index += 1;
                }
            }
            Mode::Single | Mode::Double => {
                let closing = if *modes.last().unwrap() == Mode::Single {
                    '\''
                } else {
                    '"'
                };
                out[index] = ' ';
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == closing || ch == '\n' {
                    // 单双引号跨行说明本来就残缺，别把后面整段都吞掉。
                    modes.pop();
                    braces.pop();
                }
                index += 1;
            }
            Mode::Template => {
                if escaped {
                    escaped = false;
                    out[index] = ' ';
                } else if ch == '\\' {
                    escaped = true;
                    out[index] = ' ';
                } else if ch == '`' {
                    out[index] = ' ';
                    modes.pop();
                    braces.pop();
                } else if ch == '$' && chars.get(index + 1) == Some(&'{') {
                    // 进入插值：里面是代码，保留。
                    out[index] = ' ';
                    out[index + 1] = ' ';
                    modes.push(Mode::Code);
                    braces.push(0);
                    index += 2;
                    continue;
                } else {
                    out[index] = ' ';
                }
                index += 1;
            }
            Mode::Code => {
                if ch == '/' && chars.get(index + 1) == Some(&'/') {
                    out[index] = ' ';
                    out[index + 1] = ' ';
                    modes.push(Mode::Line);
                    braces.push(0);
                    index += 2;
                    continue;
                }
                if ch == '/' && chars.get(index + 1) == Some(&'*') {
                    out[index] = ' ';
                    out[index + 1] = ' ';
                    modes.push(Mode::Block);
                    braces.push(0);
                    index += 2;
                    continue;
                }
                if ch == '/' && regex_position(&chars, index) {
                    out[index] = ' ';
                    index += 1;
                    let mut in_class = false;
                    let mut escaped_regex = false;
                    while index < chars.len() {
                        let current = chars[index];
                        out[index] = ' ';
                        if escaped_regex {
                            escaped_regex = false;
                        } else if current == '\\' {
                            escaped_regex = true;
                        } else if current == '[' {
                            in_class = true;
                        } else if current == ']' {
                            in_class = false;
                        } else if current == '/' && !in_class {
                            index += 1;
                            break;
                        } else if current == '\n' {
                            break;
                        }
                        index += 1;
                    }
                    continue;
                }
                match ch {
                    '\'' => {
                        out[index] = ' ';
                        modes.push(Mode::Single);
                        braces.push(0);
                    }
                    '"' => {
                        out[index] = ' ';
                        modes.push(Mode::Double);
                        braces.push(0);
                    }
                    '`' => {
                        out[index] = ' ';
                        modes.push(Mode::Template);
                        braces.push(0);
                    }
                    '{' => {
                        *braces.last_mut().unwrap() += 1;
                    }
                    '}' => {
                        if *braces.last().unwrap() > 0 {
                            *braces.last_mut().unwrap() -= 1;
                        } else if modes.len() > 1 {
                            // 关闭 `${`，回到模板文本。
                            modes.pop();
                            braces.pop();
                            out[index] = ' ';
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
        }
    }

    out.into_iter().collect()
}

/// JS 保留字：不能当标识符用，也就不该出现在"被调用名字"里。
const RESERVED: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "catch",
    "function",
    "return",
    "typeof",
    "new",
    "class",
    "const",
    "let",
    "var",
    "do",
    "else",
    "try",
    "finally",
    "delete",
    "void",
    "in",
    "of",
    "instanceof",
    "case",
    "default",
    "break",
    "continue",
    "throw",
    "yield",
    "await",
    "async",
    "static",
    "get",
    "set",
    "extends",
    "super",
    "this",
    "null",
    "true",
    "false",
    "undefined",
    "NaN",
    "Infinity",
    "debugger",
    "with",
    "export",
    "import",
    "from",
    "as",
    "arguments",
];

/// 浏览器/语言内置的全局对象与方法。出现这些名字是正常的，不算未定义。
const GLOBALS: &[&str] = &[
    "window",
    "document",
    "console",
    "Math",
    "JSON",
    "Date",
    "Array",
    "Object",
    "String",
    "Number",
    "Boolean",
    "RegExp",
    "Error",
    "TypeError",
    "RangeError",
    "SyntaxError",
    "Promise",
    "Set",
    "Map",
    "WeakMap",
    "WeakSet",
    "Symbol",
    "BigInt",
    "Intl",
    "Proxy",
    "Reflect",
    "globalThis",
    "event",
    "localStorage",
    "sessionStorage",
    "navigator",
    "location",
    "history",
    "screen",
    "performance",
    "crypto",
    "caches",
    "Blob",
    "File",
    "FileReader",
    "FileList",
    "URL",
    "URLSearchParams",
    "FormData",
    "Headers",
    "Request",
    "Response",
    "XMLHttpRequest",
    "fetch",
    "WebSocket",
    "Worker",
    "Event",
    "CustomEvent",
    "MouseEvent",
    "KeyboardEvent",
    "TouchEvent",
    "Node",
    "Element",
    "HTMLElement",
    "Image",
    "Audio",
    "Video",
    "MutationObserver",
    "IntersectionObserver",
    "ResizeObserver",
    "TextEncoder",
    "TextDecoder",
    "setTimeout",
    "clearTimeout",
    "setInterval",
    "clearInterval",
    "requestAnimationFrame",
    "cancelAnimationFrame",
    "queueMicrotask",
    "structuredClone",
    "alert",
    "confirm",
    "prompt",
    "print",
    "atob",
    "btoa",
    "encodeURIComponent",
    "decodeURIComponent",
    "encodeURI",
    "decodeURI",
    "escape",
    "unescape",
    "isNaN",
    "isFinite",
    "parseFloat",
    "parseInt",
    // 剪贴板 / 文件 / 媒体 / 存储等浏览器内置构造器。
    //
    // 实测漏掉 `ClipboardItem` 的代价很大：产物里 `new ClipboardItem({...})` 被当成"调用了
    // 从未定义的函数"，于是整段重写脚本、又整段重写一次，两轮共 160 秒全花在一个并不存在的
    // 缺陷上（真正的缺陷被挤到后面才轮到）。这类"全局构造器"必须认全。
    "ClipboardItem",
    "Clipboard",
    "ImageData",
    "ImageBitmap",
    "OffscreenCanvas",
    "AbortController",
    "AbortSignal",
    "DOMException",
    "DOMParser",
    "XMLSerializer",
    "MediaRecorder",
    "MediaStream",
    "AudioContext",
    "SpeechSynthesisUtterance",
    "Notification",
    "ServiceWorker",
    "BroadcastChannel",
    "MessageChannel",
    "SharedWorker",
    "FontFace",
    "CSS",
    "CSSStyleSheet",
    "CustomElementRegistry",
    "Range",
    "Selection",
    "TreeWalker",
    "NodeIterator",
    "DocumentFragment",
    "ShadowRoot",
    "HTMLCanvasElement",
    "HTMLImageElement",
    "HTMLInputElement",
    "IDBKeyRange",
    "indexedDB",
    "openDatabase",
    "Audio",
    "Path2D",
    "DOMMatrix",
    "TextMetrics",
    "PointerEvent",
    "WheelEvent",
    "DragEvent",
    "InputEvent",
    "FocusEvent",
    "AnimationEvent",
    "TransitionEvent",
    "StorageEvent",
    "MessageEvent",
    "ErrorEvent",
    "ProgressEvent",
    "CloseEvent",
    "WebGLRenderingContext",
    "AudioBuffer",
    "ImageCapture",
    "EyeDropper",
    "CompressionStream",
    "DecompressionStream",
    "ReadableStream",
    "WritableStream",
    "TransformStream",
    "crypto",
    "subtle",
    "queueMicrotask",
    "reportError",
    "matchMedia",
    "getComputedStyle",
    "requestIdleCallback",
    "cancelIdleCallback",
    "scrollTo",
    "scrollBy",
];

fn is_reserved(name: &str) -> bool {
    RESERVED.contains(&name)
}

fn is_global(name: &str) -> bool {
    GLOBALS.contains(&name)
}

/// 极简分词：标识符 / 单字符符号 / 数字。掩码之后不需要再考虑字符串。
fn tokens_of(code: &str) -> Vec<(String, bool)> {
    tokens_with_positions(code)
        .into_iter()
        .map(|(text, is_ident, _)| (text, is_ident))
        .collect()
}

/// 同 `tokens_of`，额外带回 token 在掩码串里的字符下标。
///
/// 下标按**字符**计（不是字节），这样才能和 `chars().take(cut)` 的切点对上。
fn tokens_with_positions(code: &str) -> Vec<(String, bool, usize)> {
    let mut out: Vec<(String, bool, usize)> = Vec::new();
    let mut current = String::new();
    let mut current_at = 0usize;
    let mut position = 0usize;

    for ch in code.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            if current.is_empty() {
                current_at = position;
            }
            current.push(ch);
        } else {
            if !current.is_empty() {
                out.push((std::mem::take(&mut current), true, current_at));
            }
            if !ch.is_whitespace() {
                out.push((ch.to_string(), false, position));
            }
        }
        position += 1;
    }
    if !current.is_empty() {
        out.push((current, true, current_at));
    }
    out
}

/// 从 `open`（`(` 或 `{` 或 `[`）出发找到配对的闭符号下标。
fn matching(tokens: &[(String, bool)], open: usize) -> Option<usize> {
    let opener = tokens.get(open)?.0.clone();
    let closer = match opener.as_str() {
        "(" => ")",
        "{" => "}",
        "[" => "]",
        _ => return None,
    };
    let mut depth = 0i32;
    for (index, (text, is_ident)) in tokens.iter().enumerate().skip(open) {
        if *is_ident {
            continue;
        }
        if *text == opener {
            depth += 1;
        } else if *text == closer {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

/// 收集脚本里**定义过的所有名字**：函数声明、`const/let/var`（含解构）、`class`、形参。
fn defined_names(code: &str) -> Vec<String> {
    let tokens = tokens_of(code);
    let mut names: Vec<String> = Vec::new();
    let push = |names: &mut Vec<String>, name: &str| {
        if !name.is_empty() && !is_reserved(name) && !names.iter().any(|existing| existing == name)
        {
            names.push(name.to_string());
        }
    };

    for index in 0..tokens.len() {
        let (text, is_ident) = &tokens[index];
        if !*is_ident {
            continue;
        }
        match text.as_str() {
            "function" => {
                // `function foo(` 或匿名 `function(`。注意 `collect_params` 要的是**左括号**
                // 的下标 —— 传成名字或 `function` 自己的下标时它会直接返回 `None`，
                // 形参就一个都收不到（下面几处原先都是这样，于是 `resolve` / `item` 这类
                // 回调形参全被误判成"调用了从未定义的函数"）。
                match tokens.get(index + 1) {
                    Some((name, true)) => {
                        push(&mut names, name);
                        collect_params(&tokens, index + 2, &mut names);
                    }
                    _ => {
                        collect_params(&tokens, index + 1, &mut names);
                    }
                }
            }
            "class" => {
                if let Some((name, name_is_ident)) = tokens.get(index + 1) {
                    if *name_is_ident {
                        push(&mut names, name);
                    }
                }
            }
            "const" | "let" | "var" => match tokens.get(index + 1) {
                // 普通声明：`const foo = …`
                Some((name, true)) => push(&mut names, name),
                // 解构声明：`const { a, b } = …` / `const [a, b] = …`
                Some((open, false)) if open == "{" || open == "[" => {
                    if let Some(close) = matching(&tokens, index + 1) {
                        for (inner, inner_is_ident) in &tokens[index + 2..close] {
                            if *inner_is_ident {
                                push(&mut names, inner);
                            }
                        }
                    }
                }
                _ => {}
            },
            "catch" => {
                // `catch (e) { … }` —— 同样要传左括号的下标。
                collect_params(&tokens, index + 1, &mut names);
            }
            _ => {
                // 箭头函数形参：`(a, b) => …` 与 `a => …`。
                if tokens.get(index + 1).map(|(t, _)| t.as_str()) == Some("=>") {
                    push(&mut names, text);
                }
                // 方法简写：`name(a, b) { … }`。注意必须**后面跟大括号**才算定义，
                // 否则 `name(a, b)` 只是普通调用，括号里的是实参不是形参。
                if tokens.get(index + 1).map(|(t, _)| t.as_str()) == Some("(") {
                    if let Some(close) = matching(&tokens, index + 1) {
                        if tokens.get(close + 1).map(|(t, _)| t.as_str()) == Some("{") {
                            push(&mut names, text);
                            collect_params(&tokens, index + 1, &mut names);
                        }
                    }
                }
            }
        }
    }

    names
}

/// 把 `open` 处 `(...)` 里的名字收成形参并返回右括号下标；`open` 不是左括号则返回 `None`。
fn collect_params(
    tokens: &[(String, bool)],
    open: usize,
    names: &mut Vec<String>,
) -> Option<usize> {
    let (text, is_ident) = tokens.get(open)?;
    if *is_ident || text != "(" {
        return None;
    }
    let close = matching(tokens, open)?;
    for (inner, inner_is_ident) in &tokens[open + 1..close] {
        if *inner_is_ident && !is_reserved(inner) && !names.iter().any(|existing| existing == inner)
        {
            names.push(inner.clone());
        }
    }
    Some(close)
}

/// 收集脚本里**被调用的名字**及其字符下标：`name(` 且前面不是 `.`（那是方法调用）。
fn called_identifiers(code: &str) -> Vec<(String, usize)> {
    let chars: Vec<char> = code.chars().collect();
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];
        if !(ch.is_ascii_alphabetic() || ch == '_' || ch == '$') {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len()
            && (chars[index].is_ascii_alphanumeric() || chars[index] == '_' || chars[index] == '$')
        {
            index += 1;
        }
        let name: String = chars[start..index].iter().collect();

        let mut probe = index;
        while probe < chars.len() && chars[probe].is_whitespace() {
            probe += 1;
        }
        if probe >= chars.len() || chars[probe] != '(' {
            continue;
        }
        // 前面是不是 `.`（方法调用）或者 `function` / `class`（定义）？
        let mut back = start;
        while back > 0 && chars[back - 1].is_whitespace() {
            back -= 1;
        }
        if back > 0 && chars[back - 1] == '.' {
            continue;
        }
        let mut word_end = back;
        while word_end > 0
            && (chars[word_end - 1].is_ascii_alphanumeric()
                || chars[word_end - 1] == '_'
                || chars[word_end - 1] == '$')
        {
            word_end -= 1;
        }
        let previous: String = chars[word_end..back].iter().collect();
        if previous == "function" || previous == "class" || is_reserved(&name) {
            continue;
        }
        out.push((name, start));
    }

    out
}

/// 被调用却从未定义的名字（去掉内置全局、保留字与一两个字母的简写）。
fn undefined_calls(script: &str) -> Vec<String> {
    let code = mask_non_code(script);
    let defined = defined_names(&code);
    let mut missing: Vec<String> = Vec::new();
    for (name, _) in called_identifiers(&code) {
        if is_global(&name) || defined.iter().any(|d| *d == name) {
            continue;
        }
        // 单双字母几乎都是循环变量、事件参数之类的简写，误报风险高，放过。
        if name.chars().count() < 3 {
            continue;
        }
        if !missing.contains(&name) {
            missing.push(name);
        }
    }
    missing
}

/// 供修复流程使用：列出页面里被调用却从未定义的函数名（已排序）。
pub fn missing_function_names(html: &str) -> Vec<String> {
    let mut missing = undefined_calls(&crate::webgen::dom::merged_script(html));
    missing.sort();
    missing
}

/// 页面脚本里已经定义过的函数名。
///
/// "只补缺失函数"路线要用它做一道闸：模型若顺手把已有函数重写一遍，追加进去反而会覆盖
/// 掉原本正确的实现，比不修还糟。
pub fn defined_function_names(html: &str) -> Vec<String> {
    let code = mask_non_code(&crate::webgen::dom::merged_script(html));
    let tokens = tokens_of(&code);
    let mut names: Vec<String> = Vec::new();
    for (index, (text, is_ident)) in tokens.iter().enumerate() {
        if !*is_ident || text != "function" {
            continue;
        }
        if let Some((name, name_is_ident)) = tokens.get(index + 1) {
            if *name_is_ident && !is_reserved(name) && !names.iter().any(|item| item == name) {
                names.push(name.clone());
            }
        }
    }
    names
}

/// 这段待追加的定义里，有没有会覆盖掉页面已有实现的函数名。
pub fn appendix_conflicts(html: &str, appendix: &str) -> Vec<String> {
    let existing = defined_function_names(html);
    let code = mask_non_code(appendix);
    let tokens = tokens_of(&code);
    let mut conflicts: Vec<String> = Vec::new();
    for (index, (text, is_ident)) in tokens.iter().enumerate() {
        if !*is_ident || text != "function" {
            continue;
        }
        if let Some((name, name_is_ident)) = tokens.get(index + 1) {
            if *name_is_ident
                && existing.iter().any(|item| item == name)
                && !conflicts.contains(name)
            {
                conflicts.push(name.clone());
            }
        }
    }
    conflicts
}

/// "就地补写"这条修复路线适不适用，以及要不要顺带补事件绑定。
///
/// 只有这两类问题可以就地补：
/// - `UNDEFINED_REFERENCE`：缺几个函数 —— 写上定义追加到脚本末尾就行（函数声明会提升）；
/// - `BUTTON_NO_HANDLER`：按钮没有绑事件 —— 追加几行绑定语句就行。
///
/// 别的一律返回 `None` 走整段重写：脚本被截断、重复声明、假数据桩这些补几个函数补不回来。
///
/// 之所以把 `BUTTON_NO_HANDLER` 也收进来：它本身只是提示级，但"页面画出来了、按钮点了没反应"
/// 正是要治的那种"表面工作"。实测抢救把损坏段连同尾部的绑定代码一起截掉之后就出现了这个状态，
/// 只补函数而不补绑定，页面照样是死的。
///
/// 实测第二个坑（端到端第 4 轮）：原先"只要有一条别的严重问题就 `return None`"。那次同时报了
/// `UNDEFINED_REFERENCE`（缺 7 个函数）和 `UNUSED_DATA_POOL`（三个数组没人读），第二条把这条
/// 唯一可行的路线掐掉了；接着 `function_patch_plan` 因为存在悬空调用也返回 `None`；最后整段
/// 重写又装不下（需 3745 token、上限 3502）→ **直接交付了一个点了没反应的坏页面**。
/// 实际上这两个问题是一回事：缺的那几个函数（`randomPick` / `getDailyQuestion` / `saveToHistory`…）
/// 正是应该去读那些数组的人。补写它们，两个问题一起消失。
///
/// 实测第三个坑（端到端第 7 轮）：换成白名单之后，还是被一条无关的 `PLACEHOLDER_ALERT`
/// （脚本里一处 alert）掐掉了 —— 而那次缺的 `qrcode` / `randomPick` 补上去页面就能动，
/// 唯一结果是白白多跑一轮整段重写、重写又被截断拒收，最后交付的仍是点不动的版本。
///
/// 结论：判据要反过来 —— 用**黑名单**。只有"往一个结构已坏的脚本上追加函数"才没有意义，
/// 那几类必须整段重写；其余任何问题都不该阻止这一轮先把它能修的修掉。
/// 至于"这轮修得值不值"交给 `health_with_html` 打分 + 回滚兜底，不在这里拦。
pub fn function_appendix_plan(issues: &[Issue]) -> Option<bool> {
    let mut need_functions = false;
    // 只看 Critical 会漏掉"按钮没绑事件"这类被标成 Major 的功能缺陷 —— 而那恰恰是
    // "页面看着齐全、点下去没反应"最常见的形态。这里改为按**性质**筛：
    // 凡是"功能没真正实现"的问题都参与判定。
    for issue in issues
        .iter()
        .filter(|issue| issue.severity == Severity::Critical || is_functional_defect(issue))
    {
        if issue.code == "UNDEFINED_REFERENCE" {
            need_functions = true;
            continue;
        }
        if appendix_is_unsafe(issue.code) {
            return None;
        }
    }
    let need_bindings = issues.iter().any(|issue| issue.code == "BUTTON_NO_HANDLER");
    // `DEAD_FUNCTION` 同样靠"补绑定"解决：函数已经写好了，只是没人调它。
    let need_bindings = need_bindings
        || issues
            .iter()
            .any(|issue| issue.code == "DEAD_FUNCTION" && issue.severity == Severity::Critical);
    if need_functions || need_bindings {
        Some(need_bindings)
    } else {
        None
    }
}

/// 往这个脚本上追加函数是否毫无意义 —— 脚本结构本身已经坏了，补多少函数都会被一起带走。
fn appendix_is_unsafe(code: &str) -> bool {
    matches!(
        code,
        // 脚本本身不成立：括号/引号不配平、混进围栏、模板字符串没闭合、正则属性名非法、
        // 根本没有脚本。
        "NO_SCRIPT"
            | "SCRIPT_UNBALANCED"
            | "SCRIPT_FENCE"
            | "UNCLOSED_TEMPLATE"
            | "REGEX_PROPERTY_INVALID"
            // 同一作用域里重复声明：追加同名定义只会让冲突更严重。
            | "DUPLICATE_FUNCTION"
            | "DUPLICATE_DECLARATION"
    )
}

/// 页面适不适合走"就地补写"的修复路线（保留给旧调用点）。
pub fn needs_function_appendix(issues: &[Issue]) -> bool {
    function_appendix_plan(issues).is_some()
}

/// 每个缺失函数在页面脚本里的调用现场（最多两处），给修复提示词当"签名依据"。
///
/// 模型看不到整份脚本，光给函数名它只能猜参数与返回值的形状；把调用那一行摆出来，
/// 它写出来的实现才对得上调用方。
pub fn missing_function_call_sites(html: &str, missing: &[String], budget: usize) -> String {
    let script = crate::webgen::dom::merged_script(html);
    let lines: Vec<&str> = script.lines().collect();
    let mut out = String::new();
    for name in missing {
        let needle = format!("{name}(");
        let mut shown = 0;
        for line in &lines {
            let trimmed = line.trim();
            if !trimmed.contains(&needle) || trimmed.starts_with("function") {
                continue;
            }
            let snippet: String = if trimmed.chars().count() > 240 {
                trimmed.chars().take(240).collect()
            } else {
                trimmed.to_string()
            };
            out.push_str(&format!("- {name} 在这里被调用：{snippet}\n"));
            shown += 1;
            if shown >= 2 {
                break;
            }
        }
        if out.chars().count() >= budget {
            break;
        }
    }
    if out.chars().count() > budget {
        out = out.chars().take(budget).collect();
    }
    out
}

/// 调用了从未定义的函数 —— 点下去就抛 ReferenceError。
///
/// 这是"页面看着完整、其实点了没反应"最直接的指纹，也是 `node --check` 抓不到的一类问题。
fn check_undefined_references(script: &str, issues: &mut Vec<Issue>) {
    let mut missing = undefined_calls(script);
    if missing.is_empty() {
        return;
    }
    missing.sort();
    let total = missing.len();
    missing.truncate(8);
    issues.push(Issue {
        code: "UNDEFINED_REFERENCE",
        severity: Severity::Critical,
        detail: format!(
            "脚本调用了 {} 个从头到尾没有定义的函数：{}。括号是配平的、语法检查也过得去，所以看起来没问题，但点下去会抛 ReferenceError —— 页面所有交互都没有反应。",
            total,
            missing.join(" / ")
        ),
        fix: "为这些函数补上完整实现（或改成调用已经定义好的那些函数），并确认每个被调用的函数在本文件的 <script> 里都有定义。补实现时要从词库数组真正取值，不要留空函数体、也不要只写一行 return。"
            .to_string(),
    });
}

/// 顶层函数声明的（作用域编号，名字，字符下标）。
///
/// 作用域沿用 `let_const_declarations` 的做法：用"最内层未闭合大括号的编号"表示，顶层为 0。
/// 同一个大括号里的同名函数才算真重复 —— 两个不同函数体内各有一个局部 `helper` 是合法的。
fn function_declarations_in_scopes(script: &str) -> Vec<(usize, String, usize)> {
    let code = mask_non_code(script);
    let tokens = tokens_with_positions(&code);

    let mut scopes: Vec<usize> = Vec::new();
    let mut next_scope = 1usize;
    let mut found = Vec::new();

    for (index, (text, is_ident, position)) in tokens.iter().enumerate() {
        if !*is_ident {
            match text.as_str() {
                "{" => {
                    scopes.push(next_scope);
                    next_scope += 1;
                }
                "}" => {
                    scopes.pop();
                }
                _ => {}
            }
            continue;
        }
        if text != "function" {
            continue;
        }
        let Some((name, name_is_ident, _)) = tokens.get(index + 1) else {
            continue;
        };
        if !*name_is_ident || is_reserved(name) {
            continue;
        }
        let scope = scopes.last().copied().unwrap_or(0);
        found.push((scope, name.clone(), *position));
    }

    found
}

/// 同一个作用域里被声明了不止一次的函数名。
fn duplicated_function_names(script: &str) -> Vec<String> {
    let declarations = function_declarations_in_scopes(script);
    let mut duplicated: Vec<String> = Vec::new();
    for (index, (scope, name, _)) in declarations.iter().enumerate() {
        let repeated = declarations[..index]
            .iter()
            .any(|(other_scope, other_name, _)| other_scope == scope && other_name == name);
        if repeated && !duplicated.contains(name) {
            duplicated.push(name.clone());
        }
    }
    duplicated.sort();
    duplicated
}

/// 同一个作用域里同名函数声明了两次。
///
/// 本身不报错（后一份覆盖前一份），但它几乎总是"模型写着写着把整份脚本重开了一篇"的痕迹，
/// 而后一份往往是写到一半就断的残品 —— 于是**残品覆盖了完好的那一份**，页面就死了。
fn check_duplicate_functions(script: &str, issues: &mut Vec<Issue>) {
    let duplicated = duplicated_function_names(script);
    if duplicated.is_empty() {
        return;
    }
    issues.push(Issue {
        code: "DUPLICATE_FUNCTION",
        severity: Severity::Critical,
        detail: format!(
            "同一作用域里的函数被声明了两遍：{}。这是模型在续写时把整份脚本又重开了一篇，而且后写的那一份常常是断的 —— JS 里后面的同名函数会覆盖前面的，于是完好的那一份被残品顶掉，页面等于死的。",
            duplicated.join(" / ")
        ),
        fix: "整份脚本只保留一套实现：每个函数只定义一次，把它写成完整的（有真实逻辑、从词库数组取值）。凡是重复出现的那一段，只留最完整的一份。"
            .to_string(),
    });
}

/// 模型"重开一篇"的切点（升序）：每一次"同名函数 / 同名状态变量第二次出现"的字符下标。
///
/// 切在**最靠前**的那一处，就能把完整的上一代实现整份拿回来。
///
/// 为什么变量也要算：新的一代总是先重新声明状态（`let history = []`、`const currentDilemma = …`），
/// 然后才写函数。只按函数找切点，切出来的前缀里会**残留新一代重新声明的那几行**，
/// 于是候选版本照样带着 `DUPLICATE_DECLARATION`（`node --check` 直接 SyntaxError），
/// 择优挑来挑去还是挑回一个跑不起来的页面 —— 实测就是这么发生的。
fn restart_cuts(script: &str) -> Vec<usize> {
    let mut cuts: Vec<usize> = Vec::new();

    let declarations = function_declarations_in_scopes(script);
    for (index, (scope, name, position)) in declarations.iter().enumerate() {
        let repeated = declarations[..index]
            .iter()
            .any(|(other_scope, other_name, _)| other_scope == scope && other_name == name);
        if repeated {
            cuts.push(*position);
        }
    }

    let variables = let_const_declarations(script);
    for (index, (scope, name, position)) in variables.iter().enumerate() {
        let repeated = variables[..index]
            .iter()
            .any(|(other_scope, other_name, _)| other_scope == scope && other_name == name);
        if repeated {
            cuts.push(*position);
        }
    }

    cuts.sort_unstable();
    cuts.dedup();
    cuts
}

/// 自认是桩的痕迹。
fn check_placeholder(script: &str, issues: &mut Vec<Issue>) {
    let lower = script.to_ascii_lowercase();
    let markers = [
        "模拟数据",
        "示例数据",
        "假数据",
        "待实现",
        "暂未实现",
        "此处应有",
    ];
    let mut hit: Vec<&str> = markers
        .iter()
        .copied()
        .filter(|marker| script.contains(marker))
        .collect();
    if lower.contains("//todo") || lower.contains("// todo") || lower.contains("/* todo") {
        hit.push("TODO 注释");
    }
    if hit.is_empty() {
        return;
    }
    issues.push(Issue {
        code: "PLACEHOLDER_DATA",
        severity: Severity::Critical,
        detail: format!(
            "源码里留着 {} 之类的痕迹，说明这部分没有真正实现。",
            hit.join(" / ")
        ),
        fix: "删掉占位痕迹并给出真实实现：数据从需求里取，逻辑按需求里的可验证行为写，不要留模拟数据。"
            .to_string(),
    });
}

/// 用 `alert` / `confirm` / `prompt` 代替界面。
///
/// 实测故障：需求要求"点击选项后揭示后果"，模型写成
/// `alert(\`你选择了：\n\n${optionText}\n\n后果是：\n\n随机生成\`)` —— 弹窗里那句
/// "随机生成"是写死的，真正的后果文案压根没算。这类写法把交互塞进浏览器原生弹窗，
/// 样式、状态、历史全部接不上，需求里要求的页面内提示与结果卡片根本不会出现。
fn check_alerts(script: &str, issues: &mut Vec<Issue>) {
    let code = mask_non_code(script);
    let tokens = tokens_of(&code);
    let mut count = 0usize;
    for (index, (text, is_ident)) in tokens.iter().enumerate() {
        if !*is_ident || (text != "alert" && text != "confirm" && text != "prompt") {
            continue;
        }
        // `foo.alert(...)` 之类的属性调用放过；`window.alert(...)` 仍算。
        if index > 1 && tokens[index - 1].0 == "." && tokens[index - 2].0 != "window" {
            continue;
        }
        if tokens.get(index + 1).map(|item| item.0.as_str()) == Some("(") {
            count += 1;
        }
    }
    if count == 0 {
        return;
    }
    issues.push(Issue {
        code: "PLACEHOLDER_ALERT",
        severity: Severity::Critical,
        detail: format!(
            "脚本里用了 {count} 处 alert / confirm / prompt 把结果弹出来 —— 这等于没有界面：弹窗不可样式化、与页面状态脱节，需求要求的页面内提示条或结果卡片根本不会出现。"
        ),
        fix: "改成页面内渲染：准备一个固定的提示元素（如 <div id=\"toast\"></div>），用 textContent 写入提示文字并切换 class 控制显隐；结果内容渲染进结果容器。不要用浏览器原生弹窗承载任何结果。"
            .to_string(),
    });
}

/// 对 `document.body` 做 `replaceChild` —— 运行时必抛 NotFoundError。
///
/// 实测故障：生成页里写
/// `document.body.replaceChild(optionsContainer, document.getElementById("dilemmaInput"))`。
/// `replaceChild(新节点, 旧节点)` 要求旧节点是 **body 的直接子节点**，而那个输入框嵌在
/// `<header>` 里 —— 这一句当场抛 `NotFoundError`，它后面的代码全部不执行。用户的实际观感是
/// "输入了内容、点了生成，页面什么反应都没有"（实测连点三次都毫无变化）。
///
/// 这类写法静态看毫不起眼（括号配平、语法通过），但它是页面主交互直接失效的根因。
fn check_fragile_dom_swap(script: &str, issues: &mut Vec<Issue>) {
    let lower = script.to_ascii_lowercase();
    let hits = count_of(&lower, "body.replacechild(");
    if hits == 0 {
        return;
    }
    issues.push(Issue {
        code: "FRAGILE_DOM_SWAP",
        severity: Severity::Critical,
        detail: format!(
            "脚本里有 {hits} 处 `body.replaceChild(...)`：只要被替换的元素不是 <body> 的直接子节点（例如它嵌在 <header> 里），这一句会当场抛 NotFoundError，后面的代码全都不执行 —— 用户看到的就是「点了没反应」。"
        ),
        fix: "不要对 body 调用 replaceChild。改成渲染进固定容器：先取容器（如 <main id=\"app\">），再 `container.innerHTML = ...`；确需替换元素时写成 `target.parentNode.replaceChild(node, target)`。"
            .to_string(),
    });
}

/// `<script>` 正文里混进了 Markdown 代码围栏。
///
/// 实测故障（端到端第 4 轮）：脚本第 59 行是 ```` ```}); ````。上一轮生成到半句被
/// `num_predict` 截断，续写的下一轮从"开一个代码块"写起，围栏就缝进了脚本中间。
/// 反引号在 JS 里开启模板字符串，于是浏览器报 `SyntaxError: Unexpected template string`，
/// **整段脚本一行都不执行** —— 所有按钮集体失灵，而括号配平检查报的是"正常"。
fn check_script_fences(script: &str, issues: &mut Vec<Issue>) {
    let hits = script
        .lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count();
    if hits == 0 {
        return;
    }
    issues.push(Issue {
        code: "SCRIPT_FENCE",
        severity: Severity::Critical,
        detail: format!(
            "脚本里混进了 {hits} 行 Markdown 代码围栏（```），反引号在 JS 里是模板字符串的开始 —— 浏览器会抛 SyntaxError，整段脚本一行都不执行，页面所有按钮全部失灵。括号配平检查看不出这个问题。"
        ),
        fix: "删掉脚本里所有行首的 ``` 与 ```html / ```js 标记（围栏后面若粘着代码要保留代码）。围栏只应出现在对话里，不属于网页源码。"
            .to_string(),
    });
}

/// 脚本里的模板字符串没有闭合（反引号个数是奇数）。
///
/// 正常脚本里每个模板字符串贡献 2 个反引号，总数必为偶数；奇数说明有 `` ` `` 开了没关，
/// 后面的代码全被吞进模板字符串，浏览器抛 SyntaxError。这类残句最常见于续写断点。
fn check_unclosed_template(script: &str, issues: &mut Vec<Issue>) {
    let count = crate::webgen::dom::stray_backticks(script);
    if count == 0 || count % 2 == 0 {
        return;
    }
    issues.push(Issue {
        code: "UNCLOSED_TEMPLATE",
        severity: Severity::Critical,
        detail: format!(
            "脚本里有 {count} 个反引号，是奇数 —— 有模板字符串开了没闭合。它会把后面的代码全吞进去，浏览器抛 SyntaxError，整段脚本不执行。"
        ),
        fix: "找出没闭合的那个模板字符串（通常在续写断点附近）并补上结束反引号；若该处本身就是残句，把残句删掉并补齐对应的括号。"
            .to_string(),
    });
}

/// 单文件网页里的站内绝对路径链接 —— 点开必然 404。
///
/// 实测故障：生成页的导航写着 `<a href="/daily">今日脑洞</a>`。产物是**一个 HTML 文件**，
/// 站点根下没有 `/daily` 这个路径，用户一点就跳到 404 页（实测录屏里正是如此）。
/// 单文件网页的站内跳转只能用 hash 路由（`href="#/daily"`）或按钮 + 脚本。
fn check_dead_links(html: &str, issues: &mut Vec<Issue>) {
    let dead = dead_links(html);
    if dead.is_empty() {
        return;
    }
    issues.push(Issue {
        code: "DEAD_LINK",
        severity: Severity::Critical,
        detail: format!(
            "页面里有 {} 个指向站内路径的链接：{} —— 这是单文件网页，站点里没有这些路径，点开必然 404。",
            dead.len(),
            dead.join(" / ")
        ),
        fix: "站内跳转改成 hash 路由：href=\"#/视图名\"，并在脚本里用 window.addEventListener('hashchange', ...) 切换视图；或把链接换成绑定 click 的 <button>。不要写 href=\"/xxx\" 或 href=\"xxx.html\"。"
            .to_string(),
    });
}

/// 找出站内绝对路径的 `href` 取值（去重，保持出现顺序）。
///
/// 判据：以 `/` 开头（但不是 `//` 协议相对地址），或以 `.html` / `.htm` 结尾的相对文件路径。
/// `#...`、`http(s)://`、`mailto:`、`data:`、`javascript:` 一律放过。
pub fn dead_links(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut from = 0usize;
    while let Some(relative) = lower[from..].find("href=") {
        let at = from + relative + "href=".len();
        let rest = &html[at..];
        let (value, advance) = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let body: String = rest[1..].chars().take_while(|ch| *ch != quote).collect();
                let len = body.len();
                (body, 1 + len + 1)
            }
            _ => {
                let body: String = rest
                    .chars()
                    .take_while(|ch| !ch.is_whitespace() && *ch != '>')
                    .collect();
                let len = body.len();
                (body, len)
            }
        };
        let trimmed = value.trim();
        let absolute = trimmed.starts_with('/') && !trimmed.starts_with("//");
        let file_like = !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !trimmed.contains("://")
            && !trimmed.starts_with("mailto:")
            && !trimmed.starts_with("data:")
            && !trimmed.starts_with("javascript:")
            && (trimmed.ends_with(".html") || trimmed.ends_with(".htm"));
        if (absolute || file_like) && !out.iter().any(|item| item == trimmed) {
            out.push(trimmed.to_string());
        }
        from = at + advance;
        if from >= html.len() {
            break;
        }
    }
    out
}

/// 把站内绝对路径的 `href` 就地改成 hash 路由。
///
/// 纯机械缺陷就用机械手段修，不必麻烦模型：`/daily` → `#/daily`，`about.html` → `#/about`。
/// 这样至少不会一点就 404；配合提示词里的 hash 路由要求，模型写的页面也能正常切换视图。
pub fn fix_dead_links(html: &str) -> Option<String> {
    let dead = dead_links(html);
    if dead.is_empty() {
        return None;
    }
    let mut out = html.to_string();
    for link in &dead {
        let target = if link.starts_with('/') {
            format!("#{link}")
        } else {
            let stem = link
                .trim_end_matches(".html")
                .trim_end_matches(".htm")
                .trim_start_matches("./");
            format!("#/{stem}")
        };
        out = out.replace(&format!("\"{link}\""), &format!("\"{target}\""));
        out = out.replace(&format!("'{link}'"), &format!("'{target}'"));
    }
    Some(out)
}

/// 从 `localStorage` / `sessionStorage` 读出来的值没兜底就当字符串用。
///
/// 键不存在时 `getItem` 返回 `null`，`null.split(...)` / `null.length` 会抛 TypeError，
/// 而这类读取几乎都写在初始化流程里 —— 一抛，后面所有绑定与渲染全部不执行，页面等于死的。
/// 实测：`localStorage.getItem('history').split(',').forEach(...)` 在**首次打开**（还没有历史）
/// 时必崩，而静态看它毫不起眼。
///
/// 这是纯机械缺陷，所以 `fix_null_storage_reads` 直接就地补兜底，不必麻烦模型。
fn check_null_storage_reads(script: &str, issues: &mut Vec<Issue>) {
    let hits = null_storage_reads(script);
    if hits.is_empty() {
        return;
    }
    issues.push(Issue {
        code: "NULL_STORAGE_READ",
        severity: Severity::Critical,
        detail: format!(
            "有 {} 处从 localStorage / sessionStorage 读出的值没有兜底就直接当字符串用 —— 键不存在时读到 null，`.split` 之类会抛 TypeError，整段初始化中断，页面等于死的。",
            hits.len()
        ),
        fix: "给每个读取加兜底再处理，例如 `(localStorage.getItem('history') || '')`。"
            .to_string(),
    });
}

/// 找出"`xxx.getItem(...)` 后面紧跟 `.`"的位置，返回 `(起, 闭合括号)` 的字符下标（可含接收者）。
fn null_storage_reads(script: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = script.chars().collect();
    let lower: Vec<char> = script.to_ascii_lowercase().chars().collect();
    let needle: Vec<char> = "getitem(".chars().collect();
    let mut hits = Vec::new();
    let mut index = 0usize;

    while index + needle.len() <= lower.len() {
        if lower[index..index + needle.len()] != needle[..] {
            index += 1;
            continue;
        }
        // 前面必须是 `.`（`localStorage.getItem` / `window.sessionStorage.getItem`）。
        if index == 0 || chars[index - 1] != '.' {
            index += 1;
            continue;
        }
        let open = index + needle.len() - 1;
        let Some(close) = matching_brace_quote_aware(&chars, open) else {
            index += 1;
            continue;
        };
        // 后面紧跟 `.` 才算"直接当字符串用"；`JSON.parse(getItem(...))` 这种不会抛，放过。
        let mut after = close + 1;
        while after < chars.len() && chars[after].is_whitespace() {
            after += 1;
        }
        if after < chars.len() && chars[after] == '.' {
            let mut start = index - 1;
            while start > 0
                && (chars[start - 1].is_ascii_alphanumeric()
                    || chars[start - 1] == '_'
                    || chars[start - 1] == '$')
            {
                start -= 1;
            }
            hits.push((start, close));
        }
        index = close + 1;
    }

    hits
}

/// 从 `open`（一个左括号）开始找配对的右括号，跳过字符串与模板串。
fn matching_brace_quote_aware(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for cursor in open..chars.len() {
        let ch = chars[cursor];
        if let Some(current) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == current {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' | '`' => quote = Some(ch),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(cursor);
                }
            }
            _ => {}
        }
    }
    None
}

/// 给所有"没兜底的存储读取"补上 `|| ''`。没有可修的就返回 `None`。
pub fn fix_null_storage_reads(script: &str) -> Option<String> {
    let hits = null_storage_reads(script);
    if hits.is_empty() {
        return None;
    }
    let chars: Vec<char> = script.chars().collect();
    let mut out = String::new();
    let mut cursor = 0usize;
    for (start, end) in hits {
        if start < cursor {
            continue;
        }
        out.extend(chars[cursor..start].iter());
        out.push('(');
        out.extend(chars[start..=end].iter());
        out.push_str(" || '')");
        cursor = end + 1;
    }
    out.extend(chars[cursor.min(chars.len())..].iter());
    Some(out)
}

/// 把 ASCII 意义上的 `\w` 校验改成"认汉字"的写法。
///
/// 判据形状很固定：`/^[^\w\s]+$/.test(x)` —— 意图是"输入不能全是标点或表情"，但 `\w` 不含汉字，
/// 于是「要不要辞职去旅行」被判成非法，用户输入什么都点不出结果。
///
/// 改法是往这个否定字符类里补上 CJK 区段：`[^\w` → `[^\u4e00-\u9fff\w`。
/// 之所以不用更"正统"的 `\p{L}`：那要求正则带 `u` 标志，而给一个字面量补标志要改到
/// 右斜杠附近，位置判定容易出错；加区段不需要任何标志，改完语义就直接对了。
pub fn fix_narrow_word_class(script: &str) -> Option<String> {
    if !script.contains("[^\\w") {
        return None;
    }
    let mut changed = false;
    let mut out = String::with_capacity(script.len() + 64);
    for line in script.split_inclusive('\n') {
        // 只动校验调用行；已经认汉字的行不重复加区段。
        if line.contains(".test(") && line.contains("[^\\w") && !line.contains("u4e00") {
            out.push_str(&line.replace("[^\\w", "[^\\u4e00-\\u9fff\\w"));
            changed = true;
        } else {
            out.push_str(line);
        }
    }
    if changed {
        Some(out)
    } else {
        None
    }
}

/// 单文件网页引用了外部资源。
///
/// 实测原样（端到端第 8 轮）：产物用 `<img src="https://chart.googleapis.com/chart?...">`
/// 画二维码 —— 既违反"单文件、不依赖外部服务"的约束，而且那个 Google Chart API 早在 2019 年
/// 就已废弃，图片根本加载不出来；URL 里还写死着 `https://www.example.com` 这个占位域名。
/// 用户看到的是一张破图，而不是二维码。
fn check_external_dependencies(html: &str, issues: &mut Vec<Issue>) {
    let found = external_dependencies(html);
    if found.is_empty() {
        return;
    }
    issues.push(Issue {
        code: "EXTERNAL_DEPENDENCY",
        severity: Severity::Critical,
        detail: format!(
            "页面引用了 {} 处外部资源：{} —— 这是单文件网页，离线打开或部署后这些资源都取不到（外链服务本身也可能早已下线），用户看到的是破图或缺功能。",
            found.len(),
            found.join(" / ")
        ),
        fix: "把外部资源改成页面内自产：二维码用内联的编码实现自己画（canvas 或 div 矩阵），图标用内联 SVG 或字符，字体用系统字体栈。不要写 src=\"http…\" 或 <link href=\"http…\">。"
            .to_string(),
    });
}

/// 找出 `<img>/<script>/<iframe>/<source>` 的 `src`、以及 `<link>` 的 `href` 里的外部地址。
pub fn external_dependencies(html: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        let trimmed = value.trim();
        let external = trimmed.starts_with("http://")
            || trimmed.starts_with("https://")
            || trimmed.starts_with("//");
        if external {
            let shown: String = trimmed.chars().take(72).collect();
            if !out.iter().any(|item| item == &shown) {
                out.push(shown);
            }
        }
    };

    let lower = html.to_ascii_lowercase();
    for attribute in ["src=", "srcset="] {
        let mut from = 0usize;
        while let Some(relative) = lower[from..].find(attribute) {
            let at = from + relative + attribute.len();
            let rest = &html[at..];
            let value: String = match rest.chars().next() {
                Some(quote @ ('"' | '\'')) => {
                    rest[1..].chars().take_while(|ch| *ch != quote).collect()
                }
                _ => rest
                    .chars()
                    .take_while(|ch| !ch.is_whitespace() && *ch != '>')
                    .collect(),
            };
            for part in value.split(',') {
                push(part.trim().split_whitespace().next().unwrap_or(""));
            }
            from = at + value.len().max(1);
            if from >= html.len() {
                break;
            }
        }
    }

    // `<link href="http…">` 才算外部依赖：`<a href="http…">` 是正常的站外链接。
    let mut from = 0usize;
    while let Some(relative) = lower[from..].find("<link") {
        let open = from + relative;
        let Some(gt) = lower[open..].find('>') else {
            break;
        };
        let tag = &html[open..open + gt];
        let tag_lower = &lower[open..open + gt];
        if let Some(position) = tag_lower.find("href=") {
            let rest = &tag[position + 5..];
            let value: String = match rest.chars().next() {
                Some(quote @ ('"' | '\'')) => {
                    rest[1..].chars().take_while(|ch| *ch != quote).collect()
                }
                _ => rest
                    .chars()
                    .take_while(|ch| !ch.is_whitespace() && *ch != '>')
                    .collect(),
            };
            push(&value);
        }
        from = open + gt + 1;
    }

    out
}

/// 把 `alert(...)` 换成页面内的提示条。
///
/// 为什么值得做成机械修复：`PLACEHOLDER_ALERT` 是唯一常见"非结构性"严重问题，而它**没有**
/// 可行的自动修复路线 —— 补写函数治不了它，整段重写脚本又装不下（实测重写回来的是残脚本，
/// 只能拒收）。结果是一条 alert 就让整版停在"自检不通过"。
///
/// 语义是严格等价的：`alert` 的返回值是 `undefined`，这里只是把它换成"往页面里插一个提示条
/// 并自动淡出"。消息内容、调用时机、返回行为都不变，用户从"被弹窗打断"变成"看到页内提示"。
///
/// 只动 `alert(` —— `confirm` / `prompt` 会把返回值当初值用（`if (confirm(...))`），
/// 机械替换会改语义，那两类仍交给模型。
pub fn fix_placeholder_alerts(script: &str) -> Option<String> {
    let calls = count_of(&script.to_ascii_lowercase(), "alert(");
    let inline = count_of(&script.to_ascii_lowercase(), "inlinealert(");
    if calls == 0 || calls == inline {
        return None;
    }
    let mut out = replace_identifier_calls(script, "alert", "inlineAlert");
    if out == script {
        return None;
    }
    out.push_str(INLINE_ALERT_DEFINITION);
    Some(out)
}

/// 把 `name(` 这类调用改写成另一个标识符。
///
/// 必须带词法状态：字符串、模板字符串、注释里的 `alert(` 只是内容，动它会改坏页面
/// （实测一条 `const tip = "alert(不是调用)"` 就会被误改）。
fn replace_identifier_calls(script: &str, from: &str, to: &str) -> String {
    let chars: Vec<char> = script.chars().collect();
    let needle: Vec<char> = from.chars().collect();
    let mut out = String::with_capacity(script.len() + 64);
    let mut index = 0usize;
    #[derive(PartialEq)]
    enum State {
        Code,
        Line,
        Block,
        Single,
        Double,
        Template,
    }
    let mut state = State::Code;
    while index < chars.len() {
        let ch = chars[index];
        if state != State::Code {
            out.push(ch);
            match state {
                State::Line if ch == '\n' => state = State::Code,
                State::Block => {
                    if ch == '*' && chars.get(index + 1) == Some(&'/') {
                        out.push('/');
                        index += 1;
                        state = State::Code;
                    }
                }
                State::Single | State::Double | State::Template if ch == '\\' => {
                    if let Some(next) = chars.get(index + 1) {
                        out.push(*next);
                        index += 1;
                    }
                }
                State::Single if ch == '\'' => state = State::Code,
                State::Double if ch == '"' => state = State::Code,
                State::Template if ch == '`' => state = State::Code,
                _ => {}
            }
            index += 1;
            continue;
        }
        match ch {
            '\'' => state = State::Single,
            '"' => state = State::Double,
            '`' => state = State::Template,
            '/' if chars.get(index + 1) == Some(&'/') => {
                out.push_str("//");
                index += 2;
                state = State::Line;
                continue;
            }
            '/' if chars.get(index + 1) == Some(&'*') => {
                out.push_str("/*");
                index += 2;
                state = State::Block;
                continue;
            }
            _ => {}
        }
        let starts_here = index + needle.len() < chars.len()
            && chars[index..index + needle.len()] == needle[..]
            && chars[index + needle.len()] == '(';
        if !starts_here {
            out.push(ch);
            index += 1;
            continue;
        }
        // 前面必须不是标识符字符；`.alert(` 要区分是 `obj.alert(` 还是 `window.alert(`
        // —— 后者仍然是原生弹窗，也要改写。
        let prev = if index == 0 {
            None
        } else {
            Some(chars[index - 1])
        };
        let is_prefixed = prev
            .map(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
            .unwrap_or(false);
        let is_member = prev == Some('.') && !member_base_is_global(&chars, index - 1);
        if is_member || is_prefixed {
            out.push(ch);
            index += 1;
            continue;
        }
        out.push_str(to);
        index += needle.len();
    }
    out
}

/// `.` 前面的那个标识符是不是 `window` / `globalThis`（`window.alert` 仍是原生弹窗）。
fn member_base_is_global(chars: &[char], dot: usize) -> bool {
    if dot == 0 {
        return false;
    }
    let mut start = dot;
    while start > 0
        && (chars[start - 1].is_ascii_alphanumeric()
            || chars[start - 1] == '_'
            || chars[start - 1] == '$')
    {
        start -= 1;
    }
    let base: String = chars[start..dot].iter().collect();
    base == "window" || base == "globalThis"
}

/// 页内提示条的实现。追加到脚本末尾，与 `alert` 一样不返回值。
const INLINE_ALERT_DEFINITION: &str = r#"
// 页内提示条：代替 alert，避免用系统弹窗承载页面状态（提示内容与时机不变）。
function inlineAlert(message) {
    var box = document.getElementById("inline-alert-box");
    if (!box) {
        box = document.createElement("div");
        box.id = "inline-alert-box";
        box.setAttribute("role", "status");
        box.style.cssText = "position:fixed;left:50%;bottom:24px;transform:translateX(-50%);max-width:82vw;padding:10px 16px;border-radius:10px;background:rgba(17,17,17,.88);color:#fff;font-size:14px;line-height:1.6;text-align:center;z-index:9999;opacity:0;transition:opacity .2s ease";
        document.body.appendChild(box);
    }
    box.textContent = message == null ? "" : String(message);
    box.style.opacity = "1";
    if (box.hideTimer) { clearTimeout(box.hideTimer); }
    box.hideTimer = setTimeout(function () { box.style.opacity = "0"; }, 2600);
}
"#;

/// 正则里的 `\p{...}` 写法不合法。
///
/// 实测原样（端到端第 5 轮）：模型想放宽中文校验，写了
/// `/^[a-zA-Z0-9\s\p{Han}\u3400-\u4DBF\u4E00-\u9FFF]+$/u`。
/// `Han` 是 **script 名**，JS 里必须写成 `\p{Script=Han}`；写成 `\p{Han}` 会抛
/// `SyntaxError: Invalid property name in character class` —— 又是一处"语法检查过得去、
/// 整页却一行都不执行"的缺陷，而且它藏在正则里，肉眼极难发现。
fn check_regex_property(script: &str, issues: &mut Vec<Issue>) {
    let bad = invalid_regex_properties(script);
    if bad.is_empty() {
        return;
    }
    issues.push(Issue {
        code: "REGEX_PROPERTY_INVALID",
        severity: Severity::Critical,
        detail: format!(
            "正则里有 {} 处不存在的 Unicode 属性名：{} —— 浏览器会抛 SyntaxError: Invalid property name in character class，整段脚本一行都不执行。",
            bad.len(),
            bad.join(" / ")
        ),
        fix: "script 名要写成 `\\p{Script=Han}` 这种形式；`\\p{Han}`、`\\p{Latin}` 都是错的。只想匹配汉字的话，直接用区段 `[\\u4e00-\\u9fff]` 最稳，不必依赖 `u` 标志。"
            .to_string(),
    });
}

/// 找出 `\p{...}` / `\P{...}` 里那些 JavaScript 不认的属性名。
///
/// 合法的两类：`Script=名字` / `General_Category=值`（含 `sc=` / `gc=` 简写），
/// 以及 `\p{L}`、`\p{Letter}`、`\p{Alphabetic}` 这类属性名本身。其余一律算错。
fn invalid_regex_properties(script: &str) -> Vec<String> {
    const BINARY: &[&str] = &[
        "Alphabetic",
        "Any",
        "ASCII",
        "Assigned",
        "Cased",
        "Case_Ignorable",
        "Changes_When_Lowercased",
        "Changes_When_Uppercased",
        "Default_Ignorable_Code_Point",
        "ID_Continue",
        "ID_Start",
        "Lowercase",
        "Uppercase",
        "White_Space",
        // 广义类别名（长写与缩写）
        "Letter",
        "Lowercase_Letter",
        "Uppercase_Letter",
        "Titlecase_Letter",
        "Modifier_Letter",
        "Other_Letter",
        "Number",
        "Decimal_Number",
        "Letter_Number",
        "Other_Number",
        "Mark",
        "Punctuation",
        "Separator",
        "Symbol",
        "Other",
        "L",
        "Lu",
        "Ll",
        "Lt",
        "Lm",
        "Lo",
        "M",
        "Mn",
        "Mc",
        "Me",
        "N",
        "Nd",
        "Nl",
        "No",
        "P",
        "Pc",
        "Pd",
        "Ps",
        "Pe",
        "Pi",
        "Pf",
        "Po",
        "S",
        "Sm",
        "Sc",
        "Sk",
        "So",
        "Z",
        "Zs",
        "Zl",
        "Zp",
        "C",
        "Cc",
        "Cf",
        "Cs",
        "Co",
        "Cn",
    ];
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = script.chars().collect();
    let mut index = 0usize;
    while index + 3 < chars.len() {
        let is_marker = chars[index] == '\\'
            && (chars[index + 1] == 'p' || chars[index + 1] == 'P')
            && chars[index + 2] == '{';
        if !is_marker {
            index += 1;
            continue;
        }
        let start = index + 3;
        let mut end = start;
        while end < chars.len() && chars[end] != '}' {
            end += 1;
        }
        if end >= chars.len() {
            break;
        }
        let name: String = chars[start..end].iter().collect();
        let trimmed = name.trim();
        let ok = trimmed.contains('=') || BINARY.contains(&trimmed);
        if !ok && !out.iter().any(|item| item == trimmed) {
            out.push(trimmed.to_string());
        }
        index = end + 1;
    }
    out
}

/// 把认错写法的 Unicode 属性名改对 —— `\p{Han}` → `\p{Script=Han}`。
///
/// 纯机械替换，与其它确定性修复一样不劳模型。只认几个常见的 script 名，
/// 其余不动（会由 `REGEX_PROPERTY_INVALID` 报给模型处理）。
pub fn fix_regex_properties(script: &str) -> Option<String> {
    const SCRIPTS: &[&str] = &[
        "Han",
        "Latin",
        "Greek",
        "Cyrillic",
        "Arabic",
        "Hebrew",
        "Thai",
        "Hiragana",
        "Katakana",
        "Hangul",
        "Devanagari",
        "Armenian",
        "Georgian",
        "Bengali",
        "Tamil",
    ];
    let mut out = script.to_string();
    let mut changed = false;
    for name in SCRIPTS {
        for marker in ["p", "P"] {
            let from = format!("\\{marker}{{{name}}}");
            if out.contains(&from) {
                out = out.replace(&from, &format!("\\{marker}{{Script={name}}}"));
                changed = true;
            }
        }
    }
    if changed {
        Some(out)
    } else {
        None
    }
}

/// 拿 ASCII 意义上的 `\w` 去校验中文输入。
///
/// JS 的 `\w` 等于 `[A-Za-z0-9_]`，**汉字不算**。于是 `/^[^\w\s]+$/` 这种"整句都是非法字符"
/// 的判据，会把「要不要辞职去旅行」判成非法。实测生成的页面把它挂在输入校验上，
/// 后果是**任何中文输入都点不出结果** —— 而语法、括号、数据链路全都正常，
/// 静态检查完全看不出这一层。
fn check_narrow_word_class(script: &str, issues: &mut Vec<Issue>) {
    let hits: Vec<&str> = script
        .lines()
        .filter(|line| line.contains(".test(") && line.contains("[^\\w"))
        .take(3)
        .collect();
    if hits.is_empty() {
        return;
    }
    issues.push(Issue {
        code: "NARROW_WORD_CLASS",
        severity: Severity::Critical,
        detail: format!(
            "有 {} 处用 `[^\\w…]` 做输入校验。JS 的 `\\w` 只等于 `[A-Za-z0-9_]`，**汉字不算**，所以这类判据会把正常的中文输入判成非法 —— 用户输入什么都得不到结果。",
            hits.len()
        ),
        fix: "别拿 `\\w` 判中文：去掉这条校验，或改用 `\\p{L}` 并带上 `u` 标志（如 `/^[^\\p{L}\\p{N}\\s]+$/u`），或干脆只判断是否为空白与长度。"
            .to_string(),
    });
}

/// 有按钮却没有任何事件绑定。
fn check_buttons(html: &str, script: &str, issues: &mut Vec<Issue>) {
    let html_lower = html.to_ascii_lowercase();
    let buttons = count_of(&html_lower, "<button")
        + count_of(&script.to_ascii_lowercase(), "createelement(\"button\")");
    if buttons == 0 {
        return;
    }
    // 事件绑定有两个来源：脚本里绑（`el.onclick = ...` / `addEventListener`），
    // 以及 **HTML 内联属性**（`<button onclick="generateNames()">`）。实测模型偏爱后者，
    // 而这里早先只扫脚本，于是把"按钮明明绑好了"误报成"点下去不会有反应"，
    // 白白触发一轮没必要的修复。
    let script_lower = script.to_ascii_lowercase();
    let handlers = count_of(&script_lower, "onclick")
        + count_of(&script_lower, "addeventlistener(\"click\"")
        + count_of(&script_lower, "addeventlistener('click'")
        + count_of(&script_lower, "onchange")
        + count_of(&script_lower, "oninput")
        + count_of(&html_lower, "onclick=")
        + count_of(&html_lower, "onchange=")
        + count_of(&html_lower, "oninput=");
    if handlers == 0 {
        issues.push(Issue {
            code: "BUTTON_NO_HANDLER",
            // 提级为 Critical：整份脚本连一处事件绑定都找不到，页面上的按钮**全部**点不动，
            // 与"没有脚本"（NO_SCRIPT）是同一件事，不该只当提示。过去它只是 Major，
            // 于是"页面画得完整、按钮全不响应"的成品会被直接交付给用户。
            severity: Severity::Critical,
            detail: format!(
                "页面有 {buttons} 处按钮，但脚本里没有任何事件绑定，点下去不会有反应。"
            ),
            fix:
                "给每个按钮绑定点击事件，事件里要做真实的状态变化（生成、复制、收藏、切换主题等）。"
                    .to_string(),
        });
    }
}

/// 把 HTML 里所有 `<script>…</script>` 的**内容**拿掉，只留脚本之外的标记。
///
/// 用途见 `check_dead_functions`：判断"函数有没有被外部引用"时必须排除脚本自己，
/// 否则函数名在它自己的定义处就被数到一次，判据失效。
fn html_without_scripts(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0usize;
    loop {
        let Some(relative) = lower[cursor..].find("<script") else {
            out.push_str(&html[cursor..]);
            break;
        };
        let open = cursor + relative;
        // 标签头（`<script …>`）保留下来，它本身不含函数名。
        let Some(gt) = lower[open..].find('>') else {
            out.push_str(&html[cursor..]);
            break;
        };
        let content_start = open + gt + 1;
        out.push_str(&html[cursor..content_start]);

        // 找配对的 </script>；找不到就把剩余内容整体丢弃（未闭合的脚本）。
        let Some(close_rel) = lower[content_start..].find("</script") else {
            break;
        };
        cursor = content_start + close_rel;
    }
    out
}

/// 定义了却从未被调用的函数。
///
/// 修的坑：这里原本用 `count_of(html, &name)` 统计"HTML 里的调用"，但传进来的 `html`
/// **整份文档里就包含 `<script>`**，于是函数名至少被数到 1 次（它的定义处），
/// 与 `count_of(script, &name)` 的 1 次相加恒为 2 > 1 —— **这个检查从来没触发过**。
/// 空壳函数于是永远抓不到，"需求功能只写了个函数名"这类页面畅通无阻地交付。
///
/// 现在把 HTML 里的脚本段剥掉再统计：剩下的才是真正的外部引用
/// （内联事件属性 `onclick="foo()"`、分片的第二个 `<script>`、`data-*` 里的名字等）。
fn check_dead_functions(script: &str, html: &str, issues: &mut Vec<Issue>) {
    let outside_script = html_without_scripts(html);
    let mut dead = Vec::new();
    for (name, _) in function_bodies(script) {
        if name == "<匿名>" {
            continue;
        }
        // 脚本里出现 1 次 = 只有定义；再加上脚本外部的引用次数。
        let calls = count_of(script, &name) + count_of(&outside_script, &name);
        if calls <= 1 {
            dead.push(name);
        }
    }
    if dead.is_empty() {
        return;
    }
    // 成片的死函数 = 需求里的功能整块只写了空壳（实测：模型把 6 组词库函数全定义好，
    // 却没接到任何按钮上）。这时按严重问题处理，必须走修复轮；只有零星一两条才当提示，
    // 免得因误报白跑一分钟推理。
    let severity = if dead.len() >= 3 {
        Severity::Critical
    } else {
        Severity::Major
    };
    issues.push(Issue {
        code: "DEAD_FUNCTION",
        severity,
        detail: format!(
            "这些函数定义了但从未被调用：{} —— 需求里的功能可能只是写了个空壳。",
            dead.join(" / ")
        ),
        fix: "把需要的功能接到按钮或页面事件上；确实用不到的删掉。".to_string(),
    });
}

/// 顶层数组常量里，**声明了却从未被读取**的那些名字。
///
/// 实测故障：需求里给了 6 组风格词库，模型把其中 3 组**照抄进页面**（`const casualPrefixes = [...]`），
/// 但生成分支只写了 4 组，剩下 3 组在脚本里再没出现过 —— 词库白给，用户切到那些风格时
/// 拿到的是别的风格的名字，或者干脆是 `undefined`。
///
/// `GENERATOR_IGNORES_DATA` 抓不到这种"用了一部分"的情形（只要有任意随机函数吃到了数组
/// 就放行），所以这里单独查：**顶层数组常量名在整段脚本里只出现一次**（那一次就是它自己的声明）。
///
/// 判据取 `<= 1` 而不是 `== 1`：`const X = [` 这一处 `X` 就是第一次出现，所以健康的数组
/// 至少出现两次（声明 + 使用）。
pub fn unused_data_pools(script: &str) -> Vec<String> {
    let arrays = top_level_arrays(script);
    if arrays.is_empty() {
        return Vec::new();
    }
    let masked = mask_non_code(script);
    let tokens: Vec<String> = tokens_of(&masked)
        .into_iter()
        .filter(|(_, is_ident)| *is_ident)
        .map(|(token, _)| token)
        .collect();

    arrays
        .into_iter()
        .filter(|name| tokens.iter().filter(|token| *token == name).count() <= 1)
        .collect()
}

fn check_unused_data_pools(script: &str, issues: &mut Vec<Issue>) {
    let unused = unused_data_pools(script);
    if unused.is_empty() {
        return;
    }

    issues.push(Issue {
        code: "UNUSED_DATA_POOL",
        severity: Severity::Critical,
        detail: format!(
            "这些数据数组声明了却从未被使用：{} —— 数据抄进了页面，但没有任何分支读取它。",
            unused.join(" / ")
        ),
        fix: "把每个数据数组都接进它该在的分支：需求里列出的每一项（每个场景、每种风格、每个字表）都必须有代码真正读取自己那一份数据。用不到的数据不要写在页面里。"
            .to_string(),
    });
}

/// 每个具名函数声明在脚本里的**字符区间**：`(name, start, end)`。
///
/// `start` 指向 `function` 关键字，`end` 是配对的大括号之后一位。
///
/// 用途是"只替换某个函数"的定点修复：实测"数据声明了却没用上"这类**局部**缺陷，让模型
/// 重写整段脚本有两个后果 —— 输出上限装不下（8827 字符脚本要 4200 token，上限只有 3928），
/// 且重写过程会把已经正确的逻辑写坏（实测引入悬空调用 `createCard`，整页交互全废）。
/// 缺陷是局部的，修复手段也必须局部。
///
/// 用 `match_block` 做括号配平（它认引号），所以字符串里的 `{` 不会扰乱边界。
pub fn function_spans(script: &str) -> Vec<(String, usize, usize)> {
    let keyword = "function";
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(relative) = script[from..].find(keyword) {
        let at = from + relative;
        let boundary_ok = script[..at]
            .chars()
            .next_back()
            .map(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '$')
            .unwrap_or(true);
        let after = &script[at + keyword.len()..];
        let name: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if boundary_ok && !name.is_empty() {
            if let Some(brace) = after.find('{') {
                if let Some(body) = match_block(&after[brace..]) {
                    let start_byte = at;
                    let end_byte = at + keyword.len() + brace + body.len();
                    out.push((
                        name,
                        script[..start_byte].chars().count(),
                        script[..end_byte].chars().count(),
                    ));
                }
            }
        }
        from = at + keyword.len();
        if from >= script.len() {
            break;
        }
    }
    out
}

/// 当前这组问题能不能用"只替换相关函数"的定点修复来治。
///
/// 返回需要接上数据的那几个池名。只在**没有更致命问题**时才返回 `Some`：
/// 有悬空调用 / 按钮没绑事件时，先走就地补写路线（那会让整页点不动，优先级更高）。
/// 也没有严重级的 `UNUSED_DATA_POOL` 时返回 `None` —— 那就没这个可修的。
pub fn function_patch_plan(html: &str, issues: &[Issue]) -> Option<Vec<String>> {
    let has_worse = issues
        .iter()
        .any(|issue| issue.code == "UNDEFINED_REFERENCE" || issue.code == "BUTTON_NO_HANDLER");
    if has_worse {
        return None;
    }
    let flagged = issues
        .iter()
        .any(|issue| issue.code == "UNUSED_DATA_POOL" && issue.severity == Severity::Critical);
    if !flagged {
        return None;
    }
    let pools = unused_data_pools(&crate::webgen::dom::merged_script(html));
    if pools.is_empty() {
        None
    } else {
        Some(pools)
    }
}

/// 页面上的选项在脚本里从未被处理。
///
/// 实测故障：`<select>` 里有 6 个场景（网名 / 游戏 ID / 宠物名 / 店铺名 / 角色名 / 情侣名），
/// 而 `generateNames()` 里只写了 `if (scene === '店铺名')` 和 `else if (scene === '情侣名')`
/// 两个分支。用户选"网名"时两个条件都不成立，`name` 变量从头到尾没被赋过值，结果区渲染出
/// 一串 `undefined`。**这才是"功能极不完善"的根部成因**，比随机字符更隐蔽 —— 页面看着一切正常。
///
/// 只检查**含中文的值**：`<option value="2">2 个字</option>` 这种数值选项在脚本里本来就
/// 不会被当字面量比较（走 `parseInt`），拿它做判据必然误报。
fn check_option_coverage(html: &str, script: &str, issues: &mut Vec<Issue>) {
    if script.trim().is_empty() {
        return;
    }
    let options = select_option_values(html);
    if options.len() < 2 {
        return;
    }
    let missing: Vec<String> = options
        .into_iter()
        .filter(|value| !mentions_literal(script, value))
        .collect();
    if missing.is_empty() {
        return;
    }

    let listed = missing
        .iter()
        .map(|value| format!("「{value}」"))
        .collect::<Vec<_>>()
        .join("、");
    issues.push(Issue {
        code: "OPTION_UNHANDLED",
        severity: Severity::Critical,
        detail: format!(
            "下拉选项 {listed} 在整个脚本里没有被任何条件判断或查表用到 —— 用户选中它们之后，程序不会走任何一条正经分支。"
        ),
        fix: "为每一个选项取值都写出对应的处理逻辑：用 if/else 或查表把每个取值映射到它自己的一组参数（词库、长度、文案、前缀后缀），不允许只实现一部分、其余掉进默认分支。"
            .to_string(),
    });
}

/// 抽出 `<select>` 里的选项取值。
///
/// 优先取 `value="..."`，没有 `value` 属性时退回标签文本（`<option>古风</option>`）。
/// 只保留**含中文字符**且非空的值：数值型选项（字数 2/4/6、数量 3/6/9）不走字面量比较，
/// 收进来只会制造误报。同时按出现顺序去重。
fn select_option_values(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut values: Vec<String> = Vec::new();
    let mut from = 0usize;

    while let Some(relative) = lower[from..].find("<option") {
        let at = from + relative;
        let Some(gt) = html[at..].find('>') else {
            break;
        };
        let open_tag = &html[at..at + gt];
        let body_start = at + gt + 1;
        let close = html[body_start..]
            .find("</option")
            .unwrap_or(html.len() - body_start);
        let text = html[body_start..body_start + close].trim().to_string();

        let value = attribute_value(open_tag, "value")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| text.clone());
        let value = value.trim().to_string();

        let has_cjk = value
            .chars()
            .any(|ch| matches!(ch, '\u{4E00}'..='\u{9FFF}'));
        if has_cjk && !values.contains(&value) {
            values.push(value);
        }
        from = at + "<option".len();
    }
    values
}

/// 从开标签里读一个属性的值（`value="x"` / `value='x'`），大小写不敏感。
fn attribute_value(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{name}=");
    let at = lower.find(&needle)? + needle.len();
    let rest = tag[at..].trim_start();
    let quote = rest.chars().next()?;
    if quote == '"' || quote == '\'' {
        let tail = &rest[quote.len_utf8()..];
        let end = tail.find(quote)?;
        Some(tail[..end].to_string())
    } else {
        let end = rest
            .find(|ch: char| ch.is_whitespace() || ch == '>')
            .unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

/// 这个字面量有没有被当成"真实的取值"用过。
///
/// 关键陷阱：**躺在数据数组里不算用过**。页面里常有一份
/// ```js
/// const scenes = ["网名", "游戏 ID", "宠物名", "店铺名", "角色名", "情侣名"];
/// ```
/// 这份数组把 6 个取值都写全了，如果判据只是"字面量出现过"，那 6 个场景全都会被认为
/// "已处理" —— 而实际上生成函数只写了其中两个分支。所以必须区分**取值**和**数据**：
///
/// | 写法 | 判定 |
/// |---|---|
/// | `scene === '网名'` / `!=` | ✅ 取值 |
/// | `switch (x) { case '网名': }` | ✅ 取值 |
/// | `{'网名': fn}` / `map = { '网名': .. }` | ✅ 取值（对象键） |
/// | `pools['网名']` | ✅ 取值（索引访问） |
/// | `['网名','情侣名'].includes(x)` | ✅ 取值（后一项 `]`、前一项 `.includes`） |
/// | `f('网名')` | ✅ 取值（实参） |
/// | `const scenes = ["网名", "游戏 ID", …]` | ❌ 只是数据 |
fn mentions_literal(script: &str, value: &str) -> bool {
    for quote in ['\'', '"'] {
        let needle = format!("{quote}{value}{quote}");
        let mut from = 0usize;
        while let Some(relative) = script[from..].find(&needle) {
            let at = from + relative;
            let before = script[..at].trim_end();
            let after = script[at + needle.len()..].trim_start();
            if used_as_value(before, after) {
                return true;
            }
            from = at + needle.len();
            if from >= script.len() {
                break;
            }
        }
    }
    false
}

/// 前后文是否说明这个字符串字面量被当"取值"用了（而不是数组里的一项数据）。
fn used_as_value(before: &str, after: &str) -> bool {
    if before.ends_with("==")
        || before.ends_with("!=")
        || before.ends_with("case")
        || before.ends_with("return")
        || before.ends_with("includes(")
        || before.ends_with("switch(")
        || before.ends_with("indexOf(")
    {
        return true;
    }
    // 对象键 `{'网名': …}`、数组末尾 `[…, '网名']`、实参 `f(…, '网名')`
    if after.starts_with(':') || after.starts_with(']') || after.starts_with(')') {
        return true;
    }
    // 索引访问 `map['网名']`：`[` 前面必须是标识符或闭合符号，而不是 `=`（那是数组声明）。
    if let Some(head) = before.strip_suffix('[') {
        if let Some(last) = head.trim_end().chars().next_back() {
            if last.is_ascii_alphanumeric() || matches!(last, '_' | '$' | ']' | ')') {
                return true;
            }
        }
    }
    // 作为实参传入：`render('网名')`
    before.ends_with('(')
}

/// 需求里的功能有没有落到页面上。
///
/// 判据是"**这条需求给出的任何一个词，能在页面上找到**"。早先只查需求名（`生成动作`、
/// `单条结果操作` 这类），实测必然误报 —— 那是需求文档里的**内部标签**，页面上的按钮写的
/// 是"生成名字""再摇一批"，功能明明在，名字对不上。
///
/// 现在把整行需求都拆成候选词：连续中文串（2–8 字）再加上它们的两字片段。
/// 这样"默认网名"能和页面上的"网名"对上。**刻意做宽**：这是条提示级检查，
/// 只有在页面上跟某条需求完全沾不上边时才该出声，误报比漏报更糟。
fn check_feature_coverage(html: &str, spec: &str, issues: &mut Vec<Issue>) {
    let mut features: Vec<(String, Vec<String>)> = Vec::new();
    let mut in_features = false;
    for line in spec.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            in_features = heading.trim() == "功能需求";
            continue;
        }
        if !in_features || trimmed.is_empty() {
            continue;
        }
        let text =
            trimmed.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '、');
        let text = text.trim_start();
        let text = if text.starts_with('[') {
            match text.find(']') {
                Some(index) => text[index + 1..].trim_start(),
                None => text,
            }
        } else {
            text
        };
        let Some(name) = first_cjk_run(text) else {
            continue;
        };
        let mut candidates: Vec<String> = Vec::new();
        for run in cjk_runs(text) {
            let length = run.chars().count();
            if !(2..=8).contains(&length) {
                continue;
            }
            // 长串再补上两字片段：`默认网名` 才可能和页面上的 `网名` 对上。
            if length >= 3 {
                let chars: Vec<char> = run.chars().collect();
                for window in chars.windows(2) {
                    let piece: String = window.iter().collect();
                    if !candidates.contains(&piece) {
                        candidates.push(piece);
                    }
                }
            }
            if !candidates.contains(&run) {
                candidates.push(run);
            }
        }
        if !candidates.is_empty() {
            features.push((name, candidates));
        }
    }
    if features.len() < 4 {
        return;
    }

    // 一条需求只要任何一个候选词能在页面上找到，就算它落地了。
    let missing: Vec<&str> = features
        .iter()
        .filter(|(_, candidates)| !candidates.iter().any(|token| html.contains(token.as_str())))
        .map(|(name, _)| name.as_str())
        .collect();
    // 阈值收紧：过去要求"缺失过半"（`missing * 2 >= features`）才出声，而一个真正的空壳页面
    // 往往**恰好**过半 —— 生成出来的那一半能对上文字，靠文字匹配看不出功能是空壳。
    // 现在改为"缺失达到三分之一"就记录，交给调用方按加权致命度决定要不要修。
    // 严重度仍是 Major：这条检查只做文字包含匹配，误报率高于其它检查，不该单独触发 Critical。
    if missing.len() < 2 || missing.len() * 3 < features.len() {
        return;
    }
    issues.push(Issue {
        code: "FEATURE_MISSING",
        severity: Severity::Major,
        detail: format!(
            "需求里的 {} 条功能，有 {} 条在页面里找不到任何相关文字：{}。",
            features.len(),
            missing.len(),
            missing.join(" / ")
        ),
        fix: "逐条核对需求的功能清单，把缺失的功能补进页面（含界面上可见的入口与背后的逻辑）。"
            .to_string(),
    });
}

/// 一段文本里所有的连续中文串。
fn cjk_runs(text: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if matches!(ch, '\u{4E00}'..='\u{9FFF}') {
            current.push(ch);
        } else if !current.is_empty() {
            runs.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

// ============================================================
// 源码扫描工具
// ============================================================

/// 顶层数组常量名（`const X = [` / `let X = [` / `var X = [`）。
fn top_level_arrays(script: &str) -> Vec<String> {
    let mut names = Vec::new();
    for keyword in ["const ", "let ", "var "] {
        let mut from = 0usize;
        while let Some(relative) = script[from..].find(keyword) {
            let start = from + relative + keyword.len();
            let name: String = script[start..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            if !name.is_empty() {
                let rest = script[start + name.len()..].trim_start();
                let rest = rest.strip_prefix('=').map(str::trim_start).unwrap_or(rest);
                if rest.starts_with('[') && !names.contains(&name) {
                    names.push(name);
                }
            }
            from = start;
            if from >= script.len() {
                break;
            }
        }
    }
    names
}

/// 提取具名函数的函数体（按大括号配平，跳过字符串里的括号）。
fn function_bodies(script: &str) -> Vec<(String, String)> {
    let keyword = "function";
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(relative) = script[from..].find(keyword) {
        let at = from + relative;
        let boundary_ok = script[..at]
            .chars()
            .next_back()
            .map(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '$')
            .unwrap_or(true);
        let after = &script[at + keyword.len()..];
        let name: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if boundary_ok {
            if let Some(brace) = after.find('{') {
                if let Some(body) = match_block(&after[brace..]) {
                    out.push((
                        if name.is_empty() {
                            "<匿名>".to_string()
                        } else {
                            name
                        },
                        body.to_string(),
                    ));
                }
            }
        }
        from = at + keyword.len();
        if from >= script.len() {
            break;
        }
    }
    out
}

/// 从以 `{` 开头的位置起做括号配平，返回整块内容（含首尾大括号）。
fn match_block(slice: &str) -> Option<&str> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, ch) in slice.char_indices() {
        if let Some(current) = quote {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                _ if ch == current => quote = None,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' | '\'' | '`' => quote = Some(ch),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&slice[..index + ch.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 子串 `needle` 出现的位置附近（左右各 `window` 个字符内）有没有 `other`。
///
/// 按字符窗口判断，避免在多字节字符中间切串导致 panic。
fn appears_nearby(text: &str, needle: &str, other: &str, window: usize) -> bool {
    let haystack: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let needle: Vec<char> = needle.to_ascii_lowercase().chars().collect();
    let other: Vec<char> = other.to_ascii_lowercase().chars().collect();
    if needle.is_empty() || other.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    for start in 0..=haystack.len() - needle.len() {
        if haystack[start..start + needle.len()] != needle[..] {
            continue;
        }
        let lo = start.saturating_sub(window);
        let hi = (start + needle.len() + window).min(haystack.len());
        if contains_sequence(&haystack[lo..hi], &other) {
            return true;
        }
    }
    false
}

fn contains_sequence(haystack: &[char], needle: &[char]) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }
    (0..=haystack.len() - needle.len())
        .any(|start| haystack[start..start + needle.len()] == needle[..])
}

/// 短语里第一段连续中文（≥2 字），用作功能关键词。
fn first_cjk_run(text: &str) -> Option<String> {
    let mut run = String::new();
    for ch in text.chars() {
        if is_cjk(ch) {
            run.push(ch);
        } else if run.chars().count() >= 2 {
            break;
        } else {
            run.clear();
        }
    }
    if run.chars().count() >= 2 {
        Some(run)
    } else {
        None
    }
}

fn is_cjk(ch: char) -> bool {
    matches!(ch, '\u{4E00}'..='\u{9FFF}')
}

fn count_of(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本次故障的原样复刻：随机字母拼串 + 词库只当装饰。
    const STUB_PAGE: &str = r#"<!DOCTYPE html>
<html><head><title>名字生成器</title></head><body>
<button id="go">生成</button><div id="out"></div>
<script>
const exampleNames = ["雨落江南", "星月同辉", "风起云涌", "花间一壶酒", "梦回长安"];
document.querySelector("ul").appendChild(document.createElement("li"));
function generatePart() {
    const length = Math.floor(Math.random() * 4) + 2;
    let part = "";
    for (let i = 0; i < length; i++) {
        const charCode = Math.floor(Math.random() * 26) + 97;
        part += String.fromCharCode(charCode);
    }
    return part;
}
function generateRandomName() { return generatePart(); }
document.getElementById("go").onclick = () => { document.getElementById("out").textContent = generateRandomName(); };
</script>
</body></html>"#;

    #[test]
    fn catches_random_letter_noise() {
        let issues = verify_page(STUB_PAGE, "");
        assert!(
            issues.iter().any(|issue| issue.code == "FAKE_NOISE"),
            "必须识别出随机字符拼凑：{issues:?}"
        );
    }

    #[test]
    fn catches_generator_ignoring_data() {
        let issues = verify_page(STUB_PAGE, "");
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "GENERATOR_IGNORES_DATA"),
            "必须识别出生成函数没读数据数组：{issues:?}"
        );
        assert!(has_critical(&issues));
    }

    #[test]
    fn catches_placeholder_marker() {
        let page = STUB_PAGE.replace("<script>", "<script>\n// 模拟数据");
        let issues = verify_page(&page, "");
        assert!(issues.iter().any(|issue| issue.code == "PLACEHOLDER_DATA"));
    }

    /// 录屏故障的原样复刻（《人生选择题》那份产物）：
    /// ① 生成按钮用 `document.body.replaceChild` 换掉嵌在 <header> 里的输入框 → 点了没反应；
    /// ② 「今日脑洞」写成 `<a href="/daily">` → 点开 404；
    /// ③ 后果揭示是 `alert("...随机生成")` → 功能是假的。
    const LEGACY_PAGE: &str = r#"<!DOCTYPE html>
<html><head><title>人生选择题</title></head><body>
<header>
  <input type="text" id="dilemmaInput" placeholder="输入你的困境..." />
  <button onclick="generateOptions()">生成选项</button>
  <a href="/daily" style="color:#7c4dff;">今日脑洞</a>
</header>
<script>
const act = ["抛硬币决定", "原地转三圈", "问一只路过的猫", "把决定权交给外卖小哥", "掷骰子", "反向执行"];
const obj = ["外星人", "小区保安", "楼下便利店老板", "未来穿越回来的自己", "一只柯基", "广场舞阿姨"];
function generateOptions() {
    const input = document.getElementById("dilemmaInput").value.trim();
    const optionsContainer = document.createElement("div");
    optionsContainer.innerHTML = act[Math.floor(Math.random() * act.length)] + obj[Math.floor(Math.random() * obj.length)];
    document.body.replaceChild(optionsContainer, document.getElementById("dilemmaInput"));
}
function revealConsequence() {
    alert("后果是：随机生成");
}
</script>
</body></html>"#;

    #[test]
    fn catches_dead_link_in_single_file_page() {
        let issues = verify_page(LEGACY_PAGE, "");
        assert!(
            issues.iter().any(|issue| issue.code == "DEAD_LINK"),
            "必须识别出站内绝对路径死链：{issues:?}"
        );
        assert!(has_critical(&issues));
    }

    #[test]
    fn catches_alert_used_as_interface() {
        let issues = verify_page(LEGACY_PAGE, "");
        assert!(
            issues.iter().any(|issue| issue.code == "PLACEHOLDER_ALERT"),
            "必须识别出用 alert 代替界面：{issues:?}"
        );
    }

    #[test]
    fn catches_fragile_body_replace_child() {
        let issues = verify_page(LEGACY_PAGE, "");
        assert!(
            issues.iter().any(|issue| issue.code == "FRAGILE_DOM_SWAP"),
            "必须识别出 body.replaceChild：{issues:?}"
        );
    }

    #[test]
    fn dead_link_fix_rewrites_to_hash_route() {
        let fixed = fix_dead_links(LEGACY_PAGE).expect("应当有可修复的死链");
        assert!(
            fixed.contains("href=\"#/daily\""),
            "绝对路径要改成 hash 路由"
        );
        assert!(!fixed.contains("href=\"/daily\""), "原绝对路径必须消失");
        assert!(dead_links(&fixed).is_empty(), "修完之后不应再报死链");
    }

    #[test]
    fn does_not_flag_hash_external_and_mail_links() {
        let page = r##"<a href="#/daily">今日</a><a href="https://example.com">外链</a><a href="mailto:x@y.z">邮件</a><a href="//cdn.example.com/a.js">协议相对</a>"##;
        assert!(
            dead_links(page).is_empty(),
            "hash / 外链 / mailto 不应被误报"
        );
    }

    #[test]
    fn catches_missing_data_source() {
        let page = r#"<html><body><button id="go">生成</button>
<script>function roll(){ return Math.floor(Math.random()*100); }
document.getElementById("go").onclick = roll;</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            issues.iter().any(|issue| issue.code == "NO_DATA_SOURCE"),
            "{issues:?}"
        );
    }

    #[test]
    fn catches_button_without_handler() {
        let page = r#"<html><body><button>生成</button><button>收藏</button>
<script>const a = 1;</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            issues.iter().any(|issue| issue.code == "BUTTON_NO_HANDLER"),
            "{issues:?}"
        );
    }

    #[test]
    fn catches_button_without_script_at_all() {
        let page = "<html><body><button>生成</button></body></html>";
        let issues = verify_page(page, "");
        assert!(issues.iter().any(|issue| issue.code == "NO_SCRIPT"));
        assert!(has_critical(&issues));
    }

    #[test]
    fn weighs_progress_by_how_many_functions_are_still_missing() {
        // 实测原样（端到端第 6 轮）：第一版缺 10 个函数，补写后只剩 2 个 —— 显然是进步，
        // 但 `UNDEFINED_REFERENCE` 只有一条，加上新冒出来的 `NARROW_WORD_CLASS` 后
        // 条数从 1 变 2，被旧判据当成"越改越差"整轮回滚，交付的还是 10 个函数全缺的版本。
        let before = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["a", "b"];
function start() {
    document.getElementById("out").textContent = randomPick(words) + hash("x");
}
document.getElementById("go").onclick = start;
</script></body></html>"#;
        let after = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["a", "b"];
function randomPick(list) { return list[Math.floor(Math.random() * list.length)]; }
function hash(text) { return text.length; }
function start() {
    document.getElementById("out").textContent = randomPick(words) + hash("x");
}
document.getElementById("go").onclick = start;
</script></body></html>"#;
        let before_issues = verify_page(before, "");
        let after_issues = verify_page(after, "");
        assert_eq!(missing_function_names(before).len(), 2);
        assert_eq!(missing_function_names(after).len(), 0);
        assert!(
            health_with_html(after, &after_issues) < health_with_html(before, &before_issues),
            "补齐函数后必须被判成进步：{after_issues:?}"
        );

        // 反向：缺得越多成本必须越高（同一条 `UNDEFINED_REFERENCE`，靠个数区分）。
        let worse = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["a", "b"];
function start() {
    document.getElementById("out").textContent = randomPick(words) + hash("x") + extraOne() + extraTwo();
}
document.getElementById("go").onclick = start;
</script></body></html>"#;
        assert_eq!(missing_function_names(worse).len(), 4);
        let worse_issues = verify_page(worse, "");
        assert!(
            health_with_html(worse, &worse_issues) > health_with_html(before, &before_issues),
            "缺 4 个不该比缺 2 个更便宜：{worse_issues:?}"
        );
    }

    #[test]
    fn treats_browser_builtins_as_defined() {
        // 实测代价：产物里 `new ClipboardItem({...})` 被当成"调用了从未定义的函数"，
        // 白跑两轮整段重写（160 秒）去修一个并不存在的缺陷 —— 真正的缺陷被挤到后面。
        let page = r#"<html><body><button id="go">复制图片</button><div id="out"></div>
<script>
const words = ["a", "b"];
function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
async function copyImage() {
    const blob = await new Promise(function (resolve) { resolve(new Blob(["x"])); });
    await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
    return pickOne(words);
}
document.getElementById("go").onclick = copyImage;
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "UNDEFINED_REFERENCE"),
            "浏览器内置构造器被当成悬空调用了：{issues:?}"
        );
    }

    #[test]
    fn catches_an_external_qr_api_in_a_single_file_page() {
        // 实测原样：产物用 Google Chart API 画二维码，还写死 `example.com`。
        // 单文件网页里这个资源必然取不到（该 API 也早已下线），用户看到破图。
        let page = r#"<html><body><button id="go">生成</button>
<img src="https://chart.googleapis.com/chart?chs=150x150&cht=qr&chl=https%3A%2F%2Fwww.example.com" alt="二维码">
<script>function roll() { return 1; }
document.getElementById("go").onclick = roll;</script></body></html>"#;
        let found = external_dependencies(page);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("chart.googleapis.com"), "{found:?}");
        let issues = verify_page(page, "");
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "EXTERNAL_DEPENDENCY"),
            "{issues:?}"
        );
    }

    #[test]
    fn ordinary_outbound_links_and_inline_svg_are_fine() {
        // `<a href="http…">` 是正常站外链接；内联 SVG、data: 图片都不算外部依赖。
        let page = r##"<html><body><a href="https://example.com/条款">用户条款</a>
<img src="data:image/svg+xml;base64,PHN2Zy8+" alt="图标">
<svg viewBox="0 0 8 8"><rect width="8" height="8" fill="#111"/></svg>
<button id="go">生成</button>
<script>function roll() { return 1; }
document.getElementById("go").onclick = roll;</script></body></html>"##;
        assert!(external_dependencies(page).is_empty());
        let issues = verify_page(page, "");
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "EXTERNAL_DEPENDENCY"),
            "{issues:?}"
        );
    }

    #[test]
    fn replacing_a_script_body_keeps_the_script_tag() {
        // 实测事故：机械修复算出来的是**脚本正文**，却被直接交给 `splice_script`
        // （它要的是完整的 `<script>…</script>` 块），于是开闭标签一起被吃掉 ——
        // 产出过一个 17918 字符、按钮 8 个、`<script>` 标签 0 个的页面：
        // 脚本内容还在，却被当成 HTML 正文渲染，自检报 `NO_SCRIPT`，按钮全不动。
        let html = "<html><body><button id=\"go\">生成</button><script>const a = 1;</script></body></html>";
        let replaced = crate::webgen::splice_script_body(html, "const merged = 2;");
        assert!(replaced.contains("<script>"), "{replaced}");
        assert!(replaced.contains("</script>"), "{replaced}");
        assert!(replaced.contains("const merged = 2;"), "{replaced}");
        assert!(!replaced.contains("const a = 1;"), "{replaced}");
        // 修完不该被判成"页面没有脚本"。
        let issues = verify_page(&replaced, "");
        assert!(
            !issues.iter().any(|issue| issue.code == "NO_SCRIPT"),
            "{issues:?}"
        );

        // 端到端串一遍：alert 机械修复 + 正文替换，页面仍然有脚本。
        let page = "<html><body><button id=\"go\">生成</button><div id=\"out\"></div>\n<script>\nfunction roll() { alert(\"点到了\"); return 1; }\ndocument.getElementById(\"go\").onclick = roll;\n</script></body></html>";
        let merged = crate::webgen::merged_script(page);
        let fixed = fix_placeholder_alerts(&merged).expect("应当改写 alert");
        let spliced = crate::webgen::splice_script_body(page, &fixed);
        assert!(spliced.contains("<script>"), "{spliced}");
        let after = verify_page(&spliced, "");
        assert!(
            !after.iter().any(|issue| issue.code == "NO_SCRIPT"),
            "{after:?}"
        );
    }

    #[test]
    fn turns_an_alert_into_an_in_page_notice() {
        // 实测原样（端到端第 8 轮）：整版只剩 2 处 alert，却没有任何可行的自动修复路线
        // （补写治不了、整段重写装不下，重写回来的是残脚本只能拒收）。这条机械替换把
        // `alert(x)` 变成 `inlineAlert(x)` 并追加实现 —— 语义等价（alert 返回 undefined）。
        let script = "function check(v) { if (!v) { alert(\"请输入你的困境\"); return false; } return true; }\nconst label = \"alert(不是调用)\";\n";
        let fixed = fix_placeholder_alerts(script).expect("应当改写");
        assert!(fixed.contains("inlineAlert(\"请输入你的困境\")"), "{fixed}");
        assert!(fixed.contains("function inlineAlert(message)"), "{fixed}");
        // 字符串里的 `alert(` 不能被动到。
        assert!(fixed.contains("\"alert(不是调用)\""), "{fixed}");
        // 幂等：再跑一次不该重复追加实现。
        assert!(fix_placeholder_alerts(&fixed).is_none());

        let page = format!(
            "<html><body><button id=\"go\">生成</button>\n<script>\n{fixed}\nconst ok = check(\"\");\ndocument.getElementById(\"go\").onclick = function () {{ void ok; }};\n</script></body></html>"
        );
        let issues = verify_page(&page, "");
        assert!(
            !issues.iter().any(|issue| issue.code == "PLACEHOLDER_ALERT"),
            "{issues:?}"
        );
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "UNDEFINED_REFERENCE"),
            "补的提示条实现不能被当成悬空调用：{issues:?}"
        );
    }

    #[test]
    fn leaves_member_calls_and_other_names_alone() {
        // `obj.alert(...)`、`myalert(...)` 不是原生弹窗，不能改。
        let script = "obj.alert(\"x\");\nmyalert(\"y\");\nwindow.alert(\"z\");\n";
        let fixed = fix_placeholder_alerts(script).expect("window.alert 应当被改写");
        assert!(fixed.contains("obj.alert(\"x\")"), "{fixed}");
        assert!(fixed.contains("myalert(\"y\")"), "{fixed}");
        assert!(fixed.contains("window.inlineAlert(\"z\")"), "{fixed}");
    }

    #[test]
    fn catches_an_invalid_unicode_property_name() {
        // 实测原样（端到端第 5 轮）：模型想放宽中文校验，写了 `\p{Han}`。
        // `Han` 是 script 名，必须写 `\p{Script=Han}` —— 写错直接抛
        // `SyntaxError: Invalid property name in character class`，整段脚本一行不执行。
        let script = "const ok = /^[a-zA-Z0-9\\s\\p{Han}\\u3400-\\u4DBF\\u4E00-\\u9FFF\\uFA1E-\\uFA29]+$/u.test(value);\n";
        let page = format!(
            "<html><body><button id=\"go\">生成</button>\n<script>\nconst value = \"\";\n{script}document.getElementById(\"go\").onclick = function () {{ void ok; }};\n</script></body></html>"
        );
        let issues = verify_page(&page, "");
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "REGEX_PROPERTY_INVALID"),
            "{issues:?}"
        );
        assert!(has_critical(&issues));

        // 机械修复：`\p{Han}` → `\p{Script=Han}`，修完自检不再报。
        let fixed_script = fix_regex_properties(script).expect("应当改写");
        assert!(fixed_script.contains("\\p{Script=Han}"), "{fixed_script}");
        let fixed_page = page.replace(script, &fixed_script);
        let after = verify_page(&fixed_page, "");
        assert!(
            !after
                .iter()
                .any(|issue| issue.code == "REGEX_PROPERTY_INVALID"),
            "{after:?}"
        );
    }

    #[test]
    fn accepts_legitimate_unicode_property_names() {
        // 合法的写法一律放过：`Script=` / `General_Category=` 形式、广义类别缩写与长名、
        // 二元属性名。
        for pattern in [
            "\\p{Script=Han}",
            "\\p{sc=Latin}",
            "\\p{General_Category=Letter}",
            "\\p{gc=Nd}",
            "\\p{L}",
            "\\p{Letter}",
            "\\p{Alphabetic}",
            "\\p{White_Space}",
            "\\P{L}",
        ] {
            let script = format!("const re = /^[{pattern}]+$/u;\n");
            assert!(
                invalid_regex_properties(&script).is_empty(),
                "{pattern} 不该被判成非法"
            );
            assert!(fix_regex_properties(&script).is_none(), "{pattern}");
        }
    }

    #[test]
    fn widens_a_word_class_so_chinese_input_passes() {
        // 实测原样：生成页把输入校验写成 `/^[^\w\s]+$/.test(value)`，于是「要不要辞职去旅行」
        // 被判成"全是非法字符"，用户输入什么都点不出结果。
        let script = "if (/^[^\\w\\s]+$/.test(input)) { showToast(\"请输入具体的事情\"); return; }\nconst ok = 1;\n";
        let fixed = fix_narrow_word_class(script).expect("应当改写");
        assert!(fixed.contains("[^\\u4e00-\\u9fff\\w"), "{fixed}");
        // 改完之后自检不再报这条。
        let page = format!(
            "<html><body><button id=\"go\">生成</button>\n<script>\nconst input = \"\";\n{fixed}\ndocument.getElementById(\"go\").onclick = function () {{ void input; }};\n</script></body></html>"
        );
        let issues = verify_page(&page, "");
        assert!(
            !issues.iter().any(|issue| issue.code == "NARROW_WORD_CLASS"),
            "{issues:?}"
        );
    }

    #[test]
    fn leaves_an_already_chinese_aware_check_alone() {
        let script = "if (/^[^\\u4e00-\\u9fff\\w\\s]+$/.test(v)) { return; }\n";
        assert!(fix_narrow_word_class(script).is_none());
        // 不碰校验调用的行。
        let unrelated = "const s = \"[^\\\\w\";\n";
        assert!(fix_narrow_word_class(unrelated).is_none());
    }

    #[test]
    fn an_unrelated_alert_does_not_cancel_the_append_route() {
        // 实测原样（端到端第 7 轮）：缺 `qrcode` / `randomPick` 的同时还有一处 alert。
        // 那条 alert 把补写路线掐掉，于是只能走整段重写；重写被输出上限截断、拒收，
        // 最后交付的还是点不动的版本。补写至少能让页面动起来。
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["a", "b"];
function start() {
    alert(randomPick(words));
    document.getElementById("out").textContent = qrcode("x");
}
document.getElementById("go").onclick = start;
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            issues.iter().any(|issue| issue.code == "PLACEHOLDER_ALERT"),
            "{issues:?}"
        );
        assert_eq!(function_appendix_plan(&issues), Some(false), "{issues:?}");

        // 但脚本结构本身坏了就不能补写 —— 补上去的东西会一起被语法错误带走。
        let broken = r#"<html><body><button id="go">生成</button>
<script>function a() { if (true) { b(); }</script></body></html>"#;
        assert_eq!(function_appendix_plan(&verify_page(broken, "")), None);
    }

    #[test]
    fn detects_a_fence_spliced_into_the_script() {
        // 实测原样（端到端第 4 轮，脚本第 58–59 行）：上一轮被截断在半句，续写的下一轮
        // 从"开一个代码块"写起，围栏就缝进了脚本中间。浏览器报
        // `SyntaxError: Unexpected template string`，整段脚本一行都不执行。
        let page = r#"<html><body><button id="go">生成选项</button><div id="out"></div>
<script>
const words = ["a", "b"];
function pick(list) { return list[Math.floor(Math.random() * list.length)]; }
function generateOptions() {
    return pick(words) + pick(words);
```});
document.getElementById("go").onclick = function () {
    document.getElementById("out").textContent = generateOptions();
};
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            issues.iter().any(|issue| issue.code == "SCRIPT_FENCE"),
            "围栏必须被拦下：{issues:?}"
        );
        assert!(
            issues.iter().any(|issue| issue.code == "UNCLOSED_TEMPLATE"),
            "围栏带来的奇数反引号也要被拦下：{issues:?}"
        );
    }

    #[test]
    fn removes_a_fence_and_keeps_the_code_glued_to_it() {
        // 实测原样：`push(` 那一句被截断在半句，下一行是 ```` ```}); ```` —— 那句 `});`
        // 是上一句的收尾，剥围栏时必须把代码留下，否则配平反而被破坏。
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["a", "b"];
function pick(list) { return list[Math.floor(Math.random() * list.length)]; }
function generateOptions() {
    const out = [];
    out.push(replace(pick(words), { prop: pick(words), con
```});
    return out;
}
document.getElementById("go").onclick = function () {
    document.getElementById("out").textContent = generateOptions().join("");
};
</script></body></html>"#;
        let fixed = crate::webgen::strip_script_fences(page).expect("应当识别出围栏并修复");
        assert!(!fixed
            .lines()
            .any(|line| line.trim_start().starts_with("```")));
        assert!(fixed.contains("});"), "围栏后面粘着的代码要保留：{fixed}");
        assert!(fixed.contains("function generateOptions()"), "{fixed}");
        assert!(
            fixed.contains("con\n"),
            "截断的半句要保持原样，不能凭空补全：{fixed}"
        );

        let script = crate::webgen::merged_script(&fixed);
        assert_eq!(
            crate::webgen::dom::stray_backticks(&script) % 2,
            0,
            "{script}"
        );
        let issues = verify_page(&fixed, "");
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "SCRIPT_FENCE" || issue.code == "UNCLOSED_TEMPLATE"),
            "反引号问题应当已经消失：{issues:?}"
        );

        // 关键：真实产物剥掉围栏后**括号其实少一个**（截断在半句），只是反引号把它
        // 伪装成了"配平正常"。修复的价值是把这种"静默的语法错"变成**检查看得见的错**
        // —— 要么直接报 SCRIPT_UNBALANCED 走修复，要么本来就配平。
        let leftover = crate::webgen::dom::stray_backticks(&script);
        assert_eq!(leftover, 0, "这段脚本本来就不含模板字符串：{script}");
        if delimiter_problem(&script).is_some() {
            assert!(
                issues.iter().any(|issue| issue.code == "SCRIPT_UNBALANCED"),
                "括号对不上时必须报出来，不能再静默放行：{issues:?}"
            );
        }
    }

    #[test]
    fn an_unclosed_backtick_is_never_silently_accepted() {
        // 该守住的不变量只有一条：整段脚本会因为反引号而报 SyntaxError，那就**必须**
        // 有规则拦下它，无论走的是配平检查（报「` 字符串一直没闭合」）、围栏检查，
        // 还是新的奇偶性检查。缺了这道网，坏页面就会以"自检通过"的名义交付出去。
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
function a() { return [
```});
document.getElementById("go").onclick = a;
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            issues.iter().any(|issue| matches!(
                issue.code,
                "SCRIPT_FENCE" | "SCRIPT_UNBALANCED" | "UNCLOSED_TEMPLATE"
            )),
            "未闭合的反引号必须被拦下：{issues:?}"
        );
        assert!(has_critical(&issues));
    }

    #[test]
    fn catches_a_fence_left_in_a_page_that_never_went_through_the_fix() {
        // 兜底：万一机械修复没跑到（例如某条旁路），自检也必须把它拦成致命问题 ——
        // 因为它等价于"整页没有脚本"。
        let page = r#"<html><body><button id="go">生成</button>
<script>
function roll() { return 1; }
```});
document.getElementById("go").onclick = roll;
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            issues.iter().any(|issue| issue.code == "SCRIPT_FENCE"),
            "{issues:?}"
        );
        assert!(has_critical(&issues));
    }

    #[test]
    fn leaves_a_page_without_fences_untouched() {
        let page = r#"<html><body><button id="go">生成</button>
<script>const label = `共 ${1 + 1} 条`;
document.getElementById("go").onclick = function () { document.title = label; };
</script></body></html>"#;
        assert!(
            crate::webgen::strip_script_fences(page).is_none(),
            "没有围栏就不该改"
        );
        // 成对的模板字符串（2 个反引号）不算未闭合。
        let issues = verify_page(page, "");
        assert!(
            !issues.iter().any(|issue| issue.code == "UNCLOSED_TEMPLATE"),
            "成对反引号被误判了：{issues:?}"
        );
    }

    #[test]
    fn counts_backticks_outside_strings_and_comments() {
        // 字符串里的反引号不算模板字符串；注释里的也不算。
        let script = "const a = \"a ` b\";\n// 一个 ` 反引号\n/* 另一个 ` */\nconst c = `x`;\n";
        assert_eq!(crate::webgen::dom::stray_backticks(script), 2);
    }

    #[test]
    fn accepts_a_real_page() {
        // 生成函数真的从词库数组取值 —— 不该报任何严重问题。
        let page = r#"<html><head><title>名字生成器</title></head><body>
<button id="go">生成名字</button><button id="copy">复制</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风", "长天", "花开", "岁月"];
function pick(list) { return list[Math.floor(Math.random() * list.length)]; }
function generateName() { return pick(words) + pick(words); }
document.getElementById("go").onclick = function () {
    document.getElementById("out").textContent = generateName();
};
document.getElementById("copy").onclick = function () {
    navigator.clipboard.writeText(document.getElementById("out").textContent);
};
</script></body></html>"#;
        let issues = verify_page(page, "");
        let critical: Vec<_> = issues
            .iter()
            .filter(|issue| issue.severity == Severity::Critical)
            .collect();
        assert!(critical.is_empty(), "正常页面不该被拦下：{critical:?}");
    }

    #[test]
    fn accepts_data_passed_through_helper_parameters() {
        // 抽词辅助函数把数组当参数收，函数体里看不到数组名 —— 不能因此误判成"没用数据"。
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
function roll() { return pickOne(words); }
document.getElementById("go").onclick = roll;
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "GENERATOR_IGNORES_DATA"),
            "辅助函数收数组参数不该被误判：{issues:?}"
        );
    }

    #[test]
    fn catches_a_script_that_declares_everything_twice() {
        // 续写写完不停手，把整段状态与逻辑又写了一遍 —— 实测的真实故障。
        let repeated = "let currentScene = '网名';\nlet currentStyle = '古风';\nlet historyList = [];\nlet favorites = [];\nfunction render() { document.getElementById('out').textContent = currentScene; }\n";
        let page = format!(
            "<html><body><button id=\"go\">生成</button><div id=\"out\"></div>\n<script>\n{repeated}{repeated}document.getElementById('go').onclick = render;\n</script></body></html>"
        );
        let issues = verify_page(&page, "");
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "DUPLICATE_DECLARATION"),
            "必须识别出重复声明：{issues:?}"
        );
        assert!(has_critical(&issues));
    }

    #[test]
    fn ignores_legitimate_block_scope_shadowing() {
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
function roll() {
  if (Math.random() > 0.5) { const chosenName = pickOne(words); return chosenName; }
  { const chosenName = pickOne(words); return chosenName; }
}
document.getElementById("go").onclick = roll;
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "DUPLICATE_DECLARATION"),
            "块级作用域里同名不该误报：{issues:?}"
        );
    }

    #[test]
    fn ignores_repeated_for_loop_counters() {
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
function roll() {
  let out = "";
  for (let index = 0; index < 2; index++) { out += words[index]; }
  for (let index = 0; index < 2; index++) { out += words[index]; }
  return out;
}
document.getElementById("go").onclick = roll;
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "DUPLICATE_DECLARATION"),
            "循环计数器每次都是新作用域，不该误报：{issues:?}"
        );
    }

    #[test]
    fn ignores_same_local_names_in_different_functions() {
        // 不同函数里的同名局部变量互不相干 —— 这三个名字（pool/name/char）实测曾被误判成
        // "重复声明"，害得修复轮次一直在改本来正确的代码。
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
function first() { let pool = words; let name = pool[0]; let char = name[0]; return char; }
function second() { let pool = words; let name = pool[1]; let char = name[0]; return char; }
function third() { let pool = words; let name = pool[2]; return name; }
function roll() { return first() + second() + third(); }
document.getElementById("go").onclick = roll;
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "DUPLICATE_DECLARATION"),
            "不同函数里的同名局部变量不该误报：{issues:?}"
        );
    }

    #[test]
    fn catches_a_script_truncated_mid_statement() {
        // 实测原样：单轮输出停在 `function showToast(message`，续写直接把 `}` 接了上去。
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
function showToast(message}
function roll() { return words[0]; }
document.getElementById("go").onclick = roll;
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            issues.iter().any(|issue| issue.code == "SCRIPT_UNBALANCED"),
            "必须识别出括号不配平：{issues:?}"
        );
        assert!(has_critical(&issues));
    }

    #[test]
    fn ignores_quotes_inside_regular_expressions() {
        // `replace(/["']/g, "")` 里的引号是正则内容，不能当成字符串开头。
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
function clean(text) { return text.replace(/["']/g, ""); }
function roll() { return clean(pickOne(words)); }
document.getElementById("go").onclick = roll;
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues.iter().any(|issue| issue.code == "SCRIPT_UNBALANCED"),
            "正则里的引号不该误报：{issues:?}"
        );
    }

    #[test]
    fn reports_feature_coverage_gaps() {
        let spec = "## 功能需求\n1. [P0] 生成名字 —— 随机产生\n2. [P0] 收藏列表 —— 保存\n3. [P0] 关键词筛选 —— 过滤\n4. [P0] 主题切换 —— 深浅色\n5. [P0] 一键复制 —— 复制到剪贴板";
        let page = "<html><body><div>只有一个空页面</div></body></html>";
        let issues = verify_page(page, spec);
        assert!(
            issues.iter().any(|issue| issue.code == "FEATURE_MISSING"),
            "{issues:?}"
        );
    }

    #[test]
    fn accepts_a_feature_whose_ui_copy_is_present() {
        // 需求里的名字是文档内部标签（"生成动作"），页面上写的是按钮文案 —— 功能在，不该报缺失。
        let spec = "## 功能需求\n1. [P0] 场景选择 —— 提供6个场景，单选，默认网名\n2. [P0] 风格选择 —— 单选，默认古风\n3. [P0] 生成动作 —— 主按钮文案：生成名字 / 再摇一批\n4. [P0] 单条结果操作 —— 点击名字本体即复制\n5. [P0] 收藏 —— 星标切换，写入收藏夹\n6. [P0] 清空数据 —— 二次确认";
        let page = r#"<html><body>
<select id="scene"><option>网名</option></select>
<select id="style"><option>古风</option></select>
<button id="go">生成名字</button><button>再摇一批</button>
<button>复制</button><button>收藏</button><button>清空数据</button>
</body></html>"#;
        let issues = verify_page(page, spec);
        assert!(
            !issues.iter().any(|issue| issue.code == "FEATURE_MISSING"),
            "界面文案已经落地，不该报缺失：{issues:?}"
        );
    }

    #[test]
    fn extracts_top_level_arrays() {
        let script = "const a = [1]; let b = {x:1}; var c = [2]; const d = 'x';";
        assert_eq!(
            top_level_arrays(script),
            vec!["a".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn matches_function_bodies_with_nested_braces() {
        let script = "function outer() { if (true) { return 1; } return 2; } function tail() {}";
        let bodies = function_bodies(script);
        assert_eq!(bodies.len(), 2);
        assert!(bodies[0].1.contains("return 2"));
    }

    #[test]
    fn window_scan_is_multibyte_safe() {
        // 混合中文的脚本不能因为按字节切窗而 panic。
        let script =
            "function f(){ const 名字 = Math.random(); return String.fromCharCode(名字); }";
        assert!(appears_nearby(script, "fromcharcode", "math.random", 400));
    }

    #[test]
    fn repair_instruction_carries_evidence_and_target() {
        let issues = verify_page(STUB_PAGE, "");
        let text = repair_instruction(&issues);
        assert!(text.contains("FAKE_NOISE"));
        assert!(text.contains("必须改成"));
    }

    // ---------- 标识符级检查（"看着完整、点了没反应"） ----------

    /// 本轮实测的原样复刻：括号配平、语法也过得去，但调用了从没定义过的函数。
    const DEAD_PAGE: &str = r#"<html><body>
<button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风", "白川"];
function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
function generateSingleName(length, style) {
    return generateGeneralName(length, style);
}
function generatePairName(length) {
    const first = generateBaseName(length);
    return first + "-" + calculateRank(first);
}
document.getElementById("go").onclick = () => { generateSingleName(3, "古风"); };
</script></body></html>"#;

    #[test]
    fn catches_calls_to_functions_that_were_never_defined() {
        let issues = verify_page(DEAD_PAGE, "");
        let found = issues
            .iter()
            .find(|issue| issue.code == "UNDEFINED_REFERENCE")
            .unwrap_or_else(|| panic!("必须识别出悬空调用：{issues:?}"));
        assert_eq!(found.severity, Severity::Critical);
        // 三个悬空名字都要点到，且不能把已定义/内置的名字算进来。
        assert!(
            found.detail.contains("generateGeneralName"),
            "{}",
            found.detail
        );
        assert!(
            found.detail.contains("generateBaseName"),
            "{}",
            found.detail
        );
        assert!(found.detail.contains("calculateRank"), "{}", found.detail);
        assert!(!found.detail.contains("pickOne"), "{}", found.detail);
        assert!(!found.detail.contains("Math"), "{}", found.detail);
    }

    #[test]
    fn accepts_a_page_whose_calls_are_all_defined() {
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
const shuffle = (list) => list.slice().sort(() => Math.random() - 0.5);
function roll() { return pickOne(shuffle(words)); }
document.getElementById("go").onclick = () => {
    document.getElementById("out").textContent = roll();
};
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "UNDEFINED_REFERENCE"),
            "不该误报：{issues:?}"
        );
    }

    #[test]
    fn ignores_calls_written_inside_strings_and_comments() {
        // 界面文案里提到函数名是正常的，不能当成真实调用。
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
// 下一步要接上 fetchRemoteNamesFromServer()，暂时不需要
const hint = "点击后会调用 remoteLookupFromServer() 拉取数据";
function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
document.getElementById("go").onclick = () => {
    document.getElementById("out").textContent = pickOne(words);
};
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "UNDEFINED_REFERENCE"),
            "字符串/注释里的名字不该误报：{issues:?}"
        );
    }

    #[test]
    fn allows_the_same_local_helper_in_two_different_functions() {
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
function a() { function helper() { return 1; } return helper(); }
function b() { function helper() { return 2; } return helper(); }
document.getElementById("go").onclick = () => { document.getElementById("out").textContent = String(a() + b()); };
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "DUPLICATE_FUNCTION"),
            "不同作用域的同名局部函数是合法的：{issues:?}"
        );
    }

    /// 模型续写时把整份脚本重开一篇：前一份完整，后一份写一半就断了。
    const RESTARTED_PAGE: &str = r#"<html><body>
<button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
function show(text) { document.getElementById("out").textContent = text; }
function generateNames() { return pickOne(words); }
document.getElementById("go").onclick = () => { show(generateNames()); };
function generateNames() {
    return generateHalfBaked(3);
}
function generateHalfBaked(length) {
    const first = generateMissingHelper(length);
    return first;
}
</script></body></html>"#;

    #[test]
    fn catches_two_generations_of_the_same_function() {
        let issues = verify_page(RESTARTED_PAGE, "");
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "DUPLICATE_FUNCTION"),
            "必须识别出重开一篇留下的同名函数：{issues:?}"
        );
    }

    #[test]
    fn restore_rolls_back_to_the_complete_earlier_generation() {
        let fixed = salvage_truncated_script(RESTARTED_PAGE).expect("应当能退回完整的那一份");
        // 退回去之后：后一代的残品连同它的悬空调用一起消失。
        assert!(!fixed.contains("generateHalfBaked"), "{fixed}");
        let script = crate::webgen::dom::merged_script(&fixed);
        assert!(
            undefined_calls(&script).is_empty(),
            "{:?}",
            undefined_calls(&script)
        );
        let issues = verify_page(&fixed, "");
        assert!(
            !issues
                .iter()
                .any(|issue| issue.code == "DUPLICATE_FUNCTION"
                    || issue.code == "UNDEFINED_REFERENCE"),
            "退回后应当是自洽的：{issues:?}"
        );
        // 视觉部分一个字节都不该动。
        assert!(fixed.contains("雨落") && fixed.contains("<button id=\"go\">生成</button>"));
    }

    /// 两个重启边界时，应当选**最早**的那一个：切在第二个边界虽然更长，却会把"重写那一份"
    /// 的前半段混进来，两代实现搅在一起。
    #[test]
    fn restore_prefers_the_earliest_restart_boundary() {
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
function show(text) { document.getElementById("out").textContent = text; }
function generateNames() { return pickOne(words); }
document.getElementById("go").onclick = () => { show(generateNames()); };
function show(text) { document.getElementById("out").innerHTML = text; }
function generateNames() { return generateBogus(3); }
</script></body></html>"#;
        let fixed = salvage_truncated_script(page).expect("应当能退回完整的那一代");
        assert!(!fixed.contains("generateBogus"), "{fixed}");
        // 只保留第一代的 show / generateNames 实现（第一代用 textContent）。
        assert!(fixed.contains("textContent = text"), "{fixed}");
        assert!(!fixed.contains("innerHTML = text"), "{fixed}");
        let script = crate::webgen::dom::merged_script(&fixed);
        assert!(duplicated_function_names(&script).is_empty(), "{fixed}");
    }

    #[test]
    fn guards_a_storage_read_that_can_return_null() {
        // 首次打开时 `history` 键还不存在，读到 null，`.split` 直接抛 TypeError。
        let script = "const items = localStorage.getItem('history').split(',').forEach(render);";
        assert_eq!(null_storage_reads(script).len(), 1);
        let fixed = fix_null_storage_reads(script).expect("应当能就地补兜底");
        assert!(
            fixed.contains("(localStorage.getItem('history') || '')"),
            "{fixed}"
        );

        // 已经兜过底的不动。
        assert!(
            fix_null_storage_reads("const a = (localStorage.getItem('h') || '').split(',');")
                .is_none()
        );
        // `JSON.parse(null)` 不抛，不该误报。
        assert!(
            fix_null_storage_reads("const a = JSON.parse(localStorage.getItem('h'));").is_none()
        );
        // 只是赋值也不该报。
        assert!(fix_null_storage_reads("const a = localStorage.getItem('h');").is_none());

        let page = format!("<html><body><div id=\"out\"></div><script>\n{script}\nfunction render(x) {{ document.getElementById(\"out\").textContent = x; }}\n</script></body></html>");
        let issues = verify_page(&page, "");
        assert!(
            issues.iter().any(|issue| issue.code == "NULL_STORAGE_READ"),
            "应当报出未兜底的存储读取：{issues:?}"
        );
    }

    #[test]
    fn catches_a_regex_that_rejects_every_chinese_input() {
        // `\w` 是 ASCII 意义上的，汉字不算 —— 这类判据会把整句中文输入判成非法。
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月"];
function go() {
    const text = document.getElementById("out").textContent;
    if (/^[^\w\s]+$/.test(text)) { return; }
    document.getElementById("out").textContent = words[0];
}
document.getElementById("go").onclick = () => { go(); };
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            issues.iter().any(|issue| issue.code == "NARROW_WORD_CLASS"),
            "应当报出用 \\w 校验中文的写法：{issues:?}"
        );
        // 正确写法（带 u 标志的 \p{L}）不该报。
        let ok = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月"];
function go() {
    const text = document.getElementById("out").textContent;
    if (/^[^\p{L}\p{N}\s]+$/u.test(text)) { return; }
    document.getElementById("out").textContent = words[0];
}
document.getElementById("go").onclick = () => { go(); };
</script></body></html>"#;
        assert!(
            !verify_page(ok, "")
                .iter()
                .any(|issue| issue.code == "NARROW_WORD_CLASS"),
            "带 u 标志的 \\p{{L}} 写法不该报"
        );
    }

    #[test]
    fn counts_inline_handlers_in_the_markup() {
        // 模型偏爱把事件写在 HTML 属性上（`<button onclick="go()">`），那也是真实绑定。
        // 早先只扫脚本，于是把明明绑好的按钮误报成"点下去不会有反应"，白跑一轮修复。
        let page = r#"<html><body>
<button onclick="go()">生成</button>
<div id="out"></div>
<script>
const words = ["雨落", "星月"];
function go() { document.getElementById("out").textContent = words[Math.floor(Math.random() * words.length)]; }
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues.iter().any(|issue| issue.code == "BUTTON_NO_HANDLER"),
            "内联 onclick 也是绑定：{issues:?}"
        );
    }

    #[test]
    fn rolls_back_past_a_generation_that_does_not_even_run() {
        // 后一代把脚本重开了，自己还带着重复的 `const pool` / `let history` ——
        // `node --check` 直接 SyntaxError，整段脚本一行都不执行。
        // 择优必须看出前一代是好的，别把同样跑不起来的一版挑回来。
        let page = r#"<html><body><button onclick="go()">生成</button><div id="out"></div>
<script>
const pool = ["雨落", "星月"];
let history = [];
function go() { document.getElementById("out").textContent = pool[Math.floor(Math.random() * pool.length)]; }
go();
const pool = ["雨落", "星月"];
let history = [];
function go() { return pool[0]; }
</script></body></html>"#;
        let (fixed, reason) = restore_script(page).expect("应当退回上一代");
        assert!(
            !fixed.contains("return pool[0]"),
            "后一代必须被丢掉（{reason}）：{fixed}"
        );
        assert!(fixed.contains("textContent"), "前一代的实现要留下：{fixed}");
    }

    #[test]
    fn picks_in_place_repair_for_missing_functions_and_bindings() {
        let missing_fn = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风", "白川"];
function roll() {
    const picked = words[Math.floor(Math.random() * words.length)];
    return decorate(picked);
}
document.getElementById("go").onclick = () => {
    document.getElementById("out").textContent = roll();
};
</script></body></html>"#;
        let issues = verify_page(missing_fn, "");
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "UNDEFINED_REFERENCE"),
            "{issues:?}"
        );
        assert_eq!(function_appendix_plan(&issues), Some(false));

        // 只有"按钮没绑事件"也要走就地补写 ——
        // 页面画得出来、按钮点了没反应，正是要治的那种"表面工作"。
        //
        // 注意这条现在是 **Critical**：整份脚本连一处事件绑定都找不到，页面上所有按钮全废，
        // 与"没有脚本"（NO_SCRIPT）是同一件事。过去它只是 Major，导致这种成品被直接交付。
        let unbound = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
function roll() { return pickOne(words); }
</script></body></html>"#;
        let issues = verify_page(unbound, "");
        assert!(has_critical(&issues), "按钮全无绑定应当被拦下：{issues:?}");
        assert!(
            has_functional_defect(&issues),
            "这属于功能性缺陷：{issues:?}"
        );
        assert!(
            functional_repair_worthwhile(&issues),
            "够格再跑一轮修复：{issues:?}"
        );
        assert_eq!(function_appendix_plan(&issues), Some(true));

        // 脚本被截断这种得整段重写，就地补写救不了。
        let unbalanced = r#"<html><body><button id="go">生成</button>
<script>const a = [1]; function b() { if (true) { return 1; }</script></body></html>"#;
        assert_eq!(function_appendix_plan(&verify_page(unbalanced, "")), None);
    }

    #[test]
    fn keeps_the_append_route_alive_when_a_pool_is_also_unused() {
        // 实测原样（端到端第 4 轮）：同时报 `UNDEFINED_REFERENCE`（缺 7 个函数）与
        // `UNUSED_DATA_POOL`（3 个数组没人读）。第二条严重问题一度把就地补写这条唯一可行
        // 的路线掐掉了，接着定点替换与整段重写都走不通，最后**交付了点了没反应的坏页面**。
        // 实际上缺的函数正是该去读那些数组的人，补写它们两个问题一起消失。
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const dailyQuestions = ["今天要不要出门", "要不要换工作"];
const votes = { a: 1 };
const history = [];
function start() {
    document.getElementById("out").textContent = randomPick(dailyQuestions);
    saveToHistory(pickOne(dailyQuestions));
}
document.getElementById("go").onclick = start;
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "UNDEFINED_REFERENCE"),
            "{issues:?}"
        );
        assert!(
            issues.iter().any(|issue| issue.code == "UNUSED_DATA_POOL"),
            "{issues:?}"
        );
        assert_eq!(
            function_appendix_plan(&issues),
            Some(false),
            "两条严重问题必须能共存，否则这条路线会被无谓掐掉：{issues:?}"
        );
    }

    #[test]
    fn restore_leaves_a_healthy_page_alone() {
        let page = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
document.getElementById("go").onclick = () => {
    document.getElementById("out").textContent = pickOne(words);
};
</script></body></html>"#;
        assert!(salvage_truncated_script(page).is_none(), "健康页面不该被动");
    }

    /// 本轮实测的原样复刻：模型把模板串写坏（多了一个没闭合的 `${`），
    /// 整个脚本无法解析；而括号栈几乎从不落空（整段包在 `DOMContentLoaded` 回调里），
    /// "退回栈空的行首"只能退到全文 20% 处。就地收口要能把它救成可运行的页面。
    const BROKEN_TEMPLATE_PAGE: &str = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
document.addEventListener("DOMContentLoaded", () => {
    const prefixes = ["墨", "云", "霜", "月", "青", "白"];
    const suffixes = ["衣", "舟", "鹤", "弦", "辞", "生"];
    function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
    function generateNames() {
        const out = [];
        for (let i = 0; i < 12; i++) {
            out.push(pickOne(prefixes) + pickOne(suffixes));
        }
        document.getElementById("out").textContent = out.join(" / ");
    }
    document.getElementById("go").onclick = generateNames;
    function exportFavorites(format) {
        const favorites = JSON.parse(localStorage.getItem("nlp:favorites")) || [];
        let content;
        if (format === "text") {
            content = favorites.map(f => f.name).join("\n");
        } else if (format === "csv") {
            content = favorites.map(f =>
                `${f.name},${f.name.split('').map(c => pinyin.get(c)[0]).join(''),${f.style}`)
            ).join("\n");
        }
        const blob = new Blob([content], { type: "text/plain" });
        document.body.appendChild(blob);
    }
});
</script></body></html>"#;

    #[test]
    fn closes_a_broken_tail_in_place_instead_of_giving_up() {
        // 先确认这份页面确实是坏的、而且"退回栈空"没用。
        let script = crate::webgen::dom::merged_script(BROKEN_TEMPLATE_PAGE);
        assert!(delimiter_problem(&script).is_some(), "这份页面本该是坏的");

        let fixed = salvage_truncated_script(BROKEN_TEMPLATE_PAGE).expect("必须能救回来");
        let fixed_script = crate::webgen::dom::merged_script(&fixed);
        assert!(
            delimiter_problem(&fixed_script).is_none(),
            "收口后必须括号配平：{:?}",
            delimiter_problem(&fixed_script)
        );
        // 关键：保住大部分代码，而不是退回 20% 处。
        assert!(
            fixed_script.chars().count() * 2 > script.chars().count(),
            "收口后保留了 {} 字，原文 {} 字，丢太多了",
            fixed_script.chars().count(),
            script.chars().count()
        );
        // 词库与生成逻辑都得还在。
        assert!(fixed.contains("\"墨\""), "{fixed}");
        assert!(fixed.contains("function generateNames"), "{fixed}");
        // 视觉部分一个字节都不动。
        assert!(fixed.contains("<button id=\"go\">生成</button>"));
        // 收口掉的只有坏掉的那一段。
        assert!(!fixed.contains("pinyin.get"), "损坏段应当被截去");
    }

    #[test]
    fn does_not_add_closers_to_a_healthy_script() {
        let healthy = r#"<html><body><button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月", "清风"];
document.addEventListener("DOMContentLoaded", () => {
    function pickOne(list) { return list[Math.floor(Math.random() * list.length)]; }
    document.getElementById("go").onclick = () => {
        document.getElementById("out").textContent = pickOne(words);
    };
});
</script></body></html>"#;
        assert!(
            close_truncated_tail(&crate::webgen::dom::merged_script(healthy)).is_none()
                || salvage_truncated_script(healthy).is_none()
        );
        assert!(salvage_truncated_script(healthy).is_none());
    }

    #[test]
    fn masks_strings_comments_and_regexes() {
        let code = mask_non_code(
            r#"const s = "name("; // call(here)
/* block(comment) */ const re = /abc\(/g; const t = `x()`;"#,
        );
        // 掩码后不应再残留这些被引号/注释/正则包住的名字。
        for needle in ["name(", "call(", "comment(", "abc("] {
            assert!(!code.contains(needle), "掩码残留 {needle}：{code}");
        }
        // 代码骨架必须还在。
        assert!(code.contains("const s ="));
        assert!(code.contains("const re ="));
    }

    #[test]
    fn keeps_template_interpolations_as_code() {
        // `${...}` 是代码，里面的调用必须能被查到（实测有 `${pinyin(name)}` 藏在导出逻辑里）。
        let page = r#"<html><body><button id="go">导出</button><div id="out"></div>
<script>
const rows = ["雨落", "星月", "清风"];
function exportCsv() {
    return rows.map((name) => `${name},${pinyinOf(name)}`).join("\n");
}
document.getElementById("go").onclick = () => { exportCsv(); };
</script></body></html>"#;
        let code = mask_non_code(r#"const a = `文字${pinyinOf(x)}文字`;"#);
        assert!(code.contains("pinyinOf"), "插值里的代码被误掩了：{code}");
        let issues = verify_page(page, "");
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == "UNDEFINED_REFERENCE"
                    && issue.detail.contains("pinyinOf")),
            "插值里的悬空调用必须被识别：{issues:?}"
        );
    }

    #[test]
    fn does_not_mistake_arguments_for_parameters() {
        // `pickOne(words)` 里的 words 是实参；它本来就已定义，但绝不能靠"实参"通道被算作定义。
        let code = mask_non_code("function roll() { return pickOne(undefinedName); }");
        let defined = defined_names(&code);
        assert!(defined.contains(&"roll".to_string()));
        assert!(!defined.contains(&"undefinedName".to_string()));
    }

    #[test]
    fn catches_a_word_bank_that_is_declared_but_never_read() {
        // 真实故障：需求给了 6 组风格词库，模型把其中几组照抄进页面，生成分支却没接上它们。
        let page = r#"<html><body>
<select id="style"><option value="古风">古风</option></select>
<button id="go">生成</button><div id="out"></div>
<script>
const ancientPrefixes = ["墨", "云", "霜"];
const ancientSuffixes = ["衣", "舟", "鹤"];
const unusedPool = ["摸", "摆", "躺"];
function generateNames() {
    const list = ancientPrefixes.concat(ancientSuffixes);
    return list[Math.floor(Math.random() * list.length)];
}
document.getElementById("go").onclick = () => { generateNames(); };
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            issues.iter().any(
                |issue| issue.code == "UNUSED_DATA_POOL" && issue.detail.contains("unusedPool")
            ),
            "必须报出从未被读取的词库：{issues:?}"
        );
        assert!(
            !issues.iter().any(|issue| issue.code == "UNUSED_DATA_POOL"
                && issue.detail.contains("ancientPrefixes")),
            "真正被用到的词库不能误报：{issues:?}"
        );
    }

    #[test]
    fn catches_scene_options_that_have_no_branch() {
        // 本次最隐蔽的一类"表面工作"：6 个场景选项，脚本里只写了 1–2 个分支，
        // 其余取值走不进任何逻辑，结果区渲染出 undefined。
        let page = r#"<html><body>
<select id="scene">
<option value="网名">网名</option>
<option value="游戏ID">游戏 ID</option>
<option value="店铺名">店铺名</option>
</select>
<button id="go">生成名字</button><div id="out"></div>
<script>
const shopPrefixes = ["墨", "云", "霜"];
const shopSuffixes = ["衣", "舟", "鹤"];
function generateNames() {
    const scene = document.getElementById("scene").value;
    if (scene === '店铺名') {
        return shopPrefixes[Math.floor(Math.random() * shopPrefixes.length)] + shopSuffixes[0];
    }
    return '';
}
document.getElementById("go").onclick = () => { generateNames(); };
</script></body></html>"#;
        let issues = verify_page(page, "");
        let issue = issues
            .iter()
            .find(|issue| issue.code == "OPTION_UNHANDLED")
            .unwrap_or_else(|| panic!("必须报出没有分支的场景选项：{issues:?}"));
        assert!(issue.detail.contains("网名"), "{}", issue.detail);
        assert!(issue.detail.contains("游戏ID"), "{}", issue.detail);
        assert!(
            !issue.detail.contains("店铺名"),
            "有分支的选项不能误报：{}",
            issue.detail
        );
    }

    #[test]
    fn does_not_count_a_data_pool_as_handled_options() {
        // 数组里把 6 个场景写全了，不等于生成逻辑认得这 6 个场景 —— 这正是本次漏报的原因。
        let page = r#"<html><body>
<select id="scene">
<option value="网名">网名</option>
<option value="游戏ID">游戏 ID</option>
<option value="宠物名">宠物名</option>
<option value="店铺名">店铺名</option>
</select>
<button id="go">生成名字</button><div id="out"></div>
<script>
const scenes = ["网名", "游戏ID", "宠物名", "店铺名"];
const shopPrefixes = ["墨", "云"];
function generateNames() {
    const scene = document.getElementById("scene").value;
    if (scene === '店铺名') {
        return shopPrefixes[Math.floor(Math.random() * shopPrefixes.length)];
    }
    return '';
}
document.getElementById("go").onclick = () => { generateNames(); };
</script></body></html>"#;
        let issues = verify_page(page, "");
        let issue = issues
            .iter()
            .find(|issue| issue.code == "OPTION_UNHANDLED")
            .unwrap_or_else(|| panic!("数据数组不该把选项洗白：{issues:?}"));
        assert!(issue.detail.contains("网名"), "{}", issue.detail);
        assert!(issue.detail.contains("游戏ID"), "{}", issue.detail);
        assert!(issue.detail.contains("宠物名"), "{}", issue.detail);
    }

    #[test]
    fn accepts_a_selector_that_is_wired_through_a_lookup_table() {
        // 查表写法（对象键 / 索引访问）必须算作"已处理"，否则会误报、把正确页面改坏。
        let page = r#"<html><body>
<select id="scene">
<option value="网名">网名</option>
<option value="店铺名">店铺名</option>
</select>
<button id="go">生成名字</button><div id="out"></div>
<script>
const pools = { '网名': ["风", "云"], '店铺名': ["墨", "云"] };
function generateNames() {
    const scene = document.getElementById("scene").value;
    const list = pools[scene];
    return list[Math.floor(Math.random() * list.length)];
}
document.getElementById("go").onclick = () => { generateNames(); };
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues.iter().any(|issue| issue.code == "OPTION_UNHANDLED"),
            "查表写法被误判成未处理：{issues:?}"
        );
    }

    #[test]
    fn ignores_numeric_options_when_checking_coverage() {
        // `<option value="2">2 个字</option>` 走 parseInt，脚本里不会出现 `'2'`；
        // 拿它做判据必然误报，所以只检查含中文的取值。
        let page = r#"<html><body>
<select id="length"><option value="2">2 个字</option><option value="4">4 个字</option></select>
<button id="go">生成</button><div id="out"></div>
<script>
const words = ["雨落", "星月"];
function generateNames() {
    const limit = parseInt(document.getElementById("length").value);
    return words.slice(0, limit).join("");
}
document.getElementById("go").onclick = () => { generateNames(); };
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues.iter().any(|issue| issue.code == "OPTION_UNHANDLED"),
            "数值型选项不该进覆盖检查：{issues:?}"
        );
    }

    #[test]
    fn weighs_a_dead_page_heavier_than_a_missing_feature() {
        // 分数越小越好。1 条悬空调用（点了没反应）必须比 2 条功能缺失（点得动、只是缺一块）
        // 更差 —— 否则回滚会挑错版本。
        let dead = vec![Issue {
            code: "UNDEFINED_REFERENCE",
            severity: Severity::Critical,
            detail: String::new(),
            fix: String::new(),
        }];
        let missing = vec![
            Issue {
                code: "UNUSED_DATA_POOL",
                severity: Severity::Critical,
                detail: String::new(),
                fix: String::new(),
            },
            Issue {
                code: "UNUSED_DATA_POOL",
                severity: Severity::Critical,
                detail: String::new(),
                fix: String::new(),
            },
        ];
        assert!(
            health(&missing) < health(&dead),
            "缺功能的那一版必须比整页点不动的那一版更优：missing={:?} dead={:?}",
            health(&missing),
            health(&dead)
        );
    }

    #[test]
    fn estimates_a_rewrite_that_would_not_fit_the_window() {
        // 真实用例里页面脚本 8827 字符，估算约 4200 token，超过 3928 的输出上限，
        // 必须被容量预检拦下 —— 否则模型交出的重写版必然少函数（实测少了 createCard）。
        let mut body = String::from("<html><body><button id=\"go\">生成</button>\n<script>\n");
        for index in 0..220 {
            body.push_str(&format!(
                "function filler{index}() {{ return \"这是第 {index} 段中文占位文案，用来估算重写长度\"; }}\n"
            ));
        }
        body.push_str("</script></body></html>");
        let estimate = script_token_estimate(&body);
        assert!(
            body.chars().count() > 8_000,
            "夹具规模要贴近真实脚本（{} 字符）",
            body.chars().count()
        );
        assert!(
            estimate > 3_928,
            "估算值 {estimate} 应超过输出上限 3928，才能拦住必然截断的重写"
        );
    }

    #[test]
    fn locates_named_function_spans() {
        let script = "const a = 1;\nfunction first(x) { return {x}; }\nfunction second() {\n  return `模板 ${first(1)} 里的 { 不算 `;\n}\n";
        let spans = function_spans(script);
        let names: Vec<&str> = spans.iter().map(|(name, _, _)| name.as_str()).collect();
        assert_eq!(names, vec!["first", "second"]);

        let chars: Vec<char> = script.chars().collect();
        for (name, start, end) in &spans {
            let slice: String = chars[*start..*end].iter().collect();
            assert!(
                slice.starts_with(&format!("function {name}")),
                "区间应恰好覆盖函数声明：{slice:?}"
            );
            assert!(slice.ends_with('}'), "区间应以大括号结束：{slice:?}");
        }
        // 模板串里的 `{` 不该把第二个函数的边界带偏。
        let (_, start, end) = spans[1].clone();
        let slice: String = chars[start..end].iter().collect();
        assert!(
            slice.contains("模板"),
            "第二个函数应当完整包含模板串：{slice:?}"
        );
    }

    #[test]
    fn only_offers_the_patch_route_for_unused_pools() {
        let unused = vec![Issue {
            code: "UNUSED_DATA_POOL",
            severity: Severity::Critical,
            detail: String::new(),
            fix: String::new(),
        }];
        let page = "<html><body><button id=\"go\">生成</button><script>\nconst prop = [\"用左手\"];\nfunction generate() { return 1; }\n</script></body></html>";
        let plan = function_patch_plan(page, &unused);
        assert_eq!(plan, Some(vec!["prop".to_string()]));

        // 有悬空调用时让位给就地补写 —— 那个更致命，先修。
        let worse = vec![Issue {
            code: "UNDEFINED_REFERENCE",
            severity: Severity::Critical,
            detail: String::new(),
            fix: String::new(),
        }];
        assert!(function_patch_plan(page, &worse).is_none());
    }

    /// 交付闸门加固：这些"看着齐全、其实不具备功能"的页面**必须**被拦下。
    ///
    /// 这一组测试对应的是用户最直接的不满 —— 生成的网页"按钮点了没反应、数据不参与计算"。
    /// 加固前它们能一路绿灯被交付：既没有语法错（不是 Critical），也没有可用的修复计划，
    /// 于是走到 `critical == 0 && plan.is_none()` 那个分支被直接放行。
    #[test]
    fn blocks_a_page_whose_buttons_are_all_inert() {
        // 三个按钮，一个事件绑定都没有 —— 页面画得出来，点下去毫无反应。
        let inert = r#"<html><body>
<header><h1>取名器</h1></header>
<main>
  <button id="gen">生成名字</button>
  <button id="copy">复制</button>
  <button id="fav">收藏</button>
  <div id="out"></div>
</main>
<script>
const pools = ["雨落", "星月", "清风", "白川"];
function roll() { return pools[Math.floor(Math.random() * pools.length)]; }
function copyText() { return null; }
function toggleFav() { return null; }
</script></body></html>"#;
        let issues = verify_page(inert, "");
        assert!(
            issues.iter().any(|issue| issue.code == "BUTTON_NO_HANDLER"),
            "三个按钮零绑定必须被抓到：{issues:?}"
        );
        assert!(has_critical(&issues), "必须拦住而不是放行：{issues:?}");
        assert!(
            functional_repair_worthwhile(&issues),
            "必须值得再跑一轮修复"
        );
        // 走"就地补写"：把绑定补进脚本末尾，而不是重写整页。
        assert!(function_appendix_plan(&issues).is_some());
    }

    /// 需求里的功能只写了空壳：函数都定义好了，但一个也没接到按钮上。
    #[test]
    fn blocks_a_page_full_of_shell_functions() {
        let shell = r#"<html><body><button id="go">开始</button><div id="out"></div>
<script>
const styles = ["古典", "现代", "二次元"];
function buildClassic() { return "古典名"; }
function buildModern() { return "现代名"; }
function buildAnime() { return "二次元名"; }
function stash() { return []; }
</script></body></html>"#;
        let issues = verify_page(shell, "");
        let dead = issues.iter().find(|issue| issue.code == "DEAD_FUNCTION");
        assert!(dead.is_some(), "成片空壳函数必须被抓到：{issues:?}");
        // 成片（≥3 个）时按严重问题处理，不再只是提示。
        assert_eq!(dead.unwrap().severity, Severity::Critical);
        assert!(functional_repair_worthwhile(&issues));
    }

    /// `DEAD_FUNCTION` 的已知盲区：函数名只要在 HTML 里出现过就当成"被调用了"。
    ///
    /// 这个盲区本身不影响交付闸门 —— 它只让"死函数"这一条**少报**，而同一份页面通常
    /// 还会撞上 `BUTTON_NO_HANDLER` 或 `UNUSED_DATA_POOL`，照样被拦下。这里把这个行为
    /// 钉住，避免以后有人误以为它能覆盖全部空壳场景。
    #[test]
    fn shell_functions_named_in_html_escape_the_dead_check_but_still_get_blocked() {
        // 函数名写进了 data-* 属性，于是 `check_dead_functions` 认为它们被"调用"过。
        let page = r#"<html><body>
<button id="go" data-action="generateClassic">古典</button>
<button id="copy" data-action="generateModern">现代</button>
<div id="out"></div>
<script>
function generateClassic() { return "古典名"; }
function generateModern() { return "现代名"; }
</script></body></html>"#;
        let issues = verify_page(page, "");
        assert!(
            !issues.iter().any(|issue| issue.code == "DEAD_FUNCTION"),
            "这正是那条已知盲区：{issues:?}"
        );
        // 但按钮的 data-action 只是属性，脚本里并没有把点击接上去 —— 闸门依然拦得住。
        assert!(
            issues.iter().any(|issue| issue.code == "BUTTON_NO_HANDLER"),
            "盲区之外仍要拦住：{issues:?}"
        );
        assert!(has_critical(&issues));
    }

    /// 只有零星一两条疑似问题时不该触发修复 —— 免得为误报白跑一分钟推理。
    #[test]
    fn stays_quiet_for_a_single_suspicious_shell_function() {
        let page = r#"<html><body><button id="go" onclick="start()">开始</button>
<script>
function start() { document.getElementById("out").textContent = "ok"; }
function helperNeverUsed() { return 1; }
</script></body></html>"#;
        let issues = verify_page(page, "");
        let dead: Vec<&Issue> = issues
            .iter()
            .filter(|issue| issue.code == "DEAD_FUNCTION")
            .collect();
        // 允许报一条（提示级），但绝不该因此触发严重的判定。
        assert!(dead.iter().all(|issue| issue.severity == Severity::Major));
        assert!(!has_critical(&issues), "{issues:?}");
        assert!(!functional_repair_worthwhile(&issues), "{issues:?}");
    }

    /// 退化产物要被拦住：自检面对空壳页没有问题可报，必须单独判。
    #[test]
    fn catches_a_page_rewritten_into_a_fragment() {
        let original = format!(
            "<html><head><style>{}</style></head><body>{}</body><script>{}</script></html>",
            "a".repeat(2000),
            "b".repeat(4000),
            "c".repeat(2000)
        );
        // 模型只回了一个片段当整页 —— 正是实测那次：18918 → 3006 字，body/script 全丢。
        let fragment = format!("<html><head><style>{}</style>", "a".repeat(2900));
        let reason = looks_degraded(&original, &fragment).expect("必须判定为退化");
        assert!(
            reason.contains("<body") || reason.contains("不足"),
            "{reason}"
        );
    }

    /// 结构丢失单独也要能判出来（哪怕体量没骤降）。
    #[test]
    fn catches_a_page_that_lost_its_script() {
        let original = format!(
            "<html><body><p>{}</p></body><script>init()</script></html>",
            "x".repeat(1000)
        );
        let stripped = format!("<html><body><p>{}</p></body></html>", "x".repeat(1000));
        let reason = looks_degraded(&original, &stripped).expect("脚本被删必须判定为退化");
        assert!(reason.contains("<script"), "{reason}");
    }

    /// 正常的局部修改不能被误判 —— 少几个字、多几个字都该放行。
    #[test]
    fn stays_quiet_for_a_normal_edit() {
        let original = format!(
            "<html><body><button id=\"a\">生成</button>{}</body></html>",
            "x".repeat(1000)
        );
        let edited = format!(
            "<html><body><button id=\"a\" style=\"color:red\">再摇一次</button>{}</body></html>",
            "x".repeat(1000)
        );
        assert!(looks_degraded(&original, &edited).is_none());
    }

    /// 用户明确要求删掉一大块内容时不该拦（放宽到 60% 就是为这种情况留的余地）。
    #[test]
    fn allows_a_reasonable_shrink() {
        let original = format!("<html><body>{}</body></html>", "x".repeat(1000));
        let shrunk = format!("<html><body>{}</body></html>", "x".repeat(700));
        assert!(
            looks_degraded(&original, &shrunk).is_none(),
            "缩减到 70% 该放行"
        );
    }

    /// 功能性缺陷的归类要准确：语法错不算"功能性"，缺函数/没绑定才算。
    #[test]
    fn classifies_functional_defects_apart_from_syntax_errors() {
        let syntax = Issue {
            code: "SCRIPT_UNBALANCED",
            severity: Severity::Critical,
            detail: String::new(),
            fix: String::new(),
        };
        assert!(!is_functional_defect(&syntax));

        let inert = Issue {
            code: "BUTTON_NO_HANDLER",
            severity: Severity::Critical,
            detail: String::new(),
            fix: String::new(),
        };
        assert!(is_functional_defect(&inert));

        let dangling = Issue {
            code: "UNDEFINED_REFERENCE",
            severity: Severity::Critical,
            detail: String::new(),
            fix: String::new(),
        };
        assert!(is_functional_defect(&dangling));
    }
}
