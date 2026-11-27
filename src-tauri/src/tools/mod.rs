//! 工具模块
//!
//! 所有工具的注册和管理。
//! 三层闸门第一层：ToolRegistry —— 所有工具必须在此注册，未注册工具一律拒绝。
//! 三层闸门第三层：ToolExecutor —— 实际执行工具并记录审计日志。

pub mod contract;
pub mod executor;
pub mod executors;
pub mod receipt;
pub mod registry;

pub use contract::{
    contract_for, coverage, CapabilityContract, CoverageReport, SideEffect, VerificationStrategy,
};
pub use executor::{ToolCallRequest, ToolExecutionResult, ToolExecutor};
pub use receipt::{ToolReceipt, VerificationOutcome, VerificationStatus};
pub use registry::{AgentTool, RiskLevel, ToolCategory, ToolRegistry, ToolResult};

// ============================================================
// 当前执行上下文
// ============================================================

tokio::task_local! {
    pub(crate) static TOOL_SESSION_ID: String;
}

/// 读当前会话 ID；没有执行上下文时返回 `unknown`。
pub fn current_session() -> String {
    TOOL_SESSION_ID
        .try_with(String::clone)
        .unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_and_nested_tools_keep_their_session() {
        let first = TOOL_SESSION_ID.scope("first".into(), async {
            tokio::task::yield_now().await;
            assert_eq!(current_session(), "first");
            let nested = TOOL_SESSION_ID.scope("nested".into(), async { current_session() });
            assert_eq!(nested.await, "nested");
            assert_eq!(current_session(), "first");
        });
        let second = TOOL_SESSION_ID.scope("second".into(), async {
            tokio::task::yield_now().await;
            assert_eq!(current_session(), "second");
        });
        tokio::join!(first, second);
        assert_eq!(current_session(), "unknown");
    }
}
