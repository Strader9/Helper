import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { motion } from "motion/react";
import { useNavigate } from "react-router-dom";
import {
  Activity,
  AlertTriangle,
  ArrowUpRight,
  Cpu,
  HardDrive,
  RefreshCw,
  ShieldCheck,
  Sparkles,
  Wifi,
  WifiOff,
  Zap,
} from "lucide-react";
import { Page, PageBody, PageHeader, Grid } from "../components/ui/page";
import { Card, SectionHeader } from "../components/ui/card";
import { Badge, StatusDot } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { RingMetric, Progress, BigNumber, SkeletonCard, EmptyState } from "../components/ui/feedback";

interface DiskInfo {
  drive: string;
  total_gb: number;
  used_gb: number;
  usage_percent: number;
}

interface SystemMetrics {
  cpu_usage: number;
  memory_total_gb: number;
  memory_used_gb: number;
  memory_usage_percent: number;
  disks: DiskInfo[];
  network_ok: boolean;
  gpu_name: string | null;
  gpu_usage: number | null;
}

interface CoverageReport {
  total_tools: number;
  declared: number;
  missing: string[];
  material: number;
  material_verified: number;
  material_without_verification: string[];
  material_with_verification_debt: string[];
}

interface ToolReceipt {
  receipt_id: string;
  tool_name: string;
  risk_level: string;
  side_effect: string;
  verification_status: string;
  success: boolean;
  duration_ms: number;
  created_at: string;
  error_code?: string | null;
}

const riskTone = (risk: string): "success" | "warning" | "danger" | "neutral" => {
  switch (risk.toUpperCase()) {
    case "HIGH":
      return "danger";
    case "MEDIUM":
      return "warning";
    case "LOW":
      return "success";
    default:
      return "neutral";
  }
};

function greeting(): string {
  const h = new Date().getHours();
  if (h < 6) return "夜深了";
  if (h < 11) return "早上好";
  if (h < 14) return "中午好";
  if (h < 18) return "下午好";
  return "晚上好";
}

