//! 自省工具（client 域）
//!
//! 让助手能回答关于**自己**的问题：我有哪些能力、刚才那些调用到底成了没有、
//! 库是不是好的、跑多久了。
//!
//! 这一域的价值不在功能，在于**打断猜**：模型此前被问"你能不能剪视频"，
//! 只能凭 System Prompt 里的工具名猜；被问"刚才那个成功了吗"，只能复述自己
//! 的说法。现在这些都能查。

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Instant;

use crate::error::{AppError, AppResult};
use crate::tools::{contract, registry, AgentTool, RiskLevel, ToolResult};

// ============================================================
// 启动时刻
// ============================================================

static STARTED_AT: OnceLock<Instant> = OnceLock::new();

/// 应用初始化时调用一次，之后 `client_state` 才能给出真实的运行时长。
pub fn mark_startup() {
    let _ = STARTED_AT.set(Instant::now());
}

fn uptime_seconds() -> Option<u64> {
    STARTED_AT.get().map(|at| at.elapsed().as_secs())
}

// ============================================================
// 公共辅助
// ============================================================

fn require_conn() -> AppResult<rusqlite::Connection> {
    crate::memory::open_connection().ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))
}

/// 表名只允许字母数字下划线：来源虽是 sqlite_master，但拼进 SQL 前仍要过一遍。
fn safe_table_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 统计每张表的行数，按行数倒序。
fn table_row_counts(conn: &rusqlite::Connection, limit: usize) -> Vec<(String, i64)> {
    let mut tables: Vec<String> = Vec::new();
    if let Ok(mut stmt) = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
    {
        if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
            for name in rows.flatten() {
                if safe_table_name(&name) {
                    tables.push(name);
                }
            }
        }
    }

    let mut counts: Vec<(String, i64)> = tables
        .into_iter()
        .map(|name| {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {}", name), [], |row| {
                    row.get(0)
                })
                .unwrap_or(-1);
            (name, count)
        })
        .collect();

    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts.truncate(limit);
    counts
}

// ============================================================
// client_state — 自身状态
// ============================================================

pub struct ClientStateTool;

#[async_trait]
impl AgentTool for ClientStateTool {
    fn name(&self) -> &'static str {
        "client_state"
    }

    fn description(&self) -> &'static str {
        "读助手自身的运行状态：版本、数据库位置与体积、各表行数、已加载能力数、运行时长。回答「你是什么版本」「数据存在哪」「里面有多少记录」时用。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "table_limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "返回多少张表的行数（可选，默认 10）"
                }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let table_limit = args["table_limit"].as_u64().unwrap_or(10).clamp(1, 50) as usize;
        let conn = require_conn()?;

        let db_path = crate::memory::get_db_path().map(|p| p.to_string_lossy().to_string());
        let db_bytes = db_path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        let wal_bytes = db_path
            .as_ref()
            .map(|p| format!("{}-wal", p))
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);

        let table_counts = table_row_counts(&conn, table_limit);
        let total_rows: i64 = table_counts.iter().map(|(_, rows)| (*rows).max(0)).sum();

        let snapshot = registry::registry_snapshot();
        let categories: std::collections::BTreeMap<&str, usize> =
            snapshot
                .iter()
                .fold(Default::default(), |mut acc, (_, category)| {
                    *acc.entry(category.as_str()).or_insert(0) += 1;
                    acc
                });

        Ok(ToolResult::ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "db_path": db_path,
            "db_bytes": db_bytes,
            "wal_bytes": wal_bytes,
            "uptime_seconds": uptime_seconds(),
            "capabilities": {
                "registered": snapshot.len(),
                "declared_contracts": contract::contracts().len(),
                "by_category": categories,
            },
            "tables": table_counts.iter().map(|(name, rows)| json!({
                "name": name,
                "rows": rows,
            })).collect::<Vec<_>>(),
            "top_tables_total_rows": total_rows,
        })))
    }
}

// ============================================================
// client_capability_manifest — 能力清单
// ============================================================

pub struct CapabilityManifestTool;

