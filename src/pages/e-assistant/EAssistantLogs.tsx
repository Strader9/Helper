// EAssistantCore 风格日志页 —— 审计日志 + 工具收据双视图，级别筛选 / 搜索 / 自动刷新
// 真实后端字段（tool_name/permission_result/success/duration_ms 等）与任务描述的
// 理想字段（level/action/details）做了防御性归一化，两套结构都能渲染。
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { motion } from 'motion/react';
import {
  AlertTriangle,
  CheckCircle2,
  FileText,
  Inbox,
  Loader2,
  RefreshCw,
  Search,
  ScrollText,
  Wrench,
  XCircle,
} from 'lucide-react';
import {
  Badge,
  EmptyState,
  GlassCard,
  LoadingSpinner,
  SectionTitle,
  StatCard,
  ToggleSwitch,
} from '../../components/e-assistant/EAssistantShared';
import { EAssistantPageShell } from '../../components/e-assistant/EAssistantPageShell';
import { cn } from '../../lib/cn';

const PAGE_LIMIT = 200;

type Level = 'INFO' | 'WARN' | 'ERROR' | 'DEBUG';
type Mode = 'audit' | 'receipts';

/** 审计日志行：任务描述字段与真实后端字段并存，渲染时取第一个非空。 */
interface AuditLogRow {
  id: number | string;
  timestamp: string;
  // 任务描述的理想字段
  level?: string;
  action?: string;
  details?: string;
  status?: string;
  // 真实后端字段
  tool_name?: string;
  tool_risk_level?: string;
  permission_result?: string;
  success?: boolean;
  error_code?: string | null;
  error_message?: string | null;
  duration_ms?: number | null;
}

/** 工具收据行：同上做防御性合并。 */
interface ReceiptRow {
  id?: string;
  receipt_id?: string;
  tool_name: string;
  // 任务描述的理想字段
  args?: string;
  result?: string;
  status?: string;
  timestamp?: string;
  // 真实后端字段
  created_at?: string;
  side_effect?: string;
  verification_status?: string;
  success?: boolean;
  duration_ms?: number;
  request_digest?: string;
  risk_level?: string;
  error_code?: string | null;
  error_message?: string | null;
}

/** 从一行日志推导级别：优先用显式 level，否则按 success / 风险等级推导。 */
function deriveLevel(row: AuditLogRow): Level {
  if (row.level) {
    const l = row.level.toUpperCase();
    if (l === 'ERROR' || l === 'WARN' || l === 'INFO' || l === 'DEBUG') return l as Level;
  }
  if (row.success === false) return 'ERROR';
  const risk = (row.tool_risk_level ?? '').toUpperCase();
  if (risk === 'HIGH' || risk === 'MEDIUM') return 'WARN';
  return 'INFO';
}

function levelBadgeVariant(level: Level): 'info' | 'warning' | 'danger' | 'neutral' {
  switch (level) {
    case 'ERROR':
      return 'danger';
    case 'WARN':
      return 'warning';
    case 'INFO':
      return 'info';
    default:
      return 'neutral';
  }
}

function formatTimestamp(ts?: string): string {
  if (!ts) return '—';
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return ts;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getMonth() + 1}/${d.getDate()} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function isToday(ts?: string): boolean {
  if (!ts) return false;
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return false;
  const now = new Date();
  return (
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate()
  );
}

function truncate(text: string | undefined, max = 120): string {
  if (!text) return '—';
  return text.length > max ? `${text.slice(0, max)}…` : text;
}

const LEVEL_FILTERS: Array<{ id: 'ALL' | Level; label: string }> = [
  { id: 'ALL', label: '全部' },
  { id: 'INFO', label: 'INFO' },
  { id: 'WARN', label: 'WARN' },
  { id: 'ERROR', label: 'ERROR' },
];

