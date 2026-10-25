//! 模型职责路由与显存运行时协调。
//!
//! 这里不执行模型推理，也不直接操作 GPU。它只保存经过确认的职责分工，并把
//! Jarvis 与 Ollama 的排他状态转换集中起来；实际进程启停由后续交接执行器完成。

mod handoff;
mod policy;
mod runtime;

pub use handoff::{HandoffPacket, HandoffResult, ResourceRef};
pub use policy::{Capability, ModelRole, RouteDecision, RoutingPolicy};
pub use runtime::{InferenceRuntimeManager, InferenceRuntimeState, RuntimeSnapshot};
