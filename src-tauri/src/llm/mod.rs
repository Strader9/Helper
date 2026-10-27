//! LLM 模块
//!
//! 封装与 Ollama 本地服务的通信。
//! 支持流式聊天、模型列表查询、结构化输出（tool calls）。

pub mod ollama;
pub mod openai;

pub use ollama::{ChatChunk, FunctionCall, OllamaClient, OllamaMessage, OllamaModel, ToolCall};
