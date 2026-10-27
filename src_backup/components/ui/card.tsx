import * as React from "react";
import { cn } from "../../lib/cn";

/**
 * 卡片（对齐 EAssistantCore Card）。
 *
 * 层级只靠两件事：卡片比背景亮一档的底色 + 1px 半透明 ring。
 * 不用投影。桌面应用里大面积投影会显得脏，阴影只留给真正浮起来的
 * 东西（Dialog / 下拉 / 抽屉）。
 */
export function Card({
  className,
  interactive,
  ...props
}: React.ComponentProps<"div"> & { interactive?: boolean }) {
  return (
    <div
      data-slot="card"
      className={cn(
        "relative flex flex-col gap-3 overflow-hidden rounded-xl py-3.5",
        "bg-[var(--color-card)] text-[var(--color-card-foreground)]",
        "ring-1 ring-[var(--color-line)]",
        interactive && [
          "cursor-pointer transition-colors duration-[140ms] ease-[cubic-bezier(.16,1,.3,1)]",
          "hover:bg-[var(--color-elevated)] hover:ring-[var(--color-line-strong)]",
        ],
        className
      )}
      {...props}
    />
  );
}

export function CardHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-header"
      className={cn("flex items-start justify-between gap-3 px-3.5", className)}
      {...props}
    />
  );
}

export function CardTitle({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-title"
      className={cn(
        "text-[var(--text-sm)] font-medium leading-snug text-[var(--color-foreground)]",
        className
      )}
      {...props}
    />
  );
}

export function CardDescription({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-description"
      className={cn("text-[var(--text-xs)] text-[var(--color-muted-foreground)]", className)}
      {...props}
    />
  );
}

export function CardContent({ className, ...props }: React.ComponentProps<"div">) {
  return <div data-slot="card-content" className={cn("px-3.5", className)} {...props} />;
}

export function CardFooter({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-footer"
      className={cn(
        "mt-auto flex items-center gap-2 border-t border-[var(--color-line)] px-3.5 pt-3",
        className
      )}
      {...props}
    />
  );
}

/** 分组标题：页面内的二级分节，不带卡片外框 */
export function SectionHeader({
  title,
  description,
  action,
  className,
}: {
  title: React.ReactNode;
  description?: React.ReactNode;
  action?: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("mb-2.5 flex items-end justify-between gap-4", className)}>
      <div className="min-w-0">
        <h2 className="text-[var(--text-sm)] font-medium text-[var(--color-foreground)]">
          {title}
        </h2>
        {description && (
          <p className="mt-0.5 text-[var(--text-xs)] text-[var(--color-muted-foreground)]">
            {description}
          </p>
        )}
      </div>
      {action && <div className="shrink-0">{action}</div>}
    </div>
  );
}
