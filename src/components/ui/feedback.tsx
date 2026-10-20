import * as React from "react";
import { Loader2, type LucideIcon } from "lucide-react";
import { cn } from "../../lib/cn";

/* ---------------------------------------------------------------- 进度条 */

export function Progress({
  value,
  max = 100,
  tone = "brand",
  className,
  height = 6,
}: {
  value: number;
  max?: number;
  tone?: "brand" | "success" | "warning" | "danger" | "info";
  className?: string;
  height?: number;
}) {
  const pct = Math.max(0, Math.min(100, (value / max) * 100));
  const color = {
    brand: "var(--color-brand)",
    success: "var(--color-success)",
    warning: "var(--color-warning)",
    danger: "var(--color-danger)",
    info: "var(--color-info)",
  }[tone];
  return (
    <div
      className={cn("w-full overflow-hidden rounded-full bg-[var(--color-active)]", className)}
      style={{ height }}
      role="progressbar"
      aria-valuenow={Math.round(pct)}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      <div
        className="h-full rounded-full transition-[width] duration-500 ease-[cubic-bezier(.16,1,.3,1)]"
        style={{
          width: `${pct}%`,
          background: `linear-gradient(90deg, ${color}bb, ${color})`,
          boxShadow: `0 0 12px -2px ${color}66`,
        }}
      />
    </div>
  );
}

/* ------------------------------------------------------------ 环形图 */

/**
 * 环形进度。用于 CPU/内存/GPU 这类"单一百分比"指标。
 * 数值低于 60 用中性色，60-85 用品牌色，85+ 转警告/危险——
 * 让颜色只在真正需要关注时说话。
 */
export function RingMetric({
  value,
  size = 108,
  stroke = 7,
  label,
  caption,
  tone,
}: {
  value: number;
  size?: number;
  stroke?: number;
  label: string;
  caption?: React.ReactNode;
  tone?: "brand" | "success" | "warning" | "danger" | "info";
}) {
  const clamped = Math.max(0, Math.min(100, value));
  const r = (size - stroke) / 2;
  const c = 2 * Math.PI * r;
  const auto =
    clamped >= 90 ? "danger" : clamped >= 78 ? "warning" : clamped >= 55 ? "brand" : "info";
  const key = tone ?? auto;
  const color = {
    brand: "var(--color-brand)",
    success: "var(--color-success)",
    warning: "var(--color-warning)",
    danger: "var(--color-danger)",
    info: "var(--color-info)",
  }[key];

  return (
    <div className="flex flex-col items-center gap-2">
      <div className="relative" style={{ width: size, height: size }}>
        <svg width={size} height={size} className="-rotate-90">
          <circle
            cx={size / 2}
            cy={size / 2}
            r={r}
            fill="none"
            stroke="var(--color-active)"
            strokeWidth={stroke}
          />
          <circle
            cx={size / 2}
            cy={size / 2}
            r={r}
            fill="none"
            stroke={color}
            strokeWidth={stroke}
            strokeLinecap="round"
            strokeDasharray={c}
            strokeDashoffset={c - (clamped / 100) * c}
            style={{
              transition: "stroke-dashoffset 600ms cubic-bezier(.16,1,.3,1), stroke 300ms",
              filter: `drop-shadow(0 0 6px ${color}55)`,
            }}
          />
        </svg>
        <div className="absolute inset-0 flex flex-col items-center justify-center">
          <span className="num-tabular text-[var(--text-xl)] font-semibold tracking-[-0.02em] leading-none">
            {clamped.toFixed(0)}
            <span className="text-[var(--text-xs)] font-normal text-[var(--color-muted-foreground)] ml-0.5">
              %
            </span>
          </span>
          <span className="mt-1 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
            {label}
          </span>
        </div>
      </div>
      {caption && (
        <span className="text-[var(--text-2xs)] text-[var(--color-subtle-foreground)] text-center truncate max-w-[140px]">
          {caption}
        </span>
      )}
    </div>
  );
}

/* ------------------------------------------------------------ 骨架屏 */

export function Skeleton({ className, style }: { className?: string; style?: React.CSSProperties }) {
  return <div className={cn("skeleton-shimmer rounded-[8px]", className)} style={style} />;
}

export function SkeletonCard() {
  return (
    <div className="rounded-[var(--radius-lg)] border border-[var(--color-line)] bg-[var(--color-card)] p-4">
      <Skeleton className="h-3 w-20" />
      <Skeleton className="mt-3 h-7 w-32" />
      <Skeleton className="mt-3 h-2 w-full" />
    </div>
  );
}

/* ------------------------------------------------------------ 空状态 */

export function EmptyState({
  icon: Icon,
  title,
  description,
  action,
  className,
  compact,
}: {
  icon?: LucideIcon;
  title: React.ReactNode;
  description?: React.ReactNode;
  action?: React.ReactNode;
  className?: string;
  compact?: boolean;
}) {
  return (
    <div
      className={cn(
        "flex flex-col items-center justify-center text-center",
        compact ? "py-8 px-4" : "py-16 px-6",
        className
      )}
    >
      {Icon && (
        <div className="mb-3.5 flex size-11 items-center justify-center rounded-[var(--radius-xl)] border border-[var(--color-line)] bg-[var(--color-elevated)]">
          <Icon className="size-5 text-[var(--color-subtle-foreground)]" />
        </div>
      )}
      <p className="text-[var(--text-sm)] font-medium text-[var(--color-foreground)]">{title}</p>
      {description && (
        <p className="mt-1.5 max-w-sm text-[var(--text-xs)] leading-relaxed text-[var(--color-muted-foreground)]">
          {description}
        </p>
      )}
      {action && <div className="mt-4">{action}</div>}
    </div>
  );
}

/* ------------------------------------------------------------ 加载态 */

export function Spinner({ className, size = 16 }: { className?: string; size?: number }) {
  return <Loader2 className={cn("animate-ds-spin text-[var(--color-muted-foreground)]", className)} size={size} />;
}

export function LoadingBlock({ label = "加载中", className }: { label?: string; className?: string }) {
  return (
    <div className={cn("flex items-center justify-center gap-2 py-16", className)}>
      <Spinner size={15} />
      <span className="text-[var(--text-xs)] text-[var(--color-muted-foreground)]">{label}</span>
    </div>
  );
}

/* ------------------------------------------------------------ 数值展示 */

export function BigNumber({
  value,
  unit,
  label,
  hint,
  tone,
  className,
}: {
  value: React.ReactNode;
  unit?: string;
  label: React.ReactNode;
  hint?: React.ReactNode;
  tone?: "default" | "brand" | "success" | "danger";
  className?: string;
}) {
  const color =
    tone === "brand"
      ? "text-[var(--color-brand)]"
      : tone === "success"
      ? "text-[var(--color-success)]"
      : tone === "danger"
      ? "text-[var(--color-danger)]"
      : "text-[var(--color-foreground)]";
  return (
    <div className={className}>
      <div className={cn("num-tabular text-[var(--text-2xl)] font-semibold tracking-[-0.03em] leading-none", color)}>
        {value}
        {unit && (
          <span className="ml-1 text-[var(--text-xs)] font-medium text-[var(--color-muted-foreground)]">
            {unit}
          </span>
        )}
      </div>
      <div className="mt-2 text-[var(--text-xs)] text-[var(--color-muted-foreground)]">{label}</div>
      {hint && <div className="mt-0.5 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">{hint}</div>}
    </div>
  );
}
