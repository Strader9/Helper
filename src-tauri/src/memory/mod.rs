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

pub mod consolidator;
pub mod decay;
pub mod firewall;

pub use consolidator::{consolidate, ConsolidateSummary};
pub use decay::tier_decay_rate;
pub use firewall::{evaluate_firewall, FirewallDecision};

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

/// 记忆层级（tier）：决定衰减速率与固化优先级。
///
/// 借鉴早期原型的 MemoryTier 分层，用 Rust 重写：
/// - `Episodic`：原始对话/任务记忆，快速衰减，是固化器的原料
/// - `Internalized`：内化偏好（用户偏好、应用习惯），较稳定
/// - `Growth`：固化器产出的凝练叙事记忆
/// - `CoreIdentity`：核心身份，永不衰减
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryTier {
    Episodic,
    Internalized,
    Growth,
    CoreIdentity,
}

impl MemoryTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryTier::Episodic => "episodic",
            MemoryTier::Internalized => "internalized",
            MemoryTier::Growth => "growth",
            MemoryTier::CoreIdentity => "core_identity",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "episodic" => MemoryTier::Episodic,
            "internalized" => MemoryTier::Internalized,
            "growth" => MemoryTier::Growth,
            "core_identity" => MemoryTier::CoreIdentity,
            _ => MemoryTier::Episodic,
        }
    }
}

/// 由记忆类型推断默认层级。
///
/// 用户偏好与应用习惯是「内化」的稳定事实；任务历史与对话摘要是「短期」的原始经验。
fn tier_for_type(memory_type: MemoryType) -> MemoryTier {
    match memory_type {
        MemoryType::UserPreference | MemoryType::AppUsage => MemoryTier::Internalized,
        MemoryType::Knowledge => MemoryTier::Growth,
        MemoryType::TaskHistory | MemoryType::ConversationSummary => MemoryTier::Episodic,
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
    /// 记忆层级（决定衰减速率）
    pub tier: MemoryTier,
    /// 被固化器合并后，指向合并产物的 ID；`None` 表示尚未被固化。
    pub consolidated_into: Option<String>,
}

/// 用户适应画像中的一条可追溯证据。画像只整理已保存的记忆，不凭空推断人格。
#[derive(Debug, Clone, Serialize)]
pub struct ProfileEvidence {
    pub id: String,
    pub content: String,
    pub importance: u8,
    pub confidence: u8,
    pub source: String,
    pub updated_at: String,
    pub access_count: u32,
}

/// 面向前端与提示词诊断的用户适应画像。
#[derive(Debug, Clone, Serialize)]
pub struct AdaptationProfile {
    pub generated_at: String,
    pub total_memories: usize,
    pub active_memories: usize,
    pub durable_memories: usize,
    pub episodic_memories: usize,
    pub consolidated_memories: usize,
    /// 画像覆盖度，不代表对用户人格判断的“准确率”。
    pub coverage_score: u8,
    pub preferences: Vec<ProfileEvidence>,
    pub habits: Vec<ProfileEvidence>,
    pub facts: Vec<ProfileEvidence>,
    pub recent_experiences: Vec<ProfileEvidence>,
}

impl MemoryItem {
    /// 创建新记忆（自动生成 ID、时间戳、关键词）
    pub fn new(memory_type: MemoryType, content: &str, importance: u8, source: &str) -> Self {
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
            tier: tier_for_type(memory_type),
            consolidated_into: None,
        }
    }
}

// ============================================================
// 关键词提取（简单实现）
// ============================================================

