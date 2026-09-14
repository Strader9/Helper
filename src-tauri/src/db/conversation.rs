//! 对话管理模块
//!
//! 管理 chat_sessions 和 chat_messages 表的 CRUD 操作。
//! 所有对话数据持久化到 SQLite。

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::AppResult;

/// 对话会话
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    pub is_pinned: bool,
    pub is_archived: bool,
}

/// 对话消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub id: i64,
    pub session_id: String,
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<i64>,
}

/// 创建新对话的请求
#[derive(Debug, Deserialize)]
pub struct CreateConversationRequest {
    pub title: Option<String>,
    pub model: Option<String>,
}

/// 对话管理器
pub struct ConversationManager<'a> {
    conn: &'a Connection,
}

impl<'a> ConversationManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// 创建新对话
    pub fn create(
        &self,
        title: Option<String>,
    ) -> AppResult<Conversation> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Local::now().to_rfc3339();
        let title = title.unwrap_or_else(|| "新对话".to_string());

        self.conn.execute(
            "INSERT INTO chat_sessions (id, title, created_at, updated_at, message_count, is_pinned, is_archived)
             VALUES (?1, ?2, ?3, ?4, 0, 0, 0)",
            params![&id, &title, &now, &now],
        )?;

        Ok(Conversation {
            id,
            title,
            created_at: now.clone(),
            updated_at: now,
            message_count: 0,
            last_message: None,
            is_pinned: false,
            is_archived: false,
        })
    }

    /// 获取所有对话（按 updated_at 倒序）
    pub fn list_all(&self) -> AppResult<Vec<Conversation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, created_at, updated_at, message_count, last_message, is_pinned, is_archived
             FROM chat_sessions
             WHERE is_archived = 0
             ORDER BY is_pinned DESC, updated_at DESC"
        )?;

        let conversations = stmt.query_map([], |row| {
            Ok(Conversation {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
                message_count: row.get(4)?,
                last_message: row.get(5)?,
                is_pinned: row.get::<_, i64>(6)? != 0,
                is_archived: row.get::<_, i64>(7)? != 0,
            })
        })?;

        Ok(conversations.collect::<Result<Vec<_>, _>>()?)
    }

    /// 获取单个对话
    pub fn get(&self, id: &str) -> AppResult<Option<Conversation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, created_at, updated_at, message_count, last_message, is_pinned, is_archived
             FROM chat_sessions WHERE id = ?1"
        )?;

        let mut rows = stmt.query(params![id])?;

        if let Some(row) = rows.next()? {
            Ok(Some(Conversation {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
                message_count: row.get(4)?,
                last_message: row.get(5)?,
                is_pinned: row.get::<_, i64>(6)? != 0,
                is_archived: row.get::<_, i64>(7)? != 0,
            }))
        } else {
            Ok(None)
        }
    }

    /// 追加消息到对话
    pub fn append_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        tool_calls: Option<&str>,
        tool_call_id: Option<&str>,
    ) -> AppResult<i64> {
        let now = chrono::Local::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO chat_messages (session_id, role, content, tool_calls, tool_call_id, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session_id, role, content, tool_calls, tool_call_id, &now],
        )?;

        // 更新会话的 message_count 和 last_message
        let truncated = if content.len() > 100 {
            format!("{}...", &content[..100])
        } else {
            content.to_string()
        };

        self.conn.execute(
            "UPDATE chat_sessions
             SET message_count = message_count + 1,
                 last_message = ?2,
                 updated_at = ?3
             WHERE id = ?1",
            params![session_id, &truncated, &now],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// 获取对话的所有消息
    pub fn get_messages(&self, session_id: &str) -> AppResult<Vec<ConversationMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, role, content, tool_calls, tool_call_id, timestamp, tokens_used
             FROM chat_messages
             WHERE session_id = ?1
             ORDER BY timestamp ASC"
        )?;

        let messages = stmt.query_map(params![session_id], |row| {
            Ok(ConversationMessage {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                tool_calls: row.get(4)?,
                tool_call_id: row.get(5)?,
                timestamp: row.get(6)?,
                tokens_used: row.get(7)?,
            })
        })?;

        Ok(messages.collect::<Result<Vec<_>, _>>()?)
    }

    /// 删除对话（级联删除消息）
    pub fn delete(&self, id: &str) -> AppResult<()> {
        self.conn.execute(
            "DELETE FROM chat_sessions WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// 归档对话
    pub fn archive(&self, id: &str) -> AppResult<()> {
        self.conn.execute(
            "UPDATE chat_sessions SET is_archived = 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// 置顶/取消置顶对话
    pub fn toggle_pin(&self, id: &str) -> AppResult<bool> {
        let current: i64 = self
            .conn
            .query_row(
                "SELECT is_pinned FROM chat_sessions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )?;

        let new_value = if current == 1 { 0 } else { 1 };

        self.conn.execute(
            "UPDATE chat_sessions SET is_pinned = ?1 WHERE id = ?2",
            params![new_value, id],
        )?;

        Ok(new_value == 1)
    }
}
