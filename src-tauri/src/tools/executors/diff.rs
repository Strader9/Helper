//! 文档差异对比（P3）。
//!
//! 纯文本文件的行级 diff：用最长公共子序列对齐两边的行，
//! 产出 unified 风格的 `-` / `+` 片段与增删统计。无第三方依赖。

use async_trait::async_trait;
use serde_json::json;

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;
const MAX_LINES: usize = 20_000;

/// 行级 LCS（O(N*M) 空间用滚动数组），对 2 万行内的文件足够。
fn lcs_len(a: &[String], b: &[String]) -> Vec<Vec<u32>> {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    dp
}

/// 用 dp 回溯生成 unified 风格的 diff 行（前缀 `-` / `+` / ` `）。
fn build_diff(a: &[String], b: &[String]) -> (Vec<String>, usize, usize) {
    let dp = lcs_len(a, b);
    let (mut i, mut j) = (0usize, 0usize);
    let mut out: Vec<String> = Vec::new();
    let mut added = 0usize;
    let mut removed = 0usize;

    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            out.push(format!(" {}", a[i]));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || dp[i][j + 1] >= dp[i + 1][j]) {
            out.push(format!("+{}", b[j]));
            added += 1;
            j += 1;
        } else {
            out.push(format!("-{}", a[i]));
            removed += 1;
            i += 1;
        }
    }
    (out, added, removed)
}

fn read_lines(path: &str) -> AppResult<Vec<String>> {
    let meta = std::fs::metadata(path)
        .map_err(|e| AppError::InvalidArgument(format!("文件不可读 {}: {}", path, e)))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(AppError::InvalidArgument("文件超过 5MB，暂不支持 diff".into()));
    }
    let content = std::fs::read_to_string(path)
        .map_err(|e| AppError::InvalidArgument(format!("读取失败 {}: {}", path, e)))?;
    let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    if lines.len() > MAX_LINES {
        return Err(AppError::InvalidArgument("文件行数超过 2 万，暂不支持 diff".into()));
    }
    Ok(lines)
}

pub struct DiffDocumentsTool;

#[async_trait]
impl AgentTool for DiffDocumentsTool {
    fn name(&self) -> &'static str {
        "diff_documents"
    }

    fn description(&self) -> &'static str {
        "对比两个纯文本文件，产出行级差异（- 删除 / + 新增）与增删统计。用于「这版和上一版差在哪」。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path_a": { "type": "string", "description": "旧文件（基准）路径" },
                "path_b": { "type": "string", "description": "新文件路径" },
                "max_diff_lines": { "type": "integer", "description": "diff 结果最多返回多少行，默认 200" }
            },
            "required": ["path_a", "path_b"],
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let a = args["path_a"].as_str().ok_or_else(|| AppError::InvalidArgument("path_a is required".into()))?;
        let b = args["path_b"].as_str().ok_or_else(|| AppError::InvalidArgument("path_b is required".into()))?;
        let max_lines = args["max_diff_lines"].as_i64().unwrap_or(200).clamp(10, 2000) as usize;

        let a_lines = read_lines(a)?;
        let b_lines = read_lines(b)?;
        let (diff, added, removed) = build_diff(&a_lines, &b_lines);

        let truncated = diff.len() > max_lines;
        let shown: Vec<String> = diff.into_iter().take(max_lines).collect();

        Ok(ToolResult::ok(json!({
            "path_a": a,
            "path_b": b,
            "a_lines": a_lines.len(),
            "b_lines": b_lines.len(),
            "added": added,
            "removed": removed,
            "changed": added > 0 || removed > 0,
            "diff_lines_shown": shown.len(),
            "truncated": truncated,
            "diff": shown,
        })))
    }
}