#[async_trait]
impl AgentTool for CapabilityManifestTool {
    fn name(&self) -> &'static str {
        "client_capability_manifest"
    }

    fn description(&self) -> &'static str {
        "查助手实际有哪些能力（按类别分组的权威清单），含每项的副作用类型与验证方式。用户问「你能做什么」「有没有办法做 X」时先查这个，不要凭印象回答。可按类别或关键词过滤。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "description": "只看这个类别（file / program / system / memory / knowledge / office / agents / messaging / media / browser / general …）"
                },
                "keyword": {
                    "type": "string",
                    "description": "按工具名或副作用类型做子串过滤"
                },
                "only_material": {
                    "type": "boolean",
                    "description": "只看有实质副作用的能力"
                }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let category_filter = args["category"]
            .as_str()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        let keyword = args["keyword"]
            .as_str()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        let only_material = args["only_material"].as_bool().unwrap_or(false);

        let snapshot = registry::registry_snapshot();
        let category_of = |name: &str| -> String {
            snapshot
                .iter()
                .find(|(tool, _)| tool == name)
                .map(|(_, category)| category.clone())
                .unwrap_or_else(|| "unregistered".to_string())
        };

        let mut items: Vec<Value> = Vec::new();
        for cap in contract::contracts() {
            let category = category_of(cap.tool);
            if let Some(filter) = &category_filter {
                if &category != filter {
                    continue;
                }
            }
            if only_material && !cap.side_effect.is_material() {
                continue;
            }
            if let Some(word) = &keyword {
                let haystack = format!(
                    "{} {} {}",
                    cap.tool,
                    cap.side_effect.as_str(),
                    cap.verification.as_str()
                )
                .to_lowercase();
                if !haystack.contains(word) {
                    continue;
                }
            }
            items.push(json!({
                "tool": cap.tool,
                "category": category,
                "side_effect": cap.side_effect.as_str(),
                "scope": cap.scope,
                "reversible": cap.reversible,
                "verification": cap.verification.as_str(),
                "required_fields": cap.required_fields,
                "verification_debt": contract::verification_debt(cap.tool),
            }));
        }

        // 注册了但没契约的工具：必须单独列出来，它们正是"没人说要验证"的那批
        let unregistered: Vec<String> = snapshot
            .iter()
            .filter(|(name, _)| contract::contract_for(name).is_none())
            .map(|(name, _)| name.clone())
            .collect();

        let by_category: std::collections::BTreeMap<String, usize> =
            items.iter().fold(Default::default(), |mut acc, item| {
                let category = item["category"].as_str().unwrap_or("unknown");
                *acc.entry(category.to_string()).or_insert(0) += 1;
                acc
            });

        Ok(ToolResult::ok(json!({
            "count": items.len(),
            "by_category": by_category,
            "items": items,
            "without_contract": unregistered,
        })))
    }
}

// ============================================================
// client_health_check — 自检
// ============================================================

pub struct ClientHealthCheckTool;

