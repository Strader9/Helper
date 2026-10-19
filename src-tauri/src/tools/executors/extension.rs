//! 扩展注册表工具
//!
//! 把 `crate::extensions` 的注册表暴露为 LLM 可调用工具：
//! list / install / disable / enable / receipts。
//!
//! 设计边界：安装一个扩展 = 记清单 + 记回执 + 物化它声明的官方技能；
//! 只动本地扩展表与本地技能目录，不下载、不签名、不执行外部代码。

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::extensions as ext;
use crate::tools::{AgentTool, RiskLevel, ToolResult};

fn require_str<'a>(args: &'a Value, key: &str) -> AppResult<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::InvalidArgument(format!("{} is required", key)))
}

fn str_array(args: &Value, key: &str) -> Vec<String> {
    match &args[key] {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        Value::String(s) => s
            .split(|c| c == '\n' || c == ',' || c == '，')
            .map(|x| x.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn ext_json(e: &ext::Extension) -> Value {
    json!({
        "id": e.id,
        "name": e.name,
        "version": e.version,
        "publisher": e.publisher,
        "description": e.description,
        "status": e.status,
        "permissions": e.permissions,
        "skill_ids": e.skill_ids,
        "created_at": e.created_at,
        "updated_at": e.updated_at,
    })
}

// ============================================================
// extension_list
// ============================================================

pub struct ExtensionListTool;

#[async_trait]
impl AgentTool for ExtensionListTool {
    fn name(&self) -> &'static str {
        "extension_list"
    }

    fn description(&self) -> &'static str {
        "列出本地已安装的扩展（含状态、版本、发布者、声明的权限与技能）。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: Value) -> AppResult<ToolResult> {
        let extensions = ext::list_extensions()?;
        Ok(ToolResult::ok(json!({
            "count": extensions.len(),
            "extensions": extensions.iter().map(ext_json).collect::<Vec<_>>(),
        })))
    }
}

// ============================================================
// extension_install
// ============================================================

pub struct ExtensionInstallTool;

#[async_trait]
impl AgentTool for ExtensionInstallTool {
    fn name(&self) -> &'static str {
        "extension_install"
    }

    fn description(&self) -> &'static str {
        "安装一个扩展：记一条扩展清单 + install 回执，并把它声明的 skill_ids 里、来自官方技能市场的技能物化到本地技能目录。skill_ids 必须是官方市场已有的技能。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "version": { "type": "string" },
                "publisher": { "type": "string" },
                "description": { "type": "string" },
                "permissions": { "type": "array", "items": { "type": "string" } },
                "skill_ids": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["name"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let name = require_str(&args, "name")?;
        let skill_ids = str_array(&args, "skill_ids");

        // 物化声明的官方技能（已存在的跳过）
        let mut installed_skills = Vec::new();
        for sid in &skill_ids {
            let Some((def, script)) = crate::skills::marketplace::bundled_package(sid) else {
                return Err(AppError::InvalidArgument(format!(
                    "官方市场里没有技能「{}」。skill_ids 只能用 skill_marketplace_search 返回的技能。",
                    sid
                )));
            };
            // 已安装则跳过，否则物化
            let already = crate::skills::SkillLoader::skills_root().join(sid).exists();
            if already {
                continue;
            }
            match crate::skills::SkillLoader::install_bundled(def, script) {
                Ok(_) => installed_skills.push(sid.clone()),
                Err(e) => {
                    // 已存在视为成功（并发/重试）
                    if e.to_string().contains("已存在") {
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        let input = ext::ExtensionInput {
            name: name.to_string(),
            version: args["version"].as_str().unwrap_or("1.0.0").to_string(),
            publisher: args["publisher"].as_str().unwrap_or("").to_string(),
            description: args["description"].as_str().unwrap_or("").to_string(),
            permissions: str_array(&args, "permissions"),
            skill_ids,
        };
        let extension = ext::create_extension(input)?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "installed",
            "extension": ext_json(&extension),
            "installed_skills": installed_skills,
        })))
    }
}

// ============================================================
// extension_disable
// ============================================================

pub struct ExtensionDisableTool;

#[async_trait]
impl AgentTool for ExtensionDisableTool {
    fn name(&self) -> &'static str {
        "extension_disable"
    }

    fn description(&self) -> &'static str {
        "禁用一个已安装的扩展（只改扩展状态并记回执，不删除清单与历史）。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let id = require_str(&args, "id")?;
        let extension = ext::set_status(id, "disable", "disabled")?
            .ok_or_else(|| AppError::ToolNotFound(format!("扩展不存在: {}", id)))?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "disabled",
            "extension": ext_json(&extension),
        })))
    }
}

// ============================================================
// extension_enable
// ============================================================

pub struct ExtensionEnableTool;

#[async_trait]
impl AgentTool for ExtensionEnableTool {
    fn name(&self) -> &'static str {
        "extension_enable"
    }

    fn description(&self) -> &'static str {
        "重新启用一个已禁用的扩展（只改扩展状态并记回执）。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let id = require_str(&args, "id")?;
        let extension = ext::set_status(id, "enable", "installed")?
            .ok_or_else(|| AppError::ToolNotFound(format!("扩展不存在: {}", id)))?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "enabled",
            "extension": ext_json(&extension),
        })))
    }
}

// ============================================================
// extension_receipts
// ============================================================

pub struct ExtensionReceiptsTool;

#[async_trait]
impl AgentTool for ExtensionReceiptsTool {
    fn name(&self) -> &'static str {
        "extension_receipts"
    }

    fn description(&self) -> &'static str {
        "读扩展生命周期回执账本（install/enable/disable），不暴露任何密钥或响应体。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "extension_id": { "type": "string", "description": "可选，按扩展过滤" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let extension_id = args["extension_id"].as_str();
        let receipts = ext::list_receipts(extension_id)?;
        Ok(ToolResult::ok(json!({
            "count": receipts.len(),
            "receipts": receipts.iter().map(|r| json!({
                "id": r.id,
                "extension_id": r.extension_id,
                "action": r.action,
                "detail": r.detail,
                "created_at": r.created_at,
            })).collect::<Vec<_>>(),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_array_parses_json_and_comma_string() {
        assert_eq!(str_array(&json!({"x": ["a", "b"]}), "x"), vec!["a", "b"]);
        assert_eq!(str_array(&json!({"x": "a,b,c"}), "x"), vec!["a", "b", "c"]);
        assert_eq!(str_array(&json!({"x": 1}), "x"), Vec::<String>::new());
    }
}
