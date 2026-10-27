import { useEffect, useMemo, useState } from "react";
import { emit } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import {
  Bot,
  Check,
  Eye,
  EyeOff,
  FolderPlus,
  Info,
  KeyRound,
  Loader2,
  Mail,
  Plug,
  Palette,
  RotateCcw,
  ShieldCheck,
  Sparkles,
  Trash2,
  X,
} from "lucide-react";
import { Page, PageBody, PageHeader, KeyValue, Divider } from "../components/ui/page";
import { Card, SectionHeader } from "../components/ui/card";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Input, Switch, Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from "../components/ui/form";
import { LoadingBlock } from "../components/ui/feedback";
import { cn } from "../lib/cn";

// ============================================================
// 类型
// ============================================================

interface SettingItem {
  key: string;
  value: string;
  value_type: string;
  description: string | null;
  min_value: number | null;
  max_value: number | null;
  unit: string | null;
}

interface AllowedDirectory {
  path: string;
  label: string;
  access_level: string;
  created_at: string;
}

interface TrustedTool {
  tool_name: string;
  path_pattern: string | null;
  risk_level: string;
  granted_at: string;
}

type SectionId = "model" | "connect" | "ai" | "mail" | "app" | "secret" | "security" | "about";

const SECTIONS: { id: SectionId; label: string; icon: React.ComponentType<{ className?: string }> }[] = [
  { id: "model", label: "模型与生成", icon: Bot },
  { id: "connect", label: "连接与自动化", icon: Plug },
  { id: "ai", label: "AI 图像 / 视频", icon: Sparkles },
  { id: "mail", label: "邮件", icon: Mail },
  { id: "app", label: "外观与启动", icon: Palette },
  { id: "secret", label: "凭据", icon: KeyRound },
  { id: "security", label: "安全", icon: ShieldCheck },
  { id: "about", label: "关于", icon: Info },
];

const GROUPS: Record<SectionId, { title: string; note: string; keys: string[] }[]> = {
  model: [
    {
      title: "模型连接",
      note: "服务地址与模型名在启动时读取，改动后需重启；超时立即生效",
      keys: ["ollama.base_url", "ollama.model", "ollama.timeout_ms", "sidecar.url"],
    },
    {
      title: "生成参数",
      note: "均立即生效，从下一条消息开始应用",
      keys: ["agent.max_iterations", "agent.temperature", "agent.num_ctx", "agent.num_predict"],
    },
  ],
  connect: [
    {
      title: "主动助手",
      note: "按间隔巡检系统条件，命中即触发。下一轮巡检即按新频率执行",
      keys: ["agent.proactive_interval_sec", "agent.proactive_cooldown_sec"],
    },
    {
      title: "常驻调度",
      note: "定时任务到点自动执行；间隔与超时改动下一轮生效",
      keys: ["agent.scheduler_enabled", "agent.scheduler_tick_sec", "agent.scheduler_job_timeout_sec"],
    },
    {
      title: "多端访问",
      note: "默认只允许本机。要手机连，把绑定地址改成 0.0.0.0，并确保局域网可信——配对码等同于授权凭据",
      keys: [
        "agent.gateway_enabled",
        "agent.gateway_bind",
        "agent.gateway_port",
        "agent.gateway_reply_timeout_sec",
      ],
    },
  ],
  ai: [
    {
      title: "AI 图像生成",
      note: "走 OpenAI 兼容的 /images/generations。换服务商 = 改这三行 + 在「凭据」里填 Key，代码不用动",
      keys: ["image.base_url", "image.model", "image.size", "image.timeout_ms"],
    },
    {
      title: "AI 视频生成",
      note: "走 OpenAI 兼容的 /videos 异步任务接口。等待上限决定单次调用最多等多久",
      keys: ["video.base_url", "video.model", "video.size", "video.duration_sec", "video.poll_timeout_ms"],
    },
  ],
  mail: [
    {
      title: "发信（SMTP）",
      note: "多数邮箱不接受登录密码，要在网页端开启 SMTP 后生成「授权码」",
      keys: ["mail.smtp_host", "mail.smtp_port", "mail.smtp_security", "mail.smtp_user", "mail.from", "mail.from_name"],
    },
    {
      title: "收件（IMAP）",
      note: "收件是独立协议，必须单独填服务器；账号与授权码留空会自动复用发信配置",
      keys: ["mail.imap_host", "mail.imap_port", "mail.imap_security", "mail.imap_user"],
    },
  ],
  app: [
    { title: "外观", note: "切换后立即应用到当前窗口", keys: ["ui.theme"] },
    {
      title: "窗口与启动",
      note: "开机自启会写入系统启动项，可随时关闭",
      keys: ["tray.minimize_on_close", "startup.auto_launch", "global_shortcut"],
    },
  ],
  secret: [],
  security: [],
  about: [],
};

