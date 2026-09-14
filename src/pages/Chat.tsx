import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import CyberIcon from "../components/CyberIcon";

// ============================================================
// 类型定义
// ============================================================

interface Conversation {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  last_message?: string;
  is_pinned: boolean;
}

interface ConversationMessage {
  id: number;
  session_id: string;
  role: string;
  content: string;
  tool_calls?: string;
  tool_call_id?: string;
  timestamp: string;
}

interface OllamaModel {
  name: string;
  model: string;
}

interface ChatEvent {
  type: string;
  data?: string | { name: string; arguments: string } | { name: string; success: boolean; content: string };
}

interface ToolExecution {
  id: string;
  name: string;
  arguments: string;
  status: "running" | "success" | "error";
  result?: string;
  collapsed: boolean;
}

// ---- Task 相关类型 ----

interface TaskUpdateEvent {
  task_id: string;
  session_id: string;
  goal: string;
  status: string;
  current_step: number;
  total_steps: number;
  progress_percent: number;
  current_action?: string;
  is_simple: boolean;
}

interface ActionStatusEvent {
  task_id: string;
  step: number;
  action: string;
  status: string;
  message: string;
}

// V21: Recovery 恢复事件
interface RecoveryEvent {
  task_id: string;
  step_id: number;
  tool_name: string;
  attempt: number;
  strategy: string;
  message: string;
  timestamp: number;
}

// Phase 4: 权限确认弹窗类型
interface PermissionRequestPayload {
  request_id: string;
  tool_name: string;
  description: string;
  risk_level: string;
  path?: string;
}

// ============================================================
// 组件
// ============================================================

