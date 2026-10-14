//! Ollama 客户端
//!
//! 封装 Ollama HTTP API：
//! - POST /api/chat — 流式对话
//! - GET /api/tags — 获取本地模型列表
//!
//! 消息格式使用 OpenAI 兼容格式，支持 tool_calls。

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, Stream, StreamExt};

use crate::error::{AppError, AppResult};

/// Ollama 聊天消息
///
/// 遵循 OpenAI 兼容格式，支持 system/user/assistant/tool 角色。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl OllamaMessage {
    pub fn system(content: &str) -> Self {
        Self {
            role: "system".to_string(),
            content: content.to_string(),
            tool_calls: None,
            name: None,
        }
    }

    pub fn user(content: &str) -> Self {
        Self {
            role: "user".to_string(),
            content: content.to_string(),
            tool_calls: None,
            name: None,
        }
    }

    pub fn assistant(content: &str) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.to_string(),
            tool_calls: None,
            name: None,
        }
    }

    pub fn assistant_with_tools(content: &str, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.to_string(),
            tool_calls: Some(tool_calls),
            name: None,
        }
    }

    pub fn tool(name: &str, content: &str) -> Self {
        Self {
            role: "tool".to_string(),
            content: content.to_string(),
            tool_calls: None,
            name: Some(name.to_string()),
        }
    }
}

/// Tool Call 定义
///
/// OpenAI 兼容格式，LLM 返回的工具调用指令。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(rename = "id", skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub call_type: Option<String>,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Ollama 模型信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaModel {
    pub name: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// 一次流式调用结束时的用量统计。
///
/// 为什么值得单独抽出来：本地 8B 模型跑在 8GB 显存上，"一次生成到底消耗了多少上下文"
/// 决定了页面能不能一次做完 —— `prompt_eval_count` 逼近 `num_ctx` 时模型就已经在丢前面的内容了，
/// 而这件事在日志里原本完全不可见，出了问题只能靠猜。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// 提示词占用的 token 数（Ollama 的 `prompt_eval_count`）。
    pub prompt_tokens: u64,
    /// 本轮实际生成的 token 数（Ollama 的 `eval_count`）。
    pub output_tokens: u64,
    /// 提示词处理耗时（纳秒）。
    pub prompt_nanos: u64,
    /// 生成耗时（纳秒）。
    pub eval_nanos: u64,
}

impl TokenUsage {
    /// 是否拿到了有效数据（Ollama 只在最后一个分片里给用量）。
    pub fn is_measured(&self) -> bool {
        self.prompt_tokens > 0 || self.output_tokens > 0
    }

    /// 一行的可读摘要，直接进日志。
    pub fn log_line(&self) -> String {
        let prompt_secs = self.prompt_nanos as f64 / 1_000_000_000.0;
        let eval_secs = self.eval_nanos as f64 / 1_000_000_000.0;
        let rate = if eval_secs > 0.01 {
            format!("{:.1} tok/s", self.output_tokens as f64 / eval_secs)
        } else {
            "—".to_string()
        };
        format!(
            "提示词 {} token（{:.1}s）｜输出 {} token（{:.1}s，{}）",
            self.prompt_tokens, prompt_secs, self.output_tokens, eval_secs, rate
        )
    }
}

/// 流式响应分片
///
/// 包含文本片段或完成标记。
#[derive(Debug, Clone)]
pub enum ChatChunk {
    /// 文本片段
    Text(String),
    /// 工具调用
    ToolCall(ToolCall),
    /// 流结束
    ///
    /// 第一个字段是 `done_reason`，第二个是本次调用的用量统计（拿不到时为 `None`）。
    Done(Option<String>, Option<TokenUsage>),
    /// 错误
    Error(String),
}

/// Ollama 聊天请求体
#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<serde_json::Value>,
    /// 让模型在内存中保持一段时间，降低连续指令的首 token 延迟。
    keep_alive: String,
    /// 桌面指令优先低延迟。Qwen3 等模型关闭长思维链后仍可正常工具调用。
    think: bool,
}

