import { useCallback, useEffect, useMemo, useState } from 'react';
import { motion, AnimatePresence } from 'motion/react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import {
  Brain,
  Search,
  Trash2,
  Star,
  Inbox,
  Plus,
  Clock,
  Pencil,
  X,
  Sparkles,
  Activity,
  BarChart3,
  ScanLine,
  ChevronDown,
} from 'lucide-react';
import { LumiPageShell } from '../../components/lumi/LumiPageShell';
import { LoadingSpinner, EmptyState } from '../../components/lumi/LumiShared';
import { MemoryCoreOrb } from '../../components/lumi/MemoryCoreOrb';
import { MemoryRadar, computeRadarValues, type RadarDimensions } from '../../components/lumi/MemoryRadar';
import { cn } from '../../lib/cn';

/* ── 类型（与后端 MemoryItemResponse 对齐）───────────────
   后端字段：id, memory_type, content, importance(0-100),
   access_count, created_at, last_accessed_at, source
───────────────────────────────────────────────────── */
interface MemoryItem {
  id: string;
  memory_type: string;
  content: string;
  importance: number;
  access_count: number;
  created_at: string;
  last_accessed_at: string;
  source: string;
}

interface ProfileEvidence {
  id: string;
  content: string;
  importance: number;
  confidence: number;
  source: string;
  updated_at: string;
  access_count: number;
}

interface AdaptationProfile {
  generated_at: string;
  total_memories: number;
  active_memories: number;
  durable_memories: number;
  episodic_memories: number;
  consolidated_memories: number;
  coverage_score: number;
  preferences: ProfileEvidence[];
  habits: ProfileEvidence[];
  facts: ProfileEvidence[];
  recent_experiences: ProfileEvidence[];
}

/* ── 映射工具 ───────────────────────────────────────── */
function importanceToStars(imp: number | undefined): number {
  const v = typeof imp === 'number' && Number.isFinite(imp) ? imp : 50;
  return Math.max(1, Math.min(5, Math.round(v / 20)));
}
function starsToImportance(stars: number): number {
  return stars * 20;
}

const TYPE_META: Record<string, { label: string; chip: string; dot: string; bar: string }> = {
  knowledge: {
    label: '知识',
    chip: 'border-cyan-400/25 bg-cyan-400/10 text-cyan-300',
    dot: 'border-cyan-400 bg-cyan-400/30',
    bar: 'bg-cyan-400',
  },
  user_preference: {
    label: '偏好',
    chip: 'border-violet-400/25 bg-violet-400/10 text-violet-300',
    dot: 'border-violet-400 bg-violet-400/30',
    bar: 'bg-violet-400',
  },
  task_history: {
    label: '任务历史',
    chip: 'border-amber-400/25 bg-amber-400/10 text-amber-300',
    dot: 'border-amber-400 bg-amber-400/30',
    bar: 'bg-amber-400',
  },
  conversation_summary: {
    label: '对话摘要',
    chip: 'border-emerald-400/25 bg-emerald-400/10 text-emerald-300',
    dot: 'border-emerald-400 bg-emerald-400/30',
    bar: 'bg-emerald-400',
  },
  app_usage: {
    label: '应用习惯',
    chip: 'border-sky-400/25 bg-sky-400/10 text-sky-300',
    dot: 'border-sky-400 bg-sky-400/30',
    bar: 'bg-sky-400',
  },
};

const TYPE_OPTIONS = ['knowledge', 'user_preference', 'task_history', 'conversation_summary', 'app_usage'];

function typeLabel(t: string): string {
  return TYPE_META[t]?.label ?? t;
}
function typeChipCls(t: string): string {
  return TYPE_META[t]?.chip ?? 'border-white/15 bg-white/5 text-white/60';
}

function isToday(iso: string): boolean {
  try {
    const d = new Date(iso);
    const now = new Date();
    return (
      d.getFullYear() === now.getFullYear() &&
      d.getMonth() === now.getMonth() &&
      d.getDate() === now.getDate()
    );
  } catch {
    return false;
  }
}

function formatTime(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleString('zh-CN', {
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
    });
  } catch {
    return iso;
  }
}

