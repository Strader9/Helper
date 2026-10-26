import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import {
  AlertTriangle,
  Ban,
  Bot,
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  Loader2,
  Plus,
  RefreshCw,
  Send,
  ShieldCheck,
  Sparkles,
  Square,
  Trash2,
  User,
  X,
} from "lucide-react";
import ExecutionEvidence from "../components/ExecutionEvidence";
import { createChatRunId, formatToolResult, humanizeError, parseEventEnvelope } from "../chatUi";
import { fetchTaskTruth, type ReceiptStatus, type TaskTruthResponse } from "../lib/taskTruth";
import { Button } from "../components/ui/button";
import { Badge, StatusDot } from "../components/ui/badge";
import { Progress } from "../components/ui/feedback";
import { Tooltip } from "../components/ui/primitives";
import { PermissionGate } from "../components/PermissionGate";
import { cn } from "../lib/cn";

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

interface ToolVerification {
  status: ReceiptStatus;
  evidence?: string;
}

interface ToolExecution {
  id: string;
  name: string;
  arguments: string;
  status: "running" | "success" | "error";
  result?: string;
  /** 后端回执给出的验证结论：没有它，界面就不能说"成功" */
  verification?: ToolVerification;
  collapsed: boolean;
}

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

interface RecoveryEvent {
  task_id: string;
  step_id: number;
  tool_name: string;
  attempt: number;
  strategy: string;
  message: string;
  timestamp: number;
}

interface PermissionRequestPayload {
  request_id: string;
  session_id: string;
  run_id?: string;
  tool_name: string;
  description: string;
  risk_level: string;
  path?: string;
}

// ============================================================
// 组件
// ============================================================

