//! 主动规则对话工具。
//!
//! 独立“自动化页面”取消后，用户通过自然语言创建和管理监控规则。
//! 规则仍由 ProactiveEngine 执行，工具只负责经过权限闸门的本地配置变更。

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{AppError, AppResult};
use crate::proactive::{
    self, ActionType, EventTrigger, ExecuteToolAction, NotifyAction, OpenAppAction, ProactiveRule,
    ProcessTrigger, ScheduleTrigger, SystemMetricTrigger, TriggerType,
};
use crate::tools::{AgentTool, RiskLevel, ToolResult};

fn open_db() -> AppResult<rusqlite::Connection> {
    crate::memory::open_connection()
        .ok_or_else(|| AppError::Internal("数据库尚未初始化".to_string()))
}

fn find_rule(conn: &rusqlite::Connection, key: &str) -> Result<Option<ProactiveRule>, String> {
    Ok(proactive::get_rules(conn)?
        .into_iter()
        .find(|rule| rule.id == key || rule.name == key))
}

fn validate_trigger(kind: &str, config: &Value) -> AppResult<()> {
    match TriggerType::from_str(kind) {
        TriggerType::SystemMetric => {
            let value: SystemMetricTrigger =
                serde_json::from_value(config.clone()).map_err(|error| {
                    AppError::InvalidArgument(format!("系统指标触发配置无效：{error}"))
                })?;
            if !matches!(value.metric.as_str(), "cpu" | "memory" | "disk") {
                return Err(AppError::InvalidArgument(
                    "metric 只支持 cpu / memory / disk".to_string(),
                ));
            }
            if !matches!(value.operator.as_str(), "gt" | "lt" | "gte" | "lte") {
                return Err(AppError::InvalidArgument(
                    "operator 只支持 gt / lt / gte / lte".to_string(),
                ));
            }
        }
        TriggerType::Schedule => {
            let value: ScheduleTrigger = serde_json::from_value(config.clone())
                .map_err(|error| AppError::InvalidArgument(format!("日程触发配置无效：{error}")))?;
            let valid_time = value.time.split_once(':').is_some_and(|(hour, minute)| {
                hour.len() == 2
                    && minute.len() == 2
                    && hour.parse::<u8>().is_ok_and(|value| value < 24)
                    && minute.parse::<u8>().is_ok_and(|value| value < 60)
            });
            if !valid_time || !matches!(value.repeat.as_str(), "daily" | "weekdays" | "once") {
                return Err(AppError::InvalidArgument(
                    "日程规则需要 HH:MM 时间，repeat 为 daily / weekdays / once".to_string(),
                ));
            }
        }
        TriggerType::Event => {
            let value: EventTrigger = serde_json::from_value(config.clone())
                .map_err(|error| AppError::InvalidArgument(format!("事件触发配置无效：{error}")))?;
            if !matches!(
                value.event.as_str(),
                "app_start" | "user_active" | "disk_insert"
            ) {
                return Err(AppError::InvalidArgument(
                    "event 只支持 app_start / user_active / disk_insert".to_string(),
                ));
            }
        }
        TriggerType::Process => {
            let value: ProcessTrigger = serde_json::from_value(config.clone())
                .map_err(|error| AppError::InvalidArgument(format!("进程触发配置无效：{error}")))?;
            if !matches!(value.mode.as_str(), "new" | "specific" | "unknown") {
                return Err(AppError::InvalidArgument(
                    "mode 只支持 new / specific / unknown".to_string(),
                ));
            }
            if value.mode == "specific"
                && value
                    .target
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
            {
                return Err(AppError::InvalidArgument(
                    "specific 模式必须提供 target".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_action(kind: &str, config: &Value) -> AppResult<()> {
    match ActionType::from_str(kind) {
        ActionType::Notify => {
            serde_json::from_value::<NotifyAction>(config.clone())
                .map_err(|error| AppError::InvalidArgument(format!("通知动作配置无效：{error}")))?;
        }
        ActionType::ExecuteTool => {
            serde_json::from_value::<ExecuteToolAction>(config.clone())
                .map_err(|error| AppError::InvalidArgument(format!("工具动作配置无效：{error}")))?;
        }
        ActionType::OpenApp => {
            serde_json::from_value::<OpenAppAction>(config.clone()).map_err(|error| {
                AppError::InvalidArgument(format!("打开应用动作配置无效：{error}"))
            })?;
        }
    }
    Ok(())
}

pub struct CreateProactiveRuleTool;

#[async_trait]
impl AgentTool for CreateProactiveRuleTool {
    fn name(&self) -> &'static str {
        "create_proactive_rule"
    }

    fn description(&self) -> &'static str {
        "创建本机主动监控规则。适合‘CPU 超过 90% 就通知我’、‘发现陌生进程就提醒我’等条件触发需求。普通的每天/每隔多久执行一个目标应使用 schedule_job。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "简短、唯一的规则名" },
                "trigger_type": { "type": "string", "enum": ["system_metric", "schedule", "event", "process"] },
                "trigger_config": {
                    "type": "object",
                    "description": "system_metric: {metric,threshold,operator,duration_sec?,drive?}; schedule: {time:'HH:MM',repeat:'daily|weekdays|once'}; event: {event:'app_start|user_active|disk_insert'}; process: {mode:'new|specific|unknown',target?,whitelist?}"
                },
                "action_type": { "type": "string", "enum": ["notify", "execute_tool", "open_app"] },
                "action_config": {
                    "type": "object",
                    "description": "notify: {title,message,level?}; execute_tool: {tool_name,arguments}; open_app: {app_name}"
                }
            },
            "required": ["name", "trigger_type", "trigger_config", "action_type", "action_config"]
        })
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let name = args["name"]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("name is required".to_string()))?;
        let trigger_type = args["trigger_type"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("trigger_type is required".to_string()))?;
        let action_type = args["action_type"]
            .as_str()
            .ok_or_else(|| AppError::InvalidArgument("action_type is required".to_string()))?;
        if !matches!(
            trigger_type,
            "system_metric" | "schedule" | "event" | "process"
        ) {
            return Err(AppError::InvalidArgument("未知 trigger_type".to_string()));
        }
        if !matches!(action_type, "notify" | "execute_tool" | "open_app") {
            return Err(AppError::InvalidArgument("未知 action_type".to_string()));
        }
        let trigger_config = args.get("trigger_config").cloned().unwrap_or(Value::Null);
        let action_config = args.get("action_config").cloned().unwrap_or(Value::Null);
        validate_trigger(trigger_type, &trigger_config)?;
        validate_action(action_type, &action_config)?;

        let conn = open_db()?;
        if let Some(existing) = find_rule(&conn, name).map_err(AppError::Internal)? {
            return Ok(ToolResult::ok(json!({
                "created": false,
                "reason": "已有同名规则，未重复创建",
                "id": existing.id,
                "name": existing.name,
                "enabled": existing.enabled,
            })));
        }
        let rule = ProactiveRule {
            id: format!("rule_{}", uuid::Uuid::new_v4().simple()),
            name: name.to_string(),
            trigger_type: trigger_type.to_string(),
            trigger_config: serde_json::to_string(&trigger_config)?,
            action_type: action_type.to_string(),
            action_config: serde_json::to_string(&action_config)?,
            enabled: true,
            last_triggered: None,
            trigger_count: 0,
            created_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        };
        proactive::insert_rule(&conn, &rule).map_err(AppError::Internal)?;
        Ok(ToolResult::ok(json!({
            "created": true,
            "id": rule.id,
            "name": rule.name,
            "enabled": true,
            "trigger_type": rule.trigger_type,
            "action_type": rule.action_type,
        })))
    }
}

