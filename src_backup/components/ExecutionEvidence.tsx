import { useState } from "react";
import { AlertTriangle, CheckCircle2, ChevronDown, ChevronRight, Clock, Eye, XCircle } from "lucide-react";
import { cn } from "../lib/cn";
import {
  attentionText,
  receiptLabel,
  receiptTone,
  sideEffectLabel,
  strategyLabel,
  verdictPresentation,
  type EvidenceLine,
  type EvidenceTone,
  type ClaimAssessment,
  type TaskTruthSnapshot,
} from "../lib/taskTruth";

const TONE_ICON: Record<EvidenceTone, typeof CheckCircle2> = {
  ok: CheckCircle2,
  warn: Clock,
  bad: XCircle,
  idle: Eye,
};

/** 判据色 → token 色。判据本身就是"能不能信"的结论，不能靠文字颜色随手给。 */
const TONE_TEXT: Record<EvidenceTone, string> = {
  ok: "text-[var(--color-success)]",
  warn: "text-[var(--color-warning)]",
  bad: "text-[var(--color-danger)]",
  idle: "text-[var(--color-muted-foreground)]",
};

const TONE_EDGE: Record<EvidenceTone, string> = {
  ok: "border-l-[var(--color-success)]",
  warn: "border-l-[var(--color-warning)]",
  bad: "border-l-[var(--color-danger)]",
  idle: "border-l-[var(--color-line-strong)]",
};

/**
 * 执行证据面板 —— 把回执账本里的判据摊开给用户看。
 *
 * 它存在的唯一理由：界面上"执行成功"这四个字以前只代表工具自己说了
 * 一声成功。现在这句话必须由判据支撑：有证据写"已验证"，没证据写
 * "未能确认"，被核对推翻写"与事实不符"。
 */
