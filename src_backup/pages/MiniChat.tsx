import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Loader2,
  Maximize2,
  Send,
  ShieldCheck,
  X,
  XCircle,
} from "lucide-react";
import { createChatRunId, formatToolResult, humanizeError, parseEventEnvelope } from "../chatUi";
import { cn } from "../lib/cn";
import "../styles/mini.css";

// ============================================================
// 类型定义（与 Chat.tsx 保持一致）
// ============================================================

interface ConversationMessage {
  id: number;
  session_id: string;
  role: string;
  content: string;
  tool_calls?: string;
  tool_call_id?: string;
  timestamp: string;
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

// 动态人像资源（从视频抠像生成的透明背景 WebP）
const AVATAR_SRC = "/assets/avatar/avatar_animated.webp";

// ============================================================
// MiniChat 组件 —— 无边框透明浮窗
//
// 和主界面共用一套设计 token，但布局逻辑不同：这里没有页面骨架，
// 只有一个可拖动的头像和一张按需展开的对话面板。
// ============================================================

function MiniChat() {
  // ---- 状态 ----
  const [messages, setMessages] = useState<ConversationMessage[]>([]);
  const [inputText, setInputText] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [streamingText, setStreamingText] = useState("");
  const [thinking, setThinking] = useState(false);
  const [toolExecutions, setToolExecutions] = useState<ToolExecution[]>([]);
  const [currentConversationId, setCurrentConversationId] = useState<string | null>(null);
  const [showChat, setShowChat] = useState(false); // 是否展开聊天面板
  const [avatarError, setAvatarError] = useState(false);

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const avatarAreaRef = useRef<HTMLDivElement>(null);
  const unlistenRef = useRef<UnlistenFn[]>([]);
  const listenerSetupGenerationRef = useRef(0);
  const activeRunIdRef = useRef<string | null>(null);
  // 用 ref 存储当前对话 ID，避免 setupEventListeners 闭包捕获旧值
  const currentConversationIdRef = useRef<string | null>(null);

  // ---- 初始化：加载或创建对话 + 原生拖动事件 ----
  useEffect(() => {
    initConversation();

    // 用原生 addEventListener 监听 mousedown，在原生事件同步上下文中调用 startDragging
    // Tauri 官方要求：startDragging 必须在原生 mousedown 事件中调用，React 合成事件中调用会失效
    const avatarEl = avatarAreaRef.current;
    if (avatarEl) {
      const handleNativeMouseDown = (e: MouseEvent) => {
        if (e.button !== 0) return;
        // 同步调用 startDragging，这是透明窗口最可靠的拖动方式
        getCurrentWindow().startDragging().catch(() => {});
      };
      avatarEl.addEventListener("mousedown", handleNativeMouseDown);
      return () => {
        avatarEl.removeEventListener("mousedown", handleNativeMouseDown);
        unlistenRef.current.forEach((unlisten) => unlisten());
      };
    }

    return () => {
      unlistenRef.current.forEach((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, streamingText, toolExecutions, showChat]);

  // 同步 ref，确保 setupEventListeners 闭包始终拿到最新对话 ID
  useEffect(() => {
    currentConversationIdRef.current = currentConversationId;
  }, [currentConversationId]);

  async function initConversation() {
    try {
      const conversations = await invoke<{ id: string }[]>("get_conversations");
      if (conversations.length > 0) {
        const convId = conversations[0].id;
        setCurrentConversationId(convId);
        loadMessages(convId);
      } else {
        const conv = await invoke<{ id: string }>("create_conversation", { title: null });
        setCurrentConversationId(conv.id);
        setMessages([]);
      }
    } catch (e) {
      console.error("Failed to init conversation:", e);
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

  // ---- 事件监听 ----
  const setupEventListeners = useCallback(async () => {
    const generation = ++listenerSetupGenerationRef.current;

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
        setIsLoading(true);
        setThinking(true);
        setStreamingText("");
      }],
      ["chat:tool_call", (payload) => {
        try {
          const event: ChatEvent = JSON.parse(payload);
          if (event.data && typeof event.data === "object" && "name" in event.data) {
            const data = event.data as { name: string; arguments: string };
            const newExec: ToolExecution = {
              id: `${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
              name: data.name,
              arguments: data.arguments,
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
                result: formatToolResult(data.content),
              };
              return updated;
            });
          }
        } catch (e) {
          console.error("Tool result parse error:", e);
        }
      }],
      ["chat:done", () => {
        activeRunIdRef.current = null;
        setIsLoading(false);
        setThinking(false);
        setStreamingText("");
        const cid = currentConversationIdRef.current;
        if (cid) {
          loadMessages(cid);
        }
      }],
      ["chat:error", (payload) => {
        activeRunIdRef.current = null;
        setIsLoading(false);
        setThinking(false);
        console.error("Chat error:", humanizeError(payload, "AI 响应出错"));
      }],
    ];

    const newUnlisteners = await Promise.all(
      listeners.map(([eventName, handler]) => listen(eventName, (event) => {
        const payloadStr = String(event.payload);
        // session_id 过滤——防止收到主窗口或其他对话的事件
        const cid = currentConversationIdRef.current;
        try {
          const parsed = parseEventEnvelope(payloadStr);
          if (cid && (!parsed.session_id || parsed.session_id !== cid)) {
            return;
          }
          if (eventName === "chat:thinking" && parsed.run_id) {
            activeRunIdRef.current = parsed.run_id;
          }
          if (parsed.run_id && parsed.run_id !== activeRunIdRef.current) {
            return;
          }
        } catch {
          if (cid) return;
        }
        handler(payloadStr);
      }))
    );

    if (generation !== listenerSetupGenerationRef.current) {
      newUnlisteners.forEach((unlisten) => unlisten());
      return;
    }
    unlistenRef.current.forEach((unlisten) => unlisten());
    unlistenRef.current = newUnlisteners;
  }, []);

  // 保持监听器常驻，发送时无需等待多次 IPC 注册。
  useEffect(() => {
    setupEventListeners().catch((e) => {
      console.error("Failed to set up mini chat listeners:", e);
    });
    return () => {
      listenerSetupGenerationRef.current += 1;
      unlistenRef.current.forEach((unlisten) => unlisten());
      unlistenRef.current = [];
    };
  }, [setupEventListeners]);

  // ---- 操作 ----

  async function handleSendMessage() {
    if (!inputText.trim()) return;

    let convId = currentConversationId;
    if (!convId) {
      // 无对话时自动创建
      try {
        const conv = await invoke<{ id: string }>("create_conversation", { title: null });
        convId = conv.id;
        setCurrentConversationId(convId);
      } catch (e) {
        console.error("Failed to create conversation:", e);
        return;
      }
    }
    // 立即更新 ref，确保 setupEventListeners 拿到最新对话 ID
    currentConversationIdRef.current = convId;

    const content = inputText.trim();
    const requestId = createChatRunId();
    activeRunIdRef.current = requestId;
    setInputText("");
    setIsLoading(true);
    setStreamingText("");
    // 立即反馈并乐观显示消息，避免发送前重新拉取全部历史。
    setThinking(true);
    setMessages((prev) => [
      ...prev,
      {
        id: -Date.now(),
        session_id: convId,
        role: "user",
        content,
        timestamp: new Date().toISOString(),
      },
    ]);
    setToolExecutions([]);
    setShowChat(true);

    try {
      const acceptedRunId = await invoke<string>("send_message", {
        conversationId: convId,
        content,
        requestId,
      });
      if (activeRunIdRef.current === requestId) {
        activeRunIdRef.current = acceptedRunId;
      }
    } catch (e) {
      console.error("Failed to send message:", humanizeError(e, "发送失败"));
      if (activeRunIdRef.current === requestId) {
        activeRunIdRef.current = null;
        setIsLoading(false);
        setThinking(false);
      }
    }
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSendMessage();
    }
  }

  async function handleClose() {
    try {
      await invoke("close_mini_window");
    } catch (e) {
      console.error("Failed to close mini window:", e);
    }
  }

  async function handleExpand() {
    try {
      await invoke("show_main_window");
      await invoke("close_mini_window");
    } catch (e) {
      console.error("Failed to expand to main window:", e);
    }
  }

  function toggleToolCollapse(id: string) {
    setToolExecutions((prev) =>
      prev.map((t) => (t.id === id ? { ...t, collapsed: !t.collapsed } : t))
    );
  }

  // ---- 渲染 ----
  // 纯头像模式：默认只显示动态头像，完全透明背景
  // 点击头像展开聊天面板，鼠标悬停显示控制按钮

  return (
    <div className="ds mini-window group relative flex h-screen w-screen flex-col overflow-hidden bg-transparent text-[var(--color-foreground)]">
      {/* 悬浮控制按钮 —— 默认隐藏，鼠标悬停淡入 */}
      <div className="pointer-events-none absolute right-2 top-2 z-30 flex gap-1.5 opacity-0 transition-opacity duration-300 group-hover:pointer-events-auto group-hover:opacity-100">
        <FloatBtn onClick={handleExpand} title="展开主窗口">
          <Maximize2 className="size-3.5" />
        </FloatBtn>
        <FloatBtn onClick={handleClose} title="关闭浮窗" danger>
          <X className="size-3.5" />
        </FloatBtn>
      </div>

      {/* 动态人像区域 —— 原生 mousedown 调用 startDragging，onClick 处理展开/收起 */}
      <div
        ref={avatarAreaRef}
        data-tauri-drag-region
        onClick={() => setShowChat((prev) => !prev)}
        title={showChat ? "点击收起 / 拖动移动" : "点击对话 / 拖动移动"}
        className={cn(
          "relative flex cursor-grab items-center justify-center transition-transform duration-200",
          // 分层窗口中完全透明的区域鼠标事件会穿透，这里保留一点底色来接收事件
          "bg-[rgba(13,12,11,0.15)] hover:scale-[1.03] active:scale-[0.97]",
          showChat ? "h-[104px] shrink-0 pt-2" : "min-h-0 flex-1"
        )}
      >
        {!avatarError ? (
          <img
            src={AVATAR_SRC}
            alt="AI Assistant"
            className={cn(
              "select-none object-contain drop-shadow-[0_0_12px_rgba(255,140,0,0.28)]",
              showChat ? "h-[90px] w-auto max-w-[90%]" : "max-h-[90%] max-w-[90%]",
              (thinking || isLoading) && "mini-avatar-glow"
            )}
            onError={() => setAvatarError(true)}
            draggable={false}
          />
        ) : (
          <div className="flex size-28 items-center justify-center rounded-full bg-[radial-gradient(circle,rgba(255,140,0,0.14),transparent_70%)]">
            <ShieldCheck className="size-11 text-[var(--color-brand)] drop-shadow-[0_0_10px_rgba(255,140,0,0.5)]" />
          </div>
        )}

        {/* 思考/说话状态指示器 */}
        {(thinking || isLoading) && (
          <div className="absolute bottom-2.5 left-1/2 -translate-x-1/2">
            <span className="block size-2 animate-[ds-pulse-dot_1.2s_ease-in-out_infinite] rounded-full bg-[var(--color-brand)] shadow-[0_0_10px_var(--color-brand)]" />
          </div>
        )}
      </div>

      {/* 展开的聊天面板 —— 点击头像后显示 */}
      {showChat && (
        <div className="flex min-h-0 flex-1 animate-ds-rise flex-col overflow-hidden rounded-t-[var(--radius-xl)] border border-b-0 border-[var(--color-line-strong)] bg-[var(--color-background)]/95 shadow-[var(--shadow-lg)] backdrop-blur-xl">
          {/* 消息区域 */}
          <div className="min-h-0 flex-1 space-y-2 overflow-y-auto px-3 py-2.5">
            {messages.length === 0 && !streamingText && toolExecutions.length === 0 && (
              <div className="flex h-full items-center justify-center">
                <p className="text-[var(--text-xs)] text-[var(--color-subtle-foreground)]">
                  有什么可以帮你的？
                </p>
              </div>
            )}

            {messages.map((msg) => (
              <MiniMessageBubble key={msg.id} message={msg} />
            ))}

            {isLoading && streamingText && (
              <div className="flex justify-start">
                <div className="max-w-[92%] rounded-[var(--radius-lg)] border border-[var(--color-line)] bg-[var(--color-elevated)] px-2.5 py-2">
                  <div className="whitespace-pre-wrap break-words text-[var(--text-xs)] leading-relaxed text-[var(--color-foreground)]">
                    {streamingText}
                    <span className="ml-0.5 inline-block h-3 w-0.5 translate-y-0.5 animate-[ds-pulse-dot_1s_ease-in-out_infinite] bg-[var(--color-brand)]" />
                  </div>
                </div>
              </div>
            )}

            {isLoading && thinking && !streamingText && (
              <div className="flex justify-start">
                <div className="flex items-center gap-1.5 rounded-[var(--radius-lg)] border border-[var(--color-line)] bg-[var(--color-elevated)] px-2.5 py-2">
                  {[0, 1, 2].map((i) => (
                    <span
                      key={i}
                      className="mini-dot size-1.5 rounded-full bg-[var(--color-brand)]"
                      style={{ animationDelay: `${i * 160}ms` }}
                    />
                  ))}
                  <span className="ml-1 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                    思考中
                  </span>
                </div>
              </div>
            )}

            {toolExecutions.map((tool) => (
              <MiniToolCard
                key={tool.id}
                tool={tool}
                onToggle={() => toggleToolCollapse(tool.id)}
              />
            ))}

            <div ref={messagesEndRef} />
          </div>

          {/* 输入区域 */}
          <div className="flex shrink-0 items-end gap-1.5 border-t border-[var(--color-line)] bg-[var(--color-surface)]/60 px-2.5 py-2">
            <textarea
              ref={inputRef}
              className={cn(
                "max-h-24 min-h-8 flex-1 resize-none rounded-[var(--radius-sm)] border border-[var(--color-line)]",
                "bg-[var(--color-surface)] px-2 py-1.5 text-[var(--text-xs)] leading-relaxed text-[var(--color-foreground)]",
                "placeholder:text-[var(--color-subtle-foreground)]",
                "transition-[border-color,box-shadow] duration-150",
                "hover:border-[var(--color-line-strong)] focus:border-[var(--color-brand-line)]",
                "focus:shadow-[0_0_0_3px_var(--color-brand-soft)] focus:outline-none"
              )}
              placeholder="输入消息…"
              value={inputText}
              onChange={(e) => setInputText(e.target.value)}
              onKeyDown={handleKeyDown}
              rows={1}
            />
            <button
              onClick={handleSendMessage}
              disabled={!inputText.trim()}
              title={isLoading ? "发送新指令并取消上一条" : "发送"}
              className={cn(
                "inline-flex size-8 shrink-0 cursor-pointer items-center justify-center rounded-[var(--radius-sm)]",
                "bg-[var(--color-brand)] text-[var(--color-brand-foreground)]",
                "shadow-[inset_0_1px_0_rgba(255,255,255,.22)] transition-all duration-150",
                "hover:brightness-110 active:translate-y-px",
                "disabled:pointer-events-none disabled:opacity-40"
              )}
            >
              <Send className="size-3.5" />
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

/* ---------------------------------------------------------------- 小组件 */

function FloatBtn({
  children,
  onClick,
  title,
  danger,
}: {
  children: React.ReactNode;
  onClick: () => void;
  title: string;
  danger?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      className={cn(
        "inline-flex size-7 cursor-pointer items-center justify-center rounded-[var(--radius-sm)]",
        "border border-[var(--color-line-strong)] bg-[var(--color-canvas)]/72 backdrop-blur-md",
        "text-[var(--color-muted-foreground)] transition-colors duration-150",
        danger
          ? "hover:border-[var(--color-danger)]/50 hover:bg-[var(--color-danger)]/15 hover:text-[var(--color-danger)]"
          : "hover:border-[var(--color-brand-line)] hover:text-[var(--color-brand)]"
      )}
    >
      {children}
    </button>
  );
}

// ---- 迷你消息气泡 ----

function MiniMessageBubble({ message }: { message: ConversationMessage }) {
  const isUser = message.role === "user";
  const isTool = message.role === "tool";

  if (isTool) {
    return (
      <div className="rounded-[var(--radius-md)] border border-[var(--color-line)] bg-[var(--color-card)] px-2.5 py-1.5">
        <span className="label-caps">工具结果</span>
        <pre className="mt-1 max-h-32 overflow-auto whitespace-pre-wrap break-all font-mono text-[var(--text-2xs)] leading-relaxed text-[var(--color-muted-foreground)]">
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
    );
  }

  return (
    <div className={cn("flex", isUser ? "justify-end" : "justify-start")}>
      <div
        className={cn(
          "max-w-[92%] rounded-[var(--radius-lg)] px-2.5 py-2",
          "text-[var(--text-xs)] leading-relaxed whitespace-pre-wrap break-words",
          isUser
            ? "bg-[var(--color-brand-soft)] text-[var(--color-foreground)] border border-[var(--color-brand-line)]"
            : "bg-[var(--color-elevated)] text-[var(--color-foreground)] border border-[var(--color-line)]"
        )}
      >
        {message.content.split("\n").map((line, i, arr) => (
          <span key={i}>
            {line}
            {i < arr.length - 1 && <br />}
          </span>
        ))}
      </div>
    </div>
  );
}

// ---- 迷你工具卡片 ----

function MiniToolCard({
  tool,
  onToggle,
}: {
  tool: ToolExecution;
  onToggle: () => void;
}) {
  const running = tool.status === "running";

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
      return JSON.stringify(parsed, null, 2);
    } catch {
      return tool.result;
    }
  })();

  return (
    <div
      className={cn(
        "overflow-hidden rounded-[var(--radius-md)] border bg-[var(--color-card)]",
        running
          ? "border-[var(--color-brand-line)]"
          : tool.status === "success"
          ? "border-[var(--color-success)]/25"
          : "border-[var(--color-danger)]/30"
      )}
    >
      <button
        onClick={onToggle}
        className="flex w-full cursor-pointer items-center gap-1.5 px-2 py-1.5 text-left transition-colors hover:bg-[var(--color-hover)]/50"
      >
        {running ? (
          <Loader2 className="size-3 shrink-0 animate-ds-spin text-[var(--color-brand)]" />
        ) : tool.status === "success" ? (
          <CheckCircle2 className="size-3 shrink-0 text-[var(--color-success)]" />
        ) : (
          <XCircle className="size-3 shrink-0 text-[var(--color-danger)]" />
        )}
        <span className="min-w-0 flex-1 truncate font-mono text-[var(--text-2xs)] text-[var(--color-foreground)]">
          {tool.name}
        </span>
        <span
          className={cn(
            "shrink-0 text-[var(--text-2xs)]",
            running
              ? "text-[var(--color-brand)]"
              : tool.status === "success"
              ? "text-[var(--color-success)]"
              : "text-[var(--color-danger)]"
          )}
        >
          {running ? "执行中" : tool.status === "success" ? "成功" : "失败"}
        </span>
        {tool.collapsed ? (
          <ChevronRight className="size-3 shrink-0 text-[var(--color-subtle-foreground)]" />
        ) : (
          <ChevronDown className="size-3 shrink-0 text-[var(--color-subtle-foreground)]" />
        )}
      </button>

      {!tool.collapsed && (
        <div className="border-t border-[var(--color-line-faint)] px-2 py-1.5">
          <span className="label-caps">参数</span>
          <pre className="mt-1 max-h-24 overflow-auto whitespace-pre-wrap break-all font-mono text-[var(--text-2xs)] leading-relaxed text-[var(--color-subtle-foreground)]">
            {displayArgs}
          </pre>

          {!running && displayResult && (
            <>
              <span className="label-caps mt-2 block">结果</span>
              <pre
                className={cn(
                  "mt-1 max-h-28 overflow-auto whitespace-pre-wrap break-all font-mono text-[var(--text-2xs)] leading-relaxed",
                  tool.status === "error"
                    ? "text-[var(--color-danger)]"
                    : "text-[var(--color-muted-foreground)]"
                )}
              >
                {displayResult}
              </pre>
            </>
          )}

          {running && (
            <div className="mt-1.5 flex items-center gap-1.5 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
              <Loader2 className="size-2.5 animate-ds-spin" />
              正在执行
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default MiniChat;
