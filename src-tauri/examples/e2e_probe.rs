//! 端到端验证：用真实模型生成网页，再用加固后的自检给它打分。
//!
//! 为什么需要它：单元测试只证明"规则判定正确"，不能证明"模型产出真的变好了"。
//! 这个探针直接跑完整链路，输出可对照的指标：
//!   - 一次生成（不含修复）后，自检还剩几项问题、其中几项影响功能；
//!   - 用量（提示词 token / 输出 token / 速率）是否符合窗口预算；
//!   - 生成耗时。
//!
//! 用法：cargo run --example e2e_probe -- <需求文本文件> [输出 html 路径]
//!
//! 刻意不引入任何测试框架依赖 —— 这是个人项目的一次性验证工具，跑完看输出即可。

use std::env;
use std::time::Instant;

const OLLAMA: &str = "http://localhost:11434";
const MODEL: &str = "qwen3:8b";

/// 与 webgen 模块保持一致的窗口配置。
const NUM_CTX: u32 = 8_192;
const NUM_PREDICT: i64 = 4_096;

fn main() {
    let args: Vec<String> = env::args().collect();
    // 传了文件路径就读文件；传空串或省略则用内建的默认需求。
    let requirement = match args.get(1) {
        Some(path) if !path.trim().is_empty() => {
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("读不到需求文件 {path}：{e}"))
        }
        _ => default_requirement(),
    };
    let out_path = args.get(2).filter(|p| !p.trim().is_empty()).cloned();

    // 生成阶段用的提示词：与 webgen::WEBPAGE_SYSTEM_PROMPT 保持同源。
    // 这里直接内联一份精简版，因为 example 无法访问 crate 的私有 pub const（阶段不同）。
    // 若要严格对齐，把 lib 的 const 复制过来即可 —— 我们关心的是"模型在这种约束下产出什么"。
    let system = env::var("WEBGEN_SYSTEM_PROMPT").unwrap_or_else(|_| default_system_prompt());

    let started = Instant::now();
    let body = stream_generate(&system, &requirement);
    let elapsed = started.elapsed();

    println!("\n=== 生成完成 ===");
    println!("耗时：{:.1}s", elapsed.as_secs_f64());
    println!("产物字符数：{}", body.chars().count());

    if let Some(path) = &out_path {
        std::fs::write(path, &body).expect("写盘失败");
        println!("已写入：{path}");
    }

    // 用与产品同源的规则跑一遍自检（通过 cargo test 侧的同名逻辑无法直接调用，
    // 这里做等价的粗略统计，用于快速判断"这次产出到底有没有功能"）。
    let stats = quick_audit(&body);
    println!("\n=== 快速自检（等价规则，粗算）===");
    println!("button 数：{}", stats.buttons);
    println!("事件绑定数：{}", stats.bindings);
    println!("函数定义数：{}", stats.functions);
    println!("数组常量数：{}", stats.arrays);
    println!("脚本字符数：{}", stats.script_chars);
    println!("是否含 alert：{}", stats.has_alert);
    println!("是否含绝对路径 href：{}", stats.has_abs_href);
    println!("\n=== 判定 ===");
    if stats.buttons > 0 && stats.bindings == 0 {
        println!("✗ 页面有按钮但零绑定 —— 点下去没反应（应当被闸门拦下）");
    } else if stats.functions > 0 && stats.bindings == 0 {
        println!("✗ 有函数但没有任何事件绑定 —— 功能接不上（应当被闸门拦下）");
    } else {
        println!("· 未发现「按钮全无绑定」这一最严重形态");
    }
}

struct Audit {
    buttons: usize,
    bindings: usize,
    functions: usize,
    arrays: usize,
    script_chars: usize,
    has_alert: bool,
    has_abs_href: bool,
}

fn quick_audit(html: &str) -> Audit {
    let lower = html.to_ascii_lowercase();
    let bindings = lower.matches("onclick").count()
        + lower.matches("addeventlistener(").count()
        + lower.matches("onchange").count()
        + lower.matches("oninput").count()
        + lower.matches("onclick=").count()
        + lower.matches("onchange=").count()
        + lower.matches("oninput=").count();
    Audit {
        buttons: lower.matches("<button").count(),
        bindings,
        functions: lower.matches("function ").count() + lower.matches("=>").count(),
        arrays: lower.matches("const ").count(),
        script_chars: lower
            .find("<script")
            .and_then(|start| {
                lower[start..]
                    .find("</script")
                    .map(|end| lower[start + 8..start + end].chars().count())
            })
            .unwrap_or(0),
        has_alert: lower.contains("alert("),
        has_abs_href: lower.contains("href=\"/") || lower.contains("href='/"),
    }
}

