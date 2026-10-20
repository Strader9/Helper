// LumiCore 风格 —— 媒体生成历史
import { useCallback, useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { motion } from 'motion/react';
import {
  Image as ImageIcon,
  Video,
  AudioLines,
  RefreshCw,
  LoaderCircle,
  LayoutGrid,
  Eye,
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
interface MediaGen {
  id: string;
  type: string;
  prompt: string;
  status: string;
  url?: string | null;
  created_at: string;
}

type FilterKey = 'all' | 'generating' | 'completed' | 'failed';

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

function isGenerating(s: string) {
  return ['generating', 'pending', 'processing', 'running', 'queued'].includes((s || '').toLowerCase());
}
function isDone(s: string) {
  return ['completed', 'done', 'success'].includes((s || '').toLowerCase());
}
function isFailed(s: string) {
  return ['failed', 'error'].includes((s || '').toLowerCase());
}

function typeMeta(type: string) {
  const v = (type || '').toLowerCase();
  if (v === 'video' || v === 'mov' || v === 'mp4') {
    return { label: '视频', icon: <Video size={18} />, color: 'text-purple-300 bg-purple-400/10 border-purple-400/20' };
  }
  if (v === 'audio' || v === 'music' || v === 'sound') {
    return { label: '音频', icon: <AudioLines size={18} />, color: 'text-emerald-300 bg-emerald-400/10 border-emerald-400/20' };
  }
  return { label: '图片', icon: <ImageIcon size={18} />, color: 'text-cyan-300 bg-cyan-400/10 border-cyan-400/20' };
}

/* ── 组件 ── */
export default function LumiMedia() {
  const [items, setItems] = useState<MediaGen[]>([]);
  const [loading, setLoading] = useState(true);
  const [filter, setFilter] = useState<FilterKey>('all');
  const [expanded, setExpanded] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    const rows = await invoke<MediaGen[]>('list_media_generations', { limit: 60 }).catch(() => [] as MediaGen[]);
    setItems(rows || []);
    setLoading(false);
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const stats = useMemo(
    () => ({
      total: items.length,
      done: items.filter((m) => isDone(m.status)).length,
      failed: items.filter((m) => isFailed(m.status)).length,
      running: items.filter((m) => isGenerating(m.status)).length,
    }),
    [items]
  );

  const filtered = useMemo(() => {
    if (filter === 'generating') return items.filter((m) => isGenerating(m.status));
    if (filter === 'completed') return items.filter((m) => isDone(m.status));
    if (filter === 'failed') return items.filter((m) => isFailed(m.status));
    return items;
  }, [items, filter]);

  const filters: { key: FilterKey; label: string }[] = [
    { key: 'all', label: '全部' },
    { key: 'generating', label: '生成中' },
    { key: 'completed', label: '已完成' },
    { key: 'failed', label: '失败' },
  ];

  return (
    <LumiPageShell
      icon={<LayoutGrid size={18} />}
      title="媒体工坊"
      subtitle="AI 生成图片 / 视频 / 音频历史"
      actions={
        <button onClick={load} className="lumi-icon-button" title="刷新">
          {loading ? <LoaderCircle size={15} className="animate-spin" /> : <RefreshCw size={15} />}
        </button>
      }
    >
      {/* 统计 */}
      <div className="mb-5 grid grid-cols-2 gap-3 md:grid-cols-4">
        <StatCard label="总生成" value={stats.total} icon={<LayoutGrid size={16} />} />
        <StatCard label="成功" value={stats.done} icon={<ImageIcon size={16} />} color="text-emerald-400" />
        <StatCard label="生成中" value={stats.running} icon={<LoaderCircle size={16} />} color="text-cyan-400" />
        <StatCard label="失败" value={stats.failed} icon={<Video size={16} />} color="text-red-400" />
      </div>

      {/* 筛选 */}
      <div className="mb-4 flex flex-wrap gap-2">
        {filters.map((f) => (
          <button
            key={f.key}
            onClick={() => setFilter(f.key)}
            className={cn(
              'inline-flex h-9 items-center rounded-xl border px-4 text-xs font-bold transition-colors',
              filter === f.key
                ? 'border-celestial-saturn/40 bg-celestial-saturn/15 text-celestial-saturn'
                : 'border-white/10 bg-white/[0.04] text-white/50 hover:bg-white/10 hover:text-white/80'
            )}
          >
            {f.label}
          </button>
        ))}
      </div>

      <SectionTitle label="生成历史" count={filtered.length} />

      {loading ? (
        <LoadingSpinner label="正在加载媒体记录…" />
      ) : filtered.length === 0 ? (
        <GlassCard hoverEffect={false}>
          <EmptyState
            icon={<ImageIcon size={28} />}
            title="还没有生成记录"
            description="通过对话让 AI 生成图片、视频或音频，作品会自动归档在这里。"
          />
        </GlassCard>
      ) : (
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
          {filtered.map((item, i) => {
            const meta = typeMeta(item.type);
            const open = expanded === item.id;
            return (
              <motion.div
                key={item.id}
                initial={{ opacity: 0, y: 10 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ delay: Math.min(i * 0.04, 0.4), duration: 0.25 }}
              >
                <GlassCard className="group h-full p-4">
                  <div className="mb-3 flex items-center justify-between">
                    <div className={cn('flex items-center gap-2 rounded-lg border px-2 py-1 text-[11px] font-bold', meta.color)}>
                      {meta.icon}
                      {meta.label}
                    </div>
                    <Badge
                      variant={
                        isDone(item.status) ? 'safe' : isFailed(item.status) ? 'danger' : 'info'
                      }
                    >
                      {isDone(item.status) ? '已完成' : isFailed(item.status) ? '失败' : '生成中'}
                    </Badge>
                  </div>
                  <p className={cn('text-sm text-white/85', !open && 'line-clamp-3')}>{item.prompt || '(无提示词)'}</p>
                  <div className="mt-3 flex items-center justify-between">
                    <span className="text-[11px] text-white/35">{fmtTime(item.created_at)}</span>
                    <button
                      onClick={() => setExpanded(open ? null : item.id)}
                      className="inline-flex items-center gap-1 text-[11px] font-bold text-celestial-saturn/90 hover:text-celestial-saturn"
                    >
                      <Eye size={12} />
                      {open ? '收起' : '详情'}
                    </button>
                  </div>
                  {open && item.url && (
                    <p className="mt-2 break-all rounded-lg bg-black/30 p-2 text-[11px] text-white/45">
                      文件：{item.url}
                    </p>
                  )}
                </GlassCard>
              </motion.div>
            );
          })}
        </div>
      )}
    </LumiPageShell>
  );
}
