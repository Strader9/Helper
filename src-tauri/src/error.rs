use thiserror::Error;

/// 全局错误类型
///
/// 所有模块的错误统一转换为此类型，便于上层处理和审计。
#[derive(Debug, Error)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("Migration error: {0}")]
    Migration(String),

    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Tool execution failed: {0}")]
    ToolExecution(String),

    #[error("Invalid argument: {0}")]
    InvalidArgument(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Ollama error: {0}")]
    Ollama(String),

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Tool call parse error: {0}")]
    ToolCallParse(String),

    #[error("Conversation not found: {0}")]
    ConversationNotFound(String),

    #[error("Max iterations reached")]
    MaxIterations,

    #[error("Stream closed")]
    StreamClosed,

    #[error("Task not found: {0}")]
    TaskNotFound(String),
}

pub type AppResult<T> = Result<T, AppError>;
