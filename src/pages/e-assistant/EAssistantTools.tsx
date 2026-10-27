import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { motion, AnimatePresence } from 'motion/react';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import {
  Wrench,
  Search,
  CheckCircle,
  AlertTriangle,
  EyeOff,
  ShieldCheck,
  Shield,
  PlugZap,
  ChevronDown,
  FlaskConical,
  RotateCcw,
  Ban,
  FileCode2,
  CircleDot,
  Globe2,
  Plus,
  Server,
  Trash2,
  X,
} from 'lucide-react';
import { EAssistantPageShell } from '../../components/e-assistant/EAssistantPageShell';
import {
  Badge,
  SectionTitle,
  LoadingSpinner,
  EmptyState,
  IconBox,
} from '../../components/e-assistant/EAssistantShared';
import { cn } from '../../lib/cn';

/* ── 类型（与后端 ToolContractView / TrustedTool 严格对齐） ── */
interface ToolContractView {
  tool: string;
  side_effect: string;
  scope: string;
  reversible: boolean;
  verification: string;
  required_fields: string[];
  risk_level: string;
  registered: boolean;
}

/** 后端 TrustedTool（db/mod.rs 无 serde rename，字段保持 snake_case） */
interface TrustedTool {
  id: number;
  tool_name: string;
  path_pattern?: string | null;
  risk_level: string;
  added_at: string;
}

interface McpServerInfo {
  id: string;
  name?: string;
  transport?: string;
  status?: string;
  lastError?: string | null;
  error?: string | null;
  tools?: Array<{ name: string; description?: string }>;
}

interface McpToolInfo {
  server: string;
  name: string;
  description?: string;
}

interface McpCapabilitySnapshot {
  reachable?: boolean;
  servers?: McpServerInfo[];
  tools?: McpToolInfo[];
  error?: string;
}

type SecurityLevel = 'safe' | 'confirm' | 'forbidden';
type LevelFilter = 'all' | SecurityLevel;
type TrustFilter = 'all' | 'trusted' | 'untrusted';

/** 后端 risk_level 为 "LOW"/"MEDIUM"/"HIGH"/"-"，归一到三档 */
function normalizeLevel(raw: string | undefined): SecurityLevel {
  const v = String(raw || '').toUpperCase();
  if (['LOW', 'SAFE', 'AUTO', 'ALLOW', 'GRANTED'].includes(v)) return 'safe';
  if (['HIGH', 'CRITICAL', 'FORBIDDEN', 'DENY', 'BLOCK'].includes(v)) return 'forbidden';
  // MEDIUM / "-" / 未知 一律落到"需确认"，不自动执行
  return 'confirm';
}

const SECURITY_META: Record<
  SecurityLevel,
  { label: string; badgeVariant: 'safe' | 'warning' | 'danger'; rowClass: string; icon: ReactNode; dot: string }
> = {
  safe: {
    label: '自动执行',
    badgeVariant: 'safe',
    rowClass: 'bg-emerald-500/10 border-emerald-500/25 text-emerald-400',
    icon: <CheckCircle size={14} />,
    dot: 'bg-emerald-500',
  },
  confirm: {
    label: '需确认',
    badgeVariant: 'warning',
    rowClass: 'bg-amber-500/10 border-amber-500/25 text-amber-400',
    icon: <AlertTriangle size={14} />,
    dot: 'bg-amber-500',
  },
  forbidden: {
    label: '高风险',
    badgeVariant: 'danger',
    rowClass: 'bg-red-500/10 border-red-500/25 text-red-400',
    icon: <EyeOff size={14} />,
    dot: 'bg-red-500',
  },
};

/** side_effect → 中文短标签 */
const SIDE_EFFECT_LABEL: Record<string, string> = {
  local_read: '本地只读',
  local_write: '本地写入',
  local_state_change: '状态变更',
  desktop_control: '桌面控制',
  network_read: '网络只读',
  external_state_change: '外部变更',
  external_communication: '对外通信',
  process_execution: '进程执行',
  installation: '安装/卸载',
  none: '无副作用',
};

