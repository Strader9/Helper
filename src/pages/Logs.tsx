import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ChevronRight,
  ClipboardCheck,
  RefreshCw,
  ScrollText,
  Search,
  ShieldQuestion,
  X,
} from "lucide-react";
import { Page, PageBody, PageHeader, Toolbar, KeyValue, Divider } from "../components/ui/page";
import { Card } from "../components/ui/card";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Input, Switch } from "../components/ui/form";
import { Segmented } from "../components/ui/primitives";
import { BigNumber, EmptyState, LoadingBlock } from "../components/ui/feedback";
import { cn } from "../lib/cn";

/**
 * 审计与回执。
 *
 * 这个界面把库里两本账并列展示，它们回答的是不同问题：
 *   - 审计账（audit_logs）：这次调用被允许了吗、谁拒的、耗时多少。
 *   - 回执账（tool_receipts）：它实际改变了什么、有没有证据能证明。
 *
 * 刻意不做"合并成一条"的视图。合并之后"允许了但没验证成功"会被压成
 * 一行绿字，那正是最该被看见的情况。
 */

const LIMIT = 200;

type Tone = "neutral" | "brand" | "info" | "success" | "warning" | "danger" | "outline";
type Risk = "all" | "SAFE" | "LOW" | "MEDIUM" | "HIGH";
type Mode = "audit" | "receipts";

interface AuditEntry {
  id: number;
  timestamp: string;
  session_id: string;
  tool_name: string;
  tool_risk_level: string;
  permission_result: string;
  success: boolean;
  error_code?: string | null;
  error_message?: string | null;
  duration_ms?: number | null;
}