function Chat() {
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
  const [taskTruth, setTaskTruth] = useState<TaskTruthResponse | null>(null);

  const [currentTask, setCurrentTask] = useState<TaskUpdateEvent | null>(null);
  const [taskExpanded, setTaskExpanded] = useState(false);
  const [actionStatuses, setActionStatuses] = useState<ActionStatusEvent[]>([]);
  const [recoveryEvents, setRecoveryEvents] = useState<RecoveryEvent[]>([]);

  const [permissionRequest, setPermissionRequest] = useState<PermissionRequestPayload | null>(null);
  const [permissionResponding, setPermissionResponding] = useState(false);

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const unlistenRef = useRef<UnlistenFn[]>([]);
  const listenerSetupGenerationRef = useRef(0);
  const activeRunIdRef = useRef<string | null>(null);
  const permissionUnlistenRef = useRef<UnlistenFn | null>(null);
  const currentConversationIdRef = useRef<string | null>(null);
  const lastAssistantTextRef = useRef("");

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
    setPermissionRequest(null);
    setTaskTruth(null);
    lastAssistantTextRef.current = "";
  }, []);

  useEffect(() => {
    loadConversations();
    checkOllama();

    const setupPermissionListener = async () => {
      const unlisten = await listen("permission-request", (event) => {
        const payload = event.payload as PermissionRequestPayload;
        const currentSession = currentConversationIdRef.current;
        if (currentSession && payload.session_id !== currentSession) return;
        if (activeRunIdRef.current && payload.run_id && payload.run_id !== activeRunIdRef.current) return;
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
    currentConversationIdRef.current = currentConversationId;
    resetConversationState();
    if (currentConversationId) {
      loadMessages(currentConversationId);
      loadActiveTask(currentConversationId);
    }
  }, [currentConversationId, resetConversationState]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, streamingText, toolExecutions, actionStatuses, recoveryEvents, permissionRequest]);

  // 输入框自动增高：单行起，最多 8 行后内部滚动
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
  }, [inputText]);

  useEffect(() => {
    if (!isLoading) return;
    const timer = setTimeout(() => {
      console.warn("[Chat] isLoading timed out after 90s, resetting");
      activeRunIdRef.current = null;
      setIsLoading(false);
      setThinking(false);
      setChatError("请求超时，请重试");
      if (currentConversationId) {
        invoke("cancel_task", { conversationId: currentConversationId }).catch(() => {});
      }
    }, 90000);
    return () => clearTimeout(timer);
  }, [isLoading, currentConversationId]);

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
      const data = await invoke<ConversationMessage[]>("get_messages", { conversationId });
      setMessages(data);
    } catch (e) {
      console.error("Failed to load messages:", e);
    }
  }

  async function loadActiveTask(conversationId: string) {
    try {
      const task = await invoke<TaskUpdateEvent | null>("get_active_task", { conversationId });
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
      const [models, settings] = await Promise.all([
        invoke<OllamaModel[]>("get_ollama_models"),
        invoke<{ key: string; value: string }[]>("get_settings"),
      ]);
      setOllamaModels(models);
      const configuredModel = settings.find((item) => item.key === "ollama.model")?.value;
      if (configuredModel && models.some((item) => item.model === configuredModel)) {
        setSelectedModel(configuredModel);
      } else if (models.length > 0 && !models.some((item) => item.model === selectedModel)) {
        setSelectedModel(models[0].model);
      }
    } catch (e) {
      console.error("Failed to load Ollama models:", e);
    }
  }

  // ---- 事件监听（逻辑保持原样，只换渲染）----

  const setupEventListeners = useCallback(async () => {
    const generation = ++listenerSetupGenerationRef.current;

    const listeners: [string, (payload: string) => void][] = [
      ["chat:text", (payload) => {
        try {
          const event: ChatEvent = JSON.parse(payload);
          if (typeof event.data === "string") {
            lastAssistantTextRef.current += event.data;
            setStreamingText((prev) => prev + event.data);
          }
        } catch {
          setStreamingText((prev) => prev + payload);
        }
      }],
      ["chat:thinking", () => {
        setIsLoading(true);
        setThinking(true);
        setStreamingText("");
        setPermissionRequest(null);
        setTaskTruth(null);
        lastAssistantTextRef.current = "";
      }],
      ["chat:tool_call", (payload) => {
        try {
          const event: ChatEvent = JSON.parse(payload);
          if (event.data && typeof event.data === "object" && "name" in event.data) {
            const data = event.data as { name: string; arguments: string | object };
            const argsStr = typeof data.arguments === "string" ? data.arguments : JSON.stringify(data.arguments);
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
            const verification = extractVerification(data.content);
            setToolExecutions((prev) => {
              const reversed = [...prev].reverse();
              const revIndex = reversed.findIndex((e) => e.name === data.name && e.status === "running");
              if (revIndex === -1) return prev;
              const actualIndex = prev.length - 1 - revIndex;
              const updated = [...prev];
              updated[actualIndex] = {
                ...updated[actualIndex],
                status: data.success ? "success" : "error",
                result: formatToolResult(data.content),
                verification,
              };
              return updated;
            });
          }
        } catch (e) {
          console.error("Tool result parse error:", e);
        }
      }],
      ["chat:done", () => {
        const finishedRunId = activeRunIdRef.current;
        const finalText = lastAssistantTextRef.current;
        activeRunIdRef.current = null;
        setIsLoading(false);
        setThinking(false);
        setStreamingText("");
        if (finishedRunId) {
          fetchTaskTruth(finishedRunId, finalText).then((truth) => {
            if (truth && truth.snapshot.counts.total > 0) {
              setTaskTruth(truth);
            }
          });
        }
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
        const errorMsg = humanizeError(payload, "AI 响应出错");
        activeRunIdRef.current = null;
        setChatError(errorMsg);
        console.error("Chat error:", payload);
        const cid = currentConversationIdRef.current;
        if (cid) loadActiveTask(cid);
      }],
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
      ["agent:recovery", (payload) => {
        try {
          const event: RecoveryEvent = JSON.parse(payload);
          setRecoveryEvents((prev) => [...prev, event]);
        } catch (e) {
          console.error("Recovery event parse error:", e);
        }
      }],
    ];

    const newUnlisteners = await Promise.all(
      listeners.map(([eventName, handler]) =>
        listen(eventName, (event) => {
          const payloadStr = String(event.payload);
          const cid = currentConversationIdRef.current;
          try {
            const parsed = parseEventEnvelope(payloadStr);
            if (cid) {
              if (!parsed.session_id || parsed.session_id !== cid) return;
            }
            if (eventName === "chat:thinking" && parsed.run_id) {
              activeRunIdRef.current = parsed.run_id;
            }
            if (parsed.run_id && parsed.run_id !== activeRunIdRef.current) return;
          } catch {
            if (cid) return;
          }
          handler(payloadStr);
        })
      )
    );

    if (generation !== listenerSetupGenerationRef.current) {
      newUnlisteners.forEach((unlisten) => unlisten());
      return;
    }
    unlistenRef.current.forEach((unlisten) => unlisten());
    unlistenRef.current = newUnlisteners;
  }, []);

  useEffect(() => {
    setupEventListeners().catch((e) => {
      console.error("Failed to set up chat listeners:", e);
    });
    return () => {
      listenerSetupGenerationRef.current += 1;
      unlistenRef.current.forEach((unlisten) => unlisten());
      unlistenRef.current = [];
    };
  }, [setupEventListeners]);

  // ---- 操作 ----

  async function handleCreateConversation(): Promise<string | null> {
    try {
      const conv = await invoke<Conversation>("create_conversation", { title: null });
      setConversations((prev) => [conv, ...prev]);
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
    activeRunIdRef.current = null;
    setIsLoading(false);
    setThinking(false);
    setPermissionRequest(null);
    try {
      await invoke("cancel_task", { conversationId: currentConversationId });
      setCurrentTask(null);
      setActionStatuses([]);
      loadActiveTask(currentConversationId);
    } catch (e) {
      console.error("Failed to cancel task:", e);
    }
  }

  async function handlePermissionResponse(decision: "allow_once" | "always_allow" | "deny") {
    if (!permissionRequest || permissionResponding) return;
    setPermissionResponding(true);
    try {
      await invoke("respond_permission", {
        response: { request_id: permissionRequest.request_id, decision },
      });
      setPermissionRequest(null);
    } catch (e) {
      console.error("Failed to respond to permission request:", e);
      setPermissionRequest(null);
      setIsLoading(false);
      setChatError("权限确认已超时，请重试");
    } finally {
      setPermissionResponding(false);
    }
  }

  async function handleSendMessage() {
    if (!inputText.trim()) return;

    let convId = currentConversationId;
    if (!convId) {
      const newId = await handleCreateConversation();
      if (!newId) return;
      convId = newId;
    }
    currentConversationIdRef.current = convId;

    const content = inputText.trim();
    const requestId = createChatRunId();
    activeRunIdRef.current = requestId;
    setInputText("");
    setIsLoading(true);
    setStreamingText("");
    setThinking(true);
    setMessages((prev) => [
      ...prev,
      {
        id: -Date.now(),
        session_id: convId!,
        role: "user",
        content,
        timestamp: new Date().toISOString(),
      },
    ]);
    setToolExecutions([]);
    setChatError(null);
    setActionStatuses([]);
    setRecoveryEvents([]);
    setPermissionRequest(null);

    try {
      const acceptedRunId = await invoke<string>("send_message", {
        conversationId: convId,
        content,
        model: selectedModel,
        requestId,
      });
      if (activeRunIdRef.current === requestId) {
        activeRunIdRef.current = acceptedRunId;
      }
    } catch (e) {
      console.error("Failed to send message:", e);
      if (activeRunIdRef.current === requestId) {
        activeRunIdRef.current = null;
        setIsLoading(false);
        setThinking(false);
        setChatError(humanizeError(e, "发送失败，请重试"));
      }
    }
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSendMessage();
    }
  }

  function toggleToolCollapse(id: string) {
    setToolExecutions((prev) => prev.map((t) => (t.id === id ? { ...t, collapsed: !t.collapsed } : t)));
  }

  const isTaskActive = currentTask && !["COMPLETED", "FAILED", "CANCELLED"].includes(currentTask.status);
  const taskTone =
    currentTask?.status === "COMPLETED"
      ? "success"
      : currentTask?.status === "FAILED"
      ? "danger"
      : currentTask?.status === "CANCELLED"
      ? "muted"
      : "brand";

  const hasContent = messages.length > 0 || !!streamingText || toolExecutions.length > 0;

  return (
    <div className="ds flex h-full overflow-hidden">
      {/* ---------------- 会话列表 ---------------- */}
      <aside className="flex w-56 shrink-0 flex-col border-r border-[var(--color-line)] bg-[var(--color-surface)]">
        <div className="p-2.5">
          <Button variant="secondary" size="sm" className="w-full justify-start gap-1.5" onClick={handleCreateConversation}>
            <Plus className="size-3.5" />
            新对话
          </Button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-2">
          {conversations.length === 0 ? (
            <p className="px-2 py-6 text-center text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
              点「新对话」开始
            </p>
          ) : (
            conversations.map((conv) => {
              const active = conv.id === currentConversationId;
              return (
                <div
                  key={conv.id}
                  onClick={() => setCurrentConversationId(conv.id)}
                  className={cn(
                    "group relative mb-0.5 cursor-pointer rounded-[8px] px-2.5 py-2 transition-colors",
                    active ? "bg-[var(--color-elevated)]" : "hover:bg-[var(--color-hover)]/60"
                  )}
                >
                  <div className="flex items-center gap-1.5">
                    <span
                      className={cn(
                        "min-w-0 flex-1 truncate text-[var(--text-xs)]",
                        active ? "font-medium text-[var(--color-foreground)]" : "text-[var(--color-muted-foreground)]"
                      )}
                    >
                      {conv.title || "未命名对话"}
                    </span>
                    <button
                      onClick={(e) => handleDeleteConversation(conv.id, e)}
                      className="shrink-0 rounded-[4px] p-0.5 text-[var(--color-subtle-foreground)] opacity-0 transition-opacity hover:text-[var(--color-danger)] group-hover:opacity-100"
                      title="删除对话"
                    >
                      <Trash2 className="size-3" />
                    </button>
                  </div>
                  {conv.last_message && (
                    <p className="mt-0.5 truncate text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                      {conv.last_message}
                    </p>
                  )}
                </div>
              );
            })
          )}
        </div>

        {/* 模型 */}
        <div className="border-t border-[var(--color-line-faint)] p-2.5">
          <div className="label-caps mb-1 px-1">模型</div>
          <select
            value={selectedModel}
            onChange={(e) => setSelectedModel(e.target.value)}
            disabled={!ollamaAvailable || ollamaModels.length === 0}
            className="w-full cursor-pointer rounded-[7px] border border-[var(--color-line)] bg-[var(--color-card)] px-2 py-1.5 text-[var(--text-xs)] text-[var(--color-foreground)] outline-none transition-colors hover:border-[var(--color-line-strong)] disabled:opacity-50"
          >
            {ollamaModels.map((m) => (
              <option key={m.model} value={m.model} className="bg-[var(--color-card)]">
                {m.name}
              </option>
            ))}
            {ollamaModels.length === 0 && <option>未检测到模型</option>}
          </select>
        </div>
      </aside>

      {/* ---------------- 主区 ---------------- */}
      <div className="flex min-w-0 flex-1 flex-col">
        {!ollamaAvailable && (
          <div className="flex shrink-0 items-center gap-2 border-b border-[var(--color-warning)]/25 bg-[var(--color-warning)]/8 px-4 py-2">
            <AlertTriangle className="size-3.5 shrink-0 text-[var(--color-warning)]" />
            <span className="min-w-0 flex-1 text-[var(--text-xs)] text-[var(--color-warning)]">
              Ollama 未运行。启动本地模型服务后即可对话（默认 http://localhost:11434）
            </span>
            <Button variant="ghost" size="xs" onClick={checkOllama} className="gap-1">
              <RefreshCw className="size-3" />
              重试
            </Button>
          </div>
        )}

        {chatError && (
          <div className="flex shrink-0 items-center gap-2 border-b border-[var(--color-danger)]/25 bg-[var(--color-danger)]/8 px-4 py-2">
            <AlertTriangle className="size-3.5 shrink-0 text-[var(--color-danger)]" />
            <span className="min-w-0 flex-1 text-[var(--text-xs)] text-[var(--color-danger)]">{chatError}</span>
            <button
              onClick={() => setChatError(null)}
              className="shrink-0 rounded p-0.5 text-[var(--color-danger)] hover:bg-[var(--color-danger)]/15"
            >
              <X className="size-3.5" />
            </button>
          </div>
        )}

        {/* 任务条 */}
        {currentTask && (
          <div className="shrink-0 border-b border-[var(--color-line)] bg-[var(--color-surface)]/60 px-4 py-2">
            <div className="flex items-center gap-2.5">
              <StatusDot tone={taskTone} pulse={!!isTaskActive} />
              <span className="min-w-0 flex-1 truncate text-[var(--text-xs)] text-[var(--color-foreground)]">
                {currentTask.goal}
              </span>
              <Badge tone={taskTone === "muted" ? "neutral" : taskTone}>{currentTask.status}</Badge>
              <span className="num-tabular shrink-0 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                {currentTask.current_step}/{currentTask.total_steps}
              </span>
              {isTaskActive && (
                <Button variant="ghost" size="icon-sm" onClick={handleCancelTask} title="取消任务">
                  <Ban className="size-3.5" />
                </Button>
              )}
              <button
                onClick={() => setTaskExpanded((v) => !v)}
                className="shrink-0 rounded p-0.5 text-[var(--color-subtle-foreground)] hover:text-[var(--color-foreground)]"
              >
                {taskExpanded ? <ChevronDown className="size-3.5" /> : <ChevronRight className="size-3.5" />}
              </button>
            </div>
            <Progress
              value={currentTask.progress_percent}
              tone={taskTone === "muted" ? "info" : taskTone}
              height={2}
              className="mt-1.5"
            />
            {taskExpanded && (
              <div className="mt-2 space-y-1">
                {currentTask.current_action && (
                  <p className="text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                    当前：{currentTask.current_action}
                  </p>
                )}
                {actionStatuses.slice(-5).map((a, i) => (
                  <div key={i} className="flex items-center gap-1.5 text-[var(--text-2xs)]">
                    <span
                      className={cn(
                        "size-1 shrink-0 rounded-full",
                        a.status === "completed"
                          ? "bg-[var(--color-success)]"
                          : a.status === "failed"
                          ? "bg-[var(--color-danger)]"
                          : "bg-[var(--color-brand)]"
                      )}
                    />
                    <span className="truncate text-[var(--color-muted-foreground)]">{a.action}</span>
                    <span className="truncate text-[var(--color-subtle-foreground)]">{a.message}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}

        {/* 消息流 */}
        <div className="min-h-0 flex-1 overflow-y-auto">
          <div className="mx-auto w-full max-w-[760px] px-5 py-6">
            {!hasContent && (
              <div className="flex flex-col items-center justify-center py-16 text-center">
                <div className="mb-4 flex size-12 items-center justify-center rounded-[14px] border border-[var(--color-line)] bg-[var(--color-card)] shadow-[inset_0_1px_0_rgba(255,255,255,.05)]">
                  <ShieldCheck className="size-6 text-[var(--color-brand)]" />
                </div>
                <h2 className="text-[var(--text-lg)] font-semibold tracking-[-0.01em]">今天想做什么</h2>
                <p className="mt-1.5 max-w-sm text-[var(--text-xs)] leading-relaxed text-[var(--color-muted-foreground)]">
                  我能直接操作这台电脑：读写文件、执行命令、截屏、查资料、生成图像。
                  每个动作都会经过权限闸门，并在账本里留下回执。
                </p>
                <div className="mt-5 flex flex-wrap justify-center gap-1.5">
                  {["整理一下下载目录", "看看 D 盘还剩多少空间", "读一下我的收件箱", "生成一张赛博朋克城市图"].map((s) => (
                    <button
                      key={s}
                      onClick={() => {
                        setInputText(s);
                        inputRef.current?.focus();
                      }}
                      className="cursor-pointer rounded-full border border-[var(--color-line)] bg-[var(--color-card)] px-3 py-1.5 text-[var(--text-2xs)] text-[var(--color-muted-foreground)] transition-colors hover:border-[var(--color-line-strong)] hover:text-[var(--color-foreground)]"
                    >
                      {s}
                    </button>
                  ))}
                </div>
              </div>
            )}

            {messages.map((msg) => (
              <MessageBubble key={msg.id} message={msg} />
            ))}

            {toolExecutions.map((tool) => (
              <ToolExecutionCard
                key={tool.id}
                tool={tool}
                recoveries={recoveryEvents.filter((r) => r.tool_name === tool.name)}
                onToggle={() => toggleToolCollapse(tool.id)}
              />
            ))}

            {isLoading && streamingText && (
              <AssistantRow>
                <p className="whitespace-pre-wrap break-words text-[var(--text-sm)] leading-[1.75] text-[var(--color-foreground)]">
                  {streamingText}
                  <span className="ml-0.5 inline-block h-4 w-[2px] translate-y-0.5 animate-[ds-pulse-dot_1s_ease-in-out_infinite] bg-[var(--color-brand)]" />
                </p>
              </AssistantRow>
            )}

            {isLoading && thinking && !streamingText && (
              <AssistantRow>
                <div className="flex items-center gap-1.5 py-1 text-[var(--text-xs)] text-[var(--color-muted-foreground)]">
                  {[0, 1, 2].map((i) => (
                    <span
                      key={i}
                      className="size-1.5 rounded-full bg-[var(--color-subtle-foreground)] animate-[ds-pulse-dot_1.4s_ease-in-out_infinite]"
                      style={{ animationDelay: `${i * 0.18}s` }}
                    />
                  ))}
                  <span className="ml-1">思考中</span>
                </div>
              </AssistantRow>
            )}

            {taskTruth && <ExecutionEvidence snapshot={taskTruth.snapshot} claim={taskTruth.claim} />}

            <div ref={messagesEndRef} className="h-1" />
          </div>
        </div>

        {/* 输入区 */}
        <div className="shrink-0 px-5 pb-4">
          <div className="mx-auto w-full max-w-[760px]">
            <div className="rounded-[14px] border border-[var(--color-line)] bg-[var(--color-card)] shadow-[inset_0_1px_0_rgba(255,255,255,.04),var(--shadow-sm)] transition-colors focus-within:border-[var(--color-line-strong)]">
              <textarea
                ref={inputRef}
                value={inputText}
                onChange={(e) => setInputText(e.target.value)}
                onKeyDown={handleKeyDown}
                placeholder={ollamaAvailable ? "输入指令，Enter 发送，Shift+Enter 换行" : "启动 Ollama 后即可对话"}
                disabled={!ollamaAvailable}
                rows={1}
                className="max-h-[200px] w-full resize-none bg-transparent px-3.5 pb-1 pt-3 text-[var(--text-sm)] leading-relaxed text-[var(--color-foreground)] outline-none placeholder:text-[var(--color-subtle-foreground)] disabled:opacity-50"
              />
              <div className="flex items-center gap-1.5 px-2 pb-2">
                <span className="flex items-center gap-1 truncate rounded-[6px] px-1.5 py-0.5 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                  <Sparkles className="size-3" />
                  {selectedModel}
                </span>
                <span className="ml-auto text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                  {inputText.length > 0 ? `${inputText.length} 字` : ""}
                </span>
                {isLoading ? (
                  <Button variant="secondary" size="icon-sm" onClick={handleCancelTask} title="停止">
                    <Square className="size-3 fill-current" />
                  </Button>
                ) : (
                  <Button
                    variant="primary"
                    size="icon-sm"
                    onClick={handleSendMessage}
                    disabled={!inputText.trim() || !ollamaAvailable}
                    title="发送"
                  >
                    <Send className="size-3.5" />
                  </Button>
                )}
              </div>
            </div>
            <p className="mt-1.5 text-center text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
              操作会先过权限闸门；MEDIUM 及以上会在执行前询问你
            </p>
          </div>
        </div>
      </div>

      <PermissionGate
        request={permissionRequest}
        onRespond={permissionResponding ? () => {} : handlePermissionResponse}
      />
    </div>
  );
}

// ============================================================
// 消息
// ============================================================

function AssistantRow({ children }: { children: React.ReactNode }) {
  return (
    <div className="mb-5 flex gap-3">
      <div className="mt-0.5 flex size-6 shrink-0 items-center justify-center rounded-[7px] border border-[var(--color-line)] bg-[var(--color-card)]">
        <Bot className="size-3.5 text-[var(--color-brand)]" />
      </div>
      <div className="min-w-0 flex-1">{children}</div>
    </div>
  );
}

function MessageBubble({ message }: { message: ConversationMessage }) {
  const isUser = message.role === "user";
  const [copied, setCopied] = useState(false);

  if (message.role === "tool") {
    return (
      <div className="mb-4 ml-9">
        <div className="rounded-[8px] border border-[var(--color-line)] bg-[var(--color-surface)]">
          <div className="flex items-center gap-1.5 border-b border-[var(--color-line-faint)] px-2.5 py-1.5">
            <span className="label-caps">工具结果</span>
          </div>
          <pre className="max-h-48 overflow-auto px-2.5 py-2 font-mono text-[var(--text-2xs)] leading-relaxed text-[var(--color-muted-foreground)]">
            {(() => {
              try {
                return JSON.stringify(JSON.parse(message.content), null, 2);
              } catch {
                return message.content;
              }
            })()}
          </pre>
        </div>
      </div>
    );
  }

  if (isUser) {
    return (
      <div className="mb-5 flex justify-end gap-3">
        <div className="max-w-[80%] rounded-[14px] rounded-tr-[4px] border border-[var(--color-line)] bg-[var(--color-elevated)] px-3.5 py-2.5">
          <p className="whitespace-pre-wrap break-words text-[var(--text-sm)] leading-[1.7] text-[var(--color-foreground)]">
            {message.content}
          </p>
        </div>
        <div className="mt-0.5 flex size-6 shrink-0 items-center justify-center rounded-[7px] border border-[var(--color-line)] bg-[var(--color-surface)]">
          <User className="size-3.5 text-[var(--color-muted-foreground)]" />
        </div>
      </div>
    );
  }

  return (
    <div className="mb-5">
      <AssistantRow>
        <div className="group relative">
          <p className="whitespace-pre-wrap break-words text-[var(--text-sm)] leading-[1.75] text-[var(--color-foreground)]">
            {message.content}
          </p>
          {message.tool_calls && (
            <div className="mt-2 flex items-center gap-1.5">
              <Badge tone="outline">调用了工具</Badge>
            </div>
          )}
          <div className="absolute -right-1 top-0 opacity-0 transition-opacity group-hover:opacity-100">
            <Tooltip content="复制">
              <button
                onClick={async () => {
                  try {
                    await navigator.clipboard.writeText(message.content);
                    setCopied(true);
                    setTimeout(() => setCopied(false), 1200);
                  } catch {
                    /* 剪贴板不可用时静默失败，不打断阅读 */
                  }
                }}
                className="rounded-[5px] border border-[var(--color-line)] bg-[var(--color-card)] p-1 text-[var(--color-subtle-foreground)] hover:text-[var(--color-foreground)]"
              >
                {copied ? <Check className="size-3 text-[var(--color-success)]" /> : <Copy className="size-3" />}
              </button>
            </Tooltip>
          </div>
        </div>
      </AssistantRow>
    </div>
  );
}

// ---- 工具结论呈现 ----

type ToolTone = "running" | "success" | "unverified" | "contradicted" | "error";

function toolStatusPresentation(tool: ToolExecution): { label: string; tone: ToolTone } {
  if (tool.status === "running") return { label: "执行中…", tone: "running" };
  switch (tool.verification?.status) {
    case "VERIFIED":
      return { label: "已验证", tone: "success" };
    case "CONTRADICTED":
      return { label: "与事实不符", tone: "contradicted" };
    case "FAILED":
      return { label: "执行失败", tone: "error" };
    case "UNVERIFIED":
      return { label: "未能确认", tone: "unverified" };
    case "NOT_REQUIRED":
      return { label: "已执行 · 无可核对判据", tone: "success" };
    default:
      return tool.status === "success"
        ? { label: "执行成功（未登记判据）", tone: "success" }
        : { label: "执行失败", tone: "error" };
  }
}

function extractVerification(content: string): ToolVerification | undefined {
  try {
    const parsed: unknown = JSON.parse(content);
    if (!parsed || typeof parsed !== "object") return undefined;
    const raw = (parsed as { verification?: unknown }).verification;
    if (!raw || typeof raw !== "object") return undefined;
    const record = raw as { status?: unknown; evidence?: unknown };
    if (typeof record.status !== "string") return undefined;
    return {
      status: record.status as ReceiptStatus,
      evidence: typeof record.evidence === "string" ? record.evidence : undefined,
    };
  } catch {
    return undefined;
  }
}

function ToolExecutionCard({
  tool,
  recoveries,
  onToggle,
}: {
  tool: ToolExecution;
  recoveries: RecoveryEvent[];
  onToggle: () => void;
}) {
  const presentation = toolStatusPresentation(tool);

  const toneMap: Record<ToolTone, { badge: "brand" | "success" | "warning" | "danger" | "neutral"; icon: React.ReactNode }> = {
    running: { badge: "brand", icon: <Loader2 className="size-3 animate-ds-spin" /> },
    success: { badge: "success", icon: <Check className="size-3" /> },
    unverified: { badge: "warning", icon: <AlertTriangle className="size-3" /> },
    contradicted: { badge: "danger", icon: <AlertTriangle className="size-3" /> },
    error: { badge: "danger", icon: <X className="size-3" /> },
  };
  const tone = toneMap[presentation.tone];

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
    <div className="mb-3 ml-9 overflow-hidden rounded-[9px] border border-[var(--color-line)] bg-[var(--color-card)]">
      <button
        onClick={onToggle}
        className="flex w-full cursor-pointer items-center gap-2 px-2.5 py-2 text-left transition-colors hover:bg-[var(--color-hover)]/40"
      >
        <span
          className={cn(
            "flex size-4 items-center justify-center rounded-full",
            presentation.tone === "running" && "text-[var(--color-brand)]",
            presentation.tone === "success" && "text-[var(--color-success)]",
            presentation.tone === "unverified" && "text-[var(--color-warning)]",
            (presentation.tone === "error" || presentation.tone === "contradicted") && "text-[var(--color-danger)]"
          )}
        >
          {tone.icon}
        </span>
        <span className="min-w-0 flex-1 truncate font-mono text-[var(--text-xs)] text-[var(--color-foreground)]">
          {tool.name}
        </span>
        {recoveries.length > 0 && (
          <Tooltip content={`已恢复 ${recoveries.length} 次`}>
            <span className="flex items-center gap-0.5 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
              <RefreshCw className="size-2.5" />
              {recoveries.length}
            </span>
          </Tooltip>
        )}
        <Badge tone={tone.badge}>{presentation.label}</Badge>
        {tool.collapsed ? (
          <ChevronRight className="size-3.5 shrink-0 text-[var(--color-subtle-foreground)]" />
        ) : (
          <ChevronDown className="size-3.5 shrink-0 text-[var(--color-subtle-foreground)]" />
        )}
      </button>

      {!tool.collapsed && (
        <div className="space-y-2.5 border-t border-[var(--color-line-faint)] px-2.5 py-2.5">
          <div>
            <div className="label-caps mb-1">参数</div>
            <pre className="max-h-40 overflow-auto rounded-[6px] bg-[var(--color-surface)] px-2 py-1.5 font-mono text-[var(--text-2xs)] leading-relaxed text-[var(--color-muted-foreground)]">
              {displayArgs}
            </pre>
          </div>

          {recoveries.length > 0 && (
            <div>
              <div className="label-caps mb-1">恢复过程（{recoveries.length} 次）</div>
              <div className="space-y-1">
                {recoveries.map((r, idx) => (
                  <div key={idx} className="flex items-start gap-1.5 text-[var(--text-2xs)]">
                    <Badge tone="outline">第 {r.attempt} 次</Badge>
                    <span className="shrink-0 text-[var(--color-muted-foreground)]">
                      {strategyLabels[r.strategy] || r.strategy}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-[var(--color-subtle-foreground)]">{r.message}</span>
                  </div>
                ))}
              </div>
            </div>
          )}

          {tool.status !== "running" && displayResult && (
            <div>
              <div className="label-caps mb-1">结果</div>
              <pre
                className={cn(
                  "max-h-56 overflow-auto rounded-[6px] bg-[var(--color-surface)] px-2 py-1.5 font-mono text-[var(--text-2xs)] leading-relaxed",
                  presentation.tone === "error" || presentation.tone === "contradicted"
                    ? "text-[var(--color-danger)]"
                    : "text-[var(--color-muted-foreground)]"
                )}
              >
                {displayResult}
              </pre>
            </div>
          )}

          {tool.verification && (
            <div>
              <div className="label-caps mb-1">判据</div>
              <p className="text-[var(--text-2xs)] leading-relaxed text-[var(--color-muted-foreground)]">
                {tool.verification.evidence ?? "这一轮没有拿到可以核对的证据"}
              </p>
            </div>
          )}

          {tool.status === "running" && (
            <div className="flex items-center gap-1.5 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
              <Loader2 className="size-3 animate-ds-spin" />
              正在执行 {tool.name}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default Chat;
