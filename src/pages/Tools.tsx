import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Boxes,
  ChevronRight,
  FileEdit,
  Globe,
  Search,
  ShieldAlert,
  ShieldCheck,
  Terminal,
  X,
} from "lucide-react";
import { Page, PageBody, PageHeader, Toolbar, KeyValue, Divider } from "../components/ui/page";
import { Card } from "../components/ui/card";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/form";
import { Segmented } from "../components/ui/primitives";
import { BigNumber, EmptyState, LoadingBlock } from "../components/ui/feedback";
import { cn } from "../lib/cn";

interface ToolContract {
  tool: string;
  side_effect: string;
  scope: string;
  reversible: boolean;
  verification: string;
  required_fields: string[];
  risk_level: string;
  registered: boolean;
}

const SIDE_EFFECT_LABEL: Record<string, string> = {
  none: "无副作用",
  local_read: "读取本地",
  local_write: "写入本地",
  local_state_change: "修改本地状态",
  desktop_control: "控制桌面",
  network_read: "网络读取",
  external_state_change: "改动外部状态",
  external_communication: "对外通信",
  process_execution: "执行进程",
  installation: "安装软件",
};

const VERIFY_LABEL: Record<string, string> = {
  none: "不验证",
  terminal_receipt: "终态回执",
  state_diff: "状态比对",
  artifact: "产物核对",
  provider_ack: "服务商确认",
};

const riskTone = (r: string) =>
  r === "HIGH" ? "danger" : r === "MEDIUM" ? "warning" : r === "LOW" ? "success" : "neutral";

/** 副作用分组：决定筛选与图标 */
function group(side: string): "write" | "network" | "read" | "none" {
  if (side === "network_read") return "network";
  if (side === "none") return "none";
  if (side === "local_read" || side === "local_state_change") return "read";
  return "write";
}

