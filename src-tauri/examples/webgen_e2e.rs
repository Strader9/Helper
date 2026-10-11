//! 网页工坊流水线端到端验证（离线跑，不经过 GUI）。
//!
//! 这是验证用的临时入口，不参与应用打包。它复用生产代码里的全部关键部件：
//! - `webgen::normalize_document` / `webgen::specify_digest`（文档加工）
//! - `webgen::generation_budget` / `WEBPAGE_SYSTEM_PROMPT` / `REPEAT_PENALTY`（生成参数）
//! - `webgen::merge_continuation` / `take_tail`（续写拼接）
//! 只有 `start_webpage_generation` 里的循环控制流在这里原样复刻了一遍，
//! 用来在没有 Tauri 运行时与 AppHandle 的情况下跑完同一条链路。
//!
//! 用法：
//! ```text
//! cargo run --release --example webgen_e2e -- <需求文档.md> [输出.html]
//! ```

use std::time::Instant;

use futures_util::StreamExt;
use pc_guardian_lib::llm::{ChatChunk, OllamaClient, OllamaMessage};
use pc_guardian_lib::webgen;

/// 与 lib.rs 中的 `WEBGEN_CONTINUATION_TAIL_CHARS` 保持一致。
const CONTINUATION_TAIL_CHARS: usize = 900;
/// 与 lib.rs 中的 `MAX_CONTINUATION_ROUNDS` 保持一致。
const MAX_CONTINUATION_ROUNDS: usize = 6;
/// 与 lib.rs 中的 `CONTINUATION_MIN_GAIN` 保持一致。
const CONTINUATION_MIN_GAIN: usize = 40;
/// 与 lib.rs 中的 `MAX_FUNCTION_REPAIR_ROUNDS` 保持一致。
const MAX_FUNCTION_REPAIR_ROUNDS: usize = 3;

/// 与 lib.rs 里 `start_webpage_generation` 末尾的清理逻辑保持一致。
fn tidy(html: &str) -> String {
    html.trim()
        .trim_start_matches("```html")
        .trim_start_matches("```HTML")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
        .to_string()
}