export default function Dashboard() {
  const navigate = useNavigate();
  const [metrics, setMetrics] = useState<SystemMetrics | null>(null);
  const [coverage, setCoverage] = useState<CoverageReport | null>(null);
  const [receipts, setReceipts] = useState<ToolReceipt[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [lastUpdate, setLastUpdate] = useState<Date | null>(null);

  const loadAll = useCallback(async (silent = false) => {
    if (!silent) setRefreshing(true);
    try {
      const [m, c, r] = await Promise.all([
        invoke<SystemMetrics>("get_system_metrics"),
        invoke<CoverageReport>("get_capability_coverage").catch(() => null),
        invoke<ToolReceipt[]>("get_tool_receipts", { limit: 6 }).catch(() => []),
      ]);
      setMetrics(m);
      setCoverage(c);
      setReceipts(r);
      setError(null);
      setLastUpdate(new Date());
    } catch (e) {
      setError(typeof e === "string" ? e : "获取系统指标失败");
    } finally {
      setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    loadAll(true);
    // 3s 轮询：CPU/内存是瞬时值，间隔长了就没有"实时"的意义。
    // 静默刷新，不闪 loading 骨架。
    const timer = setInterval(() => loadAll(true), 3000);
    return () => clearInterval(timer);
  }, [loadAll]);

  const alerts = useMemo(() => {
    if (!metrics) return [] as { tone: "warning" | "danger"; title: string; detail: string }[];
    const out: { tone: "warning" | "danger"; title: string; detail: string }[] = [];
    if (metrics.cpu_usage >= 90)
      out.push({ tone: "danger", title: "CPU 负载过高", detail: `当前 ${metrics.cpu_usage.toFixed(0)}%，建议检查高负载进程` });
    else if (metrics.cpu_usage >= 78)
      out.push({ tone: "warning", title: "CPU 负载偏高", detail: `当前 ${metrics.cpu_usage.toFixed(0)}%` });
    if (metrics.memory_usage_percent >= 90)
      out.push({ tone: "danger", title: "内存即将耗尽", detail: `${metrics.memory_used_gb.toFixed(1)} / ${metrics.memory_total_gb.toFixed(1)} GB` });
    if (!metrics.network_ok)
      out.push({ tone: "danger", title: "网络不可用", detail: "无法连接外部网络" });
    for (const d of metrics.disks) {
      if (d.usage_percent >= 95)
        out.push({ tone: "danger", title: `磁盘 ${d.drive} 空间告急`, detail: `已用 ${d.usage_percent.toFixed(0)}%` });
      else if (d.usage_percent >= 88)
        out.push({ tone: "warning", title: `磁盘 ${d.drive} 空间偏紧`, detail: `已用 ${d.usage_percent.toFixed(0)}%` });
    }
    return out;
  }, [metrics]);

  const diskMax = metrics?.disks.reduce((a, d) => Math.max(a, d.usage_percent), 0) ?? 0;

  return (
    <Page>
      <PageHeader
        title={`${greeting()}，欢迎回来`}
        description={
          lastUpdate
            ? `本地内核运行中 · 状态每 3 秒刷新 · 上次更新 ${lastUpdate.toLocaleTimeString("zh-CN", { hour12: false })}`
            : "正在读取本地内核状态"
        }
        icon={Activity}
        badge={
          metrics ? (
            <Badge tone={metrics.network_ok ? "success" : "danger"} dot pulse={metrics.network_ok}>
              {metrics.network_ok ? "本地在线" : "离线"}
            </Badge>
          ) : undefined
        }
        actions={
          <>
            <Button
              size="sm"
              variant="ghost"
              loading={refreshing}
              onClick={() => loadAll(false)}
              className="gap-1.5"
            >
              <RefreshCw className="size-3.5" />
              刷新
            </Button>
            <Button size="sm" variant="primary" onClick={() => navigate("/chat")} className="gap-1.5">
              <Sparkles className="size-3.5" />
              开始对话
            </Button>
          </>
        }
      />

      <PageBody>
        {/* 告警：只在真的有问题时出现，否则不占位 */}
        {alerts.length > 0 && (
          <motion.div
            initial={{ opacity: 0, y: -4 }}
            animate={{ opacity: 1, y: 0 }}
            className="mb-4 flex flex-col gap-2"
          >
            {alerts.map((a, i) => (
              <div
                key={i}
                className="flex items-start gap-2.5 rounded-[var(--radius-md)] border px-3 py-2"
                style={{
                  borderColor: `color-mix(in oklab, var(--color-${a.tone === "danger" ? "danger" : "warning"}) 28%, transparent)`,
                  background: `color-mix(in oklab, var(--color-${a.tone === "danger" ? "danger" : "warning"}) 8%, transparent)`,
                }}
              >
                <AlertTriangle
                  className="mt-0.5 size-3.5 shrink-0"
                  style={{ color: `var(--color-${a.tone === "danger" ? "danger" : "warning"})` }}
                />
                <div className="min-w-0">
                  <span className="text-[var(--text-xs)] font-medium text-[var(--color-foreground)]">{a.title}</span>
                  <span className="ml-2 text-[var(--text-xs)] text-[var(--color-muted-foreground)]">{a.detail}</span>
                </div>
              </div>
            ))}
          </motion.div>
        )}

        {/* 指标环 */}
        {!metrics ? (
          <Grid cols={4} className="mb-4">
            {Array.from({ length: 4 }).map((_, i) => (
              <SkeletonCard key={i} />
            ))}
          </Grid>
        ) : (
          <motion.div
            initial="hidden"
            animate="show"
            variants={{ show: { transition: { staggerChildren: 0.05 } } }}
            className="mb-4 grid grid-cols-2 gap-3 lg:grid-cols-4"
          >
            {[
              <Card key="cpu" className="flex items-center justify-center py-5">
                <RingMetric value={metrics.cpu_usage} label="CPU" caption={`${Math.round(metrics.cpu_usage)}% 占用率`} />
              </Card>,
              <Card key="mem" className="flex items-center justify-center py-5">
                <RingMetric
                  value={metrics.memory_usage_percent}
                  label="内存"
                  caption={`${metrics.memory_used_gb.toFixed(1)} / ${metrics.memory_total_gb.toFixed(1)} GB`}
                />
              </Card>,
              <Card key="gpu" className="flex items-center justify-center py-5">
                {metrics.gpu_name && metrics.gpu_usage !== null ? (
                  <RingMetric value={metrics.gpu_usage} label="GPU" caption={metrics.gpu_name} tone="brand" />
                ) : (
                  <div className="flex flex-col items-center gap-1.5 py-3 text-center">
                    <Cpu className="size-5 text-[var(--color-subtle-foreground)]" />
                    <span className="text-[var(--text-xs)] text-[var(--color-muted-foreground)]">未检测到独显</span>
                  </div>
                )}
              </Card>,
              <Card key="disk" className="flex items-center justify-center py-5">
                <RingMetric
                  value={diskMax}
                  label="磁盘峰值"
                  caption={metrics.disks.length ? `${metrics.disks.length} 个分区` : "无分区数据"}
                />
              </Card>,
            ].map((node, i) => (
              <motion.div
                key={i}
                variants={{ hidden: { opacity: 0, y: 8 }, show: { opacity: 1, y: 0 } }}
                transition={{ duration: 0.35, ease: [0.16, 1, 0.3, 1] }}
              >
                {node}
              </motion.div>
            ))}
          </motion.div>
        )}

        <div className="grid grid-cols-1 gap-3 lg:grid-cols-3">
          {/* 左：分区 + 网络 */}
          <Card className="lg:col-span-2">
            <SectionHeader
              className="px-4 pt-4"
              title="存储与网络"
              description="磁盘剩余空间决定能不能继续下载模型、写临时文件"
              action={
                <Badge tone={metrics?.network_ok ? "success" : "danger"} dot>
                  {metrics?.network_ok ? "外网可达" : "外网不可达"}
                </Badge>
              }
            />
            <div className="px-4 pb-4 pt-1">
              {error ? (
                <div className="py-6 text-center text-[var(--text-xs)] text-[var(--color-danger)]">{error}</div>
              ) : !metrics ? (
                <div className="flex flex-col gap-3 py-2">
                  {[0, 1, 2].map((i) => (
                    <div key={i} className="h-8 skeleton-shimmer rounded-[8px]" />
                  ))}
                </div>
              ) : metrics.disks.length === 0 ? (
                <EmptyState compact icon={HardDrive} title="读不到磁盘信息" description="系统接口未返回分区数据" />
              ) : (
                <div className="flex flex-col gap-3">
                  {metrics.disks.map((d) => {
                    const tone =
                      d.usage_percent >= 90 ? "danger" : d.usage_percent >= 78 ? "warning" : "brand";
                    return (
                      <div key={d.drive}>
                        <div className="mb-1.5 flex items-baseline justify-between gap-3">
                          <div className="flex items-center gap-1.5">
                            <HardDrive className="size-3.5 text-[var(--color-subtle-foreground)]" />
                            <span className="font-mono text-[var(--text-xs)] font-medium text-[var(--color-foreground)]">
                              {d.drive}
                            </span>
                            {d.usage_percent >= 90 && <Badge tone="danger">空间不足</Badge>}
                          </div>
                          <span className="num-tabular text-[var(--text-xs)] text-[var(--color-muted-foreground)]">
                            {d.used_gb.toFixed(0)} / {d.total_gb.toFixed(0)} GB
                            <span className="ml-2 font-medium text-[var(--color-foreground)]">
                              {(d.total_gb - d.used_gb).toFixed(0)} GB 可用
                            </span>
                          </span>
                        </div>
                        <Progress value={d.usage_percent} tone={tone} height={5} />
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          </Card>

          {/* 右：能力契约 */}
          <Card>
            <SectionHeader
              className="px-4 pt-4"
              title="能力契约"
              description="每个工具都必须声明副作用与验证方式"
              action={<Badge tone="brand">{coverage?.total_tools ?? "—"} 工具</Badge>}
            />
            <div className="px-4 pb-4 pt-1">
              {!coverage ? (
                <div className="flex flex-col gap-2 py-2">
                  <div className="h-8 skeleton-shimmer rounded-[8px]" />
                  <div className="h-8 skeleton-shimmer rounded-[8px]" />
                </div>
              ) : (
                <>
                  <div className="grid grid-cols-2 gap-2">
                    <div className="rounded-[var(--radius-md)] border border-[var(--color-line)] bg-[var(--color-surface)] px-3 py-2.5">
                      <BigNumber value={coverage.declared} label="已声明契约" tone="brand" />
                    </div>
                    <div className="rounded-[var(--radius-md)] border border-[var(--color-line)] bg-[var(--color-surface)] px-3 py-2.5">
                      <BigNumber
                        value={coverage.material_verified}
                        label="实质副作用已验证"
                        tone={coverage.material_verified === coverage.material ? "success" : "default"}
                      />
                    </div>
                  </div>
                  <div className="mt-3 space-y-1.5">
                    <div className="flex items-center justify-between text-[var(--text-2xs)]">
                      <span className="text-[var(--color-muted-foreground)]">契约覆盖率</span>
                      <span className="num-tabular font-medium text-[var(--color-foreground)]">
                        {((coverage.declared / Math.max(coverage.total_tools, 1)) * 100).toFixed(1)}%
                      </span>
                    </div>
                    <Progress value={(coverage.declared / Math.max(coverage.total_tools, 1)) * 100} height={4} />
                  </div>
                  {coverage.missing.length > 0 ? (
                    <div className="mt-3 flex items-start gap-1.5 rounded-[8px] border border-[var(--color-danger)]/25 bg-[var(--color-danger)]/8 px-2.5 py-2">
                      <AlertTriangle className="mt-0.5 size-3 shrink-0 text-[var(--color-danger)]" />
                      <div className="text-[var(--text-2xs)] leading-relaxed text-[var(--color-danger)]">
                        缺契约：{coverage.missing.slice(0, 3).join("、")}
                        {coverage.missing.length > 3 && ` 等 ${coverage.missing.length} 个`}
                      </div>
                    </div>
                  ) : (
                    <div className="mt-3 flex items-center gap-1.5 text-[var(--text-2xs)] text-[var(--color-success)]">
                      <ShieldCheck className="size-3" />
                      全部工具都有能力契约
                    </div>
                  )}
                  <Button
                    variant="ghost"
                    size="sm"
                    className="mt-2 w-full justify-between px-1 text-[var(--color-muted-foreground)]"
                    onClick={() => navigate("/tools")}
                  >
                    查看全部能力
                    <ArrowUpRight className="size-3.5" />
                  </Button>
                </>
              )}
            </div>
          </Card>
        </div>

        {/* 最近执行 */}
        <div className="mt-6">
          <SectionHeader
            title="最近执行"
            description="来自工具回执账本——这里记录的是实际发生了什么，不是模型说做了什么"
            action={
              <Button variant="ghost" size="xs" onClick={() => navigate("/logs")} className="gap-1">
                全部记录
                <ArrowUpRight className="size-3" />
              </Button>
            }
          />
          <Card>
            {receipts === null ? (
              <div className="flex flex-col gap-2 p-4">
                {[0, 1, 2].map((i) => (
                  <div key={i} className="h-7 skeleton-shimmer rounded-[6px]" />
                ))}
              </div>
            ) : receipts.length === 0 ? (
              <EmptyState
                compact
                icon={Zap}
                title="还没有执行记录"
                description="在对话里让它做点事——读写文件、查资料、生成图像，每一次工具调用都会在这里留下回执"
                action={
                  <Button size="sm" variant="secondary" onClick={() => navigate("/chat")}>
                    去对话
                  </Button>
                }
              />
            ) : (
              <div className="divide-y divide-[var(--color-line-faint)]">
                {receipts.map((r) => (
                  <div
                    key={r.receipt_id}
                    className="flex items-center gap-3 px-4 py-2.5 transition-colors hover:bg-[var(--color-hover)]/40"
                  >
                    <StatusDot tone={r.success ? "success" : "danger"} />
                    <span className="min-w-0 flex-1 truncate font-mono text-[var(--text-xs)] text-[var(--color-foreground)]">
                      {r.tool_name}
                    </span>
                    <Badge tone={riskTone(r.risk_level)}>{r.risk_level}</Badge>
                    <span className="hidden w-32 truncate text-[var(--text-2xs)] text-[var(--color-muted-foreground)] sm:block">
                      {r.side_effect}
                    </span>
                    <span className="num-tabular w-14 text-right text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                      {r.duration_ms} ms
                    </span>
                    <span className="hidden w-16 text-right text-[var(--text-2xs)] text-[var(--color-subtle-foreground)] md:block">
                      {r.created_at?.slice(11, 19) || ""}
                    </span>
                  </div>
                ))}
              </div>
            )}
          </Card>
        </div>

        <p className="mt-6 flex items-center justify-center gap-1.5 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
          {metrics?.network_ok ? <Wifi className="size-3" /> : <WifiOff className="size-3" />}
          所有数据来自本机，不上云
        </p>
      </PageBody>
    </Page>
  );
}