function dateGroupKey(iso: string): string {
  try {
    const d = new Date(iso);
    const now = new Date();
    const startOfDay = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
    const diff = Math.round((startOfDay(now) - startOfDay(d)) / 86400000);
    if (diff <= 0) return '今天';
    if (diff === 1) return '昨天';
    if (diff < 7) return `${diff} 天前`;
    return d.toLocaleDateString('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit' });
  } catch {
    return '更早';
  }
}

type ImportanceFilter = 'all' | 'high' | 'medium' | 'low';
const FILTER_META: Record<ImportanceFilter, { label: string; match: (imp: number) => boolean }> = {
  all: { label: '全部', match: () => true },
  high: { label: '高', match: (imp) => imp >= 60 },
  medium: { label: '中', match: (imp) => imp >= 40 && imp < 60 },
  low: { label: '低', match: (imp) => imp < 40 },
};

export default function LumiMemory() {
  const [memories, setMemories] = useState<MemoryItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState('');
  const [typeFilter, setTypeFilter] = useState<string>('all');
  const [importanceFilter, setImportanceFilter] = useState<ImportanceFilter>('all');

  // 新增表单
  const [newContent, setNewContent] = useState('');
  const [newType, setNewType] = useState('knowledge');
  const [newStars, setNewStars] = useState(3);
  const [submitting, setSubmitting] = useState(false);

  // 删除二次确认
  const [confirmingId, setConfirmingId] = useState<string | null>(null);

  // 编辑弹窗
  const [editing, setEditing] = useState<MemoryItem | null>(null);
  const [editContent, setEditContent] = useState('');
  const [editType, setEditType] = useState('knowledge');
  const [editStars, setEditStars] = useState(3);
  const [editSaving, setEditSaving] = useState(false);

  /* ── 动态可视化状态 ─────────────────────────────────── */
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [pulseKey, setPulseKey] = useState(0);
  const [analysisOpen, setAnalysisOpen] = useState(false);
  const [analyzing, setAnalyzing] = useState(false);
  const [profile, setProfile] = useState<AdaptationProfile | null>(null);
  const [analysisNote, setAnalysisNote] = useState<string | null>(null);

  /* ── 数据加载 ───────────────────────────────────────── */
  const loadProfile = useCallback(async () => {
    try {
      setProfile(await invoke<AdaptationProfile>('get_memory_profile'));
    } catch {
      setProfile(null);
    }
  }, []);

  const loadMemories = async (searchQuery?: string) => {
    setLoading(true);
    try {
      const q = (searchQuery ?? query).trim();
      const list = q
        ? await invoke<MemoryItem[]>('search_memories', { query: q, limit: 100 })
        : await invoke<MemoryItem[]>('get_memories', { limit: 100 });
      setMemories(Array.isArray(list) ? list : []);
      if (!q) void loadProfile();
    } catch (e) {
      setMemories([]);
      toast.error(`记忆加载失败：${typeof e === 'string' ? e : '未知错误'}`);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    const timer = setTimeout(() => {
      loadMemories(query);
    }, 300);
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query]);

  useEffect(() => {
    if (!confirmingId) return;
    const t = setTimeout(() => setConfirmingId(null), 3000);
    return () => clearTimeout(t);
  }, [confirmingId]);

  /* ── 统计 ─────────────────────────────────────────── */
  const stats = useMemo(() => {
    let today = 0;
    let high = 0;
    let impSum = 0;
    memories.forEach((m) => {
      if (isToday(m.created_at)) today += 1;
      if (m.importance >= 60) high += 1;
      impSum += m.importance;
    });
    return {
      total: memories.length,
      today,
      high,
      avgImp: memories.length > 0 ? Math.round(impSum / memories.length) : 0,
    };
  }, [memories]);

  /* ── 雷达图数据 ────────────────────────────────────── */
  const radarValues: RadarDimensions = useMemo(() => computeRadarValues(memories), [memories]);

  /* ── 前端筛选 ───────────────────────────────────────── */
  const filtered = useMemo(() => {
    return memories.filter((m) => {
      if (typeFilter !== 'all' && m.memory_type !== typeFilter) return false;
      return FILTER_META[importanceFilter].match(m.importance);
    });
  }, [memories, typeFilter, importanceFilter]);

  /* ── 按日期分组 ────────────────────────────────────── */
  const grouped = useMemo(() => {
    const order: string[] = [];
    const map = new Map<string, MemoryItem[]>();
    filtered.forEach((m) => {
      const key = dateGroupKey(m.created_at);
      if (!map.has(key)) {
        map.set(key, []);
        order.push(key);
      }
      map.get(key)!.push(m);
    });
    return order.map((k) => ({ key: k, items: map.get(k)! }));
  }, [filtered]);

  /* ── 选中记忆的类型（驱动 Orb 配色）─────────────────── */
  const selectedType = useMemo(() => {
    if (!selectedId) return null;
    return memories.find((m) => m.id === selectedId)?.memory_type ?? null;
  }, [selectedId, memories]);

  /* ── 操作：选中记忆（触发 Orb 脉冲）─────────────────── */
  const selectMemory = (m: MemoryItem) => {
    setSelectedId((prev) => (prev === m.id ? null : m.id));
    setPulseKey((k) => k + 1);
  };

  /* ── 操作：新增 ─────────────────────────────────────── */
  const addMemory = async () => {
    const content = newContent.trim();
    if (!content) {
      toast.warning('请输入记忆内容');
      return;
    }
    setSubmitting(true);
    try {
      await invoke<string>('add_memory', {
        content,
        memory_type: newType,
        importance: starsToImportance(newStars),
      });
      toast.success('记忆已保存');
      setNewContent('');
      setNewStars(3);
      setPulseKey((k) => k + 1);
      await loadMemories();
      await loadProfile();
    } catch (e) {
      toast.error(`保存失败：${typeof e === 'string' ? e : '未知错误'}`);
    } finally {
      setSubmitting(false);
    }
  };

  /* ── 操作：删除 ─────────────────────────────────────── */
  const handleDelete = async (m: MemoryItem) => {
    if (confirmingId !== m.id) {
      setConfirmingId(m.id);
      return;
    }
    try {
      await invoke<boolean>('delete_memory', { memory_id: m.id });
      setMemories((prev) => prev.filter((item) => item.id !== m.id));
      if (selectedId === m.id) setSelectedId(null);
      setConfirmingId(null);
      await loadProfile();
      toast.success('记忆已删除');
    } catch (e) {
      toast.error(`删除失败：${typeof e === 'string' ? e : '未知错误'}`);
    }
  };

  /* ── 操作：编辑 ─────────────────────────────────────── */
  const openEdit = (m: MemoryItem) => {
    setEditing(m);
    setEditContent(m.content);
    setEditType(m.memory_type);
    setEditStars(importanceToStars(m.importance));
  };

  const saveEdit = async () => {
    if (!editing) return;
    const content = editContent.trim();
    if (!content) {
      toast.warning('内容不能为空');
      return;
    }
    setEditSaving(true);
    try {
      await invoke<boolean>('update_memory_item', {
        memory_id: editing.id,
        content,
        memory_type: editType,
        importance: starsToImportance(editStars),
      });
      toast.success('记忆已更新');
      setEditing(null);
      await loadMemories();
      await loadProfile();
    } catch (e) {
      toast.error(`更新失败：${typeof e === 'string' ? e : '未知错误'}`);
    } finally {
      setEditSaving(false);
    }
  };

  /* ── 操作：执行后端真实固化并重建画像 ──────────────── */
  const startAnalysis = async () => {
    if (analyzing || memories.length === 0) return;
    setAnalyzing(true);
    setAnalysisNote(null);
    try {
      const result = await invoke<{ merged_into: number; consumed: number; profile: AdaptationProfile }>(
        'analyze_and_consolidate_memories',
      );
      setProfile(result.profile);
      setAnalysisNote(
        result.merged_into > 0
          ? `已将 ${result.consumed} 条相关经历固化为 ${result.merged_into} 条长期记忆。`
          : '已完成检查，暂时没有达到固化条件的重复经历。',
      );
      setPulseKey((k) => k + 1);
      await loadMemories();
    } catch (error) {
      toast.error(`画像更新失败：${typeof error === 'string' ? error : '未知错误'}`);
    } finally {
      setAnalyzing(false);
    }
  };

  /* ── 渲染：星级选择器 ──────────────────────────────── */
  const starPicker = (value: number, onChange: (s: number) => void) => (
    <div className="flex items-center gap-0.5">
      {[1, 2, 3, 4, 5].map((star) => (
        <button
          key={star}
          type="button"
          onClick={() => onChange(star)}
          className="p-0.5 transition-transform hover:scale-110"
        >
          <Star
            size={16}
            className={star <= value ? 'fill-amber-400 text-amber-400' : 'text-white/25'}
          />
        </button>
      ))}
    </div>
  );

  return (
    <LumiPageShell
      icon={<Brain size={20} />}
      iconBg="bg-violet-500/15 border-violet-400/20 text-violet-400"
      title="记忆化身"
      subtitle={loading ? '正在加载...' : `${memories.length} 条记忆 · 偏好与经历分层管理`}
    >
      {/* ══════════════════════════════════════════════════
          记忆核心可视化区（Orb + Radar + 统计）
          ══════════════════════════════════════════════════ */}
      <motion.div
        initial={{ opacity: 0, y: -12 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ duration: 0.6, ease: 'easeOut' }}
        className="glass-panel relative mb-5 overflow-hidden rounded-2xl p-5"
      >
        {/* 背景装饰光晕 */}
        <div className="pointer-events-none absolute -right-20 -top-20 h-64 w-64 rounded-full bg-violet-500/10 blur-3xl" />
        <div className="pointer-events-none absolute -bottom-20 -left-20 h-64 w-64 rounded-full bg-celestial-saturn/5 blur-3xl" />

        <div className="relative z-10 flex flex-col items-center gap-6 md:flex-row md:items-center md:justify-between">
          {/* 左：记忆核心 Orb */}
          <div className="flex flex-col items-center gap-2">
            <div className="h-36 w-36 md:h-44 md:w-44">
              <MemoryCoreOrb
                activeType={selectedType}
                pulseKey={pulseKey}
                particleCount={380}
              />
            </div>
            <div className="flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-wider text-white/35">
              <Activity size={10} className="animate-pulse" />
              记忆核心
              {selectedType && (
                <span className="text-white/50">· {typeLabel(selectedType)}</span>
              )}
            </div>
          </div>

          {/* 中：关键统计 */}
          <div className="grid flex-1 grid-cols-2 gap-3 md:grid-cols-2 md:gap-4">
            {[
              { label: '记忆总数', value: stats.total, cls: 'text-celestial-saturn', icon: <Brain size={14} /> },
              { label: '今日新增', value: stats.today, cls: 'text-emerald-400', icon: <Sparkles size={14} /> },
              { label: '高重要性', value: stats.high, cls: 'text-amber-400', icon: <Star size={14} /> },
              { label: '平均权重', value: `${stats.avgImp}`, cls: 'text-violet-400', icon: <BarChart3 size={14} /> },
            ].map((s) => (
              <motion.div
                key={s.label}
                whileHover={{ scale: 1.03 }}
                className="rounded-xl border border-white/[0.06] bg-white/[0.02] p-3"
              >
                <div className="flex items-center gap-1.5 text-white/35">
                  {s.icon}
                  <span className="text-[9px] font-bold uppercase tracking-wider">{s.label}</span>
                </div>
                <p className={cn('mt-1 text-2xl font-black tabular-nums', s.cls)}>{s.value}</p>
              </motion.div>
            ))}
          </div>

          {/* 右：雷达图 */}
          <div className="flex flex-col items-center gap-1">
            <MemoryRadar values={radarValues} size={180} delay={300} />
            <p className="text-[9px] font-bold uppercase tracking-wider text-white/35">记忆证据构成</p>
          </div>
        </div>
      </motion.div>

      {/* ══════════════════════════════════════════════════
          添加记忆表单
          ══════════════════════════════════════════════════ */}
      <div className="glass-panel mb-5 rounded-2xl p-4">
        <textarea
          value={newContent}
          onChange={(e) => setNewContent(e.target.value)}
          placeholder="记录一条新记忆..."
          rows={3}
          className="w-full resize-none rounded-xl border border-white/10 bg-black/25 px-3 py-2 text-sm text-white outline-none placeholder:text-white/25 focus:border-celestial-saturn/40"
        />
        <div className="mt-3 flex flex-wrap items-center gap-3">
          <div className="flex flex-wrap items-center gap-1.5">
            {TYPE_OPTIONS.map((t) => (
              <button
                key={t}
                type="button"
                onClick={() => setNewType(t)}
                className={cn(
                  'rounded-full border px-2.5 py-1 text-[10px] font-bold transition-colors',
                  newType === t ? typeChipCls(t) : 'border-white/10 bg-white/[0.03] text-white/40 hover:text-white/70'
                )}
              >
                {typeLabel(t)}
              </button>
            ))}
          </div>
          {starPicker(newStars, setNewStars)}
          <button
            onClick={addMemory}
            disabled={submitting}
            className="ml-auto flex items-center gap-1.5 rounded-xl border border-celestial-saturn/30 bg-celestial-saturn/15 px-4 py-2 text-xs font-black text-celestial-saturn transition-all hover:bg-celestial-saturn/25 hover:shadow-[0_0_20px_rgba(255,204,0,0.15)] disabled:opacity-40"
          >
            <Plus size={14} />
            {submitting ? '保存中...' : '保存记忆'}
          </button>
        </div>
      </div>

      {/* ══════════════════════════════════════════════════
          搜索 + 筛选 + 分析按钮
          ══════════════════════════════════════════════════ */}
      <div className="mb-5 space-y-3">
        <div className="flex flex-wrap items-center gap-3">
          <div className="relative min-w-[200px] flex-1">
            <Search size={15} className="absolute left-3 top-1/2 -translate-y-1/2 text-white/35" />
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="搜索记忆内容..."
              className="w-full rounded-xl border border-white/10 bg-black/25 py-2.5 pl-9 pr-3 text-sm text-white outline-none placeholder:text-white/25 focus:border-celestial-saturn/40"
            />
          </div>
          <div className="flex gap-2">
            {(Object.keys(FILTER_META) as ImportanceFilter[]).map((key) => (
              <button
                key={key}
                onClick={() => setImportanceFilter(key)}
                className={cn(
                  'rounded-full border px-3 py-1.5 text-xs font-bold transition-colors',
                  importanceFilter === key
                    ? 'border-celestial-saturn/40 bg-celestial-saturn/15 text-celestial-saturn'
                    : 'border-white/10 bg-white/[0.04] text-white/50 hover:bg-white/[0.08] hover:text-white/80'
                )}
              >
                {FILTER_META[key].label}
              </button>
            ))}
          </div>
          {/* 分析面板开关 */}
          <button
            onClick={() => setAnalysisOpen((v) => !v)}
            className={cn(
              'flex items-center gap-1.5 rounded-full border px-3 py-1.5 text-xs font-bold transition-all',
              analysisOpen
                ? 'border-violet-400/40 bg-violet-400/15 text-violet-300'
                : 'border-white/10 bg-white/[0.04] text-white/50 hover:bg-white/[0.08] hover:text-white/80'
            )}
          >
            <ScanLine size={13} />
            用户画像
            <ChevronDown size={12} className={cn('transition-transform', analysisOpen && 'rotate-180')} />
          </button>
        </div>
        {/* 类型筛选 chips */}
        <div className="flex flex-wrap items-center gap-1.5">
          <button
            onClick={() => setTypeFilter('all')}
            className={cn(
              'rounded-full border px-2.5 py-1 text-[10px] font-bold transition-colors',
              typeFilter === 'all'
                ? 'border-white/30 bg-white/15 text-white'
                : 'border-white/10 bg-white/[0.03] text-white/40 hover:text-white/70'
            )}
          >
            全部类型
          </button>
          {TYPE_OPTIONS.map((t) => (
            <button
              key={t}
              onClick={() => setTypeFilter(typeFilter === t ? 'all' : t)}
              className={cn(
                'rounded-full border px-2.5 py-1 text-[10px] font-bold transition-colors',
                typeFilter === t ? typeChipCls(t) : 'border-white/10 bg-white/[0.03] text-white/40 hover:text-white/70'
              )}
            >
              {typeLabel(t)}
            </button>
          ))}
        </div>
      </div>

      {/* ══════════════════════════════════════════════════
          用户适应画像（可折叠）
          ══════════════════════════════════════════════════ */}
      <AnimatePresence initial={false}>
        {analysisOpen && (
          <motion.div
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: 'auto', opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: 0.35, ease: 'easeInOut' }}
            className="mb-5 overflow-hidden"
          >
            <div className="glass-panel rounded-2xl p-5">
              <div className="mb-4 flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-violet-500/15 text-violet-400">
                    <ScanLine size={15} />
                  </div>
                  <div>
                    <h3 className="text-xs font-black text-white/90">用户适应画像</h3>
                    <p className="text-[10px] text-white/35">只整理有来源的偏好、习惯、事实与经历，不推测人格</p>
                  </div>
                </div>
                <button
                  onClick={() => void startAnalysis()}
                  disabled={analyzing || memories.length === 0}
                  className="rounded-lg border border-violet-400/30 bg-violet-400/10 px-3 py-1.5 text-[10px] font-black text-violet-300 transition-colors hover:bg-violet-400/20 disabled:opacity-30"
                >
                  {analyzing ? '正在固化…' : '更新画像并固化'}
                </button>
              </div>

              {analysisNote && (
                <p className="mb-4 rounded-lg border border-emerald-400/20 bg-emerald-400/[0.07] p-3 text-[10px] text-emerald-300">
                  {analysisNote}
                </p>
              )}

              {profile ? (
                <div className="space-y-4">
                  <div className="grid grid-cols-2 gap-2 md:grid-cols-4">
                    {[
                      ['画像覆盖度', `${profile.coverage_score}%`],
                      ['稳定记忆', profile.durable_memories],
                      ['情景记忆', profile.episodic_memories],
                      ['已固化原始项', profile.consolidated_memories],
                    ].map(([label, value]) => (
                      <div key={String(label)} className="rounded-lg border border-white/[0.06] bg-white/[0.02] p-3">
                        <p className="text-[9px] text-white/35">{label}</p>
                        <p className="mt-1 text-lg font-black text-white/80">{value}</p>
                      </div>
                    ))}
                  </div>

                  <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
                    {[
                      { title: '明确偏好', items: profile.preferences, empty: '尚未记录明确偏好' },
                      { title: '应用与工作习惯', items: profile.habits, empty: '尚未记录稳定习惯' },
                      { title: '长期事实', items: profile.facts, empty: '尚未形成长期事实' },
                      { title: '近期经历', items: profile.recent_experiences, empty: '暂无近期经历证据' },
                    ].map((section) => (
                      <div key={section.title} className="rounded-xl border border-white/[0.06] bg-black/10 p-3">
                        <p className="mb-2 text-[10px] font-bold uppercase tracking-wider text-white/45">{section.title}</p>
                        {section.items.length === 0 ? (
                          <p className="text-[10px] text-white/25">{section.empty}</p>
                        ) : (
                          <div className="space-y-2">
                            {section.items.map((item) => (
                              <button
                                key={item.id}
                                onClick={() => setSelectedId(item.id)}
                                className="block w-full rounded-lg border border-white/[0.05] bg-white/[0.025] p-2 text-left transition-colors hover:border-violet-400/25"
                              >
                                <p className="line-clamp-2 text-[11px] leading-relaxed text-white/65">{item.content}</p>
                                <p className="mt-1 text-[9px] text-white/25">
                                  证据 {item.confidence}% · {['manual', 'user_explicit'].includes(item.source) ? '用户确认' : item.source}
                                </p>
                              </button>
                            ))}
                          </div>
                        )}
                      </div>
                    ))}
                  </div>
                </div>
              ) : memories.length === 0 ? (
                <div className="flex flex-col items-center justify-center gap-2 py-6 text-center">
                  <Inbox size={24} className="text-white/20" />
                  <p className="text-xs text-white/35">暂无记忆数据，先记录几条记忆再分析</p>
                </div>
              ) : (
                <LoadingSpinner label="正在生成可追溯画像..." />
              )}
            </div>
          </motion.div>
        )}
      </AnimatePresence>

      {/* ══════════════════════════════════════════════════
          时间线（按日期分组，带动画 + 选中交互）
          ══════════════════════════════════════════════════ */}
      {loading ? (
        <LoadingSpinner label="正在加载记忆..." />
      ) : filtered.length === 0 ? (
        <EmptyState
          icon={<Inbox size={28} />}
          title={memories.length === 0 ? '还没有任何记忆' : '该筛选下暂无记忆'}
          description={
            memories.length === 0
              ? '在上方输入框记录第一条记忆吧'
              : '试试切换筛选条件或搜索关键词'
          }
        />
      ) : (
        <div className="space-y-6">
          {grouped.map((group, groupIdx) => (
            <div key={group.key}>
              <p className="mb-2 pl-1 text-[11px] font-bold uppercase tracking-wider text-white/35">
                {group.key}
                <span className="ml-2 font-normal text-white/25">{group.items.length}</span>
              </p>
              <div className="relative space-y-3 pl-5">
                <div className="absolute bottom-2 left-[7px] top-2 w-px bg-white/[0.08]" />
                <AnimatePresence initial={false}>
                  {group.items.map((m, itemIdx) => {
                    const imp = importanceToStars(m.importance);
                    const dotCls = TYPE_META[m.memory_type]?.dot ?? 'border-white/30 bg-white/10';
                    const isSelected = selectedId === m.id;
                    return (
                      <motion.div
                        key={m.id}
                        layout
                        initial={{ opacity: 0, x: -16 }}
                        animate={{ opacity: 1, x: 0 }}
                        exit={{ opacity: 0, x: 16 }}
                        transition={{ duration: 0.35, delay: (groupIdx * 0.05 + itemIdx * 0.04) }}
                        className="relative"
                      >
                        {/* 时间线节点 */}
                        <div
                          className={cn(
                            'absolute -left-5 top-4 h-3.5 w-3.5 rounded-full border-2 transition-all duration-300',
                            dotCls,
                            isSelected && 'scale-125 shadow-[0_0_10px_currentColor]'
                          )}
                        />
                        <motion.div
                          onClick={() => selectMemory(m)}
                          whileHover={{ scale: 1.008, x: 2 }}
                          whileTap={{ scale: 0.995 }}
                          className={cn(
                            'glass-panel group cursor-pointer rounded-2xl p-4 transition-all duration-300',
                            isSelected
                              ? 'border-celestial-saturn/40 shadow-[0_0_24px_rgba(255,204,0,0.1)]'
                              : 'hover:border-white/[0.14] hover:shadow-[0_0_16px_rgba(255,255,255,0.04)]'
                          )}
                        >
                          {/* 选中指示条 */}
                          {isSelected && (
                            <motion.div
                              layoutId="memory-select-bar"
                              className="absolute left-0 top-3 bottom-3 w-0.5 rounded-full bg-celestial-saturn"
                            />
                          )}
                          <p className="text-sm leading-relaxed text-white/80">{m.content}</p>
                          <div className="mt-3 flex flex-wrap items-center gap-2">
                            <span
                              className={cn(
                                'rounded-md border px-1.5 py-0.5 text-[10px] font-bold',
                                typeChipCls(m.memory_type)
                              )}
                            >
                              {typeLabel(m.memory_type)}
                            </span>
                            <span className="flex items-center gap-0.5">
                              {[1, 2, 3, 4, 5].map((star) => (
                                <Star
                                  key={star}
                                  size={10}
                                  className={star <= imp ? 'fill-amber-400 text-amber-400' : 'text-white/15'}
                                />
                              ))}
                            </span>
                            <span className="flex items-center gap-1 text-[10px] text-white/35">
                              <Clock size={10} />
                              {formatTime(m.created_at)}
                            </span>
                            {m.access_count > 0 && (
                              <span className="text-[10px] text-white/25">访问 {m.access_count}</span>
                            )}
                            <div className="ml-auto flex items-center gap-1 opacity-0 transition-all group-hover:opacity-100">
                              <button
                                onClick={(e) => { e.stopPropagation(); openEdit(m); }}
                                title="编辑记忆"
                                className="flex h-7 w-7 items-center justify-center rounded-lg text-white/25 transition-colors hover:bg-celestial-saturn/10 hover:text-celestial-saturn"
                              >
                                <Pencil size={13} />
                              </button>
                              <button
                                onClick={(e) => { e.stopPropagation(); handleDelete(m); }}
                                title={confirmingId === m.id ? '再次点击确认删除' : '删除记忆'}
                                className={cn(
                                  'flex h-7 items-center justify-center rounded-lg transition-all',
                                  confirmingId === m.id
                                    ? 'w-auto bg-red-500/20 px-2 text-[10px] font-black text-red-300'
                                    : 'w-7 text-white/25 hover:bg-red-500/10 hover:text-red-400'
                                )}
                              >
                                {confirmingId === m.id ? '确认删除？' : <Trash2 size={13} />}
                              </button>
                            </div>
                          </div>
                        </motion.div>
                      </motion.div>
                    );
                  })}
                </AnimatePresence>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* ══════════════════════════════════════════════════
          编辑弹窗
          ══════════════════════════════════════════════════ */}
      <AnimatePresence>
        {editing && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-sm"
            onClick={() => !editSaving && setEditing(null)}
          >
            <motion.div
              initial={{ scale: 0.95, y: 10 }}
              animate={{ scale: 1, y: 0 }}
              exit={{ scale: 0.95, y: 10 }}
              onClick={(e) => e.stopPropagation()}
              className="glass-panel w-full max-w-lg rounded-2xl p-5"
            >
              <div className="mb-4 flex items-center justify-between">
                <h3 className="text-sm font-black text-white">编辑记忆</h3>
                <button
                  onClick={() => setEditing(null)}
                  disabled={editSaving}
                  className="text-white/30 transition-colors hover:text-white/70"
                >
                  <X size={16} />
                </button>
              </div>
              <textarea
                value={editContent}
                onChange={(e) => setEditContent(e.target.value)}
                rows={4}
                className="w-full resize-none rounded-xl border border-white/10 bg-black/25 px-3 py-2 text-sm text-white outline-none placeholder:text-white/25 focus:border-celestial-saturn/40"
              />
              <div className="mt-3 flex flex-wrap items-center gap-3">
                <div className="flex flex-wrap items-center gap-1.5">
                  {TYPE_OPTIONS.map((t) => (
                    <button
                      key={t}
                      type="button"
                      onClick={() => setEditType(t)}
                      className={cn(
                        'rounded-full border px-2.5 py-1 text-[10px] font-bold transition-colors',
                        editType === t ? typeChipCls(t) : 'border-white/10 bg-white/[0.03] text-white/40 hover:text-white/70'
                      )}
                    >
                      {typeLabel(t)}
                    </button>
                  ))}
                </div>
                {starPicker(editStars, setEditStars)}
              </div>
              <p className="mt-3 text-[10px] text-white/30">
                说明：后端暂不支持单独更新，保存将删除原记录并以新内容重新写入（创建时间会更新）。
              </p>
              <div className="mt-4 flex justify-end gap-2">
                <button
                  onClick={() => setEditing(null)}
                  disabled={editSaving}
                  className="rounded-xl border border-white/10 bg-white/[0.04] px-4 py-2 text-xs font-bold text-white/60 transition-colors hover:bg-white/[0.08] disabled:opacity-40"
                >
                  取消
                </button>
                <button
                  onClick={saveEdit}
                  disabled={editSaving}
                  className="rounded-xl border border-celestial-saturn/30 bg-celestial-saturn/15 px-4 py-2 text-xs font-black text-celestial-saturn transition-colors hover:bg-celestial-saturn/25 disabled:opacity-40"
                >
                  {editSaving ? '保存中...' : '保存修改'}
                </button>
              </div>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
    </LumiPageShell>
  );
}
