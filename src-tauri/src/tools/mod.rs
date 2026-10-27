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

use std::sync::{Mutex, OnceLock};

fn session_slot() -> &'static Mutex<Option<String>> {
    static SLOT: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// 执行工具前由 executor 注入当前会话 ID。
///
/// 工具的 `execute` 只收 args，拿不到会话——但任务编排类工具必须知道
/// 这条指令属于哪个会话，否则会把不同对话的任务混到同一个列表里。
pub fn set_current_session(session_id: &str) {
    if let Ok(mut slot) = session_slot().lock() {
        *slot = Some(session_id.to_string());
    }
}

/// 读当前会话 ID；没有执行上下文时返回 `unknown`。
pub fn current_session() -> String {
    session_slot()
        .lock()
        .ok()
        .and_then(|slot| slot.clone())
        .unwrap_or_else(|| "unknown".to_string())
}
