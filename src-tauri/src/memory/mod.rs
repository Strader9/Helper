//! 长期记忆模块（V17 新增）
//!
//! MemoryEngine 负责管理 AI 的长期记忆，包括用户偏好、任务历史、知识事实、对话摘要、应用使用习惯。
//! 支持记忆的增删查、时间衰减检索、相关记忆获取（用于注入 System Prompt）。
//!
//! 设计原则：不引入向量数据库，关键词用简单实现（标点分割 + 停用词过滤）。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::OnceLock;

/// 全局数据库路径（在 AppState::new 时设置）
static DB_PATH: OnceLock<PathBuf> = OnceLock::new();

/// 设置全局数据库路径（应用启动时调用一次）
pub fn set_db_path(path: PathBuf) {
    let _ = DB_PATH.set(path);
}

/// 获取全局数据库路径
pub fn get_db_path() -> Option<&'static PathBuf> {
    DB_PATH.get()
}

/// 打开一个短生命周期的数据库连接（用于工具层）
pub fn open_connection() -> Option<rusqlite::Connection> {
    let path = DB_PATH.get()?;
    let conn = rusqlite::Connection::open(path).ok()?;
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    let _ = conn.pragma_update(None, "busy_timeout", "5000");
    Some(conn)
}

// ============================================================
// 数据模型
// ============================================================

/// 记忆类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryType {
    /// 用户偏好（如"常用编辑器是VSCode"、"喜欢深色主题"）
    UserPreference,
    /// 任务历史摘要（如"2026-08-23 整理了下载文件夹"）
    TaskHistory,
    /// 知识/事实（如"用户的项目在D:\projects"）
    Knowledge,
    /// 对话摘要
    ConversationSummary,
    /// 应用使用习惯（如"每天早上打开Chrome和VSCode"）
    AppUsage,
}

impl MemoryType {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryType::UserPreference => "user_preference",
            MemoryType::TaskHistory => "task_history",
            MemoryType::Knowledge => "knowledge",
            MemoryType::ConversationSummary => "conversation_summary",
            MemoryType::AppUsage => "app_usage",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "user_preference" | "preference" => MemoryType::UserPreference,
            "task_history" | "history" => MemoryType::TaskHistory,
            "knowledge" | "fact" => MemoryType::Knowledge,
            "conversation_summary" | "summary" => MemoryType::ConversationSummary,
            "app_usage" | "usage" => MemoryType::AppUsage,
            _ => MemoryType::Knowledge,
        }
    }
}

/// 记忆条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub memory_type: MemoryType,
    pub content: String,
    pub keywords: Vec<String>,
    pub importance: u8,
    pub access_count: u32,
    pub created_at: String,
    pub last_accessed_at: String,
    pub expires_at: Option<String>,
    pub source: String,
}

impl MemoryItem {
    /// 创建新记忆（自动生成 ID、时间戳、关键词）
    pub fn new(
        memory_type: MemoryType,
        content: &str,
        importance: u8,
        source: &str,
    ) -> Self {
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let keywords = extract_keywords(content);
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            memory_type,
            content: content.to_string(),
            keywords,
            importance: importance.min(100),
            access_count: 0,
            created_at: now.clone(),
            last_accessed_at: now,
            expires_at: None,
            source: source.to_string(),
        }
    }
}

// ============================================================
// 关键词提取（简单实现）
// ============================================================

