use std::io::Read;

use base64::{engine::general_purpose::STANDARD, Engine};

use crate::error::{AppError, AppResult};
use crate::llm::OllamaMessage;
use crate::tools::ToolExecutionResult;

const MAX_SCREEN_BYTES: u64 = 12 * 1024 * 1024;

pub(super) struct ScreenObservation {
    pub(super) captured_at: String,
    image: String,
}

pub(super) fn requests_screen_observation(text: &str) -> bool {
    let text = text.trim().to_lowercase();
    let text = text
        .trim_start_matches("请")
        .trim_start_matches("帮我")
        .trim();
    let forbidden = [
        "不要截图",
        "别截图",
        "无需截图",
        "不截图",
        "不读取屏幕",
        "不看屏幕",
        "不许截图",
        "don't capture",
        "do not capture",
        "without taking a screenshot",
    ];
    if forbidden.iter().any(|phrase| text.contains(phrase)) {
        return false;
    }
    let prefix = [
        "看看",
        "看一下",
        "看一看",
        "看屏幕",
        "查看",
        "分析",
        "识别",
        "读一下",
        "look at ",
        "inspect ",
        "analyze ",
        "read ",
    ];
    let first_clause = text
        .split(['，', '。', '；', ',', ';', '\n'])
        .next()
        .unwrap_or_default();
    prefix.iter().any(|phrase| first_clause.starts_with(phrase))
        && [
            "当前屏幕",
            "屏幕上",
            "屏幕里",
            "我的屏幕",
            "看看屏幕",
            "看屏幕",
            "my screen",
            "the screen",
            "current screen",
        ]
        .iter()
        .any(|target| first_clause.contains(target))
}

