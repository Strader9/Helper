use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::ModelRole;

/// GPU 推理运行时的唯一真相来源。只有状态转换完成，调用方才能启动另一模型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceRuntimeState {
    Idle,
    JarvisStarting,
    JarvisActive,
    JarvisStopping,
    OllamaStarting,
    OllamaActive,
    OllamaUnloading,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub state: InferenceRuntimeState,
    pub active_role: Option<ModelRole>,
    pub detail: Option<String>,
}

impl Default for RuntimeSnapshot {
    fn default() -> Self {
        Self {
            state: InferenceRuntimeState::Idle,
            active_role: None,
            detail: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct InferenceRuntimeManager {
    snapshot: Arc<Mutex<RuntimeSnapshot>>,
}

impl InferenceRuntimeManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&self) -> RuntimeSnapshot {
        self.snapshot.lock().await.clone()
    }

    pub async fn begin_start(&self, role: ModelRole) -> Result<(), String> {
        if role == ModelRole::System {
            return Err("系统安全内核不占用模型运行时".to_string());
        }
        let mut snapshot = self.snapshot.lock().await;
        if snapshot.active_role.is_some()
            || !matches!(
                snapshot.state,
                InferenceRuntimeState::Idle | InferenceRuntimeState::Error
            )
        {
            return Err(format!("推理运行时忙碌中：{:?}", snapshot.state));
        }
        snapshot.state = match role {
            ModelRole::Jarvis => InferenceRuntimeState::JarvisStarting,
            ModelRole::Ollama => InferenceRuntimeState::OllamaStarting,
            ModelRole::System => unreachable!(),
        };
        snapshot.detail = None;
        Ok(())
    }

    /// 对应用启动前已存在的外部模型进程建立状态认知。只能从空闲状态接管，
    /// 防止外部 Jarvis 覆盖本应用已经记录的 Ollama 占用。
    pub async fn adopt_active(&self, role: ModelRole) -> Result<(), String> {
        if role == ModelRole::System {
            return Err("系统安全内核不占用模型运行时".to_string());
        }
        let mut snapshot = self.snapshot.lock().await;
        if snapshot.active_role.is_some() && snapshot.active_role != Some(role) {
            return Err(format!(
                "不能接管 {:?}：当前已由 {:?} 占用",
                role, snapshot.active_role
            ));
        }
        snapshot.state = match role {
            ModelRole::Jarvis => InferenceRuntimeState::JarvisActive,
            ModelRole::Ollama => InferenceRuntimeState::OllamaActive,
            ModelRole::System => unreachable!(),
        };
        snapshot.active_role = Some(role);
        snapshot.detail = Some("检测到应用外已运行的模型进程，已建立只读状态认知".to_string());
        Ok(())
    }

    pub async fn mark_active(&self, role: ModelRole) -> Result<(), String> {
        let mut snapshot = self.snapshot.lock().await;
        let expected = match role {
            ModelRole::Jarvis => InferenceRuntimeState::JarvisStarting,
            ModelRole::Ollama => InferenceRuntimeState::OllamaStarting,
            ModelRole::System => return Err("系统安全内核不占用模型运行时".to_string()),
        };
        if snapshot.state != expected {
            return Err(format!(
                "不能从 {:?} 标记 {:?} 为活动",
                snapshot.state, role
            ));
        }
        snapshot.state = match role {
            ModelRole::Jarvis => InferenceRuntimeState::JarvisActive,
            ModelRole::Ollama => InferenceRuntimeState::OllamaActive,
            ModelRole::System => unreachable!(),
        };
        snapshot.active_role = Some(role);
        Ok(())
    }

    pub async fn begin_release(&self, role: ModelRole) -> Result<(), String> {
        let mut snapshot = self.snapshot.lock().await;
        if snapshot.active_role != Some(role) {
            return Err(format!("当前活动模型不是 {:?}", role));
        }
        snapshot.state = match role {
            ModelRole::Jarvis => InferenceRuntimeState::JarvisStopping,
            ModelRole::Ollama => InferenceRuntimeState::OllamaUnloading,
            ModelRole::System => return Err("系统安全内核不占用模型运行时".to_string()),
        };
        Ok(())
    }

    pub async fn mark_idle(&self) -> Result<(), String> {
        let mut snapshot = self.snapshot.lock().await;
        if !matches!(
            snapshot.state,
            InferenceRuntimeState::JarvisStopping | InferenceRuntimeState::OllamaUnloading
        ) {
            return Err(format!("不能从 {:?} 标记为空闲", snapshot.state));
        }
        *snapshot = RuntimeSnapshot::default();
        Ok(())
    }

    pub async fn mark_error(&self, detail: impl Into<String>) {
        let mut snapshot = self.snapshot.lock().await;
        *snapshot = RuntimeSnapshot {
            state: InferenceRuntimeState::Error,
            active_role: None,
            detail: Some(detail.into()),
        };
    }

    /// 仅在已确认本应用托管的模型进程结束后调用。
    pub async fn force_idle(&self) {
        *self.snapshot.lock().await = RuntimeSnapshot::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn models_are_mutually_exclusive() {
        let manager = InferenceRuntimeManager::new();
        manager.begin_start(ModelRole::Jarvis).await.unwrap();
        manager.mark_active(ModelRole::Jarvis).await.unwrap();
        assert!(manager.begin_start(ModelRole::Ollama).await.is_err());
        manager.begin_release(ModelRole::Jarvis).await.unwrap();
        manager.mark_idle().await.unwrap();
        manager.begin_start(ModelRole::Ollama).await.unwrap();
    }

    #[tokio::test]
    async fn invalid_transition_is_rejected() {
        let manager = InferenceRuntimeManager::new();
        assert!(manager.mark_active(ModelRole::Ollama).await.is_err());
        assert!(manager.begin_release(ModelRole::Jarvis).await.is_err());
    }

    #[tokio::test]
    async fn cannot_adopt_a_second_model() {
        let manager = InferenceRuntimeManager::new();
        manager.adopt_active(ModelRole::Jarvis).await.unwrap();
        assert!(manager.adopt_active(ModelRole::Ollama).await.is_err());
        manager.force_idle().await;
        assert_eq!(manager.snapshot().await.state, InferenceRuntimeState::Idle);
    }
}