fn now() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let doc_path = args
        .get(1)
        .cloned()
        .expect("用法: webgen_e2e <需求文档.md> [输出.html]");
    let out_path = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "webgen_e2e_out.html".to_string());

    let raw = std::fs::read_to_string(&doc_path).expect("读取需求文档失败");
    let base_url =
        std::env::var("OLLAMA_BASE_URL").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen3:8b".to_string());
    let client = OllamaClient::new(base_url, 300_000);

    println!("[{}] 输入文档：{}（{} 字）", now(), doc_path, raw.chars().count());
    println!("[{}] 模型：{}", now(), model);

    // ---------- 阶段一：清洗 → 梳理 → 规格化 ----------
    let t_pre = Instant::now();
    let normalized = webgen::normalize_document(&client, &model, &raw, |message: &str| {
        println!("[{}] [加工] {}", now(), message);
    })
    .await;
    let pre_ms = t_pre.elapsed().as_millis();
    println!(
        "[{}] 加工阶段结束：{}，耗时 {} 毫秒，规格 {} 字",
        now(),
        normalized.stage.label(),
        pre_ms,
        normalized.spec.chars().count()
    );
    if let Some(digest) = normalized.digest.as_ref() {
        let digest_out = out_path.replace(".html", "").to_string() + ".digest.txt";
        let _ = std::fs::write(&digest_out, digest);
        println!("[{}] 梳理稿已存：{}（{} 字）", now(), digest_out, digest.chars().count());
    }
    let spec_out = out_path.replace(".html", "").to_string() + ".spec.md";
    let _ = std::fs::write(&spec_out, &normalized.spec);
    println!("[{}] 标准格式规格已存：{}", now(), spec_out);

    // ---------- 阶段二：生成单文件 HTML ----------
    let spec = normalized.spec;
    let data_contract = webgen::data_contract_block(&spec);
    let coverage_contract = webgen::coverage_contract_block(&spec);
    let budget = webgen::generation_budget(
        spec.chars().count(),
        CONTINUATION_TAIL_CHARS,
        data_contract.chars().count() + coverage_contract.chars().count(),
    );
    println!(
        "[{}] 生成预算：窗口 {} token，单轮输出上限 {} token",
        now(),
        budget.num_ctx,
        budget.num_predict
    );

    let requirements_block = format!(
        "请严格依据下面的网页需求生成产品网页：\n\n<requirements>\n{spec}\n</requirements>{data_contract}{coverage_contract}"
    );
    let options = serde_json::json!({
        "num_ctx": budget.num_ctx,
        "num_predict": budget.num_predict,
        "repeat_penalty": webgen::REPEAT_PENALTY,
        "repeat_last_n": webgen::REPEAT_LAST_N,
    });

    let t_gen = Instant::now();
    let mut html = String::new();
    let mut rounds = 0usize;
    loop {
        let messages = if rounds == 0 {
            vec![
                OllamaMessage::system(webgen::WEBPAGE_SYSTEM_PROMPT),
                OllamaMessage::user(&requirements_block),
            ]
        } else {
            let tail = webgen::take_tail(&html, CONTINUATION_TAIL_CHARS);
            let continuation = format!(
                "{requirements_block}\n\n上面这个单文件网页你已经写了一部分，但中途停止了。下面是已输出内容的结尾片段（不是文件开头）：\n\n<tail>\n{tail}\n</tail>\n\n请紧接这段结尾继续输出剩余的 HTML 源码，补齐尚未实现的功能，直到包含 </body></html> 为止。只输出续写部分，不要重复已有片段，不要重新开始，不要输出任何解释。"
            );
            vec![
                OllamaMessage::system(webgen::WEBPAGE_SYSTEM_PROMPT),
                OllamaMessage::user(&continuation),
            ]
        };

        println!(
            "[{}] {}",
            now(),
            if rounds == 0 {
                "发送需求，开始生成…"
            } else {
                "从断点继续补全…"
            }
        );

        let mut stream = match client
            .chat_stream_with_options(&model, messages, 0.2, options.clone())
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                println!("[{}] [错误] 启动模型请求失败：{error:#}", now());
                std::process::exit(2);
            }
        };

        let round_start = html.chars().count();
        let round_t0 = Instant::now();
        let mut done_reason = None;
        let mut reported = 0usize;
        while let Some(chunk) = stream.next().await {
            match chunk {
                ChatChunk::Text(text) => {
                    html.push_str(&text);
                    let total = html.chars().count();
                    if total.saturating_sub(reported) >= 2000 {
                        reported = total;
                        println!("[{}]   …已接收约 {} 字符", now(), total);
                    }
                    // 与 lib.rs 一致：页面闭合就立刻收手，否则模型会接着把整段逻辑再写一遍。
                    if text.contains('<') && html.to_ascii_lowercase().contains("</html>") {
                        println!("[{}]   检测到 </html>，提前停止本轮接收。", now());
                        break;
                    }
                }
                ChatChunk::Error(error) => {
                    println!("[{}] [错误] {error}", now());
                    std::process::exit(2);
                }
                ChatChunk::Done(reason) => {
                    done_reason = reason;
                    break;
                }
                ChatChunk::ToolCall(_) => {}
            }
        }

        if rounds > 0 {
            html = webgen::merge_continuation(&html, round_start);
        }

        let mut cleaned = webgen::repair_html(&tidy(&html));
        let lower = cleaned.to_ascii_lowercase();
        let grew = html.chars().count().saturating_sub(round_start);
        println!(
            "[{}] 第 {} 轮结束：本轮 +{} 字符，累计 {} 字符，耗时 {:.1} 秒，停止原因 {:?}",
            now(),
            rounds + 1,
            grew,
            html.chars().count(),
            round_t0.elapsed().as_secs_f64(),
            done_reason
        );

        if lower.contains("<html") && lower.contains("</html>") {
            // ---------- 确定性收口（与 lib.rs 的 start_webpage_generation 一致）----------
            // 模型把整份脚本重开了一篇时，退回到完整的那一代，再进自检修复。
            if let Some((fixed, reason)) = webgen::restore_script(&cleaned) {
                println!("[{}] [收口] {reason}。", now());
                cleaned = fixed;
            }

            // 与 lib.rs 一致：给 localStorage 读取补空值兜底（确定性修复）。
            {
                let merged = webgen::merged_script(&cleaned);
                if let Some(fixed) = webgen::fix_null_storage_reads(&merged) {
                    println!(
                        "[{}] [收口] 给 localStorage / sessionStorage 的读取补了空值兜底。",
                        now()
                    );
                    cleaned = webgen::splice_script_body(&cleaned, &fixed);
                }
            }

            // 与 lib.rs 一致：单文件网页里的站内绝对路径链接改成 hash 路由（否则点开必然 404）。
            if let Some(fixed) = webgen::fix_dead_links(&cleaned) {
                println!(
                    "[{}] [收口] 站内绝对路径链接已改写成 hash 路由。",
                    now()
                );
                cleaned = fixed;
            }

            // 与 lib.rs 一致：把 `alert(...)` 换成页内提示条（语义等价，不再用弹窗承载状态）。
            {
                let merged = webgen::merged_script(&cleaned);
                if let Some(fixed) = webgen::fix_placeholder_alerts(&merged) {
                    println!("[{}] [收口] 已把 alert 换成页内提示条。", now());
                    cleaned = webgen::splice_script_body(&cleaned, &fixed);
                }
            }

            // 与 lib.rs 一致：把正则里认错写法的 Unicode 属性名改对（\p{Han} → \p{Script=Han}）。
            {
                let merged = webgen::merged_script(&cleaned);
                if let Some(fixed) = webgen::fix_regex_properties(&merged) {
                    println!("[{}] [收口] 已修正正则里的 Unicode 属性名写法。", now());
                    cleaned = webgen::splice_script_body(&cleaned, &fixed);
                }
            }

            // 与 lib.rs 一致：把用 \w 校验中文输入的写法改成认汉字（否则中文输入被判非法）。
            {
                let merged = webgen::merged_script(&cleaned);
                if let Some(fixed) = webgen::fix_narrow_word_class(&merged) {
                    println!("[{}] [收口] 已把 \\w 校验改成认汉字的写法。", now());
                    cleaned = webgen::splice_script_body(&cleaned, &fixed);
                }
            }

            // 与 lib.rs 一致：清掉脚本里混进来的 Markdown 代码围栏（否则整段脚本 SyntaxError）。
            if let Some(fixed) = webgen::strip_script_fences(&cleaned) {
                println!(
                    "[{}] [收口] 清掉了脚本里混进来的 Markdown 代码围栏。",
                    now()
                );
                cleaned = fixed;
            }

            // ---------- 功能自检 + 定向修复（与 lib.rs 的 start_webpage_generation 一致）----------
            let mut repair_round = 0usize;
            // 与 lib.rs 一致：判"有没有变好"要看加权成本（含缺失函数个数），不能只数条数。
            let mut last_health = (usize::MAX, usize::MAX);
            // 与 lib.rs 一致：留"至今最好"的快照，修复改坏了就回滚（按加权致命度比较）。
            let mut best: Option<(String, (usize, usize))> = None;
            loop {
                let issues = webgen::verify_page(&cleaned, &spec);
                let health = webgen::health_with_html(&cleaned, &issues);
                if best
                    .as_ref()
                    .map_or(true, |(_, best_health)| health < *best_health)
                {
                    best = Some((cleaned.clone(), health));
                }
                if issues.is_empty() {
                    println!("[{}] [自检] 通过：数据来源、交互绑定与需求覆盖都正常。", now());
                    break;
                }
                let critical = issues
                    .iter()
                    .filter(|issue| issue.severity == webgen::Severity::Critical)
                    .count();
                println!(
                    "[{}] [自检] 发现 {} 项问题，其中 {critical} 项严重：",
                    now(),
                    issues.len()
                );
                for issue in &issues {
                    println!("[{}]   {}", now(), issue.log_line());
                }
                let plan = webgen::function_appendix_plan(&issues);
                if critical == 0 && plan.is_none() {
                    println!("[{}] [自检] 没有需要修复的严重问题，保留当前版本。", now());
                    break;
                }
                if repair_round >= MAX_FUNCTION_REPAIR_ROUNDS {
                    println!("[{}] [自检] 修复轮次用尽，仍有 {critical} 项严重问题。", now());
                    break;
                }
                if repair_round > 0 && health >= last_health {
                    println!("[{}] [自检] 修复没有减少问题，停止。", now());
                    break;
                }
                last_health = health;

                // 与 lib.rs 一致：整段重写前的容量预检 —— 装不下就别开始。
                if plan.is_none() {
                    let (probe_system, probe_prompt) =
                        webgen::script_repair_prompt(&spec, &cleaned, &issues);
                    let cap =
                        webgen::budget_for_prompt(&format!("{probe_system}{probe_prompt}")).num_predict;
                    let need = webgen::script_token_estimate(&cleaned);
                    if need as i64 > cap {
                        println!(
                            "[{}] [自检] 重写脚本约需 {need} token，超过本轮输出上限 {cap} token，装不下，跳过重写并保留当前版本。",
                            now()
                        );
                        break;
                    }
                }

                // 三条路线（与 lib.rs 一致）：就地补写 / 定点接线 / 整段重写脚本块。
                let missing = webgen::missing_function_names(&cleaned);
                let appendix_route = plan.is_some();
                let pool_patch = if appendix_route {
                    None
                } else {
                    webgen::function_patch_plan(&cleaned, &issues)
                };
                let wiring_route = pool_patch.is_some();
                let need_bindings = plan.unwrap_or(false);
                let (system, prompt, options) = if appendix_route {
                    let (system, prompt) =
                        webgen::missing_function_prompt(&spec, &cleaned, &missing, need_bindings);
                    let mut parts: Vec<String> = Vec::new();
                    if !missing.is_empty() {
                        parts.push(format!(
                            "补写 {} 个缺失函数（{}）",
                            missing.len(),
                            missing.join(" / ")
                        ));
                    }
                    if need_bindings {
                        parts.push("补上按钮的事件绑定".to_string());
                    }
                    println!(
                        "[{}] [修复] 第 {} 轮：就地补写 —— {}",
                        now(),
                        repair_round + 1,
                        parts.join("，")
                    );
                    let options = serde_json::json!({
                        "num_ctx": webgen::NUM_CTX,
                        "num_predict": webgen::APPENDIX_NUM_PREDICT,
                        "repeat_penalty": webgen::REPEAT_PENALTY,
                        "repeat_last_n": webgen::REPEAT_LAST_N,
                    });
                    (system, prompt, options)
                } else if let Some(pools) = &pool_patch {
                    let (system, prompt) = webgen::data_wiring_prompt(&spec, &cleaned, pools);
                    println!(
                        "[{}] [修复] 第 {} 轮：定点接线 —— 把 {} 真正接进生成逻辑",
                        now(),
                        repair_round + 1,
                        pools.join(" / ")
                    );
                    let options = serde_json::json!({
                        "num_ctx": webgen::NUM_CTX,
                        "num_predict": webgen::WIRING_NUM_PREDICT,
                        "repeat_penalty": webgen::REPEAT_PENALTY,
                        "repeat_last_n": webgen::REPEAT_LAST_N,
                    });
                    (system, prompt, options)
                } else {
                    let (system, prompt) = webgen::script_repair_prompt(&spec, &cleaned, &issues);
                    let budget = webgen::budget_for_prompt(&format!("{system}{prompt}"));
                    println!(
                        "[{}] [修复] 第 {} 轮：整段重写脚本块（窗口 {} token，单轮输出 {} token）",
                        now(),
                        repair_round + 1,
                        budget.num_ctx,
                        budget.num_predict
                    );
                    let options = serde_json::json!({
                        "num_ctx": budget.num_ctx,
                        "num_predict": budget.num_predict,
                        "repeat_penalty": webgen::REPEAT_PENALTY,
                        "repeat_last_n": webgen::REPEAT_LAST_N,
                    });
                    (system, prompt, options)
                };
                let messages = vec![
                    OllamaMessage::system(&system),
                    OllamaMessage::user(&prompt),
                ];
                let mut stream = match client
                    .chat_stream_with_options(&model, messages, 0.2, options)
                    .await
                {
                    Ok(stream) => stream,
                    Err(error) => {
                        println!("[{}] [修复] 启动失败，保留当前版本：{error:#}", now());
                        break;
                    }
                };
                let round_t0 = Instant::now();
                let mut patch = String::new();
                let mut broken = false;
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        ChatChunk::Text(text) => patch.push_str(&text),
                        ChatChunk::Error(error) => {
                            println!("[{}] [修复] 中断：{error}", now());
                            broken = true;
                            break;
                        }
                        ChatChunk::Done(_) => break,
                        ChatChunk::ToolCall(_) => {}
                    }
                }
                if broken {
                    break;
                }
                let next = if appendix_route {
                    match webgen::tidy_function_appendix(&patch) {
                        Some(definitions) => {
                            if let Some(problem) = webgen::delimiter_problem(&definitions) {
                                println!(
                                    "[{}] [修复] 补写的函数自身就是残的（{problem}），拒收。",
                                    now()
                                );
                                None
                            } else {
                                let conflicts =
                                    webgen::appendix_conflicts(&cleaned, &definitions);
                                if !conflicts.is_empty() {
                                    println!(
                                        "[{}] [修复] 补写内容会覆盖已有函数（{}），拒收。",
                                        now(),
                                        conflicts.join(" / ")
                                    );
                                    None
                                } else {
                                    Some(webgen::append_to_script(&cleaned, &definitions))
                                }
                            }
                        }
                        None => {
                            println!("[{}] [修复] 没有返回可用的函数定义，保留当前版本。", now());
                            None
                        }
                    }
                } else if wiring_route {
                    let replacements = webgen::tidy_function_patch(&patch);
                    if replacements.is_empty() {
                        println!("[{}] [修复] 没有返回可用的函数定义，保留当前版本。", now());
                        None
                    } else if let Some((name, problem)) = replacements
                        .iter()
                        .find_map(|(name, definition)| {
                            webgen::delimiter_problem(definition)
                                .map(|problem| (name.clone(), problem))
                        })
                    {
                        println!(
                            "[{}] [修复] 返回的函数 {name} 自身就是残的（{problem}），拒收。",
                            now()
                        );
                        None
                    } else {
                        let names: Vec<String> =
                            replacements.iter().map(|(name, _)| name.clone()).collect();
                        match webgen::replace_functions(&cleaned, &replacements) {
                            Some(page) => {
                                println!(
                                    "[{}] [修复] 已替换函数：{}。",
                                    now(),
                                    names.join(" / ")
                                );
                                Some(page)
                            }
                            None => {
                                println!(
                                    "[{}] [修复] 要改的函数在脚本里找不到对应声明，保留当前版本。",
                                    now()
                                );
                                None
                            }
                        }
                    }
                } else {
                    match webgen::tidy_script_block(&patch) {
                        Some(block) => {
                            if let Some(problem) = webgen::delimiter_problem(&block) {
                                println!(
                                    "[{}] [修复] 返回的脚本自身就是残的（{problem}），拒收。",
                                    now()
                                );
                                None
                            } else {
                                Some(webgen::splice_script(&cleaned, &block))
                            }
                        }
                        None => {
                            println!("[{}] [修复] 没有返回可用的脚本块，保留当前版本。", now());
                            None
                        }
                    }
                };
                let Some(spliced) = next else {
                    break;
                };
                if spliced == cleaned {
                    println!("[{}] [修复] 没有带来任何改动，停止。", now());
                    break;
                }
                cleaned = spliced;
                repair_round += 1;
                println!(
                    "[{}] [修复] 第 {} 轮已应用，耗时 {:.1} 秒，重新自检。",
                    now(),
                    repair_round,
                    round_t0.elapsed().as_secs_f64()
                );
            }

            // 与 lib.rs 一致：修复越改越差就回滚到问题更少的那一版。
            if let Some((snapshot, _)) = best {
                if snapshot != cleaned {
                    println!(
                        "[{}] [自检] 修复后的版本比修复前更差，已回滚到问题更少的那一版。",
                        now()
                    );
                    cleaned = snapshot;
                }
            }

            // 交付前的确定性抢救（与 lib.rs 一致）。
            if let Some(problem) = webgen::delimiter_problem(&webgen::merged_script(&cleaned)) {
                match webgen::restore_script(&cleaned) {
                    Some((fixed, reason)) => {
                        println!("[{}] [抢救] 脚本仍不完整（{problem}），已{reason}。", now());
                        cleaned = fixed;
                    }
                    None => println!(
                        "[{}] [抢救] 脚本不完整（{problem}），自动抢救失败。",
                        now()
                    ),
                }
            }

            let _ = std::fs::write(&out_path, &cleaned);
            println!(
                "[{}] 生成完成：{}（{} 字符）。总耗时 {:.1} 秒",
                now(),
                out_path,
                cleaned.chars().count(),
                t_gen.elapsed().as_secs_f64()
            );
            return;
        }

        if grew < CONTINUATION_MIN_GAIN {
            // 与 lib.rs 一致：续写没产出新内容，也不要把已攒下的大半个页面丢掉，
            // 补齐闭合标签并跑确定性收口后交付。
            if cleaned.to_ascii_lowercase().contains("<html") {
                let mut salvaged = cleaned.clone();
                if let Some((fixed, reason)) = webgen::restore_script(&salvaged) {
                    println!("[{}] [收口] 续写中断，脚本收口：{reason}。", now());
                    salvaged = fixed;
                }
                if let Some(fixed) =
                    webgen::fix_null_storage_reads(&webgen::merged_script(&salvaged))
                {
                    salvaged = webgen::splice_script_body(&salvaged, &fixed);
                }
                if let Some(fixed) = webgen::fix_dead_links(&salvaged) {
                    salvaged = fixed;
                }
                if let Some(fixed) =
                    webgen::fix_narrow_word_class(&webgen::merged_script(&salvaged))
                {
                    salvaged = webgen::splice_script_body(&salvaged, &fixed);
                }
                if let Some(fixed) =
                    webgen::fix_placeholder_alerts(&webgen::merged_script(&salvaged))
                {
                    salvaged = webgen::splice_script_body(&salvaged, &fixed);
                }
                if let Some(fixed) =
                    webgen::fix_regex_properties(&webgen::merged_script(&salvaged))
                {
                    salvaged = webgen::splice_script_body(&salvaged, &fixed);
                }
                if let Some(fixed) = webgen::strip_script_fences(&salvaged) {
                    println!("[{}] [收口] 清掉了脚本里混进来的 Markdown 代码围栏。", now());
                    salvaged = fixed;
                }
                let lower_salvaged = salvaged.to_ascii_lowercase();
                if !lower_salvaged.contains("</body>") {
                    salvaged.push_str("\n</body>");
                }
                if !lower_salvaged.contains("</html>") {
                    salvaged.push_str("\n</html>");
                }
                let issues = webgen::verify_page(&salvaged, &spec);
                println!(
                    "[{}] [收口] 续写中断（停止原因：{}），交付当前可救的版本，仍有 {} 项问题：",
                    now(),
                    done_reason.clone().unwrap_or_else(|| "未知".to_string()),
                    issues.len()
                );
                for issue in &issues {
                    println!("[{}]   {}", now(), issue.log_line());
                }
                let _ = std::fs::write(&out_path, &salvaged);
                println!(
                    "[{}] 生成完成（降级交付）：{}（{} 字符）。总耗时 {:.1} 秒",
                    now(),
                    out_path,
                    salvaged.chars().count(),
                    t_gen.elapsed().as_secs_f64()
                );
                return;
            }
            println!(
                "[{}] [失败] 模型未继续输出（停止原因：{}）",
                now(),
                done_reason.unwrap_or_else(|| "未知".to_string())
            );
            let _ = std::fs::write(&out_path, &cleaned);
            std::process::exit(3);
        }

        if rounds >= MAX_CONTINUATION_ROUNDS {
            println!("[{}] [失败] 已达最大续写轮次仍未写完", now());
            let _ = std::fs::write(&out_path, &cleaned);
            std::process::exit(4);
        }
        // 与 lib.rs 一致：被输出上限截断时，先退回最后一个完整行再续写，
        // 否则续写会把 `}` 接在半个 token 后面，得到语法错误的脚本。
        if done_reason.as_deref() == Some("length") {
            if let Some(index) = html.rfind('\n') {
                html.truncate(index + 1);
            }
        }

        rounds += 1;
    }
}
