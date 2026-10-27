//! OpenAI 兼容协议的结构体与 SSE 流解析。
//!
//! 与 `ollama.rs` 的职责对称：只做「字节 → 结构体 → ChatChunk」，
//! 不含网络。网络请求仍在 `OllamaClient` 里按 endpoint 分派。
//!
//! 关键差异：OpenAI 流式的 `tool_calls[].function.arguments` 是**分片**，
//! 跨多个 chunk 累积，必须在 `finish_reason` 或流结束时聚合成完整调用，
//! 否则上层拿到残缺 JSON 无法执行工具。

use std::collections::HashMap;
use tokio::sync::mpsc;

use super::ollama::{ChatChunk, FunctionCall, OllamaMessage, TokenUsage, ToolCall};

/// OpenAI 兼容流式请求体。
#[derive(Debug, serde::Serialize)]
pub struct OpenAiChatRequest {
    pub model: String,
    pub messages: Vec<OllamaMessage>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    pub temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<serde_json::Value>,
}

/// OpenAI 兼容响应 chunk。
#[derive(Debug, serde::Deserialize)]
struct OpenAiChunk {
    #[serde(default)]
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenAiChoice {
    #[serde(default)]
    delta: OpenAiDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct OpenAiDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCallDelta>>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenAiToolCallDelta {
    #[serde(default)]
    index: u64,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<OpenAiFunctionDelta>,
}

#[derive(Debug, serde::Deserialize)]
struct OpenAiFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// 从 OpenAI 兼容 usage 对象转 `TokenUsage`。
/// 兼容 `prompt_tokens/completion_tokens`（OpenAI）与
/// `prompt_eval_count/eval_count`（sidecar 转发 Ollama 时透传的字段）。
fn usage_from_openai(raw: Option<&serde_json::Value>) -> TokenUsage {
    let Some(raw) = raw else {
        return TokenUsage::default();
    };
    let get = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|k| raw.get(k).and_then(|v| v.as_u64()))
            .unwrap_or(0)
    };
    TokenUsage {
        prompt_tokens: get(&["prompt_tokens", "prompt_eval_count"]),
        output_tokens: get(&["completion_tokens", "eval_count"]),
        prompt_nanos: 0,
        eval_nanos: 0,
    }
}

/// 一次未完成的工具调用（`arguments` 分片累积中）。
struct PendingCall {
    id: Option<String>,
    name: String,
    args: String,
}

/// OpenAI 兼容 SSE 解析器，持有跨 chunk 的工具调用聚合状态。
pub struct OpenAiStreamParser {
    pending_calls: HashMap<u64, PendingCall>,
}

impl OpenAiStreamParser {
    pub fn new() -> Self {
        Self {
            pending_calls: HashMap::new(),
        }
    }

    /// 处理一行 SSE JSON。返回 true 表示这条是 `finish_reason`（流收尾）。
    pub async fn process_line(&mut self, json_str: &str, tx: &mpsc::Sender<ChatChunk>) -> bool {
        let parsed: Result<OpenAiChunk, _> = serde_json::from_str(json_str);
        let Ok(chunk) = parsed else {
            return false; // 坏行跳过
        };

        if let Some(usage) = &chunk.usage {
            // usage 只在最后一个 chunk 出现，记在 done 里发出。
            let _ = usage;
        }

        let Some(choice) = chunk.choices.first() else {
            return false;
        };

        // 累积工具调用分片
        if let Some(tool_calls) = &choice.delta.tool_calls {
            for tc in tool_calls {
                let entry = self
                    .pending_calls
                    .entry(tc.index)
                    .or_insert_with(|| PendingCall {
                        id: None,
                        name: String::new(),
                        args: String::new(),
                    });
                if let Some(id) = &tc.id {
                    entry.id = Some(id.clone());
                }
                if let Some(function) = &tc.function {
                    if let Some(name) = &function.name {
                        entry.name = name.clone();
                    }
                    if let Some(arguments) = &function.arguments {
                        entry.args.push_str(arguments);
                    }
                }
            }
        }

        // 文本增量
        if let Some(content) = &choice.delta.content {
            if !content.is_empty() {
                let _ = tx.send(ChatChunk::Text(content.clone())).await;
            }
        }

        // finish_reason：flush 完整工具调用，再发 done。
        if let Some(reason) = &choice.finish_reason {
            self.flush_calls(tx).await;
            let usage = usage_from_openai(chunk.usage.as_ref());
            let _ = tx
                .send(ChatChunk::Done(Some(reason.clone()), Some(usage)))
                .await;
            return true;
        }

        false
    }

