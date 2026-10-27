//! 自治策略（P3）。
//!
//! 描述助手在「无人值守 / 自治」语义下的放行边界。当前执行层的无人值守由
//! `autonomous:` 会话前缀触发（见 scheduler/executor），本策略补充可配置的
//! 风险放行阈值与「始终要求确认」的工具名单，落库在 settings 的 `autonomy.policy`。

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

const POLICY_KEY: &str = "autonomy.policy";
const VALID_RISKS: &[&str] = &["SAFE", "LOW", "MEDIUM", "HIGH", "CRITICAL"];

fn db() -> AppResult<crate::db::Database> {
    let path = crate::memory::get_db_path()
        .ok_or_else(|| AppError::Internal("数据库未初始化".to_string()))?;
    crate::db::Database::new(path)
}

fn default_policy() -> Value {
    json!({
        "enabled": false,
        "auto_confirm_risk": ["SAFE", "LOW"],
        "always_confirm_tools": [],
        "max_unattended_iterations": 10,
    })
}

fn read_policy() -> AppResult<Value> {
    let db = db()?;
    let raw = db.get_setting(POLICY_KEY)?;
    let mut policy = raw
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(default_policy);
    // 逐字段补默认，避免旧配置缺字段时误判
    let d = default_policy();
    if policy.get("enabled").is_none() {
        policy["enabled"] = d["enabled"].clone();
    }
    if policy.get("auto_confirm_risk").is_none() {
        policy["auto_confirm_risk"] = d["auto_confirm_risk"].clone();
    }
    if policy.get("always_confirm_tools").is_none() {
        policy["always_confirm_tools"] = d["always_confirm_tools"].clone();
    }
    if policy.get("max_unattended_iterations").is_none() {
        policy["max_unattended_iterations"] = d["max_unattended_iterations"].clone();
    }
    Ok(policy)
}

pub struct AutonomyGetPolicyTool;

#[async_trait]
impl AgentTool for AutonomyGetPolicyTool {
    fn name(&self) -> &'static str {
        "autonomy_get_policy"
    }

    fn description(&self) -> &'static str {
        "读取自治策略：是否启用自治、自治下自动放行的风险等级、始终要求确认的工具名单、无人值守最大轮次。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: serde_json::Value) -> AppResult<ToolResult> {
        let policy = read_policy()?;
        Ok(ToolResult::ok(json!({ "policy": policy })))
    }
}

pub struct AutonomyUpdatePolicyTool;

#[async_trait]
impl AgentTool for AutonomyUpdatePolicyTool {
    fn name(&self) -> &'static str {
        "autonomy_update_policy"
    }

    fn description(&self) -> &'static str {
        "更新自治策略（只更新传入的字段，风险等级须在 SAFE/LOW/MEDIUM/HIGH/CRITICAL 内）。"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "enabled": { "type": "boolean", "description": "是否启用自治" },
                "auto_confirm_risk": { "type": "array", "items": { "type": "string" }, "description": "自治下自动放行的风险等级" },
                "always_confirm_tools": { "type": "array", "items": { "type": "string" }, "description": "始终要求确认的工具名" },
                "max_unattended_iterations": { "type": "integer", "description": "无人值守最大轮次（1-50）" }
            },
            "additionalProperties": false
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: serde_json::Value) -> AppResult<ToolResult> {
        let mut policy = read_policy()?;
        let mut changed = false;

        if let Some(v) = args.get("enabled").and_then(|v| v.as_bool()) {
            policy["enabled"] = json!(v);
            changed = true;
        }
        if let Some(arr) = args.get("auto_confirm_risk").and_then(|v| v.as_array()) {
            let risks: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if risks.iter().any(|r| !VALID_RISKS.contains(&r.as_str())) {
                return Ok(ToolResult::err(
                    "INVALID_RISK",
                    &format!("风险等级只能取 {}", VALID_RISKS.join("/")),
                ));
            }
            policy["auto_confirm_risk"] = json!(risks);
            changed = true;
        }
        if let Some(arr) = args.get("always_confirm_tools").and_then(|v| v.as_array()) {
            let tools: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            policy["always_confirm_tools"] = json!(tools);
            changed = true;
        }
        if let Some(v) = args
            .get("max_unattended_iterations")
            .and_then(|v| v.as_i64())
        {
            let v = v.clamp(1, 50);
            policy["max_unattended_iterations"] = json!(v);
            changed = true;
        }

        if !changed {
            return Ok(ToolResult::err("NO_FIELDS", "没有提供任何要更新的字段"));
        }

        let db = db()?;
        db.connection().execute(
            "INSERT INTO settings (key, value, value_type) VALUES (?1, ?2, 'string')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
            rusqlite::params![POLICY_KEY, policy.to_string()],
        )?;

        Ok(ToolResult::ok(json!({ "ok": true, "policy": policy })))
    }
}
