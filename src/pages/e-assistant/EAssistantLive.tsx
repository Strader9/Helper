// 按需启动实时感知，任务对话统一在主聊天页。
import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { motion } from 'motion/react';
import { Link } from 'react-router-dom';
import {
  Activity,
  Ear,
  EarOff,
  Loader2,
  Play,
  Square,
} from 'lucide-react';
import { Badge, GlassCard, SectionTitle } from '../../components/e-assistant/EAssistantShared';
import { EAssistantPageShell } from '../../components/e-assistant/EAssistantPageShell';
import { cn } from '../../lib/cn';

interface EAssistantHealth {
  status?: string;
  lifecycle?: string;
  native_connected?: boolean;
  inference_backend?: string;
  version?: string;
}

interface EAssistantVramStatus {
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

interface RuntimeSnapshot {
  state: string;
  detail?: string | null;
}

interface PerceptionObservation {
  id: string;
  observed_at: string;
  description: string;
}

const INPUT_CLASS =
  'w-full rounded-xl border border-white/10 bg-black/30 px-3.5 py-2.5 text-sm text-white/85 outline-none transition-colors placeholder:text-white/25 hover:border-white/20 focus:border-celestial-saturn/50';

export default function EAssistantLive() {
  const [health, setHealth] = useState<EAssistantHealth | null>(null);
  const [vram, setVram] = useState<EAssistantVramStatus | null>(null);
  const [online, setOnline] = useState<boolean | null>(null);
  const [busyStart, setBusyStart] = useState(false);
  const [startError, setStartError] = useState<string | null>(null);
  const [runtime, setRuntime] = useState<RuntimeSnapshot>({ state: 'idle' });
  const [observation, setObservation] = useState<PerceptionObservation | null>(null);

  const [duplex, setDuplex] = useState<DuplexState>({ active: false });
  const [instruction, setInstruction] = useState('盯着屏幕，发现报错或异常就简短提醒我');
  const [busyDuplex, setBusyDuplex] = useState(false);

  /* ── 健康检查：3s 一轮，未运行不当作错误，只是状态未知 ── */
  const probe = useCallback(async () => {
    // 显存查询不能被 EAssistant HTTP 健康检查阻塞；后者在后端未运行时会等待较长超时，
    // 但用户仍应立刻看到“此刻能否启动”。
    void invoke<EAssistantVramStatus>('get_jarvis_vram_status')
      .then(setVram)
      .catch(() => setVram(null));
    try {
      const h = await invoke<EAssistantHealth>('jarvis_health');
      setHealth(h);
      setOnline(true);
      setStartError(null);
    } catch {
      setOnline(false);
    }
  }, []);

  const refreshRuntime = useCallback(async () => {
    try {
      setRuntime(await invoke<RuntimeSnapshot>('get_inference_runtime'));
      setObservation(await invoke<PerceptionObservation | null>('get_jarvis_observation'));
    } catch {
      // 调度信息只是辅助诊断；不能让它影响 EAssistant 本身的健康检测。
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
    setStartError(null);
    try {
      await invoke('jarvis_stop');
    } catch (error) {
      setStartError(String(error));
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
      ? '感知运行中'
      : runtime.state === 'idle'
        ? '未启用'
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
        ? 'E-Assisant 已占用'
        : vram?.status === 'insufficient'
          ? '需释放显存'
          : '未检测';
  const vramPercent =
    vram?.totalMb && vram.usedMb !== null && vram.usedMb !== undefined
      ? Math.min(100, Math.max(0, (vram.usedMb / vram.totalMb) * 100))
      : 0;

  return (
    <EAssistantPageShell
      icon={<Ear size={18} />}
      iconBg="bg-cyan-500/15"
      title="AI 贾维斯"
      subtitle="按需启用屏幕与系统音频感知"
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
              {!online && !runtime.state.startsWith('jarvis_') ? (
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
                  disabled={busyStart}
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
                  <p className="text-sm font-bold text-white/85">E-Assisant 启动条件</p>
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

        <GlassCard hoverEffect={false}>
          {observation && (
            <div className="mb-3">
              <p className="text-[11px] text-white/45">{new Date(observation.observed_at).toLocaleString()}</p>
              <p className="mt-1 text-sm text-white/80">{observation.description}</p>
              {!online && (
                <Link
                  to="/chat"
                  state={{ voiceText: '请根据上一次感知观察，总结看到了什么。' }}
                  className="mt-2 inline-block text-sm text-celestial-saturn hover:underline"
                >
                  在对话中使用这次观察
                </Link>
              )}
            </div>
          )}
          <Link to="/chat" className="text-sm font-medium text-celestial-saturn hover:underline">
            在统一对话中发送文字或语音
          </Link>
          <p className="mt-2 text-xs text-white/45">实时感知开启时，助手仍可执行明确的本地指令。使用模型处理对话前，请停止感知运行时。</p>
        </GlassCard>
      </div>
    </EAssistantPageShell>
  );
}