/** verification → 中文短标签 */
const VERIFICATION_LABEL: Record<string, string> = {
  terminal_receipt: '终端回执',
  state_diff: '状态差异',
  artifact: '产出物校验',
  provider_ack: '提供方确认',
  none: '无机器判据',
};

function sideEffectLabel(se: string): string {
  return SIDE_EFFECT_LABEL[se] || se || '未知';
}
function verificationLabel(v: string): string {
  return VERIFICATION_LABEL[v] || v || '—';
}

interface EAssistantToolsProps {
  /** 嵌入“能力中心”时由父页面提供标题与页签。 */
  embedded?: boolean;
}

export default function EAssistantTools({ embedded = false }: EAssistantToolsProps) {
  const [tools, setTools] = useState<ToolContractView[]>([]);
  const [trustedSet, setTrustedSet] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState('');
  const [levelFilter, setLevelFilter] = useState<LevelFilter>('all');
  const [trustFilter, setTrustFilter] = useState<TrustFilter>('all');
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [toggling, setToggling] = useState<string | null>(null);
  const [mcpServers, setMcpServers] = useState<McpServerInfo[]>([]);
  const [mcpTools, setMcpTools] = useState<McpToolInfo[]>([]);
  const [mcpError, setMcpError] = useState<string | null>(null);
  const [showInstall, setShowInstall] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [installForm, setInstallForm] = useState({ id: '', name: '', url: '' });

  const loadTools = async () => {
    setLoading(true);
    try {
      const [listRes, trustRes] = await Promise.allSettled([
        invoke<ToolContractView[]>('list_tool_contracts'),
        invoke<TrustedTool[]>('get_trusted_tools'),
      ]);
      if (listRes.status === 'fulfilled' && Array.isArray(listRes.value)) {
        setTools(listRes.value);
      } else {
        setTools([]);
        if (listRes.status === 'rejected') {
          toast.error(`工具契约加载失败：${String(listRes.reason)}`);
        }
      }
      if (trustRes.status === 'fulfilled' && Array.isArray(trustRes.value)) {
        // 后端 TrustedTool 字段为 tool_name（snake_case）
        setTrustedSet(new Set(trustRes.value.map((t) => t.tool_name)));
      } else {
        setTrustedSet(new Set());
      }
    } catch (e) {
      toast.error(`工具列表加载失败：${typeof e === 'string' ? e : '未知错误'}`);
    } finally {
      setLoading(false);
    }
  };

  const loadMcpCapabilities = async () => {
    try {
      const snapshot = await invoke<McpCapabilitySnapshot>('list_mcp_capabilities');
      setMcpServers(Array.isArray(snapshot.servers) ? snapshot.servers : []);
      setMcpTools(Array.isArray(snapshot.tools) ? snapshot.tools : []);
      setMcpError(snapshot.reachable === false ? snapshot.error || 'MCP 网关不可达' : null);
    } catch (error) {
      setMcpServers([]);
      setMcpTools([]);
      setMcpError(typeof error === 'string' ? error : 'MCP 工具源加载失败');
    }
  };

  useEffect(() => {
    loadTools();
    void loadMcpCapabilities();
  }, []);

  const installMcpServer = async () => {
    const id = installForm.id.trim();
    const name = installForm.name.trim();
    const url = installForm.url.trim();
    if (!id || !url) {
      toast.warning('请填写工具源 ID 和 MCP 地址');
      return;
    }
    setInstalling(true);
    try {
      const result = await invoke<{ tools?: Array<{ name: string }> }>('install_mcp_server', { id, name, url });
      const count = Array.isArray(result.tools) ? result.tools.length : 0;
      toast.success(`工具源安装成功，已发现 ${count} 个工具`);
      setShowInstall(false);
      setInstallForm({ id: '', name: '', url: '' });
      await loadMcpCapabilities();
    } catch (error) {
      toast.error(`安装失败：${typeof error === 'string' ? error : '未知错误'}`);
    } finally {
      setInstalling(false);
    }
  };

  const removeMcpServer = async (server: McpServerInfo) => {
    if (!window.confirm(`确定移除工具源「${server.name || server.id}」吗？AI 将无法再调用它提供的工具。`)) return;
    try {
      await invoke('remove_mcp_server', { id: server.id });
      toast.success(`已移除工具源：${server.name || server.id}`);
      await loadMcpCapabilities();
    } catch (error) {
      toast.error(`移除失败：${typeof error === 'string' ? error : '未知错误'}`);
    }
  };

  /* ── 过滤与分组 ────────────────────────────────────── */
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return tools.filter((t) => {
      if (q && !t.tool.toLowerCase().includes(q)) return false;
      if (levelFilter !== 'all' && normalizeLevel(t.risk_level) !== levelFilter) return false;
      const isTrusted = trustedSet.has(t.tool);
      if (trustFilter === 'trusted' && !isTrusted) return false;
      if (trustFilter === 'untrusted' && isTrusted) return false;
      return true;
    });
  }, [tools, query, levelFilter, trustFilter]);

  const groups = useMemo(() => {
    const result: Record<SecurityLevel, ToolContractView[]> = { safe: [], confirm: [], forbidden: [] };
    filtered.forEach((t) => result[normalizeLevel(t.risk_level)].push(t));
    return result;
  }, [filtered]);

  const stats = useMemo(
    () => ({
      total: tools.length,
      trusted: tools.filter((t) => trustedSet.has(t.tool)).length,
      registered: tools.filter((t) => t.registered).length,
    }),
    [tools, trustedSet]
  );

  /* ── 信任切换 ────────────────────────────────────── */
  const toggleTrust = async (tool: ToolContractView) => {
    if (toggling) return;
    const name = tool.tool;
    const isTrusted = trustedSet.has(name);
    setToggling(name);
    try {
      if (isTrusted) {
        // Tauri: JS camelCase → Rust snake_case
        await invoke('remove_trusted_tool', { toolName: name });
        setTrustedSet((prev) => {
          const next = new Set(prev);
          next.delete(name);
          return next;
        });
        toast.success(`已取消信任：${name}`);
      } else {
        // risk_level 是 Rust String（必填），传工具当前风险等级；
        // 未注册工具后端给的是 "-"，照传即可
        await invoke('add_trusted_tool', {
          toolName: name,
          riskLevel: tool.risk_level || 'MEDIUM',
        });
        setTrustedSet((prev) => new Set(prev).add(name));
        toast.success(`已信任工具：${name}`);
      }
    } catch (e) {
      toast.error(`操作失败：${typeof e === 'string' ? e : '未知错误'}`);
    } finally {
      setToggling(null);
    }
  };

  const toggleExpand = (name: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  /* ── 过滤器 chips ────────────────────────────────────── */
  const levelChips: { key: LevelFilter; label: string }[] = [
    { key: 'all', label: '全部等级' },
    { key: 'safe', label: '安全' },
    { key: 'confirm', label: '需确认' },
    { key: 'forbidden', label: '高风险' },
  ];
  const trustChips: { key: TrustFilter; label: string }[] = [
    { key: 'all', label: '全部状态' },
    { key: 'trusted', label: '已信任' },
    { key: 'untrusted', label: '未信任' },
  ];

  const installButton = (
    <button
      type="button"
      onClick={() => setShowInstall(true)}
      className="flex items-center gap-1.5 rounded-xl border border-celestial-saturn/25 bg-celestial-saturn/[0.12] px-3 py-2 text-xs font-black text-celestial-saturn transition-colors hover:bg-celestial-saturn/[0.18]"
    >
      <Plus size={14} />
      安装工具源
    </button>
  );

  const content = (
    <>
      {embedded && <div className="mb-4 flex justify-end">{installButton}</div>}
      {/* 统计行 */}
      <div className="mb-5 grid grid-cols-2 gap-3 lg:grid-cols-4">
        {[
          { label: '契约总数', value: stats.total, cls: 'text-celestial-saturn' },
          { label: '已注册', value: stats.registered, cls: 'text-emerald-400' },
          { label: '已信任', value: stats.trusted, cls: 'text-amber-400' },
          { label: '外部工具', value: mcpTools.length, cls: 'text-cyan-400' },
        ].map((s) => (
          <div key={s.label} className="glass-panel rounded-2xl p-4">
            <p className="text-[10px] font-bold uppercase tracking-wider text-white/40">{s.label}</p>
            <p className={cn('mt-1 text-2xl font-black tabular-nums', s.cls)}>{s.value}</p>
          </div>
        ))}
      </div>

      <SectionTitle label="外部工具源" count={mcpServers.length} color="bg-cyan-400" />
      {mcpError ? (
        <div className="mb-5 rounded-2xl border border-amber-500/20 bg-amber-500/[0.06] px-4 py-3">
          <p className="text-xs font-bold text-amber-300">MCP 网关未连接</p>
          <p className="mt-1 text-[11px] leading-relaxed text-amber-200/55">{mcpError}</p>
        </div>
      ) : mcpServers.length === 0 ? (
        <div className="mb-5 flex items-center justify-between gap-4 rounded-2xl border border-white/[0.06] bg-white/[0.02] px-4 py-3">
          <div>
            <p className="text-xs font-bold text-white/65">还没有外部工具源</p>
            <p className="mt-1 text-[11px] text-white/35">安装标准 MCP 服务后，AI 会自动发现它公开的工具。</p>
          </div>
          {installButton}
        </div>
      ) : (
        <div className="mb-5 grid gap-3 md:grid-cols-2">
          {mcpServers.map((server) => {
            const tools = mcpTools.filter((tool) => tool.server === server.id);
            const connected = server.status === 'connected' && !server.error;
            return (
              <div key={server.id} className="rounded-2xl border border-white/[0.08] bg-white/[0.025] p-4">
                <div className="flex items-start gap-3">
                  <IconBox
                    size="sm"
                    className={connected ? 'border-cyan-400/20 bg-cyan-400/10 text-cyan-300' : 'border-amber-400/20 bg-amber-400/10 text-amber-300'}
                    icon={<Server size={16} />}
                  />
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <p className="truncate text-sm font-bold text-white/85">{server.name || server.id}</p>
                      <Badge variant={connected ? 'safe' : 'warning'}>{connected ? '已连接' : '异常'}</Badge>
                    </div>
                    <p className="mt-1 font-mono text-[10px] text-white/30">{server.id} · {server.transport || 'http'}</p>
                    <p className="mt-2 text-xs text-white/45">{tools.length} 个工具已适配</p>
                    {(server.error || server.lastError) && (
                      <p className="mt-1 line-clamp-2 text-[10px] text-amber-300/65">{server.error || server.lastError}</p>
                    )}
                  </div>
                  <button
                    type="button"
                    title="移除工具源"
                    onClick={() => void removeMcpServer(server)}
                    className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-white/10 bg-white/5 text-white/35 transition-colors hover:border-red-500/30 hover:bg-red-500/10 hover:text-red-400"
                  >
                    <Trash2 size={13} />
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* 搜索框 */}
      <div className="relative mb-3">
        <Search size={15} className="absolute left-3 top-1/2 -translate-y-1/2 text-white/35" />
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜索工具名..."
          className="w-full rounded-xl border border-white/10 bg-black/25 py-2.5 pl-9 pr-3 text-sm text-white outline-none placeholder:text-white/25 focus:border-celestial-saturn/40"
        />
      </div>

      {/* 筛选 chips */}
      <div className="mb-5 flex flex-wrap items-center gap-2">
        <span className="text-[10px] font-bold uppercase tracking-wider text-white/35">等级</span>
        {levelChips.map((c) => (
          <button
            key={c.key}
            onClick={() => setLevelFilter(c.key)}
            className={cn(
              'rounded-lg border px-2.5 py-1 text-[11px] font-bold transition-colors',
              levelFilter === c.key
                ? 'border-celestial-saturn/50 bg-celestial-saturn/15 text-celestial-saturn'
                : 'border-white/10 bg-white/[0.03] text-white/45 hover:bg-white/[0.07]'
            )}
          >
            {c.label}
          </button>
        ))}
        <span className="ml-2 text-[10px] font-bold uppercase tracking-wider text-white/35">信任</span>
        {trustChips.map((c) => (
          <button
            key={c.key}
            onClick={() => setTrustFilter(c.key)}
            className={cn(
              'rounded-lg border px-2.5 py-1 text-[11px] font-bold transition-colors',
              trustFilter === c.key
                ? 'border-emerald-500/50 bg-emerald-500/15 text-emerald-400'
                : 'border-white/10 bg-white/[0.03] text-white/45 hover:bg-white/[0.07]'
            )}
          >
            {c.label}
          </button>
        ))}
      </div>

      {loading ? (
        <LoadingSpinner label="正在加载工具契约..." />
      ) : filtered.length === 0 ? (
        <EmptyState
          icon={<PlugZap size={28} />}
          title={tools.length === 0 ? '暂无工具契约' : '没有匹配的工具'}
          description={tools.length === 0 ? '后端注册工具后将在这里展示' : '换个关键词或筛选条件试试'}
        />
      ) : (
        (['safe', 'confirm', 'forbidden'] as SecurityLevel[]).map((level) => {
          const meta = SECURITY_META[level];
          const items = groups[level];
          return (
            <div key={level} className="mb-6">
              <SectionTitle label={meta.label} count={items.length} color={meta.dot} />
              {items.length === 0 ? (
                <p className="rounded-2xl border border-white/[0.06] bg-white/[0.02] px-4 py-3 text-xs italic text-white/35">
                  该分类下暂无工具
                </p>
              ) : (
                <div className="space-y-1.5">
                  {items.map((tool, idx) => {
                    const isTrusted = trustedSet.has(tool.tool);
                    const isOpen = expanded.has(tool.tool);
                    return (
                      <motion.div
                        key={tool.tool}
                        initial={{ opacity: 0, y: 8 }}
                        animate={{ opacity: 1, y: 0 }}
                        transition={{ delay: idx * 0.02 }}
                        className={cn(
                          'rounded-2xl border transition-colors',
                          isOpen
                            ? 'border-white/[0.14] bg-white/[0.06]'
                            : 'border-white/[0.08] bg-white/[0.04] hover:bg-white/[0.07]'
                        )}
                      >
                        {/* 主行 */}
                        <div className="flex items-center gap-3 px-4 py-3">
                          <button
                            onClick={() => toggleExpand(tool.tool)}
                            className="flex min-w-0 flex-1 items-center gap-3 text-left"
                            title="展开详情"
                          >
                            <IconBox
                              size="sm"
                              className={cn('border', meta.rowClass)}
                              icon={meta.icon}
                            />
                            <div className="min-w-0 flex-1">
                              <div className="flex flex-wrap items-center gap-2">
                                <span className="font-mono text-xs font-bold text-white/85">
                                  {tool.tool}
                                </span>
                                <Badge variant={meta.badgeVariant}>{meta.label}</Badge>
                                <Badge variant="neutral" className="normal-case">
                                  {sideEffectLabel(tool.side_effect)}
                                </Badge>
                                {!tool.registered && (
                                  <span className="inline-flex items-center rounded-md border border-white/10 bg-white/5 px-1.5 py-0.5 text-[10px] font-bold text-white/40">
                                    未注册
                                  </span>
                                )}
                                {tool.reversible && (
                                  <span
                                    className="inline-flex items-center gap-0.5 rounded-md border border-cyan-500/30 bg-cyan-500/10 px-1.5 py-0.5 text-[10px] font-bold text-cyan-400"
                                    title="操作可逆"
                                  >
                                    <RotateCcw size={10} />
                                    可逆
                                  </span>
                                )}
                              </div>
                              <p className="mt-0.5 truncate text-xs text-white/55">
                                {tool.scope || '—'}
                              </p>
                            </div>
                          </button>

                          {/* 信任状态 */}
                          {isTrusted && (
                            <span className="hidden items-center gap-1 rounded-md border border-emerald-500/30 bg-emerald-500/10 px-1.5 py-0.5 text-[10px] font-black uppercase text-emerald-400 sm:inline-flex">
                              <ShieldCheck size={10} />
                              已信任
                            </span>
                          )}

                          {/* 调用测试（占位禁用） */}
                          <button
                            disabled
                            title="后端暂未提供 run_tool 接口，调用测试待实现"
                            className="flex h-8 shrink-0 items-center gap-1 rounded-lg border border-white/10 bg-white/[0.03] px-2 text-[10px] font-bold text-white/25 cursor-not-allowed"
                          >
                            <FlaskConical size={12} />
                            调用测试
                          </button>

                          {/* 信任切换 */}
                          <button
                            onClick={() => toggleTrust(tool)}
                            disabled={toggling === tool.tool}
                            title={isTrusted ? '取消信任' : '信任此工具'}
                            className={cn(
                              'flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border transition-colors',
                              isTrusted
                                ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-400 hover:bg-emerald-500/20'
                                : 'border-white/10 bg-white/5 text-white/40 hover:border-celestial-saturn/30 hover:text-celestial-saturn'
                            )}
                          >
                            <Shield size={14} />
                          </button>

                          <ChevronDown
                            size={14}
                            className={cn(
                              'shrink-0 text-white/30 transition-transform',
                              isOpen && 'rotate-180'
                            )}
                          />
                        </div>

                        {/* 展开详情 */}
                        <AnimatePresence initial={false}>
                          {isOpen && (
                            <motion.div
                              initial={{ height: 0, opacity: 0 }}
                              animate={{ height: 'auto', opacity: 1 }}
                              exit={{ height: 0, opacity: 0 }}
                              transition={{ duration: 0.18 }}
                              className="overflow-hidden border-t border-white/[0.06]"
                            >
                              <div className="grid gap-3 px-4 py-3 md:grid-cols-2">
                                <DetailRow label="副作用类型" value={sideEffectLabel(tool.side_effect)} sub={tool.side_effect} />
                                <DetailRow label="作用域" value={tool.scope || '—'} />
                                <DetailRow
                                  label="是否可逆"
                                  value={tool.reversible ? '可逆' : '不可逆'}
                                  icon={tool.reversible ? <RotateCcw size={11} /> : <Ban size={11} />}
                                  tone={tool.reversible ? 'ok' : 'warn'}
                                />
                                <DetailRow label="验证方式" value={verificationLabel(tool.verification)} sub={tool.verification} />
                                <DetailRow
                                  label="注册状态"
                                  value={tool.registered ? '已注册' : '未注册'}
                                  icon={tool.registered ? <CheckCircle size={11} /> : <CircleDot size={11} />}
                                  tone={tool.registered ? 'ok' : 'warn'}
                                />
                                <DetailRow label="风险等级" value={tool.risk_level || '—'} />
                                <div className="md:col-span-2">
                                  <p className="mb-1.5 text-[10px] font-bold uppercase tracking-wider text-white/40">
                                    必填参数（{tool.required_fields.length}）
                                  </p>
                                  {tool.required_fields.length === 0 ? (
                                    <p className="text-xs italic text-white/30">无必填参数</p>
                                  ) : (
                                    <div className="flex flex-wrap gap-1.5">
                                      {tool.required_fields.map((f) => (
                                        <span
                                          key={f}
                                          className="inline-flex items-center gap-1 rounded-md border border-celestial-saturn/25 bg-celestial-saturn/10 px-1.5 py-0.5 font-mono text-[10px] text-celestial-saturn"
                                        >
                                          <FileCode2 size={10} />
                                          {f}
                                        </span>
                                      ))}
                                    </div>
                                  )}
                                </div>
                              </div>
                            </motion.div>
                          )}
                        </AnimatePresence>
                      </motion.div>
                    );
                  })}
                </div>
              )}
            </div>
          );
        })
      )}

      <AnimatePresence>
        {showInstall && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/65 p-4 backdrop-blur-sm"
            onClick={() => !installing && setShowInstall(false)}
          >
            <motion.div
              initial={{ opacity: 0, scale: 0.96, y: 10 }}
              animate={{ opacity: 1, scale: 1, y: 0 }}
              exit={{ opacity: 0, scale: 0.96, y: 10 }}
              onClick={(event) => event.stopPropagation()}
              className="glass-panel w-full max-w-lg rounded-2xl p-6"
            >
              <div className="mb-4 flex items-center justify-between">
                <div>
                  <h3 className="text-sm font-bold text-white/90">安装 MCP 工具源</h3>
                  <p className="mt-1 text-[11px] text-white/35">连接、发现工具并验证成功后才会保存。</p>
                </div>
                <button type="button" onClick={() => !installing && setShowInstall(false)} className="text-white/40 hover:text-white">
                  <X size={18} />
                </button>
              </div>

              <div className="space-y-3">
                <label className="block">
                  <span className="mb-1 block text-[10px] font-black uppercase text-white/40">工具源 ID</span>
                  <input
                    value={installForm.id}
                    onChange={(event) => setInstallForm((prev) => ({ ...prev, id: event.target.value }))}
                    placeholder="例如 notion-tools"
                    className="w-full rounded-xl border border-white/10 bg-black/25 px-3 py-2 font-mono text-xs text-white outline-none placeholder:text-white/25 focus:border-celestial-saturn/40"
                  />
                </label>
                <label className="block">
                  <span className="mb-1 block text-[10px] font-black uppercase text-white/40">显示名称</span>
                  <input
                    value={installForm.name}
                    onChange={(event) => setInstallForm((prev) => ({ ...prev, name: event.target.value }))}
                    placeholder="例如 Notion 工具"
                    className="w-full rounded-xl border border-white/10 bg-black/25 px-3 py-2 text-xs text-white outline-none placeholder:text-white/25 focus:border-celestial-saturn/40"
                  />
                </label>
                <label className="block">
                  <span className="mb-1 flex items-center gap-1 text-[10px] font-black uppercase text-white/40"><Globe2 size={10} /> Streamable HTTP 地址</span>
                  <input
                    value={installForm.url}
                    onChange={(event) => setInstallForm((prev) => ({ ...prev, url: event.target.value }))}
                    placeholder="https://example.com/mcp"
                    className="w-full rounded-xl border border-white/10 bg-black/25 px-3 py-2 font-mono text-xs text-white outline-none placeholder:text-white/25 focus:border-celestial-saturn/40"
                  />
                </label>

                <div className="rounded-xl border border-amber-500/15 bg-amber-500/[0.05] px-3 py-2 text-[10px] leading-relaxed text-amber-200/55">
                  安装会与该地址建立连接并读取工具名称、说明和参数结构。第三方工具每次执行仍按高风险操作确认，不会因安装而自动获得系统权限。
                </div>

                <button
                  type="button"
                  onClick={() => void installMcpServer()}
                  disabled={installing}
                  className="w-full rounded-xl border border-celestial-saturn/30 bg-celestial-saturn/15 py-2.5 text-xs font-black text-celestial-saturn transition-colors hover:bg-celestial-saturn/25 disabled:opacity-40"
                >
                  {installing ? '正在连接并验证…' : '确认安装'}
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
    <EAssistantPageShell
      icon={<Wrench size={20} />}
      iconBg="bg-amber-500/15 border-amber-400/20 text-amber-400"
      title="工具控制面板"
      subtitle={loading ? '正在加载...' : `${tools.length} 个能力契约`}
      actions={installButton}
    >
      {content}
    </EAssistantPageShell>
  );
}

/* ── 详情行小组件 ── */
function DetailRow({
  label,
  value,
  sub,
  icon,
  tone,
}: {
  label: string;
  value: string;
  sub?: string;
  icon?: ReactNode;
  tone?: 'ok' | 'warn';
}) {
  return (
    <div className="rounded-lg border border-white/[0.06] bg-black/20 px-3 py-2">
      <p className="text-[10px] font-bold uppercase tracking-wider text-white/40">{label}</p>
      <p
        className={cn(
          'mt-0.5 flex items-center gap-1.5 text-xs font-semibold',
          tone === 'ok' && 'text-emerald-400',
          tone === 'warn' && 'text-amber-400',
          !tone && 'text-white/80'
        )}
      >
        {icon}
        {value}
      </p>
      {sub && <p className="mt-0.5 font-mono text-[10px] text-white/35">{sub}</p>}
    </div>
  );
}