interface Receipt {
  receipt_id: string;
  session_id: string;
  run_id?: string | null;
  tool_name: string;
  capability_id: string;
  side_effect: string;
  scope: string;
  reversible: boolean;
  risk_level: string;
  verification_strategy: string;
  verification_status: string;
  verification_evidence?: string | null;
  missing_fields: string;
  idempotency_key?: string | null;
  request_digest: string;
  result_digest?: string | null;
  permission_result: string;
  success: boolean;
  error_code?: string | null;
  duration_ms: number;
  created_at: string;
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

const VERIFY_STRATEGY_LABEL: Record<string, string> = {
  none: "不验证",
  terminal_receipt: "终态回执",
  state_diff: "状态比对",
  artifact: "产物核对",
  provider_ack: "服务商确认",
};

/** 验证结论：判定优先级就是这里列出的顺序，不是"成功即完成" */
const VERIFICATION: Record<string, { label: string; tone: Tone; note: string }> = {
  VERIFIED: {
    label: "已验证",
    tone: "success",
    note: "独立探查确认事实成立，或返回字段齐全且可核对。这次调用算完成。",
  },
  CONTRADICTED: {
    label: "事实冲突",
    tone: "danger",
    note: "探查说事实不成立——字段再齐全也不算完成，需要重新执行或人工介入。",
  },
  FAILED: {
    label: "执行失败",
    tone: "danger",
    note: "工具本身报错，这次调用没有产生可用的结果。",
  },
  UNVERIFIED: {
    label: "待验证",
    tone: "warning",
    note: "返回里缺了必填字段，无法证明改动真的发生了。模型说做完了不算数。",
  },
  NOT_REQUIRED: {
    label: "无需验证",
    tone: "neutral",
    note: "这类工具没有实质副作用，不靠返回字段判断成败。",
  },
};

const riskTone = (r: string): Tone =>
  r === "HIGH" ? "danger" : r === "MEDIUM" ? "warning" : r === "LOW" ? "success" : "neutral";

function permissionMeta(raw: string): { label: string; tone: Tone } {
  if (!raw) return { label: "未知", tone: "neutral" };
  if (raw.startsWith("DENIED")) {
    const code = raw.split(":").slice(1).join(":").trim();
    return { label: code ? `拒绝 · ${code}` : "拒绝", tone: "danger" };
  }
  if (raw === "ALLOWED_ONCE") return { label: "允许一次", tone: "info" };
  if (raw === "ALLOWED_ALWAYS") return { label: "永久允许", tone: "success" };
  if (raw === "ALLOWED") return { label: "允许", tone: "success" };
  return { label: raw, tone: "neutral" };
}

/** SQLite 存的是 UTC `YYYY-MM-DD HH:MM:SS`，没有时区后缀时按 UTC 解释 */
function toDate(raw?: string | null): Date | null {
  if (!raw) return null;
  const s = raw.trim();
  if (!s) return null;
  const iso = s.includes("T") ? s : s.replace(" ", "T");
  const zoned = /(?:Z|[+-]\d{2}:?\d{2})$/i.test(iso);
  const d = new Date(zoned ? iso : `${iso}Z`);
  return Number.isNaN(d.getTime()) ? null : d;
}

function fmtStamp(raw?: string | null): string {
  const d = toDate(raw);
  if (!d) return raw?.trim() || "—";
  const hhmm = d.toLocaleTimeString("zh-CN", { hour12: false });
  if (d.toDateString() === new Date().toDateString()) return hhmm;
  return `${d.getMonth() + 1}/${d.getDate()} ${hhmm.slice(0, 5)}`;
}

function fmtFull(raw?: string | null): string {
  const d = toDate(raw);
  return d ? d.toLocaleString("zh-CN", { hour12: false }) : raw?.trim() || "—";
}

function fmtDuration(ms?: number | null): string {
  if (ms === undefined || ms === null) return "—";
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
}

function parseMissing(raw: string): string[] {
  if (!raw) return [];
  try {
    const v = JSON.parse(raw);
    return Array.isArray(v) ? v.map(String) : [];
  } catch {
    return raw
      .split(",")
      .map((s) => s.trim().replace(/^\[?"?|"?\]?$/g, ""))
      .filter(Boolean);
  }
}

type Detail = { kind: "audit"; row: AuditEntry } | { kind: "receipt"; row: Receipt };

/** 两本账把风险等级存在不同字段名里，展示前先归一 */
const riskOf = (d: Detail) => (d.kind === "audit" ? d.row.tool_risk_level : d.row.risk_level);

export default function Logs() {
  const [mode, setMode] = useState<Mode>("audit");
  const [audit, setAudit] = useState<AuditEntry[] | null>(null);
  const [receipts, setReceipts] = useState<Receipt[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [risk, setRisk] = useState<Risk>("all");
  const [onlyFailed, setOnlyFailed] = useState(false);
  const [detail, setDetail] = useState<Detail | null>(null);

  const load = useCallback(async (m: Mode) => {
    setLoading(true);
    setError(null);
    try {
      if (m === "audit") {
        setAudit(await invoke<AuditEntry[]>("get_audit_logs", { limit: LIMIT }));
      } else {
        setReceipts(await invoke<Receipt[]>("get_tool_receipts", { limit: LIMIT }));
      }
    } catch (e) {
      setError(typeof e === "string" ? e : "读取账本失败");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (mode === "audit" && audit === null) void load("audit");
    if (mode === "receipts" && receipts === null) void load("receipts");
  }, [mode, audit, receipts, load]);

  const auditRows = useMemo(() => {
    if (!audit) return [];
    const q = query.trim().toLowerCase();
    return audit.filter((r) => {
      if (risk !== "all" && r.tool_risk_level !== risk) return false;
      if (onlyFailed && r.success) return false;
      if (!q) return true;
      return (
        r.tool_name.toLowerCase().includes(q) ||
        r.session_id.toLowerCase().includes(q) ||
        (r.error_code ?? "").toLowerCase().includes(q) ||
        (r.error_message ?? "").toLowerCase().includes(q)
      );
    });
  }, [audit, query, risk, onlyFailed]);

  const receiptRows = useMemo(() => {
    if (!receipts) return [];
    const q = query.trim().toLowerCase();
    return receipts.filter((r) => {
      if (risk !== "all" && r.risk_level !== risk) return false;
      if (onlyFailed && r.success && r.verification_status !== "CONTRADICTED") return false;
      if (!q) return true;
      return (
        r.tool_name.toLowerCase().includes(q) ||
        r.scope.toLowerCase().includes(q) ||
        r.session_id.toLowerCase().includes(q) ||
        r.receipt_id.toLowerCase().includes(q)
      );
    });
  }, [receipts, query, risk, onlyFailed]);

  /** 统计只描述"当前加载的这一页账"，不做全量推断 */
  const stats = useMemo(() => {
    if (mode === "audit") {
      const rows = audit ?? [];
      return [
        { value: rows.length, label: "审计记录", tone: "brand" as const },
        { value: rows.filter((r) => !r.success).length, label: "执行失败", tone: "danger" as const },
        {
          value: rows.filter((r) => r.permission_result.startsWith("DENIED")).length,
          label: "被闸门拒绝",
          tone: "danger" as const,
        },
        {
          value: rows.filter((r) => r.tool_risk_level === "MEDIUM" || r.tool_risk_level === "HIGH").length,
          label: "中高风险调用",
          tone: "default" as const,
        },
      ];
    }
    const rows = receipts ?? [];
    return [
      { value: rows.length, label: "工具回执", tone: "brand" as const },
      {
        value: rows.filter((r) => r.verification_status === "VERIFIED").length,
        label: "已验证完成",
        tone: "success" as const,
      },
      {
        value: rows.filter((r) => r.verification_status === "UNVERIFIED").length,
        label: "待验证",
        tone: "danger" as const,
      },
      {
        value: rows.filter((r) => r.verification_status === "CONTRADICTED").length,
        label: "事实冲突",
        tone: "danger" as const,
      },
    ];
  }, [mode, audit, receipts]);

  const rows = mode === "audit" ? auditRows : receiptRows;
  const total = mode === "audit" ? audit?.length ?? 0 : receipts?.length ?? 0;
  const dataReady = mode === "audit" ? audit !== null : receipts !== null;

  return (
    <Page wide>
      <PageHeader
        title="审计与回执"
        icon={ScrollText}
        badge={<Badge tone="outline">最近 {LIMIT} 条</Badge>}
        description="两本账。审计账记录闸门做了什么决定，回执账记录工具实际改了什么、有没有证据。数据来自本地数据库，不做任何加工。"
        actions={
          <Button
            variant="secondary"
            size="sm"
            onClick={() => void load(mode)}
            disabled={loading}
          >
            <RefreshCw className={cn(loading && "animate-ds-spin")} />
            刷新
          </Button>
        }
      />

      <PageBody>
        {error && (
          <Card className="mb-4 border-[var(--color-danger)]/30 p-4 text-[var(--text-xs)] text-[var(--color-danger)]">
            {error}
          </Card>
        )}

        <div className="mb-4 grid grid-cols-2 gap-3 lg:grid-cols-4">
          {stats.map((s) => (
            <Card key={s.label} className="p-3.5">
              <BigNumber value={s.value} label={s.label} tone={s.tone} />
            </Card>
          ))}
        </div>

        <Toolbar>
          <Segmented
            value={mode}
            onChange={(v) => {
              setMode(v);
              setDetail(null);
            }}
            options={[
              { value: "audit" as Mode, label: "审计账" },
              { value: "receipts" as Mode, label: "回执账" },
            ]}
          />
          <div className="relative w-64 max-w-full">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-[var(--color-subtle-foreground)]" />
            <Input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={mode === "audit" ? "搜工具名、会话、错误码…" : "搜工具名、落点、回执号…"}
              className="pl-8"
            />
          </div>
          <Segmented
            value={risk}
            onChange={setRisk}
            options={[
              { value: "all" as Risk, label: "全部" },
              { value: "SAFE" as Risk, label: "SAFE" },
              { value: "LOW" as Risk, label: "LOW" },
              { value: "MEDIUM" as Risk, label: "MEDIUM" },
              { value: "HIGH" as Risk, label: "HIGH" },
            ]}
          />
          <label className="ml-1 flex cursor-pointer items-center gap-1.5 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
            <Switch checked={onlyFailed} onCheckedChange={setOnlyFailed} />
            只看异常
          </label>
          <span className="ml-auto text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
            {rows.length} / {total}
          </span>
        </Toolbar>

        {!dataReady ? (
          <LoadingBlock label="读取账本" />
        ) : rows.length === 0 ? (
          <Card>
            <EmptyState
              icon={mode === "audit" ? ShieldQuestion : ClipboardCheck}
              title={total === 0 ? "账本还是空的" : "没有匹配的记录"}
              description={
                total === 0
                  ? "每一次工具调用都会在这里留痕：审计一行记决定，回执一行记结果。现在还没有任何调用。"
                  : "把筛选放宽，或清空搜索词再看。"
              }
              action={
                total > 0 ? (
                  <Button
                    size="sm"
                    variant="secondary"
                    onClick={() => {
                      setQuery("");
                      setRisk("all");
                      setOnlyFailed(false);
                    }}
                  >
                    清空条件
                  </Button>
                ) : undefined
              }
            />
          </Card>
        ) : mode === "audit" ? (
          <Card className="overflow-hidden">
            <div className="grid grid-cols-[104px_minmax(0,1fr)_72px_124px_76px_64px_20px] items-center gap-3 border-b border-[var(--color-line)] bg-[var(--color-surface)]/60 px-3.5 py-2">
              <span className="label-caps">时间</span>
              <span className="label-caps">工具</span>
              <span className="label-caps">风险</span>
              <span className="label-caps">闸门决定</span>
              <span className="label-caps">结果</span>
              <span className="label-caps text-right">耗时</span>
              <span />
            </div>
            <div className="max-h-[calc(100vh-400px)] overflow-y-auto">
              {(auditRows as AuditEntry[]).map((r) => {
                const perm = permissionMeta(r.permission_result);
                return (
                  <button
                    key={r.id}
                    onClick={() => setDetail({ kind: "audit", row: r })}
                    className={cn(
                      "grid w-full cursor-pointer grid-cols-[104px_minmax(0,1fr)_72px_124px_76px_64px_20px] items-center gap-3",
                      "border-b border-[var(--color-line-faint)] px-3.5 py-2 text-left last:border-0",
                      "transition-colors hover:bg-[var(--color-hover)]/50"
                    )}
                  >
                    <span className="num-tabular truncate text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                      {fmtStamp(r.timestamp)}
                    </span>
                    <span className="truncate font-mono text-[var(--text-xs)] text-[var(--color-foreground)]">
                      {r.tool_name}
                    </span>
                    <span>
                      <Badge tone={riskTone(r.tool_risk_level)}>{r.tool_risk_level}</Badge>
                    </span>
                    <span>
                      <Badge tone={perm.tone} className="max-w-full truncate">
                        {perm.label}
                      </Badge>
                    </span>
                    <span>
                      {r.success ? (
                        <Badge tone="success" dot>
                          成功
                        </Badge>
                      ) : (
                        <Badge tone="danger" dot>
                          失败
                        </Badge>
                      )}
                    </span>
                    <span className="num-tabular text-right text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                      {fmtDuration(r.duration_ms)}
                    </span>
                    <ChevronRight className="size-3.5 text-[var(--color-subtle-foreground)]" />
                  </button>
                );
              })}
            </div>
          </Card>
        ) : (
          <Card className="overflow-hidden">
            <div className="grid grid-cols-[104px_minmax(0,1fr)_96px_84px_64px_20px] items-center gap-3 border-b border-[var(--color-line)] bg-[var(--color-surface)]/60 px-3.5 py-2">
              <span className="label-caps">时间</span>
              <span className="label-caps">工具</span>
              <span className="label-caps">验证结论</span>
              <span className="label-caps">落点</span>
              <span className="label-caps text-right">耗时</span>
              <span />
            </div>
            <div className="max-h-[calc(100vh-400px)] overflow-y-auto">
              {(receiptRows as Receipt[]).map((r) => {
                const v = VERIFICATION[r.verification_status] ?? {
                  label: r.verification_status,
                  tone: "neutral" as Tone,
                };
                return (
                  <button
                    key={r.receipt_id}
                    onClick={() => setDetail({ kind: "receipt", row: r })}
                    className={cn(
                      "grid w-full cursor-pointer grid-cols-[104px_minmax(0,1fr)_96px_84px_64px_20px] items-center gap-3",
                      "border-b border-[var(--color-line-faint)] px-3.5 py-2 text-left last:border-0",
                      "transition-colors hover:bg-[var(--color-hover)]/50"
                    )}
                  >
                    <span className="num-tabular truncate text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                      {fmtStamp(r.created_at)}
                    </span>
                    <span className="truncate font-mono text-[var(--text-xs)] text-[var(--color-foreground)]">
                      {r.tool_name}
                    </span>
                    <span>
                      <Badge tone={v.tone} dot>
                        {v.label}
                      </Badge>
                    </span>
                    <span className="truncate text-[var(--text-2xs)] text-[var(--color-muted-foreground)]" title={r.scope}>
                      {r.scope}
                    </span>
                    <span className="num-tabular text-right text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                      {fmtDuration(r.duration_ms)}
                    </span>
                    <ChevronRight className="size-3.5 text-[var(--color-subtle-foreground)]" />
                  </button>
                );
              })}
            </div>
          </Card>
        )}

        <p className="mt-3 text-[var(--text-2xs)] leading-relaxed text-[var(--color-subtle-foreground)]">
          {mode === "audit"
            ? "审计账只回答「允许了吗」。工具实际改了什么、有没有证据，在回执账里。"
            : "回执账只有 VERIFIED 才算完成。字段缺失是待验证，探查说不成立是事实冲突——这两种都会被 Chat 里的回复判为不可信。"}
        </p>
      </PageBody>

      {detail && (
        <>
          <div className="fixed inset-0 z-40 bg-black/40" onClick={() => setDetail(null)} />
          <aside className="fixed right-0 top-0 z-50 flex h-full w-[440px] max-w-[92vw] flex-col border-l border-[var(--color-line-strong)] bg-[var(--color-elevated)] shadow-[var(--shadow-xl)] animate-[ds-slide-right_.22s_cubic-bezier(.16,1,.3,1)]">
            <div className="flex items-start justify-between gap-3 border-b border-[var(--color-line)] px-4 py-3.5">
              <div className="min-w-0">
                <div className="truncate font-mono text-[var(--text-sm)] font-medium text-[var(--color-foreground)]">
                  {detail.row.tool_name}
                </div>
                <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
                  <Badge tone={riskTone(riskOf(detail))}>{riskOf(detail)} RISK</Badge>
                  {detail.kind === "audit" ? (
                    detail.row.success ? (
                      <Badge tone="success" dot>
                        成功
                      </Badge>
                    ) : (
                      <Badge tone="danger" dot>
                        失败
                      </Badge>
                    )
                  ) : (
                    <Badge
                      tone={(VERIFICATION[detail.row.verification_status]?.tone ?? "neutral") as Tone}
                      dot
                    >
                      {VERIFICATION[detail.row.verification_status]?.label ?? detail.row.verification_status}
                    </Badge>
                  )}
                </div>
              </div>
              <Button variant="ghost" size="icon-sm" onClick={() => setDetail(null)}>
                <X className="size-4" />
              </Button>
            </div>

            <div className="flex-1 overflow-y-auto px-4 py-3">
              {detail.kind === "audit" ? (
                <AuditDetail row={detail.row} onFindReceipt={(tool) => {
                  setMode("receipts");
                  setQuery(tool);
                  setOnlyFailed(false);
                  setRisk("all");
                  setDetail(null);
                }} />
              ) : (
                <ReceiptDetail row={detail.row} />
              )}
            </div>
          </aside>
        </>
      )}
    </Page>
  );
}

/* ------------------------------------------------------------------ 详情 */

function AuditDetail({ row, onFindReceipt }: { row: AuditEntry; onFindReceipt: (tool: string) => void }) {
  const perm = permissionMeta(row.permission_result);
  return (
    <>
      <KeyValue label="闸门决定">
        <Badge tone={perm.tone}>{perm.label}</Badge>
      </KeyValue>
      <KeyValue label="发生时间">{fmtFull(row.timestamp)}</KeyValue>
      <KeyValue label="风险等级">{row.tool_risk_level}</KeyValue>
      <KeyValue label="耗时">{fmtDuration(row.duration_ms)}</KeyValue>
      <KeyValue label="会话" mono>
        {row.session_id}
      </KeyValue>
      <KeyValue label="记录 ID" mono>
        #{row.id}
      </KeyValue>

      {(row.error_code || row.error_message) && (
        <>
          <Divider className="my-3" />
          <div className="label-caps mb-1.5">错误</div>
          <div className="rounded-[var(--radius-sm)] border border-[var(--color-danger)]/25 bg-[var(--color-danger)]/8 p-2.5">
            {row.error_code && (
              <div className="font-mono text-[var(--text-2xs)] text-[var(--color-danger)]">
                {row.error_code}
              </div>
            )}
            {row.error_message && (
              <p className="mt-1 whitespace-pre-wrap text-[var(--text-xs)] leading-relaxed text-[var(--color-foreground)]">
                {row.error_message}
              </p>
            )}
          </div>
        </>
      )}

      <Divider className="my-3" />

      <p className="text-[var(--text-2xs)] leading-relaxed text-[var(--color-subtle-foreground)]">
        审计这条只证明「闸门做了这个决定」，不证明工具真的做成了。要确认结果，去看回执。
      </p>

      <Button variant="secondary" size="sm" className="mt-3 w-full" onClick={() => onFindReceipt(row.tool_name)}>
        <ClipboardCheck className="size-3.5" />
        在回执账里查这个工具
      </Button>
    </>
  );
}

function ReceiptDetail({ row }: { row: Receipt }) {
  const v = VERIFICATION[row.verification_status];
  const missing = parseMissing(row.missing_fields);
  const perm = permissionMeta(row.permission_result);

  return (
    <>
      <div className="rounded-[var(--radius-sm)] border border-[var(--color-line)] bg-[var(--color-surface)] p-2.5">
        <p className="text-[var(--text-xs)] leading-relaxed text-[var(--color-foreground)]">
          {v?.note ?? "无法识别的验证结论。"}
        </p>
      </div>

      <div className="mt-3 label-caps mb-1.5">副作用</div>
      <KeyValue label="类型">{SIDE_EFFECT_LABEL[row.side_effect] ?? row.side_effect}</KeyValue>
      <KeyValue label="落点" mono>
        {row.scope}
      </KeyValue>
      <KeyValue label="可撤销">
        {row.reversible ? (
          <span className="text-[var(--color-success)]">是</span>
        ) : (
          <span className="text-[var(--color-warning)]">否（不可回滚）</span>
        )}
      </KeyValue>
      <KeyValue label="验证策略">{VERIFY_STRATEGY_LABEL[row.verification_strategy] ?? row.verification_strategy}</KeyValue>

      {row.verification_evidence && (
        <>
          <div className="mt-3 label-caps mb-1.5">探查证据</div>
          <pre className="max-h-40 overflow-auto whitespace-pre-wrap rounded-[var(--radius-sm)] border border-[var(--color-line)] bg-[var(--color-surface)] p-2.5 font-mono text-[var(--text-2xs)] leading-relaxed text-[var(--color-muted-foreground)]">
            {row.verification_evidence}
          </pre>
        </>
      )}

      <Divider className="my-3" />

      <div className="label-caps mb-1.5">缺失字段</div>
      {missing.length === 0 ? (
        <p className="text-[var(--text-xs)] text-[var(--color-muted-foreground)]">
          无。返回里的必填字段齐全。
        </p>
      ) : (
        <div className="flex flex-wrap gap-1.5">
          {missing.map((f) => (
            <Badge key={f} tone="danger" className="font-mono">
              {f}
            </Badge>
          ))}
        </div>
      )}

      <Divider className="my-3" />

      <KeyValue label="闸门决定">
        <Badge tone={perm.tone}>{perm.label}</Badge>
      </KeyValue>
      <KeyValue label="耗时">{fmtDuration(row.duration_ms)}</KeyValue>
      <KeyValue label="落库时间">{fmtFull(row.created_at)}</KeyValue>
      <KeyValue label="回执号" mono>
        {row.receipt_id}
      </KeyValue>
      <KeyValue label="请求摘要" mono>
        {row.request_digest}
      </KeyValue>
      {row.result_digest && (
        <KeyValue label="结果摘要" mono>
          {row.result_digest}
        </KeyValue>
      )}
      {row.idempotency_key && (
        <KeyValue label="幂等键" mono>
          {row.idempotency_key}
        </KeyValue>
      )}
      <KeyValue label="会话" mono>
        {row.session_id}
      </KeyValue>
      {row.run_id && (
        <KeyValue label="本轮 run" mono>
          {row.run_id}
        </KeyValue>
      )}
      {row.error_code && (
        <KeyValue label="错误码">
          <span className="text-[var(--color-danger)]">{row.error_code}</span>
        </KeyValue>
      )}
    </>
  );
}
