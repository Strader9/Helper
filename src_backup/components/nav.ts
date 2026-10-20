import {
  MessageSquare,
  LayoutDashboard,
  Sparkles,
  Mail,
  Workflow,
  Boxes,
  ListChecks,
  Brain,
  Package,
  Smartphone,
  Globe,
  Code2,
  Settings2,
  ScrollText,
  type LucideIcon,
} from "lucide-react";

export type NavItem = {
  to: string;
  label: string;
  icon: LucideIcon;
  /** 命令面板里的补充说明，写"这个页面能干什么"，不写功能名重复 */
  hint?: string;
  keywords?: string;
};

export type NavGroup = {
  title: string;
  items: NavItem[];
};

/**
 * 全站导航的唯一来源。侧边栏与命令面板都从这里取，
 * 加页面只改这一处——两处各写一份必然漂移。
 */
export const NAV_GROUPS: NavGroup[] = [
  {
    title: "工作",
    items: [
      { to: "/chat", label: "对话", icon: MessageSquare, hint: "和助手对话，执行工具", keywords: "chat 聊天" },
      { to: "/dashboard", label: "工作台", icon: LayoutDashboard, hint: "系统状态与今日概览", keywords: "dashboard 仪表盘 监控" },
      { to: "/tasks", label: "任务", icon: ListChecks, hint: "运行中与历史任务", keywords: "task 任务 队列" },
    ],
  },
  {
    title: "能力",
    items: [
      { to: "/media", label: "媒体工坊", icon: Sparkles, hint: "生成图像与视频", keywords: "image video 图像 视频 生成" },
      { to: "/mail", label: "邮件", icon: Mail, hint: "收件箱与发信", keywords: "mail email 邮箱 imap smtp" },
      { to: "/automation", label: "自动化", icon: Workflow, hint: "主动规则与定时任务", keywords: "automation 定时 计划 规则" },
      { to: "/tools", label: "能力中心", icon: Boxes, hint: "全部工具与契约覆盖", keywords: "tools 工具 能力 契约" },
    ],
  },
  {
    title: "资产",
    items: [
      { to: "/memory", label: "记忆", icon: Brain, hint: "长期记忆条目", keywords: "memory 记忆" },
      { to: "/skills", label: "技能", icon: Package, hint: "已安装技能与市场", keywords: "skill 技能 插件" },
      { to: "/devices", label: "设备", icon: Smartphone, hint: "配对设备与网关", keywords: "device 设备 配对 网关" },
    ],
  },
  {
    title: "系统",
    items: [
      { to: "/web-studio", label: "网页工坊", icon: Globe, hint: "生成并发布网页", keywords: "web 网页 html 发布" },
      { to: "/github", label: "GitHub", icon: Code2, hint: "趋势仓库", keywords: "github 趋势" },
      { to: "/settings", label: "设置", icon: Settings2, hint: "模型、凭据、安全策略", keywords: "settings 设置 配置 key" },
      { to: "/logs", label: "日志", icon: ScrollText, hint: "审计与执行回执", keywords: "log 日志 审计 回执" },
    ],
  },
];

export const ALL_NAV: NavItem[] = NAV_GROUPS.flatMap((g) => g.items);