    /// 流结束时兜底 flush 未完成的工具调用（部分后端不派发 finish_reason）。
    pub async fn flush_calls(&mut self, tx: &mpsc::Sender<ChatChunk>) {
        let mut calls: Vec<(u64, PendingCall)> = self.pending_calls.drain().collect();
        calls.sort_by_key(|(idx, _)| *idx);
        for (_, call) in calls {
            if call.name.is_empty() {
                continue;
            }
            let arguments = serde_json::from_str::<serde_json::Value>(&call.args)
                .unwrap_or_else(|_| serde_json::Value::String(call.args.clone()));
            let _ = tx
                .send(ChatChunk::ToolCall(ToolCall {
                    id: call.id,
                    call_type: Some("function".to_string()),
                    function: FunctionCall {
                        name: call.name,
                        arguments,
                    },
                }))
                .await;
        }
    }
}

impl Default for OpenAiStreamParser {
    fn default() -> Self {
        Self::new()
    }
}

/// OpenAI 兼容非流式响应（`stream: false`）。
#[derive(Debug, serde::Deserialize)]
pub struct OpenAiChatResponse {
    #[serde(default)]
    pub choices: Vec<OpenAiChatChoice>,
}

#[derive(Debug, serde::Deserialize)]
pub struct OpenAiChatChoice {
    #[serde(default)]
    pub message: OpenAiChatMessage,
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct OpenAiChatMessage {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl OpenAiChatMessage {
    /// 拆成 (content, tool_calls)，并把字符串型 `arguments` 解析成 JSON 值
    /// （与 Ollama 路径对齐，上层拿到的是对象而不是字符串）。
    pub fn into_parts(self) -> (String, Option<Vec<ToolCall>>) {
        let content = self.content.unwrap_or_default();
        let calls = self.tool_calls.map(|calls| {
            calls
                .into_iter()
                .map(|mut call| {
                    if let serde_json::Value::String(args) = &call.function.arguments {
                        call.function.arguments = serde_json::from_str(args)
                            .unwrap_or_else(|_| serde_json::Value::String(args.clone()));
                    }
                    call
                })
                .collect()
        });
        (content, calls)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(chunks: Vec<ChatChunk>) -> (Vec<String>, Vec<ToolCall>, Option<TokenUsage>) {
        let mut texts = Vec::new();
        let mut calls = Vec::new();
        let mut usage = None;
        for c in chunks {
            match c {
                ChatChunk::Text(t) => texts.push(t),
                ChatChunk::ToolCall(tc) => calls.push(tc),
                ChatChunk::Done(_, u) => usage = u,
                _ => {}
            }
        }
        (texts, calls, usage)
    }

    async fn parse_lines(lines: &[&str]) -> Vec<ChatChunk> {
        let (tx, mut rx) = mpsc::channel::<ChatChunk>(64);
        let mut parser = OpenAiStreamParser::new();
        for line in lines {
            parser.process_line(line, &tx).await;
        }
        parser.flush_calls(&tx).await;
        drop(tx);
        let mut out = Vec::new();
        while let Ok(c) = rx.try_recv() {
            out.push(c);
        }
        out
    }

    #[tokio::test]
    async fn parses_text_delta() {
        let chunks = parse_lines(&[
            r#"{"choices":[{"index":0,"delta":{"content":"你"},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"好"},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#,
        ])
        .await;
        let (texts, calls, usage) = collect(chunks);
        assert_eq!(texts, vec!["你".to_string(), "好".to_string()]);
        assert!(calls.is_empty());
        assert_eq!(usage.unwrap().output_tokens, 2);
    }

    #[tokio::test]
    async fn aggregates_fragmented_tool_call_arguments() {
        let chunks = parse_lines(&[
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":""}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"/etc/hosts\"}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ])
        .await;
        let (_texts, calls, _usage) = collect(chunks);
        assert_eq!(calls.len(), 1, "分片应聚合为一个完整调用");
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(
            calls[0].function.arguments,
            serde_json::json!({"path": "/etc/hosts"})
        );
        assert_eq!(calls[0].id.as_deref(), Some("call_1"));
    }

    #[tokio::test]
    async fn flushes_pending_calls_on_stream_end_without_finish_reason() {
        let chunks = parse_lines(&[
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c9","function":{"name":"ping","arguments":"{}"}}]},"finish_reason":null}]}"#,
        ])
        .await;
        let (_texts, calls, _usage) = collect(chunks);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "ping");
    }

    #[tokio::test]
    async fn tolerates_bad_lines() {
        let chunks = parse_lines(&[
            "not-json",
            r#"{"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":null}]}"#,
        ])
        .await;
        let (texts, _calls, _usage) = collect(chunks);
        assert_eq!(texts, vec!["ok".to_string()]);
    }
}
