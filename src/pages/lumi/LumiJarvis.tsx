// AI Jarvis —— 外部感知进程控制台
//
// 和其余页面不同，这页操作的是"另一个进程"：一个带 DXGI 屏幕采集 +
// WASAPI 系统音频 + MiniCPM-o 的 Python 服务。它只产出文字，不出声，
// 所以这里承担两件它做不到的事：
//   1. 把它的回复念出来（speechSynthesis）
//   2. 把用户说的话转成文字喂给它（SpeechRecognition）
import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { motion } from 'motion/react';
import {
  Activity,
  Cpu,
  Ear,
  EarOff,
  Loader2,
  Mic,
  MicOff,
  Play,
  Send,
  Square,
  Volume2,
  VolumeX,
} from 'lucide-react';
import { Badge, GlassCard, SectionTitle } from '../../components/lumi/LumiShared';
import { LumiPageShell } from '../../components/lumi/LumiPageShell';
import { useVoice } from '../../hooks/useVoice';
import { cn } from '../../lib/cn';

interface JarvisHealth {
  status?: string;
  lifecycle?: string;
  native_connected?: boolean;
  inference_backend?: string;
  version?: string;
}

interface DuplexState {
  active?: boolean;
  session_id?: string | null;
  instruction?: string;
}

interface ChatMsg {
  role: 'user' | 'jarvis';
  text: string;
  at: number;
}

const INPUT_CLASS =
  'w-full rounded-xl border border-white/10 bg-black/30 px-3.5 py-2.5 text-sm text-white/85 outline-none transition-colors placeholder:text-white/25 hover:border-white/20 focus:border-celestial-saturn/50';

