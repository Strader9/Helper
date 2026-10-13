//! 定向修改链路验证：用真实模型跑一次"点选元素 → 只改那一处"。
//!
//! 验证目标（对应阶段二的"直接告诉程序哪里需要改"）：
//! 1. 修改指令里的位置信息是否被模型正确理解（改的是指定元素而非整页）；
//! 2. 改完之后，页面其他部分是否**真的**原样保留（不是被顺手重写）；
//! 3. 一次定向修改后，功能自检是否仍然通过。
//!
//! 用法：
//! ```text
//! cargo run --example refine_probe -- <原html> <修改意见> [target描述] [输出html]
//! ```
//! 省略 target 就是普通整页修改，可以用来对照两者的产物差异。

use std::env;
use std::time::Instant;

use futures_util::StreamExt;
use pc_guardian_lib::llm::{ChatChunk, OllamaClient, OllamaMessage};
use pc_guardian_lib::webgen;

const OLLAMA: &str = "http://localhost:11434";
const MODEL: &str = "qwen3:8b";

/// 与 lib.rs 的 `WEBGEN_PREV_HTML_FULL_LIMIT` 对齐：整页在此字符数以内就全量回传。
const FULL_LIMIT: usize = 6000;
/// 与 lib.rs 的 `WEBGEN_PREV_HTML_PATCH_WINDOW` 对齐：定向修改的片段窗口。
const PATCH_WINDOW: usize = 6000;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let Some(html_path) = args.get(1) else {
        eprintln!("用法：refine_probe <原html> <修改意见> [target描述] [输出html]");
        std::process::exit(2);
    };
    let html = std::fs::read_to_string(html_path).expect("读不到原 HTML");
    let feedback = args.get(2).cloned().unwrap_or_else(|| "把主色调改成绿色".to_string());
    let target = args.get(3).filter(|t| !t.trim().is_empty()).cloned();
    let out_path = args.get(4).cloned();

    // 复刻生产链路的修改提示词拼装（位置在前、要求在后 —— 见 lib.rs 里的说明）。
    let mut user = String::from("请严格依据下面的网页需求生成产品网页：\n\n<requirements>\n（沿用上一版需求，这里只做修改）\n</requirements>");
    if let Some(target) = target.as_ref() {
        user.push_str(&format!(
            "\n\n【只改这一处】用户在预览里点选了下面这个元素，请**只针对它**做修改，其余部分原样保留：\n{target}"
        ));
    }

    // **必须复刻生产链路的片段摘取与合并**，否则探针验证的不是真实路径：
    // 整页超过阈值时只回传包含目标元素的那一段，**并要求模型只输出改后的一小块**，
    // 由程序把这一小块贴回原页。
    //
    // 早先探针把全量 HTML 塞进提示词、又要模型吐完整页面，测的是"模型能不能在片段里
    // 补全整页"——那是不可能完成的任务，实测模型把 18918 字的整页替换成了 3006 字片段。
    let chars = html.chars().count();
    let mut patch_mode = false;
    let (payload, note) = if chars <= FULL_LIMIT {
        (html.clone(), String::new())
    } else if let Some(target) = target.as_ref() {
        let (excerpt, located) = webgen::take_excerpt(&html, target, PATCH_WINDOW);
        patch_mode = located;
        println!(
            "片段摘取：整页 {chars} 字 → 回传 {} 字（{}）",
            excerpt.chars().count(),
            if located { "已定位到目标元素 → 片段模式" } else { "未定位到，退回开头" }
        );
        let note = if located {
            format!("\n\n（注意：现有页面共约 {chars} 字，这里只回传了包含**你要修改的元素**的片段。）")
        } else {
            format!("\n\n（注意：现有页面共约 {chars} 字，受上下文长度限制只回传了开头片段，且没能定位到你要修改的元素。请结合上面的元素描述与修改意见进行修改，并保持原有视觉风格与结构。）")
        };
        (excerpt, note)
    } else {
        (
            webgen::take_head(&html, FULL_LIMIT / 2),
            format!("\n\n（注意：现有页面共约 {chars} 字，受上下文长度限制这里只回传了开头部分。请在保持其视觉风格、配色和整体结构的前提下，按修改意见输出完整的新版本。）"),
        )
    };
    let note = if patch_mode {
        format!(
            "{note}\n\n**只输出你改过的那一小块代码**（例如那一个按钮、那一段样式、那一个函数），\
             不要输出整页，不要输出片段里其余未改动的部分。程序会把这一小块贴回原页对应位置。\
             除了这一小块之外，什么都不要返回。"
        )
    } else {
        note
    };
    user.push_str(&format!("\n{note}"));
    // 要求必须与模式一致：片段模式要"只给一小块"，否则两条互斥指令会让模型两头不照做。
    if patch_mode {
        user.push_str(&format!(
            "\n\n请根据以下修改意见，**只输出你改动过的那一小块代码**，不要输出整页：\n{feedback}"
        ));
    } else {
        user.push_str(&format!(
            "\n\n请根据以下修改意见，在现有网页基础上修改，并返回完整 HTML：\n{feedback}"
        ));
    }
    user.push_str(&format!("\n\n<current_html>\n{payload}\n</current_html>"));

    let client = OllamaClient::new(OLLAMA.to_string(), 900_000);
    let options = serde_json::json!({
        "num_ctx": 16_384,
        "num_predict": 6_144,
        "repeat_penalty": webgen::REPEAT_PENALTY,
        "repeat_last_n": webgen::REPEAT_LAST_N,
    });

    println!("=== 定向修改 ===");
    if let Some(target) = target.as_ref() {
        println!("目标元素：\n{target}");
    } else {
        println!("目标元素：（未指定，整页修改）");
    }
    println!("修改意见：{feedback}\n");

    let started = Instant::now();
    let messages = vec![
        OllamaMessage::system("你是资深网页产品设计师与前端工程师。在现有单文件网页的基础上按用户要求修改，返回完整 HTML 源码，不要代码围栏或解释。保持其余部分不变，只改动用户指定的部分。"),
        OllamaMessage::user(&user),
    ];
    let mut stream = match client
        .chat_stream_with_options(MODEL, messages, 0.2, options)
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("启动请求失败：{error:#}");
            std::process::exit(2);
        }
    };

    let mut output = String::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            ChatChunk::Text(text) => output.push_str(&text),
            ChatChunk::Error(error) => {
                eprintln!("出错：{error}");
                break;
            }
            ChatChunk::Done(_, usage) => {
                if let Some(usage) = usage.filter(|u| u.is_measured()) {
                    println!("用量：{}", usage.log_line());
                }
                break;
            }
            ChatChunk::ToolCall(_) => {}
        }
    }
    let elapsed = started.elapsed();

    let cleaned = webgen::repair_html(
        output
            .trim()
            .trim_start_matches("```html")
            .trim_start_matches("```HTML")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim(),
    );

    // 片段模式下要把模型返回的"这一小块"贴回原页 —— 与生产链路一致。
    let cleaned = if patch_mode {
        match target.as_ref().and_then(|target| {
            webgen::merge_patch(&html, target, PATCH_WINDOW, &cleaned)
        }) {
            Some(outcome) if outcome.changed => {
                println!("\n片段合并：改后 {} 字已贴回原页", cleaned.chars().count());
                outcome.html
            }
            Some(_) => {
                println!("\n片段合并：模型没有改动这一处，保留原页");
                html.clone()
            }
            None => {
                println!("\n片段合并：无法安全贴回（结构或目标对不上），保留原页");
                html.clone()
            }
        }
    } else {
        cleaned
    };

    println!("\n=== 结果 ===");
    println!("耗时：{:.1}s", elapsed.as_secs_f64());
    println!("原字符数：{}｜新字符数：{}", html.chars().count(), cleaned.chars().count());
    println!("字符数变化：{:+}", cleaned.chars().count() as i64 - html.chars().count() as i64);

    // 关键指标：改动范围。
    //
    // "只改一处"是否成立，看的是**有多少行真的变了**。整页重写会让几乎每一行都不同
    // （缩进、顺序、注释都可能被重排），而定向修改只动几行。
    let diff = line_diff_ratio(&html, &cleaned);
    println!(
        "行级差异：{} 行不同 / 共 {} 行（{:.1}%）",
        diff.changed,
        diff.total,
        diff.ratio * 100.0
    );
    if diff.ratio < 0.25 {
        println!("✓ 改动局限在局部（定向修改生效）");
    } else if diff.ratio < 0.6 {
        println!("· 改动范围中等（部分保留了原文）");
    } else {
        println!("✗ 几乎整页重写（定向没生效）");
    }

    // 修改后功能是否还完好。
    let issues = webgen::verify_page(&cleaned, "");
    println!("\n=== 修改后自检 ===");
    if issues.is_empty() {
        println!("✓ 自检通过，功能未因修改而损坏");
    } else {
        println!("发现 {} 项问题：", issues.len());
        for issue in &issues {
            println!("  [{}] {}：{}", issue.severity.label(), issue.code, issue.detail);
        }
    }

    if let Some(path) = out_path {
        std::fs::write(&path, &cleaned).expect("写盘失败");
        println!("\n已写入：{path}");
    }
}

struct LineDiff {
    changed: usize,
    total: usize,
    ratio: f64,
}

/// 粗算行级差异比例。
///
/// 不做真正的 LCS diff（没必要）：按行去空白后比较"同一位置的行是否相同"。
/// 对"整页重写 vs 局部修改"这个判断已经足够 —— 整页重写时行数与行内容都会大范围变动。
fn line_diff_ratio(before: &str, after: &str) -> LineDiff {
    let a: Vec<String> = before.lines().map(|l| l.trim().to_string()).collect();
    let b: Vec<String> = after.lines().map(|l| l.trim().to_string()).collect();
    let total = a.len().max(b.len());
    if total == 0 {
        return LineDiff { changed: 0, total: 0, ratio: 0.0 };
    }
    let mut changed = 0usize;
    for index in 0..total {
        match (a.get(index), b.get(index)) {
            (Some(left), Some(right)) if left == right => {}
            _ => changed += 1,
        }
    }
    LineDiff {
        changed,
        total,
        ratio: changed as f64 / total as f64,
    }
}
