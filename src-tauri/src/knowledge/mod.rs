//! 本地知识库：把目录里的文本文件切块入库，供关键词检索。
//!
//! 与 `memory` 的分工：
//! - 记忆（`memory_items`）是"助手记住的事实"，会被注入 System Prompt；
//! - 知识库（`knowledge_documents` / `knowledge_chunks`）是"用户文档的原文"，
//!   只在用户问到时按需取出。
//!
//! 这个分层是刻意的：把原文塞进 System Prompt 会挤掉对话历史，
//! 而把事实塞进检索库又会让助手"每次都忘"。

use std::fs;
use std::path::Path;

use rusqlite::{params, Connection};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};

/// 单个分块的目标字符数。
///
/// 太小会把一句话切碎、检索命中后拼不回原意；太大则一次带进上下文的内容过多。
/// 800 字大约是一屏中文正文，足够自洽。
const CHUNK_TARGET: usize = 800;

/// 单文件上限。再大就不是"个人文档"而是数据集了。
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// 能索引的扩展名。二进制格式先不收——读出来是乱码的"索引"没有意义。
const INDEXABLE_EXT: &[&str] = &[
    "txt", "md", "markdown", "json", "jsonl", "yaml", "yml", "toml", "ini", "csv", "tsv", "log",
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "h", "cpp", "hpp", "cs", "sh", "ps1",
    "html", "htm", "css", "scss", "xml", "sql",
];

#[derive(Debug, Clone, Serialize)]
pub struct IndexOutcome {
    pub path: String,
    pub title: String,
    pub sha256: String,
    pub bytes: u64,
    pub chunks: usize,
    /// 内容与已入库版本一致，没有重写
    pub unchanged: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkHit {
    pub path: String,
    pub title: String,
    pub ordinal: usize,
    pub score: usize,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentRow {
    pub id: String,
    pub path: String,
    pub title: String,
    pub ext: Option<String>,
    pub bytes: i64,
    pub chunks: i64,
    pub indexed_at: String,
    /// 源文件已不存在（索引是陈的）
    pub missing: bool,
}

pub fn is_indexable(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| INDEXABLE_EXT.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 按行聚合切块，单行过长时硬切。
///
/// 不按固定字数硬切整篇：那会把句子拦腰截断，
/// 检索出来的片段读起来像乱码。以空行分段更接近人读文档的方式。
pub fn chunk_text(text: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut buf = String::new();

    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            if !buf.trim().is_empty() {
                chunks.push(buf.trim().to_string());
                buf.clear();
            }
            continue;
        }

        let line_len = trimmed.chars().count();

        // 超长单行：先冲掉缓冲，再按目标长度硬切
        if line_len > CHUNK_TARGET * 2 {
            if !buf.trim().is_empty() {
                chunks.push(buf.trim().to_string());
                buf.clear();
            }
            let chars: Vec<char> = trimmed.chars().collect();
            for piece in chars.chunks(CHUNK_TARGET) {
                chunks.push(piece.iter().collect::<String>());
            }
            continue;
        }

        if buf.chars().count() + line_len + 1 > CHUNK_TARGET && !buf.trim().is_empty() {
            chunks.push(buf.trim().to_string());
            buf.clear();
        }
        buf.push_str(trimmed);
        buf.push('\n');
    }

    if !buf.trim().is_empty() {
        chunks.push(buf.trim().to_string());
    }
    chunks
}

/// 索引（或重新索引）一个文件。
///
/// 同一路径内容未变时不动数据库——重复索引最常发生在"用户反复让我记住这个文件夹"，
/// 每次都重写会让 `indexed_at` 失去意义，也会白白翻倍写放大。
pub fn index_file(conn: &Connection, path: &Path) -> AppResult<IndexOutcome> {
    let display = path.to_string_lossy().to_string();

    if !path.is_file() {
        return Err(AppError::InvalidArgument(format!("不是文件: {}", display)));
    }
    if !is_indexable(path) {
        return Err(AppError::InvalidArgument(format!(
            "不支持的文件类型（目前只索引文本类文件）: {}",
            display
        )));
    }
    let bytes = fs::read(path)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(AppError::InvalidArgument(format!(
            "文件超过 {} MB，未索引",
            MAX_FILE_BYTES / 1024 / 1024
        )));
    }

