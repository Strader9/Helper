//! 记忆固化器：把短期 episodic 记忆合并为长期 growth 记忆
//!
//! 借鉴早期原型的 Consolidator 设计（读逻辑、用 Rust 重写）：
//! 周期性把「未固化的原始对话/任务记忆」按语义聚类，每簇达到阈值后合并成
//! 一条更凝练的长期记忆，并回链原记忆。与早期原型的差异：原型用 LLM 生成
//! 第一人称叙事，这里用**确定性模板合并**——不依赖模型，可离线、可单测、
//! 结果可复现。LLM 增强留待 sidecar 模型网关接入后再叠加。

use std::collections::HashSet;

use crate::memory::{MemoryEngine, MemoryItem, MemoryTier, MemoryType};

/// 一次固化操作的统计结果
#[derive(Debug, Clone)]
pub struct ConsolidateSummary {
    /// 生成了几条合并记忆
    pub merged_into: usize,
    /// 消耗了几条原 episodic 记忆
    pub consumed: usize,
}

/// 拉取并合并未固化的 episodic 记忆。
///
/// - `min_cluster`：每簇至少多少条才触发合并（低于此值保持原样，等下次累积）
/// - `similarity_threshold`：关键词 Jaccard 相似度阈值（0.0~1.0），高于它视为同类
///
/// 返回 `None` 表示没有可固化的记忆（数量不足以成簇）。
pub fn consolidate(
    conn: &rusqlite::Connection,
    min_cluster: usize,
    similarity_threshold: f64,
) -> rusqlite::Result<ConsolidateSummary> {
    let episodic = load_unconsolidated_episodic(conn)?;
    if episodic.is_empty() {
        return Ok(ConsolidateSummary {
            merged_into: 0,
            consumed: 0,
        });
    }

    // 贪心聚类：每条记忆归入相似度最高且超阈值的簇，否则新建簇
    let clusters = cluster_by_similarity(&episodic, similarity_threshold);

    let mut merged_into = 0usize;
    let mut consumed = 0usize;

    for cluster in clusters {
        if cluster.len() < min_cluster {
            continue;
        }

        // 确定性合并成一条 growth 记忆
        let merged = build_merged_item(&cluster);

        // 插入合并记忆（直接走 MemoryEngine 的插入路径，绕过再次聚类）
        let merged_id = merged.id.clone();
        MemoryEngine::insert_item(conn, &merged)?;

        // 回链原记忆
        for item in &cluster {
            conn.execute(
                "UPDATE memory_items SET consolidated_into = ?1 WHERE id = ?2",
                rusqlite::params![merged_id, item.id],
            )?;
        }

        merged_into += 1;
        consumed += cluster.len();
    }

    Ok(ConsolidateSummary {
        merged_into,
        consumed,
    })
}