#[async_trait]
impl AgentTool for ClientHealthCheckTool {
    fn name(&self) -> &'static str {
        "client_health_check"
    }

    fn description(&self) -> &'static str {
        "对助手自身做一次体检：数据库完整性、数据目录可写性、契约覆盖缺口、模型配置是否齐全、最近失败的回执。回答「你为什么不好用」「有没有问题」时用，别猜。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: Value) -> AppResult<ToolResult> {
        let conn = require_conn()?;
        let mut findings: Vec<String> = Vec::new();

        // 1. 数据库完整性
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap_or_else(|_| "unknown".to_string());
        if integrity != "ok" {
            findings.push(format!("数据库完整性检查未通过：{}", integrity));
        }

        // 2. 数据目录可写（真写一个文件再删，不看权限位）
        let mut dir_writable = false;
        if let Some(path) = crate::memory::get_db_path() {
            if let Some(parent) = path.parent() {
                let probe = parent.join(".health-probe");
                if std::fs::write(&probe, b"ok").is_ok() {
                    dir_writable = std::fs::remove_file(&probe).is_ok();
                }
            }
        }
        if !dir_writable {
            findings.push("数据目录不可写：设置、记忆、回执都存不下去".to_string());
        }

        // 3. 契约覆盖
        let registered: Vec<String> = registry::registry_snapshot()
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        let coverage = contract::coverage(&registered);
        if !coverage.missing.is_empty() {
            findings.push(format!(
                "{} 个能力没有契约：{}",
                coverage.missing.len(),
                coverage.missing.join(", ")
            ));
        }
        if !coverage.material_without_verification.is_empty() {
            findings.push(format!(
                "{} 个实质副作用能力既无验证策略也无欠账说明：{}",
                coverage.material_without_verification.len(),
                coverage.material_without_verification.join(", ")
            ));
        }

        // 4. 模型配置
        let model_base = crate::memory::open_connection().and_then(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'ollama.base_url'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
        });
        let model_name = crate::memory::open_connection().and_then(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'ollama.model'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
        });
        if model_base.is_none() || model_name.is_none() {
            findings.push("模型配置不完整：ollama.base_url / ollama.model 至少缺一个".to_string());
        }

        // 5. 最近失败的回执
        let recent_failed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tool_receipts
                 WHERE success = 0 AND created_at >= datetime('now', '-1 day')",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if recent_failed > 0 {
            findings.push(format!("过去 24 小时有 {} 次工具调用失败", recent_failed));
        }

        let status = if findings
            .iter()
            .any(|f| f.contains("完整性") || f.contains("不可写"))
        {
            "attention"
        } else if findings.is_empty() {
            "ready"
        } else {
            "working"
        };

        Ok(ToolResult::ok(json!({
            "status": status,
            "integrity": integrity,
            "dir_writable": dir_writable,
            "coverage": {
                "total": coverage.total_tools,
                "declared": coverage.declared,
                "material": coverage.material,
                "material_verified": coverage.material_verified,
                "verification_debt": coverage.material_with_verification_debt,
            },
            "model": { "base_url": model_base, "model": model_name },
            "recent_failures_24h": recent_failed,
            "findings": findings,
        })))
    }
}

// ============================================================
// client_usage_stats — 用事实说话
// ============================================================

pub struct ClientUsageStatsTool;

