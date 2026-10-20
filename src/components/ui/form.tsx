import * as React from "react";
import * as SwitchPrimitive from "@radix-ui/react-switch";
import * as SelectPrimitive from "@radix-ui/react-select";
import { Check, ChevronDown } from "lucide-react";
import { cn } from "../../lib/cn";

const fieldBase = [
  "w-full bg-[var(--color-surface)] text-[var(--color-foreground)]",
  "border border-[var(--color-line)] rounded-[var(--radius-sm)]",
  "text-[var(--text-sm)] placeholder:text-[var(--color-subtle-foreground)]",
  "transition-[border-color,box-shadow,background] duration-[140ms]",
  "hover:border-[var(--color-line-strong)]",
  "focus:outline-none focus:border-[var(--color-brand-line)]",
  "focus:shadow-[0_0_0_3px_var(--color-brand-soft)]",
  "disabled:opacity-50 disabled:cursor-not-allowed",
].join(" ");

export const Input = React.forwardRef<HTMLInputElement, React.ComponentProps<"input">>(
  ({ className, ...props }, ref) => (
    <input ref={ref} className={cn(fieldBase, "h-8.5 px-2.5", className)} {...props} />
  )
);
Input.displayName = "Input";

export const Textarea = React.forwardRef<HTMLTextAreaElement, React.ComponentProps<"textarea">>(
  ({ className, ...props }, ref) => (
    <textarea
      ref={ref}
      className={cn(fieldBase, "min-h-20 px-2.5 py-2 resize-y leading-relaxed", className)}
      {...props}
    />
  )
);
Textarea.displayName = "Textarea";

export function Label({
  className,
  children,
  hint,
  ...props
}: React.ComponentProps<"label"> & { hint?: React.ReactNode }) {
  return (
    <label
      className={cn(
        "flex items-center gap-1.5 text-[var(--text-xs)] font-medium text-[var(--color-muted-foreground)] mb-1.5",
        className
      )}
      {...props}
    >
      {children}
      {hint && (
        <span className="text-[var(--text-2xs)] font-normal text-[var(--color-subtle-foreground)]">
          {hint}
        </span>
      )}
    </label>
  );
}

/** 表单行：标签 + 控件 + 说明。设置页统一用它，保证纵向节奏一致。 */
export function Field({
  label,
  hint,
  description,
  children,
  className,
  horizontal,
}: {
  label?: React.ReactNode;
  hint?: React.ReactNode;
  description?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
  horizontal?: boolean;
}) {
  return (
    <div
      className={cn(
        horizontal ? "flex items-start justify-between gap-6" : "flex flex-col",
        className
      )}
    >
      <div className={cn(horizontal && "min-w-0 flex-1 py-0.5")}>
        {label && <Label hint={hint}>{label}</Label>}
        {description && (
          <p className="text-[var(--text-2xs)] leading-relaxed text-[var(--color-subtle-foreground)] mt-1">
            {description}
          </p>
        )}
      </div>
      <div className={cn(horizontal && "shrink-0 w-[46%] max-w-72")}>{children}</div>
    </div>
  );
}

export const Switch = React.forwardRef<
  React.ElementRef<typeof SwitchPrimitive.Root>,
  React.ComponentPropsWithoutRef<typeof SwitchPrimitive.Root>
>(({ className, ...props }, ref) => (
  <SwitchPrimitive.Root
    ref={ref}
    className={cn(
      "peer inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full",
      "border border-transparent transition-colors duration-200",
      "data-[state=checked]:bg-[var(--color-brand)] data-[state=unchecked]:bg-[var(--color-active)]",
      "disabled:cursor-not-allowed disabled:opacity-50",
      "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-brand)] focus-visible:ring-offset-2 focus-visible:ring-offset-[var(--color-background)]",
      className
    )}
    {...props}
  >
    <SwitchPrimitive.Thumb
      className={cn(
        "pointer-events-none block size-4 rounded-full bg-white shadow-sm",
        "transition-transform duration-200 ease-[cubic-bezier(.16,1,.3,1)]",
        "data-[state=checked]:translate-x-[18px] data-[state=unchecked]:translate-x-[2px]"
      )}
    />
  </SwitchPrimitive.Root>
));
Switch.displayName = "Switch";

export const Select = SelectPrimitive.Root;
export const SelectValue = SelectPrimitive.Value;

export const SelectTrigger = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Trigger>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Trigger>
>(({ className, children, ...props }, ref) => (
  <SelectPrimitive.Trigger
    ref={ref}
    className={cn(
      fieldBase,
      "h-8.5 px-2.5 flex items-center justify-between gap-2 text-left",
      "data-[placeholder]:text-[var(--color-subtle-foreground)]",
      className
    )}
    {...props}
  >
    {children}
    <SelectPrimitive.Icon asChild>
      <ChevronDown className="size-3.5 shrink-0 text-[var(--color-subtle-foreground)]" />
    </SelectPrimitive.Icon>
  </SelectPrimitive.Trigger>
));
SelectTrigger.displayName = "SelectTrigger";

export const SelectContent = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Content>
>(({ className, children, position = "popper", ...props }, ref) => (
  <SelectPrimitive.Portal>
    <SelectPrimitive.Content
      ref={ref}
      position={position}
      className={cn(
        "z-50 min-w-[var(--radix-select-trigger-width)] overflow-hidden",
        "rounded-[var(--radius-md)] border border-[var(--color-line-strong)]",
        "bg-[var(--color-elevated)] shadow-[var(--shadow-lg)]",
        "animate-ds-scale-in origin-[var(--radix-select-content-transform-origin)]",
        className
      )}
      {...props}
    >
      <SelectPrimitive.Viewport className="p-1">{children}</SelectPrimitive.Viewport>
    </SelectPrimitive.Content>
  </SelectPrimitive.Portal>
));
SelectContent.displayName = "SelectContent";

export const SelectItem = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Item>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Item>
>(({ className, children, ...props }, ref) => (
  <SelectPrimitive.Item
    ref={ref}
    className={cn(
      "relative flex w-full cursor-pointer select-none items-center gap-2",
      "rounded-[6px] py-1.5 pl-2 pr-7 text-[var(--text-sm)] outline-none",
      "text-[var(--color-foreground)]",
      "data-[highlighted]:bg-[var(--color-hover)]",
      "data-[disabled]:pointer-events-none data-[disabled]:opacity-50",
      className
    )}
    {...props}
  >
    <SelectPrimitive.ItemText>{children}</SelectPrimitive.ItemText>
    <span className="absolute right-2 flex size-3.5 items-center justify-center">
      <SelectPrimitive.ItemIndicator>
        <Check className="size-3.5 text-[var(--color-brand)]" />
      </SelectPrimitive.ItemIndicator>
    </span>
  </SelectPrimitive.Item>
));
SelectItem.displayName = "SelectItem";
