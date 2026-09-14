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
    Done,
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
    base_url: String,
    #[allow(dead_code)]
    timeout_ms: u64,
}

impl OllamaClient {
    /// 创建 Ollama 客户端
    pub fn new(base_url: String, timeout_ms: u64) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_millis(timeout_ms))
                .build()
                .unwrap_or_default(),
            base_url,
            timeout_ms,
        }
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
        let (tx, rx) = mpsc::channel::<ChatChunk>(128);

        let url = format!("{}/api/chat", self.base_url);
        let client = self.client.clone();
        let model = model.to_string();

        let request_body = ChatRequest {
            model,
            messages,
            stream: true,
            tools,
            options: Some(serde_json::json!({"temperature": temperature})),
        };

        // 在后台任务中执行 HTTP 请求和 SSE 解析
        tokio::spawn(async move {
            let result = async {
                let resp = client
                    .post(&url)
                    .json(&request_body)
                    .send()
                    .await
                    .map_err(AppError::Network)?;

                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(AppError::Ollama(format!(
                        "Ollama returned {}: {}",
                        status, body
                    )));
                }

                let mut stream = resp.bytes_stream();

                while let Some(chunk_result) = stream.next().await {
                    let chunk = chunk_result.map_err(AppError::Network)?;
                    let text = String::from_utf8_lossy(&chunk);

                    for line in text.lines() {
                        let line = line.trim();
                        if line.is_empty() || line == "data: [DONE]" {
                            continue;
                        }

                        // 去掉 "data: " 前缀
                        let json_str = if line.starts_with("data: ") {
                            &line[6..]
                        } else {
                            line
                        };

                        let parsed: Result<ChatResponse, _> =
                            serde_json::from_str(json_str);

                        match parsed {
                            Ok(resp) => {
                                if resp.done {
                                    let _ = tx.send(ChatChunk::Done).await;
                                    break;
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
                                        let _ = tx
                                            .send(ChatChunk::Text(msg.content))
                                            .await;
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!("[Ollama] JSON parse error: {} | line: {}", e, line);
                            }
                        }
                    }
                }

                Ok(())
            }
            .await;

            if let Err(e) = result {
                let _ = tx.send(ChatChunk::Error(e.to_string())).await;
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
        };

        let resp = self
            .client
            .post(&url)
            .json(&request_body)
            .send()
            .await?;

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
async fn process_chat_response_line(
    json_str: &str,
    tx: &mpsc::Sender<ChatChunk>,
) {
    let parsed: Result<ChatResponse, _> = serde_json::from_str(json_str);

    match parsed {
        Ok(resp) => {
            if resp.done {
                let _ = tx.send(ChatChunk::Done).await;
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