    let digest = sha256_hex(&bytes);
    let title = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| display.clone());
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    // 已索引进且摘要一致 → 什么都不做
    let existing: Option<(String, String)> = conn
        .query_row(
            "SELECT id, sha256 FROM knowledge_documents WHERE path = ?1",
            params![display],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .ok();

    if let Some((id, old_digest)) = &existing {
        if old_digest == &digest {
            let chunk_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM knowledge_chunks WHERE document_id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            return Ok(IndexOutcome {
                path: display,
                title,
                sha256: digest,
                bytes: bytes.len() as u64,
                chunks: chunk_count as usize,
                unchanged: true,
            });
        }
    }

    let text = String::from_utf8_lossy(&bytes).to_string();
    let chunks = chunk_text(&text);
    let doc_id = existing
        .as_ref()
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // 旧块先清掉：内容变了以后，旧块留在库里只会检索出已删除的段落
    conn.execute(
        "DELETE FROM knowledge_chunks WHERE document_id = ?1",
        params![doc_id],
    )?;

    conn.execute(
        "INSERT INTO knowledge_documents (id, path, title, ext, bytes, sha256, chunk_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(path) DO UPDATE SET
            title = excluded.title,
            ext = excluded.ext,
            bytes = excluded.bytes,
            sha256 = excluded.sha256,
            chunk_count = excluded.chunk_count,
            indexed_at = datetime('now')",
        params![
            doc_id,
            display,
            title,
            ext,
            bytes.len() as i64,
            digest,
            chunks.len() as i64
        ],
    )?;

    for (ordinal, chunk) in chunks.iter().enumerate() {
        conn.execute(
            "INSERT INTO knowledge_chunks (id, document_id, ordinal, content, chars)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                uuid::Uuid::new_v4().to_string(),
                doc_id,
                ordinal as i64,
                chunk,
                chunk.chars().count() as i64
            ],
        )?;
    }

    Ok(IndexOutcome {
        path: display,
        title,
        sha256: digest,
        bytes: bytes.len() as u64,
        chunks: chunks.len(),
        unchanged: false,
    })
}

/// 关键词检索。
///
/// 排序在 Rust 侧做：SQLite 的 LIKE 只能回答"有没有"，
/// 而用户真正想知道的是"哪一段最相关"。
pub fn search(conn: &Connection, query: &str, limit: usize) -> AppResult<Vec<ChunkHit>> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let like_parts: Vec<String> = (0..terms.len())
        .map(|i| format!("c.content LIKE ?{}", i + 1))
        .collect();
    let sql = format!(
        "SELECT c.content, c.ordinal, d.path, d.title
         FROM knowledge_chunks c
         JOIN knowledge_documents d ON d.id = c.document_id
         WHERE {}
         LIMIT ?{}",
        like_parts.join(" OR "),
        terms.len() + 1
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut bound: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    for term in &terms {
        bound.push(Box::new(format!("%{}%", term)));
    }
    // 先多取一些再排序，避免"命中很多但都被 LIMIT 截掉"
    bound.push(Box::new(((limit.max(1) * 5) as i64).min(2000)));

    let refs: Vec<&dyn rusqlite::ToSql> = bound.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(refs.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    let mut hits: Vec<ChunkHit> = Vec::new();
    for row in rows.flatten() {
        let (content, ordinal, path, title) = row;
        let lower = content.to_lowercase();
        let score: usize = terms
            .iter()
            .map(|t| lower.matches(t.as_str()).count())
            .sum();
        if score == 0 {
            continue;
        }
        hits.push(ChunkHit {
            snippet: make_snippet(&content, &terms),
            path,
            title,
            ordinal: ordinal as usize,
            score,
        });
    }

    hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.ordinal.cmp(&b.ordinal)));
    hits.truncate(limit.max(1));
    Ok(hits)
}

/// 截取命中词附近的上下文，而不是永远返回块的开头。
///
/// 用户搜"部署脚本"时想看的是那句话，不是块首的文档标题。
fn make_snippet(content: &str, terms: &[String]) -> String {
    const WINDOW: usize = 160;

    let lower = content.to_lowercase();
    let first_hit = terms.iter().filter_map(|t| lower.find(t.as_str())).min();

    let chars: Vec<char> = content.chars().collect();
    if chars.len() <= WINDOW * 2 {
        return content.to_string();
    }

    let hit_char = first_hit
        .map(|byte_idx| content[..byte_idx].chars().count())
        .unwrap_or(0);
    let start = hit_char.saturating_sub(WINDOW / 4).min(chars.len());
    let end = (start + WINDOW * 2).min(chars.len());

    let mut snippet: String = chars[start..end].iter().collect();
    if start > 0 {
        snippet.insert_str(0, "…");
    }
    if end < chars.len() {
        snippet.push('…');
    }
    snippet
}

/// 列出已索引文档
pub fn list_documents(conn: &Connection, limit: usize) -> AppResult<Vec<DocumentRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, title, ext, bytes, chunk_count, indexed_at
         FROM knowledge_documents
         ORDER BY indexed_at DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(DocumentRow {
            id: row.get(0)?,
            path: row.get(1)?,
            title: row.get(2)?,
            ext: row.get(3)?,
            bytes: row.get(4)?,
            chunks: row.get(5)?,
            indexed_at: row.get(6)?,
            missing: false,
        })
    })?;

    let mut docs: Vec<DocumentRow> = rows.filter_map(|r| r.ok()).collect();
    for doc in &mut docs {
        doc.missing = !Path::new(&doc.path).is_file();
    }
    Ok(docs)
}