impl ScreenObservation {
    pub(super) async fn from_capture(execution: ToolExecutionResult) -> AppResult<Self> {
        if !execution.success {
            return Err(AppError::ToolExecution(
                execution
                    .result
                    .error
                    .unwrap_or_else(|| "无法获取当前屏幕".into()),
            ));
        }
        let path = execution
            .result
            .data
            .and_then(|data| {
                data.get("path")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .ok_or_else(|| AppError::ToolExecution("截图结果缺少图像文件".into()))?;
        let image = tokio::task::spawn_blocking(move || -> AppResult<String> {
            let mut bytes = Vec::new();
            std::fs::File::open(path)?
                .take(MAX_SCREEN_BYTES + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_SCREEN_BYTES || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err(AppError::ToolExecution("截图格式无效或超过 12 MiB".into()));
            }
            Ok(STANDARD.encode(bytes))
        })
        .await
        .map_err(|error| AppError::Internal(error.to_string()))??;
        Ok(Self {
            captured_at: chrono::Utc::now().to_rfc3339(),
            image,
        })
    }

    pub(super) fn attach_to(self, messages: &mut Vec<OllamaMessage>, user_content: &str) {
        if let Some(current) = messages
            .last_mut()
            .filter(|message| message.role == "user" && message.content == user_content)
        {
            current.images.push(self.image);
        } else {
            let mut current = OllamaMessage::user(user_content);
            current.images.push(self.image);
            messages.push(current);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_screen_requests_match() {
        assert!(requests_screen_observation(
            "请帮我看看当前屏幕上的报错，并写一份诊断"
        ));
        assert!(requests_screen_observation(
            "Inspect my screen and explain the error"
        ));
        assert!(!requests_screen_observation("翻译文字中的“看看屏幕”"));
        assert!(!requests_screen_observation("看看当前屏幕，但不要截图"));
        assert!(!requests_screen_observation("看看这个文件"));
    }

    #[test]
    fn image_is_attached_only_to_the_current_turn() {
        let mut messages = vec![
            OllamaMessage::user("以前的消息"),
            OllamaMessage::user("看看屏幕"),
        ];
        let observation = ScreenObservation {
            captured_at: "timestamp".into(),
            image: "image".into(),
        };
        observation.attach_to(&mut messages, "看看屏幕");
        assert!(messages[0].images.is_empty());
        assert_eq!(messages[1].images, vec!["image"]);
        assert_eq!(messages[1].content, "看看屏幕");
    }

    #[tokio::test]
    async fn screen_task_passes_image_to_local_model_and_verifies_written_diagnosis() {
        use crate::agent::{assistant, task::TaskManager, ChatEvent, RunCancellation};
        use crate::{
            db::{conversation::ConversationManager, Database},
            llm::OllamaClient,
            security::PermissionManager,
            tools::{ToolCategory, ToolExecutor, ToolRegistry},
        };
        use serde_json::{json, Value};
        use std::{path::Path, sync::Arc, time::Duration};
        use tokio::{
            io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
            net::TcpListener,
            sync::{mpsc, Mutex},
        };

        let root = std::env::current_dir()
            .unwrap()
            .join(format!(".assist-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let output = root.join("diagnosis.txt");
        let output_for_model = output.clone();
        let user = "看看当前屏幕上的报错，并写一份诊断";
        let mock = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ollama = OllamaClient::new(format!("http://{}", mock.local_addr().unwrap()), 2_000);
        let server = tokio::spawn(async move {
            let first = json!({"model":"mock","created_at":"2026-11-27T00:00:00Z",
                "message":{"role":"assistant","content":"","tool_calls":[{"function":{
                "name":"write_file","arguments":{"path": output_for_model, "content":"诊断结果"}
            }}]},"done":false});
            let second = json!({"model":"mock","created_at":"2026-11-27T00:00:01Z",
                "message":{"role":"assistant","content":"已保存诊断"},"done":false});
            for (index, response) in [first, second].into_iter().enumerate() {
                let (socket, _) = mock.accept().await.unwrap();
                let mut reader = BufReader::new(socket);
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                assert!(line.starts_with("POST /api/chat "));
                let mut length = None;
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).await.unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = Some(value.trim().parse::<usize>().unwrap());
                        }
                    }
                }
                let mut body = vec![0; length.unwrap()];
                reader.read_exact(&mut body).await.unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                if index == 0 {
                    let messages = request["messages"].as_array().unwrap();
                    assert_eq!(messages.last().unwrap()["content"], user);
                    assert_eq!(messages.last().unwrap()["images"], json!(["mock-image"]));
                    assert!(request["tools"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|tool| { tool["function"]["name"] == "write_file" }));
                } else {
                    assert!(request["messages"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|message| { message["role"] == "tool" }));
                }
                let done = json!({"model":"mock","created_at":"2026-11-27T00:00:01Z","done":true});
                let body = format!("{response}\n{done}\n");
                let http = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                reader.get_mut().write_all(http.as_bytes()).await.unwrap();
            }
        });

        let database = Database::new(Path::new(":memory:")).unwrap();
        database.init_schema().unwrap();
        let conversations = ConversationManager::new(database.connection());
        let session = conversations.create(None).unwrap();
        conversations
            .append_message(&session.id, "user", user, None, None)
            .unwrap();
        let task = TaskManager::new(database.connection())
            .create(&session.id, user)
            .unwrap();
        let db = Arc::new(Mutex::new(database));
        let registry = Arc::new(ToolRegistry::new(db.clone(), root.clone()));
        let mut permissions = PermissionManager::new();
        permissions.update_config(false);
        permissions
            .path_policy_mut()
            .set_trusted_dirs(vec![root.clone()]);
        let executor = ToolExecutor::new(
            Arc::new(Mutex::new(permissions)),
            registry.clone(),
            db.clone(),
        );
        let cancellation = RunCancellation::new();
        let (tx, mut rx) = mpsc::channel(64);
        let run_id = "screen-test";
        tokio::time::timeout(
            Duration::from_secs(10),
            assistant::run(
                assistant::AssistantContext {
                    session_id: &session.id,
                    user_content: user,
                    db: &db,
                    ollama: &ollama,
                    executor: &executor,
                    tool_registry: &registry,
                    model: "mock",
                    temperature: 0.0,
                    max_iterations: 2,
                    tx: &tx,
                    categories: &[ToolCategory::File],
                    run_id,
                    cancellation: &cancellation,
                    screen_observation: Some(ScreenObservation {
                        captured_at: "timestamp".into(),
                        image: "mock-image".into(),
                    }),
                    jarvis_observation: None,
                },
                task,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            output.is_file(),
            "no diagnosis artifact, agent events: {events:?}"
        );
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "诊断结果");
        let mut verified = false;
        for event in events {
            if let ChatEvent::ToolResult {
                verification_status,
                receipt_id,
                ..
            } = event
            {
                assert_eq!(
                    verification_status,
                    crate::tools::VerificationStatus::Verified
                );
                assert!(!receipt_id.is_empty());
                verified = true;
            }
        }
        assert!(verified);
        assert_eq!(
            db.lock()
                .await
                .list_tool_receipts_by_run(run_id, 10)
                .unwrap()
                .len(),
            1
        );
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
