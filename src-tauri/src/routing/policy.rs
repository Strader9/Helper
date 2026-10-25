use serde::{Deserialize, Serialize};

/// 参与任务理解或规划的模型角色。实际执行始终由 Rust 安全内核负责。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    Jarvis,
    Ollama,
    System,
}

/// 用户确认过归属的顶层能力。保持业务语言，避免将前端路由绑定到工具文件名。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Conversation,
    ScreenUnderstanding,
    ProactiveMonitoring,
    VoiceInteraction,
    FileWork,
    DesktopGui,
    BrowserWork,
    DocumentWork,
    MemoryKnowledge,
    WorkflowAutomation,
    WebStudio,
    CodeGit,
    SystemOperations,
    MessagingCalendar,
    MediaWork,
    CadIndustry,
    McpExternalAi,
    SafetyExecution,
}

/// 当前能力的职责路由。`Handoff` 表示串行交接，绝不表示两个模型同时驻留。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RouteDecision {
    Direct { owner: ModelRole },
    Handoff { front: ModelRole, worker: ModelRole },
}

/// 第一版固定策略，严格反映用户确认的 18 项职责选择。
#[derive(Debug, Clone, Default)]
pub struct RoutingPolicy;

impl RoutingPolicy {
    pub fn decide(&self, capability: Capability) -> RouteDecision {
        use Capability::*;
        use ModelRole::*;

        match capability {
            Conversation | ScreenUnderstanding | ProactiveMonitoring | VoiceInteraction
            | DesktopGui | MemoryKnowledge | MessagingCalendar | MediaWork | CadIndustry
            | McpExternalAi => RouteDecision::Direct { owner: Jarvis },
            FileWork | WebStudio | CodeGit => RouteDecision::Direct { owner: Ollama },
            BrowserWork | DocumentWork | WorkflowAutomation | SystemOperations => {
                RouteDecision::Handoff {
                    front: Jarvis,
                    worker: Ollama,
                }
            }
            SafetyExecution => RouteDecision::Direct { owner: System },
        }
    }

    pub fn all() -> Vec<(Capability, RouteDecision)> {
        const CAPABILITIES: &[Capability] = &[
            Capability::Conversation,
            Capability::ScreenUnderstanding,
            Capability::ProactiveMonitoring,
            Capability::VoiceInteraction,
            Capability::FileWork,
            Capability::DesktopGui,
            Capability::BrowserWork,
            Capability::DocumentWork,
            Capability::MemoryKnowledge,
            Capability::WorkflowAutomation,
            Capability::WebStudio,
            Capability::CodeGit,
            Capability::SystemOperations,
            Capability::MessagingCalendar,
            Capability::MediaWork,
            Capability::CadIndustry,
            Capability::McpExternalAi,
            Capability::SafetyExecution,
        ];
        let policy = Self;
        CAPABILITIES
            .iter()
            .map(|capability| (*capability, policy.decide(*capability)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_matches_user_confirmed_responsibilities() {
        let policy = RoutingPolicy;
        assert_eq!(
            policy.decide(Capability::FileWork),
            RouteDecision::Direct {
                owner: ModelRole::Ollama
            }
        );
        assert_eq!(
            policy.decide(Capability::BrowserWork),
            RouteDecision::Handoff {
                front: ModelRole::Jarvis,
                worker: ModelRole::Ollama
            }
        );
        assert_eq!(
            policy.decide(Capability::SafetyExecution),
            RouteDecision::Direct {
                owner: ModelRole::System
            }
        );
    }

    #[test]
    fn policy_declares_every_capability_once() {
        assert_eq!(RoutingPolicy::all().len(), 18);
    }
}