/// 从索引移除一个文件（不动磁盘上的原文件）
pub fn remove_document(conn: &Connection, path: &str) -> AppResult<bool> {
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM knowledge_documents WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )
        .ok();
    let Some(id) = id else {
        return Ok(false);
    };
    conn.execute(
        "DELETE FROM knowledge_chunks WHERE document_id = ?1",
        params![id],
    )?;
    conn.execute("DELETE FROM knowledge_documents WHERE id = ?1", params![id])?;
    Ok(true)
}

pub fn stats(conn: &Connection) -> AppResult<(i64, i64, i64)> {
    let docs: i64 = conn.query_row("SELECT COUNT(*) FROM knowledge_documents", [], |r| r.get(0))?;
    let chunks: i64 = conn.query_row("SELECT COUNT(*) FROM knowledge_chunks", [], |r| r.get(0))?;
    let chars: i64 = conn.query_row(
        "SELECT COALESCE(SUM(chars), 0) FROM knowledge_chunks",
        [],
        |r| r.get(0),
    )?;
    Ok((docs, chunks, chars))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("内存库");
        conn.execute_batch(crate::db::schema::MIGRATION_KNOWLEDGE_SQL)
            .expect("建表");
        conn
    }

    fn temp_doc(tag: &str, name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pcg-kb-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn chunks_stay_within_target_and_keep_text() {
        // 30 段只有约 720 字，还没到单块目标，不会被切开——
        // 要验证"确实会分块"，样本就必须长于一个块。
        let para = "这是一段用于测试分块的中文内容，长度适中。\n".repeat(80);
        let chunks = chunk_text(&para);
        assert!(chunks.len() > 1, "长文本必须被切成多块");
        for chunk in &chunks {
            assert!(!chunk.trim().is_empty(), "不该产生空块");
            assert!(
                chunk.chars().count() <= CHUNK_TARGET * 2,
                "单块不应超过目标的 2 倍"
            );
        }
        let joined: String = chunks.concat();
        assert!(joined.contains("长度适中"), "分块不能丢内容");
    }

    #[test]
    fn oversized_single_line_is_hard_split() {
        let line = "字".repeat(3000);
        let chunks = chunk_text(&line);
        assert!(chunks.len() >= 2);
        assert_eq!(
            chunks.iter().map(|c| c.chars().count()).sum::<usize>(),
            3000
        );
    }

    #[test]
    fn index_then_search_finds_the_right_chunk() {
        let conn = mem_conn();
        let path = temp_doc(
            "search",
            "notes.md",
            "# 项目笔记\n\n部署脚本放在 scripts/deploy.ps1，执行前要先跑 build。\n\n另一段讲的是数据库备份策略。",
        );

        let first = index_file(&conn, &path).expect("首次索引");
        assert_eq!(first.unchanged, false);
        assert!(first.chunks >= 1);

        // 同内容再索引一次：必须标记 unchanged，且不重复写块
        let again = index_file(&conn, &path).expect("重复索引");
        assert!(again.unchanged, "内容未变时应跳过重写");
        let chunk_total: i64 = conn
            .query_row("SELECT COUNT(*) FROM knowledge_chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(chunk_total as usize, first.chunks, "重复索引不该翻倍");

        let hits = search(&conn, "部署脚本", 5).expect("检索");
        assert_eq!(hits.len(), 1, "只应命中讲部署的那一块");
        assert!(hits[0].snippet.contains("deploy.ps1"));
        assert_eq!(hits[0].title, "notes.md");

        let none = search(&conn, "完全不存在的词", 5).expect("检索");
        assert!(none.is_empty());

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn changed_content_replaces_old_chunks() {
        let conn = mem_conn();
        let path = temp_doc("replace", "a.txt", "第一版内容：旧的关键字 alpha");
        index_file(&conn, &path).expect("首次");

        fs::write(&path, "第二版内容：新的关键字 beta").unwrap();
        let second = index_file(&conn, &path).expect("重新索引");
        assert!(!second.unchanged);

        assert!(
            search(&conn, "alpha", 5).unwrap().is_empty(),
            "旧内容必须从索引里消失"
        );
        assert_eq!(search(&conn, "beta", 5).unwrap().len(), 1);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn remove_document_clears_chunks_but_keeps_file() {
        let conn = mem_conn();
        let path = temp_doc("remove", "b.txt", "待删除索引的内容 gamma");
        index_file(&conn, &path).expect("索引");

        assert!(remove_document(&conn, &path.to_string_lossy()).expect("移除"));
        let (docs, chunks, _) = stats(&conn).expect("统计");
        assert_eq!((docs, chunks), (0, 0));
        assert!(path.exists(), "移除索引不该动磁盘上的文件");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn rejects_binary_and_out_of_scope_files() {
        let conn = mem_conn();
        let path = temp_doc("reject", "image.png", "not really a png");
        let err = index_file(&conn, &path).expect_err("非文本扩展名应被拒绝");
        assert!(err.to_string().contains("不支持"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
