//! 记忆工具（V17 新增）
//!
//! 提供 remember / recall / forget 三个工具，让 AI 能管理长期记忆。
//! 风险等级：recall SAFE，remember LOW，forget MEDIUM。

use async_trait::async_trait;
use serde_json::json;

use crate::error::{AppError, AppResult};
use crate::memory::{MemoryEngine, MemoryType};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

// ============================================================
// remember 工具 — 显式记住信息
// ============================================================

/// 显式记住信息
pub struct RememberTool;

#[async_trait]
impl AgentTool for RememberTool {
    fn name(&self) -> &'static str {
        "remember"
    }

    fn description(&self) -> &'static str {
        "记住一条信息，用于长期记忆。当用户说'记住xxx'、'别忘了xxx'、'以后记得xxx'，或你认为某条信息值得长期保存（用户偏好、重要事实、项目路径等）时使用此工具。记忆会在后续对话中自动被检索和引用。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "content": {
                    "type": "string",
                    "description": "要记住的内容（简洁、完整、可独立理解）"
                },
                "memory_type": {
                    "type": "string",
                    "enum": ["user_preference", "task_history", "knowledge", "conversation_summary", "app_usage"],
                    "description": "记忆类型（可选，默认 knowledge）"
                },
                "importance": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 100,
                    "description": "重要性 0-100（可选，默认 50）"
                }
            },
            "required": ["content"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let content = args["content"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("content is required".to_string()))?;

        let memory_type = args["memory_type"]
            .as_str()
            .map(MemoryType::from_str)
            .unwrap_or(MemoryType::Knowledge);

        let importance = args["importance"]
            .as_u64()
            .map(|v| v.min(100) as u8)
            .unwrap_or(50);

        let conn = crate::memory::open_connection()
            .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;

        let id = MemoryEngine::add_memory(&conn, memory_type, content, importance, "user_explicit")?;

        Ok(ToolResult::ok(json!({
            "success": true,
            "memory_id": id,
            "memory_type": memory_type.as_str(),
            "content": content,
            "importance": importance,
            "message": "已记住"
        })))
    }
}

// ============================================================
// recall 工具 — 检索记忆
// ============================================================

/// 检索记忆
pub struct RecallTool;

#[async_trait]
impl AgentTool for RecallTool {
    fn name(&self) -> &'static str {
        "recall"
    }

    fn description(&self) -> &'static str {
        "从长期记忆中检索相关信息。当你需要回忆之前记住的用户偏好、历史任务、知识事实时使用此工具。输入关键词即可检索最相关的记忆。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "检索关键词（用于匹配记忆内容和关键词）"
                },
                "memory_type": {
                    "type": "string",
                    "enum": ["user_preference", "task_history", "knowledge", "conversation_summary", "app_usage"],
                    "description": "按类型过滤（可选）"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20,
                    "description": "返回数量上限（可选，默认 5）"
                }
            },
            "required": ["query"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let query = args["query"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("query is required".to_string()))?;

        let memory_type = args["memory_type"].as_str().map(MemoryType::from_str);
        let limit = args["limit"]
            .as_u64()
            .map(|v| v.min(20) as usize)
            .unwrap_or(5);

        let conn = crate::memory::open_connection()
            .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;

        let memories = MemoryEngine::recall(&conn, query, memory_type, limit)?;

        let items: Vec<serde_json::Value> = memories
            .iter()
            .map(|m| {
                json!({
                    "id": m.id,
                    "memory_type": m.memory_type.as_str(),
                    "content": m.content,
                    "importance": m.importance,
                    "access_count": m.access_count,
                    "created_at": m.created_at,
                    "source": m.source
                })
            })
            .collect();

        Ok(ToolResult::ok(json!({
            "success": true,
            "count": items.len(),
            "query": query,
            "memories": items
        })))
    }
}

// ============================================================
// forget 工具 — 删除记忆
// ============================================================

/// 删除记忆
pub struct ForgetTool;

#[async_trait]
impl AgentTool for ForgetTool {
    fn name(&self) -> &'static str {
        "forget"
    }

    fn description(&self) -> &'static str {
        "删除一条记忆。当用户说'忘掉xxx'、'删除那条记忆'，或某条记忆已过时需要清理时使用此工具。可以通过 memory_id 精确删除，或通过 query 关键词匹配删除。风险等级 MEDIUM，需要用户确认。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "memory_id": {
                    "type": "string",
                    "description": "要删除的记忆 ID（与 query 二选一）"
                },
                "query": {
                    "type": "string",
                    "description": "按关键词匹配删除（与 memory_id 二选一）"
                }
            },
            "required": []
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let memory_id = args["memory_id"].as_str();
        let query = args["query"].as_str();

        if memory_id.is_none() && query.is_none() {
            return Ok(ToolResult::err(
                "INVALID_PARAMS",
                "必须提供 memory_id 或 query 之一",
            ));
        }

        let conn = crate::memory::open_connection()
            .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;

        if let Some(id) = memory_id {
            let deleted = MemoryEngine::forget_by_id(&conn, id)?;
            if deleted {
                Ok(ToolResult::ok(json!({
                    "success": true,
                    "deleted": 1,
                    "memory_id": id,
                    "message": "记忆已删除"
                })))
            } else {
                Ok(ToolResult::err("NOT_FOUND", &format!("未找到 ID 为 {} 的记忆", id)))
            }
        } else if let Some(q) = query {
            let deleted = MemoryEngine::forget_by_query(&conn, q)?;
            Ok(ToolResult::ok(json!({
                "success": true,
                "deleted": deleted,
                "query": q,
                "message": format!("已删除 {} 条匹配记忆", deleted)
            })))
        } else {
            unreachable!()
        }
    }
}