export default function Tools() {
  const [rows, setRows] = useState<ToolContract[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<"all" | "write" | "network" | "read" | "none">("all");
  const [selected, setSelected] = useState<ToolContract | null>(null);

  useEffect(() => {
    invoke<ToolContract[]>("list_tool_contracts")
      .then(setRows)
      .catch((e) => setError(typeof e === "string" ? e : "读取契约失败"));
  }, []);

  const filtered = useMemo(() => {
    if (!rows) return [];
    const q = query.trim().toLowerCase();
    return rows.filter((r) => {
      if (filter !== "all" && group(r.side_effect) !== filter) return false;
      if (!q) return true;
      return (
        r.tool.toLowerCase().includes(q) ||
        r.scope.toLowerCase().includes(q) ||
        r.side_effect.includes(q)
      );
    });
  }, [rows, query, filter]);

  const stats = useMemo(() => {
    if (!rows) return null;
    return {
      total: rows.length,
      registered: rows.filter((r) => r.registered).length,
      material: rows.filter((r) => group(r.side_effect) === "write").length,
      unverified: rows.filter((r) => group(r.side_effect) === "write" && r.verification === "none").length,
    };
  }, [rows]);

  return (
    <Page wide>
      <PageHeader
        title="能力中心"
        description="每个工具都必须声明副作用、落点与验证方式。这里是能力的完整清单，不是文档——数据直接来自运行时注册表。"
        icon={Boxes}
        badge={stats && <Badge tone="brand">{stats.total} 个能力</Badge>}
      />

      <PageBody>
        {error && (
          <Card className="mb-4 border-[var(--color-danger)]/30 p-4 text-[var(--text-xs)] text-[var(--color-danger)]">
            {error}
          </Card>
        )}

        {/* 统计 */}
        {stats && (
          <div className="mb-4 grid grid-cols-2 gap-3 lg:grid-cols-4">
            <Card className="p-3.5">
              <BigNumber value={stats.total} label="已声明契约" tone="brand" />
            </Card>
            <Card className="p-3.5">
              <BigNumber value={stats.registered} label="运行时已注册" />
            </Card>
            <Card className="p-3.5">
              <BigNumber value={stats.material} label="有实质副作用" />
            </Card>
            <Card className="p-3.5">
              <BigNumber
                value={stats.unverified}
                label="副作用未声明验证"
                tone={stats.unverified === 0 ? "success" : "danger"}
              />
            </Card>
          </div>
        )}

        <Toolbar>
          <div className="relative w-72 max-w-full">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-[var(--color-subtle-foreground)]" />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="搜索工具名、落点、副作用…"
              className="pl-8"
            />
          </div>
          <Segmented
            value={filter}
            onChange={setFilter}
            options={[
              { value: "all", label: "全部" },
              { value: "write", label: "写操作" },
              { value: "network", label: "网络" },
              { value: "read", label: "只读" },
            ]}
          />
          <span className="ml-auto text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
            {filtered.length} / {stats?.total ?? 0}
          </span>
        </Toolbar>

        {!rows ? (
          <LoadingBlock label="读取运行时注册表" />
        ) : filtered.length === 0 ? (
          <Card>
            <EmptyState
              icon={Search}
              title="没有匹配的能力"
              description="换个关键词，或把筛选切回「全部」"
              action={
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => {
                    setQuery("");
                    setFilter("all");
                  }}
                >
                  清空条件
                </Button>
              }
            />
          </Card>
        ) : (
          <Card className="overflow-hidden">
            {/* 表头 */}
            <div className="grid grid-cols-[minmax(0,1.6fr)_72px_minmax(0,1fr)_96px_28px] items-center gap-3 border-b border-[var(--color-line)] bg-[var(--color-surface)]/60 px-3.5 py-2">
              <span className="label-caps">工具</span>
              <span className="label-caps">风险</span>
              <span className="label-caps">副作用 / 落点</span>
              <span className="label-caps">验证</span>
              <span />
            </div>

            <div className="max-h-[calc(100vh-380px)] overflow-y-auto">
              {filtered.map((r) => (
                <button
                  key={r.tool}
                  onClick={() => setSelected(r)}
                  className={cn(
                    "grid w-full cursor-pointer grid-cols-[minmax(0,1.6fr)_72px_minmax(0,1fr)_96px_28px] items-center gap-3",
                    "border-b border-[var(--color-line-faint)] px-3.5 py-2 text-left last:border-0",
                    "transition-colors hover:bg-[var(--color-hover)]/50"
                  )}
                >
                  <span className="flex min-w-0 items-center gap-2">
                    {group(r.side_effect) === "write" ? (
                      <FileEdit className="size-3.5 shrink-0 text-[var(--color-warning)]" />
                    ) : group(r.side_effect) === "network" ? (
                      <Globe className="size-3.5 shrink-0 text-[var(--color-info)]" />
                    ) : group(r.side_effect) === "read" ? (
                      <ShieldCheck className="size-3.5 shrink-0 text-[var(--color-success)]" />
                    ) : (
                      <Terminal className="size-3.5 shrink-0 text-[var(--color-subtle-foreground)]" />
                    )}
                    <span className="truncate font-mono text-[var(--text-xs)] text-[var(--color-foreground)]">
                      {r.tool}
                    </span>
                    {!r.registered && <Badge tone="danger">未注册</Badge>}
                  </span>
                  <span>
                    <Badge tone={riskTone(r.risk_level)}>{r.risk_level}</Badge>
                  </span>
                  <span className="min-w-0">
                    <span className="block truncate text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                      {SIDE_EFFECT_LABEL[r.side_effect] ?? r.side_effect}
                    </span>
                    <span className="block truncate text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                      {r.scope}
                    </span>
                  </span>
                  <span className="text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                    {VERIFY_LABEL[r.verification] ?? r.verification}
                  </span>
                  <ChevronRight className="size-3.5 text-[var(--color-subtle-foreground)]" />
                </button>
              ))}
            </div>
          </Card>
        )}
      </PageBody>

      {/* 详情抽屉（右侧浮层） */}
      {selected && (
        <>
          <div className="fixed inset-0 z-40 bg-black/40" onClick={() => setSelected(null)} />
          <aside className="fixed right-0 top-0 z-50 flex h-full w-[420px] max-w-[92vw] flex-col border-l border-[var(--color-line-strong)] bg-[var(--color-elevated)] shadow-[var(--shadow-xl)] animate-[ds-slide-right_.22s_cubic-bezier(.16,1,.3,1)]">
            <div className="flex items-start justify-between gap-3 border-b border-[var(--color-line)] px-4 py-3.5">
              <div className="min-w-0">
                <div className="truncate font-mono text-[var(--text-sm)] font-medium text-[var(--color-foreground)]">
                  {selected.tool}
                </div>
                <div className="mt-1 flex items-center gap-1.5">
                  <Badge tone={riskTone(selected.risk_level)}>{selected.risk_level} RISK</Badge>
                  {selected.registered ? (
                    <Badge tone="success" dot>
                      已注册
                    </Badge>
                  ) : (
                    <Badge tone="danger" dot>
                      未注册
                    </Badge>
                  )}
                </div>
              </div>
              <Button variant="ghost" size="icon-sm" onClick={() => setSelected(null)}>
                <X className="size-4" />
              </Button>
            </div>

            <div className="flex-1 overflow-y-auto px-4 py-3">
              <div className="label-caps mb-1.5">副作用</div>
              <div className="mb-3 flex items-center gap-2">
                {group(selected.side_effect) === "write" ? (
                  <ShieldAlert className="size-4 text-[var(--color-warning)]" />
                ) : (
                  <ShieldCheck className="size-4 text-[var(--color-success)]" />
                )}
                <span className="text-[var(--text-sm)] text-[var(--color-foreground)]">
                  {SIDE_EFFECT_LABEL[selected.side_effect] ?? selected.side_effect}
                </span>
              </div>

              <KeyValue label="落点范围">{selected.scope}</KeyValue>
              <KeyValue label="可撤销">
                {selected.reversible ? (
                  <span className="text-[var(--color-success)]">是</span>
                ) : (
                  <span className="text-[var(--color-warning)]">否（不可回滚）</span>
                )}
              </KeyValue>
              <KeyValue label="验证策略">{VERIFY_LABEL[selected.verification] ?? selected.verification}</KeyValue>

              <Divider className="my-3" />

              <div className="label-caps mb-1.5">验证必填字段</div>
              {selected.required_fields.length === 0 ? (
                <p className="text-[var(--text-xs)] text-[var(--color-muted-foreground)]">
                  无。这类工具要么无副作用，要么不靠返回字段判断成败。
                </p>
              ) : (
                <div className="flex flex-wrap gap-1.5">
                  {selected.required_fields.map((f) => (
                    <Badge key={f} tone="outline" className="font-mono">
                      {f}
                    </Badge>
                  ))}
                </div>
              )}

              <Divider className="my-3" />

              <p className="text-[var(--text-2xs)] leading-relaxed text-[var(--color-subtle-foreground)]">
                验证必填字段的意思是：工具返回成功后，这些字段必须存在且可核对，
                否则这次调用不算完成，回执里会标成待验证。这是防止"HTTP 200 就宣布成功"的唯一手段。
              </p>
            </div>
          </aside>
        </>
      )}
    </Page>
  );
}
