import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "../../lib/cn";

const badgeVariants = cva(
  [
    "inline-flex items-center gap-1 rounded-full border px-2 h-5",
    "text-[var(--text-2xs)] font-medium whitespace-nowrap",
    "[&_svg]:size-2.5",
  ].join(" "),
  {
    variants: {
      tone: {
        neutral: "border-[var(--color-line)] bg-[var(--color-elevated)] text-[var(--color-muted-foreground)]",
        brand: "border-[var(--color-brand-line)] bg-[var(--color-brand-soft)] text-[var(--color-brand)]",
        info: "border-[var(--color-info)]/25 bg-[var(--color-info)]/10 text-[var(--color-info)]",
        success: "border-[var(--color-success)]/25 bg-[var(--color-success)]/10 text-[var(--color-success)]",
        warning: "border-[var(--color-warning)]/25 bg-[var(--color-warning)]/10 text-[var(--color-warning)]",
        danger: "border-[var(--color-danger)]/25 bg-[var(--color-danger)]/10 text-[var(--color-danger)]",
        outline: "border-[var(--color-line-strong)] bg-transparent text-[var(--color-muted-foreground)]",
      },
      dot: { true: "pl-1.5", false: "" },
    },
    defaultVariants: { tone: "neutral", dot: false },
  }
);

export interface BadgeProps
  extends React.HTMLAttributes<HTMLSpanElement>,
    VariantProps<typeof badgeVariants> {
  pulse?: boolean;
}

export function Badge({ className, tone, dot, pulse, children, ...props }: BadgeProps) {
  return (
    <span className={cn(badgeVariants({ tone, dot }), className)} {...props}>
      {dot && (
        <span
          className={cn(
            "size-1.5 rounded-full bg-current shrink-0",
            pulse && "animate-[ds-pulse-dot_1.8s_ease-in-out_infinite]"
          )}
        />
      )}
      {children}
    </span>
  );
}

/** 状态点：只表示"活着/挂了"，不带文案时用 */
export function StatusDot({
  tone = "success",
  pulse,
  className,
}: {
  tone?: "success" | "warning" | "danger" | "brand" | "muted";
  pulse?: boolean;
  className?: string;
}) {
  const color = {
    success: "var(--color-success)",
    warning: "var(--color-warning)",
    danger: "var(--color-danger)",
    brand: "var(--color-brand)",
    muted: "var(--color-subtle-foreground)",
  }[tone];
  return (
    <span className={cn("relative flex size-2 shrink-0", className)}>
      {pulse && (
        <span
          className="absolute inset-0 rounded-full animate-[ds-pulse-dot_1.8s_ease-in-out_infinite]"
          style={{ background: color, opacity: 0.35 }}
        />
      )}
      <span className="relative inline-flex size-2 rounded-full" style={{ background: color }} />
    </span>
  );
}

/** 键帽：用于展示快捷键 */
export function Kbd({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <kbd
      className={cn(
        "inline-flex h-5 min-w-5 items-center justify-center rounded-[5px] px-1",
        "border border-[var(--color-line-strong)] bg-[var(--color-elevated)]",
        "text-[var(--text-2xs)] font-medium text-[var(--color-muted-foreground)]",
        "shadow-[inset_0_-1px_0_rgba(0,0,0,.25)]",
        className
      )}
    >
      {children}
    </kbd>
  );
}
