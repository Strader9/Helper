//! 协调主模型任务与按需启动的 Jarvis 感知运行时。

mod runtime;

pub use runtime::{
    InferenceRuntimeManager, InferenceRuntimeState, PerceptionObservation, RuntimeSnapshot,
};
