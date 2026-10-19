//! 知识库工具
//!
//! 把本地文本文件索引进库，再按关键词检索原文片段。
//! 索引本身只是"记账"，真正被用户感知的是检索——
//! 所以 `search_knowledge` 必须返回命中位置附近的上下文，而不是块首。

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::error::{AppError, AppResult};
use crate::knowledge;
use crate::tools::{AgentTool, RiskLevel, ToolResult};

fn connection() -> AppResult<std::sync::Arc<rusqlite::Connection>> {
    crate::memory::open_connection()
        .map(std::sync::Arc::new)
        .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))
}

/// 递归收集可索引文件
fn collect_files(root: &Path, recursive: bool, limit: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if files.len() >= limit {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                // 依赖目录里没有用户知识，扫进去只会污染检索结果
                let skip = matches!(
                    name.as_str(),
                    "node_modules" | ".git" | "target" | "dist" | "build" | "__pycache__" | ".venv"
                );
                if recursive && !skip {
                    stack.push(path);
                }
                continue;
            }
            if knowledge::is_indexable(&path) {
                files.push(path);
            }
            if files.len() >= limit {
                break;
            }
        }
    }
    files
}

// ============================================================
// 索引
// ============================================================

/// 把文件或目录索引进知识库
pub struct IndexDocumentTool;

#[async_trait]
impl AgentTool for IndexDocumentTool {
    fn name(&self) -> &'static str {
        "index_document"
    }

    fn description(&self) -> &'static str {
        "把本地文本文件（或整个目录）索引进知识库，之后可以用 search_knowledge 检索原文。用户说「记住这个文件夹」「以后能查这些资料吗」时使用。内容没变的文件不会重复写入。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "文件或目录路径" },
                "recursive": { "type": "boolean", "description": "目录是否递归子目录，默认 true" },
                "max_files": { "type": "integer", "description": "本次最多索引多少个文件，默认 200" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path_str = args["path"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;
        let recursive = args["recursive"].as_bool().unwrap_or(true);
        let max_files = args["max_files"].as_u64().unwrap_or(200).min(2000) as usize;

        let root = Path::new(path_str);
        if !root.exists() {
            return Ok(ToolResult::err(
                "PATH_NOT_FOUND",
                &format!("路径不存在: {}", path_str),
            ));
        }

        let conn = connection()?;
        let targets: Vec<PathBuf> = if root.is_file() {
            vec![root.to_path_buf()]
        } else {
            collect_files(root, recursive, max_files)
        };

        let mut indexed = 0usize;
        let mut unchanged = 0usize;
        let mut failed: Vec<serde_json::Value> = Vec::new();

        for target in &targets {
            match knowledge::index_file(&conn, target) {
                Ok(outcome) => {
                    if outcome.unchanged {
                        unchanged += 1;
                    } else {
                        indexed += 1;
                    }
                }
                Err(e) => failed.push(serde_json::json!({
                    "path": target.to_string_lossy(),
                    "error": e.to_string(),
                })),
            }
        }

        let (docs, chunks, _chars) = knowledge::stats(&conn)?;

        Ok(ToolResult::ok(serde_json::json!({
            "path": path_str,
            "kind": if root.is_file() { "file" } else { "dir" },
            "scanned": targets.len(),
            "indexed": indexed,
            "unchanged": unchanged,
            "failed": failed,
            "library_documents": docs,
            "library_chunks": chunks,
        })))
    }
}

// ============================================================
// 检索
// ============================================================

/// 检索知识库
pub struct SearchKnowledgeTool;

#[async_trait]
impl AgentTool for SearchKnowledgeTool {
    fn name(&self) -> &'static str {
        "search_knowledge"
    }

    fn description(&self) -> &'static str {
        "在已索引的本地文档里检索原文，返回命中的片段与出处。用户问「我之前写的那份文档里怎么说的」时用它，比 search_content 更快（只查索引，不扫磁盘）。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "关键词，多个词用空格分隔" },
                "limit": { "type": "integer", "description": "最多返回多少条片段，默认 5" }
            },
            "required": ["query"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let query = args["query"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("query is required".to_string()))?;
        let limit = args["limit"].as_u64().unwrap_or(5).min(20) as usize;

        let conn = connection()?;
        let hits = knowledge::search(&conn, query, limit)?;

        Ok(ToolResult::ok(serde_json::json!({
            "query": query,
            "hits": hits,
            "count": hits.len(),
        })))
    }
}

// ============================================================
// 列出
// ============================================================

/// 列出知识库里已索引的文档
pub struct ListKnowledgeTool;

#[async_trait]
impl AgentTool for ListKnowledgeTool {
    fn name(&self) -> &'static str {
        "list_knowledge"
    }

    fn description(&self) -> &'static str {
        "列出知识库里已经索引了哪些文档，含分块数、索引时间和源文件是否还在。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "description": "最多返回多少条，默认 50" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let limit = args["limit"].as_u64().unwrap_or(50).min(500) as usize;
        let conn = connection()?;
        let docs = knowledge::list_documents(&conn, limit)?;
        let (total_docs, total_chunks, total_chars) = knowledge::stats(&conn)?;

        Ok(ToolResult::ok(serde_json::json!({
            "documents": docs,
            "count": docs.len(),
            "total_documents": total_docs,
            "total_chunks": total_chunks,
            "total_chars": total_chars,
        })))
    }
}

// ============================================================
// 移除索引
// ============================================================

/// 从知识库移除某个文档的索引
pub struct ForgetDocumentTool;

#[async_trait]
impl AgentTool for ForgetDocumentTool {
    fn name(&self) -> &'static str {
        "forget_document"
    }

    fn description(&self) -> &'static str {
        "把某个文件从知识库里移除（只删索引，不删磁盘上的文件）。用户说「别再查那份文档了」时使用。"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "文件路径（需与索引时一致）" }
            },
            "required": ["path"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let path = args["path"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("path is required".to_string()))?;

        let conn = connection()?;
        let removed = knowledge::remove_document(&conn, path)?;
        let (docs, chunks, _) = knowledge::stats(&conn)?;

        Ok(ToolResult::ok(serde_json::json!({
            "path": path,
            "removed": removed,
            "library_documents": docs,
            "library_chunks": chunks,
        })))
    }
}
