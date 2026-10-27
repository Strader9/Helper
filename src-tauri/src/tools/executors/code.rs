//! JS 沙箱执行工具（code_execution）
//!
//! 对齐早期原型的 `code_execution`：在隔离的 JS 引擎堆里做纯计算 /
//! 数据变换。**没有文件系统、网络、Node API、import 或宿主定时器**——
//! 引擎是纯 Rust 实现（Boa，ES 规范），默认不注入任何宿主能力，
//! 沙箱边界由构造保证，而不是靠"逐个禁用"。
//!
//! 数据通过 `input` 全局注入，代码把结果作为最后一个表达式返回。
//! 返回 `output`（结果的字符串表示）与 `result`（JSON 形式，若可序列化）。

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

const MAX_CODE_BYTES: usize = 64 * 1024; // 64 KiB，与早期原型一致
const MAX_INPUT_BYTES: usize = 128 * 1024;

/// 在隔离堆里执行一段 JS，返回 (output 字符串, result JSON)。
///
/// 放独立函数便于单测；错误统一转成 AppError。
fn run_js_sandbox(code: &str, input: Option<&Value>) -> AppResult<(String, Value)> {
    if code.len() > MAX_CODE_BYTES {
        return Err(AppError::InvalidArgument(format!(
            "代码超过 {} 字节上限",
            MAX_CODE_BYTES
        )));
    }
    if let Some(i) = input {
        if serde_json::to_string(i).map(|s| s.len()).unwrap_or(0) > MAX_INPUT_BYTES {
            return Err(AppError::InvalidArgument(format!(
                "input 超过 {} 字节上限",
                MAX_INPUT_BYTES
            )));
        }
    }

    let mut context = boa_engine::Context::default();

    // 注入 input 全局
    if let Some(input) = input {
        let js_input = boa_engine::JsValue::from_json(input, &mut context)
            .map_err(|e| AppError::Internal(format!("input 注入失败: {}", e)))?;
        context
            .register_global_property(
                boa_engine::string::JsString::from("input"),
                js_input,
                boa_engine::property::Attribute::all(),
            )
            .map_err(|e| AppError::Internal(format!("input 注入失败: {}", e)))?;
    }

    // 执行
    let result = context
        .eval(boa_engine::Source::from_bytes(code.as_bytes()))
        .map_err(|e| AppError::InvalidArgument(format!("JS 执行失败: {}", e)))?;

    let output = result.display().to_string();

    // 序列化为 JSON；undefined / 不可序列化时降级为 null
    let result_json = result
        .to_json(&mut context)
        .ok()
        .flatten()
        .unwrap_or(Value::Null);

    Ok((output, result_json))
}

// ============================================================
// code_execution
// ============================================================

pub struct CodeExecutionTool;

#[async_trait]
impl AgentTool for CodeExecutionTool {
    fn name(&self) -> &'static str {
        "code_execution"
    }

    fn description(&self) -> &'static str {
        "在隔离的 JS 沙箱里执行一段 JavaScript，做纯计算或数据变换。代码把结果作为最后一个表达式返回；数据通过全局 input 传入，代码里直接读 input。无文件系统、网络、Node API、import 或定时器。代码上限 64 KiB。用于复杂计算、格式化、数据清洗等纯函数式任务。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "code": { "type": "string", "description": "要执行的 JS 脚本，按原样求值，不是函数体。用最后一个表达式作为结果；不支持顶层 return。" },
                "input": { "description": "可选的 JSON 值，作为全局 input 暴露给代码。传实际解码后的数据，不要传 JSON.stringify 的副本。" }
            },
            "required": ["code"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let code = args["code"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("code is required".to_string()))?;
        let input = args.get("input");

        // 放 spawn_blocking 兜底：正常计算微秒级完成，这里只为隔离可能的死循环
        // 对 tokio worker 的影响（超时后阻塞线程泄漏是已知可接受限制）。
        let code_owned = code.to_string();
        let input_owned = input.cloned();
        let result =
            tokio::task::spawn_blocking(move || run_js_sandbox(&code_owned, input_owned.as_ref()))
                .await
                .map_err(|e| AppError::Internal(format!("JS 沙箱任务失败: {}", e)))?;

        let (output, result_json) = result?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "completed",
            "output": output,
            "result": result_json,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evals_arithmetic_and_returns_output() {
        let (out, res) = run_js_sandbox("1 + 2 * 3", None).unwrap();
        assert!(out.contains('7'), "output 应含 7，实际: {out}");
        assert_eq!(res, json!(7));
    }

    #[test]
    fn reads_injected_input() {
        let input = json!({"xs": [1, 2, 3, 4]});
        let (out, res) =
            run_js_sandbox("input.xs.reduce((a, b) => a + b, 0)", Some(&input)).unwrap();
        assert!(out.contains("10"), "output 应含 10，实际: {out}");
        assert_eq!(res, json!(10));
    }

    #[test]
    fn returns_string_result() {
        let (out, res) = run_js_sandbox("\"hello\" + \" \" + \"world\"", None).unwrap();
        assert!(out.contains("hello world"), "实际: {out}");
        assert_eq!(res, json!("hello world"));
    }

    #[test]
    fn rejects_oversized_code() {
        let big = " ".repeat(MAX_CODE_BYTES + 1);
        assert!(run_js_sandbox(&big, None).is_err());
    }

    #[test]
    fn reports_syntax_error() {
        // 注意：`let = 1` 在非严格模式下是合法的（let 可作标识符），
        // 故用 `1 +` 这种确定非法的表达式验证错误路径。
        assert!(run_js_sandbox("1 +", None).is_err());
    }
}
