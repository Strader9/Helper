// LumiCore 风格 —— 任务中心（活跃任务看板 + 常驻定时任务）
import { useCallback, useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { motion } from 'motion/react';
import { toast } from 'sonner';
import {
  ListTodo,
  RefreshCw,
  Square,
  Zap,
  Inbox,
  LoaderCircle,
  Plus,
  CalendarClock,
  Play,
  Trash2,
  History,
  X,
  Bot,
  CircleCheck,
  PauseCircle,
} from 'lucide-react';
import {
  GlassCard,
  Badge,
  SectionTitle,
  LoadingSpinner,
  EmptyState,
  StatCard,
  ToggleSwitch,
} from '../../components/lumi/LumiShared';
import { LumiPageShell } from '../../components/lumi/LumiPageShell';
import { cn } from '../../lib/cn';

/* ── 类型（与 src-tauri 结构体一一对应） ── */
interface Conversation {
  id: string;
  title: string;
  messageCount?: number;
}

/** agent::Task，状态为 UPPERCASE TaskStatus */
interface AgentTask {
  id: string;
  session_id: string;
  goal: string;
  status: string;
  current_step: number;
  created_at: string;
  updated_at: string;
}

/** scheduler::ScheduledJob */
interface ScheduledJob {
  id: string;
  name: string;
  kind: string;
  schedule: string;
  goal: string;
  enabled: boolean;
  next_run_at: string | null;
  last_run_at: string | null;
  run_count: number;
  max_runs: number | null;
  last_status: string | null;
  last_summary: string | null;
  last_error: string | null;
  created_at: string;
  updated_at: string;
}

/** scheduler::JobRun */
interface JobRun {
  id: string;
  job_id: string;
  started_at: string;
  finished_at: string | null;
  status: string;
  summary: string | null;
  error: string | null;
  steps: number;
  tools_used: string[];
}

/** scheduler::SchedulerStatus */
interface SchedulerStatus {
  running: boolean;
  ticks: number;
  runs: number;
  last_tick: string | null;
  in_flight: string[];
}

type TabKey = 'active' | 'jobs';
type JobKind = 'interval' | 'cron' | 'once';

/* ── 工具 ── */
function fmtTime(v?: string | number | null): string {
  if (v === undefined || v === null || v === '') return '—';
  try {
    const d = typeof v === 'number' ? new Date(v * 1000) : new Date(v.replace(' ', 'T'));
    if (Number.isNaN(d.getTime())) return String(v);
    return d.toLocaleString('zh-CN', {
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
    });
  } catch {
    return String(v);
  }
}

/** TaskStatus 为 UPPERCASE 字符串 */
function taskStatusVariant(s: string): 'info' | 'warning' | 'safe' | 'danger' {
  const v = (s || '').toUpperCase();
  if (v === 'COMPLETED') return 'safe';
  if (v === 'FAILED' || v === 'CANCELLED') return 'danger';
  if (v === 'PENDING') return 'warning';
  return 'info';
}

function taskStatusLabel(s: string): string {
  const v = (s || '').toUpperCase();
  const map: Record<string, string> = {
    PENDING: '等待中',
    ANALYZING: '分析中',
    PLANNING: '规划中',
    EXECUTING: '执行中',
    VERIFYING: '验证中',
    COMPLETED: '已完成',
    FAILED: '失败',
    CANCELLED: '已取消',
  };
  return map[v] || s || '—';
}

function taskIsActive(s: string): boolean {
  const v = (s || '').toUpperCase();
  return ['ANALYZING', 'PLANNING', 'EXECUTING', 'VERIFYING'].includes(v);
}

function taskIsDone(s: string): boolean {
  return (s || '').toUpperCase() === 'COMPLETED';
}

function runStatusVariant(s?: string | null): 'info' | 'warning' | 'safe' | 'danger' {
  if (!s) return 'warning';
  const v = s.toLowerCase();
  if (v.includes('fail') || v.includes('error')) return 'danger';
  if (v.includes('success') || v.includes('done') || v.includes('ok')) return 'safe';
  if (v.includes('running') || v.includes('progress')) return 'info';
  return 'warning';
}

const KIND_LABEL: Record<string, string> = {
  interval: '固定间隔',
  cron: 'Cron',
  once: '一次性',
};

const KIND_HINT: Record<JobKind, string> = {
  interval: '例如 30m / 2h / 90（分钟数）',
  cron: '五段 cron，例如 0 9 * * 1-5',
  once: '例如 2026-10-21 09:00',
};

/* ── 组件 ── */
export default function LumiTasks() {
  const [tab, setTab] = useState<TabKey>('active');

  /* 活跃任务 */
  const [conversations, setConversations] = useState<Conversation[]>([]);
  const [selectedConvId, setSelectedConvId] = useState<string | null>(null);
  const [tasks, setTasks] = useState<AgentTask[]>([]);
  const [activeTask, setActiveTask] = useState<AgentTask | null>(null);
  const [loadingTasks, setLoadingTasks] = useState(true);
  const [cancelling, setCancelling] = useState(false);

  /* 定时任务 */
  const [jobs, setJobs] = useState<ScheduledJob[] | null>(null);
  const [schedStatus, setSchedStatus] = useState<SchedulerStatus | null>(null);
  const [runsByJob, setRunsByJob] = useState<Record<string, JobRun[]>>({});
  const [expandedJobId, setExpandedJobId] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [loadingJobs, setLoadingJobs] = useState(true);

  /* 新建任务弹窗 */
  const [showCreate, setShowCreate] = useState(false);
  const [form, setForm] = useState<{
    name: string;
    kind: JobKind;
    schedule: string;
    goal: string;
    maxRuns: string;
  }>({ name: '', kind: 'interval', schedule: '30m', goal: '', maxRuns: '' });

  /* ── 加载会话列表 ── */
  const loadConversations = useCallback(async () => {
    try {
      const convs = await invoke<Conversation[]>('get_conversations');
      setConversations(convs || []);
    } catch {
      setConversations([]);
    }
  }, []);

  /* ── 加载指定会话的任务 ── */
  const loadTasks = useCallback(async (convId: string) => {
    setLoadingTasks(true);
    try {
      const [rows, active] = await Promise.all([
        invoke<AgentTask[]>('get_tasks', { conversationId: convId, limit: 50 }).catch(() => [] as AgentTask[]),
        invoke<AgentTask | null>('get_active_task', { conversationId: convId }).catch(() => null),
      ]);
      setTasks(rows || []);
      setActiveTask(active || null);
    } finally {
      setLoadingTasks(false);
    }
  }, []);

  /* ── 加载定时任务 + 调度器状态 ── */
  const loadJobs = useCallback(async () => {
    setLoadingJobs(true);
    try {
      const [jobRows, sched] = await Promise.all([
        invoke<ScheduledJob[]>('list_scheduled_jobs').catch(() => [] as ScheduledJob[]),
        invoke<SchedulerStatus>('get_scheduler_status').catch(() => null),
      ]);
      setJobs(jobRows || []);
      setSchedStatus(sched);
    } finally {
      setLoadingJobs(false);
    }
  }, []);

  useEffect(() => {
    void loadConversations();
    void loadJobs();
  }, [loadConversations, loadJobs]);

  /* 会话默认选中第一个 */
  useEffect(() => {
    if (!selectedConvId && conversations.length > 0) {
      setSelectedConvId(conversations[0].id);
    }
  }, [conversations, selectedConvId]);

  /* 选中会话变化时加载任务 */
  useEffect(() => {
    if (selectedConvId) void loadTasks(selectedConvId);
  }, [selectedConvId, loadTasks]);

  /* ── 统计 ── */
  const taskStats = useMemo(() => ({
    total: tasks.length,
    running: tasks.filter((t) => taskIsActive(t.status)).length,
    done: tasks.filter((t) => taskIsDone(t.status)).length,
    failed: tasks.filter((t) => ['FAILED', 'CANCELLED'].includes((t.status || '').toUpperCase())).length,
  }), [tasks]);

  const jobStats = useMemo(() => ({
    total: jobs?.length ?? 0,
    enabled: jobs?.filter((j) => j.enabled).length ?? 0,
    totalRuns: jobs?.reduce((sum, j) => sum + j.run_count, 0) ?? 0,
  }), [jobs]);

  /* ── 取消当前活跃任务 ── */
  const cancelActive = async () => {
    if (!activeTask || !selectedConvId) return;
    setCancelling(true);
    try {
      await invoke('cancel_task', { conversationId: selectedConvId });
      toast.success('已取消当前任务');
      await loadTasks(selectedConvId);
    } catch (e) {
      toast.error(typeof e === 'string' ? e : '取消失败');
    } finally {
      setCancelling(false);
    }
  };

  /* ── 定时任务操作 ── */
  const toggleJob = async (job: ScheduledJob, enabled: boolean) => {
    setBusyId(job.id);
    try {
      await invoke('set_scheduled_job_enabled', { id: job.id, enabled });
      setJobs((prev) => prev?.map((j) => (j.id === job.id ? { ...j, enabled } : j)) ?? prev);
      toast.success(enabled ? `已启用「${job.name}」` : `已暂停「${job.name}」`);
    } catch (e) {
      toast.error(typeof e === 'string' ? e : '切换状态失败');
    } finally {
      setBusyId(null);
    }
  };

  const runNow = async (job: ScheduledJob) => {
    setBusyId(job.id);
    try {
      await invoke('run_scheduled_job_now', { id: job.id });
      toast.success('已标记为立即执行', { description: '调度器下一轮会接手，通常 30 秒内开始' });
      setTimeout(() => void loadJobs(), 1500);
    } catch (e) {
      toast.error(typeof e === 'string' ? e : '触发失败');
    } finally {
      setBusyId(null);
    }
  };

  const removeJob = async (job: ScheduledJob) => {
    if (!window.confirm(`删除定时任务「${job.name || job.id}」？执行历史会一并删除。`)) return;
    setBusyId(job.id);
    try {
      await invoke('delete_scheduled_job', { id: job.id });
      setJobs((prev) => prev?.filter((j) => j.id !== job.id) ?? prev);
      setExpandedJobId((cur) => (cur === job.id ? null : cur));
      toast.success('已删除');
    } catch (e) {
      toast.error(typeof e === 'string' ? e : '删除失败');
    } finally {
      setBusyId(null);
    }
  };

  const toggleRuns = async (job: ScheduledJob) => {
    if (expandedJobId === job.id) {
      setExpandedJobId(null);
      return;
    }
    setExpandedJobId(job.id);
    if (!runsByJob[job.id]) {
      try {
        const rows = await invoke<JobRun[]>('get_scheduled_job_runs', { id: job.id, limit: 10 });
        setRunsByJob((prev) => ({ ...prev, [job.id]: rows || [] }));
      } catch {
        setRunsByJob((prev) => ({ ...prev, [job.id]: [] }));
      }
    }
  };

  /* ── 新建定时任务 ── */
  const createJob = async () => {
    if (!form.name.trim() || !form.goal.trim() || !form.schedule.trim()) {
      toast.error('名称、调度时间和目标都要填');
      return;
    }
    setBusyId('__create__');
    try {
      const maxRunsNum = form.maxRuns.trim() ? Number(form.maxRuns) : null;
      await invoke('create_scheduled_job', {
        name: form.name.trim(),
        kind: form.kind,
        schedule: form.schedule.trim(),
        goal: form.goal.trim(),
        maxRuns: maxRunsNum && maxRunsNum > 0 ? maxRunsNum : null,
      });
      toast.success(`已创建定时任务「${form.name.trim()}」`);
      setShowCreate(false);
      setForm({ name: '', kind: 'interval', schedule: '30m', goal: '', maxRuns: '' });
      void loadJobs();
    } catch (e) {
      toast.error(typeof e === 'string' ? e : '创建失败');
    } finally {
      setBusyId(null);
    }
  };

  const tabs: { key: TabKey; label: string; icon: React.ReactNode }[] = [
    { key: 'active', label: '活跃任务', icon: <Bot size={13} /> },
    { key: 'jobs', label: '定时任务', icon: <CalendarClock size={13} /> },
  ];

  return (
    <LumiPageShell
      icon={<ListTodo size={18} />}
      title="任务中心"
      subtitle="AI 任务看板与常驻定时任务"
      actions={
        <button
          onClick={() => (tab === 'active' && selectedConvId ? loadTasks(selectedConvId) : loadJobs())}
          className="lumi-icon-button"
          title="刷新"
        >
          {(loadingTasks || loadingJobs) ? <LoaderCircle size={15} className="animate-spin" /> : <RefreshCw size={15} />}
        </button>
      }
    >
      {/* Tab 切换 */}
      <div className="mb-5 flex flex-wrap items-center gap-2">
        {tabs.map((t) => (
          <button
            key={t.key}
            onClick={() => setTab(t.key)}
            className={cn(
              'inline-flex h-9 items-center gap-1.5 rounded-xl border px-4 text-xs font-bold transition-colors',
              tab === t.key
                ? 'border-celestial-saturn/40 bg-celestial-saturn/15 text-celestial-saturn'
                : 'border-white/10 bg-white/[0.04] text-white/50 hover:bg-white/10 hover:text-white/80'
            )}
          >
            {t.icon}
            {t.label}
          </button>
        ))}
        {tab === 'jobs' && (
          <button
            onClick={() => setShowCreate(true)}
            className="ml-auto inline-flex h-9 items-center gap-1.5 rounded-xl border border-celestial-saturn/40 bg-celestial-saturn/15 px-4 text-xs font-bold text-celestial-saturn transition-colors hover:bg-celestial-saturn/25"
          >
            <Plus size={14} />
            新建任务
          </button>
        )}
      </div>

      {/* ════════════ 活跃任务 ════════════ */}
      {tab === 'active' && (
        <>
          {/* 会话选择 */}
          <GlassCard hoverEffect={false} className="mb-4 flex flex-wrap items-center gap-3 p-3">
            <span className="text-xs font-bold text-white/50">查看会话</span>
            <select
              value={selectedConvId ?? ''}
              onChange={(e) => setSelectedConvId(e.target.value)}
              className="h-9 min-w-[220px] flex-1 rounded-xl border border-white/10 bg-white/[0.04] px-3 text-xs text-white/85 outline-none focus:border-celestial-saturn/40"
            >
              {conversations.length === 0 && <option value="">暂无会话</option>}
              {conversations.map((c) => (
                <option key={c.id} value={c.id} className="bg-[#1a1a2e]">
                  {c.title || '(未命名会话)'}
                </option>
              ))}
            </select>
            {schedStatus?.running !== undefined && (
              <Badge variant={schedStatus.running ? 'safe' : 'warning'}>
                调度器 {schedStatus.running ? '运行中' : '已停止'}
              </Badge>
            )}
          </GlassCard>

          {/* 统计 */}
          <div className="mb-5 grid grid-cols-2 gap-3 md:grid-cols-4">
            <StatCard label="任务总数" value={taskStats.total} icon={<ListTodo size={16} />} />
            <StatCard label="进行中" value={taskStats.running} icon={<Zap size={16} />} color="text-cyan-400" />
            <StatCard label="已完成" value={taskStats.done} icon={<CircleCheck size={16} />} color="text-emerald-400" />
            <StatCard label="失败/取消" value={taskStats.failed} icon={<PauseCircle size={16} />} color="text-red-400" />
          </div>

          {/* 当前活跃任务高亮 */}
          {activeTask && (
            <motion.div initial={{ opacity: 0, y: -6 }} animate={{ opacity: 1, y: 0 }} className="mb-5">
              <GlassCard glowColor="rgba(34,211,238,0.08)" className="border-cyan-400/20">
                <div className="flex flex-wrap items-center gap-3">
                  <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-cyan-400/10 text-cyan-400">
                    <LoaderCircle size={18} className="animate-spin" />
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="text-[10px] font-bold uppercase tracking-widest text-cyan-400/80">当前活跃任务</p>
                    <p className="truncate text-sm font-bold text-white/90">{activeTask.goal}</p>
                    <p className="mt-0.5 text-[11px] text-white/40">
                      步骤 {activeTask.current_step + 1} · 开始于 {fmtTime(activeTask.created_at)}
                    </p>
                  </div>
                  <Badge variant="info">{taskStatusLabel(activeTask.status)}</Badge>
                  <button
                    onClick={cancelActive}
                    disabled={cancelling}
                    className="inline-flex h-9 items-center gap-1.5 rounded-xl border border-red-500/30 bg-red-500/10 px-3 text-xs font-bold text-red-400 transition-colors hover:bg-red-500/20 disabled:opacity-40"
                  >
                    <Square size={13} />
                    {cancelling ? '取消中…' : '取消任务'}
                  </button>
                </div>
              </GlassCard>
            </motion.div>
          )}

          {/* 任务列表 */}
          {loadingTasks ? (
            <LoadingSpinner label="正在加载任务…" />
          ) : tasks.length === 0 ? (
            <GlassCard hoverEffect={false}>
              <EmptyState
                icon={<Inbox size={28} />}
                title="暂无任务"
                description={selectedConvId ? '这个会话还没有任务记录。' : '请先在对话中发起一次任务。'}
              />
            </GlassCard>
          ) : (
            <div className="mb-6 flex flex-col gap-2.5">
              {tasks.map((task, i) => (
                <motion.div
                  key={task.id}
                  initial={{ opacity: 0, y: 8 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={{ delay: Math.min(i * 0.03, 0.3), duration: 0.25 }}
                >
                  <GlassCard hoverEffect={false} className={cn('p-4', taskIsActive(task.status) && 'border-cyan-400/20')}>
                    <div className="flex flex-wrap items-center gap-3">
                      <div className="min-w-0 flex-1">
                        <p className="truncate text-sm font-bold text-white/90">{task.goal}</p>
                        <p className="mt-0.5 text-[11px] text-white/40">
                          创建于 {fmtTime(task.created_at)} · 更新于 {fmtTime(task.updated_at)}
                        </p>
                      </div>
                      <Badge variant={taskStatusVariant(task.status)}>{taskStatusLabel(task.status)}</Badge>
                    </div>
                  </GlassCard>
                </motion.div>
              ))}
            </div>
          )}
        </>
      )}

      {/* ════════════ 定时任务 ════════════ */}
      {tab === 'jobs' && (
        <>
          {/* 调度器状态 + 统计 */}
          <div className="mb-5 grid grid-cols-2 gap-3 md:grid-cols-4">
            <StatCard label="定时任务" value={jobStats.total} icon={<CalendarClock size={16} />} />
            <StatCard label="启用中" value={jobStats.enabled} icon={<Zap size={16} />} color="text-emerald-400" />
            <StatCard label="累计执行" value={jobStats.totalRuns} icon={<Play size={16} />} color="text-cyan-400" />
            <StatCard
              label="调度器"
              value={schedStatus?.running ? '运行中' : '已停止'}
              icon={<LoaderCircle size={16} className={schedStatus?.running ? 'animate-spin' : ''} />}
              color={schedStatus?.running ? 'text-emerald-400' : 'text-amber-400'}
            />
          </div>

          {schedStatus && (
            <GlassCard hoverEffect={false} className="mb-4 flex flex-wrap items-center gap-x-6 gap-y-1 p-3 text-[11px] text-white/50">
              <span>调度 ticks：<span className="font-mono text-white/80">{schedStatus.ticks}</span></span>
              <span>累计轮次：<span className="font-mono text-white/80">{schedStatus.runs}</span></span>
              <span>最近心跳：<span className="font-mono text-white/80">{fmtTime(schedStatus.last_tick)}</span></span>
              <span>
                执行中：
                <span className="font-mono text-white/80">{schedStatus.in_flight.length}</span>
              </span>
            </GlassCard>
          )}

          <SectionTitle label="常驻定时任务" count={jobs?.length ?? 0} color="bg-celestial-saturn" />

          {loadingJobs ? (
            <LoadingSpinner label="正在加载定时任务…" />
          ) : !jobs || jobs.length === 0 ? (
            <GlassCard hoverEffect={false}>
              <EmptyState
                icon={<CalendarClock size={28} />}
                title="还没有定时任务"
                description="例如「每天早上 9 点汇总昨天的工作」「每 2 小时检查一次磁盘空间」"
              />
            </GlassCard>
          ) : (
            <div className="mb-6 flex flex-col gap-2.5">
              {jobs.map((job, i) => (
                <motion.div
                  key={job.id}
                  initial={{ opacity: 0, y: 8 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={{ delay: Math.min(i * 0.03, 0.3), duration: 0.25 }}
                >
                  <GlassCard hoverEffect={false} className="p-4">
                    {/* 主行 */}
                    <div className="flex flex-wrap items-center gap-3">
                      <div className="min-w-0 flex-1">
                        <div className="flex flex-wrap items-center gap-2">
                          <p className="truncate text-sm font-bold text-white/90">{job.name || '(未命名)'}</p>
                          <Badge variant="info">{KIND_LABEL[job.kind] ?? job.kind}</Badge>
                          {!job.enabled && <Badge variant="warning">已暂停</Badge>}
                          {job.last_status && (
                            <Badge variant={runStatusVariant(job.last_status)}>
                              上次：{job.last_status}
                            </Badge>
                          )}
                        </div>
                        <p className="mt-1 truncate text-[11px] text-white/50">
                          <span className="font-mono text-celestial-saturn/80">{job.schedule}</span>
                          <span className="mx-1.5 opacity-40">·</span>
                          {job.goal}
                        </p>
                        <p className="mt-1 text-[11px] text-white/35">
                          下次：{fmtTime(job.next_run_at)} · 上次：{fmtTime(job.last_run_at)} · 已跑 {job.run_count} 次
                          {job.max_runs ? ` / 上限 ${job.max_runs}` : ''}
                        </p>
                      </div>

                      {/* 操作按钮 */}
                      <div className="flex shrink-0 items-center gap-1.5">
                        <ToggleSwitch
                          enabled={job.enabled}
                          disabled={busyId === job.id}
                          onChange={() => toggleJob(job, !job.enabled)}
                        />
                        <button
                          onClick={() => runNow(job)}
                          disabled={busyId === job.id || !job.enabled}
                          title="立即执行一次"
                          className="inline-flex h-8 w-8 items-center justify-center rounded-lg border border-white/10 bg-white/[0.04] text-white/60 transition-colors hover:bg-white/10 hover:text-emerald-400 disabled:opacity-40"
                        >
                          {busyId === job.id ? <LoaderCircle size={14} className="animate-spin" /> : <Play size={14} />}
                        </button>
                        <button
                          onClick={() => void toggleRuns(job)}
                          title="执行历史"
                          className={cn(
                            'inline-flex h-8 w-8 items-center justify-center rounded-lg border transition-colors',
                            expandedJobId === job.id
                              ? 'border-celestial-saturn/40 bg-celestial-saturn/15 text-celestial-saturn'
                              : 'border-white/10 bg-white/[0.04] text-white/60 hover:bg-white/10 hover:text-white/80'
                          )}
                        >
                          <History size={14} />
                        </button>
                        <button
                          onClick={() => void removeJob(job)}
                          disabled={busyId === job.id}
                          title="删除"
                          className="inline-flex h-8 w-8 items-center justify-center rounded-lg border border-white/10 bg-white/[0.04] text-white/60 transition-colors hover:border-red-500/30 hover:bg-red-500/10 hover:text-red-400 disabled:opacity-40"
                        >
                          <Trash2 size={14} />
                        </button>
                      </div>
                    </div>

                    {/* 展开：执行历史 */}
                    {expandedJobId === job.id && (
                      <motion.div
                        initial={{ opacity: 0, height: 0 }}
                        animate={{ opacity: 1, height: 'auto' }}
                        className="mt-3 overflow-hidden rounded-xl border border-white/[0.06] bg-white/[0.02] p-3"
                      >
                        <p className="mb-2 text-[10px] font-bold uppercase tracking-widest text-white/40">最近执行记录</p>
                        {!runsByJob[job.id] ? (
                          <p className="text-[11px] text-white/40">加载中…</p>
                        ) : runsByJob[job.id].length === 0 ? (
                          <p className="text-[11px] text-white/40">还没有执行记录</p>
                        ) : (
                          <div className="flex flex-col gap-1.5">
                            {runsByJob[job.id].map((r) => (
                              <div key={r.id} className="flex flex-wrap items-center gap-2 text-[11px]">
                                <Badge variant={runStatusVariant(r.status)}>{r.status}</Badge>
                                <span className="shrink-0 text-white/40">
                                  {fmtTime(r.started_at)} → {fmtTime(r.finished_at)}
                                </span>
                                <span className="min-w-0 flex-1 truncate text-white/60">
                                  {r.summary || r.error || `执行 ${r.steps} 步 · 工具：${r.tools_used.join(', ')}`}
                                </span>
                              </div>
                            ))}
                          </div>
                        )}
                        {job.last_error && (
                          <p className="mt-2 break-all text-[11px] text-red-400/80">最近错误：{job.last_error}</p>
                        )}
                      </motion.div>
                    )}
                  </GlassCard>
                </motion.div>
              ))}
            </div>
          )}
        </>
      )}

      {/* ════════════ 新建定时任务弹窗 ════════════ */}
      {showCreate && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-sm"
          onClick={() => setShowCreate(false)}
        >
          <motion.div
            initial={{ opacity: 0, scale: 0.96 }}
            animate={{ opacity: 1, scale: 1 }}
            onClick={(e) => e.stopPropagation()}
            className="w-full max-w-lg rounded-2xl border border-white/10 bg-[#15152a] p-5 shadow-2xl"
          >
            <div className="mb-4 flex items-center justify-between">
              <div className="flex items-center gap-2">
                <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-celestial-saturn/15 text-celestial-saturn">
                  <CalendarClock size={16} />
                </div>
                <p className="text-sm font-bold text-white/90">新建定时任务</p>
              </div>
              <button
                onClick={() => setShowCreate(false)}
                className="flex h-8 w-8 items-center justify-center rounded-lg text-white/40 hover:bg-white/10 hover:text-white/80"
              >
                <X size={16} />
              </button>
            </div>

            <div className="flex flex-col gap-3.5">
              <div>
                <label className="mb-1 block text-[11px] font-bold text-white/50">任务名称</label>
                <input
                  value={form.name}
                  onChange={(e) => setForm({ ...form, name: e.target.value })}
                  placeholder="例如：早间系统体检"
                  className="h-10 w-full rounded-xl border border-white/10 bg-white/[0.04] px-3 text-sm text-white/90 outline-none focus:border-celestial-saturn/40"
                />
              </div>

              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="mb-1 block text-[11px] font-bold text-white/50">触发方式</label>
                  <select
                    value={form.kind}
                    onChange={(e) => setForm({ ...form, kind: e.target.value as JobKind })}
                    className="h-10 w-full rounded-xl border border-white/10 bg-white/[0.04] px-3 text-sm text-white/90 outline-none focus:border-celestial-saturn/40"
                  >
                    <option value="interval" className="bg-[#1a1a2e]">固定间隔</option>
                    <option value="cron" className="bg-[#1a1a2e]">Cron 表达式</option>
                    <option value="once" className="bg-[#1a1a2e]">一次性</option>
                  </select>
                </div>
                <div>
                  <label className="mb-1 block text-[11px] font-bold text-white/50">最多执行次数（可选）</label>
                  <input
                    value={form.maxRuns}
                    onChange={(e) => setForm({ ...form, maxRuns: e.target.value.replace(/[^0-9]/g, '') })}
                    placeholder="不限"
                    className="h-10 w-full rounded-xl border border-white/10 bg-white/[0.04] px-3 text-sm text-white/90 outline-none focus:border-celestial-saturn/40"
                  />
                </div>
              </div>

              <div>
                <label className="mb-1 block text-[11px] font-bold text-white/50">调度时间</label>
                <input
                  value={form.schedule}
                  onChange={(e) => setForm({ ...form, schedule: e.target.value })}
                  placeholder={KIND_HINT[form.kind]}
                  className="h-10 w-full rounded-xl border border-white/10 bg-white/[0.04] px-3 font-mono text-sm text-white/90 outline-none focus:border-celestial-saturn/40"
                />
                <p className="mt-1 text-[10px] text-white/35">{KIND_HINT[form.kind]}</p>
              </div>

              <div>
                <label className="mb-1 block text-[11px] font-bold text-white/50">任务目标 / 提示词</label>
                <textarea
                  value={form.goal}
                  onChange={(e) => setForm({ ...form, goal: e.target.value })}
                  placeholder="检查磁盘剩余空间，低于 10% 时告诉我哪些目录最占地方"
                  rows={4}
                  className="w-full resize-none rounded-xl border border-white/10 bg-white/[0.04] px-3 py-2.5 text-sm text-white/90 outline-none focus:border-celestial-saturn/40"
                />
              </div>
            </div>

            <div className="mt-5 flex justify-end gap-2">
              <button
                onClick={() => setShowCreate(false)}
                className="inline-flex h-10 items-center rounded-xl border border-white/10 bg-white/[0.04] px-4 text-xs font-bold text-white/60 hover:bg-white/10"
              >
                取消
              </button>
              <button
                onClick={createJob}
                disabled={busyId === '__create__'}
                className="inline-flex h-10 items-center gap-1.5 rounded-xl border border-celestial-saturn/40 bg-celestial-saturn/15 px-4 text-xs font-bold text-celestial-saturn hover:bg-celestial-saturn/25 disabled:opacity-40"
              >
                {busyId === '__create__' ? <LoaderCircle size={14} className="animate-spin" /> : <Plus size={14} />}
                创建任务
              </button>
            </div>
          </motion.div>
        </div>
      )}
    </LumiPageShell>
  );
}
