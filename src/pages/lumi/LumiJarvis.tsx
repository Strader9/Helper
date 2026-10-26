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
import { useLocation } from 'react-router-dom';
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

interface JarvisVramStatus {
  gpuName?: string | null;
  usedMb?: number | null;
  totalMb?: number | null;
  availableMb?: number | null;
  requiredMb: number;
  jarvisRunning: boolean;
  status: 'ready' | 'loading' | 'occupied' | 'insufficient' | 'unavailable';
  message: string;
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

interface RuntimeSnapshot {
  state: string;
  activeRole?: 'jarvis' | 'ollama' | 'system' | null;
  detail?: string | null;
}

type RouteDecision =
  | { mode: 'direct'; owner: 'jarvis' | 'ollama' | 'system' }
  | { mode: 'handoff'; front: 'jarvis'; worker: 'ollama' };

type RoutingEntry = [string, RouteDecision];

const CAPABILITY_LABELS: Record<string, string> = {
  conversation: '对话',
  screen_understanding: '屏幕理解',
  proactive_monitoring: '持续监控',
  voice_interaction: '语音交互',
  desktop_gui: '桌面操作',
  memory_knowledge: '记忆知识库',
  messaging_calendar: '消息与日程',
  media_work: '媒体工作',
  cad_industry: 'CAD / 行业工作',
  mcp_external_ai: '外部 AI / MCP',
  file_work: '文件工作',
  web_studio: '网页工坊',
  code_git: '代码与 Git',
  browser_work: '浏览器任务',
  document_work: '文档与 OCR',
  workflow_automation: '工作流与定时',
  system_operations: '系统操作',
  safety_execution: '安全执行',
};

const INPUT_CLASS =
  'w-full rounded-xl border border-white/10 bg-black/30 px-3.5 py-2.5 text-sm text-white/85 outline-none transition-colors placeholder:text-white/25 hover:border-white/20 focus:border-celestial-saturn/50';

function fmtClock(at: number) {
  const d = new Date(at);
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

export default function LumiJarvis() {
  const location = useLocation();
  const incomingVoiceText = (location.state as { voiceText?: string } | null)?.voiceText;
  const [health, setHealth] = useState<JarvisHealth | null>(null);
  const [vram, setVram] = useState<JarvisVramStatus | null>(null);
  const [online, setOnline] = useState<boolean | null>(null);
  const [busyStart, setBusyStart] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);
  const [runtime, setRuntime] = useState<RuntimeSnapshot>({ state: 'idle' });
  const [routing, setRouting] = useState<RoutingEntry[]>([]);

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
  const consumedIncomingVoice = useRef('');

  /* ── 健康检查：3s 一轮，未运行不当作错误，只是状态未知 ── */
  const probe = useCallback(async () => {
    // 显存查询不能被 Jarvis HTTP 健康检查阻塞；后者在后端未运行时会等待较长超时，
    // 但用户仍应立刻看到“此刻能否启动”。
    void invoke<JarvisVramStatus>('get_jarvis_vram_status')
      .then(setVram)
      .catch(() => setVram(null));
    try {
      const h = await invoke<JarvisHealth>('jarvis_health');
      setHealth(h);
      setOnline(true);
      setStartError(null);
    } catch {
      setOnline(false);
    }
  }, []);

  const refreshRuntime = useCallback(async () => {
    try {
      const [snapshot, decisions] = await Promise.all([
        invoke<RuntimeSnapshot>('get_inference_runtime'),
        invoke<RoutingEntry[]>('get_model_routing'),
      ]);
      setRuntime(snapshot);
      setRouting(decisions);
    } catch {
      // 调度信息只是辅助诊断；不能让它影响 Jarvis 本身的健康检测。
    }
  }, []);

  useEffect(() => {
    void probe();
    void refreshRuntime();
    const timer = window.setInterval(() => {
      void probe();
      void refreshRuntime();
    }, 3000);
    return () => window.clearInterval(timer);
  }, [probe, refreshRuntime]);

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
      window.setTimeout(() => {
        void probe();
        void refreshRuntime();
      }, 1500);
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
    void refreshRuntime();
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

  // 首页话筒识别完成后将文本随路由带到这里；只发送一次，避免 StrictMode
  // 或状态回放让同一句话重复进入 Jarvis。
  useEffect(() => {
    const text = (incomingVoiceText || '').trim();
    if (!text || text === consumedIncomingVoice.current) return;
    consumedIncomingVoice.current = text;
    void send(text);
  }, [incomingVoiceText, send]);

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

  const runtimeLabel =
    runtime.state === 'jarvis_active'
      ? 'Jarvis 前台'
      : runtime.state === 'ollama_active'
        ? 'Ollama 后台'
        : runtime.state === 'idle'
          ? '空闲'
          : runtime.state === 'error'
            ? '需要处理'
            : '切换中';

  const runtimeVariant =
    runtime.state === 'jarvis_active'
      ? 'safe'
      : runtime.state === 'ollama_active' || runtime.state === 'error'
        ? 'warning'
        : 'neutral';

  const vramVariant =
    vram?.status === 'ready' || vram?.status === 'occupied'
      ? 'safe'
      : vram?.status === 'loading' || vram?.status === 'insufficient'
        ? 'warning'
        : 'neutral';
  const vramLabel =
    vram?.status === 'ready'
      ? '可启动'
      : vram?.status === 'loading'
        ? '正在加载'
      : vram?.status === 'occupied'
        ? 'Jarvis 已占用'
        : vram?.status === 'insufficient'
          ? '需释放显存'
          : '未检测';
  const vramPercent =
    vram?.totalMb && vram.usedMb !== null && vram.usedMb !== undefined
      ? Math.min(100, Math.max(0, (vram.usedMb / vram.totalMb) * 100))
      : 0;

  const jarvisDirect = routing
    .filter(([, decision]) => decision.mode === 'direct' && decision.owner === 'jarvis')
    .map(([capability]) => CAPABILITY_LABELS[capability] ?? capability);
  const ollamaDirect = routing
    .filter(([, decision]) => decision.mode === 'direct' && decision.owner === 'ollama')
    .map(([capability]) => CAPABILITY_LABELS[capability] ?? capability);
  const handoff = routing
    .filter(([, decision]) => decision.mode === 'handoff')
    .map(([capability]) => CAPABILITY_LABELS[capability] ?? capability);

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
            <div className="grid grid-cols-2 gap-3 md:grid-cols-5">
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
              <div className="rounded-xl bg-white/[0.03] px-4 py-3">
                <p className="text-[11px] font-bold uppercase tracking-wider text-white/35">显存主控</p>
                <div className="mt-1.5">
                  <Badge variant={runtimeVariant}>{runtimeLabel}</Badge>
                </div>
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

        {/* 显存监控：只报告真实 nvidia-smi 数据与后端安全判定，不以模型文件大小猜测。 */}
        <div>
          <SectionTitle label="显存监控" color="bg-emerald-400" />
          <GlassCard hoverEffect={false}>
            <div className="flex flex-col gap-4 md:flex-row md:items-center md:justify-between">
              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-2">
                  <p className="text-sm font-bold text-white/85">Jarvis 启动条件</p>
                  <Badge variant={vramVariant}>{vramLabel}</Badge>
                </div>
                <p className="mt-1.5 text-[11px] leading-relaxed text-white/40">
                  {vram?.message ?? '正在读取 NVIDIA 显存状态…'}
                </p>
              </div>
              <div className="w-full shrink-0 md:w-72">
                <div className="flex justify-between gap-3 text-[11px] text-white/45">
                  <span className="truncate">{vram?.gpuName ?? 'NVIDIA GPU'}</span>
                  <span className="shrink-0 font-mono">
                    {vram?.usedMb !== null && vram?.usedMb !== undefined && vram?.totalMb
                      ? `${(vram.usedMb / 1024).toFixed(1)} / ${(vram.totalMb / 1024).toFixed(1)} GiB`
                      : '—'}
                  </span>
                </div>
                <div className="mt-2 h-2 overflow-hidden rounded-full bg-white/[0.08]">
                  <motion.div
                    className={cn(
                      'h-full rounded-full',
                      vram?.status === 'insufficient' || vram?.status === 'loading'
                        ? 'bg-amber-400'
                        : 'bg-emerald-400'
                    )}
                    animate={{ width: `${vramPercent}%` }}
                    transition={{ type: 'spring', stiffness: 90, damping: 18 }}
                  />
                </div>
                <p className="mt-1.5 text-right text-[10px] text-white/30">
                  可用 {vram?.availableMb !== null && vram?.availableMb !== undefined ? `${(vram.availableMb / 1024).toFixed(1)} GiB` : '—'} · 启动需 {((vram?.requiredMb ?? 7200) / 1024).toFixed(1)} GiB
                </p>
              </div>
            </div>
          </GlassCard>
        </div>

        {/* 模型职责 */}
        <div>
          <SectionTitle label="模型调度" color="bg-sky-400" />
          <GlassCard hoverEffect={false}>
            <p className="text-[11px] leading-relaxed text-white/35">
              同一时刻只允许一个本地推理模型占用显存。需要后台能力时，贾维斯先整理上下文，再将任务串行交给 Ollama；完成后释放模型并恢复贾维斯。
            </p>
            <div className="mt-3 grid gap-3 md:grid-cols-3">
              <div className="rounded-xl border border-cyan-400/15 bg-cyan-400/[0.04] px-3.5 py-3">
                <p className="text-xs font-bold text-cyan-200">Jarvis · 前台感知</p>
                <p className="mt-1.5 text-[11px] leading-relaxed text-white/45">
                  {jarvisDirect.length ? jarvisDirect.join('、') : '正在读取职责表…'}
                </p>
              </div>
              <div className="rounded-xl border border-violet-400/15 bg-violet-400/[0.04] px-3.5 py-3">
                <p className="text-xs font-bold text-violet-200">Ollama · 后台专长</p>
                <p className="mt-1.5 text-[11px] leading-relaxed text-white/45">
                  {ollamaDirect.length ? ollamaDirect.join('、') : '正在读取职责表…'}
                </p>
              </div>
              <div className="rounded-xl border border-amber-400/15 bg-amber-400/[0.04] px-3.5 py-3">
                <p className="text-xs font-bold text-amber-200">串行交接 · Jarvis → Ollama</p>
                <p className="mt-1.5 text-[11px] leading-relaxed text-white/45">
                  {handoff.length ? handoff.join('、') : '正在读取职责表…'}
                </p>
              </div>
            </div>
            {runtime.detail && <p className="mt-3 text-[10px] text-white/25">调度状态：{runtime.detail}</p>}
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