/// 从数据库拉取未固化的 episodic 记忆。
fn load_unconsolidated_episodic(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<MemoryItem>> {
    let mut stmt = conn.prepare(
        "SELECT id, memory_type, content, keywords, importance, access_count, created_at,
                last_accessed_at, expires_at, source, tier, consolidated_into
         FROM memory_items
         WHERE tier = 'episodic' AND consolidated_into IS NULL
         ORDER BY created_at ASC",
    )?;

    let items = stmt
        .query_map([], crate::memory::row_to_memory_item)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(items)
}

/// 按关键词 Jaccard 相似度做贪心聚类。
fn cluster_by_similarity(items: &[MemoryItem], threshold: f64) -> Vec<Vec<MemoryItem>> {
    let mut clusters: Vec<Vec<MemoryItem>> = Vec::new();

    for item in items {
        let item_kws: HashSet<&str> = item.keywords.iter().map(|s| s.as_str()).collect();

        let mut best_idx: Option<usize> = None;
        let mut best_sim = 0.0f64;

        for (idx, cluster) in clusters.iter().enumerate() {
            // 与簇内所有成员的**最大**相似度作为该簇的相似度
            let cluster_sim = cluster
                .iter()
                .map(|m| {
                    let kws: HashSet<&str> = m.keywords.iter().map(|s| s.as_str()).collect();
                    jaccard(&item_kws, &kws)
                })
                .fold(0.0f64, f64::max);

            if cluster_sim >= threshold && cluster_sim > best_sim {
                best_sim = cluster_sim;
                best_idx = Some(idx);
            }
        }

        match best_idx {
            Some(idx) => clusters[idx].push(item.clone()),
            None => clusters.push(vec![item.clone()]),
        }
    }

    clusters
}

/// 两条关键词集合的 Jaccard 相似度（交集 / 并集）。
fn jaccard(a: &HashSet<&str>, b: &HashSet<&str>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let union: HashSet<&str> = a.union(b).copied().collect();
    if union.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    intersection as f64 / union.len() as f64
}

/// 把一簇记忆确定性合并成一条 growth 记忆。
///
/// 合并策略（可复现、无 LLM）：
/// 1. 以簇内 `importance` 最高的一条作为「代表」；
/// 2. 其余各条内容去重后按原文拼接为补充线索；
/// 3. 关键词取簇内所有关键词的并集（截断到 40 个）；
/// 4. 新记忆类型为 `Knowledge`、层级 `Growth`。
fn build_merged_item(cluster: &[MemoryItem]) -> MemoryItem {
    // 代表条目：importance 最高（并列取时间早的）
    let representative = cluster
        .iter()
        .max_by(|a, b| {
            a.importance
                .cmp(&b.importance)
                .then(b.created_at.cmp(&a.created_at))
        })
        .expect("簇非空");

    // 去重内容（保留出现顺序）
    let mut seen = HashSet::new();
    let mut parts: Vec<String> = Vec::new();
    for item in cluster {
        if seen.insert(item.content.clone()) {
            parts.push(item.content.clone());
        }
    }

    let content = format!("用户近期 {} 次提及：{}", cluster.len(), parts.join("；"));
    // 截断到 500 字，避免超长
    let content = truncate_chars(&content, 500);

    // 关键词并集
    let mut kws: Vec<String> = Vec::new();
    let mut kw_seen = HashSet::new();
    for item in cluster {
        for kw in &item.keywords {
            if kw_seen.insert(kw.clone()) {
                kws.push(kw.clone());
                if kws.len() >= 40 {
                    break;
                }
            }
        }
    }

    let mut merged = MemoryItem::new(
        MemoryType::Knowledge,
        &content,
        representative.importance,
        "consolidator",
    );
    merged.tier = MemoryTier::Growth;
    merged.keywords = kws;
    merged
}

/// 按字符数截断（安全处理多字节 UTF-8）。
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(content: &str, keywords: &[&str], importance: u8) -> MemoryItem {
        MemoryItem {
            id: uuid::Uuid::new_v4().to_string(),
            memory_type: MemoryType::TaskHistory,
            content: content.to_string(),
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            importance,
            access_count: 0,
            created_at: "2026-10-19 10:00:00".to_string(),
            last_accessed_at: "2026-10-19 10:00:00".to_string(),
            expires_at: None,
            source: "test".to_string(),
            tier: MemoryTier::Episodic,
            consolidated_into: None,
        }
    }

    #[test]
    fn jaccard_measures_keyword_overlap() {
        let a: HashSet<&str> = ["rust", "内存", "分层"].iter().copied().collect();
        let b: HashSet<&str> = ["rust", "内存"].iter().copied().collect();
        // 交集 2 / 并集 3 ≈ 0.667
        let sim = jaccard(&a, &b);
        assert!((sim - 0.667).abs() < 0.001, "实际 {}", sim);
    }

    #[test]
    fn clusters_similar_memories_together() {
        let items = vec![
            mem("用户在做 Rust 项目", &["rust", "项目"], 50),
            mem("Rust 的 borrow checker 难", &["rust", "borrow"], 40),
            mem("用户喜欢喝咖啡", &["咖啡", "偏好"], 60),
        ];
        let clusters = cluster_by_similarity(&items, 0.3);
        // 前两条共享 "rust"，应同簇；第三条独立
        assert_eq!(clusters.len(), 2, "应聚成 2 簇，实际 {}", clusters.len());
        let rust_cluster = clusters
            .iter()
            .find(|c| c.iter().any(|m| m.keywords.contains(&"rust".to_string())))
            .expect("应有 rust 簇");
        assert_eq!(rust_cluster.len(), 2, "rust 簇应含 2 条");
    }

    #[test]
    fn merged_item_picks_highest_importance_and_growth_tier() {
        let cluster = vec![
            mem("用户做 Rust", &["rust"], 30),
            mem("用户做 Rust 项目", &["rust", "项目"], 80),
        ];
        let merged = build_merged_item(&cluster);
        assert_eq!(merged.tier, MemoryTier::Growth);
        assert_eq!(merged.memory_type, MemoryType::Knowledge);
        assert_eq!(merged.importance, 80, "importance 应取代表条目");
        assert!(merged.content.contains("2 次"), "内容应标注合并条数");
    }

    /// 端到端：建内存库 → 插 5 条 episodic → 固化 → 校验合并与回链。
    #[test]
    fn consolidate_merges_a_cluster_and_links_originals() {
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

        // 3 条 rust 相关（应成簇合并）+ 2 条咖啡相关（不足 3，暂不合并）
        let rust_items = vec![
            mem("用户在做 Rust 项目", &["rust", "项目"], 50),
            mem("Rust 的 borrow checker 很难", &["rust", "borrow"], 40),
            mem("用户想用 Rust 写工具", &["rust", "工具"], 60),
        ];
        let coffee_items = vec![
            mem("用户喜欢喝咖啡", &["咖啡"], 30),
            mem("用户每天早上一杯咖啡", &["咖啡", "早上"], 30),
        ];
        for item in rust_items.iter().chain(coffee_items.iter()) {
            MemoryEngine::insert_item(&conn, item).expect("插入");
        }

        let summary = consolidate(&conn, 3, 0.3).expect("固化");

        assert_eq!(summary.merged_into, 1, "应只合并出一簇");
        assert_eq!(summary.consumed, 3, "应消耗 3 条 rust 记忆");

        // 3 条 rust 记忆应被回链到同一条合并记忆
        let linked: Vec<String> = rust_items
            .iter()
            .map(|m| {
                conn.query_row(
                    "SELECT consolidated_into FROM memory_items WHERE id = ?1",
                    rusqlite::params![m.id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .expect("查询回链")
                .expect("应已回链")
            })
            .collect();
        assert_eq!(linked[0], linked[1]);
        assert_eq!(linked[1], linked[2], "3 条应回链到同一条合并记忆");

        // 咖啡记忆未成簇，应保持未固化
        let coffee_unlinked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memory_items WHERE consolidated_into IS NULL AND content LIKE '%咖啡%'",
                [],
                |row| row.get(0),
            )
            .expect("查询咖啡");
        assert_eq!(coffee_unlinked, 2, "咖啡记忆应保持未固化");
    }
}
