import { useState, useRef, useEffect, useCallback, useMemo } from 'react';
import { useLocation } from 'react-router-dom';
import { motion, AnimatePresence } from 'motion/react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import type { Components } from 'react-markdown';
import {
  MessageSquare, Send, Plus, Trash2, Loader2, Mic, Copy, Check,
  PanelLeft, PanelLeftClose, Cpu, Terminal, FileText, Search, Globe,
  CheckCircle, XCircle, Sparkles, ChevronRight, Bot, Volume2, VolumeX, Phone, PhoneOff,
  Square, RefreshCw, Pencil,
} from 'lucide-react';
import { EAssistantPageShell } from '../../components/e-assistant/EAssistantPageShell';
import { cn } from '../../lib/cn';
import { useVoice, type VoiceStatus } from '../../hooks/useVoice';

/* ──────────────────────────────────────────────
   类型定义
   ────────────────────────────────────────────── */
interface Conversation {
  id: string;
  title: string;
  messageCount: number;
  lastActiveAt: string;
  preview: string;
}

interface StoredConversation {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  last_message?: string | null;
}

interface ToolCall {
  id: string;
  name: string;
  arguments?: Record<string, unknown>;
  status: 'running' | 'done' | 'error';
  result?: string;
  error?: string;
}

interface ChatMessage {
  id: string;
  role: 'user' | 'assistant';
  content: string;
  timestamp: string;
  toolCalls?: ToolCall[];
  /** AI 是否正在思考（尚未产出内容） */
  thinking?: boolean;
}

/* ── Tauri 流式事件 payload（payload 为 JSON 字符串，需 parse） ── */
interface ChatStreamPayload {
  type?: string;
  data?: unknown;
  session_id?: string;
  run_id?: string;
}

interface ToolCallEventData {
  name?: string;
  arguments?: Record<string, unknown>;
}

interface ToolResultEventData {
  name?: string;
  success?: boolean;
  content?: string;
}

interface HandoffPacket {
  handoffId: string;
  taskId: string;
  capability: string;
  userGoal: string;
  conversationSummary: string;
  requiresConfirmationForWrite: boolean;
  degradedFallback?: boolean;
}

interface HandoffRecommendation {
  capability: string;
  owner: 'jarvis' | 'ollama';
  handoff_to_ollama: boolean;
  user_goal: string;
  context_summary: string;
  requires_confirmation_for_write: boolean;
  degraded_fallback?: boolean;
}

interface HandoffPlan {
  recommendation: HandoffRecommendation;
  packet: HandoffPacket | null;
  executionStarted: boolean;
}

interface PendingHandoff {
  packet: HandoffPacket;
  recommendation: HandoffRecommendation;
  originalMessage: string;
}

interface HandoffStatePayload {
  state: 'ollama_starting' | 'ollama_unloading' | 'jarvis_starting' | 'jarvis_active' | 'jarvis_restore_failed';
  error?: string;
}

/* ──────────────────────────────────────────────
   工具函数
   ────────────────────────────────────────────── */
const TOOL_ICONS: Record<string, React.ReactNode> = {
  read_file: <FileText size={12} />,
  write_file: <FileText size={12} />,
  search: <Search size={12} />,
  web: <Globe size={12} />,
  command: <Terminal size={12} />,
  shell: <Terminal size={12} />,
};

function getToolIcon(name: string): React.ReactNode {
  for (const [key, icon] of Object.entries(TOOL_ICONS)) {
    if (name.toLowerCase().includes(key)) return icon;
  }
  return <Cpu size={12} />;
}

function formatTime(ts: string): string {
  const d = new Date(ts);
  if (!ts || Number.isNaN(d.getTime())) return '';
  return d.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit' });
}

function formatDate(ts: string): string {
  const d = new Date(ts);
  if (!ts || Number.isNaN(d.getTime())) return '';
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const day = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const daysAgo = Math.round((today - day) / 86400000);
  if (daysAgo === 0) return '今天';
  if (daysAgo === 1) return '昨天';
  return d.toLocaleDateString('zh-CN', { month: 'short', day: 'numeric' });
}

function truncate(s: string, n: number): string {
  if (!s) return '';
  return s.length > n ? s.slice(0, n) + '…' : s;
}

/** 工具参数摘要：取第一个键值对，过长截断（参考 EAssistantCore ExecutionStream） */
function formatArgs(args?: Record<string, unknown>): string {
  if (!args) return '';
  const entries = Object.entries(args);
  if (entries.length === 0) return '';
  const [k, v] = entries[0];
  const val = typeof v === 'string' ? v : JSON.stringify(v);
  const label = k.replace(/_/g, ' ');
  return val.length > 60 ? `${label}: ${val.slice(0, 60)}…` : `${label}: ${val}`;
}

/* ──────────────────────────────────────────────
   Markdown 渲染配置
   ────────────────────────────────────────────── */
const markdownComponents: Components = {
  code({ className, children, ...props }) {
    const isBlock = /language-/.test(className || '');
    if (isBlock) {
      return (
        <code
          className="block overflow-x-auto rounded-lg bg-black/40 p-3 text-xs leading-relaxed text-cyan-100/90"
          {...props}
        >
          {children}
        </code>
      );
    }
    return (
      <code
        className="rounded bg-white/10 px-1.5 py-0.5 text-[0.85em] text-celestial-saturn"
        {...props}
      >
        {children}
      </code>
    );
  },
  pre({ children }) {
    return <div className="my-2 overflow-hidden">{children}</div>;
  },
  a({ href, children }) {
    return (
      <a
        href={href}
        target="_blank"
        rel="noreferrer"
        className="text-celestial-saturn underline underline-offset-2 hover:text-celestial-saturn/80"
      >
        {children}
      </a>
    );
  },
  ul({ children }) {
    return <ul className="my-2 list-disc space-y-1 pl-5">{children}</ul>;
  },
  ol({ children }) {
    return <ol className="my-2 list-decimal space-y-1 pl-5">{children}</ol>;
  },
  blockquote({ children }) {
    return (
      <blockquote className="my-2 border-l-2 border-celestial-saturn/40 pl-3 text-white/60 italic">
        {children}
      </blockquote>
    );
  },
  table({ children }) {
    return (
      <div className="my-2 overflow-x-auto">
        <table className="w-full text-xs">{children}</table>
      </div>
    );
  },
  th({ children }) {
    return <th className="border border-white/10 bg-white/5 px-2 py-1 text-left font-bold">{children}</th>;
  },
  td({ children }) {
    return <td className="border border-white/10 px-2 py-1">{children}</td>;
  },
};