function Chat() {
  // ---- 状态 ----
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [currentConversationId, setCurrentConversationId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ConversationMessage[]>([]);
  const [inputText, setInputText] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [ollamaAvailable, setOllamaAvailable] = useState(false);
  const [ollamaModels, setOllamaModels] = useState<OllamaModel[]>([]);
  const [selectedModel, setSelectedModel] = useState("qwen3:8b");
  const [streamingText, setStreamingText] = useState("");
  const [thinking, setThinking] = useState(false);
  const [toolExecutions, setToolExecutions] = useState<ToolExecution[]>([]);
  const [chatError, setChatError] = useState<string | null>(null);

  // ---- Task 状态 ----
  const [currentTask, setCurrentTask] = useState<TaskUpdateEvent | null>(null);
  const [taskExpanded, setTaskExpanded] = useState(false);
  const [actionStatuses, setActionStatuses] = useState<ActionStatusEvent[]>([]);
  // V21: Recovery 恢复事件
  const [recoveryEvents, setRecoveryEvents] = useState<RecoveryEvent[]>([]);

  // Phase 4: 权限确认弹窗状态
  const [permissionRequest, setPermissionRequest] = useState<PermissionRequestPayload | null>(null);
  const [permissionResponding, setPermissionResponding] = useState(false);

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const unlistenRef = useRef<UnlistenFn[]>([]);
  const permissionUnlistenRef = useRef<UnlistenFn | null>(null);
  // 用 ref 存储当前对话 ID，避免 setupEventListeners 闭包捕获旧值
  const currentConversationIdRef = useRef<string | null>(null);

  // ---- 对话状态重置（切换/新建对话时必须调用，防止旧对话残留）----
  const resetConversationState = useCallback(() => {
    setMessages([]);
    setStreamingText("");
    setThinking(false);
    setIsLoading(false);
    setToolExecutions([]);
    setActionStatuses([]);
    setRecoveryEvents([]);
    setCurrentTask(null);
    setChatError(null);
    setTaskExpanded(false);
  }, []);

  // ---- 初始化 ----
  useEffect(() => {
    loadConversations();
    checkOllama();

    // Phase 4: 监听权限请求事件
    const setupPermissionListener = async () => {
      const unlisten = await listen("permission-request", (event) => {
        const payload = event.payload as PermissionRequestPayload;
        setPermissionRequest(payload);
      });
      permissionUnlistenRef.current = unlisten;
    };
    setupPermissionListener();

    return () => {
      unlistenRef.current.forEach((unlisten) => unlisten());
      if (permissionUnlistenRef.current) {
        permissionUnlistenRef.current();
      }
    };
  }, []);

  useEffect(() => {
    // 同步 ref，确保 setupEventListeners 闭包始终拿到最新对话 ID
    currentConversationIdRef.current = currentConversationId;
    // 切换对话时先重置所有运行时状态，防止旧对话残留
    resetConversationState();
    if (currentConversationId) {
      loadMessages(currentConversationId);
      loadActiveTask(currentConversationId);
    }
  }, [currentConversationId, resetConversationState]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, streamingText, toolExecutions, actionStatuses, recoveryEvents, permissionRequest]);

  // 全局超时保护：isLoading 超过 90 秒自动重置，防止永久卡死
  useEffect(() => {
    if (!isLoading) return;
    const timer = setTimeout(() => {
      console.warn("[Chat] isLoading timed out after 90s, resetting");
      setIsLoading(false);
      setThinking(false);
      setChatError("请求超时，请重试");
    }, 90000);
    return () => clearTimeout(timer);
  }, [isLoading]);

  // ---- 数据加载 ----

  async function loadConversations() {
    try {
      const data = await invoke<Conversation[]>("get_conversations");
      setConversations(data);
      if (data.length > 0 && !currentConversationId) {
        setCurrentConversationId(data[0].id);
      }
    } catch (e) {
      console.error("Failed to load conversations:", e);
    }
  }

  async function loadMessages(conversationId: string) {
    try {
      const data = await invoke<ConversationMessage[]>("get_messages", {
        conversationId,
      });
      setMessages(data);
    } catch (e) {
      console.error("Failed to load messages:", e);
    }
  }

  async function loadActiveTask(conversationId: string) {
    try {
      const task = await invoke<TaskUpdateEvent | null>("get_active_task", {
        conversationId,
      });
      setCurrentTask(task);
    } catch (e) {
      console.error("Failed to load active task:", e);
    }
  }

  async function checkOllama() {
    try {
      const available = await invoke<boolean>("check_ollama");
      setOllamaAvailable(available);
      if (available) {
        loadOllamaModels();
      }
    } catch (e) {
      console.error("Failed to check Ollama:", e);
      setOllamaAvailable(false);
    }
  }

  async function loadOllamaModels() {
    try {
      const models = await invoke<OllamaModel[]>("get_ollama_models");
      setOllamaModels(models);
      if (models.length > 0 && !selectedModel) {
        setSelectedModel(models[0].model);
      }
    } catch (e) {
      console.error("Failed to load Ollama models:", e);
    }
  }

  // ---- 事件监听 ----

  const setupEventListeners = useCallback(async () => {
    unlistenRef.current.forEach((u) => u());
    unlistenRef.current = [];

    const listeners: [string, (payload: string) => void][] = [
      ["chat:text", (payload) => {
        try {
          const event: ChatEvent = JSON.parse(payload);
          if (typeof event.data === "string") {
            setStreamingText((prev) => prev + event.data);
          }
        } catch {
          setStreamingText((prev) => prev + payload);
        }
      }],
      ["chat:thinking", () => {
        setThinking(true);
        setStreamingText("");
      }],
      ["chat:tool_call", (payload) => {
        try {
          const event: ChatEvent = JSON.parse(payload);
          if (event.data && typeof event.data === "object" && "name" in event.data) {
            const data = event.data as { name: string; arguments: string | object };
            const argsStr = typeof data.arguments === "string"
              ? data.arguments
              : JSON.stringify(data.arguments);
            const newExec: ToolExecution = {
              id: `${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
              name: data.name,
              arguments: argsStr,
              status: "running",
              collapsed: false,
            };
            setToolExecutions((prev) => [...prev, newExec]);
          }
        } catch (e) {
          console.error("Tool call parse error:", e);
        }
      }],
      ["chat:tool_result", (payload) => {
        try {
          const event: ChatEvent = JSON.parse(payload);
          if (event.data && typeof event.data === "object" && "name" in event.data) {
            const data = event.data as { name: string; success: boolean; content: string };
            setToolExecutions((prev) => {
              const reversed = [...prev].reverse();
              const revIndex = reversed.findIndex(
                (e) => e.name === data.name && e.status === "running"
              );
              if (revIndex === -1) return prev;
              const actualIndex = prev.length - 1 - revIndex;
              const updated = [...prev];
              updated[actualIndex] = {
                ...updated[actualIndex],
                status: data.success ? "success" : "error",
                result: data.content,
              };
              return updated;
            });
          }
        } catch (e) {
          console.error("Tool result parse error:", e);
        }
      }],
      ["chat:done", () => {
        setIsLoading(false);
        setThinking(false);
        setStreamingText("");
        const cid = currentConversationIdRef.current;
        if (cid) {
          loadMessages(cid);
          loadConversations();
          loadActiveTask(cid);
        }
      }],
      ["chat:error", (payload) => {
        setIsLoading(false);
        setThinking(false);
        // 解析错误消息：可能是纯字符串或 JSON 格式
        let errorMsg = "AI 响应出错";
        if (typeof payload === "string") {
          try {
            const parsed = JSON.parse(payload);
            errorMsg = parsed.data || parsed.message || parsed.error || payload;
          } catch {
            errorMsg = payload;
          }
        }
        setChatError(errorMsg);
        console.error("Chat error:", payload);
        const cid = currentConversationIdRef.current;
        if (cid) {
          loadActiveTask(cid);
        }
      }],
      // ---- Task 状态事件 ----
      ["agent:task_update", (payload) => {
        try {
          const event: TaskUpdateEvent = JSON.parse(payload);
          setCurrentTask(event);
        } catch (e) {
          console.error("Task update parse error:", e);
        }
      }],
      ["agent:action_status", (payload) => {
        try {
          const event: ActionStatusEvent = JSON.parse(payload);
          setActionStatuses((prev) => {
            const filtered = prev.filter((a) => !(a.step === event.step && a.task_id === event.task_id));
            return [...filtered, event];
          });
        } catch (e) {
          console.error("Action status parse error:", e);
        }
      }],
      // V21: Recovery 恢复事件
      ["agent:recovery", (payload) => {
        try {
          const event: RecoveryEvent = JSON.parse(payload);
          setRecoveryEvents((prev) => [...prev, event]);
        } catch (e) {
          console.error("Recovery event parse error:", e);
        }
      }],
    ];

    for (const [eventName, handler] of listeners) {
      const unlisten = await listen(eventName, (event) => {
        const payloadStr = String(event.payload);
        // V21: session_id 深度防御过滤——所有对话事件必须携带 session_id 且匹配当前对话
        const cid = currentConversationIdRef.current;
        try {
          const parsed = JSON.parse(payloadStr);
          if (cid) {
            // 有当前对话时：事件必须有 session_id 且必须匹配，否则忽略（防止旧对话/无主事件污染）
            if (!parsed.session_id || parsed.session_id !== cid) {
              return;
            }
          }
        } catch {
          // 非 JSON payload（理论上不应发生），有当前对话时忽略，无对话时放行兼容
          if (cid) {
            return;
          }
        }
        handler(payloadStr);
      });
      unlistenRef.current.push(unlisten);
    }
  }, []);

  // ---- 操作 ----

  async function handleCreateConversation(): Promise<string | null> {
    try {
      const conv = await invoke<Conversation>("create_conversation", {
        title: null,
      });
      setConversations((prev) => [conv, ...prev]);
      // 先重置所有状态，再切换到新对话（useEffect 会再次重置，双重保险）
      resetConversationState();
      setCurrentConversationId(conv.id);
      return conv.id;
    } catch (e) {
      console.error("Failed to create conversation:", e);
      return null;
    }
  }

  async function handleDeleteConversation(id: string, e: React.MouseEvent) {
    e.stopPropagation();
    try {
      await invoke("delete_conversation", { conversationId: id });
      setConversations((prev) => prev.filter((c) => c.id !== id));
      if (currentConversationId === id) {
        resetConversationState();
        setCurrentConversationId(null);
      }
    } catch (e) {
      console.error("Failed to delete conversation:", e);
    }
  }

  async function handleCancelTask() {
    if (!currentConversationId) return;
    try {
      await invoke("cancel_task", { conversationId: currentConversationId });
      setCurrentTask(null);
      setActionStatuses([]);
      if (currentConversationId) {
        loadActiveTask(currentConversationId);
      }
    } catch (e) {
      console.error("Failed to cancel task:", e);
    }
  }

  // Phase 4: 权限确认操作
  async function handlePermissionResponse(decision: "allow_once" | "always_allow" | "deny") {
    if (!permissionRequest || permissionResponding) return;

    setPermissionResponding(true);
    try {
      await invoke("respond_permission", {
        response: {
          request_id: permissionRequest.request_id,
          decision: decision,
        },
      });
      setPermissionRequest(null);
    } catch (e) {
      console.error("Failed to respond to permission request:", e);
      // 请求已超时或不存在，关闭弹窗并重置加载状态
      setPermissionRequest(null);
      setIsLoading(false);
      setChatError("权限确认已超时，请重试");
    } finally {
      setPermissionResponding(false);
    }
  }

  async function handleSendMessage() {
    if (!inputText.trim() || isLoading) return;

    let convId = currentConversationId;
    if (!convId) {
      const newId = await handleCreateConversation();
      if (!newId) return;
      convId = newId;
    }
    // 立即更新 ref，确保 setupEventListeners 闭包拿到最新对话 ID
    currentConversationIdRef.current = convId;

    const content = inputText.trim();
    setInputText("");
    setIsLoading(true);
    setStreamingText("");
    setThinking(false);
    setToolExecutions([]);
    setChatError(null);
    setActionStatuses([]);
    setRecoveryEvents([]);

    await loadMessages(convId);
    await setupEventListeners();

    try {
      await invoke("send_message", {
        conversationId: convId,
        content,
      });
    } catch (e) {
      console.error("Failed to send message:", e);
      setIsLoading(false);
    }
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSendMessage();
    }
  }

  function toggleToolCollapse(id: string) {
    setToolExecutions((prev) =>
      prev.map((t) => (t.id === id ? { ...t, collapsed: !t.collapsed } : t))
    );
  }

  // ---- 渲染辅助 ----

  const isTaskActive = currentTask && !["COMPLETED", "FAILED", "CANCELLED"].includes(currentTask.status);
  const statusColor =
    currentTask?.status === "COMPLETED" ? "#4caf50" :
    currentTask?.status === "FAILED" ? "#f44336" :
    currentTask?.status === "CANCELLED" ? "#9e9e9e" :
    "#2196f3";

  // ---- 渲染 ----

  return (
    <div className="chat-page">
      {/* 对话列表侧边栏 */}
      <aside className="chat-sidebar">
        <div className="chat-sidebar-header">
          <button className="new-chat-btn" onClick={handleCreateConversation}>
            + 新对话
          </button>
        </div>

        <div className="conversations-list">
          {conversations.map((conv) => (
            <div
              key={conv.id}
              className={`conversation-item ${
                conv.id === currentConversationId ? "active" : ""
              }`}
              onClick={() => setCurrentConversationId(conv.id)}
            >
              <div className="conv-title">{conv.title}</div>
              <div className="conv-meta">
                {conv.last_message && (
                  <span className="conv-preview">{conv.last_message}</span>
                )}
                <button
                  className="conv-delete"
                  onClick={(e) => handleDeleteConversation(conv.id, e)}
                  title="删除对话"
                >
                  ×
                </button>
              </div>
            </div>
          ))}
          {conversations.length === 0 && (
            <div className="empty-conversations">点击"新对话"开始</div>
          )}
        </div>

        {/* 模型选择 */}
        <div className="model-selector">
          <label>模型</label>
          <select
            value={selectedModel}
            onChange={(e) => setSelectedModel(e.target.value)}
            disabled={!ollamaAvailable || ollamaModels.length === 0}
          >
            {ollamaModels.map((m) => (
              <option key={m.model} value={m.model}>
                {m.name}
              </option>
            ))}
            {ollamaModels.length === 0 && (
              <option>未检测到模型</option>
            )}
          </select>
        </div>
      </aside>

      {/* 主聊天区域 */}
      <div className="chat-main">
        {/* Ollama 状态提示 */}
        {!ollamaAvailable && (
          <div className="ollama-warning">
            <CyberIcon name="alertTriangle" size={14} /> Ollama 未运行，请启动 Ollama 服务（默认 http://localhost:11434）
            <button onClick={checkOllama}>重试</button>
          </div>
        )}

        {/* 错误提示 */}
        {chatError && (
          <div className="chat-error-banner">
            <span className="chat-error-icon"><CyberIcon name="alertTriangle" size={14} /></span>
            <span className="chat-error-text">{chatError}</span>
            <button className="chat-error-close" onClick={() => setChatError(null)}>
              <CyberIcon name="close" size={14} />
            </button>
          </div>
        )}

        {/* 任务状态卡片 */}
        {currentTask && (
          <div className="task-card" style={{ borderLeftColor: statusColor }}>
            <div className="task-card-header" onClick={() => setTaskExpanded(!taskExpanded)}>
              <span className="task-status-dot" style={{ backgroundColor: statusColor }} />
              <span className="task-goal">{currentTask.goal}</span>
              <span className="task-status-badge" style={{ backgroundColor: statusColor + "20", color: statusColor }}>
                {currentTask.status}
              </span>
              {isTaskActive && (
                <button
                  className="task-cancel-btn"
                  onClick={(e) => { e.stopPropagation(); handleCancelTask(); }}
                  title="取消任务"
                >
                  <CyberIcon name="close" size={14} />
                </button>
              )}
              <span className="task-toggle">{taskExpanded ? <CyberIcon name="chevronDown" size={14} /> : <CyberIcon name="chevronRight" size={14} />}</span>
            </div>

            <div className="task-progress">
              <div className="task-progress-bar">
                <div
                  className="task-progress-fill"
                  style={{ width: `${currentTask.progress_percent}%`, backgroundColor: statusColor }}
                />
              </div>
              <span className="task-progress-text">
                {currentTask.current_step} / {currentTask.total_steps} 步
              </span>
            </div>

            {taskExpanded && (
              <div className="task-card-body">
                {currentTask.current_action && (
                  <div className="task-current-action">
                    <span className="task-action-label">当前步骤：</span>
                    <span>{currentTask.current_action}</span>
                  </div>
                )}

                {actionStatuses.length > 0 && (
                  <div className="task-action-log">
                    {actionStatuses.slice(-5).map((action, idx) => (
                      <div key={idx} className={`task-action-item ${action.status}`}>
                        <span className="task-action-icon">
                          {action.status === "completed" ? <CyberIcon name="check" size={14} /> :
                           action.status === "failed" ? <CyberIcon name="xCircle" size={14} /> :
                           action.status === "thinking" ? <CyberIcon name="brain" size={14} /> :
                           action.status === "planning" ? <CyberIcon name="logs" size={14} /> :
                           action.status === "finished" ? <CyberIcon name="checkCircle" size={14} /> : <CyberIcon name="chevronRight" size={14} />}
                        </span>
                        <span className="task-action-name">{action.action}</span>
                        <span className="task-action-msg">{action.message}</span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>
        )}

        {/* 消息列表 */}
        <div className="messages-container">
          {messages.length === 0 && !streamingText && toolExecutions.length === 0 && (
            <div className="chat-welcome">
              <div className="welcome-icon"><CyberIcon name="shieldCheck" size={56} /></div>
              <h2>PC Guardian AI</h2>
              <p>我是你的本地 AI 助手，可以直接操作你的电脑：</p>
              <ul>
                <li>打开文件和应用程序</li>
                <li>查看目录内容和读取文件</li>
                <li>执行系统命令</li>
                <li>截取屏幕</li>
              </ul>
              <p className="welcome-note">
                所有操作经过三层闸门权限校验，确保系统安全。
              </p>
            </div>
          )}

          {messages.map((msg) => (
            <MessageBubble key={msg.id} message={msg} />
          ))}

          {/* 流式输出 */}
          {isLoading && streamingText && (
            <div className="message-bubble assistant">
              <div className="message-avatar"><CyberIcon name="bot" size={18} /></div>
              <div className="message-content">
                <div className="message-text">{streamingText}</div>
                <div className="message-cursor" />
              </div>
            </div>
          )}

          {/* 思考中 */}
          {isLoading && thinking && !streamingText && (
            <div className="message-bubble assistant">
              <div className="message-avatar"><CyberIcon name="bot" size={18} /></div>
              <div className="message-content">
                <div className="thinking-indicator">
                  <span className="dot" />
                  <span className="dot" />
                  <span className="dot" />
                  <span>思考中...</span>
                </div>
              </div>
            </div>
          )}

          {/* 工具执行列表 */}
          {toolExecutions.map((tool) => {
            const toolRecoveries = recoveryEvents.filter((r) => r.tool_name === tool.name);
            return (
              <ToolExecutionCard
                key={tool.id}
                tool={tool}
                recoveries={toolRecoveries}
                onToggle={() => toggleToolCollapse(tool.id)}
              />
            );
          })}

          <div ref={messagesEndRef} />
        </div>

        {/* 输入框 */}
        <div className="chat-input-area">
          <textarea
            ref={inputRef}
            className="chat-input"
            placeholder={ollamaAvailable ? "输入消息..." : "请启动 Ollama 后使用"}
            value={inputText}
            onChange={(e) => setInputText(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={!ollamaAvailable || isLoading}
            rows={1}
          />
          <button
            className="send-btn"
            onClick={handleSendMessage}
            disabled={!inputText.trim() || !ollamaAvailable || isLoading}
          >
            {isLoading ? <span className="btn-spinner" /> : <CyberIcon name="send" size={18} />}
          </button>
        </div>
      </div>

      {/* Phase 4: 权限确认弹窗 */}
      {permissionRequest && (
        <div className="permission-overlay" onClick={() => !permissionResponding && handlePermissionResponse("deny")}>
          <div className="permission-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="permission-header">
              <div className="permission-icon"><CyberIcon name="lock" size={24} /></div>
              <h3>权限确认</h3>
            </div>
            <div className="permission-body">
              <div className="permission-section">
                <span className="permission-label">操作</span>
                <span className="permission-value">{permissionRequest.description}</span>
              </div>
              <div className="permission-section">
                <span className="permission-label">工具</span>
                <span className="permission-value">{permissionRequest.tool_name}</span>
              </div>
              {permissionRequest.path && (
                <div className="permission-section">
                  <span className="permission-label">路径</span>
                  <span className="permission-value path">{permissionRequest.path}</span>
                </div>
              )}
              <div className="permission-section">
                <span className="permission-label">风险等级</span>
                <span className={`permission-risk ${permissionRequest.risk_level.toLowerCase()}`}>
                  {permissionRequest.risk_level}
                </span>
              </div>
              {permissionRequest.risk_level === "HIGH" && (
                <div className="permission-warning">
                  <CyberIcon name="alertTriangle" size={14} /> 此操作风险等级较高，请确认是否执行。
                </div>
              )}
            </div>
            <div className="permission-actions">
              <button
                className="permission-btn deny"
                onClick={() => handlePermissionResponse("deny")}
                disabled={permissionResponding}
              >
                拒绝
              </button>
              <button
                className="permission-btn allow-once"
                onClick={() => handlePermissionResponse("allow_once")}
                disabled={permissionResponding}
              >
                允许一次
              </button>
              <button
                className="permission-btn allow-always"
                onClick={() => handlePermissionResponse("always_allow")}
                disabled={permissionResponding}
              >
                始终允许
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

// ---- 消息气泡组件 ----

function MessageBubble({ message }: { message: ConversationMessage }) {
  const isUser = message.role === "user";
  const isTool = message.role === "tool";

  if (isTool) {
    return (
      <div className="message-bubble tool">
        <div className="tool-result-inline">
          <span className="tool-label">工具结果</span>
          <pre className="tool-content">
            {(() => {
              try {
                const parsed = JSON.parse(message.content);
                return JSON.stringify(parsed, null, 2);
              } catch {
                return message.content;
              }
            })()}
          </pre>
        </div>
      </div>
    );
  }

  return (
    <div className={`message-bubble ${isUser ? "user" : "assistant"}`}>
      <div className="message-avatar">{isUser ? <CyberIcon name="user" size={18} /> : <CyberIcon name="bot" size={18} />}</div>
      <div className="message-content">
        <div className="message-text">
          {message.content.split("\n").map((line, i, arr) => (
            <span key={i}>
              {line}
              {i < arr.length - 1 && <br />}
            </span>
          ))}
        </div>
        {message.tool_calls && (
          <div className="message-tool-calls">
            <span className="tool-badge"><CyberIcon name="zap" size={14} /> 调用了工具</span>
          </div>
        )}
      </div>
    </div>
  );
}

// ---- 工具执行卡片组件 ----

function ToolExecutionCard({
  tool,
  recoveries,
  onToggle,
}: {
  tool: ToolExecution;
  recoveries: RecoveryEvent[];
  onToggle: () => void;
}) {
  const statusIcon =
    tool.status === "running" ? <CyberIcon name="clock" size={14} /> :
    tool.status === "success" ? <CyberIcon name="check" size={14} /> : <CyberIcon name="xCircle" size={14} />;

  const statusText =
    tool.status === "running" ? "执行中..." :
    tool.status === "success" ? "执行成功" : "执行失败";

  const strategyLabels: Record<string, string> = {
    retry: "重试",
    adjust_params: "调整参数",
    switch_tool: "切换工具",
    replan: "重新规划",
    escalate: "请求用户介入",
    fail: "标记完成",
  };

  const displayArgs = (() => {
    try {
      return JSON.stringify(JSON.parse(tool.arguments), null, 2);
    } catch {
      return tool.arguments;
    }
  })();

  const displayResult = (() => {
    if (!tool.result) return null;
    try {
      const parsed = JSON.parse(tool.result);
      if (parsed.error && typeof parsed.error === "string") {
        const code = parsed.code ? `[${parsed.code}] ` : "";
        return `${code}${parsed.error}`;
      }
      return JSON.stringify(parsed, null, 2);
    } catch {
      return tool.result;
    }
  })();

  return (
    <div className={`tool-execution-card ${tool.status}`}>
      <div className="tool-execution-header" onClick={onToggle}>
        <span className="tool-status-icon">{statusIcon}</span>
        <span className="tool-name">{tool.name}</span>
        <span className="tool-status-text">{statusText}</span>
        {recoveries.length > 0 && (
          <span className="tool-recovery-badge" title={`已恢复 ${recoveries.length} 次`}>
            <CyberIcon name="refresh" size={14} /> {recoveries.length}
          </span>
        )}
        <span className="tool-toggle">{tool.collapsed ? <CyberIcon name="chevronRight" size={14} /> : <CyberIcon name="chevronDown" size={14} />}</span>
      </div>

      {!tool.collapsed && (
        <div className="tool-execution-body">
          <div className="tool-section">
            <span className="tool-section-label">参数</span>
            <pre className="tool-code">{displayArgs}</pre>
          </div>

          {/* V21: Recovery 恢复状态展示 */}
          {recoveries.length > 0 && (
            <div className="tool-section recovery-section">
              <span className="tool-section-label">
                <CyberIcon name="zap" size={14} /> 恢复过程（{recoveries.length} 次）
              </span>
              <div className="recovery-list">
                {recoveries.map((r, idx) => (
                  <div key={idx} className="recovery-item">
                    <span className="recovery-attempt">第 {r.attempt} 次</span>
                    <span className={`recovery-strategy strategy-${r.strategy}`}>
                      {strategyLabels[r.strategy] || r.strategy}
                    </span>
                    <span className="recovery-message">{r.message}</span>
                  </div>
                ))}
              </div>
            </div>
          )}

          {tool.status !== "running" && displayResult && (
            <div className="tool-section">
              <span className="tool-section-label">结果</span>
              <pre className={`tool-code ${tool.status}`}>{displayResult}</pre>
            </div>
          )}

          {tool.status === "running" && (
            <div className="tool-running-indicator">
              <span className="tool-spinner" />
              <span>正在执行 {tool.name}...</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default Chat;
