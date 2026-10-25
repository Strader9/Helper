use serde::{Deserialize, Serialize};

use super::{Capability, ModelRole};

/// 交接中允许引用的资源。凭据、确认令牌与原始长期屏幕录像不能进入交接包。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceRef {
    Path { path: String },
    Url { url: String },
    Observation { summary: String },
    ToolReceipt { receipt_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffPacket {
    pub handoff_id: String,
    pub task_id: String,
    pub from: ModelRole,
    pub to: ModelRole,
    pub capability: Capability,
    pub user_goal: String,
    pub conversation_summary: String,
    pub resources: Vec<ResourceRef>,
    pub requires_confirmation_for_write: bool,
}

impl HandoffPacket {
    pub fn validate(&self) -> Result<(), String> {
        if self.from == self.to || self.from == ModelRole::System || self.to == ModelRole::System {
            return Err("交接只能发生在 Jarvis 与 Ollama 之间".to_string());
        }
        if self.task_id.trim().is_empty()
            || self.handoff_id.trim().is_empty()
            || self.user_goal.trim().is_empty()
        {
            return Err("交接包缺少任务、交接编号或用户目标".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffResult {
    pub task_id: String,
    pub status: String,
    pub summary: String,
    pub warnings: Vec<String>,
    pub receipt_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_requires_real_cross_model_handoff() {
        let packet = HandoffPacket {
            handoff_id: "handoff-1".into(),
            task_id: "task-1".into(),
            from: ModelRole::Jarvis,
            to: ModelRole::Ollama,
            capability: Capability::DocumentWork,
            user_goal: "整理文档".into(),
            conversation_summary: "用户要求生成摘要".into(),
            resources: vec![],
            requires_confirmation_for_write: true,
        };
        assert!(packet.validate().is_ok());
    }
}