/// 中文停用词集合
fn stop_words() -> HashSet<&'static str> {
    let words = [
        "的",
        "了",
        "是",
        "在",
        "我",
        "有",
        "和",
        "就",
        "不",
        "人",
        "都",
        "一",
        "一个",
        "上",
        "也",
        "很",
        "到",
        "说",
        "要",
        "去",
        "你",
        "会",
        "着",
        "没有",
        "看",
        "好",
        "自己",
        "这",
        "那",
        "他",
        "她",
        "它",
        "们",
        "这个",
        "那个",
        "什么",
        "怎么",
        "为什么",
        "可以",
        "能",
        "应该",
        "需要",
        "想",
        "知道",
        "觉得",
        "认为",
        "the",
        "a",
        "an",
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "being",
        "have",
        "has",
        "had",
        "do",
        "does",
        "did",
        "will",
        "would",
        "could",
        "should",
        "may",
        "might",
        "must",
        "shall",
        "can",
        "need",
        "dare",
        "to",
        "of",
        "in",
        "for",
        "on",
        "with",
        "at",
        "by",
        "from",
        "as",
        "into",
        "through",
        "during",
        "before",
        "after",
        "above",
        "below",
        "between",
        "out",
        "off",
        "over",
        "under",
        "again",
        "further",
        "then",
        "once",
        "here",
        "there",
        "when",
        "where",
        "why",
        "how",
        "all",
        "both",
        "each",
        "few",
        "more",
        "most",
        "other",
        "some",
        "such",
        "no",
        "nor",
        "not",
        "only",
        "own",
        "same",
        "so",
        "than",
        "too",
        "very",
        "just",
        "because",
        "but",
        "and",
        "or",
        "if",
        "while",
        "about",
        "up",
        "it",
        "its",
        "i",
        "me",
        "my",
        "we",
        "our",
        "you",
        "your",
        "he",
        "him",
        "his",
        "she",
        "her",
        "they",
        "them",
        "their",
        "what",
        "which",
        "who",
        "whom",
        "this",
        "that",
        "these",
        "those",
        "am",
        "also",
        "get",
        "got",
        "make",
        "made",
        "take",
        "took",
        "go",
        "went",
        "come",
        "came",
        "see",
        "saw",
        "know",
        "knew",
        "think",
        "thought",
        "say",
        "said",
        "tell",
        "told",
        "use",
        "used",
        "find",
        "found",
        "give",
        "gave",
        "work",
        "worked",
        "like",
        "liked",
        "want",
        "wanted",
        "try",
        "tried",
        "ask",
        "asked",
        "need",
        "needed",
        "feel",
        "felt",
        "become",
        "became",
        "leave",
        "left",
        "put",
        "set",
        "let",
        "begin",
        "began",
        "seem",
        "seemed",
        "help",
        "helped",
        "show",
        "showed",
        "call",
        "called",
        "keep",
        "kept",
        "let",
        "mean",
        "meant",
        "hold",
        "held",
        "move",
        "moved",
        "live",
        "lived",
        "believe",
        "bring",
        "brought",
        "happen",
        "write",
        "wrote",
        "provide",
        "sit",
        "stand",
        "lose",
        "pay",
        "meet",
        "include",
        "continue",
        "learn",
        "change",
        "lead",
        "understand",
        "watch",
        "follow",
        "stop",
        "create",
        "speak",
        "read",
        "allow",
        "add",
        "spend",
        "grow",
        "open",
        "walk",
        "win",
        "offer",
        "remember",
        "love",
        "consider",
        "appear",
        "buy",
        "wait",
        "serve",
        "die",
        "send",
        "expect",
        "build",
        "stay",
        "fall",
        "cut",
        "reach",
        "kill",
        "remain",
        "suggest",
        "raise",
        "pass",
        "sell",
        "require",
        "report",
        "decide",
        "pull",
        "develop",
        "carry",
        "break",
        "receive",
        "agree",
        "hit",
        "produce",
        "eat",
        "cover",
        "catch",
        "draw",
        "choose",
        "could",
    ];
    words.iter().copied().collect()
}

