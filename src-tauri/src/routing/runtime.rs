use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceRuntimeState {
    #[default]
    Idle,
    JarvisStarting,
    JarvisActive,
    Error,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSnapshot {
    pub state: InferenceRuntimeState,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerceptionObservation {
    pub id: String,
    pub observed_at: chrono::DateTime<chrono::Utc>,
    pub description: String,
}

#[derive(Clone, Default)]
pub struct InferenceRuntimeManager {
    snapshot: Arc<Mutex<RuntimeSnapshot>>,
    model_access: Arc<RwLock<()>>,
    latest_perception: Arc<Mutex<Option<PerceptionObservation>>>,
}

impl InferenceRuntimeManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&self) -> RuntimeSnapshot {
        self.snapshot.lock().await.clone()
    }

    pub async fn latest_perception(&self) -> Option<PerceptionObservation> {
        self.latest_perception.lock().await.clone()
    }

    pub async fn record_perception(&self, observation: PerceptionObservation) {
        let mut current = self.latest_perception.lock().await;
        if current.as_ref().map_or(true, |previous| {
            previous.observed_at <= observation.observed_at
        }) {
            *current = Some(observation);
        }
    }

    pub async fn acquire_agent_access(&self) -> Result<OwnedRwLockReadGuard<()>, String> {
        let access = self
            .model_access
            .clone()
            .try_read_owned()
            .map_err(|_| "Jarvis 正在切换感知运行时，请稍后重试".to_string())?;
        if matches!(
            self.snapshot().await.state,
            InferenceRuntimeState::JarvisStarting | InferenceRuntimeState::JarvisActive
        ) {
            return Err("感知运行时占用中，请停止实时感知后重试".to_string());
        }
        Ok(access)
    }

    pub fn acquire_perception_access(&self) -> Result<OwnedRwLockWriteGuard<()>, String> {
        self.model_access
            .clone()
            .try_write_owned()
            .map_err(|_| "助手正在处理模型任务，请等待任务结束后再切换实时感知".to_string())
    }

    pub async fn begin_jarvis_start(&self) -> Result<(), String> {
        let mut snapshot = self.snapshot.lock().await;
        if !matches!(
            snapshot.state,
            InferenceRuntimeState::Idle | InferenceRuntimeState::Error
        ) {
            return Err(format!("感知运行时仍为 {:?}，请先停止", snapshot.state));
        }
        snapshot.state = InferenceRuntimeState::JarvisStarting;
        snapshot.detail = None;
        Ok(())
    }

    pub async fn adopt_jarvis(&self) {
        *self.snapshot.lock().await = RuntimeSnapshot {
            state: InferenceRuntimeState::JarvisActive,
            detail: Some("检测到已运行的感知服务".into()),
        };
    }

    pub async fn mark_jarvis_active(&self) -> Result<(), String> {
        let mut snapshot = self.snapshot.lock().await;
        if snapshot.state != InferenceRuntimeState::JarvisStarting {
            return Err("感知服务没有处于启动状态".into());
        }
        snapshot.state = InferenceRuntimeState::JarvisActive;
        snapshot.detail = None;
        Ok(())
    }

    pub async fn mark_error(&self, detail: impl Into<String>) {
        *self.snapshot.lock().await = RuntimeSnapshot {
            state: InferenceRuntimeState::Error,
            detail: Some(detail.into()),
        };
    }

    pub async fn force_idle(&self) {
        *self.snapshot.lock().await = RuntimeSnapshot::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn perception_and_agent_admission_are_mutually_exclusive() {
        let manager = InferenceRuntimeManager::new();
        let agent = manager.acquire_agent_access().await.unwrap();
        assert!(manager.acquire_agent_access().await.is_ok());
        assert!(manager.acquire_perception_access().is_err());
        drop(agent);
        let perception = manager.acquire_perception_access().unwrap();
        assert!(manager.acquire_agent_access().await.is_err());
        manager.begin_jarvis_start().await.unwrap();
        manager.mark_jarvis_active().await.unwrap();
        drop(perception);
        assert!(manager.acquire_agent_access().await.is_err());
        manager.force_idle().await;
        assert!(manager.acquire_agent_access().await.is_ok());
    }
}
