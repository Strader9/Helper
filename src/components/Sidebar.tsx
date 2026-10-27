import * as React from "react";
import { NavLink } from "react-router-dom";
import { invoke } from "@tauri-apps/api/core";
import {
  Shield,
  PanelLeftClose,
  PanelLeftOpen,
  PictureInPicture2,
  Command,
} from "lucide-react";
import { cn } from "../lib/cn";
import { StatusDot } from "./ui/badge";
import { Tooltip } from "./ui/primitives";
import { NAV_GROUPS, type NavItem } from "./nav";

/**
 * 侧边栏。
 *
 * 交互约定：
 *   - 折叠状态由 App 持有并持久化到 localStorage（刷新不还原，桌面应用
 *     用户一旦收起就不希望它自己弹回来）
 *   - 激活项用「左侧 2px 品牌色条 + 提亮背景」表达，而不是整块橙色填充：
 *     整块填充会让侧栏 6 个入口抢注意力
 *   - 折叠后 tooltip 补位，图标点击区域保持 40px 便于点击
 */

type NavRowItem = Pick<NavItem, "to" | "label" | "icon">;

function NavRow({ item, collapsed }: { item: NavRowItem; collapsed: boolean }) {
  const Icon = item.icon;
  const link = (
    <NavLink
      to={item.to}
      className={({ isActive }) =>
        cn(
          "group relative flex h-8 items-center gap-2.5 rounded-lg px-2.5",
          "text-[var(--text-sm)] font-medium",
          "transition-colors duration-[140ms]",
          collapsed && "justify-center px-0",
          isActive
            ? "bg-[var(--color-sidebar-accent)] text-[var(--color-sidebar-accent-foreground)]"
            : "text-[var(--color-sidebar-foreground)]/70 hover:bg-[var(--color-sidebar-accent)] hover:text-[var(--color-sidebar-foreground)]"
        )
      }
    >
      {({ isActive }) => (
        <>
          {isActive && (
            <span className="absolute left-0 top-1/2 h-4 w-[2px] -translate-y-1/2 rounded-full bg-[var(--color-brand)]" />
          )}
          <Icon
            className={cn(
              "size-4 shrink-0 transition-colors",
              isActive ? "text-[var(--color-brand)]" : "text-[var(--color-subtle-foreground)] group-hover:text-[var(--color-muted-foreground)]"
            )}
          />
          {!collapsed && <span className="truncate">{item.label}</span>}
        </>
      )}
    </NavLink>
  );

  return collapsed ? (
    <Tooltip content={item.label} side="right">
      {link}
    </Tooltip>
  ) : (
    link
  );
}

