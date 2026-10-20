// LumiCore 风格 —— 邮件中心（基于工具执行记录）
import { useCallback, useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { motion } from 'motion/react';
import {
  Mail,
  Search,
  RefreshCw,
  LoaderCircle,
  Send,
  Inbox,
  CircleCheck,
  CircleX,
  Clock3,
} from 'lucide-react';
import {
  GlassCard,
  Badge,
  SectionTitle,
  LoadingSpinner,
  EmptyState,
  StatCard,
} from '../../components/lumi/LumiShared';
import { LumiPageShell } from '../../components/lumi/LumiPageShell';
import { cn } from '../../lib/cn';

/* ── 类型 ── */
interface ToolReceipt {
  id: string;
  tool?: string;
  tool_name?: string;
  name?: string;
  status?: string;
  created_at?: string;
  time?: string;
  summary?: string;
  result?: string;
}

type MailStatus = 'all' | 'sent' | 'failed' | 'pending';

/* ── 工具 ── */
function fmtTime(v?: string | number | null): string {
  if (v === undefined || v === null || v === '') return '—';
  try {
    const d = typeof v === 'number' ? new Date(v * 1000) : new Date(v);
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

function receiptTool(r: ToolReceipt): string {
  return r.tool || r.tool_name || r.name || 'mail_unknown';
}

function isSent(s?: string) {
  return ['success', 'sent', 'completed', 'done'].includes((s || '').toLowerCase());
}
function isFailed(s?: string) {
  return ['failed', 'error'].includes((s || '').toLowerCase());
}
function isPending(s?: string) {
  return !s || ['pending', 'running', 'processing', 'queued'].includes(s.toLowerCase());
}

/* ── 组件 ── */
export default function LumiMail() {
  const [receipts, setReceipts] = useState<ToolReceipt[]>([]);
  const [loading, setLoading] = useState(true);
  const [status, setStatus] = useState<MailStatus>('all');
  const [query, setQuery] = useState('');

  const load = useCallback(async () => {
    setLoading(true);
    const rows = await invoke<ToolReceipt[]>('get_tool_receipts', { limit: 100 }).catch(() => [] as ToolReceipt[]);
    // 过滤出 mail 相关的工具调用记录
    const mailRows = (rows || []).filter((r) => receiptTool(r).toLowerCase().includes('mail'));
    setReceipts(mailRows);
    setLoading(false);
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const stats = useMemo(
    () => ({
      total: receipts.length,
      sent: receipts.filter((r) => isSent(r.status)).length,
      failed: receipts.filter((r) => isFailed(r.status)).length,
      pending: receipts.filter((r) => isPending(r.status)).length,
    }),
    [receipts]
  );

  const filtered = useMemo(() => {
    let list = receipts;
    if (status === 'sent') list = list.filter((r) => isSent(r.status));
    if (status === 'failed') list = list.filter((r) => isFailed(r.status));
    if (status === 'pending') list = list.filter((r) => isPending(r.status));
    const q = query.trim().toLowerCase();
    if (q) {
      list = list.filter(
        (r) =>
          receiptTool(r).toLowerCase().includes(q) ||
          (r.summary || '').toLowerCase().includes(q) ||
          (r.result || '').toLowerCase().includes(q)
      );
    }
    return list;
  }, [receipts, status, query]);

  const tabs: { key: MailStatus; label: string }[] = [
    { key: 'all', label: '全部' },
    { key: 'sent', label: '已发送' },
    { key: 'failed', label: '失败' },
    { key: 'pending', label: '待处理' },
  ];

  return (
    <LumiPageShell
      icon={<Mail size={18} />}
      title="邮件中心"
      subtitle="邮件发送记录与投递状态"
      actions={
        <button onClick={load} className="lumi-icon-button" title="刷新">
          {loading ? <LoaderCircle size={15} className="animate-spin" /> : <RefreshCw size={15} />}
        </button>
      }
    >
      {/* 统计 */}
      <div className="mb-5 grid grid-cols-2 gap-3 md:grid-cols-4">
        <StatCard label="邮件总数" value={stats.total} icon={<Mail size={16} />} />
        <StatCard label="已发送" value={stats.sent} icon={<Send size={16} />} color="text-emerald-400" />
        <StatCard label="待处理" value={stats.pending} icon={<Clock3 size={16} />} color="text-amber-400" />
        <StatCard label="失败" value={stats.failed} icon={<CircleX size={16} />} color="text-red-400" />
      </div>

      {/* 搜索 + 筛选 */}
      <div className="mb-4 flex flex-wrap items-center gap-2">
        <div className="relative">
          <Search size={14} className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-white/30" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索主题 / 摘要…"
            className="lumi-field h-9 w-56 pl-8 text-xs"
          />
        </div>
        <div className="flex flex-wrap gap-2">
          {tabs.map((t) => (
            <button
              key={t.key}
              onClick={() => setStatus(t.key)}
              className={cn(
                'inline-flex h-9 items-center rounded-xl border px-4 text-xs font-bold transition-colors',
                status === t.key
                  ? 'border-celestial-saturn/40 bg-celestial-saturn/15 text-celestial-saturn'
                  : 'border-white/10 bg-white/[0.04] text-white/50 hover:bg-white/10 hover:text-white/80'
              )}
            >
              {t.label}
            </button>
          ))}
        </div>
      </div>

      <SectionTitle label="投递记录" count={filtered.length} />

      {loading ? (
        <LoadingSpinner label="正在加载邮件记录…" />
      ) : filtered.length === 0 ? (
        <GlassCard hoverEffect={false}>
          <EmptyState
            icon={<Inbox size={28} />}
            title={receipts.length === 0 ? '暂无邮件活动' : '没有匹配的邮件'}
            description={
              receipts.length === 0
                ? 'AI 代发邮件、读取收件箱等操作会记录在这里。'
                : '尝试调整筛选条件或搜索关键词。'
            }
          />
        </GlassCard>
      ) : (
        <div className="flex flex-col gap-2.5">
          {filtered.map((r, i) => (
            <motion.div
              key={r.id}
              initial={{ opacity: 0, y: 8 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ delay: Math.min(i * 0.03, 0.3), duration: 0.25 }}
            >
              <GlassCard hoverEffect={false} className="p-4">
                <div className="flex flex-wrap items-center gap-3">
                  <div
                    className={cn(
                      'flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border',
                      isSent(r.status) && 'border-emerald-400/20 bg-emerald-400/10 text-emerald-400',
                      isFailed(r.status) && 'border-red-400/20 bg-red-400/10 text-red-400',
                      isPending(r.status) && 'border-amber-400/20 bg-amber-400/10 text-amber-400'
                    )}
                  >
                    {isSent(r.status) ? <CircleCheck size={17} /> : isFailed(r.status) ? <CircleX size={17} /> : <Mail size={17} />}
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-bold text-white/90">{receiptTool(r)}</p>
                    <p className="mt-0.5 truncate text-[11px] text-white/40">
                      {r.summary || r.result || '—'}
                    </p>
                  </div>
                  <span className="text-[11px] text-white/35">{fmtTime(r.created_at || r.time)}</span>
                  <Badge variant={isSent(r.status) ? 'safe' : isFailed(r.status) ? 'danger' : 'warning'}>
                    {isSent(r.status) ? '已发送' : isFailed(r.status) ? '失败' : '待处理'}
                  </Badge>
                </div>
              </GlassCard>
            </motion.div>
          ))}
        </div>
      )}
    </LumiPageShell>
  );
}
