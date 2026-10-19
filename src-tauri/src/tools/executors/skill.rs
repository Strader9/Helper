//! 技能管理 + 技能市场工具
//!
//! 把 `crate::skills` 的现有能力暴露为 LLM 可调用的工具：
//! - 管理：list / get / toggle / install（从目录）/ uninstall
//! - 市场：search（搜官方目录）/ install（装官方打包技能）
//!
//! 设计边界：安装与卸载只动本地技能目录（%APPDATA%/pc-guardian/skills），
//! 技能本体运行时走 `SkillTool` 的权限分级，外部副作用由技能自己声明。

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::skills::{self, SkillDefinition};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

/// 取全局技能管理器；未初始化时先初始化（真实应用在 main.rs 已初始化）。
async fn manager() -> &'static skills::SkillManager {
    skills::init_global().await
}

fn skill_json(s: &SkillDefinition) -> Value {
    json!({
        "id": s.id,
        "name": s.name,
        "description": s.description,
        "version": s.version,
        "author": s.author,
        "type": s.skill_type.as_str(),
        "enabled": s.enabled,
        "installed_at": s.installed_at,
        "triggers": s.triggers,
        "permissions": s.permissions,
        "parameters": s.parameters.iter().map(|p| json!({
            "name": p.name,
            "description": p.description,
            "required": p.required,
            "type": p.param_type,
        })).collect::<Vec<_>>(),
    })
}

fn require_str<'a>(args: &'a Value, key: &str) -> AppResult<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AppError::InvalidArgument(format!("{} is required", key)))
}

// ============================================================
// skill_list
// ============================================================

pub struct SkillListTool;

#[async_trait]
impl AgentTool for SkillListTool {
    fn name(&self) -> &'static str {
        "skill_list"
    }

    fn description(&self) -> &'static str {
        "列出所有已安装技能（内置 + 本地），含 id、名称、描述、启用状态与权限。要判断有没有现成技能能复用、或要启用/卸载某个技能前用。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: Value) -> AppResult<ToolResult> {
        let skills = manager().await.list_skills().await;
        Ok(ToolResult::ok(json!({
            "count": skills.len(),
            "skills": skills.iter().map(skill_json).collect::<Vec<_>>(),
        })))
    }
}

// ============================================================
// skill_get
// ============================================================

pub struct SkillGetTool;

#[async_trait]
impl AgentTool for SkillGetTool {
    fn name(&self) -> &'static str {
        "skill_get"
    }

    fn description(&self) -> &'static str {
        "按 id 查单个技能的详情（参数、权限、触发词、入口脚本）。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id": { "type": "string" } },
            "required": ["id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let id = require_str(&args, "id")?;
        let skill = manager().await.get_skill(id).await
            .ok_or_else(|| AppError::ToolNotFound(format!("技能不存在: {}", id)))?;
        Ok(ToolResult::ok(json!({ "skill": skill_json(&skill) })))
    }
}

// ============================================================
// skill_toggle
// ============================================================

pub struct SkillToggleTool;

#[async_trait]
impl AgentTool for SkillToggleTool {
    fn name(&self) -> &'static str {
        "skill_toggle"
    }

    fn description(&self) -> &'static str {
        "启用或禁用某个已安装技能。禁用后该技能不再作为工具被调用。只改本地技能状态。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "enabled": { "type": "boolean" }
            },
            "required": ["id", "enabled"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Low
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let id = require_str(&args, "id")?;
        let enabled = args["enabled"].as_bool().unwrap_or(false);
        manager().await.toggle_skill(id, enabled).await?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "id": id,
            "enabled": enabled,
        })))
    }
}

// ============================================================
// skill_install
// ============================================================

pub struct SkillInstallTool;

#[async_trait]
impl AgentTool for SkillInstallTool {
    fn name(&self) -> &'static str {
        "skill_install"
    }

    fn description(&self) -> &'static str {
        "从本地目录安装一个技能（目录须含 skill.json + 入口脚本）。复制到本地技能目录并加载。安装会引入新代码，须先确认。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "directory": { "type": "string", "description": "技能源码目录绝对路径" }
            },
            "required": ["directory"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let dir = require_str(&args, "directory")?;
        let skill = manager().await.install_skill(dir).await?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "installed",
            "skill": skill_json(&skill),
        })))
    }
}