#[derive(Debug, Serialize)]
struct WarmupRequest {
    model: String,
    prompt: String,
    stream: bool,
    keep_alive: String,
}

/// Ollama 聊天流式响应体
#[derive(Debug, Deserialize)]
struct ChatResponse {
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    created_at: String,
    message: Option<OllamaMessage>,
    done: bool,
    #[allow(dead_code)]
    #[serde(skip_serializing_if = "Option::is_none")]
    done_reason: Option<String>,
    /// 用量字段只在最后一个分片（`done: true`）里出现，中间分片为 `None`。
    #[serde(default)]
    prompt_eval_count: Option<u64>,
    #[serde(default)]
    eval_count: Option<u64>,
    #[serde(default)]
    prompt_eval_duration: Option<u64>,
    #[serde(default)]
    eval_duration: Option<u64>,
}

impl ChatResponse {
    /// 从最后一个分片里取用量。任何字段缺失都按 0 处理 —— 用量只用于观测，不该影响生成。
    fn usage(&self) -> Option<TokenUsage> {
        let usage = TokenUsage {
            prompt_tokens: self.prompt_eval_count.unwrap_or(0),
            output_tokens: self.eval_count.unwrap_or(0),
            prompt_nanos: self.prompt_eval_duration.unwrap_or(0),
            eval_nanos: self.eval_duration.unwrap_or(0),
        };
        usage.is_measured().then_some(usage)
    }
}

/// Ollama 模型列表响应
#[derive(Debug, Deserialize)]
struct TagsResponse {
    models: Vec<OllamaModel>,
}

/// Ollama 客户端
///
/// 封装与 Ollama 本地服务的所有 HTTP 通信。
pub struct OllamaClient {
    client: Client,
    unlimited_client: Client,
    base_url: String,
    /// 请求超时，单位毫秒。
    ///
    /// 用原子量而非普通字段：用户在设置里改了超时后应当立刻生效，
    /// 不必重启应用。`Client` 的 timeout 是构造时烘死的、改不动，
    /// 所以这里额外保留一份可变的超时值，由请求方每轮现取。
    timeout_ms: Arc<AtomicU64>,
}

