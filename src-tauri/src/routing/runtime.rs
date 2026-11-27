use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

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
    model_access: Arc<RwLock<()>>,
}

impl InferenceRuntimeManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&self) -> RuntimeSnapshot {
        self.snapshot.lock().await.clone()
    }

    pub async fn acquire_agent_access(&self) -> Result<OwnedRwLockReadGuard<()>, String> {
        let access = self.model_access.clone().try_read_owned().map_err(|_| {
            "Jarvis 正在切换感知运行时，请稍后重试".to_string()
        })?;
        if matches!(
            self.snapshot().await.state,
            InferenceRuntimeState::JarvisStarting
                | InferenceRuntimeState::JarvisActive
                | InferenceRuntimeState::JarvisStopping
                | InferenceRuntimeState::OllamaUnloading
        ) {
            return Err("感知运行时占用中，请停止实时感知后重试".to_string());
        }
        Ok(access)
    }

    pub fn acquire_perception_access(&self) -> Result<OwnedRwLockWriteGuard<()>, String> {
        self.model_access.clone().try_write_owned().map_err(|_| {
            "助手正在处理模型任务，请等待任务结束后再启动实时感知".to_string()
        })
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
            // 同一角色可能已被并发路径接管（例如恢复流程等待健康检查期间，
            // 探测线程先一步 adopt_active 把它写成了 Active）。此时进程本就就绪，
            // 拒绝这次标记只会让上层误判为恢复失败，故按幂等成功处理。
            let already_active = snapshot.active_role == Some(role)
                && matches!(
                    snapshot.state,
                    InferenceRuntimeState::JarvisActive | InferenceRuntimeState::OllamaActive
                );
            if already_active {
                return Ok(());
            }
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
    async fn agent_and_perception_admission_are_mutually_exclusive() {
        let manager = InferenceRuntimeManager::new();
        let agent = manager.acquire_agent_access().await.unwrap();
        assert!(manager.acquire_agent_access().await.is_ok());
        assert!(manager.acquire_perception_access().is_err());
        drop(agent);

        let perception = manager.acquire_perception_access().unwrap();
        assert!(manager.acquire_agent_access().await.is_err());
        manager.begin_start(ModelRole::Jarvis).await.unwrap();
        manager.mark_active(ModelRole::Jarvis).await.unwrap();
        drop(perception);
        assert!(manager.acquire_agent_access().await.is_err());
    }

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