pub struct ListProactiveRulesTool;

#[async_trait]
impl AgentTool for ListProactiveRulesTool {
    fn name(&self) -> &'static str {
        "list_proactive_rules"
    }
    fn description(&self) -> &'static str {
        "列出本机所有主动监控规则、启用状态与触发次数。"
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Safe
    }

    async fn execute(&self, _args: Value) -> AppResult<ToolResult> {
        let conn = open_db()?;
        let rules = proactive::get_rules(&conn).map_err(AppError::Internal)?;
        Ok(ToolResult::ok(json!({
            "count": rules.len(),
            "enabled": rules.iter().filter(|rule| rule.enabled).count(),
            "rules": rules,
        })))
    }
}

pub struct SetProactiveRuleEnabledTool;

#[async_trait]
impl AgentTool for SetProactiveRuleEnabledTool {
    fn name(&self) -> &'static str {
        "set_proactive_rule_enabled"
    }
    fn description(&self) -> &'static str {
        "暂停或恢复一条主动监控规则，保留它的配置与触发统计。"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id_or_name": { "type": "string", "description": "规则 id 或名称" },
                "enabled": { "type": "boolean" }
            },
            "required": ["id_or_name", "enabled"]
        })
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let key = args["id_or_name"]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("id_or_name is required".to_string()))?;
        let enabled = args["enabled"]
            .as_bool()
            .ok_or_else(|| AppError::InvalidArgument("enabled is required".to_string()))?;
        let conn = open_db()?;
        let rule = find_rule(&conn, key)
            .map_err(AppError::Internal)?
            .ok_or_else(|| AppError::InvalidArgument(format!("没有找到规则「{key}」")))?;
        proactive::toggle_rule(&conn, &rule.id, enabled).map_err(AppError::Internal)?;
        Ok(ToolResult::ok(
            json!({ "id": rule.id, "name": rule.name, "enabled": enabled }),
        ))
    }
}

pub struct DeleteProactiveRuleTool;

#[async_trait]
impl AgentTool for DeleteProactiveRuleTool {
    fn name(&self) -> &'static str {
        "delete_proactive_rule"
    }
    fn description(&self) -> &'static str {
        "永久删除一条主动监控规则。只是暂时停用时应使用 set_proactive_rule_enabled。"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "id_or_name": { "type": "string", "description": "规则 id 或名称" } },
            "required": ["id_or_name"]
        })
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::Medium
    }

    async fn execute(&self, args: Value) -> AppResult<ToolResult> {
        let key = args["id_or_name"]
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AppError::InvalidArgument("id_or_name is required".to_string()))?;
        let conn = open_db()?;
        let rule = find_rule(&conn, key)
            .map_err(AppError::Internal)?
            .ok_or_else(|| AppError::InvalidArgument(format!("没有找到规则「{key}」")))?;
        proactive::delete_rule(&conn, &rule.id).map_err(AppError::Internal)?;
        Ok(ToolResult::ok(
            json!({ "deleted": true, "id": rule.id, "name": rule.name }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_common_metric_rule_shape() {
        assert!(validate_trigger(
            "system_metric",
            &json!({
                "metric": "cpu", "threshold": 90, "operator": "gte", "duration_sec": 30
            })
        )
        .is_ok());
        assert!(validate_action(
            "notify",
            &json!({
                "title": "CPU 告警", "message": "CPU 已持续超过 90%"
            })
        )
        .is_ok());
    }

    #[test]
    fn rejects_invalid_schedule_time() {
        assert!(validate_trigger(
            "schedule",
            &json!({
                "time": "25:70", "repeat": "daily"
            })
        )
        .is_err());
    }
}