const LABELS: Record<string, string> = {
  "ollama.base_url": "服务地址",
  "ollama.model": "模型名称",
  "ollama.timeout_ms": "请求超时",
  "sidecar.url": "模型网关地址",
  "agent.max_iterations": "最大执行轮次",
  "agent.temperature": "模型温度",
  "agent.num_ctx": "上下文窗口",
  "agent.num_predict": "单次回复上限",
  "agent.proactive_interval_sec": "检查间隔",
  "agent.proactive_cooldown_sec": "触发冷却期",
  "agent.scheduler_enabled": "启用定时任务",
  "agent.scheduler_tick_sec": "调度检查间隔",
  "agent.scheduler_job_timeout_sec": "单个任务超时",
  "agent.gateway_enabled": "启用多端网关",
  "agent.gateway_bind": "绑定地址",
  "agent.gateway_port": "网关端口",
  "agent.gateway_reply_timeout_sec": "远端回复超时",
  "ui.theme": "界面主题",
  "tray.minimize_on_close": "关闭按钮最小化到托盘",
  "startup.auto_launch": "开机自动启动",
  "global_shortcut": "全局快捷键",
  "security.confirm_medium_risk": "MEDIUM 风险需弹窗确认",
  "image.base_url": "服务地址",
  "image.model": "图像模型",
  "image.size": "默认尺寸",
  "image.timeout_ms": "请求超时",
  "video.base_url": "服务地址",
  "video.model": "视频模型",
  "video.size": "默认分辨率",
  "video.duration_sec": "默认时长",
  "video.poll_timeout_ms": "最长等待",
  "mail.smtp_host": "服务器",
  "mail.smtp_port": "端口",
  "mail.smtp_security": "加密方式",
  "mail.smtp_user": "账号",
  "mail.from": "发件人地址",
  "mail.from_name": "发件人显示名",
  "mail.imap_host": "服务器",
  "mail.imap_port": "端口",
  "mail.imap_security": "加密方式",
  "mail.imap_user": "账号",
};

const SELECT_OPTIONS: Record<string, { value: string; label: string }[]> = {
  "ui.theme": [
    { value: "dark", label: "深色" },
    { value: "light", label: "浅色" },
  ],
  "mail.smtp_security": [
    { value: "tls", label: "TLS（465）" },
    { value: "starttls", label: "STARTTLS（587）" },
    { value: "none", label: "不加密" },
  ],
  "mail.imap_security": [
    { value: "tls", label: "TLS（993）" },
    { value: "starttls", label: "STARTTLS（143）" },
    { value: "none", label: "不加密" },
  ],
  "agent.gateway_bind": [
    { value: "127.0.0.1", label: "仅本机 127.0.0.1" },
    { value: "0.0.0.0", label: "局域网 0.0.0.0" },
  ],
};

type SecretSpec = { key: string; label: string; desc: string; placeholder: string };