function fmtClock(at: number) {
  const d = new Date(at);
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

export default function LumiJarvis() {
  const [health, setHealth] = useState<JarvisHealth | null>(null);
  const [online, setOnline] = useState<boolean | null>(null);
  const [busyStart, setBusyStart] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);

  const [duplex, setDuplex] = useState<DuplexState>({ active: false });
  const [instruction, setInstruction] = useState('盯着屏幕，发现报错或异常就简短提醒我');
  const [busyDuplex, setBusyDuplex] = useState(false);

  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [autoSpeak, setAutoSpeak] = useState(true);

  const { isListening, supported, startListening, stopListening, transcript, clearTranscript, speak, stopSpeaking, isSpeaking } =
    useVoice();

  const scrollRef = useRef<HTMLDivElement | null>(null);
  // 语音识别结果是"稳定后才回调"的，用 ref 记住已消费的那一条，
  // 否则 StrictMode 双挂载或重渲染会把同一句话发两次。
  const consumedTranscript = useRef('');

  /* ── 健康检查：3s 一轮，未运行不当作错误，只是状态未知 ── */
  const probe = useCallback(async () => {
    try {
      const h = await invoke<JarvisHealth>('jarvis_health');
      setHealth(h);
      setOnline(true);
      setStartError(null);
    } catch {
      setOnline(false);
    }
  }, []);

  useEffect(() => {
    void probe();
    const timer = window.setInterval(() => void probe(), 3000);
    return () => window.clearInterval(timer);
  }, [probe]);

  /* ── 感知状态跟着后端在线情况走 ── */
  const refreshDuplex = useCallback(async () => {
    try {
      const d = await invoke<DuplexState>('jarvis_duplex', { action: 'status' });
      setDuplex(d ?? { active: false });
    } catch {
      setDuplex({ active: false });
    }
  }, []);

  useEffect(() => {
    if (online) void refreshDuplex();
  }, [online, refreshDuplex]);

  const handleStart = async () => {
    setBusyStart(true);
    setStartError(null);
    try {
      await invoke('jarvis_start', {});
      // 后端起来还要加载模型，立刻探一次通常仍失败，交给轮询接管
      window.setTimeout(() => void probe(), 1500);
    } catch (e) {
      setStartError(String(e));
    } finally {
      setBusyStart(false);
    }
  };

  const handleStop = async () => {
    try {
      await invoke('jarvis_stop');
    } catch {
      /* 非本进程拉起的实例停不掉，忽略即可 */
    }
    void probe();
  };

  const handleDuplex = async (action: 'start' | 'stop') => {
    setBusyDuplex(true);
    try {
      const d = await invoke<DuplexState>('jarvis_duplex', {
        action,
        instruction: action === 'start' ? instruction : null,
      });
      setDuplex(d ?? { active: false });
    } catch (e) {
      setDuplex((prev) => ({ ...prev, active: false }));
      setStartError(String(e));
    } finally {
      setBusyDuplex(false);
    }
  };

  const send = useCallback(
    async (raw?: string) => {
      const text = (raw ?? input).trim();
      if (!text || sending) return;
      setInput('');
      setMessages((prev) => [...prev, { role: 'user', text, at: Date.now() }]);
      setSending(true);
      try {
        const r = await invoke<{ reply?: string }>('jarvis_chat', { message: text });
        const reply = (r?.reply ?? '').trim() || '（没有返回内容）';
        setMessages((prev) => [...prev, { role: 'jarvis', text: reply, at: Date.now() }]);
        if (autoSpeak) speak(reply);
      } catch (e) {
        const raw = String(e);
        // 503 是"后端在、模型没就绪"，和"后端挂了"是两回事，不能混成一句报错。
        const friendly =
          raw.includes('503') || raw.includes('模型')
            ? '模型还没就绪：贾维斯的本地推理需要 native worker 与 6.32 GiB 权重。当前只能验证链路。'
            : `调用失败：${raw}`;
        setMessages((prev) => [...prev, { role: 'jarvis', text: friendly, at: Date.now() }]);
      } finally {
        setSending(false);
      }
    },
    [autoSpeak, input, sending, speak]
  );

  /* ── 语音识别完成 → 直接发送 ── */
  useEffect(() => {
    const t = (transcript || '').trim();
    if (!t || t === consumedTranscript.current) return;
    consumedTranscript.current = t;
    void send(t);
    clearTranscript();
  }, [transcript, send, clearTranscript]);

  /* ── 新消息滚到底 ── */
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: 'smooth' });
  }, [messages.length]);

  const statusBadge = () => {
    if (online === null) return <Badge variant="neutral">检测中</Badge>;
    if (!online) return <Badge variant="danger">未运行</Badge>;
    return health?.status === 'ok' ? <Badge variant="safe">在线</Badge> : <Badge variant="warning">降级</Badge>;
  };

  const backendLabel =
    health?.inference_backend === 'cuda'
      ? 'CUDA'
      : health?.inference_backend === 'cpu'
        ? 'CPU'
        : '未知';

  return (
    <LumiPageShell
      icon={<Ear size={18} />}
      iconBg="bg-cyan-500/15"
      title="AI 贾维斯"
      subtitle="屏幕与系统音频感知 · 全双工对话"
    >
      <div className="mx-auto max-w-5xl space-y-5">
        {/* 状态 */}
        <div>
          <SectionTitle label="运行状态" color="bg-cyan-400" />
          <GlassCard hoverEffect={false}>
            <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
              <div className="rounded-xl bg-white/[0.03] px-4 py-3">
                <p className="text-[11px] font-bold uppercase tracking-wider text-white/35">服务</p>
                <div className="mt-1.5">{statusBadge()}</div>
              </div>
              <div className="rounded-xl bg-white/[0.03] px-4 py-3">
                <p className="text-[11px] font-bold uppercase tracking-wider text-white/35">原生采集</p>
                <div className="mt-1.5">
                  {health?.native_connected ? (
                    <Badge variant="safe">已连接</Badge>
                  ) : (
                    <Badge variant="neutral">未连接</Badge>
                  )}
                </div>
              </div>
              <div className="rounded-xl bg-white/[0.03] px-4 py-3">
                <p className="text-[11px] font-bold uppercase tracking-wider text-white/35">推理后端</p>
                <p className="mt-1 font-mono text-sm text-white/80">{backendLabel}</p>
              </div>
              <div className="rounded-xl bg-white/[0.03] px-4 py-3">
                <p className="text-[11px] font-bold uppercase tracking-wider text-white/35">版本</p>
                <p className="mt-1 font-mono text-sm text-white/80">{health?.version ?? '—'}</p>
              </div>
            </div>

            <div className="mt-3 flex flex-wrap items-center gap-2">
              {!online ? (
                <button
                  onClick={handleStart}
                  disabled={busyStart}
                  className="inline-flex items-center gap-2 rounded-xl border border-cyan-400/30 bg-cyan-400/10 px-3.5 py-2 text-xs font-bold text-cyan-200 transition-colors hover:bg-cyan-400/20 disabled:opacity-50"
                >
                  {busyStart ? <Loader2 size={14} className="animate-spin" /> : <Play size={14} />}
                  启动贾维斯
                </button>
              ) : (
                <button
                  onClick={handleStop}
                  className="inline-flex items-center gap-2 rounded-xl border border-white/10 bg-white/[0.04] px-3.5 py-2 text-xs font-bold text-white/60 transition-colors hover:bg-white/[0.08] hover:text-white/85"
                >
                  <Square size={13} />
                  停止
                </button>
              )}
              <button
                onClick={() => void probe()}
                className="inline-flex items-center gap-1.5 rounded-xl px-2.5 py-2 text-xs font-bold text-white/40 transition-colors hover:text-white/70"
              >
                <Activity size={13} />
                重新检测
              </button>
              {!online && (
                <span className="text-[11px] text-white/30">
                  首次启动需加载 6.32 GiB 模型，可能等待数分钟
                </span>
              )}
            </div>

            {startError && (
              <p className="mt-3 rounded-lg border border-red-500/20 bg-red-500/[0.06] px-3 py-2 text-[11px] leading-relaxed text-red-300/80">
                {startError}
              </p>
            )}
          </GlassCard>
        </div>

        {/* 全双工感知 */}
        <div>
          <SectionTitle label="持续感知" color="bg-violet-400" />
          <GlassCard hoverEffect={false}>
            <p className="mb-3 text-[11px] leading-relaxed text-white/35">
              开启后贾维斯会持续看屏幕、听系统声音，只在需要时出声，不是一问一答。
              需要已编译的原生采集模块（native_connected 为已连接）。
            </p>
            <div className="flex flex-col gap-2 md:flex-row">
              <input
                value={instruction}
                onChange={(e) => setInstruction(e.target.value)}
                placeholder="告诉它该关注什么，例如：盯着编译输出，报错就提醒我"
                className={INPUT_CLASS}
                disabled={!online || duplex.active}
              />
              {duplex.active ? (
                <button
                  onClick={() => void handleDuplex('stop')}
                  disabled={busyDuplex}
                  className="inline-flex shrink-0 items-center justify-center gap-2 rounded-xl border border-amber-400/30 bg-amber-400/10 px-4 py-2.5 text-xs font-bold text-amber-200 transition-colors hover:bg-amber-400/20 disabled:opacity-50"
                >
                  {busyDuplex ? <Loader2 size={14} className="animate-spin" /> : <EarOff size={14} />}
                  停止感知
                </button>
              ) : (
                <button
                  onClick={() => void handleDuplex('start')}
                  disabled={!online || busyDuplex || !instruction.trim()}
                  className="inline-flex shrink-0 items-center justify-center gap-2 rounded-xl border border-violet-400/30 bg-violet-400/10 px-4 py-2.5 text-xs font-bold text-violet-200 transition-colors hover:bg-violet-400/20 disabled:opacity-40"
                >
                  {busyDuplex ? <Loader2 size={14} className="animate-spin" /> : <Ear size={14} />}
                  开始感知
                </button>
              )}
            </div>
            {duplex.active && (
              <p className="mt-2.5 flex items-center gap-2 text-[11px] text-emerald-300/70">
                <span className="relative flex h-1.5 w-1.5">
                  <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-70" />
                  <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-emerald-400" />
                </span>
                正在持续感知{duplex.instruction ? `：${duplex.instruction}` : ''}
              </p>
            )}
          </GlassCard>
        </div>

        {/* 对话 */}
        <div>
          <SectionTitle label="对话" color="bg-emerald-400" />
          <GlassCard hoverEffect={false} className="flex flex-col">
            <div ref={scrollRef} className="max-h-[340px] min-h-[180px] space-y-3 overflow-y-auto pr-1">
              {messages.length === 0 ? (
                <div className="flex h-[180px] flex-col items-center justify-center gap-2 text-center">
                  <Cpu size={22} className="text-white/15" />
                  <p className="text-xs text-white/30">
                    问它屏幕上正在发生什么，或者直接下指令
                    <br />
                    语音输入会直接发送，回复会自动朗读
                  </p>
                </div>
              ) : (
                messages.map((m, i) => (
                  <motion.div
                    key={`${m.at}-${i}`}
                    initial={{ opacity: 0, y: 6 }}
                    animate={{ opacity: 1, y: 0 }}
                    className={cn('flex', m.role === 'user' ? 'justify-end' : 'justify-start')}
                  >
                    <div
                      className={cn(
                        'max-w-[82%] rounded-2xl px-3.5 py-2.5 text-sm leading-relaxed',
                        m.role === 'user'
                          ? 'bg-white/[0.08] text-white/85'
                          : 'border border-white/[0.07] bg-cyan-400/[0.06] text-white/80'
                      )}
                    >
                      <p className="whitespace-pre-wrap">{m.text}</p>
                      <p className="mt-1 text-[10px] text-white/25">{fmtClock(m.at)}</p>
                    </div>
                  </motion.div>
                ))
              )}
              {sending && (
                <div className="flex items-center gap-2 text-[11px] text-white/35">
                  <Loader2 size={12} className="animate-spin" />
                  贾维斯正在思考…
                </div>
              )}
            </div>

            <div className="mt-3 flex items-center gap-2 border-t border-white/[0.06] pt-3">
              <button
                onClick={() => (isListening ? stopListening() : startListening())}
                disabled={!online || !supported}
                title={supported ? '语音输入' : '当前环境不支持语音识别'}
                className={cn(
                  'flex h-9 w-9 shrink-0 items-center justify-center rounded-xl border transition-colors disabled:opacity-30',
                  isListening
                    ? 'border-red-400/40 bg-red-400/15 text-red-300'
                    : 'border-white/10 bg-white/[0.04] text-white/45 hover:text-white/80'
                )}
              >
                {isListening ? <MicOff size={15} /> : <Mic size={15} />}
              </button>

              <input
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !e.shiftKey) {
                    e.preventDefault();
                    void send();
                  }
                }}
                placeholder={online ? '说点什么…' : '先启动贾维斯'}
                disabled={!online || sending}
                className={INPUT_CLASS}
              />

              <button
                onClick={() => (isSpeaking ? stopSpeaking() : setAutoSpeak((v) => !v))}
                title={isSpeaking ? '停止朗读' : autoSpeak ? '自动朗读：开' : '自动朗读：关'}
                className={cn(
                  'flex h-9 w-9 shrink-0 items-center justify-center rounded-xl border transition-colors',
                  autoSpeak
                    ? 'border-emerald-400/30 bg-emerald-400/10 text-emerald-300'
                    : 'border-white/10 bg-white/[0.04] text-white/35'
                )}
              >
                {isSpeaking ? <VolumeX size={15} /> : <Volume2 size={15} />}
              </button>

              <button
                onClick={() => void send()}
                disabled={!online || sending || !input.trim()}
                className="flex h-9 shrink-0 items-center gap-1.5 rounded-xl border border-celestial-saturn/30 bg-celestial-saturn/10 px-3.5 text-xs font-bold text-celestial-saturn transition-colors hover:bg-celestial-saturn/20 disabled:opacity-35"
              >
                <Send size={13} />
                发送
              </button>
            </div>

            {!supported && (
              <p className="mt-2 text-[10px] leading-relaxed text-amber-300/50">
                当前 WebView 不支持语音识别（webkitSpeechRecognition），语音输入不可用；文字与朗读不受影响。
              </p>
            )}
          </GlassCard>
        </div>
      </div>
    </LumiPageShell>
  );
}