/// 中文停用词集合
fn stop_words() -> HashSet<&'static str> {
    let words = [
        "的", "了", "是", "在", "我", "有", "和", "就", "不", "人", "都", "一", "一个",
        "上", "也", "很", "到", "说", "要", "去", "你", "会", "着", "没有", "看", "好",
        "自己", "这", "那", "他", "她", "它", "们", "这个", "那个", "什么", "怎么",
        "为什么", "可以", "能", "应该", "需要", "想", "知道", "觉得", "认为",
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
        "have", "has", "had", "do", "does", "did", "will", "would", "could",
        "should", "may", "might", "must", "shall", "can", "need", "dare",
        "to", "of", "in", "for", "on", "with", "at", "by", "from", "as",
        "into", "through", "during", "before", "after", "above", "below",
        "between", "out", "off", "over", "under", "again", "further",
        "then", "once", "here", "there", "when", "where", "why", "how",
        "all", "both", "each", "few", "more", "most", "other", "some",
        "such", "no", "nor", "not", "only", "own", "same", "so", "than",
        "too", "very", "just", "because", "but", "and", "or", "if", "while",
        "about", "up", "it", "its", "i", "me", "my", "we", "our", "you",
        "your", "he", "him", "his", "she", "her", "they", "them", "their",
        "what", "which", "who", "whom", "this", "that", "these", "those",
        "am", "also", "get", "got", "make", "made", "take", "took", "go",
        "went", "come", "came", "see", "saw", "know", "knew", "think",
        "thought", "say", "said", "tell", "told", "use", "used", "find",
        "found", "give", "gave", "work", "worked", "like", "liked", "want",
        "wanted", "try", "tried", "ask", "asked", "need", "needed", "feel",
        "felt", "become", "became", "leave", "left", "put", "set", "let",
        "begin", "began", "seem", "seemed", "help", "helped", "show",
        "showed", "call", "called", "keep", "kept", "let", "mean", "meant",
        "hold", "held", "move", "moved", "live", "lived", "believe",
        "bring", "brought", "happen", "write", "wrote", "provide", "sit",
        "stand", "lose", "pay", "meet", "include", "continue", "learn",
        "change", "lead", "understand", "watch", "follow", "stop", "create",
        "speak", "read", "allow", "add", "spend", "grow", "open", "walk",
        "win", "offer", "remember", "love", "consider", "appear", "buy",
        "wait", "serve", "die", "send", "expect", "build", "stay", "fall",
        "cut", "reach", "kill", "remain", "suggest", "raise", "pass",
        "sell", "require", "report", "decide", "pull", "develop", "carry",
        "break", "receive", "agree", "hit", "produce", "eat", "cover",
        "catch", "draw", "choose", "could",
    ];
    words.iter().copied().collect()
}

/// 从文本中提取关键词（简单实现：标点分割 + 停用词过滤 + 长度过滤）
pub fn extract_keywords(text: &str) -> Vec<String> {
    let stop = stop_words();
    let mut keywords = Vec::new();
    let mut seen = HashSet::new();

    // 按标点和空白分割
    let tokens: Vec<&str> = text
        .split(|c: char| {
            c.is_whitespace()
                || c == ','
                || c == '，'
                || c == '.'
                || c == '。'
                || c == '!'
                || c == '！'
                || c == '?'
                || c == '？'
                || c == ';'
                || c == '；'
                || c == ':'
                || c == '：'
                || c == '"'
                || c == '"'
                || c == '\''
                || c == '\''
                || c == '('
                || c == ')'
                || c == '（'
                || c == '）'
                || c == '['
                || c == ']'
                || c == '【'
                || c == '】'
                || c == '/'
                || c == '\\'
                || c == '-'
                || c == '_'
                || c == '='
                || c == '+'
                || c == '*'
                || c == '&'
                || c == '|'
                || c == '<'
                || c == '>'
                || c == '#'
                || c == '@'
                || c == '$'
                || c == '%'
                || c == '^'
                || c == '~'
                || c == '`'
        })
        .filter(|s| !s.is_empty())
        .collect();

    for token in tokens {
        let lower = token.to_lowercase();
        // 过滤：停用词、长度<2、纯数字
        if lower.len() < 2 {
            continue;
        }
        if lower.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if stop.contains(lower.as_str()) {
            continue;
        }
        if seen.insert(lower.clone()) {
            keywords.push(lower);
        }
    }

    // 最多保留 20 个关键词
    keywords.truncate(20);
    keywords
}

// ============================================================
// MemoryEngine — 核心逻辑
// ============================================================

/// 记忆引擎
///
/// 封装记忆的增删查、时间衰减检索、相关记忆获取。
/// 数据库操作通过传入的 &rusqlite::Connection 执行（由调用方持有锁）。
pub struct MemoryEngine;