impl OllamaClient {
    /// 创建 Ollama 客户端
    pub fn new(base_url: String, timeout_ms: u64) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_millis(timeout_ms))
                .build()
                .unwrap_or_default(),
            unlimited_client: Client::builder().build().unwrap_or_default(),
            base_url,
            timeout_ms: Arc::new(AtomicU64::new(timeout_ms)),
        }
    }

    /// 热更新请求超时（毫秒）。供设置保存时调用。
    ///
    /// 注意只影响之后发起的请求：已经建好的连接不会被追改，
    /// 这是 HTTP 客户端层面的固有限制，不是这里的疏漏。
    pub fn set_timeout_ms(&self, timeout_ms: u64) {
        self.timeout_ms.store(timeout_ms, Ordering::SeqCst);
    }

    /// 当前生效的请求超时（毫秒）
    fn current_timeout(&self) -> u64 {
        self.timeout_ms.load(Ordering::SeqCst)
    }

    /// 检查 Ollama 服务是否可用
    pub async fn is_available(&self) -> bool {
        let url = format!("{}/api/tags", self.base_url);
        self.client.get(&url).send().await.is_ok()
    }

    /// 获取本地模型列表
    pub async fn list_models(&self) -> AppResult<Vec<OllamaModel>> {
        let url = format!("{}/api/tags", self.base_url);
        let resp = self.client.get(&url).send().await?;

        if !resp.status().is_success() {
            return Err(AppError::Ollama(format!(
                "Ollama returned status {}",
                resp.status()
            )));
        }

        let data: TagsResponse = resp.json().await?;
        Ok(data.models)
    }

    /// 后台预热模型。空 prompt 只触发加载，不产生用户可见内容。
    pub async fn warm_up(&self, model: &str) -> AppResult<()> {
        let url = format!("{}/api/generate", self.base_url);
        let response = self
            .client
            .post(&url)
            .timeout(std::time::Duration::from_secs(120))
            .json(&WarmupRequest {
                model: model.to_string(),
                prompt: String::new(),
                stream: false,
                keep_alive: "30m".to_string(),
            })
            .send()
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(AppError::Ollama(format!(
                "模型预热失败，Ollama 返回 {}",
                response.status()
            )))
        }
    }

    /// 流式聊天
    ///
    /// 返回一个 Stream，前端可以逐片消费。
    ///
    /// # Arguments
    /// * `model` - 模型名称（如 "qwen3:8b"）
    /// * `messages` - 消息历史
    /// * `tools` - 可选的工具定义列表（预构建的 JSON）
    /// * `temperature` - 采样温度
    ///
    /// # Returns
    /// Stream of ChatChunk
    pub async fn chat_stream(
        &self,
        model: &str,
        messages: Vec<OllamaMessage>,
        tools: Option<Vec<serde_json::Value>>,
        temperature: f32,
    ) -> AppResult<Pin<Box<dyn Stream<Item = ChatChunk> + Send>>> {
        self.chat_stream_with_timeout(
            model,
            messages,
            tools,
            temperature,
            Some(self.current_timeout()),
            None,
        )
        .await
    }

    /// 带工具、带总时限、且可追加推理参数的流式聊天。
    ///
    /// `chat_stream` 无法指定 `num_ctx`（走 Ollama 默认的 4096），
    /// `chat_stream_with_options` 又丢掉了 tools 参数且不设总时限，
    /// 两者都不满足 Agent 对话路径的需要（要同时有工具、有超时、有窗口配置）。
    pub async fn chat_stream_with_tools_and_options(
        &self,
        model: &str,
        messages: Vec<OllamaMessage>,
        tools: Option<Vec<serde_json::Value>>,
        temperature: f32,
        extra_options: serde_json::Value,
    ) -> AppResult<Pin<Box<dyn Stream<Item = ChatChunk> + Send>>> {
        self.chat_stream_with_timeout(
            model,
            messages,
            tools,
            temperature,
            Some(self.current_timeout()),
            Some(extra_options),
        )
        .await
    }

    /// 长任务专用流式聊天：不设总时限，由模型完成或用户退出应用时结束。
    pub async fn chat_stream_unlimited(
        &self,
        model: &str,
        messages: Vec<OllamaMessage>,
        tools: Option<Vec<serde_json::Value>>,
        temperature: f32,
    ) -> AppResult<Pin<Box<dyn Stream<Item = ChatChunk> + Send>>> {
        self.chat_stream_with_timeout(model, messages, tools, temperature, None, None)
            .await
    }

    /// 长输出场景（网页生成等）专用流式聊天。
    ///
    /// Ollama 的 `num_ctx` 默认只有 4096 token，既装不下需求文档，也写不完一个完整页面，
    /// 必须由调用方显式指定，否则提示词会被静默截断。此方法允许传入额外的 options。
    pub async fn chat_stream_with_options(
        &self,
        model: &str,
        messages: Vec<OllamaMessage>,
        temperature: f32,
        extra_options: serde_json::Value,
    ) -> AppResult<Pin<Box<dyn Stream<Item = ChatChunk> + Send>>> {
        self.chat_stream_with_timeout(model, messages, None, temperature, None, Some(extra_options))
            .await
    }

    async fn chat_stream_with_timeout(
        &self,
        model: &str,
        messages: Vec<OllamaMessage>,
        tools: Option<Vec<serde_json::Value>>,
        temperature: f32,
        timeout_ms: Option<u64>,
        extra_options: Option<serde_json::Value>,
    ) -> AppResult<Pin<Box<dyn Stream<Item = ChatChunk> + Send>>> {
        let (tx, rx) = mpsc::channel::<ChatChunk>(128);

        let url = format!("{}/api/chat", self.base_url);
        let client = if timeout_ms.is_some() {
            self.client.clone()
        } else {
            self.unlimited_client.clone()
        };
        let model = model.to_string();

        let mut options = if timeout_ms.is_none() {
            serde_json::json!({"temperature": temperature, "num_predict": -1})
        } else {
            serde_json::json!({"temperature": temperature})
        };
        if let Some(extra) = extra_options {
            if let (Some(base), Some(extra_map)) = (options.as_object_mut(), extra.as_object()) {
                for (key, value) in extra_map {
                    base.insert(key.clone(), value.clone());
                }
            }
        }

        let request_body = ChatRequest {
            model,
            messages,
            stream: true,
            tools,
            options: Some(options),
            keep_alive: "30m".to_string(),
            think: false,
        };

        // 在后台任务中执行 HTTP 请求和 SSE 解析
        tokio::spawn(async move {
            let result = async {
                let request = client.post(&url).json(&request_body);
                let request = match timeout_ms {
                    Some(timeout_ms) => request.timeout(std::time::Duration::from_millis(timeout_ms)),
                    None => request,
                };
                let resp = request.send().await.map_err(AppError::Network)?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(AppError::Ollama(format!(
                        "Ollama returned {}: {}",
                        status, body
                    )));
                }

                let mut stream = resp.bytes_stream();
                let mut pending_line = Vec::<u8>::new();

                while let Some(chunk_result) = stream.next().await {
                    let chunk = chunk_result.map_err(AppError::Network)?;
                    pending_line.extend_from_slice(&chunk);
                    while let Some(newline) = pending_line.iter().position(|byte| *byte == b'\n') {
                        let line_bytes = pending_line.drain(..=newline).collect::<Vec<_>>();
                        let line = String::from_utf8_lossy(&line_bytes);
                        let line = line.trim();
                        if line.is_empty() || line == "data: [DONE]" {
                            continue;
                        }
                        let json = line.strip_prefix("data: ").unwrap_or(line);
                        process_chat_response_line(json, &tx).await;
                    }
                }

                if !pending_line.is_empty() {
                    let line = String::from_utf8_lossy(&pending_line);
                    let line = line.trim();
                    if !line.is_empty() && line != "data: [DONE]" {
                        let json = line.strip_prefix("data: ").unwrap_or(line);
                        process_chat_response_line(json, &tx).await;
                    }
                }

                Ok(())
            }
            .await;

            if let Err(e) = result {
                let _ = tx.send(ChatChunk::Error(format!("{e:#}"))).await;
            }
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    /// 非流式聊天（简单场景）
    pub async fn chat(
        &self,
        model: &str,
        messages: Vec<OllamaMessage>,
        tools: Option<Vec<serde_json::Value>>,
        temperature: f32,
    ) -> AppResult<(String, Option<Vec<ToolCall>>)> {
        let url = format!("{}/api/chat", self.base_url);

        let request_body = ChatRequest {
            model: model.to_string(),
            messages,
            stream: false,
            tools,
            options: Some(serde_json::json!({"temperature": temperature})),
            keep_alive: "30m".to_string(),
            think: false,
        };

        let resp = self.client.post(&url).json(&request_body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::Ollama(format!(
                "Ollama returned {}: {}",
                status, body
            )));
        }

        let data: ChatResponse = resp.json().await?;

        if let Some(msg) = data.message {
            Ok((msg.content, msg.tool_calls))
        } else {
            Err(AppError::Ollama("Empty response from Ollama".to_string()))
        }
    }
}