/// 从文本中提取关键词（简单实现：标点分割 + 停用词过滤 + 中文 n-gram 切分）
///
/// 关于中文切分的说明：原先只按标点切分，导致「把刚才改的那几个文件提交一下」
/// 整段成为一个 token，与记忆里存的关键词（"提交""文件"）一个都对不上 ——
/// 中文没有空格，标点切分对口语长句几乎无效。
///
/// 这里对**不含 ASCII 的连续中文串**补一层 2–3 字滑动窗口切分（无需引入分词库）。
/// 例如「文件提交一下」→ 文件/提交/交一/一下/文件提/件提交/提交一/交一下。
/// 配合 `compute_relevance_score` 里的子串包含匹配，能显著提高召回。
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

    // 判断一个字符是否属于"中文"（CJK 统一表意文字基本区）
    fn is_cjk(c: char) -> bool {
        matches!(c as u32, 0x4E00..=0x9FFF)
    }

    for token in tokens {
        let lower = token.to_lowercase();
        let chars: Vec<char> = lower.chars().collect();

        // 纯中文或中英混合的长串：补 n-gram
        if chars.len() >= 5 && chars.iter().any(|c| is_cjk(*c)) {
            for n in 2..=3usize {
                if chars.len() < n {
                    continue;
                }
                for window in chars.windows(n) {
                    let gram: String = window.iter().collect();
                    if gram.chars().all(is_cjk) && !stop.contains(gram.as_str()) {
                        if seen.insert(gram.clone()) {
                            keywords.push(gram);
                        }
                    }
                }
            }
        }

        // 保留原 token（英文词、短中文词走这里）
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

    // 最多保留 40 个关键词（n-gram 会显著增加数量，上限相应放宽）
    keywords.truncate(40);
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
        let keywords_json =
            serde_json::to_string(&item.keywords).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
            "INSERT INTO memory_items (id, memory_type, content, keywords, importance, access_count, created_at, last_accessed_at, expires_at, source, tier, consolidated_into)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL)",
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
                item.tier.as_str(),
            ],
        )?;

        Ok(item.id)
    }

    /// 插入一条已构造好的记忆（供固化器等内部流程使用）。
    ///
    /// 与 `add_memory` 的区别：不走去重、不过防火墙，原样写入完整条目。
    /// 调用方须保证内容已经过防火墙或来自可信的内部生成。
    pub fn insert_item(conn: &rusqlite::Connection, item: &MemoryItem) -> rusqlite::Result<()> {
        let keywords_json =
            serde_json::to_string(&item.keywords).unwrap_or_else(|_| "[]".to_string());
        conn.execute(
            "INSERT INTO memory_items (id, memory_type, content, keywords, importance, access_count, created_at, last_accessed_at, expires_at, source, tier, consolidated_into)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                item.id,
                item.memory_type.as_str(),
                item.content,
                keywords_json,
                item.importance as i64,
                item.access_count as i64,
                item.created_at,
                item.last_accessed_at,
                item.expires_at,
                item.source,
                item.tier.as_str(),
                item.consolidated_into,
            ],
        )?;
        Ok(())
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
            "SELECT id, memory_type, content, keywords, importance, access_count, created_at, last_accessed_at, expires_at, source, tier, consolidated_into
             FROM memory_items WHERE memory_type = ?1 AND (expires_at IS NULL OR expires_at > datetime('now'))
             ORDER BY importance DESC, created_at DESC LIMIT 200"
        } else {
            "SELECT id, memory_type, content, keywords, importance, access_count, created_at, last_accessed_at, expires_at, source, tier, consolidated_into
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

        // 非空查询只返回真正命中的条目。保留 0 分条目会让高重要性但无关的记忆
        // 混进每次对话，造成“助手总拿旧偏好答非所问”。
        let results: Vec<MemoryItem> = scored
            .into_iter()
            .filter(|(score, _)| query_keywords.is_empty() || *score > 0.0)
            .take(limit)
            .map(|(_, item)| item)
            .collect();

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
            "SELECT id, memory_type, content, keywords, importance, access_count, created_at, last_accessed_at, expires_at, source, tier, consolidated_into
             FROM memory_items WHERE memory_type = ?1 ORDER BY importance DESC, created_at DESC LIMIT ?2"
        } else {
            "SELECT id, memory_type, content, keywords, importance, access_count, created_at, last_accessed_at, expires_at, source, tier, consolidated_into
             FROM memory_items ORDER BY importance DESC, created_at DESC LIMIT ?1"
        };

        let mut stmt = conn.prepare(sql)?;
        let items = if let Some(t) = memory_type {
            stmt.query_map(
                rusqlite::params![t.as_str(), limit as i64],
                row_to_memory_item,
            )?
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

    /// 生成可解释的用户适应画像。所有画像条目都保留原记忆 ID、来源和更新时间。
    pub fn build_adaptation_profile(
        conn: &rusqlite::Connection,
        per_section_limit: usize,
    ) -> rusqlite::Result<AdaptationProfile> {
        let all = Self::list_by_type(conn, None, 500)?;
        let total_memories = all.len();
        let active: Vec<&MemoryItem> = all
            .iter()
            .filter(|item| item.consolidated_into.is_none())
            .collect();
        let durable_memories = active
            .iter()
            .filter(|item| {
                matches!(
                    item.tier,
                    MemoryTier::Internalized | MemoryTier::CoreIdentity
                )
            })
            .count();
        let episodic_memories = active
            .iter()
            .filter(|item| item.tier == MemoryTier::Episodic)
            .count();
        let consolidated_memories = all
            .iter()
            .filter(|item| item.consolidated_into.is_some())
            .count();

        let evidence = |item: &&MemoryItem| ProfileEvidence {
            id: item.id.clone(),
            content: item.content.clone(),
            importance: item.importance,
            // 手动确认的内容置信度最高；自动来源适当降权，并用访问次数小幅增益。
            confidence: ((item.importance as u16
                + if matches!(item.source.as_str(), "manual" | "user_explicit") {
                    15
                } else {
                    0
                }
                + item.access_count.min(5) as u16 * 2)
                .min(100)) as u8,
            source: item.source.clone(),
            updated_at: item.last_accessed_at.clone(),
            access_count: item.access_count,
        };

        let collect_type = |kind: MemoryType| -> Vec<ProfileEvidence> {
            active
                .iter()
                .filter(|item| item.memory_type == kind)
                .take(per_section_limit)
                .map(evidence)
                .collect()
        };
        let recent_experiences = active
            .iter()
            .filter(|item| {
                matches!(
                    item.memory_type,
                    MemoryType::TaskHistory | MemoryType::ConversationSummary
                )
            })
            .take(per_section_limit)
            .map(evidence)
            .collect();

        let explicit_count = active
            .iter()
            .filter(|item| {
                matches!(item.source.as_str(), "manual" | "user_explicit")
                    && matches!(
                        item.memory_type,
                        MemoryType::UserPreference | MemoryType::AppUsage
                    )
            })
            .count();
        let coverage_score =
            (durable_memories * 10 + explicit_count * 8 + active.len().min(20) * 2).min(100) as u8;

        Ok(AdaptationProfile {
            generated_at: chrono::Utc::now().to_rfc3339(),
            total_memories,
            active_memories: active.len(),
            durable_memories,
            episodic_memories,
            consolidated_memories,
            coverage_score,
            preferences: collect_type(MemoryType::UserPreference),
            habits: collect_type(MemoryType::AppUsage),
            facts: collect_type(MemoryType::Knowledge),
            recent_experiences,
        })
    }

    /// 构造“稳定画像 + 当前相关经历”的双层上下文。
    /// 稳定偏好无需关键词碰巧命中；历史事实仍按当前问题检索，避免污染每轮提示词。
    pub fn build_adaptive_context(
        conn: &rusqlite::Connection,
        query: &str,
        stable_limit: usize,
        relevant_limit: usize,
    ) -> rusqlite::Result<String> {
        let all = Self::list_by_type(conn, None, 300)?;
        let mut stable: Vec<&MemoryItem> = all
            .iter()
            .filter(|item| {
                item.consolidated_into.is_none()
                    && matches!(
                        item.memory_type,
                        MemoryType::UserPreference | MemoryType::AppUsage
                    )
                    && item.importance >= 40
            })
            .collect();
        stable.sort_by(|a, b| {
            b.importance
                .cmp(&a.importance)
                .then(b.last_accessed_at.cmp(&a.last_accessed_at))
        });
        stable.truncate(stable_limit);

        let stable_ids: HashSet<&str> = stable.iter().map(|item| item.id.as_str()).collect();
        let relevant = Self::recall(conn, query, None, relevant_limit)?;
        let mut sections = Vec::new();
        if !stable.is_empty() {
            let lines = stable
                .iter()
                .map(|item| format!("- {}", item.content))
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!("### 稳定偏好与习惯（默认遵守）\n{}", lines));
        }
        let contextual = relevant
            .iter()
            .filter(|item| !stable_ids.contains(item.id.as_str()))
            .map(|item| format!("- {}", item.content))
            .collect::<Vec<_>>();
        if !contextual.is_empty() {
            sections.push(format!(
                "### 与当前请求相关的经历或事实\n{}",
                contextual.join("\n")
            ));
        }
        Ok(sections.join("\n\n"))
    }

    /// 修改一条记忆的内容或重要性。
    ///
    /// 返回 false 表示这条记忆不存在——不是"改成功了但内容一样"。
    /// 两个参数都没给时不发 SQL，避免一次无意义的全表扫描。
    pub fn update_memory(
        conn: &rusqlite::Connection,
        memory_id: &str,
        content: Option<&str>,
        importance: Option<u8>,
    ) -> rusqlite::Result<bool> {
        if content.is_none() && importance.is_none() {
            return Ok(false);
        }

        let changed = match (content, importance) {
            (Some(text), Some(score)) => {
                let keywords = serde_json::to_string(&extract_keywords(text))
                    .unwrap_or_else(|_| "[]".to_string());
                conn.execute(
                    "UPDATE memory_items
                     SET content = ?1, keywords = ?2, importance = ?3, last_accessed_at = datetime('now')
                     WHERE id = ?4",
                    rusqlite::params![text, keywords, score.min(100) as i64, memory_id],
                )?
            }
            (Some(text), None) => {
                let keywords = serde_json::to_string(&extract_keywords(text))
                    .unwrap_or_else(|_| "[]".to_string());
                conn.execute(
                    "UPDATE memory_items
                     SET content = ?1, keywords = ?2, last_accessed_at = datetime('now')
                     WHERE id = ?3",
                    rusqlite::params![text, keywords, memory_id],
                )?
            }
            (None, Some(score)) => conn.execute(
                "UPDATE memory_items SET importance = ?1, last_accessed_at = datetime('now') WHERE id = ?2",
                rusqlite::params![score.min(100) as i64, memory_id],
            )?,
            (None, None) => 0,
        };

        Ok(changed > 0)
    }

    /// 前端完整编辑：保留原 ID、来源、访问次数和时间线，只更新用户实际修改的字段。
    pub fn update_memory_details(
        conn: &rusqlite::Connection,
        memory_id: &str,
        memory_type: MemoryType,
        content: &str,
        importance: u8,
    ) -> rusqlite::Result<bool> {
        let keywords =
            serde_json::to_string(&extract_keywords(content)).unwrap_or_else(|_| "[]".to_string());
        let affected = conn.execute(
            "UPDATE memory_items
             SET memory_type = ?1, content = ?2, keywords = ?3, importance = ?4,
                 tier = ?5, last_accessed_at = datetime('now')
             WHERE id = ?6",
            rusqlite::params![
                memory_type.as_str(),
                content,
                keywords,
                importance.min(100) as i64,
                tier_for_type(memory_type).as_str(),
                memory_id,
            ],
        )?;
        Ok(affected > 0)
    }

    /// 各类型的记忆条数
    pub fn counts_by_type(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<(String, usize)>> {
        let mut stmt = conn.prepare(
            "SELECT memory_type, COUNT(*) FROM memory_items GROUP BY memory_type ORDER BY COUNT(*) DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
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
pub fn row_to_memory_item(row: &rusqlite::Row) -> rusqlite::Result<MemoryItem> {
    let type_str: String = row.get(1)?;
    let keywords_str: String = row.get(3)?;
    let keywords: Vec<String> = serde_json::from_str(&keywords_str).unwrap_or_default();
    let expires_at: Option<String> = row.get(8)?;
    let tier_str: String = row.get(10)?;
    let consolidated_into: Option<String> = row.get(11)?;

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
        tier: MemoryTier::from_str(&tier_str),
        consolidated_into,
    })
}

/// 计算记忆与查询关键词的相关性分数
///
/// score = importance * (0.95 ^ days_since_creation) * (1 + log(access_count + 1)) * keyword_match_factor
///
/// 关于 `keyword_match_factor` 的两处修正（原先都会导致检索不准）：
///
/// 1. **匹配从「精确相等」放宽为「双向子串包含」**。原先要求查询词与记忆关键词完全相同，
///    但中文没有分词：用户说"帮我把改的提交一下"，切出来的是整段"把改的提交一下"，
///    而记忆里存的关键词是"提交" —— 一个都对不上。现在只要任一方包含另一方就算命中。
///
/// 2. **无匹配时不再给 0.1 保底分**。原先无匹配返回 0.1，本意是"不排除"，但乘以高
///    importance（如 90）之后仍有 9 分，足以在**任何无关问法**里都挤进前五 ——
///    实测"下载 20GB 模型"这类与纪律条目毫无关系的问法，都被 git 纪律条目占满了名额。
///    改为 0（完全排除），让真正匹配的记忆有位置。
fn compute_relevance_score(item: &MemoryItem, query_keywords: &[String]) -> f64 {
    let importance = item.importance as f64;

    // 时间衰减：按层级取衰减率（episodic 快、core_identity 不衰减）
    let days = days_since(&item.created_at);
    let time_decay = tier_decay_rate(item.tier).powi(days.min(365) as i32);

    // 访问次数加成
    let access_factor = 1.0 + (item.access_count as f64 + 1.0).ln();

    // 关键词匹配因子
    let keyword_factor = if query_keywords.is_empty() {
        1.0
    } else {
        let content_lower = item.content.to_lowercase();

        // 双向子串包含：查询词落在关键词里，或关键词落在查询词里，都算命中。
        let mut matches = 0usize;
        for q in query_keywords {
            if q.chars().count() < 2 {
                continue;
            }
            let in_keywords = item.keywords.iter().any(|k| {
                let k_lower = k.to_lowercase();
                !k_lower.is_empty()
                    && (k_lower.contains(q.as_str()) || q.contains(k_lower.as_str()))
            });
            let in_content = content_lower.contains(q.as_str());
            if in_keywords || in_content {
                matches += 1;
            }
        }

        if matches == 0 {
            0.0 // 无匹配：完全排除，不再靠 importance 保底占位
        } else {
            1.0 + matches as f64 * 0.5
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

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("内存库");
        conn.execute_batch(
            "CREATE TABLE memory_items (
                id TEXT PRIMARY KEY,
                memory_type TEXT NOT NULL,
                content TEXT NOT NULL,
                keywords TEXT,
                importance INTEGER NOT NULL DEFAULT 50,
                access_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                last_accessed_at TEXT NOT NULL DEFAULT (datetime('now')),
                expires_at TEXT,
                source TEXT NOT NULL DEFAULT 'auto',
                tier TEXT NOT NULL DEFAULT 'episodic',
                consolidated_into TEXT
            );",
        )
        .expect("建表");
        conn
    }

    fn item(content: &str, keywords: &[&str], importance: u8) -> MemoryItem {
        MemoryItem {
            id: "test".to_string(),
            memory_type: MemoryType::UserPreference,
            content: content.to_string(),
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            importance,
            access_count: 0,
            created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            last_accessed_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            expires_at: None,
            source: "test".to_string(),
            tier: MemoryTier::Internalized,
            consolidated_into: None,
        }
    }

    #[test]
    fn splits_a_long_chinese_sentence_into_searchable_ngrams() {
        // 为什么要切：中文没有空格，整句标点切分后是一个 token，
        // 与记忆里存的关键词（"提交""文件"）一个都对不上，检索必然落空。
        let kws = extract_keywords("把刚才改的那几个文件提交一下");
        assert!(
            kws.iter().any(|k| k == "文件"),
            "应切出「文件」，实际: {:?}",
            kws
        );
        assert!(
            kws.iter().any(|k| k == "提交"),
            "应切出「提交」，实际: {:?}",
            kws
        );
    }

    #[test]
    fn keeps_short_tokens_and_english_words_intact() {
        let kws = extract_keywords("pip install");
        assert!(
            kws.iter().any(|k| k == "pip"),
            "应保留 pip，实际: {:?}",
            kws
        );
        assert!(
            kws.iter().any(|k| k == "install"),
            "应保留 install，实际: {:?}",
            kws
        );
    }

    #[test]
    fn matches_a_memory_whose_keyword_is_a_substring_of_the_query() {
        // 核心回归：原先要求精确相等，"提交一下"匹配不上关键词"提交"。
        let mem = item("禁止 git add .", &["git", "提交", "暂存"], 90);
        let query = extract_keywords("帮我把改的文件提交一下");
        let score = compute_relevance_score(&mem, &query);
        assert!(score > 0.0, "含「提交」的问法应当命中，实际得分 {}", score);
    }

    #[test]
    fn gives_zero_to_an_unrelated_memory_instead_of_a_floor_score() {
        // 为什么必须是 0：原先无匹配给 0.1 保底，乘上 importance 90 仍有 9 分，
        // 导致高优先级纪律条目在**任何**无关问法里都占满名额。
        let mem = item("禁止 git add .", &["git", "提交", "暂存"], 90);
        let query = extract_keywords("下载一个二十GB的模型到D盘");
        let score = compute_relevance_score(&mem, &query);
        assert_eq!(
            score, 0.0,
            "无关问法不应命中高优先级条目，实际得分 {}",
            score
        );
    }

    #[test]
    fn recall_excludes_zero_score_memories() {
        let conn = memory_db();
        MemoryEngine::add_memory(
            &conn,
            MemoryType::Knowledge,
            "Rust 项目使用 cargo test",
            60,
            "manual",
        )
        .expect("写入相关记忆");
        MemoryEngine::add_memory(&conn, MemoryType::Knowledge, "早餐喜欢喝牛奶", 95, "manual")
            .expect("写入无关记忆");

        let results = MemoryEngine::recall(&conn, "Rust 怎么测试", None, 5).expect("召回");
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("Rust"));
    }

    #[test]
    fn adaptive_context_keeps_stable_preferences_and_retrieves_relevant_facts() {
        let conn = memory_db();
        MemoryEngine::add_memory(
            &conn,
            MemoryType::UserPreference,
            "回答保持简洁，先给结论",
            80,
            "manual",
        )
        .expect("写偏好");
        MemoryEngine::add_memory(
            &conn,
            MemoryType::Knowledge,
            "Rust 项目统一使用 cargo test",
            60,
            "manual",
        )
        .expect("写事实");

        let context = MemoryEngine::build_adaptive_context(&conn, "运行 Rust 测试", 4, 5)
            .expect("构建上下文");
        assert!(context.contains("稳定偏好与习惯"));
        assert!(context.contains("回答保持简洁"));
        assert!(context.contains("与当前请求相关"));
        assert!(context.contains("cargo test"));
    }

    #[test]
    fn adaptation_profile_keeps_evidence_provenance() {
        let conn = memory_db();
        let id = MemoryEngine::add_memory(
            &conn,
            MemoryType::UserPreference,
            "偏好深色界面",
            70,
            "manual",
        )
        .expect("写偏好");

        let profile = MemoryEngine::build_adaptation_profile(&conn, 6).expect("画像");
        assert_eq!(profile.preferences.len(), 1);
        assert_eq!(profile.preferences[0].id, id);
        assert_eq!(profile.preferences[0].source, "manual");
        assert!(profile.preferences[0].confidence >= 70);
        assert!(profile.coverage_score > 0);
    }

    #[test]
    fn editing_memory_preserves_identity_source_and_usage_history() {
        let conn = memory_db();
        let id = MemoryEngine::add_memory(
            &conn,
            MemoryType::Knowledge,
            "回答尽量简洁",
            55,
            "user_explicit",
        )
        .expect("写入记忆");
        conn.execute(
            "UPDATE memory_items SET access_count = 7 WHERE id = ?1",
            rusqlite::params![id],
        )
        .expect("设置访问历史");

        assert!(MemoryEngine::update_memory_details(
            &conn,
            &id,
            MemoryType::UserPreference,
            "回答先给结论，再补充必要细节",
            85,
        )
        .expect("编辑记忆"));

        let edited: (String, String, String, i64) = conn
            .query_row(
                "SELECT id, memory_type, source, access_count FROM memory_items WHERE id = ?1",
                rusqlite::params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("读取编辑后的记忆");
        assert_eq!(edited.0, id);
        assert_eq!(edited.1, "user_preference");
        assert_eq!(edited.2, "user_explicit");
        assert_eq!(edited.3, 7);
    }

    #[test]
    fn a_high_importance_memory_still_loses_to_a_matched_one() {
        // 排序正确性：高 importance 但无关的条目，不能压过相关但低 importance 的条目。
        let unrelated_important = item("禁止 git add .", &["git", "提交"], 90);
        let relevant_modest = item("大文件优先放 D 盘", &["d盘", "磁盘", "下载"], 60);
        let query = extract_keywords("我要下载一个大文件放哪");
        let s_unrelated = compute_relevance_score(&unrelated_important, &query);
        let s_relevant = compute_relevance_score(&relevant_modest, &query);
        assert!(
            s_relevant > s_unrelated,
            "相关条目({})应高于无关高优先级条目({})",
            s_relevant,
            s_unrelated
        );
    }

    #[test]
    fn does_not_split_gibberish_into_single_chars() {
        // 反向断言：n-gram 的下界是 2 字，1 字碎片（噪音极大）不应出现。
        let kws = extract_keywords("文件提交一下");
        assert!(
            kws.iter().all(|k| k.chars().count() >= 2),
            "不应产生单字关键词，实际: {:?}",
            kws
        );
    }
}