export default function EAssistantLogs() {
  const [mode, setMode] = useState<Mode>('audit');
  const [levelFilter, setLevelFilter] = useState<'ALL' | Level>('ALL');
  const [search, setSearch] = useState('');
  const [autoRefresh, setAutoRefresh] = useState(false);

  const [auditLogs, setAuditLogs] = useState<AuditLogRow[]>([]);
  const [receipts, setReceipts] = useState<ReceiptRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const load = useCallback(async (silent = false) => {
    if (!silent) setRefreshing(true);
    try {
      const [logs, receiptsData] = await Promise.all([
        invoke<AuditLogRow[]>('get_audit_logs', { limit: PAGE_LIMIT }).catch(() => [] as AuditLogRow[]),
        invoke<ReceiptRow[]>('get_tool_receipts', { limit: PAGE_LIMIT }).catch(() => [] as ReceiptRow[]),
      ]);
      setAuditLogs(logs);
      setReceipts(receiptsData);
    } finally {
      if (!silent) setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    void load(true).finally(() => {
      if (!cancelled) setLoading(false);
    });
    return () => {
      cancelled = true;
    };
  }, [load]);

  // 自动刷新：5 秒轮询一次
  useEffect(() => {
    if (autoRefresh) {
      timerRef.current = setInterval(() => void load(true), 5000);
    }
    return () => {
      if (timerRef.current) {
        clearInterval(timerRef.current);
        timerRef.current = null;
      }
    };
  }, [autoRefresh, load]);

  /* ── 统计：今日日志总数 / 错误 / 警告 ── */
  const stats = useMemo(() => {
    const todayLogs = auditLogs.filter((r) => isToday(r.timestamp));
    const pool = todayLogs.length > 0 ? todayLogs : auditLogs;
    const errors = pool.filter((r) => deriveLevel(r) === 'ERROR').length;
    const warns = pool.filter((r) => deriveLevel(r) === 'WARN').length;
    return { total: pool.length, errors, warns, today: todayLogs.length > 0 };
  }, [auditLogs]);

  /* ── 筛选后的审计日志 ── */
  const filteredLogs = useMemo(() => {
    const q = search.trim().toLowerCase();
    return auditLogs.filter((row) => {
      if (levelFilter !== 'ALL' && deriveLevel(row) !== levelFilter) return false;
      if (!q) return true;
      const haystack = [
        row.action,
        row.details,
        row.status,
        row.tool_name,
        row.permission_result,
        row.error_message,
      ]
        .filter(Boolean)
        .join(' ')
        .toLowerCase();
      return haystack.includes(q);
    });
  }, [auditLogs, levelFilter, search]);

  /* ── 筛选后的收据 ── */
  const filteredReceipts = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return receipts;
    return receipts.filter((row) =>
      [row.tool_name, row.args, row.result, row.status, row.side_effect, row.verification_status]
        .filter(Boolean)
        .join(' ')
        .toLowerCase()
        .includes(q),
    );
  }, [receipts, search]);

  return (
    <EAssistantPageShell
      icon={<ScrollText size={18} />}
      title="日志"
      subtitle="审计记录与工具执行收据"
      actions={
        <>
          <div className="flex items-center gap-2 rounded-xl border border-white/10 bg-white/5 px-2.5 py-1.5">
            <span className="text-[10px] font-bold uppercase tracking-wider text-white/40">实时</span>
            <ToggleSwitch enabled={autoRefresh} onChange={() => setAutoRefresh((v) => !v)} />
          </div>
          <button
            onClick={() => void load()}
            className="flex h-9 items-center gap-2 rounded-xl border border-white/10 bg-white/5 px-3 text-xs font-bold text-white/60 transition-colors hover:bg-white/10 hover:text-white"
          >
            {refreshing ? <Loader2 size={14} className="animate-spin" /> : <RefreshCw size={14} />}
            刷新
          </button>
        </>
      }
    >
      {/* 统计卡片 */}
      <div className="mb-5 grid gap-3 md:grid-cols-3">
        <StatCard
          label={stats.today ? '今日日志总数' : '日志总数（近 200 条）'}
          value={stats.total}
          icon={<FileText size={18} />}
          color="text-celestial-saturn"
        />
        <StatCard
          label="错误数"
          value={stats.errors}
          icon={<XCircle size={18} />}
          color={stats.errors > 0 ? 'text-red-400' : 'text-emerald-400'}
        />
        <StatCard
          label="警告数"
          value={stats.warns}
          icon={<AlertTriangle size={18} />}
          color={stats.warns > 0 ? 'text-amber-400' : 'text-celestial-saturn'}
        />
      </div>

      {/* 工具栏：视图切换 + 级别筛选 + 搜索 */}
      <GlassCard hoverEffect={false} className="p-4">
        <div className="flex flex-wrap items-center gap-3">
          <div className="flex gap-1 rounded-xl border border-white/10 bg-black/30 p-1">
            {(
              [
                { id: 'audit', label: '审计日志', icon: <FileText size={13} /> },
                { id: 'receipts', label: '工具收据', icon: <Wrench size={13} /> },
              ] as Array<{ id: Mode; label: string; icon: ReactNode }>
            ).map((m) => (
              <button
                key={m.id}
                onClick={() => setMode(m.id)}
                className={cn(
                  'flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-bold transition-all',
                  mode === m.id
                    ? 'bg-celestial-saturn/15 text-celestial-saturn'
                    : 'text-white/40 hover:text-white/70',
                )}
              >
                {m.icon}
                {m.label}
              </button>
            ))}
          </div>

          {mode === 'audit' && (
            <div className="flex gap-1">
              {LEVEL_FILTERS.map((f) => (
                <button
                  key={f.id}
                  onClick={() => setLevelFilter(f.id)}
                  className={cn(
                    'rounded-lg border px-2.5 py-1 text-[11px] font-black transition-all',
                    levelFilter === f.id
                      ? 'border-celestial-saturn/40 bg-celestial-saturn/10 text-celestial-saturn'
                      : 'border-white/10 text-white/40 hover:text-white/70',
                  )}
                >
                  {f.label}
                </button>
              ))}
            </div>
          )}

          <div className="relative ml-auto">
            <Search size={14} className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-white/30" />
            <input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="搜索日志关键词…"
              className="w-56 rounded-xl border border-white/10 bg-black/30 py-2 pl-9 pr-3 text-xs text-white/80 placeholder:text-white/25 outline-none transition-colors focus:border-celestial-saturn/50"
            />
          </div>
        </div>
      </GlassCard>

      {/* 内容区 */}
      {loading ? (
        <LoadingSpinner label="正在加载日志…" />
      ) : mode === 'audit' ? (
        <>
          <SectionTitle label={`审计日志 (${filteredLogs.length})`} color="bg-celestial-saturn" />
          {filteredLogs.length === 0 ? (
            <GlassCard hoverEffect={false}>
              <EmptyState
                icon={<Inbox size={28} />}
                title="暂无日志"
                description={search ? '没有匹配关键词的日志，换个词试试' : '系统运行后产生的审计记录会显示在这里'}
              />
            </GlassCard>
          ) : (
            <div className="space-y-2">
              {filteredLogs.map((row, idx) => {
                const level = deriveLevel(row);
                const action = row.action ?? row.tool_name ?? row.permission_result ?? '未知操作';
                const details =
                  row.details ??
                  row.error_message ??
                  (row.success === false && row.error_code ? `错误码：${row.error_code}` : row.permission_result) ??
                  '';
                return (
                  <motion.div
                    key={row.id}
                    initial={{ opacity: 0, y: 6 }}
                    animate={{ opacity: 1, y: 0 }}
                    transition={{ delay: Math.min(idx * 0.02, 0.3) }}
                    className="glass-panel flex items-start gap-3 rounded-xl p-3.5 transition-colors hover:border-celestial-saturn/20"
                  >
                    <span className="mt-0.5 shrink-0 font-mono text-[11px] text-white/35">
                      {formatTimestamp(row.timestamp)}
                    </span>
                    <Badge variant={levelBadgeVariant(level)} className="mt-0.5 shrink-0">
                      {level}
                    </Badge>
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-xs font-bold text-white/85">{truncate(action, 80)}</p>
                      {details && <p className="mt-0.5 text-[11px] leading-relaxed text-white/40">{truncate(details, 160)}</p>}
                    </div>
                    {row.success === true && <Badge variant="safe">成功</Badge>}
                    {row.success === false && <Badge variant="danger">失败</Badge>}
                    {row.success === undefined && row.status && <Badge variant="neutral">{row.status}</Badge>}
                  </motion.div>
                );
              })}
            </div>
          )}
        </>
      ) : (
        <>
          <SectionTitle label={`工具收据 (${filteredReceipts.length})`} color="bg-cyan-400" />
          {filteredReceipts.length === 0 ? (
            <GlassCard hoverEffect={false}>
              <EmptyState
                icon={<Inbox size={28} />}
                title="暂无工具执行记录"
                description={search ? '没有匹配关键词的收据' : '工具被调用并产生副作用后，会在这里留下执行收据'}
              />
            </GlassCard>
          ) : (
            <div className="overflow-hidden rounded-2xl">
              {/* 表头 */}
              <div className="grid grid-cols-[140px_1.2fr_1.5fr_90px_70px] gap-3 border-b border-white/10 px-4 py-2.5 text-[10px] font-black uppercase tracking-wider text-white/35">
                <span>时间</span>
                <span>工具</span>
                <span>结果 / 校验</span>
                <span>耗时</span>
                <span>状态</span>
              </div>
              {filteredReceipts.map((row, idx) => (
                <motion.div
                  key={row.id ?? row.receipt_id ?? idx}
                  initial={{ opacity: 0, y: 6 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={{ delay: Math.min(idx * 0.02, 0.3) }}
                  className="grid grid-cols-[140px_1.2fr_1.5fr_90px_70px] items-center gap-3 border-b border-white/5 px-4 py-3 transition-colors hover:bg-white/[0.03]"
                >
                  <span className="font-mono text-[11px] text-white/35">
                    {formatTimestamp(row.timestamp ?? row.created_at)}
                  </span>
                  <div className="min-w-0">
                    <p className="truncate font-mono text-xs font-bold text-celestial-saturn">{row.tool_name}</p>
                    {row.args && <p className="truncate text-[10px] text-white/30">{truncate(row.args, 60)}</p>}
                  </div>
                  <p className="truncate text-[11px] text-white/50">
                    {row.result ??
                      ([row.verification_status, row.side_effect, row.error_message].filter(Boolean).join(' · ') ||
                        '—')}
                  </p>
                  <span className="font-mono text-[11px] text-white/45">
                    {row.duration_ms != null ? `${row.duration_ms}ms` : '—'}
                  </span>
                  <span>
                    {row.success === true ? (
                      <Badge variant="safe">
                        <CheckCircle2 size={10} className="mr-0.5" />
                        成功
                      </Badge>
                    ) : row.success === false ? (
                      <Badge variant="danger">失败</Badge>
                    ) : (
                      <Badge variant="neutral">{row.status ?? '—'}</Badge>
                    )}
                  </span>
                </motion.div>
              ))}
            </div>
          )}
        </>
      )}
    </EAssistantPageShell>
  );
}