/// 处理单条 Ollama 流式响应 JSON 行
#[allow(dead_code)]
async fn process_chat_response_line(json_str: &str, tx: &mpsc::Sender<ChatChunk>) {
    let parsed: Result<ChatResponse, _> = serde_json::from_str(json_str);

    match parsed {
        Ok(resp) => {
            if resp.done {
                let _ = tx
                    .send(ChatChunk::Done(resp.done_reason.clone(), resp.usage()))
                    .await;
                return;
            }

            if let Some(msg) = resp.message {
                // 检查是否有 tool_calls
                if let Some(tool_calls) = msg.tool_calls {
                    for tc in tool_calls {
                        let _ = tx.send(ChatChunk::ToolCall(tc)).await;
                    }
                }

                // 发送文本内容
                if !msg.content.is_empty() {
                    let _ = tx.send(ChatChunk::Text(msg.content)).await;
                }
            }
        }
        Err(_e) => {
            // 忽略解析失败的行（可能是 SSE 格式不完整或心跳包）
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ollama 真正的最后一个分片长这样 —— 用量字段只在这里出现。
    #[test]
    fn reads_usage_from_the_final_chunk() {
        let line = r#"{"model":"qwen3:8b","created_at":"2026-10-12T06:00:00Z","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","prompt_eval_count":3120,"eval_count":2048,"prompt_eval_duration":1500000000,"eval_duration":41000000000}"#;
        let resp: ChatResponse = serde_json::from_str(line).expect("应当能解析");
        let usage = resp.usage().expect("应当拿到用量");
        assert_eq!(usage.prompt_tokens, 3120);
        assert_eq!(usage.output_tokens, 2048);
        assert_eq!(usage.prompt_nanos, 1_500_000_000);
        assert_eq!(usage.eval_nanos, 41_000_000_000);
        assert!(usage.is_measured());
    }

    /// 中间分片没有用量字段，不能因此解析失败 —— 否则整条流会被打断。
    #[test]
    fn tolerates_stream_chunks_without_usage() {
        let line = r#"{"model":"qwen3:8b","created_at":"2026-10-12T06:00:00Z","message":{"role":"assistant","content":"<div>"},"done":false}"#;
        let resp: ChatResponse = serde_json::from_str(line).expect("应当能解析");
        assert!(!resp.done);
        assert!(resp.usage().is_none(), "没有用量就该是 None");
    }

    /// 用量全是 0 时按"没测到"处理，不要往日志里写一行全零的假数据。
    #[test]
    fn treats_all_zero_counts_as_unmeasured() {
        let usage = TokenUsage::default();
        assert!(!usage.is_measured());
        assert!(ChatResponse {
            model: String::new(),
            created_at: String::new(),
            message: None,
            done: true,
            done_reason: None,
            prompt_eval_count: Some(0),
            eval_count: Some(0),
            prompt_eval_duration: None,
            eval_duration: None,
        }
        .usage()
        .is_none());
    }

    /// 日志摘要要能算出生成速率，且除零不能 panic。
    #[test]
    fn formats_a_readable_summary_without_dividing_by_zero() {
        let usage = TokenUsage {
            prompt_tokens: 1000,
            output_tokens: 500,
            prompt_nanos: 1_000_000_000,
            eval_nanos: 10_000_000_000,
        };
        let line = usage.log_line();
        assert!(line.contains("提示词 1000 token"));
        assert!(line.contains("输出 500 token"));
        assert!(line.contains("50.0 tok/s"), "{line}");

        // 耗时为 0（某些后端不给）时不该出现 inf / NaN。
        let no_timing = TokenUsage {
            prompt_tokens: 1,
            output_tokens: 1,
            prompt_nanos: 0,
            eval_nanos: 0,
        };
        let line = no_timing.log_line();
        assert!(!line.contains("inf") && !line.contains("NaN"), "{line}");
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    /// 超时热更新：设置里改完之后，请求路径必须拿到新值。
    ///
    /// 改动前 timeout 是构造时烘死的普通字段，改了要重启才生效；
    /// 现在放在原子量里，`current_timeout()` 每轮现取。
    #[test]
    fn timeout_updates_take_effect_without_rebuilding_the_client() {
        let client = OllamaClient::new("http://localhost:11434".to_string(), 30_000);
        assert_eq!(client.current_timeout(), 30_000);

        client.set_timeout_ms(120_000);
        assert_eq!(client.current_timeout(), 120_000, "改完之后应立刻读出新值");

        client.set_timeout_ms(5_000);
        assert_eq!(client.current_timeout(), 5_000, "再次修改同样立即生效");
    }

    /// 原子量在 Arc 上是共享的，设置端 clone 出来的句柄改了也要能被读到。
    #[test]
    fn timeout_atomic_is_shared_across_clones_of_the_field() {
        let client = OllamaClient::new("http://localhost:11434".to_string(), 30_000);
        let handle = client.timeout_ms.clone();
        // 模拟命令层通过另一个句柄写值
        handle.store(88_000, Ordering::SeqCst);
        assert_eq!(client.current_timeout(), 88_000);
    }
}
