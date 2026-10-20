import * as React from "react";
import * as TabsPrimitive from "@radix-ui/react-tabs";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import * as TooltipPrimitive from "@radix-ui/react-tooltip";
import { X } from "lucide-react";
import { cn } from "../../lib/cn";

/* ------------------------------------------------------------------ Tabs */

export const Tabs = TabsPrimitive.Root;

export const TabsList = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.List>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.List>
>(({ className, ...props }, ref) => (
  <TabsPrimitive.List
    ref={ref}
    className={cn(
      "inline-flex items-center gap-0.5 rounded-[10px] p-0.5",
      "border border-[var(--color-line)] bg-[var(--color-surface)]",
      className
    )}
    {...props}
  />
));
TabsList.displayName = "TabsList";

export const TabsTrigger = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.Trigger>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.Trigger>
>(({ className, ...props }, ref) => (
  <TabsPrimitive.Trigger
    ref={ref}
    className={cn(
      "inline-flex items-center gap-1.5 rounded-[7px] px-2.5 h-6.5",
      "text-[var(--text-xs)] font-medium whitespace-nowrap",
      "text-[var(--color-muted-foreground)] cursor-pointer",
      "transition-all duration-[140ms]",
      "hover:text-[var(--color-foreground)]",
      "data-[state=active]:bg-[var(--color-elevated)] data-[state=active]:text-[var(--color-foreground)]",
      "data-[state=active]:shadow-[inset_0_1px_0_rgba(255,255,255,.05),var(--shadow-xs)]",
      "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-brand)]",
      "[&_svg]:size-3.5",
      className
    )}
    {...props}
  />
));
TabsTrigger.displayName = "TabsTrigger";

export const TabsContent = TabsPrimitive.Content;

/* ---------------------------------------------------------------- Dialog */

export const Dialog = DialogPrimitive.Root;
export const DialogTrigger = DialogPrimitive.Trigger;
export const DialogClose = DialogPrimitive.Close;

export const DialogContent = React.forwardRef<
  React.ElementRef<typeof DialogPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof DialogPrimitive.Content> & {
    size?: "sm" | "md" | "lg" | "xl";
  }
>(({ className, children, size = "md", ...props }, ref) => (
  <DialogPrimitive.Portal>
    <DialogPrimitive.Overlay
      className={cn(
        "fixed inset-0 z-50 bg-black/55 backdrop-blur-[2px]",
        "data-[state=open]:animate-ds-fade-in"
      )}
    />
    <DialogPrimitive.Content
      ref={ref}
      className={cn(
        "fixed left-1/2 top-1/2 z-50 w-[92vw] -translate-x-1/2 -translate-y-1/2",
        "rounded-[var(--radius-xl)] border border-[var(--color-line-strong)]",
        "bg-[var(--color-elevated)] shadow-[var(--shadow-xl)]",
        "data-[state=open]:animate-ds-scale-in",
        "max-h-[86vh] overflow-hidden flex flex-col",
        size === "sm" && "max-w-sm",
        size === "md" && "max-w-md",
        size === "lg" && "max-w-2xl",
        size === "xl" && "max-w-4xl",
        className
      )}
      {...props}
    >
      {children}
      <DialogPrimitive.Close
        className={cn(
          "absolute right-3 top-3 inline-flex size-7 items-center justify-center rounded-[7px]",
          "text-[var(--color-subtle-foreground)] cursor-pointer",
          "hover:bg-[var(--color-hover)] hover:text-[var(--color-foreground)]",
          "transition-colors"
        )}
      >
        <X className="size-4" />
        <span className="sr-only">关闭</span>
      </DialogPrimitive.Close>
    </DialogPrimitive.Content>
  </DialogPrimitive.Portal>
));
DialogContent.displayName = "DialogContent";

export function DialogHeader({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("px-5 pt-5 pb-3", className)} {...props} />;
}

export function DialogTitle({ className, ...props }: React.ComponentProps<"h2">) {
  return (
    <DialogPrimitive.Title
      className={cn("text-[var(--text-lg)] font-semibold tracking-[-0.01em]", className)}
      {...props}
    />
  );
}

export function DialogDescription({ className, ...props }: React.ComponentProps<"p">) {
  return (
    <DialogPrimitive.Description
      className={cn("mt-1.5 text-[var(--text-xs)] leading-relaxed text-[var(--color-muted-foreground)]", className)}
      {...props}
    />
  );
}

export function DialogBody({ className, ...props }: React.ComponentProps<"div">) {
  return <div className={cn("px-5 py-3 overflow-y-auto", className)} {...props} />;
}

export function DialogFooter({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      className={cn(
        "flex items-center justify-end gap-2 px-5 py-3.5",
        "border-t border-[var(--color-line-faint)] bg-[var(--color-surface)]/40",
        className
      )}
      {...props}
    />
  );
}

/* --------------------------------------------------------------- Tooltip */

export const TooltipProvider = TooltipPrimitive.Provider;

export function Tooltip({
  content,
  children,
  side = "top",
  delay = 240,
}: {
  content: React.ReactNode;
  children: React.ReactNode;
  side?: "top" | "right" | "bottom" | "left";
  delay?: number;
}) {
  return (
    <TooltipPrimitive.Root delayDuration={delay}>
      <TooltipPrimitive.Trigger asChild>{children}</TooltipPrimitive.Trigger>
      <TooltipPrimitive.Portal>
        <TooltipPrimitive.Content
          side={side}
          sideOffset={6}
          className={cn(
            "z-50 max-w-64 rounded-[7px] px-2.5 py-1.5",
            "bg-[var(--color-active)] text-[var(--text-2xs)] leading-snug text-[var(--color-foreground)]",
            "border border-[var(--color-line-strong)] shadow-[var(--shadow-md)]",
            "animate-ds-fade-in"
          )}
        >
          {content}
        </TooltipPrimitive.Content>
      </TooltipPrimitive.Portal>
    </TooltipPrimitive.Root>
  );
}

/* ------------------------------------------------- 分段控件（Segmented） */

/**
 * 分段控件：2-4 个互斥选项的紧凑切换。
 * 比 Select 更快，比 Tabs 更轻（不改内容区结构）。
 */
export function Segmented<T extends string>({
  value,
  options,
  onChange,
  className,
  size = "md",
}: {
  value: T;
  options: { value: T; label: React.ReactNode }[];
  onChange: (v: T) => void;
  className?: string;
  size?: "sm" | "md";
}) {
  return (
    <div
      className={cn(
        "inline-flex items-center gap-0.5 rounded-[9px] p-0.5",
        "border border-[var(--color-line)] bg-[var(--color-surface)]",
        className
      )}
    >
      {options.map((o) => {
        const active = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            onClick={() => onChange(o.value)}
            className={cn(
              "inline-flex items-center justify-center gap-1.5 rounded-[7px] font-medium",
              "whitespace-nowrap cursor-pointer transition-all duration-[140ms]",
              size === "sm" ? "h-5.5 px-2 text-[var(--text-2xs)]" : "h-6.5 px-2.5 text-[var(--text-xs)]",
              active
                ? "bg-[var(--color-elevated)] text-[var(--color-foreground)] shadow-[inset_0_1px_0_rgba(255,255,255,.05),var(--shadow-xs)]"
                : "text-[var(--color-muted-foreground)] hover:text-[var(--color-foreground)]"
            )}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}