/* ──────────────────────────────────────────────
   快捷建议
   ────────────────────────────────────────────── */
const SUGGESTIONS = [
  { label: '每天上午九点给我一份系统简报', prompt: '每天上午九点检查这台电脑的运行状态，并把简短的系统简报发给我。' },
  { label: 'CPU 持续过高时提醒我', prompt: '创建一条主动规则：CPU 使用率持续 30 秒超过 90% 时通知我。' },
  { label: '帮我清理桌面文件', prompt: '帮我整理一下桌面上的文件，按类型分类到对应文件夹。' },
  { label: '总结当前系统状态', prompt: '帮我看看当前电脑的运行状态，给出优化建议。' },
];

/* ──────────────────────────────────────────────
   主组件
   ────────────────────────────────────────────── */
export default function EAssistantChat() {
  const location = useLocation();
  const voiceText = (location.state as { voiceText?: string } | null)?.voiceText;

  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [activeConvId, setActiveConvId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState('');
  const [isSending, setIsSending] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [expandedTools, setExpandedTools] = useState<Record<string, boolean>>({});
  const [autoRead, setAutoRead] = useState(false);
  // 哪条消息正在被朗读（用于按钮图标切换；语音引擎是全局单例，同时只念一条）
  const [speakingMsgId, setSpeakingMsgId] = useState<string | null>(null);
  const [editingMsgId, setEditingMsgId] = useState<string | null>(null);
  const [editingText, setEditingText] = useState('');
  const [pendingHandoff, setPendingHandoff] = useState<PendingHandoff | null>(null);
  const [handoffState, setHandoffState] = useState<HandoffStatePayload | null>(null);

  const scrollRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const voiceConsumedRef = useRef(false);

  /* ── 流式会话追踪 refs（事件监听器只注册一次，用 ref 避免闭包陷阱） ── */
  const activeConvIdRef = useRef<string | null>(null);
  const currentRunIdRef = useRef<string | null>(null);
  const streamingMsgIdRef = useRef<string | null>(null);
  const isSendingRef = useRef(false);
  const streamingTextRef = useRef('');

  // 每次渲染同步 ref，保证事件回调读到最新值
  activeConvIdRef.current = activeConvId;
  isSendingRef.current = isSending;

  /* ── 语音交互：识别 + TTS + 实时对话模式 ── */
  const handleSendRef = useRef<(text?: string) => void>(() => undefined);
  const voice = useVoice({
    // 实时对话模式下，hook 把识别到的整句文本交给这里发送
    onSendMessage: (text) => {
      handleSendRef.current(text);
    },
  });
  const autoReadRef = useRef(autoRead);
  autoReadRef.current = autoRead;
  const voiceModeRef = useRef(voice.isVoiceMode);
  voiceModeRef.current = voice.isVoiceMode;
  // 用 ref 暴露 voice API，供只注册一次的事件监听器调用 speak
  const voiceApiRef = useRef(voice);
  voiceApiRef.current = voice;

  // 朗读结束（自然念完或被停止）后清掉高亮的那条消息
  useEffect(() => {
    if (!voice.isSpeaking) setSpeakingMsgId(null);
  }, [voice.isSpeaking]);

  /* ── 语音文本接收 ── */
  useEffect(() => {
    if (voiceText && !voiceConsumedRef.current) {
      voiceConsumedRef.current = true;
      setInput(voiceText);
      textareaRef.current?.focus();
    }
  }, [voiceText]);

  /* ── 自动滚动到底部 ── */
  const scrollToBottom = useCallback(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, []);

  useEffect(() => {
    scrollToBottom();
  }, [messages, isSending, scrollToBottom]);

  /* ── 加载会话列表 ── */
  const loadConversations = useCallback(async () => {
    try {
      const convs = await invoke<StoredConversation[]>('get_conversations');
      setConversations((convs || []).map((conv) => ({
        id: conv.id,
        title: conv.title,
        messageCount: conv.message_count ?? 0,
        lastActiveAt: conv.updated_at || conv.created_at,
        preview: conv.last_message || '',
      })));
    } catch {
      setConversations([]);
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    loadConversations();
  }, [loadConversations]);

  /* ── 加载消息 ── */
  const loadMessages = useCallback(async (convId: string) => {
    try {
      const msgs = await invoke<ChatMessage[]>('get_messages', { conversationId: convId });
      setMessages(msgs || []);
    } catch {
      setMessages([]);
    }
  }, []);

  const selectConversation = useCallback((convId: string) => {
    activeConvIdRef.current = convId;
    setActiveConvId(convId);
    setMessages([]);
    setEditingMsgId(null);
    loadMessages(convId);
  }, [loadMessages]);

  /* ── 新建会话 ── */
  const handleNewConversation = useCallback(() => {
    activeConvIdRef.current = null;
    setActiveConvId(null);
    setMessages([]);
    setEditingMsgId(null);
    textareaRef.current?.focus();
  }, []);

  /* ── 删除会话 ── */
  const handleDeleteConversation = useCallback(async (convId: string, e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await invoke('delete_conversation', { id: convId });
    } catch {
      /* 忽略后端错误 */
    }
    setConversations(prev => prev.filter(c => c.id !== convId));
    if (activeConvId === convId) {
      activeConvIdRef.current = null;
      setActiveConvId(null);
      setMessages([]);
    }
  }, [activeConvId]);

  /* ──────────────────────────────────────────────
     流式核心：更新"正在生成的 AI 消息"
     ────────────────────────────────────────────── */
  const updateStreamingMessage = useCallback((updater: (msg: ChatMessage) => ChatMessage) => {
    const id = streamingMsgIdRef.current;
    if (!id) return;
    setMessages(prev => prev.map(m => (m.id === id ? updater(m) : m)));
  }, []);

  const finishStream = useCallback(() => {
    setIsSending(false);
    isSendingRef.current = false;
    streamingMsgIdRef.current = null;
    currentRunIdRef.current = null;
  }, []);

  /** 创建空 AI 气泡并发起 send_message，事件流负责填充内容 */
  const runAssistant = useCallback(async (text: string) => {
    const convId = activeConvIdRef.current;
    if (!convId) return;

    const aiMsgId = `ai-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
    setMessages(prev => [
      ...prev,
      {
        id: aiMsgId,
        role: 'assistant',
        content: '',
        timestamp: new Date().toISOString(),
        toolCalls: [],
        thinking: true,
      },
    ]);
    streamingMsgIdRef.current = aiMsgId;
    streamingTextRef.current = '';
    setIsSending(true);
    isSendingRef.current = true;

    try {
      // 主对话由 EAssistant 负责语言、视觉与感知呈现；返回 run_id，内容仍由
      // 同一套 Tauri Event 流填充。工具型任务将在串行交接器中转给 Ollama。
      const runId = await invoke<string>('send_jarvis_message', {
        conversationId: convId,
        content: text,
        requestId: null,
      });
      currentRunIdRef.current = runId;
    } catch {
      const errText = '抱歉，请求发送失败，请检查连接后重试。';
      updateStreamingMessage(m => ({ ...m, thinking: false, content: errText }));
      finishStream();
      loadConversations();
      // 实时对话模式下也要把错误读出来，否则状态机会卡住
      if (voiceModeRef.current) voiceApiRef.current.speak(errText);
    }
  }, [finishStream, loadConversations, updateStreamingMessage]);

  /** EAssistant 先判断职责；需要工具执行时只展示已签发的交接包，等待用户确认。 */
  const planAndRunAssistant = useCallback(async (text: string) => {
    const convId = activeConvIdRef.current;
    if (!convId) return;
    setIsSending(true);
    isSendingRef.current = true;
    setHandoffState(null);

    try {
      const plan = await invoke<HandoffPlan>('jarvis_recommend_handoff', {
        message: text,
        observationSummary: null,
        taskId: null,
      });
      if (plan.packet) {
        if (!plan.recommendation.requires_confirmation_for_write) {
          const aiMsgId = `ai-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
          setMessages(prev => [
            ...prev,
            {
              id: aiMsgId,
              role: 'assistant',
              content: '',
              timestamp: new Date().toISOString(),
              toolCalls: [],
              thinking: true,
            },
          ]);
          streamingMsgIdRef.current = aiMsgId;
          streamingTextRef.current = '';
          setHandoffState({ state: 'ollama_starting' });
          try {
            const runId = await invoke<string>('execute_ollama_handoff', {
              packet: plan.packet,
              conversationId: convId,
              requestId: null,
            });
            currentRunIdRef.current = runId;
          } catch (error) {
            const detail = error instanceof Error ? error.message : String(error);
            updateStreamingMessage(message => ({
              ...message,
              thinking: false,
              content: `⚠️ 无法启动只读任务：${detail}`,
            }));
            finishStream();
            setHandoffState({ state: 'jarvis_restore_failed', error: detail });
          }
          return;
        }
        setPendingHandoff({
          packet: plan.packet,
          recommendation: plan.recommendation,
          originalMessage: text,
        });
        setMessages(prev => [
          ...prev,
          {
            id: `handoff-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
            role: 'assistant',
            content: plan.recommendation.degraded_fallback
              ? `E-Assisant 当前无法完成职责判断，已由本地安全路由器准备降级到 Ollama：**${plan.recommendation.user_goal}**。请确认后执行，完成后会自动尝试恢复 E-Assisant。`
              : `我已理解这项任务。它需要交给 Ollama 执行：**${plan.recommendation.user_goal}**。请确认后我会暂时释放 E-Assisant 的显存，执行完成后自动恢复。`,
            timestamp: new Date().toISOString(),
          },
        ]);
        finishStream();
        return;
      }
      await runAssistant(text);
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      setMessages(prev => [
        ...prev,
        {
          id: `plan-error-${Date.now()}`,
          role: 'assistant',
          content: `⚠️ E-Assisant 暂时无法判断此请求的处理方式：${detail}`,
          timestamp: new Date().toISOString(),
        },
      ]);
      finishStream();
    }
  }, [finishStream, runAssistant, updateStreamingMessage]);

  const confirmHandoff = useCallback(async () => {
    const pending = pendingHandoff;
    const convId = activeConvIdRef.current;
    if (!pending || !convId || isSendingRef.current) return;

    const aiMsgId = `ai-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
    setMessages(prev => [
      ...prev,
      {
        id: aiMsgId,
        role: 'assistant',
        content: '',
        timestamp: new Date().toISOString(),
        toolCalls: [],
        thinking: true,
      },
    ]);
    streamingMsgIdRef.current = aiMsgId;
    streamingTextRef.current = '';
    setIsSending(true);
    isSendingRef.current = true;
    setHandoffState({ state: 'ollama_starting' });
    setPendingHandoff(null);

    try {
      const runId = await invoke<string>('execute_ollama_handoff', {
        packet: pending.packet,
        conversationId: convId,
        requestId: null,
      });
      currentRunIdRef.current = runId;
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      updateStreamingMessage(m => ({
        ...m,
        thinking: false,
        content: `⚠️ 无法启动 Ollama 交接：${detail}`,
      }));
      finishStream();
      setHandoffState({ state: 'jarvis_restore_failed', error: detail });
    }
  }, [finishStream, pendingHandoff, updateStreamingMessage]);

  /* ── 发送消息 ── */
  const handleSend = useCallback(async (textOverride?: string) => {
    const text = (textOverride ?? input).trim();
    if (!text || isSendingRef.current || pendingHandoff) return;
    if (!textOverride) setInput('');

    // 确保有会话
    let convId = activeConvIdRef.current;
    if (!convId) {
      try {
        const res = await invoke<{ id: string }>('create_conversation', { title: text.slice(0, 20) });
        convId = res.id;
      } catch {
        convId = `local-${Date.now()}`;
      }
      activeConvIdRef.current = convId;
      setActiveConvId(convId);
    }

    // 本地追加用户消息
    const userMsg: ChatMessage = {
      id: `user-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
      role: 'user',
      content: text,
      timestamp: new Date().toISOString(),
    };
    setMessages(prev => [...prev, userMsg]);

    // EAssistant 先决定职责；需要工具时先向用户展示交接确认。
    await planAndRunAssistant(text);
  }, [input, pendingHandoff, planAndRunAssistant]);
  handleSendRef.current = handleSend;

  /* ── 中断生成 ── */
  const handleCancel = useCallback(async () => {
    if (pendingHandoff) {
      setPendingHandoff(null);
      setIsSending(false);
      isSendingRef.current = false;
      return;
    }
    const convId = activeConvIdRef.current;
    if (!convId) return;
    try {
      await invoke('cancel_task', { conversationId: convId });
    } catch {
      /* 忽略 */
    }
    updateStreamingMessage(m => ({
      ...m,
      thinking: false,
      content: m.content || '（已停止生成）',
    }));
    finishStream();
  }, [finishStream, pendingHandoff, updateStreamingMessage]);

  /* ── 重新生成：丢弃末尾 AI 消息，重发最后一条用户消息 ── */
  const handleRegenerate = useCallback(() => {
    if (isSendingRef.current) return;
    if (!activeConvIdRef.current) return;
    const lastUser = [...messages].reverse().find(m => m.role === 'user');
    if (!lastUser) return;
    setMessages(prev => {
      const arr = [...prev];
      while (arr.length && arr[arr.length - 1].role === 'assistant') arr.pop();
      return arr;
    });
    planAndRunAssistant(lastUser.content);
  }, [messages, planAndRunAssistant]);

  /* ── 用户消息编辑重发 ── */
  const startEdit = useCallback((msg: ChatMessage) => {
    setEditingMsgId(msg.id);
    setEditingText(msg.content);
  }, []);

  const commitEdit = useCallback(() => {
    const id = editingMsgId;
    const text = editingText.trim();
    setEditingMsgId(null);
    if (!id || !text || isSendingRef.current) return;
    // 截断到该条用户消息（含），并更新内容
    setMessages(prev => {
      const idx = prev.findIndex(m => m.id === id);
      if (idx === -1) return prev;
      return prev.slice(0, idx + 1).map((m, i) => (i === idx ? { ...m, content: text } : m));
    });
    if (activeConvIdRef.current) planAndRunAssistant(text);
  }, [editingMsgId, editingText, planAndRunAssistant]);

  /* ── 键盘事件 ── */
  const handleKeyDown = useCallback((e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  }, [handleSend]);

  /* ── 复制消息 ── */
  const handleCopy = useCallback(async (content: string, id: string) => {
    try {
      await navigator.clipboard.writeText(content);
      setCopiedId(id);
      setTimeout(() => setCopiedId(null), 1500);
    } catch {
      /* 忽略 */
    }
  }, []);

  /* ── 工具调用展开切换 ── */
  const toggleTools = useCallback((msgId: string) => {
    setExpandedTools(prev => ({ ...prev, [msgId]: !prev[msgId] }));
  }, []);

  /* ── 普通语音模式：识别结束后把最终文本填入输入框 ── */
  useEffect(() => {
    if (!voice.isListening && voice.transcript && !voice.isVoiceMode) {
      setInput(prev => (prev ? prev.trimEnd() + ' ' : '') + voice.transcript);
      voice.clearTranscript();
      textareaRef.current?.focus();
    }
  }, [voice.isListening, voice.transcript, voice.isVoiceMode, voice]);

  /* ── 麦克风按钮：开始/停止单次语音识别 ── */
  const handleMicToggle = useCallback(() => {
    if (voice.isListening) voice.stopListening();
    else voice.startListening();
  }, [voice]);

  /* ── 语音状态对应的按钮样式 ── */
  const micStatusStyle: Record<VoiceStatus, string> = {
    idle: 'border-white/10 bg-white/5 text-white/45 hover:bg-white/10 hover:text-white/80',
    listening: 'border-red-500/60 bg-red-500/20 text-red-400 animate-pulse',
    thinking: 'border-cyan-400/60 bg-cyan-400/15 text-cyan-300',
    speaking: 'border-emerald-400/60 bg-emerald-400/15 text-emerald-300',
  };

  /* ── 串行模型交接状态：由 Rust 在 Ollama 结束后持续推送 ── */
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    listen<HandoffStatePayload>('handoff:state', (event) => {
      setHandoffState(event.payload);
    }).then(fn => {
      unlisten = fn;
    }).catch(err => console.error('Failed to listen for handoff state:', err));
    return () => unlisten?.();
  }, []);

  /* ──────────────────────────────────────────────
     Tauri 事件流监听（组件挂载时注册一次，卸载时清理）
     ────────────────────────────────────────────── */
  useEffect(() => {
    const handlers: Array<[string, (p: ChatStreamPayload) => void]> = [
      // AI 开始思考
      ['chat:thinking', (p) => {
        if (!currentRunIdRef.current && p.run_id) currentRunIdRef.current = p.run_id;
        updateStreamingMessage(m => ({ ...m, thinking: true }));
      }],
      // 流式文本片段：追加到当前 AI 消息
      ['chat:text', (p) => {
        const chunk = typeof p.data === 'string' ? p.data : '';
        if (!chunk) return;
        streamingTextRef.current += chunk;
        updateStreamingMessage(m => ({ ...m, thinking: false, content: m.content + chunk }));
      }],
      // 工具调用开始：追加一个 running 状态的 toolCall
      ['chat:tool_call', (p) => {
        const d = (p.data ?? {}) as ToolCallEventData;
        if (!d.name) return;
        const tc: ToolCall = {
          id: `tc-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
          name: d.name,
          arguments: d.arguments || {},
          status: 'running',
        };
        updateStreamingMessage(m => ({
          ...m,
          thinking: false,
          toolCalls: [...(m.toolCalls || []), tc],
        }));
      }],
      // 工具执行结果：反向匹配最近一次同名 running 调用，更新为 done/error
      ['chat:tool_result', (p) => {
        const d = (p.data ?? {}) as ToolResultEventData;
        if (!d.name) return;
        updateStreamingMessage(m => {
          const tcs = m.toolCalls ? [...m.toolCalls] : [];
          for (let i = tcs.length - 1; i >= 0; i--) {
            if (tcs[i].name === d.name && tcs[i].status === 'running') {
              tcs[i] = {
                ...tcs[i],
                status: d.success ? 'done' : 'error',
                result: d.success ? d.content : undefined,
                error: d.success ? undefined : d.content,
              };
              break;
            }
          }
          return { ...m, toolCalls: tcs };
        });
      }],
      // 生成完成：结束流式状态，刷新会话列表，按设置朗读完整回复
      ['chat:done', () => {
        const finalText = streamingTextRef.current;
        streamingTextRef.current = '';
        updateStreamingMessage(m => ({ ...m, thinking: false }));
        finishStream();
        loadConversations();
        if (finalText && (autoReadRef.current || voiceModeRef.current)) {
          voiceApiRef.current.speak(finalText);
        }
      }],
      // 出错：把错误信息追加到 AI 消息
      ['chat:error', (p) => {
        const errMsg = typeof p.data === 'string' ? p.data : '生成过程中出错';
        streamingTextRef.current = '';
        updateStreamingMessage(m => ({
          ...m,
          thinking: false,
          content: m.content
            ? m.content + `\n\n> ⚠️ 出错了：${errMsg}`
            : `⚠️ 出错了：${errMsg}`,
        }));
        finishStream();
        loadConversations();
        if (voiceModeRef.current) voiceApiRef.current.speak(`出错了：${errMsg}`);
      }],
    ];

    let unlistenFns: UnlistenFn[] = [];
    let cancelled = false;

    (async () => {
      const fns = await Promise.all(
        handlers.map(([eventName, handler]) =>
          listen(eventName, (event) => {
            let payload: ChatStreamPayload;
            try {
              payload = JSON.parse(String(event.payload)) as ChatStreamPayload;
            } catch {
              return;
            }
            // 只处理当前会话的事件（session_id = conversation_id）
            if (payload.session_id && activeConvIdRef.current && payload.session_id !== activeConvIdRef.current) return;
            // 只处理当前 run 的事件
            if (payload.run_id && currentRunIdRef.current && payload.run_id !== currentRunIdRef.current) return;
            handler(payload);
          })
        )
      );
      if (cancelled) {
        fns.forEach(u => u());
        return;
      }
      unlistenFns = fns;
    })().catch(err => console.error('Failed to set up chat event listeners:', err));

    return () => {
      cancelled = true;
      unlistenFns.forEach(u => u());
    };
  }, [finishStream, loadConversations, updateStreamingMessage]);

  /* ── 当前会话标题 / 最后一条 AI 消息 id ── */
  const activeConv = conversations.find(c => c.id === activeConvId);
  const isEmpty = messages.length === 0 && !isSending;
  const lastAssistantId = useMemo(() => {
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i].role === 'assistant') return messages[i].id;
    }
    return null;
  }, [messages]);
  const handoffLabel: Record<HandoffStatePayload['state'], string> = {
    ollama_starting: '正在切换到 Ollama…',
    ollama_unloading: '任务结束，正在卸载 Ollama 模型…',
    jarvis_starting: '正在恢复 E-Assisant…',
    jarvis_active: 'E-Assisant 已恢复前台运行',
    jarvis_restore_failed: 'E-Assisant 自动恢复失败',
  };

  return (
    <EAssistantPageShell
      icon={<MessageSquare size={18} />}
      title="E-Assisant 对话"
      subtitle="前台语言、视觉与持续感知 · 工具任务将串行交接"
      contentClassName="overflow-hidden p-0"
    >
      <div className="flex h-full">
        {/* ──────── 左侧会话列表 ──────── */}
        <AnimatePresence initial={false}>
          {sidebarOpen && (
            <motion.aside
              initial={{ width: 0, opacity: 0 }}
              animate={{ width: 264, opacity: 1 }}
              exit={{ width: 0, opacity: 0 }}
              transition={{ duration: 0.25, ease: 'easeInOut' }}
              className="flex flex-shrink-0 flex-col overflow-hidden border-r border-white/[0.08]"
            >
              {/* 新建会话按钮 */}
              <div className="p-3 border-b border-white/[0.08]">
                <button
                  onClick={handleNewConversation}
                  className={cn(
                    'flex w-full items-center gap-2 rounded-xl px-3 py-2.5 text-xs font-bold transition-all',
                    activeConvId === null
                      ? 'bg-celestial-saturn/15 text-celestial-saturn border border-celestial-saturn/30'
                      : 'bg-white/5 text-white/60 hover:bg-white/10 hover:text-white/90 border border-white/10'
                  )}
                >
                  <Plus size={14} />
                  新建会话
                </button>
              </div>

              {/* 会话列表 */}
              <div className="flex-1 overflow-y-auto custom-scrollbar">
                {!loaded && (
                  <div className="flex justify-center py-10">
                    <Loader2 size={18} className="animate-spin text-celestial-saturn/60" />
                  </div>
                )}
                {loaded && conversations.length === 0 && (
                  <p className="px-4 py-10 text-center text-xs text-white/35">
                    暂无会话，开始新的对话吧
                  </p>
                )}
                {conversations.map(conv => (
                  <div
                    key={conv.id}
                    onClick={() => selectConversation(conv.id)}
                    className={cn(
                      'group relative w-full cursor-pointer border-b border-white/[0.05] px-3 py-3 transition-colors',
                      activeConvId === conv.id ? 'bg-white/[0.08]' : 'hover:bg-white/[0.04]'
                    )}
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span className="truncate text-xs font-bold text-white/85">
                        {conv.title || '未命名会话'}
                      </span>
                      <span className="flex-shrink-0 text-[10px] text-white/35">
                        {formatDate(conv.lastActiveAt)}
                      </span>
                    </div>
                    <div className="mt-1 flex items-center justify-between gap-2">
                      <span className="truncate text-[11px] text-white/45">
                        {truncate(conv.preview || '暂无预览', 22)}
                      </span>
                      <div className="flex flex-shrink-0 items-center gap-1.5">
                        {conv.messageCount > 0 && (
                          <span className="text-[10px] tabular-nums text-white/30">{conv.messageCount}</span>
                        )}
                        <button
                          onClick={(e) => handleDeleteConversation(conv.id, e)}
                          className="rounded p-0.5 opacity-0 transition-opacity hover:bg-red-500/20 group-hover:opacity-100"
                          title="删除会话"
                        >
                          <Trash2 size={11} className="text-white/40 hover:text-red-400" />
                        </button>
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            </motion.aside>
          )}
        </AnimatePresence>

        {/* ──────── 右侧聊天区域 ──────── */}
        <div className="flex min-w-0 flex-1 flex-col">
          {/* 聊天头部 */}
          <div className="flex flex-shrink-0 items-center gap-2 border-b border-white/[0.08] px-4 py-3">
            <button
              onClick={() => setSidebarOpen(v => !v)}
              className="flex h-8 w-8 items-center justify-center rounded-lg border border-white/10 bg-white/5 text-white/50 transition-colors hover:bg-white/10 hover:text-white"
              title={sidebarOpen ? '收起侧边栏' : '展开侧边栏'}
            >
              {sidebarOpen ? <PanelLeftClose size={14} /> : <PanelLeft size={14} />}
            </button>
            <Bot size={16} className="text-celestial-saturn" />
            <span className="truncate text-sm font-bold text-white/85">
              {activeConv ? activeConv.title : '新会话'}
            </span>
            {activeConv && activeConv.messageCount > 0 && (
              <span className="text-xs text-white/35">· {activeConv.messageCount} 条消息</span>
            )}
          </div>

          {/* 消息列表 */}
          <div
            ref={scrollRef}
            className="flex-1 overflow-y-auto custom-scrollbar px-4 py-4 md:px-6"
          >
            {/* 空状态 */}
            {isEmpty && (
              <div className="flex h-full flex-col items-center justify-center">
                <motion.div
                  initial={{ opacity: 0, y: 10 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={{ duration: 0.4 }}
                  className="flex flex-col items-center gap-4"
                >
                  <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-celestial-saturn/10 border border-celestial-saturn/20">
                    <Sparkles size={28} className="text-celestial-saturn" />
                  </div>
                  <div className="text-center">
                    <h2 className="text-lg font-black text-white/90">你好，我是 E-Assisant</h2>
                    <p className="mt-1 text-xs text-white/45">有什么我可以帮你的吗？</p>
                  </div>
                  <div className="mt-2 grid w-full max-w-md gap-2">
                    {SUGGESTIONS.map((s, i) => (
                      <button
                        key={i}
                        onClick={() => handleSend(s.prompt)}
                        className="group flex items-center justify-between rounded-xl border border-white/[0.08] bg-white/[0.03] px-4 py-2.5 text-left text-xs text-white/55 transition-all hover:border-celestial-saturn/30 hover:bg-celestial-saturn/5 hover:text-celestial-saturn"
                      >
                        <span>{s.label}</span>
                        <ChevronRight size={12} className="opacity-0 transition-opacity group-hover:opacity-100" />
                      </button>
                    ))}
                  </div>
                </motion.div>
              </div>
            )}

            {/* 消息流 */}
            <div className="mx-auto max-w-3xl space-y-4">
              <AnimatePresence initial={false}>
                {messages.map(msg => {
                  const toolsRunning = !!msg.toolCalls?.some(tc => tc.status === 'running');
                  const toolsOpen = expandedTools[msg.id] ?? toolsRunning;
                  const isLastAssistant = msg.role === 'assistant' && msg.id === lastAssistantId;

                  return (
                    <motion.div
                      key={msg.id}
                      initial={{ opacity: 0, y: 8 }}
                      animate={{ opacity: 1, y: 0 }}
                      transition={{ duration: 0.2 }}
                    >
                      {msg.role === 'user' ? (
                        editingMsgId === msg.id ? (
                          /* ── 用户消息编辑态 ── */
                          <div className="flex justify-end">
                            <div className="w-full max-w-[80%]">
                              <textarea
                                autoFocus
                                value={editingText}
                                onChange={(e) => setEditingText(e.target.value)}
                                onKeyDown={(e) => {
                                  if (e.key === 'Enter' && !e.shiftKey) {
                                    e.preventDefault();
                                    commitEdit();
                                  } else if (e.key === 'Escape') {
                                    setEditingMsgId(null);
                                  }
                                }}
                                rows={Math.min(6, Math.max(2, editingText.split('\n').length))}
                                className="custom-scrollbar w-full resize-none rounded-xl border border-blue-400/40 bg-blue-500/10 px-3 py-2 text-sm text-blue-50/90 outline-none focus:border-celestial-saturn/50"
                              />
                              <div className="mt-1.5 flex justify-end gap-2">
                                <button
                                  onClick={() => setEditingMsgId(null)}
                                  className="rounded-lg border border-white/10 bg-white/5 px-2.5 py-1 text-[11px] text-white/50 hover:bg-white/10"
                                >
                                  取消
                                </button>
                                <button
                                  onClick={commitEdit}
                                  className="rounded-lg border border-celestial-saturn/40 bg-celestial-saturn/15 px-2.5 py-1 text-[11px] font-bold text-celestial-saturn hover:bg-celestial-saturn/25"
                                >
                                  发送
                                </button>
                              </div>
                            </div>
                          </div>
                        ) : (
                          /* ── 用户消息气泡 ── */
                          <div className="group flex justify-end">
                            <div className="relative max-w-[80%] rounded-2xl rounded-br-md border border-blue-400/30 bg-blue-500/20 px-4 py-2.5">
                              <div className="whitespace-pre-wrap text-sm leading-relaxed text-blue-50/90">
                                {msg.content}
                              </div>
                              <span className="mt-1 block text-right text-[10px] text-blue-200/40">
                                {formatTime(msg.timestamp)}
                              </span>
                              <div className="absolute -left-7 top-2 flex flex-col gap-1 opacity-0 transition-opacity group-hover:opacity-100">
                                <button
                                  onClick={() => handleCopy(msg.content, msg.id)}
                                  className="rounded p-1"
                                  title="复制"
                                >
                                  {copiedId === msg.id
                                    ? <Check size={12} className="text-emerald-400" />
                                    : <Copy size={12} className="text-white/40 hover:text-white/70" />}
                                </button>
                                <button
                                  onClick={() => startEdit(msg)}
                                  className="rounded p-1"
                                  title="编辑并重新发送"
                                >
                                  <Pencil size={12} className="text-white/40 hover:text-white/70" />
                                </button>
                              </div>
                            </div>
                          </div>
                        )
                      ) : (
                        /* ── AI 消息气泡 ── */
                        <div className="group flex justify-start">
                          <div className="relative max-w-[85%] rounded-2xl rounded-bl-md border border-white/10 bg-white/[0.04] px-4 py-3 backdrop-blur-xl">
                            {/* 工具调用展示 */}
                            {msg.toolCalls && msg.toolCalls.length > 0 && (
                              <div className="mb-2 border-b border-white/[0.06] pb-2">
                                <button
                                  onClick={() => toggleTools(msg.id)}
                                  className="flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-wider text-cyan-400/70"
                                >
                                  <Cpu size={11} />
                                  工具调用 ({msg.toolCalls.length})
                                  <ChevronRight
                                    size={11}
                                    className={cn('transition-transform', toolsOpen && 'rotate-90')}
                                  />
                                </button>
                                <AnimatePresence>
                                  {toolsOpen && (
                                    <motion.div
                                      initial={{ height: 0, opacity: 0 }}
                                      animate={{ height: 'auto', opacity: 1 }}
                                      exit={{ height: 0, opacity: 0 }}
                                      className="mt-2 space-y-1.5 overflow-hidden"
                                    >
                                      {msg.toolCalls.map(tc => (
                                        <div
                                          key={tc.id}
                                          className={cn(
                                            'flex items-start gap-2 rounded-lg border px-2.5 py-1.5',
                                            tc.status === 'running' && 'border-cyan-500/25 bg-cyan-500/[0.05]',
                                            tc.status === 'error' && 'border-red-500/25 bg-red-500/[0.05]',
                                            tc.status === 'done' && 'border-white/[0.07] bg-white/[0.03]'
                                          )}
                                        >
                                          {/* 状态圆点 */}
                                          <div className="mt-0.5 flex h-4 w-4 flex-shrink-0 items-center justify-center rounded-full">
                                            {tc.status === 'running' ? (
                                              <Loader2 size={11} className="animate-spin text-cyan-400" />
                                            ) : tc.status === 'error' ? (
                                              <XCircle size={11} className="text-red-400" />
                                            ) : (
                                              <CheckCircle size={11} className="text-emerald-400" />
                                            )}
                                          </div>
                                          <div className="min-w-0 flex-1">
                                            <div className="flex items-center gap-1.5">
                                              <span className="text-cyan-400/80">{getToolIcon(tc.name)}</span>
                                              <span className="font-mono text-[11px] uppercase tracking-wide text-white/75">
                                                {tc.name.replace(/_/g, ' ')}
                                              </span>
                                            </div>
                                            {formatArgs(tc.arguments) && (
                                              <div className="mt-0.5 truncate font-mono text-[10px] text-white/40">
                                                {formatArgs(tc.arguments)}
                                              </div>
                                            )}
                                            {tc.result && (
                                              <div className="mt-0.5 whitespace-pre-wrap break-all font-mono text-[10px] leading-relaxed text-emerald-400/60">
                                                {truncate(String(tc.result), 400)}
                                              </div>
                                            )}
                                            {tc.error && (
                                              <div className="mt-0.5 whitespace-pre-wrap break-all font-mono text-[10px] leading-relaxed text-red-400/70">
                                                {truncate(String(tc.error), 400)}
                                              </div>
                                            )}
                                          </div>
                                        </div>
                                      ))}
                                    </motion.div>
                                  )}
                                </AnimatePresence>
                              </div>
                            )}

                            {/* 思考指示器 */}
                            {msg.thinking && !msg.content && (
                              <div className="flex items-center gap-2 py-1">
                                <Loader2 size={13} className="animate-spin text-celestial-saturn" />
                                <span className="text-xs text-white/50">正在思考…</span>
                                <div className="flex gap-1">
                                  <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-celestial-saturn/60" style={{ animationDelay: '0ms' }} />
                                  <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-celestial-saturn/60" style={{ animationDelay: '150ms' }} />
                                  <span className="h-1.5 w-1.5 animate-bounce rounded-full bg-celestial-saturn/60" style={{ animationDelay: '300ms' }} />
                                </div>
                              </div>
                            )}

                            {/* Markdown 流式内容 */}
                            {msg.content && (
                              <div className="markdown-body prose-invert text-sm leading-relaxed text-white/85">
                                <Markdown remarkPlugins={[remarkGfm]} components={markdownComponents}>
                                  {msg.content}
                                </Markdown>
                                {isLastAssistant && isSending && (
                                  <span className="ml-0.5 inline-block h-3.5 w-[7px] animate-pulse bg-celestial-saturn/60 align-middle" />
                                )}
                              </div>
                            )}
                            <span className="mt-1 block text-[10px] text-white/30">
                              {formatTime(msg.timestamp)}
                            </span>

                            {/* hover 操作：复制 + 重新生成 */}
                            <div className="absolute -right-7 top-2 flex flex-col gap-1 opacity-0 transition-opacity group-hover:opacity-100">
                              {msg.content && (
                                <button
                                  onClick={() => handleCopy(msg.content, msg.id)}
                                  className="rounded p-1"
                                  title="复制"
                                >
                                  {copiedId === msg.id
                                    ? <Check size={12} className="text-emerald-400" />
                                    : <Copy size={12} className="text-white/40 hover:text-white/70" />}
                                </button>
                              )}
                              {isLastAssistant && !isSending && (
                                <button
                                  onClick={handleRegenerate}
                                  className="rounded p-1"
                                  title="重新生成"
                                >
                                  <RefreshCw size={12} className="text-white/40 hover:text-celestial-saturn" />
                                </button>
                              )}
                              {msg.content && !isSending && (
                                <button
                                  onClick={() => {
                                    if (speakingMsgId === msg.id && voice.isSpeaking) {
                                      voice.stopSpeaking();
                                    } else {
                                      setSpeakingMsgId(msg.id);
                                      voice.speak(msg.content);
                                    }
                                  }}
                                  className="rounded p-1"
                                  title={speakingMsgId === msg.id ? '停止朗读' : '朗读这条回复'}
                                >
                                  {speakingMsgId === msg.id ? (
                                    <VolumeX size={12} className="text-celestial-saturn" />
                                  ) : (
                                    <Volume2 size={12} className="text-white/40 hover:text-white/70" />
                                  )}
                                </button>
                              )}
                            </div>
                          </div>
                        </div>
                      )}
                    </motion.div>
                  );
                })}
              </AnimatePresence>
            </div>
          </div>

          {(pendingHandoff || handoffState) && (
            <div className="flex-shrink-0 border-t border-cyan-400/15 bg-cyan-400/[0.035] px-3 py-2.5 md:px-4">
              <div className="mx-auto max-w-3xl">
                {pendingHandoff ? (
                  <div className="rounded-xl border border-cyan-400/25 bg-slate-950/35 px-3.5 py-3">
                    <div className="flex items-start gap-2.5">
                      <Cpu size={16} className="mt-0.5 flex-shrink-0 text-cyan-300" />
                      <div className="min-w-0 flex-1">
                        <div className="text-xs font-bold text-cyan-100">E-Assisant 建议交给 Ollama 执行</div>
                        <p className="mt-1 text-xs leading-relaxed text-white/65">
                          {pendingHandoff.recommendation.user_goal}
                        </p>
                        {pendingHandoff.recommendation.context_summary && (
                          <p className="mt-1 text-[11px] leading-relaxed text-white/40">
                            上下文：{pendingHandoff.recommendation.context_summary}
                          </p>
                        )}
                        {pendingHandoff.recommendation.requires_confirmation_for_write && (
                          <p className="mt-1.5 text-[11px] text-amber-300/85">
                            此任务可能产生写入或系统变更；实际执行前仍会逐项请求权限确认。
                          </p>
                        )}
                        <div className="mt-2.5 flex flex-wrap gap-2">
                          <button
                            onClick={confirmHandoff}
                            className="rounded-lg border border-cyan-300/35 bg-cyan-400/15 px-3 py-1.5 text-xs font-bold text-cyan-100 transition-colors hover:bg-cyan-400/25"
                          >
                            确认交给 Ollama
                          </button>
                          <button
                            onClick={() => setPendingHandoff(null)}
                            className="rounded-lg border border-white/10 bg-white/5 px-3 py-1.5 text-xs text-white/55 transition-colors hover:bg-white/10 hover:text-white/80"
                          >
                            取消
                          </button>
                        </div>
                      </div>
                    </div>
                  </div>
                ) : handoffState ? (
                  <div className={cn(
                    'flex items-center gap-2 rounded-xl border px-3.5 py-2 text-xs',
                    handoffState.state === 'jarvis_restore_failed'
                      ? 'border-red-400/25 bg-red-400/[0.06] text-red-200'
                      : 'border-cyan-400/20 bg-slate-950/30 text-cyan-100/85'
                  )}>
                    {handoffState.state === 'jarvis_restore_failed'
                      ? <XCircle size={14} className="flex-shrink-0" />
                      : handoffState.state === 'jarvis_active'
                        ? <CheckCircle size={14} className="flex-shrink-0 text-emerald-300" />
                        : <Loader2 size={14} className="flex-shrink-0 animate-spin" />}
                    <span>{handoffLabel[handoffState.state]}</span>
                    {handoffState.error && <span className="truncate text-white/45">· {handoffState.error}</span>}
                  </div>
                ) : null}
              </div>
            </div>
          )}

          {/* 输入区域 */}
          <div className="flex-shrink-0 border-t border-white/[0.08] p-3 md:p-4">
            <div className="mx-auto flex max-w-3xl items-end gap-2">
              {/* 语音输入按钮：颜色随语音状态变化 */}
              <button
                onClick={handleMicToggle}
                className={cn(
                  'flex h-10 w-10 flex-shrink-0 items-center justify-center rounded-xl border transition-colors',
                  micStatusStyle[voice.voiceStatus]
                )}
                title={voice.isListening ? '停止语音输入' : '开始语音输入'}
              >
                <Mic size={16} />
              </button>

              {/* 实时对话模式开关 */}
              <button
                onClick={voice.toggleVoiceMode}
                className={cn(
                  'flex h-10 w-10 flex-shrink-0 items-center justify-center rounded-xl border transition-colors',
                  voice.isVoiceMode
                    ? 'border-violet-400/60 bg-violet-400/20 text-violet-300'
                    : 'border-white/10 bg-white/5 text-white/45 hover:bg-white/10 hover:text-white/80'
                )}
                title={voice.isVoiceMode ? '关闭实时对话' : '开启实时对话（说话→自动发送→自动朗读）'}
              >
                {voice.isVoiceMode ? <PhoneOff size={16} /> : <Phone size={16} />}
              </button>

              {/* 多行输入框 */}
              <textarea
                ref={textareaRef}
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={handleKeyDown}
                placeholder={
                  voice.isListening
                    ? `聆听中… ${voice.interimTranscript}`
                    : isSending
                      ? '正在生成中…'
                      : pendingHandoff
                        ? '请先确认或取消当前交接任务…'
                      : '输入消息，Enter 发送，Shift+Enter 换行…'
                }
                rows={1}
                className="custom-scrollbar max-h-32 flex-1 resize-none rounded-xl border border-white/10 bg-white/[0.05] px-3 py-2.5 text-sm text-white/90 placeholder-white/30 outline-none transition-colors focus:border-celestial-saturn/40"
              />

              {/* TTS 朗读开关 */}
              <button
                onClick={() => setAutoRead(v => !v)}
                className={cn(
                  'flex h-10 w-10 flex-shrink-0 items-center justify-center rounded-xl border transition-colors',
                  autoRead
                    ? 'border-emerald-400/50 bg-emerald-400/15 text-emerald-300'
                    : 'border-white/10 bg-white/5 text-white/45 hover:bg-white/10 hover:text-white/80'
                )}
                title={autoRead ? '关闭自动朗读' : '开启自动朗读 AI 回复'}
              >
                {autoRead ? <Volume2 size={16} /> : <VolumeX size={16} />}
              </button>

              {/* 发送 / 停止按钮 */}
              <button
                onClick={() => (isSending ? handleCancel() : handleSend())}
                disabled={!isSending && (!input.trim() || !!pendingHandoff)}
                className={cn(
                  'flex h-10 w-10 flex-shrink-0 items-center justify-center rounded-xl border transition-all disabled:cursor-not-allowed disabled:opacity-30',
                  isSending
                    ? 'border-red-400/40 bg-red-500/15 text-red-300 hover:bg-red-500/25'
                    : 'border-celestial-saturn/30 bg-celestial-saturn/15 text-celestial-saturn hover:bg-celestial-saturn/25'
                )}
                title={isSending ? '停止生成' : '发送'}
              >
                {isSending ? <Square size={14} /> : <Send size={16} />}
              </button>
            </div>
          </div>
        </div>
      </div>
    </EAssistantPageShell>
  );
}
