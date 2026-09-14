import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import CyberIcon from "../components/CyberIcon";

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
                result: data.content,
              };
              return updated;
            });
            setIsLoading(false);
            setThinking(false);
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
        }
      }],
      ["chat:error", (payload) => {
        setIsLoading(false);
        setThinking(false);
        console.error("Chat error:", payload);
      }],
    ];

    for (const [eventName, handler] of listeners) {
      const unlisten = await listen(eventName, (event) => {
        const payloadStr = String(event.payload);
        // session_id 过滤——防止收到主窗口或其他对话的事件
        const cid = currentConversationIdRef.current;
        try {
          const parsed = JSON.parse(payloadStr);
          if (cid && (!parsed.session_id || parsed.session_id !== cid)) {
            return;
          }
        } catch {
          if (cid) return;
        }
        handler(payloadStr);
      });
      unlistenRef.current.push(unlisten);
    }
  }, []);

  // ---- 操作 ----

  async function handleSendMessage() {
    if (!inputText.trim() || isLoading) return;

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
    setInputText("");
    setIsLoading(true);
    setStreamingText("");
    setThinking(false);
    setToolExecutions([]);
    setShowChat(true);

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
    <div className="mini-chat transparent">
      {/* 悬浮控制按钮 —— 默认隐藏，鼠标悬停淡入 */}
      <div className="mini-float-controls">
        <button className="mini-float-btn expand" onClick={handleExpand} title="展开主窗口">
          <CyberIcon name="expand" size={14} />
        </button>
        <button className="mini-float-btn close" onClick={handleClose} title="关闭浮窗">
          <CyberIcon name="close" size={14} />
        </button>
      </div>

      {/* 动态人像区域 —— 原生 mousedown 调用 startDragging，onClick 处理展开/收起 */}
      <div
        ref={avatarAreaRef}
        className={`mini-avatar-area ${showChat ? "chat-open" : ""}`}
        data-tauri-drag-region
        onClick={() => setShowChat((prev) => !prev)}
        title={showChat ? "点击收起 / 拖动移动" : "点击对话 / 拖动移动"}
      >
        {!avatarError ? (
          <img
            src={AVATAR_SRC}
            alt="AI Assistant"
            className={`mini-avatar ${thinking || isLoading ? "speaking" : ""}`}
            onError={() => setAvatarError(true)}
            draggable={false}
          />
        ) : (
          <div className="mini-avatar-fallback">
            <span className="fallback-icon"><CyberIcon name="shield" size={28} /></span>
          </div>
        )}

        {/* 思考/说话状态指示器 */}
        {(thinking || isLoading) && (
          <div className="mini-status-ring">
            <span className="ring-pulse" />
          </div>
        )}
      </div>

      {/* 展开的聊天面板 —— 点击头像后显示 */}
      {showChat && (
        <div className="mini-chat-panel">
          {/* 消息区域 */}
          <div className="mini-messages">
            {messages.length === 0 && !streamingText && toolExecutions.length === 0 && (
              <div className="mini-welcome">
                <p>有什么可以帮你的？</p>
              </div>
            )}

            {messages.map((msg) => (
              <MiniMessageBubble key={msg.id} message={msg} />
            ))}

            {isLoading && streamingText && (
              <div className="mini-bubble assistant">
                <div className="mini-bubble-content">{streamingText}</div>
                <div className="message-cursor" />
              </div>
            )}

            {isLoading && thinking && !streamingText && (
              <div className="mini-bubble assistant">
                <div className="thinking-indicator">
                  <span className="dot" />
                  <span className="dot" />
                  <span className="dot" />
                  <span>思考中...</span>
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
          <div className="mini-input-area">
            <textarea
              ref={inputRef}
              className="mini-input"
              placeholder="输入消息..."
              value={inputText}
              onChange={(e) => setInputText(e.target.value)}
              onKeyDown={handleKeyDown}
              disabled={isLoading}
              rows={1}
            />
            <button
              className="mini-send-btn"
              onClick={handleSendMessage}
              disabled={!inputText.trim() || isLoading}
            >
              {isLoading ? <span className="btn-spinner" /> : <CyberIcon name="send" size={16} />}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

// ---- 迷你消息气泡 ----

function MiniMessageBubble({ message }: { message: ConversationMessage }) {
  const isUser = message.role === "user";
  const isTool = message.role === "tool";

  if (isTool) {
    return (
      <div className="mini-bubble tool">
        <div className="mini-tool-result">
          <span className="mini-tool-label">工具结果</span>
          <pre className="mini-tool-code">
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
    <div className={`mini-bubble ${isUser ? "user" : "assistant"}`}>
      <div className="mini-bubble-content">
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
  const statusIcon =
    tool.status === "running" ? <CyberIcon name="clock" size={14} /> :
    tool.status === "success" ? <CyberIcon name="check" size={14} /> : <CyberIcon name="xCircle" size={14} />;

  const statusText =
    tool.status === "running" ? "执行中..." :
    tool.status === "success" ? "成功" : "失败";

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
    <div className={`mini-tool-card ${tool.status}`}>
      <div className="mini-tool-header" onClick={onToggle}>
        <span>{statusIcon}</span>
        <span className="mini-tool-name">{tool.name}</span>
        <span className="mini-tool-status">{statusText}</span>
        <span>{tool.collapsed ? <CyberIcon name="chevronRight" size={14} /> : <CyberIcon name="chevronDown" size={14} />}</span>
      </div>

      {!tool.collapsed && (
        <div className="mini-tool-body">
          <div className="mini-tool-section">
            <span className="mini-section-label">参数</span>
            <pre className="mini-tool-code">{displayArgs}</pre>
          </div>

          {tool.status !== "running" && displayResult && (
            <div className="mini-tool-section">
              <span className="mini-section-label">结果</span>
              <pre className={`mini-tool-code ${tool.status}`}>{displayResult}</pre>
            </div>
          )}

          {tool.status === "running" && (
            <div className="mini-tool-running">
              <span className="tool-spinner" />
              <span>正在执行...</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default MiniChat;
