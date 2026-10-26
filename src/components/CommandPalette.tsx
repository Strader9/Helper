import * as React from "react";
import { useNavigate } from "react-router-dom";
import { Search, CornerDownLeft, ArrowUp, ArrowDown } from "lucide-react";
import { Dialog, DialogContent } from "./ui/primitives";
import { ALL_NAV, type NavItem } from "./nav";
import { cn } from "../lib/cn";

/**
 * 命令面板（⌘K / Ctrl+K）。
 *
 * 这是桌面应用"高级感"最划算的一处投入：键盘用户不必先去点侧栏。
 * 只做跳转与轻量动作，不做参数补全——参数补全留给对话框。
 */
export function CommandPalette({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
}) {
  const navigate = useNavigate();
  const [query, setQuery] = React.useState("");
  const [active, setActive] = React.useState(0);
  const listRef = React.useRef<HTMLDivElement>(null);

  const results = React.useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return ALL_NAV;
    return ALL_NAV.filter((item) => {
      const hay = `${item.label} ${item.hint ?? ""} ${item.keywords ?? ""}`.toLowerCase();
      return q.split(/\s+/).every((part) => hay.includes(part));
    });
  }, [query]);

  React.useEffect(() => {
    if (open) {
      setQuery("");
      setActive(0);
    }
  }, [open]);

  React.useEffect(() => {
    setActive(0);
  }, [query]);

  // 选中项滚进视口：键盘连续按 ↓ 时不能跑到可视区外
  React.useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-index="${active}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [active]);

  function go(item: NavItem) {
    navigate(item.to);
    onOpenChange(false);
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => (results.length ? (i + 1) % results.length : 0));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => (results.length ? (i - 1 + results.length) % results.length : 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const item = results[active];
      if (item) go(item);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        size="lg"
        className="top-[22%] translate-y-0 max-h-[62vh] p-0 gap-0"
        onKeyDown={onKeyDown}
      >
        <div className="flex items-center gap-2.5 border-b border-[var(--color-line)] px-3.5">
          <Search className="size-4 shrink-0 text-[var(--color-subtle-foreground)]" />
          <input
            autoFocus
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索页面、能力、设置…"
            className="h-12 w-full bg-transparent text-[var(--text-base)] text-[var(--color-foreground)] outline-none placeholder:text-[var(--color-subtle-foreground)]"
          />
          <kbd className="shrink-0 rounded-[5px] border border-[var(--color-line)] bg-[var(--color-surface)] px-1.5 py-0.5 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
            ESC
          </kbd>
        </div>

        <div ref={listRef} className="min-h-0 flex-1 overflow-y-auto p-1.5">
          {results.length === 0 ? (
            <div className="px-3 py-10 text-center text-[var(--text-xs)] text-[var(--color-muted-foreground)]">
              没有匹配「{query}」的页面
            </div>
          ) : (
            results.map((item, i) => {
              const Icon = item.icon;
              return (
                <button
                  key={item.to}
                  data-index={i}
                  onMouseMove={() => setActive(i)}
                  onClick={() => go(item)}
                  className={cn(
                    "flex w-full cursor-pointer items-center gap-3 rounded-[8px] px-2.5 py-2 text-left",
                    "transition-colors duration-[100ms]",
                    i === active ? "bg-[var(--color-hover)]" : "hover:bg-[var(--color-hover)]/60"
                  )}
                >
                  <Icon
                    className={cn(
                      "size-4 shrink-0",
                      i === active ? "text-[var(--color-brand)]" : "text-[var(--color-subtle-foreground)]"
                    )}
                  />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate text-[var(--text-sm)] font-medium text-[var(--color-foreground)]">
                      {item.label}
                    </span>
                    {item.hint && (
                      <span className="block truncate text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                        {item.hint}
                      </span>
                    )}
                  </span>
                  {i === active && (
                    <CornerDownLeft className="size-3.5 shrink-0 text-[var(--color-subtle-foreground)]" />
                  )}
                </button>
              );
            })
          )}
        </div>

        <div className="flex shrink-0 items-center gap-4 border-t border-[var(--color-line-faint)] px-3.5 py-2 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
          <span className="flex items-center gap-1">
            <ArrowUp className="size-3" />
            <ArrowDown className="size-3" />
            选择
          </span>
          <span className="flex items-center gap-1">
            <CornerDownLeft className="size-3" />
            打开
          </span>
          <span className="ml-auto">{results.length} 项</span>
        </div>
      </DialogContent>
    </Dialog>
  );
}
