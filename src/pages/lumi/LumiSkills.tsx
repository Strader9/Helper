import { useEffect, useMemo, useState } from 'react';
import { motion, AnimatePresence } from 'motion/react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { toast } from 'sonner';
import {
  Puzzle,
  Plus,
  Search,
  X,
  Trash2,
  ChevronDown,
  Tag,
  Clock,
  User,
  Zap,
  PackageOpen,
  FolderOpen,
  Play,
  ShieldCheck,
} from 'lucide-react';
import { LumiPageShell } from '../../components/lumi/LumiPageShell';
import {
  GlassCard,
  Badge,
  ToggleSwitch,
  LoadingSpinner,
  EmptyState,
  IconBox,
} from '../../components/lumi/LumiShared';
import { cn } from '../../lib/cn';

/* ── 类型（与后端 get_skills / get_skill_details 返回结构对齐）────────── */
interface SkillInfo {
  id: string;
  name: string;
  description: string;
  version: string;
  author: string;
  skill_type: string; // 下划线字段
  enabled: boolean;
  installed_at: string; // 下划线字段
  permissions: string[];
  triggers: string[];
}

type StatusFilter = 'all' | 'enabled' | 'disabled';

/** 按类型名稳定取一个高亮色，用于图标容器 */
function categoryColor(type: unknown): string {
  const palette = [
    'bg-amber-500/10 text-amber-400 border-amber-500/20',
    'bg-emerald-500/10 text-emerald-400 border-emerald-500/20',
    'bg-sky-500/10 text-sky-400 border-sky-500/20',
    'bg-violet-500/10 text-violet-400 border-violet-500/20',
    'bg-rose-500/10 text-rose-400 border-rose-500/20',
    'bg-cyan-500/10 text-cyan-400 border-cyan-500/20',
  ];
  const str = typeof type === 'string' ? type : String(type || '');
  let hash = 0;
  for (const ch of str) hash = (hash * 31 + ch.charCodeAt(0)) >>> 0;
  return palette[hash % palette.length];
}