export default function ExecutionEvidence({
  snapshot,
  claim,
}: {
  snapshot: TaskTruthSnapshot;
  claim?: ClaimAssessment;
}) {
  const [expanded, setExpanded] = useState(false);
  const presentation = verdictPresentation(snapshot);
  const counts = snapshot.counts;
  const showLines = snapshot.lines.length > 0;
  const VerdictIcon = TONE_ICON[presentation.tone];

  const chips: Array<{ label: string; value: number; tone: EvidenceTone }> = [
    { label: "已验证", value: counts.verified, tone: "ok" },
    { label: "未确认", value: counts.unverified, tone: "warn" },
    { label: "失败", value: counts.failed, tone: "bad" },
    { label: "与事实不符", value: counts.contradicted, tone: "bad" },
  ];

  return (
    <section
      className={cn(
        "mt-2 overflow-hidden rounded-[var(--radius-md)] border border-[var(--color-line)] border-l-2 bg-[var(--color-card)]",
        TONE_EDGE[presentation.tone]
      )}
    >
      <header className="flex flex-wrap items-center gap-x-2 gap-y-1 px-2.5 py-2">
        <VerdictIcon className={cn("size-3.5 shrink-0", TONE_TEXT[presentation.tone])} />
        <span className="text-[var(--text-2xs)] font-semibold text-[var(--color-foreground)]">执行证据</span>
        <span className={cn("text-[var(--text-2xs)] font-medium", TONE_TEXT[presentation.tone])}>
          {presentation.label}
        </span>
        <span className="min-w-0 flex-1 truncate text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
          {presentation.detail}
        </span>
        {showLines && (
          <button
            type="button"
            onClick={() => setExpanded((prev) => !prev)}
            aria-expanded={expanded}
            className="inline-flex shrink-0 cursor-pointer items-center gap-1 rounded-[5px] px-1.5 py-0.5 text-[var(--text-2xs)] text-[var(--color-muted-foreground)] transition-colors hover:bg-[var(--color-hover)] hover:text-[var(--color-foreground)]"
          >
            {expanded ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
            {snapshot.lines.length} 条回执
          </button>
        )}
      </header>

      <div className="grid grid-cols-4 gap-1.5 px-2.5 pb-2">
        {chips.map((chip) => {
          const active = chip.value > 0;
          return (
            <div
              key={chip.label}
              className={cn(
                "rounded-[6px] border px-2 py-1.5",
                active
                  ? "border-[var(--color-line)] bg-[var(--color-elevated)]"
                  : "border-[var(--color-line-faint)] bg-transparent"
              )}
            >
              <div
                className={cn(
                  "num-tabular text-[var(--text-base)] font-semibold leading-none",
                  active ? TONE_TEXT[chip.tone] : "text-[var(--color-subtle-foreground)]"
                )}
              >
                {chip.value}
              </div>
              <div className="mt-1 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                {chip.label}
              </div>
            </div>
          );
        })}
      </div>

      {snapshot.attention.length > 0 && (
        <ul className="space-y-1 border-t border-[var(--color-line-faint)] px-2.5 py-1.5">
          {snapshot.attention.map((reason) => (
            <li
              key={reason}
              className="flex items-start gap-1.5 text-[var(--text-2xs)] leading-relaxed text-[var(--color-warning)]"
            >
              <AlertTriangle className="mt-px size-3 shrink-0" />
              <span className="min-w-0">{attentionText(reason)}</span>
            </li>
          ))}
        </ul>
      )}

      {claim?.correction && (
        <p className="flex items-start gap-1.5 border-t border-[var(--color-warning)]/20 bg-[var(--color-warning)]/8 px-2.5 py-1.5 text-[var(--text-2xs)] leading-relaxed text-[var(--color-warning)]">
          <AlertTriangle className="mt-px size-3 shrink-0" />
          <span className="min-w-0">{claim.correction}</span>
        </p>
      )}

      {expanded && showLines && (
        <ul className="border-t border-[var(--color-line-faint)]">
          {snapshot.lines.map((line) => (
            <EvidenceRow key={line.receipt_id} line={line} />
          ))}
        </ul>
      )}
    </section>
  );
}

/**
 * 从 "2026-10-19 12:03:44" 里取出 "12:03:44"。
 *
 * 只显示时分秒：同一次任务不会跨天，把日期重复印在每一行反而挤掉了
 * 真正要看的判据。取不到（旧回执没有时间）就返回空串，不画时间列。
 */
function timeOf(at?: string): string {
  if (!at) return "";
  const match = at.match(/(\d{2}:\d{2}:\d{2})/);
  return match ? match[1] : "";
}

function EvidenceRow({ line }: { line: EvidenceLine }) {
  const tone = receiptTone(line.verification_status);
  const Icon = TONE_ICON[tone];
  const time = timeOf(line.at);
  return (
    <li className="flex items-start gap-2 border-b border-[var(--color-line-faint)] px-2.5 py-1.5 last:border-0">
      {time && (
        <span className="num-tabular mt-px shrink-0 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
          {time}
        </span>
      )}
      <Icon className={cn("mt-px size-3 shrink-0", TONE_TEXT[tone])} />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
          <span className="truncate font-mono text-[var(--text-2xs)] text-[var(--color-foreground)]">
            {line.tool_name}
          </span>
          <span className={cn("text-[var(--text-2xs)] font-medium", TONE_TEXT[tone])}>
            {receiptLabel(line.verification_status)}
          </span>
          <span className="text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
            {sideEffectLabel(line.side_effect)} · {strategyLabel(line.strategy)} · {line.duration_ms}ms
          </span>
        </div>
        <p className="mt-0.5 break-words text-[var(--text-2xs)] leading-relaxed text-[var(--color-muted-foreground)]">
          {line.summary}
        </p>
        {line.missing_fields.length > 0 && (
          <p className="mt-0.5 text-[var(--text-2xs)] text-[var(--color-danger)]">
            缺少字段：{line.missing_fields.join("、")}
          </p>
        )}
      </div>
    </li>
  );
}
