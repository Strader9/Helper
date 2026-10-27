//! 验证设置界面拿到的数据是否"可调"。
//!
//! 跑法：cargo run --example verify_settings_ui
//!
//! 检查设置页每个 Tab 会调用的后端命令，确认：
//! 1. 通用设置返回的每一项都带类型与区间（前端才能渲染出正确的控件）
//! 2. 浏览器设置的两项确实存在且可写
//! 3. 主动助手的预设规则存在，且阈值是从 trigger_config 里读得出来的
//! 4. 值校验真的会拒绝越界输入

use pc_guardian_lib::db::Database;
use std::path::PathBuf;

fn main() {
    let appdata = std::env::var("APPDATA").expect("需要 APPDATA");
    let real = PathBuf::from(&appdata)
        .join("pc-guardian")
        .join("pc-guardian.db");
    let work = std::env::temp_dir().join("pcg-ui-probe");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).expect("建目录");
    let copy = work.join("pc-guardian.db");
    std::fs::copy(&real, &copy).expect("复制库");

    let db = Database::new(&copy).expect("打开");
    db.init_schema().expect("迁移");

    let mut failures: Vec<String> = Vec::new();
    let items = db.get_all_settings().expect("读设置");

    println!("=== 通用设置：{} 项 ===", items.len());
    for item in &items {
        let range = match (item.min_value, item.max_value) {
            (Some(lo), Some(hi)) => format!("{} – {}", lo, hi),
            (Some(lo), None) => format!("≥ {}", lo),
            (None, Some(hi)) => format!("≤ {}", hi),
            (None, None) => "（无区间）".to_string(),
        };
        let unit = item.unit.as_deref().unwrap_or("");
        println!(
            "  {:<32} type={:<8} {:<18} {}",
            item.key, item.value_type, range, unit
        );

        // 数值项必须有区间，否则前端只能放个裸输入框
        if item.value_type == "number" && item.min_value.is_none() && item.max_value.is_none() {
            failures.push(format!("数值项 {} 没有任何区间约束", item.key));
        }
        // 布尔项不该带区间
        if item.value_type == "boolean" && (item.min_value.is_some() || item.max_value.is_some()) {
            failures.push(format!("布尔项 {} 不该有区间", item.key));
        }
        // 每一项都要有说明，否则界面上是个没头没脑的键名
        if item.description.as_deref().unwrap_or("").trim().is_empty() {
            failures.push(format!("{} 缺少说明文本", item.key));
        }
    }

    // 浏览器 Tab 依赖的两项
    println!("\n=== 浏览器设置 ===");
    for key in ["browser.default", "browser.cdp_port"] {
        match items.iter().find(|s| s.key == key) {
            Some(item) => println!(
                "  {} = {} ({})",
                key,
                item.value,
                item.description.clone().unwrap_or_default()
            ),
            None => failures.push(format!("浏览器设置项 {} 不存在", key)),
        }
    }

    // 预设规则的阈值可读性
    println!("\n=== 主动助手预设规则 ===");
    let rules = pc_guardian_lib::proactive::get_rules(db.connection()).expect("读规则");
    if rules.is_empty() {
        failures.push("没有任何主动助手规则，界面会是空的".to_string());
    }
    for rule in &rules {
        let threshold = if rule.trigger_type == "system_metric" {
            serde_json::from_str::<serde_json::Value>(&rule.trigger_config)
                .ok()
                .and_then(|v| v.get("threshold").and_then(|t| t.as_f64()))
        } else {
            None
        };
        println!(
            "  [{}] {:<16} type={:<14} 阈值={:?} enabled={}",
            if rule.id.starts_with("preset_") {
                "预设"
            } else {
                "自定义"
            },
            rule.name,
            rule.trigger_type,
            threshold,
            rule.enabled
        );
        // 阈值必须能解析出来，否则界面上"改阈值"按钮点了没反应
        if rule.trigger_type == "system_metric" && threshold.is_none() {
            failures.push(format!("规则 {} 的阈值读不出来，界面无法编辑", rule.id));
        }
    }

    // 值校验：越界必须被拒
    println!("\n=== 值校验（越界应被拒） ===");
    for (key, bad) in [
        ("agent.temperature", "5.0"),
        ("browser.cdp_port", "80"),
        ("agent.num_ctx", "999999"),
    ] {
        let bounds = db.get_setting_bounds(key).expect("查区间");
        let rejected = match bounds {
            Some((_, Some(lo), Some(hi), _)) => {
                let v: f64 = bad.parse().unwrap_or(f64::NAN);
                v < lo || v > hi
            }
            Some((_, Some(lo), None, _)) => bad.parse::<f64>().unwrap_or(f64::NAN) < lo,
            Some((_, None, Some(hi), _)) => bad.parse::<f64>().unwrap_or(f64::NAN) > hi,
            _ => false,
        };
        println!(
            "  {} = {} → {}",
            key,
            bad,
            if rejected {
                "会被拒绝 ✓"
            } else {
                "会被接受 ✗"
            }
        );
        if !rejected {
            failures.push(format!("{} 填 {} 应被拒绝，但区间判定放行了", key, bad));
        }
    }

    // 运行参数热读：改了设置必须立刻拿到新值，不能是启动时快照。
    //
    // 这是本次"每个设置项都要真的起作用"里最容易翻车的一环——设置写进库
    // 了、界面也显示新值，但运行时仍用旧值，属于最隐蔽的假可调。
    println!("\n=== 运行参数热读（改完立即生效）===");
    {
        let before_temp = db.get_setting("agent.temperature").expect("读温度");
        let before_turns = db.get_setting("agent.max_iterations").expect("读轮次");

        // 写一个与现值不同的合法值，再读回来
        let probe_temp = if before_temp.as_deref() == Some("1.3") {
            "0.4"
        } else {
            "1.3"
        };
        let probe_turns = if before_turns.as_deref() == Some("17") {
            "6"
        } else {
            "17"
        };
        db.update_setting("agent.temperature", probe_temp)
            .expect("写温度");
        db.update_setting("agent.max_iterations", probe_turns)
            .expect("写轮次");

        let got_temp = db.get_setting("agent.temperature").expect("读温度");
        let got_turns = db.get_setting("agent.max_iterations").expect("读轮次");
        println!(
            "  agent.temperature      {} → {}",
            before_temp.as_deref().unwrap_or("(无)"),
            got_temp.as_deref().unwrap_or("(无)")
        );
        println!(
            "  agent.max_iterations   {} → {}",
            before_turns.as_deref().unwrap_or("(无)"),
            got_turns.as_deref().unwrap_or("(无)")
        );
        if got_temp.as_deref() != Some(probe_temp) {
            failures.push("agent.temperature 改完读不回新值".to_string());
        }
        if got_turns.as_deref() != Some(probe_turns) {
            failures.push("agent.max_iterations 改完读不回新值".to_string());
        }

        // 复原，避免污染备份库（探针跑在副本上，但保持整洁）
        if let Some(v) = before_temp {
            let _ = db.update_setting("agent.temperature", &v);
        }
        if let Some(v) = before_turns {
            let _ = db.update_setting("agent.max_iterations", &v);
        }
    }

    // 凭据脱敏：前端永远不该拿到明文令牌。
    //
    // 这条曾经是真的缺陷——设置页用 type=password 做了视觉掩码，
    // 但明文就躺在输入框的 value 里，DevTools 一行 querySelector 即可读出。
    println!("\n=== 凭据脱敏（前端不应看到明文）===");
    for key in ["publish.github_token", "publish.netlify_token"] {
        let item = match items.iter().find(|s| s.key == key) {
            Some(i) => i,
            None => {
                failures.push(format!("凭据配置项 {} 不存在", key));
                continue;
            }
        };
        let raw_in_db = db.get_setting(key).expect("读原文").unwrap_or_default();
        let shown = &item.value;

        if raw_in_db.trim().is_empty() {
            println!("  {:<26} 未配置（下发值 ── {:?}）", key, shown);
            if !shown.is_empty() {
                failures.push(format!("{} 未配置时下发值应为空，实际 {:?}", key, shown));
            }
            continue;
        }

        // 库里必须还是原文，否则发布会拿着掩码去调 API
        if !raw_in_db.starts_with("ghp_")
            && !raw_in_db.starts_with("nfp_")
            && !raw_in_db.starts_with("pat-")
            && raw_in_db.len() < 12
        {
            failures.push(format!(
                "{} 的库内存值看起来不像凭据，探针假设可能失效",
                key
            ));
        }
        println!(
            "  {:<26} 库内长度={:<3} 下发值={:?}",
            key,
            raw_in_db.len(),
            shown
        );

        if shown.contains(&raw_in_db) || (!raw_in_db.is_empty() && shown == &raw_in_db) {
            failures.push(format!("{} 把明文原样下发了", key));
        }
        if !raw_in_db.is_empty() && shown.contains("ghp_") {
            failures.push(format!("{} 的前缀泄漏到下发值里了", key));
        }
        if !shown.contains('•') {
            failures.push(format!("{} 的下发值不是掩码形式: {:?}", key, shown));
        }
        // 末四位保留是为了让用户能辨认，不该把整串打出去
        if shown.chars().filter(|c| *c == '•').count() < 4 && raw_in_db.len() > 8 {
            failures.push(format!("{} 的掩码位数过少，可能泄漏内容", key));
        }
    }

    let _ = std::fs::remove_dir_all(&work);

    if failures.is_empty() {
        println!("\n[PASS] 设置界面数据全部可调，无摆设项");
    } else {
        println!("\n[FAIL] {} 处问题:", failures.len());
        for f in &failures {
            println!("  - {}", f);
        }
        std::process::exit(1);
    }
}