#[async_trait]
impl AgentTool for ClientUsageStatsTool {
    fn name(&self) -> &'static str {
        "client_usage_stats"
    }

    fn description(&self) -> &'static str {
        "按回执账本统计工具使用情况：总调用数、各验证结论的分布、哪些能力最常被调用、平均耗时、最近失败明细。回答「最近都干了什么」「哪个工具老出错」时用，这是账本统计，不是估算。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "days": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 365,
                    "description": "统计最近多少天（可选，默认 7）"
                },
                "top": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 50,
                    "description": "最常用能力列前几名（可选，默认 5）"
                }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let days = args["days"].as_u64().unwrap_or(7).clamp(1, 365) as i64;
        let top = args["top"].as_u64().unwrap_or(5).clamp(1, 50) as i64;
        let conn = require_conn()?;

        let window = format!("-{} days", days);

        // 分状态计数
        let mut by_status: Vec<Value> = Vec::new();
        let mut total = 0i64;
        if let Ok(mut stmt) = conn.prepare(
            "SELECT verification_status, COUNT(*) FROM tool_receipts
             WHERE created_at >= datetime('now', ?1)
             GROUP BY verification_status ORDER BY COUNT(*) DESC",
        ) {
            if let Ok(rows) = stmt.query_map([&window], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            }) {
                for (status, count) in rows.flatten() {
                    total += count;
                    by_status.push(json!({ "status": status, "count": count }));
                }
            }
        }

        // 最常用
        let mut top_tools: Vec<Value> = Vec::new();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT tool_name, COUNT(*) AS calls,
                    SUM(CASE WHEN success = 1 THEN 1 ELSE 0 END) AS ok_calls,
                    AVG(duration_ms) AS avg_ms
             FROM tool_receipts
             WHERE created_at >= datetime('now', ?1)
             GROUP BY tool_name ORDER BY calls DESC LIMIT ?2",
        ) {
            if let Ok(rows) = stmt.query_map(rusqlite::params![&window, top], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                ))
            }) {
                for (tool, calls, ok_calls, avg_ms) in rows.flatten() {
                    top_tools.push(json!({
                        "tool": tool,
                        "calls": calls,
                        "ok": ok_calls,
                        "failed": calls - ok_calls,
                        "avg_ms": avg_ms.map(|v| v.round() as i64),
                    }));
                }
            }
        }

        // 最近失败明细
        let mut recent_failures: Vec<Value> = Vec::new();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT tool_name, error_code, verification_status, created_at
             FROM tool_receipts
             WHERE created_at >= datetime('now', ?1)
               AND (success = 0 OR verification_status IN ('FAILED', 'CONTRADICTED'))
             ORDER BY created_at DESC LIMIT 5",
        ) {
            if let Ok(rows) = stmt.query_map([&window], |row| {
                Ok(json!({
                    "tool": row.get::<_, String>(0)?,
                    "error_code": row.get::<_, Option<String>>(1)?,
                    "verification_status": row.get::<_, String>(2)?,
                    "created_at": row.get::<_, String>(3)?,
                }))
            }) {
                recent_failures = rows.flatten().collect();
            }
        }

        Ok(ToolResult::ok(json!({
            "window_days": days,
            "total_receipts": total,
            "by_status": by_status,
            "top_tools": top_tools,
            "recent_failures": recent_failures,
        })))
    }
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_name_guard_rejects_injection_shapes() {
        assert!(safe_table_name("tool_receipts"));
        assert!(safe_table_name("agent_tasks"));
        assert!(!safe_table_name("t; DROP TABLE settings"));
        assert!(!safe_table_name("t--"));
        assert!(!safe_table_name(""));
    }

    #[test]
    fn mark_startup_enables_uptime() {
        // 未标记前返回 None（宁可说"不知道"，也不编一个 0 秒）
        mark_startup();
        assert!(uptime_seconds().is_some(), "标记后必须能读到运行时长");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::tools::ToolRegistry;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// 真实注册表 → 快照 → 清单 的端到端对齐检查。
    ///
    /// 这个测试守的是最容易静默退化的地方：新增工具忘了写契约时，
    /// `without_contract` 会多出一条，或者清单条数和实际注册数对不上。
    #[tokio::test]
    #[ignore = "要构造完整 ToolRegistry，手动跑"]
    async fn manifest_matches_the_real_registry() {
        let dir = std::env::temp_dir().join(format!("pcg-client-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        let db_path = dir.join("client.db");
        let db = crate::db::Database::new(&db_path).expect("开库");
        db.init_schema().expect("建表");
        crate::memory::set_db_path(db_path);
        mark_startup();

        let registry = ToolRegistry::new(Arc::new(Mutex::new(db)), dir.clone());
        let registered = registry.len();

        let manifest = CapabilityManifestTool
            .execute(json!({}))
            .await
            .expect("capability manifest");
        assert!(manifest.success, "{:?}", manifest.error);
        let data = manifest.data.expect("有数据");

        assert_eq!(
            data["without_contract"].as_array().map(|a| a.len()),
            Some(0),
            "有工具没写契约：{:?}",
            data["without_contract"]
        );
        assert_eq!(
            data["count"].as_u64().unwrap_or(0) as usize,
            registered,
            "清单条数与实际注册数不符"
        );
        // 新补齐的四个域必须在清单里出现
        let categories = data["by_category"].as_object().expect("有分类统计");
        for expected in ["agents", "messaging", "media", "client"] {
            let count = categories
                .get(expected)
                .and_then(Value::as_u64)
                .unwrap_or(0);
            assert!(count > 0, "类别 {} 的能力数为 0", expected);
        }

        let health = ClientHealthCheckTool
            .execute(json!({}))
            .await
            .expect("health check");
        assert!(health.success, "{:?}", health.error);
        let h = health.data.expect("有数据");
        assert_eq!(h["integrity"], "ok");
        assert_eq!(h["dir_writable"], true);

        let usage = ClientUsageStatsTool
            .execute(json!({ "days": 7 }))
            .await
            .expect("usage stats");
        assert!(usage.success, "{:?}", usage.error);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
