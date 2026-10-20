import * as React from "react";
import { cn } from "../../lib/cn";

/**
 * 页面骨架。
 *
 * 统一三件事，避免每个页面各写一套：
 *   - 纵向结构：固定 header + 可滚动 body
 *   - 横向留白：常规 24px，宽屏 32px，最大 1180px 居中（否则超宽屏一行
 *     拉到 2000px 可读性会崩）
 *   - 标题区排版：主标题 20px/600，副标题 12px/次要色，右侧放主操作
 */
export function Page({
  children,
  className,
  wide,
}: {
  children: React.ReactNode;
  className?: string;
  wide?: boolean;
}) {
  return (
    <div className={cn("ds flex h-full flex-col overflow-hidden", className)}>
      <div className={cn("mx-auto flex h-full w-full flex-col", wide ? "max-w-none" : "max-w-[1180px]")}>
        {children}
      </div>
    </div>
  );
}

export function PageHeader({
  title,
  description,
  actions,
  icon: Icon,
  badge,
  className,
  children,
}: {
  title: React.ReactNode;
  description?: React.ReactNode;
  actions?: React.ReactNode;
  icon?: React.ComponentType<{ className?: string }>;
  badge?: React.ReactNode;
  className?: string;
  children?: React.ReactNode;
}) {
  return (
    <header className={cn("shrink-0 px-5 pb-3.5 pt-4 md:px-6", className)}>
      <div className="flex items-start justify-between gap-4">
        <div className="flex min-w-0 items-start gap-2.5">
          {Icon && (
            <div className="mt-px flex size-8 shrink-0 items-center justify-center rounded-lg bg-[var(--color-card)] ring-1 ring-[var(--color-line)]">
              <Icon className="size-4 text-[var(--color-muted-foreground)]" />
            </div>
          )}
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <h1 className="truncate text-[var(--text-xl)] font-semibold leading-tight tracking-[-0.015em] text-[var(--color-foreground)]">
                {title}
              </h1>
              {badge}
            </div>
            {description && (
              <p className="mt-0.5 text-[var(--text-xs)] leading-relaxed text-[var(--color-muted-foreground)]">
                {description}
              </p>
            )}
          </div>
        </div>
        {actions && <div className="flex shrink-0 items-center gap-1.5">{actions}</div>}
      </div>
      {children}
    </header>
  );
}

export function PageBody({
  children,
  className,
  padded = true,
}: {
  children: React.ReactNode;
  className?: string;
  padded?: boolean;
}) {
  return (
    <div
      className={cn(
        "min-h-0 flex-1 overflow-y-auto",
        padded && "px-5 pb-8 md:px-6",
        className
      )}
    >
      {children}
    </div>
  );
}

/** 工具条：搜索 + 筛选 + 计数，页面内列表页统一用它 */
export function Toolbar({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("mb-4 flex flex-wrap items-center gap-2", className)}>{children}</div>
  );
}

/** 网格容器：响应式卡片网格，统一 gap */
export function Grid({
  children,
  cols = 3,
  className,
}: {
  children: React.ReactNode;
  cols?: 2 | 3 | 4;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "grid gap-3",
        cols === 2 && "grid-cols-1 sm:grid-cols-2",
        cols === 3 && "grid-cols-1 sm:grid-cols-2 lg:grid-cols-3",
        cols === 4 && "grid-cols-2 sm:grid-cols-3 lg:grid-cols-4",
        className
      )}
    >
      {children}
    </div>
  );
}

/** 键值对行：详情面板用，左标签右值，值可换行 */
export function KeyValue({
  label,
  children,
  mono,
  className,
}: {
  label: React.ReactNode;
  children: React.ReactNode;
  mono?: boolean;
  className?: string;
}) {
  return (
    <div className={cn("flex items-start justify-between gap-4 py-1.5", className)}>
      <span className="shrink-0 text-[var(--text-xs)] text-[var(--color-muted-foreground)]">
        {label}
      </span>
      <span
        className={cn(
          "min-w-0 break-all text-right text-[var(--text-xs)] text-[var(--color-foreground)]",
          mono && "font-mono text-[var(--text-2xs)]"
        )}
      >
        {children}
      </span>
    </div>
  );
}

/** 分隔线：比 border-t 更弱，用于卡片内部 */
export function Divider({ className }: { className?: string }) {
  return <div className={cn("h-px w-full bg-[var(--color-line-faint)]", className)} />;
}