impl MemoryEngine {
    /// 添加记忆（自动去重：相同 content + type 不重复添加）
    pub fn add_memory(
        conn: &rusqlite::Connection,
        memory_type: MemoryType,
        content: &str,
        importance: u8,
        source: &str,
    ) -> rusqlite::Result<String> {
        // 去重检查
        let type_str = memory_type.as_str();
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) FROM memory_items WHERE memory_type = ?1 AND content = ?2",
            rusqlite::params![type_str, content],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        )?;

        if exists {
            // 更新访问时间和重要性（取较高值）
            conn.execute(
                "UPDATE memory_items SET last_accessed_at = datetime('now'), importance = MAX(importance, ?1) WHERE memory_type = ?2 AND content = ?3",
                rusqlite::params![importance as i64, type_str, content],
            )?;
            // 返回已有 ID
            return conn.query_row(
                "SELECT id FROM memory_items WHERE memory_type = ?1 AND content = ?2",
                rusqlite::params![type_str, content],
                |row| row.get(0),
            );
        }

        let item = MemoryItem::new(memory_type, content, importance, source);
        let keywords_json = serde_json::to_string(&item.keywords).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
            "INSERT INTO memory_items (id, memory_type, content, keywords, importance, access_count, created_at, last_accessed_at, expires_at, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                item.id,
                type_str,
                item.content,
                keywords_json,
                item.importance as i64,
                item.access_count as i64,
                item.created_at,
                item.last_accessed_at,
                item.expires_at,
                item.source,
            ],
        )?;

        Ok(item.id)
    }

    /// 检索记忆（按关键词匹配 + 类型过滤 + 重要性排序 + 时间衰减）
    pub fn recall(
        conn: &rusqlite::Connection,
        query: &str,
        memory_type: Option<MemoryType>,
        limit: usize,
    ) -> rusqlite::Result<Vec<MemoryItem>> {
        let query_keywords = extract_keywords(query);
        let type_filter = memory_type.map(|t| t.as_str().to_string());

        // 先获取所有未过期记忆
        let sql = if type_filter.is_some() {
            "SELECT id, memory_type, content, keywords, importance, access_count, created_at, last_accessed_at, expires_at, source
             FROM memory_items WHERE memory_type = ?1 AND (expires_at IS NULL OR expires_at > datetime('now'))
             ORDER BY importance DESC, created_at DESC LIMIT 200"
        } else {
            "SELECT id, memory_type, content, keywords, importance, access_count, created_at, last_accessed_at, expires_at, source
             FROM memory_items WHERE expires_at IS NULL OR expires_at > datetime('now')
             ORDER BY importance DESC, created_at DESC LIMIT 200"
        };

        let mut stmt = conn.prepare(sql)?;
        let items: Vec<MemoryItem> = if let Some(ref t) = type_filter {
            stmt.query_map(rusqlite::params![t], row_to_memory_item)?
        } else {
            stmt.query_map([], row_to_memory_item)?
        }
        .filter_map(|r| r.ok())
        .collect();

        // 计算相关性分数并排序
        let mut scored: Vec<(f64, MemoryItem)> = items
            .into_iter()
            .map(|item| {
                let score = compute_relevance_score(&item, &query_keywords);
                (score, item)
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let results: Vec<MemoryItem> = scored.into_iter().take(limit).map(|(_, item)| item).collect();

        // 更新访问计数
        for item in &results {
            let _ = conn.execute(
                "UPDATE memory_items SET access_count = access_count + 1, last_accessed_at = datetime('now') WHERE id = ?1",
                rusqlite::params![item.id],
            );
        }

        Ok(results)
    }

    /// 删除记忆（按 ID）
    pub fn forget_by_id(conn: &rusqlite::Connection, memory_id: &str) -> rusqlite::Result<bool> {
        let affected = conn.execute(
            "DELETE FROM memory_items WHERE id = ?1",
            rusqlite::params![memory_id],
        )?;
        Ok(affected > 0)
    }

    /// 删除记忆（按关键词匹配删除）
    pub fn forget_by_query(conn: &rusqlite::Connection, query: &str) -> rusqlite::Result<usize> {
        let keywords = extract_keywords(query);
        if keywords.is_empty() {
            return Ok(0);
        }
        // 简单实现：content 包含查询关键词的记忆
        let mut deleted = 0;
        for kw in &keywords {
            let pattern = format!("%{}%", kw);
            deleted += conn.execute(
                "DELETE FROM memory_items WHERE content LIKE ?1",
                rusqlite::params![pattern],
            )?;
        }
        Ok(deleted)
    }

    /// 按类型列出记忆
    pub fn list_by_type(
        conn: &rusqlite::Connection,
        memory_type: Option<MemoryType>,
        limit: usize,
    ) -> rusqlite::Result<Vec<MemoryItem>> {
        let sql = if memory_type.is_some() {
            "SELECT id, memory_type, content, keywords, importance, access_count, created_at, last_accessed_at, expires_at, source
             FROM memory_items WHERE memory_type = ?1 ORDER BY importance DESC, created_at DESC LIMIT ?2"
        } else {
            "SELECT id, memory_type, content, keywords, importance, access_count, created_at, last_accessed_at, expires_at, source
             FROM memory_items ORDER BY importance DESC, created_at DESC LIMIT ?1"
        };

        let mut stmt = conn.prepare(sql)?;
        let items = if let Some(t) = memory_type {
            stmt.query_map(rusqlite::params![t.as_str(), limit as i64], row_to_memory_item)?
        } else {
            stmt.query_map(rusqlite::params![limit as i64], row_to_memory_item)?
        };

        Ok(items.filter_map(|r| r.ok()).collect())
    }

    /// 清理过期记忆
    pub fn cleanup_expired(conn: &rusqlite::Connection) -> rusqlite::Result<usize> {
        let deleted = conn.execute(
            "DELETE FROM memory_items WHERE expires_at IS NOT NULL AND expires_at < datetime('now')",
            [],
        )?;
        Ok(deleted)
    }

    /// 获取最相关的 N 条记忆（用于注入 System Prompt）
    ///
    /// 根据当前对话上下文（用户输入 + 最近消息）检索最相关的记忆。
    pub fn get_relevant_memories(
        conn: &rusqlite::Connection,
        context: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<MemoryItem>> {
        // 清理过期记忆
        let _ = Self::cleanup_expired(conn);
        // 检索相关记忆
        Self::recall(conn, context, None, limit)
    }

    /// 获取所有记忆数量
    pub fn count(conn: &rusqlite::Connection) -> rusqlite::Result<usize> {
        conn.query_row("SELECT COUNT(*) FROM memory_items", [], |row| {
            row.get::<_, i64>(0).map(|c| c as usize)
        })
    }
}

// ============================================================
// 辅助函数
// ============================================================

/// 从数据库行构建 MemoryItem
fn row_to_memory_item(row: &rusqlite::Row) -> rusqlite::Result<MemoryItem> {
    let type_str: String = row.get(1)?;
    let keywords_str: String = row.get(3)?;
    let keywords: Vec<String> = serde_json::from_str(&keywords_str).unwrap_or_default();
    let expires_at: Option<String> = row.get(8)?;

    Ok(MemoryItem {
        id: row.get(0)?,
        memory_type: MemoryType::from_str(&type_str),
        content: row.get(2)?,
        keywords,
        importance: row.get::<_, i64>(4)? as u8,
        access_count: row.get::<_, i64>(5)? as u32,
        created_at: row.get(6)?,
        last_accessed_at: row.get(7)?,
        expires_at,
        source: row.get(9)?,
    })
}

/// 计算记忆与查询关键词的相关性分数
///
/// score = importance * (0.95 ^ days_since_creation) * (1 + log(access_count + 1)) * keyword_match_factor
fn compute_relevance_score(item: &MemoryItem, query_keywords: &[String]) -> f64 {
    let importance = item.importance as f64;

    // 时间衰减：创建天数
    let days = days_since(&item.created_at);
    let time_decay = 0.95_f64.powi(days.min(365) as i32);

    // 访问次数加成
    let access_factor = 1.0 + (item.access_count as f64 + 1.0).ln();

    // 关键词匹配因子
    let keyword_factor = if query_keywords.is_empty() {
        1.0
    } else {
        let item_kw_set: HashSet<&str> = item.keywords.iter().map(|s| s.as_str()).collect();
        let matches = query_keywords
            .iter()
            .filter(|q| item_kw_set.contains(q.as_str()))
            .count();
        // content 包含查询词也加分
        let content_matches = query_keywords
            .iter()
            .filter(|q| item.content.to_lowercase().contains(q.as_str()))
            .count();
        let total_matches = matches + content_matches;
        if total_matches == 0 {
            0.1 // 无匹配时给低分但不完全排除
        } else {
            1.0 + total_matches as f64 * 0.5
        }
    };

    importance * time_decay * access_factor * keyword_factor
}

/// 计算从日期字符串到现在的天数
fn days_since(date_str: &str) -> i64 {
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(date_str, "%Y-%m-%d %H:%M:%S") {
        let now = chrono::Utc::now().naive_utc();
        (now - dt).num_days()
    } else {
        0
    }
}
