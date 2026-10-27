//! 扩展注册表（extension registry）
//!
//! 一个**本地扩展清单 + 生命周期回执账本**。与早期原型的 extension registry
//! 对齐的只是"注册表 + 回执"这一层语义；e-assisant 的扩展不是"远程签名
//! provider 包"，而是**把一组官方技能打包成一个可整体启停的单元**。
//!
//! 安装一个扩展 = 记一条扩展清单 + 记一条 install 回执，并把它声明的
//! `skill_ids` 里、来自官方技能市场的技能物化到本地技能目录。启用/禁用
//! 只改扩展状态（技能的启停由 skill_toggle 负责），并各记一条回执，
//! 形成可审计的生命周期账本。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};

/// 一条扩展记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extension {
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub description: String,
    pub permissions: Vec<String>,
    pub skill_ids: Vec<String>,
    pub status: String,
    pub manifest: Value,
    pub created_at: String,
    pub updated_at: String,
}

/// 一条生命周期回执。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionReceipt {
    pub id: i64,
    pub extension_id: String,
    pub action: String,
    pub detail: Value,
    pub created_at: String,
}

fn conn() -> AppResult<rusqlite::Connection> {
    crate::memory::open_connection().ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))
}

fn parse_str_array(v: &str) -> Vec<String> {
    serde_json::from_str(v).unwrap_or_default()
}

const COLS: &str = "id, name, version, publisher, description, permissions_json, skill_ids_json, status, manifest_json, created_at, updated_at";

fn row_to_extension(row: &rusqlite::Row) -> rusqlite::Result<Extension> {
    let permissions: String = row.get(5)?;
    let skill_ids: String = row.get(6)?;
    let manifest: String = row.get(8)?;
    Ok(Extension {
        id: row.get(0)?,
        name: row.get(1)?,
        version: row.get(2)?,
        publisher: row.get(3)?,
        description: row.get(4)?,
        permissions: parse_str_array(&permissions),
        skill_ids: parse_str_array(&skill_ids),
        status: row.get(7)?,
        manifest: serde_json::from_str(&manifest).unwrap_or_else(|_| json!({})),
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn row_to_receipt(row: &rusqlite::Row) -> rusqlite::Result<ExtensionReceipt> {
    let detail: String = row.get(2)?;
    Ok(ExtensionReceipt {
        id: row.get(0)?,
        extension_id: row.get(1)?,
        action: row.get(3)?,
        detail: serde_json::from_str(&detail).unwrap_or_else(|_| json!({})),
        created_at: row.get(4)?,
    })
}

/// 建扩展入参。
pub struct ExtensionInput {
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub description: String,
    pub permissions: Vec<String>,
    pub skill_ids: Vec<String>,
}

/// 记一条扩展 + install 回执。
pub fn create_extension(input: ExtensionInput) -> AppResult<Extension> {
    let c = conn()?;
    let id = uuid::Uuid::new_v4().to_string();
    let manifest = json!({
        "name": input.name,
        "version": input.version,
        "publisher": input.publisher,
        "description": input.description,
        "permissions": input.permissions,
        "skill_ids": input.skill_ids,
    });
    c.execute(
        "INSERT INTO extensions
         (id, name, version, publisher, description, permissions_json, skill_ids_json, status, manifest_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'installed', ?8)",
        rusqlite::params![
            id,
            input.name,
            input.version,
            input.publisher,
            input.description,
            serde_json::to_string(&input.permissions)?,
            serde_json::to_string(&input.skill_ids)?,
            serde_json::to_string(&manifest)?,
        ],
    )?;
    record_receipt(&id, "install", &json!({ "skill_ids": input.skill_ids }))?;
    get_extension(&id)?.ok_or_else(|| AppError::Internal("扩展创建后读回失败".to_string()))
}

pub fn get_extension(id: &str) -> AppResult<Option<Extension>> {
    let c = conn()?;
    let mut stmt = c.prepare(&format!("SELECT {} FROM extensions WHERE id = ?1", COLS))?;
    let mut rows = stmt.query([id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row_to_extension(row)?))
    } else {
        Ok(None)
    }
}

pub fn list_extensions() -> AppResult<Vec<Extension>> {
    let c = conn()?;
    let mut stmt = c.prepare(&format!(
        "SELECT {} FROM extensions ORDER BY updated_at DESC",
        COLS
    ))?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(row_to_extension(row)?);
    }
    Ok(out)
}

/// 改扩展状态（enable / disable），并记一条回执。
pub fn set_status(id: &str, action: &str, status: &str) -> AppResult<Option<Extension>> {
    let c = conn()?;
    let changed = c.execute(
        "UPDATE extensions SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
        rusqlite::params![status, id],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    record_receipt(id, action, &json!({ "status": status }))?;
    get_extension(id)
}

/// 记一条回执。
pub fn record_receipt(extension_id: &str, action: &str, detail: &Value) -> AppResult<()> {
    let c = conn()?;
    c.execute(
        "INSERT INTO extension_receipts (extension_id, action, detail_json) VALUES (?1, ?2, ?3)",
        rusqlite::params![extension_id, action, serde_json::to_string(detail)?],
    )?;
    Ok(())
}

/// 列回执（可选按扩展过滤）。
pub fn list_receipts(extension_id: Option<&str>) -> AppResult<Vec<ExtensionReceipt>> {
    let c = conn()?;
    let mut out = Vec::new();
    if let Some(eid) = extension_id.filter(|s| !s.is_empty()) {
        let mut stmt = c.prepare(
            "SELECT id, extension_id, detail_json, action, created_at
             FROM extension_receipts WHERE extension_id = ?1 ORDER BY id DESC",
        )?;
        let mut rows = stmt.query([eid])?;
        while let Some(row) = rows.next()? {
            out.push(row_to_receipt(row)?);
        }
    } else {
        let mut stmt = c.prepare(
            "SELECT id, extension_id, detail_json, action, created_at
             FROM extension_receipts ORDER BY id DESC",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            out.push(row_to_receipt(row)?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_row_roundtrip_uses_named_column_order() {
        // COLS 与 row_to_extension 的索引必须一致，这里只做静态自检：
        // 列序 = id,name,version,publisher,description,permissions,skill_ids,status,manifest,created_at,updated_at
        let cols: Vec<&str> = COLS.split(',').map(|s| s.trim()).collect();
        assert_eq!(cols[5], "permissions_json");
        assert_eq!(cols[6], "skill_ids_json");
        assert_eq!(cols[8], "manifest_json");
        assert_eq!(cols.len(), 11);
    }
}