/// 直接调 Ollama 的流式接口，把文本拼起来。
fn stream_generate(system: &str, user: &str) -> String {
    let payload = serde_json::json!({
        "model": MODEL,
        "stream": false,
        "think": false,
        "options": {
            "num_ctx": NUM_CTX,
            "num_predict": NUM_PREDICT,
            "repeat_penalty": 1.12,
            "repeat_last_n": 256,
            "temperature": 0.7,
        },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(900))
        .build()
        .expect("建 client 失败");
    let resp = client
        .post(format!("{OLLAMA}/api/chat"))
        .json(&payload)
        .send()
        .expect("请求 Ollama 失败");
    let json: serde_json::Value = resp.json().expect("解析响应失败");

    if let Some(usage_line) = json.get("eval_count") {
        let prompt_tokens = json
            .get("prompt_eval_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output_tokens = usage_line.as_u64().unwrap_or(0);
        let eval_ns = json
            .get("eval_duration")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let rate = if eval_ns > 0 {
            format!("{:.1} tok/s", output_tokens as f64 / (eval_ns as f64 / 1e9))
        } else {
            "—".into()
        };
        println!("用量：提示词 {prompt_tokens} token｜输出 {output_tokens} token｜{rate}");
    }

    json["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn default_system_prompt() -> String {
    "你是资深网页产品设计师与前端工程师。按需求生成完整、可直接打开的单文件网页：HTML + 内联 CSS + 原生 JS，不依赖任何外部资源（CDN、图片、构建工具、服务）；中文界面、响应式、视觉完整。只返回 HTML 源码，不要代码围栏或解释。\n\n【功能必须真实现】\n1. 每个按钮都绑定事件，事件里真的改状态、改文字、改列表；输出前数一遍：有几个 button 就有几处绑定。\n2. 需求给出的词库、枚举、取值必须原样内联成 JS 数组，并让生成、筛选、组合逻辑真的从数组取值；禁止只摆成静态列表不参与计算。\n3. 每个函数都要被别处调用（事件、初始化或另一个函数里），函数名只出现在 HTML 属性里不算被调用；用不到的不要定义。\n4. 严禁编造：不用随机字符凑数（如 Math.random().toString(36)），不写「随机生成」「待实现」这类占位，不留模拟数据和 TODO。\n5. 源码控制在 20000 字符内，数据只保留需求给出的条目，不要自行扩写或反复罗列。\n\n【交互必须真能用，禁止以下写法】\n6. 用 alert/confirm/prompt 弹出结果 —— 提示必须渲染成页面内元素（提示条、状态文字）。\n7. 站内跳转一律 href=\"#/名字\" 配 hashchange 切视图；禁止 href=\"/名字\"、\"名字.html\" 这类绝对路径，单文件里必然 404。\n8. 用 document.body.replaceChild 或整页 innerHTML 覆盖切换内容；渲染只写进一个固定容器（如 <main id=\"app\">）。\n9. 引用从未声明的变量；也不要在同一作用域写两遍同名函数或同名 const。\n\n【视觉与结构】\n严格使用需求给出的色值、字体、圆角与配色，不得替换或省略。浅底配深字、深底配浅字，正文对比度不低于 4.5:1。所有标签都要闭合，尤其是 </script>。".to_string()
}

fn default_requirement() -> String {
    "# 随机取名器\n\n## 页面结构\n- 顶部标题区\n- 主生成区（生成按钮 + 结果展示）\n- 收藏列表区\n\n## 功能需求\n1. [P0] 生成名字 —— 点击后从词库里随机组合出一个名字并显示\n2. [P0] 复制结果 —— 点击后复制当前名字到剪贴板\n3. [P0] 收藏 —— 把当前名字加入收藏列表并渲染\n4. [P1] 清空收藏 —— 清空收藏列表\n\n## 数据与字段\n- 前置字：雨、星、清、白、寒、疏、浅、薄、墨、云\n- 后置字：衣、舟、鹤、弦、辞、生、客、子、娘、君\n\n## 交互与状态\n- 点击生成时按钮文字短暂变为「生成中」\n- 收藏为空时显示空状态提示\n\n## 视觉规范\n- 浅色主题，主色 #2f6feb，背景 #f7f8fa，文字 #1f2430\n- 圆角 12px，无外链字体\n\n## 约束\n- 单文件，不使用任何外部资源".to_string()
}
