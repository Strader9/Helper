// EAssistantCore 风格 —— 自动化中心（主动规则 + 定时任务）
import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { motion } from 'motion/react';
import {
  Zap,
  CalendarClock,
  RefreshCw,
  LoaderCircle,
  Plus,
  Trash,
  Play,
  Power,
  Webhook,
  Clock3,
} from 'lucide-react';
import {
  GlassCard,
  Badge,
  ToggleSwitch,
  SectionTitle,
  LoadingSpinner,
  EmptyState,
  StatCard,
} from '../../components/e-assistant/EAssistantShared';
import { EAssistantPageShell } from '../../components/e-assistant/EAssistantPageShell';
import { cn } from '../../lib/cn';

/* ── 类型 ── */
interface ProactiveRule {
  id: string;
  name: string;
  trigger: string;
  action: string;
  enabled: boolean;
  last_triggered?: string | null;
}

interface ScheduledJob {
  id: string;
  name: string;
  cron: string;
  enabled: boolean;
  last_run?: string | null;
  next_run?: string | null;
}

interface ProactiveStatus {
  active: boolean;
  rules_count: number;
  today_triggered: number;
}

type TabKey = 'rules' | 'jobs';

/* ── 工具 ── */
function fmtTime(v?: string | null): string {
  if (!v) return '—';
  try {
    const d = new Date(v);
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

/* ── 组件 ── */
export default function EAssistantAutomation() {
  const [rules, setRules] = useState<ProactiveRule[]>([]);
  const [jobs, setJobs] = useState<ScheduledJob[]>([]);
  const [status, setStatus] = useState<ProactiveStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [tab, setTab] = useState<TabKey>('rules');

  // 新增规则表单
  const [ruleForm, setRuleForm] = useState({ name: '', trigger: '', action: '' });
  const [ruleSaving, setRuleSaving] = useState(false);
  // 新增任务表单
  const [jobForm, setJobForm] = useState({ name: '', cron: '', action: '' });
  const [jobSaving, setJobSaving] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    const [ruleRows, jobRows, st] = await Promise.all([
      invoke<ProactiveRule[]>('get_proactive_rules').catch(() => [] as ProactiveRule[]),
      invoke<ScheduledJob[]>('list_scheduled_jobs').catch(() => [] as ScheduledJob[]),
      invoke<ProactiveStatus>('get_proactive_status').catch(() => null),
    ]);
    setRules(ruleRows || []);
    setJobs(jobRows || []);
    setStatus(st);
    setLoading(false);
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /* ── 规则操作 ── */
  const toggleRule = async (r: ProactiveRule) => {
    setRules((prev) => prev.map((x) => (x.id === r.id ? { ...x, enabled: !x.enabled } : x)));
    try {
      await invoke('toggle_proactive_rule', { id: r.id, enabled: !r.enabled });
    } catch {
      void load();
    }
  };

  const deleteRule = async (id: string) => {
    setRules((prev) => prev.filter((x) => x.id !== id));
    try {
      await invoke('delete_proactive_rule', { id });
    } catch {
      void load();
    }
  };

  const addRule = async () => {
    if (!ruleForm.name.trim() || !ruleForm.trigger.trim()) return;
    setRuleSaving(true);
    try {
      await invoke('add_proactive_rule', {
        name: ruleForm.name.trim(),
        trigger: ruleForm.trigger.trim(),
        action: ruleForm.action.trim() || 'notify',
      });
      setRuleForm({ name: '', trigger: '', action: '' });
      await load();
    } catch {
      /* 降级：静默 */
    }
    setRuleSaving(false);
  };

  /* ── 定时任务操作 ── */
  const toggleJob = async (j: ScheduledJob) => {
    setJobs((prev) => prev.map((x) => (x.id === j.id ? { ...x, enabled: !x.enabled } : x)));
    try {
      await invoke('set_scheduled_job_enabled', { id: j.id, enabled: !j.enabled });
    } catch {
      void load();
    }
  };

  const deleteJob = async (id: string) => {
    setJobs((prev) => prev.filter((x) => x.id !== id));
    try {
      await invoke('delete_scheduled_job', { id });
    } catch {
      void load();
    }
  };

  const runJobNow = async (id: string) => {
    try {
      await invoke('run_scheduled_job_now', { id });
    } catch {
      /* 降级 */
    }
    void load();
  };

  const addJob = async () => {
    if (!jobForm.name.trim() || !jobForm.cron.trim()) return;
    setJobSaving(true);
    try {
      await invoke('create_scheduled_job', {
        name: jobForm.name.trim(),
        cron: jobForm.cron.trim(),
        action: jobForm.action.trim() || 'notify',
      });
      setJobForm({ name: '', cron: '', action: '' });
      await load();
    } catch {
      /* 降级 */
    }
    setJobSaving(false);
  };

  return (
    <EAssistantPageShell
      icon={<Zap size={18} />}
      title="自动化中心"
      subtitle="主动规则与定时任务"
      actions={
        <button onClick={load} className="eassistant-icon-button" title="刷新">
          {loading ? <LoaderCircle size={15} className="animate-spin" /> : <RefreshCw size={15} />}
        </button>
      }
    >
      {/* 状态总览 */}
      <div className="mb-5 grid grid-cols-2 gap-3 md:grid-cols-4">
        <StatCard
          label="自动化状态"
          value={status?.active ? '运行中' : '待机'}
          icon={<Power size={16} />}
          color={status?.active ? 'text-emerald-400' : 'text-white/60'}
        />
        <StatCard label="规则总数" value={status?.rules_count ?? rules.length} icon={<Webhook size={16} />} />
        <StatCard label="今日触发" value={status?.today_triggered ?? 0} icon={<Zap size={16} />} color="text-cyan-400" />
        <StatCard label="定时任务" value={jobs.length} icon={<CalendarClock size={16} />} color="text-celestial-saturn" />
      </div>

      {/* Tab 切换 */}
      <div className="mb-4 flex gap-2">
        {(
          [
            { key: 'rules', label: `主动规则 (${rules.length})`, icon: <Webhook size={14} /> },
            { key: 'jobs', label: `定时任务 (${jobs.length})`, icon: <Clock3 size={14} /> },
          ] as const
        ).map((t) => (
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
      </div>

      {loading ? (
        <LoadingSpinner label="正在加载自动化配置…" />
      ) : tab === 'rules' ? (
        <div>
          <SectionTitle label="主动规则" count={rules.length} />
          {rules.length === 0 ? (
            <GlassCard hoverEffect={false}>
              <EmptyState icon={<Webhook size={28} />} title="暂无主动规则" description="创建规则后，AI 会在条件满足时自动执行动作。" />
            </GlassCard>
          ) : (
            <div className="mb-5 flex flex-col gap-2.5">
              {rules.map((r, i) => (
                <motion.div
                  key={r.id}
                  initial={{ opacity: 0, y: 8 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={{ delay: Math.min(i * 0.03, 0.3), duration: 0.25 }}
                >
                  <GlassCard hoverEffect={false} className="p-4">
                    <div className="flex flex-wrap items-center gap-3">
                      <ToggleSwitch enabled={r.enabled} onChange={() => void toggleRule(r)} />
                      <div className="min-w-0 flex-1">
                        <p className="text-sm font-bold text-white/90">{r.name}</p>
                        <p className="mt-0.5 text-[11px] text-white/40">
                          触发：{r.trigger} · 动作：{r.action}
                        </p>
                      </div>
                      <span className="text-[11px] text-white/35">上次 {fmtTime(r.last_triggered || null)}</span>
                      <Badge variant={r.enabled ? 'safe' : 'neutral'}>{r.enabled ? '启用' : '停用'}</Badge>
                      <button onClick={() => void deleteRule(r.id)} className="eassistant-icon-button" title="删除">
                        <Trash size={14} />
                      </button>
                    </div>
                  </GlassCard>
                </motion.div>
              ))}
            </div>
          )}

          {/* 新增规则 */}
          <SectionTitle label="新增规则" />
          <GlassCard hoverEffect={false}>
            <div className="grid grid-cols-1 gap-2 md:grid-cols-3">
              <input
                value={ruleForm.name}
                onChange={(e) => setRuleForm({ ...ruleForm, name: e.target.value })}
                placeholder="规则名称，如：磁盘告警"
                className="eassistant-field h-9 text-xs"
              />
              <input
                value={ruleForm.trigger}
                onChange={(e) => setRuleForm({ ...ruleForm, trigger: e.target.value })}
                placeholder="触发条件，如：disk_usage > 90%"
                className="eassistant-field h-9 text-xs"
              />
              <input
                value={ruleForm.action}
                onChange={(e) => setRuleForm({ ...ruleForm, action: e.target.value })}
                placeholder="执行动作，如：notify"
                className="eassistant-field h-9 text-xs"
              />
            </div>
            <button
              onClick={() => void addRule()}
              disabled={ruleSaving || !ruleForm.name.trim() || !ruleForm.trigger.trim()}
              className="eassistant-button-primary mt-3"
            >
              <Plus size={14} />
              {ruleSaving ? '创建中…' : '创建规则'}
            </button>
          </GlassCard>
        </div>
      ) : (
        <div>
          <SectionTitle label="定时任务" count={jobs.length} />
          {jobs.length === 0 ? (
            <GlassCard hoverEffect={false}>
              <EmptyState icon={<CalendarClock size={28} />} title="暂无定时任务" description="按 cron 表达式创建周期性执行的任务。" />
            </GlassCard>
          ) : (
            <div className="mb-5 flex flex-col gap-2.5">
              {jobs.map((j, i) => (
                <motion.div
                  key={j.id}
                  initial={{ opacity: 0, y: 8 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={{ delay: Math.min(i * 0.03, 0.3), duration: 0.25 }}
                >
                  <GlassCard hoverEffect={false} className="p-4">
                    <div className="flex flex-wrap items-center gap-3">
                      <ToggleSwitch enabled={j.enabled} onChange={() => void toggleJob(j)} />
                      <div className="min-w-0 flex-1">
                        <p className="text-sm font-bold text-white/90">{j.name}</p>
                        <p className="mt-0.5 text-[11px] text-white/40">
                          cron：<span className="font-mono text-celestial-saturn/80">{j.cron}</span>
                        </p>
                      </div>
                      <div className="text-right text-[11px] leading-4 text-white/35">
                        <div>上次 {fmtTime(j.last_run || null)}</div>
                        <div>下次 {fmtTime(j.next_run || null)}</div>
                      </div>
                      <button onClick={() => void runJobNow(j.id)} className="eassistant-icon-button" title="立即运行">
                        <Play size={14} />
                      </button>
                      <button onClick={() => void deleteJob(j.id)} className="eassistant-icon-button" title="删除">
                        <Trash size={14} />
                      </button>
                    </div>
                  </GlassCard>
                </motion.div>
              ))}
            </div>
          )}

          {/* 新增任务 */}
          <SectionTitle label="新增定时任务" />
          <GlassCard hoverEffect={false}>
            <div className="grid grid-cols-1 gap-2 md:grid-cols-3">
              <input
                value={jobForm.name}
                onChange={(e) => setJobForm({ ...jobForm, name: e.target.value })}
                placeholder="任务名称，如：每日备份"
                className="eassistant-field h-9 text-xs"
              />
              <input
                value={jobForm.cron}
                onChange={(e) => setJobForm({ ...jobForm, cron: e.target.value })}
                placeholder="cron 表达式，如：0 9 * * *"
                className="eassistant-field h-9 font-mono text-xs"
              />
              <input
                value={jobForm.action}
                onChange={(e) => setJobForm({ ...jobForm, action: e.target.value })}
                placeholder="执行动作，如：notify"
                className="eassistant-field h-9 text-xs"
              />
            </div>
            <button
              onClick={() => void addJob()}
              disabled={jobSaving || !jobForm.name.trim() || !jobForm.cron.trim()}
              className="eassistant-button-primary mt-3"
            >
              <Plus size={14} />
              {jobSaving ? '创建中…' : '创建任务'}
            </button>
          </GlassCard>
        </div>
      )}
    </EAssistantPageShell>
  );
}