/** 把可能为 undefined / 字符串 / 数组的字段统一成字符串数组 */
function toList(v: string[] | string | undefined | null): string[] {
  if (!v) return [];
  if (Array.isArray(v)) return v.filter((x) => typeof x === 'string' && x.trim());
  return String(v)
    .split(/[,\n]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

/** 安全格式化安装时间（兼容 ISO 字符串 / 时间戳） */
function formatInstalledAt(raw: unknown): string {
  if (!raw) return '';
  try {
    const d = new Date(typeof raw === 'number' ? raw : String(raw));
    if (Number.isNaN(d.getTime())) return String(raw);
    return d.toLocaleString('zh-CN', {
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
    });
  } catch {
    return String(raw);
  }
}

interface LumiSkillsProps {
  /** 嵌入“能力中心”时由父页面提供标题与页签。 */
  embedded?: boolean;
}

export default function LumiSkills({ embedded = false }: LumiSkillsProps) {
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState('');
  const [typeFilter, setTypeFilter] = useState<string>('全部');
  const [statusFilter, setStatusFilter] = useState<StatusFilter>('all');
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [details, setDetails] = useState<Record<string, SkillInfo>>({});
  const [detailLoading, setDetailLoading] = useState<string | null>(null);
  const [showInstall, setShowInstall] = useState(false);
  const [installSourceDir, setInstallSourceDir] = useState('');
  const [installing, setInstalling] = useState(false);
  const [togglingId, setTogglingId] = useState<string | null>(null);

  /* ── 加载技能列表（get_skills）────────────────────────── */
  const loadSkills = async () => {
    setLoading(true);
    try {
      const list = await invoke<SkillInfo[]>('get_skills');
      setSkills(Array.isArray(list) ? list : []);
    } catch (e) {
      setSkills([]);
      toast.error(`技能列表加载失败：${typeof e === 'string' ? e : '未知错误'}`);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadSkills();
  }, []);

  /* ── 类型与状态筛选 ──────────────────────────────────── */
  const types = useMemo(() => {
    const set = new Set<string>();
    skills.forEach((s) => s.skill_type && set.add(s.skill_type));
    return ['全部', ...Array.from(set)];
  }, [skills]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return skills.filter((s) => {
      const matchType = typeFilter === '全部' || s.skill_type === typeFilter;
      const matchStatus =
        statusFilter === 'all' ||
        (statusFilter === 'enabled' && s.enabled) ||
        (statusFilter === 'disabled' && !s.enabled);
      const matchQuery =
        !q ||
        s.name.toLowerCase().includes(q) ||
        s.description.toLowerCase().includes(q);
      return matchType && matchStatus && matchQuery;
    });
  }, [skills, query, typeFilter, statusFilter]);

  /* ── 详情展开（get_skill_details）────────────────────── */
  const toggleExpand = async (id: string) => {
    if (expandedId === id) {
      setExpandedId(null);
      return;
    }
    setExpandedId(id);
    if (details[id]) return;
    setDetailLoading(id);
    try {
      const d = await invoke<SkillInfo>('get_skill_details', { skillId: id });
      setDetails((prev) => ({ ...prev, [id]: d }));
    } catch (e) {
      toast.error(`技能详情加载失败：${typeof e === 'string' ? e : '未知错误'}`);
    } finally {
      setDetailLoading(null);
    }
  };

  /* ── 启用 / 禁用（toggle_skill）───────────────────────── */
  const toggleSkill = async (skill: SkillInfo) => {
    if (togglingId) return;
    setTogglingId(skill.id);
    const next = !skill.enabled;
    setSkills((prev) => prev.map((s) => (s.id === skill.id ? { ...s, enabled: next } : s)));
    try {
      await invoke('toggle_skill', { skillId: skill.id, enabled: next });
      toast.success(next ? `已启用：${skill.name}` : `已禁用：${skill.name}`);
    } catch (e) {
      setSkills((prev) => prev.map((s) => (s.id === skill.id ? { ...s, enabled: !next } : s)));
      toast.error(`操作失败：${typeof e === 'string' ? e : '未知错误'}`);
    } finally {
      setTogglingId(null);
    }
  };

  /* ── 卸载技能（uninstall_skill）───────────────────────── */
  const uninstallSkill = async (skill: SkillInfo) => {
    if (!window.confirm(`确定卸载技能「${skill.name}」吗？该操作不可恢复。`)) return;
    try {
      await invoke('uninstall_skill', { skillId: skill.id });
      setSkills((prev) => prev.filter((s) => s.id !== skill.id));
      setDetails((prev) => {
        const next = { ...prev };
        delete next[skill.id];
        return next;
      });
      if (expandedId === skill.id) setExpandedId(null);
      toast.success(`已卸载：${skill.name}`);
    } catch (e) {
      toast.error(`卸载失败：${typeof e === 'string' ? e : '未知错误'}`);
    }
  };

  /* ── 安装技能（install_skill，sourceDir）──────────────── */
  const browseDir = async () => {
    try {
      const dir = await open({
        directory: true,
        multiple: false,
        title: '选择技能目录（含 manifest）',
      });
      if (dir && typeof dir === 'string') setInstallSourceDir(dir);
    } catch (e) {
      toast.error(`目录选择失败：${typeof e === 'string' ? e : '未知错误'}`);
    }
  };

  const submitInstall = async () => {
    const sourceDir = installSourceDir.trim();
    if (!sourceDir) {
      toast.warning('请选择或输入技能目录路径');
      return;
    }
    setInstalling(true);
    try {
      await invoke('install_skill', { sourceDir });
      toast.success('技能安装成功');
      setShowInstall(false);
      setInstallSourceDir('');
      await loadSkills();
    } catch (e) {
      toast.error(`安装失败：${typeof e === 'string' ? e : '未知错误'}`);
    } finally {
      setInstalling(false);
    }
  };

  const statusChips: { key: StatusFilter; label: string }[] = [
    { key: 'all', label: '全部状态' },
    { key: 'enabled', label: '已启用' },
    { key: 'disabled', label: '已禁用' },
  ];

  const installButton = (
    <button
      onClick={() => setShowInstall(true)}
      className="flex items-center gap-1.5 rounded-xl border border-celestial-saturn/25 bg-celestial-saturn/[0.12] px-3 py-2 text-xs font-black text-celestial-saturn transition-colors hover:bg-celestial-saturn/[0.18]"
    >
      <Plus size={14} />
      安装技能
    </button>
  );

  const content = (
    <>
      {embedded && <div className="mb-4 flex justify-end">{installButton}</div>}
      {/* 搜索 */}
      <div className="relative mb-4">
        <Search size={15} className="absolute left-3 top-1/2 -translate-y-1/2 text-white/35" />
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="按名称或描述搜索技能..."
          className="w-full rounded-xl border border-white/10 bg-black/25 py-2.5 pl-9 pr-3 text-sm text-white outline-none placeholder:text-white/25 focus:border-celestial-saturn/40"
        />
      </div>

      {/* 类型标签 */}
      <div className="mb-3 flex flex-wrap gap-2">
        {types.map((t) => (
          <button
            key={t}
            onClick={() => setTypeFilter(t)}
            className={cn(
              'rounded-full border px-3 py-1.5 text-xs font-bold transition-colors',
              typeFilter === t
                ? 'border-celestial-saturn/40 bg-celestial-saturn/15 text-celestial-saturn'
                : 'border-white/10 bg-white/[0.04] text-white/50 hover:bg-white/[0.08] hover:text-white/80'
            )}
          >
            {t}
          </button>
        ))}
      </div>

      {/* 状态筛选 */}
      <div className="mb-5 flex flex-wrap items-center gap-2">
        <span className="text-[10px] font-black uppercase tracking-wider text-white/30">状态</span>
        {statusChips.map((s) => (
          <button
            key={s.key}
            onClick={() => setStatusFilter(s.key)}
            className={cn(
              'rounded-full border px-2.5 py-1 text-[11px] font-bold transition-colors',
              statusFilter === s.key
                ? 'border-emerald-500/40 bg-emerald-500/15 text-emerald-400'
                : 'border-white/10 bg-white/[0.04] text-white/50 hover:bg-white/[0.08] hover:text-white/80'
            )}
          >
            {s.label}
          </button>
        ))}
      </div>

      {loading ? (
        <LoadingSpinner label="正在加载技能..." />
      ) : filtered.length === 0 ? (
        <EmptyState
          icon={<PackageOpen size={28} />}
          title={skills.length === 0 ? '还没有安装任何技能' : '没有匹配的技能'}
          description={
            skills.length === 0
              ? '点击右上角「安装技能」来扩展你的助手能力'
              : '换个类型、状态或关键词试试'
          }
        />
      ) : (
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2">
          {filtered.map((skill, idx) => {
            const expanded = expandedId === skill.id;
            // 详情未返回前先用列表数据，返回后合并展示
            const detail = details[skill.id] ?? skill;
            const triggers = toList(detail.triggers);
            const permissions = toList(detail.permissions);
            const installedAt = formatInstalledAt(detail.installed_at);
            return (
              <motion.div
                key={skill.id}
                initial={{ opacity: 0, y: 10 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ delay: idx * 0.04 }}
              >
                <GlassCard
                  hoverEffect
                  className={cn(
                    '!p-4',
                    expanded && 'border-celestial-saturn/25',
                    !skill.enabled && 'opacity-55 saturate-50'
                  )}
                >
                  <div className="flex items-start gap-3">
                    <IconBox size="sm" className={categoryColor(skill.skill_type)} icon={<Zap size={16} />} />
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <h3 className="text-sm font-bold text-white/90">{skill.name}</h3>
                        {skill.version && <Badge variant="neutral">v{skill.version}</Badge>}
                        <Badge variant={skill.enabled ? 'safe' : 'neutral'}>
                          {skill.enabled ? '已启用' : '已禁用'}
                        </Badge>
                      </div>
                      <div className="mt-1 flex flex-wrap items-center gap-2 text-[10px] text-white/40">
                        {skill.skill_type && (
                          <span className="rounded border border-white/10 bg-white/5 px-1.5 py-0.5 uppercase">
                            {skill.skill_type}
                          </span>
                        )}
                        {skill.author && (
                          <span className="inline-flex items-center gap-1">
                            <User size={9} />
                            {skill.author}
                          </span>
                        )}
                      </div>
                      <p className="mt-2 line-clamp-2 text-xs leading-relaxed text-white/55">
                        {skill.description || '暂无描述'}
                      </p>
                    </div>
                  </div>

                  {/* 展开详情 */}
                  <AnimatePresence initial={false}>
                    {expanded && (
                      <motion.div
                        initial={{ height: 0, opacity: 0 }}
                        animate={{ height: 'auto', opacity: 1 }}
                        exit={{ height: 0, opacity: 0 }}
                        className="overflow-hidden"
                      >
                        <div className="mt-4 space-y-3 border-t border-white/[0.06] pt-3">
                          {detailLoading === skill.id ? (
                            <p className="text-xs text-white/40">正在读取详情...</p>
                          ) : (
                            <>
                              {/* 完整描述 */}
                              {skill.description && (
                                <p className="text-xs leading-relaxed text-white/65">{skill.description}</p>
                              )}

                              {/* 元信息 */}
                              <div className="flex flex-wrap gap-x-4 gap-y-1 text-[10px] text-white/40">
                                {skill.version && <span>版本 v{skill.version}</span>}
                                {skill.author && (
                                  <span className="inline-flex items-center gap-1">
                                    <User size={9} /> {skill.author}
                                  </span>
                                )}
                                {installedAt && (
                                  <span className="inline-flex items-center gap-1">
                                    <Clock size={9} /> 安装于 {installedAt}
                                  </span>
                                )}
                              </div>

                              {/* 权限列表 */}
                              {permissions.length > 0 && (
                                <div>
                                  <p className="mb-1.5 flex items-center gap-1 text-[10px] font-black uppercase text-white/40">
                                    <ShieldCheck size={10} /> 权限
                                  </p>
                                  <div className="flex flex-wrap gap-1.5">
                                    {permissions.map((p) => (
                                      <span
                                        key={p}
                                        className="rounded-md border border-amber-500/20 bg-amber-500/10 px-1.5 py-0.5 text-[10px] text-amber-300"
                                      >
                                        {p}
                                      </span>
                                    ))}
                                  </div>
                                </div>
                              )}

                              {/* 触发词列表 */}
                              {triggers.length > 0 && (
                                <div>
                                  <p className="mb-1.5 flex items-center gap-1 text-[10px] font-black uppercase text-white/40">
                                    <Tag size={10} /> 触发词
                                  </p>
                                  <div className="flex flex-wrap gap-1.5">
                                    {triggers.map((t) => (
                                      <span
                                        key={t}
                                        className="rounded-md border border-cyan-500/20 bg-cyan-500/10 px-1.5 py-0.5 text-[10px] text-cyan-300"
                                      >
                                        {t}
                                      </span>
                                    ))}
                                  </div>
                                </div>
                              )}
                            </>
                          )}
                        </div>
                      </motion.div>
                    )}
                  </AnimatePresence>

                  {/* 底部操作条 */}
                  <div className="mt-3 flex items-center justify-between border-t border-white/[0.06] pt-3">
                    <button
                      onClick={() => toggleExpand(skill.id)}
                      className="inline-flex items-center gap-1 text-xs text-white/45 transition-colors hover:text-celestial-saturn"
                    >
                      <ChevronDown
                        size={14}
                        className={cn('transition-transform', expanded && 'rotate-180')}
                      />
                      {expanded ? '收起详情' : '查看详情'}
                    </button>
                    <div className="flex items-center gap-2">
                      {/* 运行技能：后端暂未提供 run_skill，做禁用占位 */}
                      <button
                        disabled
                        title="后端待实现：技能运行接口尚未接入"
                        className="flex h-8 items-center gap-1 rounded-lg border border-white/10 bg-white/5 px-2 text-[11px] font-bold text-white/25 cursor-not-allowed"
                      >
                        <Play size={12} />
                        运行
                      </button>
                      <button
                        onClick={() => uninstallSkill(skill)}
                        title="卸载技能"
                        className="flex h-8 w-8 items-center justify-center rounded-lg border border-white/10 bg-white/5 text-white/40 transition-colors hover:border-red-500/30 hover:bg-red-500/10 hover:text-red-400"
                      >
                        <Trash2 size={14} />
                      </button>
                      <ToggleSwitch
                        enabled={skill.enabled}
                        onChange={() => toggleSkill(skill)}
                        disabled={togglingId === skill.id}
                      />
                    </div>
                  </div>
                </GlassCard>
              </motion.div>
            );
          })}
        </div>
      )}

      {/* 安装技能弹窗（输入目录路径，可调用 Tauri dialog 浏览选择） */}
      <AnimatePresence>
        {showInstall && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-sm"
            onClick={() => !installing && setShowInstall(false)}
          >
            <motion.div
              initial={{ scale: 0.95, y: 10 }}
              animate={{ scale: 1, y: 0 }}
              exit={{ scale: 0.95, y: 10 }}
              onClick={(e) => e.stopPropagation()}
              className="glass-panel w-full max-w-lg rounded-2xl p-6"
            >
              <div className="mb-4 flex items-center justify-between">
                <h3 className="text-sm font-bold text-white/90">安装新技能</h3>
                <button
                  onClick={() => !installing && setShowInstall(false)}
                  className="text-white/40 transition-colors hover:text-white"
                >
                  <X size={18} />
                </button>
              </div>
              <div className="space-y-3">
                <div>
                  <label className="mb-1 block text-[10px] font-black uppercase text-white/40">
                    技能目录路径（含 manifest）
                  </label>
                  <div className="flex gap-2">
                    <input
                      value={installSourceDir}
                      onChange={(e) => setInstallSourceDir(e.target.value)}
                      placeholder="例如：C:\\skills\\my-skill"
                      className="flex-1 rounded-xl border border-white/10 bg-black/25 px-3 py-2 font-mono text-xs text-white outline-none placeholder:text-white/25 focus:border-celestial-saturn/40"
                    />
                    <button
                      onClick={browseDir}
                      disabled={installing}
                      className="flex items-center gap-1.5 rounded-xl border border-white/10 bg-white/5 px-3 text-xs font-bold text-white/70 transition-colors hover:bg-white/10 disabled:opacity-40"
                    >
                      <FolderOpen size={14} />
                      浏览
                    </button>
                  </div>
                  <p className="mt-1.5 text-[10px] leading-relaxed text-white/30">
                    选择包含技能 manifest 的本地目录，将通过 install_skill(sourceDir) 安装。
                  </p>
                </div>
                <button
                  onClick={submitInstall}
                  disabled={installing}
                  className="w-full rounded-xl border border-celestial-saturn/30 bg-celestial-saturn/15 py-2.5 text-xs font-black text-celestial-saturn transition-colors hover:bg-celestial-saturn/25 disabled:opacity-40"
                >
                  {installing ? '正在安装...' : '确认安装'}
                </button>
              </div>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
    </>
  );

  if (embedded) return content;

  return (
    <LumiPageShell
      icon={<Puzzle size={20} />}
      iconBg="bg-emerald-500/15 border-emerald-400/20 text-emerald-400"
      title="技能大厅"
      subtitle={loading ? '正在加载...' : `${skills.length} 个已安装技能`}
      actions={installButton}
    >
      {content}
    </LumiPageShell>
  );
}
