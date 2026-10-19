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

pub use contract::{contract_for, coverage, CapabilityContract, CoverageReport, SideEffect, VerificationStrategy};
pub use executor::{ToolExecutor, ToolCallRequest, ToolExecutionResult};
pub use receipt::{ToolReceipt, VerificationOutcome, VerificationStatus};
pub use registry::{ToolRegistry, AgentTool, ToolResult, RiskLevel, ToolCategory};