const SECRETS: SecretSpec[] = [
  {
    key: "image.api_key",
    label: "图像生成 · API Key",
    desc: "填了才启用图像生成。任何 OpenAI 兼容的图像服务都行，把 Key 粘到这里即可。",
    placeholder: "粘贴图像服务 API Key",
  },
  {
    key: "video.api_key",
    label: "视频生成 · API Key",
    desc: "填了才启用视频生成。服务需兼容 OpenAI 的异步任务形状：POST /videos、GET /videos/{id}。",
    placeholder: "粘贴视频服务 API Key",
  },
  {
    key: "mail.smtp_password",
    label: "邮件发信 · 密码 / 授权码",
    desc: "QQ、163、Gmail、Outlook 都需要网页端开启 SMTP 后生成的授权码，不是登录密码。",
    placeholder: "粘贴 SMTP 授权码",
  },
  {
    key: "mail.imap_password",
    label: "邮件收件 · 密码 / 授权码",
    desc: "留空会自动复用上面的发信授权码；只有收发用不同凭据时才需要单独填。",
    placeholder: "留空则复用发信授权码",
  },
  {
    key: "publish.github_token",
    label: "网页发布 · GitHub Token",
    desc: "用于把生成的网页发布到 GitHub Pages。需要 repo 权限。",
    placeholder: "ghp_…",
  },
];

/** 后端对凭据只下发掩码，空串表示未配置 */
function isConfiguredSecret(value: string): boolean {
  return value.length > 0;
}

function secretTail(masked: string): string {
  const tail = masked.slice(-4);
  return /[•]/.test(tail) ? "不可辨认" : tail;
}

// ============================================================
// 页面
// ============================================================