// ============================================================
// skill_uninstall
// ============================================================

pub struct SkillUninstallTool;

#[async_trait]
impl AgentTool for SkillUninstallTool {
    fn name(&self) -> &'static str {
        "skill_uninstall"
    }

    fn description(&self) -> &'static str {
        "卸载一个本地技能（内置技能不可卸载）。删除其本地目录并从注册表移除。"
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
        manager().await.uninstall_skill(id).await?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "uninstalled",
            "id": id,
        })))
    }
}

// ============================================================
// skill_marketplace_search
// ============================================================

pub struct SkillMarketplaceSearchTool;

#[async_trait]
impl AgentTool for SkillMarketplaceSearchTool {
    fn name(&self) -> &'static str {
        "skill_marketplace_search"
    }

    fn description(&self) -> &'static str {
        "搜索内置官方技能市场（Skill Hall）。返回可安装的官方技能目录；先搜索再安装，避免重复造轮子。空查询返回全部。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "能力/关键词；空则列全部" }
            }
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let query = args["query"].as_str().unwrap_or("");
        let results = skills::marketplace::search(query);
        Ok(ToolResult::ok(json!({
            "query": query,
            "count": results.len(),
            "skills": results.iter().map(|s| json!({
                "id": s.id,
                "name": s.name,
                "description": s.description,
                "category": s.category,
                "author": s.author,
                "version": s.version,
                "installable": s.installable,
            })).collect::<Vec<_>>(),
        })))
    }
}

// ============================================================
// skill_marketplace_install
// ============================================================

pub struct SkillMarketplaceInstallTool;

#[async_trait]
impl AgentTool for SkillMarketplaceInstallTool {
    fn name(&self) -> &'static str {
        "skill_marketplace_install"
    }

    fn description(&self) -> &'static str {
        "安装一个官方市场技能（skillId 来自 skill_marketplace_search）。把随应用打包的、经审阅的 skill.json + 入口脚本物化到本地技能目录。只安装官方目录里存在的技能，不接受任意目录。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "skill_id": { "type": "string" } },
            "required": ["skill_id"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let skill_id = require_str(&args, "skill_id")?;
        let (def, script) = skills::marketplace::bundled_package(skill_id)
            .ok_or_else(|| AppError::InvalidArgument(format!(
                "官方市场里没有技能「{}」。先用 skill_marketplace_search 查询可安装的技能。",
                skill_id
            )))?;
        let skill = manager().await.install_bundled_skill(def, script).await?;
        Ok(ToolResult::ok(json!({
            "ok": true,
            "status": "installed",
            "skill_id": skill_id,
            "skill": skill_json(&skill),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_json_carries_parameters_and_permissions() {
        let def = SkillDefinition {
            id: "x".into(),
            name: "X".into(),
            description: "d".into(),
            version: "1.0.0".into(),
            author: "a".into(),
            skill_type: skills::SkillType::Local,
            triggers: vec!["t".into()],
            parameters: vec![skills::SkillParameter {
                name: "p".into(),
                description: "pd".into(),
                required: true,
                param_type: "string".into(),
                default: None,
            }],
            entry_point: "run.ps1".into(),
            permissions: vec!["read".into()],
            enabled: true,
            installed_at: String::new(),
            skill_dir: std::path::PathBuf::new(),
        };
        let j = skill_json(&def);
        assert_eq!(j["id"], "x");
        assert_eq!(j["parameters"][0]["name"], "p");
        assert_eq!(j["permissions"][0], "read");
    }

    #[test]
    fn require_str_rejects_blank() {
        assert!(require_str(&json!({"id": "  "}), "id").is_err());
        assert_eq!(require_str(&json!({"id": "abc"}), "id").unwrap(), "abc");
    }
}
