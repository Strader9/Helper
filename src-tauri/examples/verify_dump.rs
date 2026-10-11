//! 把一个已生成的 HTML 丢进功能自检，打印全部发现。用来校准规则、复盘历史产物。
//!
//! 这是验证用的临时入口，不参与应用打包。
//!
//! 用法：
//! ```text
//! cargo run --release --example verify_dump -- <页面.html> [需求.md]
//! ```

use pc_guardian_lib::webgen;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("用法: verify_dump <页面.html> [需求.md]");
    let html = std::fs::read_to_string(path).expect("读取页面失败");
    let spec = args
        .get(2)
        .map(|spec_path| std::fs::read_to_string(spec_path).expect("读取需求失败"))
        .unwrap_or_default();

    println!("页面：{path}（{} 字符）", html.chars().count());
    let script = webgen::merged_script(&html);
    println!("脚本：{} 字符", script.chars().count());
    println!(
        "配平：{}",
        webgen::delimiter_problem(&script).unwrap_or_else(|| "正常".to_string())
    );
    println!();

    let issues = webgen::verify_page(&html, &spec);
    if issues.is_empty() {
        println!("自检通过，没有发现问题。");
    } else {
        println!("自检发现 {} 项问题：", issues.len());
        for issue in &issues {
            println!("  {}", issue.log_line());
        }
    }
    println!();

    match webgen::restore_script(&html) {
        Some((fixed, reason)) => {
            let fixed_script = webgen::merged_script(&fixed);
            println!("抢救可用：{reason}");
            println!(
                "  {} 字符 → {} 字符",
                html.chars().count(),
                fixed.chars().count()
            );
            println!(
                "  抢救后配平：{}",
                webgen::delimiter_problem(&fixed_script).unwrap_or_else(|| "正常".to_string())
            );
            let after = webgen::verify_page(&fixed, &spec);
            println!("  抢救后问题数：{}（原 {}）", after.len(), issues.len());
            if let Some(out) = args.get(3) {
                let _ = std::fs::write(out, &fixed);
                println!("  已写出：{out}");
            }
        }
        None => println!("抢救不适用（页面已经是自洽的，或没有更好的切点）。"),
    }
}