export default function Settings() {
  const [settings, setSettings] = useState<SettingItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [savingKey, setSavingKey] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [fieldHint, setFieldHint] = useState<Record<string, { kind: "ok" | "error"; text: string }>>({});
  const [secretDrafts, setSecretDrafts] = useState<Record<string, string>>({});
  const [revealed, setRevealed] = useState<Record<string, boolean>>({});

  const [allowedDirs, setAllowedDirs] = useState<AllowedDirectory[]>([]);
  const [trustedTools, setTrustedTools] = useState<TrustedTool[]>([]);
  const [securityError, setSecurityError] = useState<string | null>(null);
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [newDirLabel, setNewDirLabel] = useState("");

  const [section, setSection] = useState<SectionId>("model");

  useEffect(() => {
    loadSettings();
    loadSecurity();
    invoke<boolean | null>("check_autostart_status")
      .then(setAutostart)
      .catch(() => setAutostart(null));
  }, []);

  async function loadSettings() {
    try {
      const data = await invoke<SettingItem[]>("get_settings");
      setSettings(data);
      setSaveError(null);
    } catch (e) {
      setSaveError(typeof e === "string" ? e : "加载设置失败");
    } finally {
      setLoading(false);
    }
  }

  async function loadSecurity() {
    try {
      const [dirs, tools] = await Promise.all([
        invoke<AllowedDirectory[]>("get_allowed_directories"),
        invoke<TrustedTool[]>("get_trusted_tools"),
      ]);
      setAllowedDirs(dirs);
      setTrustedTools(tools);
      setSecurityError(null);
    } catch (e) {
      setSecurityError(typeof e === "string" ? e : "加载安全数据失败");
    }
  }

  async function handleSave(key: string, value: string): Promise<boolean> {
    setSavingKey(key);
    setSaveError(null);
    try {
      await invoke("update_setting", { key, value });
      setSettings((prev) => prev.map((s) => (s.key === key ? { ...s, value } : s)));
      setFieldHint((prev) => ({ ...prev, [key]: { kind: "ok", text: "已保存" } }));

      if (key === "ui.theme") await emit("theme:changed", value);
      // 这两项后端会做夹取/回退，重新拉一次让界面显示真正生效的值
      if (key === "global_shortcut" || key === "ollama.timeout_ms") {
        setSettings(await invoke<SettingItem[]>("get_settings"));
      }

      window.setTimeout(() => {
        setFieldHint((prev) => {
          if (prev[key]?.kind !== "ok") return prev;
          const next = { ...prev };
          delete next[key];
          return next;
        });
      }, 2000);
      return true;
    } catch (e) {
      const text = typeof e === "string" ? e : `保存 ${key} 失败`;
      setFieldHint((prev) => ({ ...prev, [key]: { kind: "error", text } }));
      setSaveError(text);
      return false;
    } finally {
      setSavingKey(null);
    }
  }

  function patchLocal(key: string, value: string) {
    setSettings((prev) => prev.map((s) => (s.key === key ? { ...s, value } : s)));
  }

  async function saveSecret(key: string) {
    const draft = (secretDrafts[key] ?? "").trim();
    if (!draft) return;
    const ok = await handleSave(key, draft);
    if (ok) {
      setSecretDrafts((prev) => ({ ...prev, [key]: "" }));
      toast.success("凭据已保存", { description: "只以掩码形式回显，明文不会出现在界面上" });
    }
  }

  async function addDirectory() {
    try {
      const selected = await open({ directory: true, multiple: false, title: "选择可信目录" });
      if (selected && typeof selected === "string") {
        await invoke("add_allowed_directory", {
          path: selected,
          label: newDirLabel.trim() || "自定义目录",
          accessLevel: "TRUSTED",
        });
        setNewDirLabel("");
        await loadSecurity();
      }
    } catch (e) {
      setSecurityError(typeof e === "string" ? e : "添加目录失败");
    }
  }

  async function removeDirectory(path: string) {
    if (!confirm(`移除可信目录 "${path}"？之后该目录下的写入会重新走确认。`)) return;
    try {
      await invoke("remove_allowed_directory", { path });
      await loadSecurity();
    } catch (e) {
      setSecurityError(typeof e === "string" ? e : "删除目录失败");
    }
  }

  async function removeTrustedTool(toolName: string, pathPattern: string | null) {
    if (!confirm(`撤销对 "${toolName}" 的信任？`)) return;
    try {
      await invoke("remove_trusted_tool", { toolName, pathPattern });
      await loadSecurity();
    } catch (e) {
      setSecurityError(typeof e === "string" ? e : "撤销失败");
    }
  }

  const index = useMemo(() => new Map(settings.map((s) => [s.key, s])), [settings]);

  function renderField(item: SettingItem) {
    const hint = fieldHint[item.key];
    const isSaving = savingKey === item.key;
    const label = LABELS[item.key] ?? item.key;
    const options = SELECT_OPTIONS[item.key];

    let control: React.ReactNode;
    if (options) {
      control = (
        <Select
          value={item.value}
          onValueChange={async (v) => {
            patchLocal(item.key, v);
            await handleSave(item.key, v);
          }}
        >
          <SelectTrigger>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {options.map((o) => (
              <SelectItem key={o.value} value={o.value}>
                {o.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      );
    } else if (item.value_type === "bool") {
      const checked = item.value === "true";
      control = (
        <div className="flex justify-end">
          <Switch
            checked={checked}
            disabled={isSaving}
            onCheckedChange={async (v) => {
              patchLocal(item.key, v ? "true" : "false");
              await handleSave(item.key, v ? "true" : "false");
            }}
          />
        </div>
      );
    } else if (item.value_type === "number") {
      control = (
        <div className="relative">
          <Input
            type="number"
            className="num-tabular pr-10"
            value={item.value}
            min={item.min_value ?? undefined}
            max={item.max_value ?? undefined}
            onChange={(e) => patchLocal(item.key, e.target.value)}
            onBlur={(e) => {
              if (e.target.value !== item.value) handleSave(item.key, e.target.value);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") (e.target as HTMLInputElement).blur();
            }}
          />
          {item.unit && (
            <span className="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
              {item.unit}
            </span>
          )}
        </div>
      );
    } else {
      control = (
        <Input
          value={item.value}
          onChange={(e) => patchLocal(item.key, e.target.value)}
          onBlur={(e) => {
            if (e.target.value !== item.value) handleSave(item.key, e.target.value);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") (e.target as HTMLInputElement).blur();
          }}
        />
      );
    }

    return (
      <div
        key={item.key}
        className="grid grid-cols-[minmax(0,1fr)_minmax(140px,260px)] items-start gap-4 border-b border-[var(--color-line-faint)] py-2.5 last:border-0"
      >
        <div className="min-w-0 pt-1">
          <div className="flex items-center gap-1.5">
            <span className="text-[var(--text-xs)] font-medium text-[var(--color-foreground)]">{label}</span>
            {hint && (
              <span
                className={cn(
                  "text-[var(--text-2xs)]",
                  hint.kind === "ok" ? "text-[var(--color-success)]" : "text-[var(--color-danger)]"
                )}
              >
                {hint.text}
              </span>
            )}
            {isSaving && <Loader2 className="size-3 animate-ds-spin text-[var(--color-subtle-foreground)]" />}
          </div>
          <div className="mt-0.5 truncate font-mono text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
            {item.key}
            {item.description ? ` · ${item.description}` : ""}
          </div>
        </div>
        <div>{control}</div>
      </div>
    );
  }

  return (
    <Page wide>
      <PageHeader
        title="设置"
        description="改完即生效的会立即应用；标了「需重启」的要重开应用。凭据只以掩码回显，明文不落界面。"
      />

      <PageBody padded={false} className="pb-8">
        <div className="flex h-full gap-5 px-6 md:px-8">
          {/* 分类导航 */}
          <nav className="w-44 shrink-0 pt-1">
            {SECTIONS.map((s) => {
              const Icon = s.icon;
              const active = section === s.id;
              return (
                <button
                  key={s.id}
                  onClick={() => setSection(s.id)}
                  className={cn(
                    "mb-0.5 flex w-full cursor-pointer items-center gap-2 rounded-[8px] px-2.5 py-1.5 text-left",
                    "text-[var(--text-xs)] transition-colors",
                    active
                      ? "bg-[var(--color-elevated)] font-medium text-[var(--color-foreground)]"
                      : "text-[var(--color-muted-foreground)] hover:bg-[var(--color-hover)]/60 hover:text-[var(--color-foreground)]"
                  )}
                >
                  <Icon className={cn("size-3.5 shrink-0", active && "text-[var(--color-brand)]")} />
                  {s.label}
                </button>
              );
            })}
          </nav>

          {/* 内容 */}
          <div className="min-w-0 flex-1">
            {saveError && (
              <Card className="mb-3 border-[var(--color-danger)]/30 p-3">
                <div className="flex items-start gap-2">
                  <X className="mt-0.5 size-3.5 shrink-0 text-[var(--color-danger)]" />
                  <p className="min-w-0 flex-1 break-words text-[var(--text-xs)] text-[var(--color-danger)]">{saveError}</p>
                  <Button variant="ghost" size="xs" onClick={() => setSaveError(null)}>
                    关闭
                  </Button>
                </div>
              </Card>
            )}

            {loading ? (
              <LoadingBlock label="读取配置" />
            ) : section === "secret" ? (
              <div className="flex flex-col gap-3">
                <Card>
                  <SectionHeader
                    className="px-4 pt-4"
                    title="服务凭据"
                    description="这是切换服务商的唯一入口：换一家只改地址 + 模型 + 这里粘一个新 Key"
                  />
                  <div className="px-4 pb-4 pt-1">
                    {SECRETS.map((s) => {
                      const item = index.get(s.key);
                      const configured = item ? isConfiguredSecret(item.value) : false;
                      const draft = secretDrafts[s.key] ?? "";
                      const show = revealed[s.key];
                      return (
                        <div key={s.key} className="border-b border-[var(--color-line-faint)] py-3.5 last:border-0">
                          <div className="flex items-center gap-2">
                            <span className="text-[var(--text-xs)] font-medium text-[var(--color-foreground)]">
                              {s.label}
                            </span>
                            {configured ? (
                              <Badge tone="success" dot>
                                已配置 · 尾号 {item ? secretTail(item.value) : ""}
                              </Badge>
                            ) : (
                              <Badge tone="neutral">未配置</Badge>
                            )}
                          </div>
                          <p className="mt-1 text-[var(--text-2xs)] leading-relaxed text-[var(--color-muted-foreground)]">
                            {s.desc}
                          </p>
                          <div className="mt-2 flex items-center gap-2">
                            <div className="relative flex-1">
                              <Input
                                type={show ? "text" : "password"}
                                value={draft}
                                placeholder={configured ? "留空表示不修改，填新值会覆盖" : s.placeholder}
                                onChange={(e) =>
                                  setSecretDrafts((prev) => ({ ...prev, [s.key]: e.target.value }))
                                }
                                onKeyDown={(e) => {
                                  if (e.key === "Enter") saveSecret(s.key);
                                }}
                                className="pr-8 font-mono text-[var(--text-2xs)]"
                              />
                              <button
                                onClick={() => setRevealed((prev) => ({ ...prev, [s.key]: !show }))}
                                className="absolute right-2 top-1/2 -translate-y-1/2 text-[var(--color-subtle-foreground)] hover:text-[var(--color-foreground)]"
                                title={show ? "隐藏" : "显示"}
                              >
                                {show ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
                              </button>
                            </div>
                            <Button
                              size="sm"
                              variant={configured ? "secondary" : "primary"}
                              disabled={!draft.trim()}
                              loading={savingKey === s.key}
                              onClick={() => saveSecret(s.key)}
                            >
                              保存
                            </Button>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                </Card>
                <p className="flex items-center gap-1.5 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                  <ShieldCheck className="size-3" />
                  凭据存入本机数据库，只在下发时打码；不会出现在工具返回值或日志里
                </p>
              </div>
            ) : section === "security" ? (
              <div className="flex flex-col gap-3">
                <Card>
                  <SectionHeader
                    className="px-4 pt-4"
                    title="风险确认策略"
                    description="MEDIUM 指会产生真实副作用（写入文件 / 计费 / 对外通信）的操作"
                  />
                  <div className="px-4 pb-4 pt-1">
                    {index.get("security.confirm_medium_risk") &&
                      renderField(index.get("security.confirm_medium_risk")!)}
                  </div>
                </Card>

                <Card>
                  <SectionHeader
                    className="px-4 pt-4"
                    title="可信目录"
                    description="这些目录下的读写不再逐次确认。目录越大，被误改的面积越大"
                    action={
                      <Button size="xs" variant="secondary" onClick={addDirectory} className="gap-1">
                        <FolderPlus className="size-3" />
                        添加
                      </Button>
                    }
                  />
                  <div className="px-4 pb-4 pt-1">
                    {securityError && (
                      <p className="mb-2 text-[var(--text-2xs)] text-[var(--color-danger)]">{securityError}</p>
                    )}
                    {allowedDirs.length === 0 ? (
                      <p className="py-3 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                        还没有可信目录。所有写入都会逐次询问。
                      </p>
                    ) : (
                      allowedDirs.map((d) => (
                        <div
                          key={d.path}
                          className="flex items-center gap-2 border-b border-[var(--color-line-faint)] py-2 last:border-0"
                        >
                          <Badge tone={d.access_level === "TRUSTED" ? "success" : "warning"}>{d.access_level}</Badge>
                          <div className="min-w-0 flex-1">
                            <div className="truncate text-[var(--text-xs)]">{d.label || d.path}</div>
                            <div className="truncate font-mono text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                              {d.path}
                            </div>
                          </div>
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            className="text-[var(--color-danger)]"
                            onClick={() => removeDirectory(d.path)}
                          >
                            <Trash2 className="size-3.5" />
                          </Button>
                        </div>
                      ))
                    )}
                    <div className="mt-2 flex items-center gap-2">
                      <Input
                        value={newDirLabel}
                        onChange={(e) => setNewDirLabel(e.target.value)}
                        placeholder="目录备注（可选），例如：项目代码"
                        className="text-[var(--text-2xs)]"
                      />
                    </div>
                  </div>
                </Card>

                <Card>
                  <SectionHeader
                    className="px-4 pt-4"
                    title="已信任的工具"
                    description="勾选「以后都允许」后记录在这里。撤销后下次会重新询问"
                  />
                  <div className="px-4 pb-4 pt-1">
                    {trustedTools.length === 0 ? (
                      <p className="py-3 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                        还没有被永久信任的工具
                      </p>
                    ) : (
                      trustedTools.map((t) => (
                        <div
                          key={`${t.tool_name}-${t.path_pattern ?? ""}`}
                          className="flex items-center gap-2 border-b border-[var(--color-line-faint)] py-2 last:border-0"
                        >
                          <span className="min-w-0 flex-1 truncate font-mono text-[var(--text-xs)]">
                            {t.tool_name}
                          </span>
                          <Badge tone={t.risk_level === "HIGH" ? "danger" : t.risk_level === "MEDIUM" ? "warning" : "success"}>
                            {t.risk_level}
                          </Badge>
                          <span className="hidden max-w-40 truncate font-mono text-[var(--text-2xs)] text-[var(--color-subtle-foreground)] sm:block">
                            {t.path_pattern ?? "全部路径"}
                          </span>
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            className="text-[var(--color-danger)]"
                            onClick={() => removeTrustedTool(t.tool_name, t.path_pattern)}
                          >
                            <Trash2 className="size-3.5" />
                          </Button>
                        </div>
                      ))
                    )}
                  </div>
                </Card>
              </div>
            ) : section === "about" ? (
              <div className="flex flex-col gap-3">
                <Card>
                  <SectionHeader className="px-4 pt-4" title="运行信息" description="全部来自本机，不上云" />
                  <div className="px-4 pb-4 pt-1">
                    <KeyValue label="产品">E-Assisant</KeyValue>
                    <KeyValue label="版本" mono>0.1.0</KeyValue>
                    <KeyValue label="内核">Rust + Tauri 2</KeyValue>
                    <KeyValue label="数据位置" mono>
                      本机 SQLite
                    </KeyValue>
                    <KeyValue label="开机自启">
                      {autostart === null ? (
                        <Badge tone="neutral">未知</Badge>
                      ) : autostart ? (
                        <Badge tone="success" dot>
                          已启用
                        </Badge>
                      ) : (
                        <Badge tone="neutral">未启用</Badge>
                      )}
                    </KeyValue>
                    <Divider className="my-3" />
                    <div className="flex items-center gap-2">
                      <Button
                        variant="secondary"
                        size="sm"
                        onClick={() => {
                          invoke<SettingItem[]>("get_settings").then(setSettings);
                          loadSecurity();
                          toast.success("已重新读取配置");
                        }}
                        className="gap-1.5"
                      >
                        <RotateCcw className="size-3.5" />
                        重新读取
                      </Button>
                      <span className="text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                        若外部改过配置文件，用它同步
                      </span>
                    </div>
                  </div>
                </Card>
                <Card>
                  <SectionHeader className="px-4 pt-4" title="设计" description="界面语言与交互约定" />
                  <div className="px-4 pb-4 pt-1 space-y-1.5">
                    {[
                      "一个界面里只有一个主操作按钮，其余全部中性",
                      "有副作用的操作必须能说清副作用落在哪",
                      "模型说成功不算数，账本里有回执才算",
                      "留白优先：宁可少放一张卡，也不挤",
                    ].map((t) => (
                      <p key={t} className="flex items-start gap-1.5 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                        <Check className="mt-0.5 size-3 shrink-0 text-[var(--color-success)]" />
                        {t}
                      </p>
                    ))}
                  </div>
                </Card>
              </div>
            ) : (
              <div className="flex flex-col gap-3">
                {GROUPS[section].map((group) => (
                  <Card key={group.title}>
                    <SectionHeader className="px-4 pt-4" title={group.title} description={group.note} />
                    <div className="px-4 pb-3 pt-1">
                      {group.keys
                        .map((k) => index.get(k))
                        .filter((x): x is SettingItem => !!x)
                        .map(renderField)}
                    </div>
                  </Card>
                ))}
              </div>
            )}
          </div>
        </div>
      </PageBody>
    </Page>
  );
}