export default function Sidebar({
  collapsed,
  onToggleCollapse,
  onOpenCommand,
}: {
  collapsed: boolean;
  onToggleCollapse: () => void;
  onOpenCommand: () => void;
}) {
  const [healthy, setHealthy] = React.useState<boolean | null>(null);

  React.useEffect(() => {
    let alive = true;
    invoke<unknown>("get_system_status")
      .then(() => alive && setHealthy(true))
      .catch(() => alive && setHealthy(false));
    return () => {
      alive = false;
    };
  }, []);

  async function handleToggleMini() {
    try {
      await invoke("toggle_mini_window");
    } catch (e) {
      console.error("Failed to toggle mini window:", e);
    }
  }

  return (
    <aside
      className={cn(
        "ds flex h-full shrink-0 flex-col border-r border-[var(--color-sidebar-border)]",
        "bg-[var(--color-sidebar)]",
        "transition-[width] duration-[220ms] ease-[cubic-bezier(.16,1,.3,1)]",
        collapsed ? "w-60" : "w-56"
      )}
    >
      {/* 品牌区 */}
      <div
        className={cn(
          "flex h-13 shrink-0 items-center gap-2.5 px-3",
          collapsed && "justify-center px-0"
        )}
      >
        <div className="flex size-7 shrink-0 items-center justify-center rounded-lg bg-[var(--color-brand)]">
          <Shield className="size-4 text-[var(--color-brand-foreground)]" strokeWidth={2.4} />
        </div>
        {!collapsed && (
          <div className="min-w-0 flex-1">
            <div className="truncate text-[var(--text-sm)] font-semibold tracking-[-0.01em]">
              E-Assisant
            </div>
            <div className="truncate text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
              本地 AI 助手
            </div>
          </div>
        )}
        {!collapsed && (
          <Tooltip content="收起侧栏" side="bottom">
            <button
              onClick={onToggleCollapse}
              className="inline-flex size-7 cursor-pointer items-center justify-center rounded-[7px] text-[var(--color-subtle-foreground)] transition-colors hover:bg-[var(--color-hover)] hover:text-[var(--color-foreground)]"
            >
              <PanelLeftClose className="size-4" />
            </button>
          </Tooltip>
        )}
      </div>

      {/* 命令面板入口 */}
      <div className={cn("px-2.5 pb-2", collapsed && "px-2")}>
        <button
          onClick={onOpenCommand}
          className={cn(
            "group flex h-7.5 w-full cursor-pointer items-center gap-2 rounded-[8px] px-2",
            "border border-[var(--color-line)] bg-[var(--color-card)]",
            "text-[var(--text-xs)] text-[var(--color-subtle-foreground)]",
            "transition-colors hover:border-[var(--color-line-strong)] hover:text-[var(--color-muted-foreground)]",
            collapsed && "justify-center px-0"
          )}
        >
          <Command className="size-3.5 shrink-0" />
          {!collapsed && (
            <>
              <span className="flex-1 text-left">搜索或跳转</span>
              <kbd className="rounded-[4px] border border-[var(--color-line)] bg-[var(--color-elevated)] px-1 text-[var(--text-2xs)]">
                ⌘K
              </kbd>
            </>
          )}
        </button>
      </div>

      {/* 导航 */}
      <nav className="min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-2.5 pb-3">
        {NAV_GROUPS.map((group) => (
          <div key={group.title} className="mb-3 last:mb-0">
            {!collapsed && <div className="label-caps px-2 pb-1.5 pt-1">{group.title}</div>}
            <div className="flex flex-col gap-0.5">
              {group.items.map((item) => (
                <NavRow key={item.to} item={item} collapsed={collapsed} />
              ))}
            </div>
          </div>
        ))}
      </nav>

      {/* 底部 */}
      <div
        className={cn(
          "shrink-0 border-t border-[var(--color-line-faint)] px-2.5 py-2.5",
          collapsed && "px-2"
        )}
      >
        {collapsed ? (
          <div className="flex flex-col items-center gap-2">
            <Tooltip content={healthy ? "后端已连接" : healthy === false ? "后端未连接" : "检测中"} side="right">
              <div className="flex size-7 items-center justify-center">
                <StatusDot tone={healthy ? "success" : healthy === false ? "danger" : "muted"} pulse={healthy === true} />
              </div>
            </Tooltip>
            <Tooltip content="浮窗模式" side="right">
              <button
                onClick={handleToggleMini}
                className="inline-flex size-7 cursor-pointer items-center justify-center rounded-[7px] text-[var(--color-subtle-foreground)] hover:bg-[var(--color-hover)] hover:text-[var(--color-foreground)]"
              >
                <PictureInPicture2 className="size-4" />
              </button>
            </Tooltip>
            <Tooltip content="展开侧栏" side="right">
              <button
                onClick={onToggleCollapse}
                className="inline-flex size-7 cursor-pointer items-center justify-center rounded-[7px] text-[var(--color-subtle-foreground)] hover:bg-[var(--color-hover)] hover:text-[var(--color-foreground)]"
              >
                <PanelLeftOpen className="size-4" />
              </button>
            </Tooltip>
          </div>
        ) : (
          <>
            <div className="mb-2 flex items-center gap-2 px-1.5 py-1">
              <StatusDot tone={healthy ? "success" : healthy === false ? "danger" : "muted"} pulse={healthy === true} />
              <span className="text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                {healthy ? "本地内核已连接" : healthy === false ? "内核未连接" : "正在检测内核"}
              </span>
            </div>
            <div className="flex items-center gap-1">
              <button
                onClick={handleToggleMini}
                className="flex h-7 flex-1 cursor-pointer items-center gap-1.5 rounded-[7px] px-2 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)] transition-colors hover:bg-[var(--color-hover)] hover:text-[var(--color-foreground)]"
              >
                <PictureInPicture2 className="size-3.5" />
                浮窗
              </button>
              <span className="pr-1.5 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)] opacity-60">
                v2.1
              </span>
            </div>
          </>
        )}
      </div>
    </aside>
  );
}
