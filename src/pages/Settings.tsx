import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { listen, emit, UnlistenFn } from "@tauri-apps/api/event";
import CyberIcon from "../components/CyberIcon";
import SettingsAutomation from "./SettingsAutomation";
import SettingsModel from "./SettingsModel";

interface SettingItem {
  key: string;
  value: string;
  value_type: string;
  description: string | null;
  /** 数值型配置的合法下界；null 表示无约束 */
  min_value: number | null;
  /** 数值型配置的合法上界；null 表示无约束 */
  max_value: number | null;
  /** 数值单位（%、ms、s、token…），用于界面展示 */
  unit: string | null;
}

interface AllowedDirectory {
  id: number;
  path: string;
  label: string | null;
  access_level: string;
  is_default: boolean;
  added_at: string;
}

interface TrustedTool {
  id: number;
  tool_name: string;
  path_pattern: string | null;
  risk_level: string;
  added_at: string;
}

// V18: Proactive 规则类型
interface ProactiveRule {
  id: string;
  name: string;
  trigger_type: string;
  trigger_config: string;
  action_type: string;
  action_config: string;
  enabled: boolean;
  last_triggered: string | null;
  trigger_count: number;
  created_at: string;
}

interface ProactiveStatus {
  running: boolean;
  rules_total: number;
  rules_enabled: number;
  last_check: string | null;
  checks_count: number;
  triggers_count: number;
}

interface ProactiveNotification {
  rule_id: string;
  rule_name: string;
  title: string;
  message: string;
  level: string;
  context: string;
  timestamp: string;
}

// V20: 技能信息
interface SkillInfo {
  id: string;
  name: string;
  description: string;
  version: string;
  author: string;
  skill_type: string;
  enabled: boolean;
  installed_at: string;
  permissions: string[];
  triggers: string[];
}

type TabType =
  | "general"
  | "model"
  | "security"
  | "proactive"
  | "automation"
  | "browser"
  | "skills"
  | "memory";

/**
 * 通用设置的分组定义。
 *
 * 为什么硬编码分组而不是按 key 前缀自动分组：分组标题和说明本身是产品文案，
 * 需要"给谁看的、在什么场景下改"这种语境，自动生成只会得到
 * "ollama.*" 这样对用户毫无意义的标题。
 *
 * 未出现在任何分组里的键会在界面上被忽略——这正好让"后端新增了配置
 * 但前端还没想好怎么呈现"保持静默，而不是冒出个裸键给用户看。
 */
const SETTING_GROUPS: { title: string; note: string; keys: string[] }[] = [
  {
    title: "模型连接",
    note: "服务地址与模型名在启动时确定，改动需重启应用；超时立即生效",
    keys: ["ollama.base_url", "ollama.model", "ollama.timeout_ms", "sidecar.url"],
  },
  {
    title: "生成参数",
    note: "均立即生效，从下一条消息开始应用",
    keys: [
      "agent.max_iterations",
      "agent.temperature",
      "agent.num_ctx",
      "agent.num_predict",
    ],
  },
  {
    title: "主动助手",
    note: "均立即生效，下一轮巡检即按新频率执行",
    keys: ["agent.proactive_interval_sec", "agent.proactive_cooldown_sec"],
  },
  {
    title: "常驻调度",
    note: "定时任务到点自动执行；间隔与超时改动下一轮生效",
    keys: [
      "agent.scheduler_enabled",
      "agent.scheduler_tick_sec",
      "agent.scheduler_job_timeout_sec",
    ],
  },
  {
    title: "多端访问",
    note:
      "默认只允许本机（127.0.0.1）。要手机/平板连，把绑定地址改成 0.0.0.0，" +
      "并确保局域网可信——配对码等同于授权凭据，别截图外发",
    keys: [
      "agent.gateway_enabled",
      "agent.gateway_bind",
      "agent.gateway_port",
      "agent.gateway_reply_timeout_sec",
    ],
  },
  {
    title: "外观",
    note: "切换后立即应用到当前窗口",
    keys: ["ui.theme"],
  },
  {
    title: "窗口与启动",
    note: "均立即生效；开机自启会写入系统启动项，可随时关闭",
    keys: [
      "tray.minimize_on_close",
      "startup.auto_launch",
      "global_shortcut",
    ],
  },
  {
    title: "安全",
    note: "立即生效，下一条待确认的操作即按新策略处理",
    keys: ["security.confirm_medium_risk"],
  },
];

/** 配置键到中文标签的映射；没登记的就直接显示原键名 */
const SETTING_LABELS: Record<string, string> = {
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
  "security.confirm_medium_risk": "MEDIUM 风险弹窗确认",
};

/** 含密钥的发布配置：用密码框渲染，避免明文暴露 */
const PUBLISH_SECRET_KEYS = [
  {
    key: "publish.github_token",
    label: "网页发布 · GitHub 访问令牌",
    desc:
      "填了它，网页工坊每生成一版就自动发布到 GitHub Pages，预览区顶部直接显示网址。" +
      "到 github.com → Settings → Developer settings → Personal access tokens（Tokens classic）" +
      "→ Generate new token，勾选 repo，生成后只显示一次，粘贴到这里。留空则不发布。",
    placeholder: "粘贴 Personal access token",
  },
  {
    key: "publish.github_repo",
    label: "网页发布 · GitHub 仓库名",
    desc:
      "网页发布到哪个仓库，不存在会自动创建（公开仓库才能免费用 Pages）。" +
      "仓库名不变，网址就不会变。留空则用默认的 pc-guardian-web。",
    placeholder: "pc-guardian-web",
  },
  {
    key: "publish.netlify_token",
    label: "网页发布 · Netlify 令牌（可选）",
    desc:
      "备用发布通道，只在上面没填 GitHub 令牌时生效。到 app.netlify.com → User settings" +
      " → Applications → Personal access tokens 生成一个（只显示一次）粘贴到这里。",
    placeholder: "粘贴 Personal access token",
  },
  {
    key: "publish.proxy",
    label: "网页发布 · 代理地址（可选）",
    desc:
      "生成时报「连接 GitHub 失败」时填这里。留空则自动使用系统代理；填了就以它为准，" +
      "例如 Clash 的 http://127.0.0.1:7890。只影响发布，不影响本机模型。",
    placeholder: "http://127.0.0.1:7890",
  },
];

/**
 * 后端对凭据类配置只下发掩码（形如 ••••••••1234），空串表示未配置。
 * 这里据此判断"填没填过"——不依赖明文，因为前端根本拿不到明文。
 */
function isConfiguredSecret(value: string): boolean {
  return value.length > 0;
}

/** 从掩码里取出末四位，供用户辨认是哪一个凭据 */
function secretTail(masked: string): string {
  const tail = masked.slice(-4);
  // 短值会被后端整体打码，取不出可辨认的尾巴，就别硬凑
  return /[•]/.test(tail) ? "不可辨认" : tail;
}

// V21: 记忆管理类型
interface MemoryItem {
  id: string;
  memory_type: string;
  content: string;
  keywords?: string;
  importance: number;
  access_count: number;
  created_at: string;
  last_accessed_at: string;
  source: string;
}

/**
 * 设置页面
 *
 * 支持查看和修改配置项。
 * Phase 4: 新增安全设置（可信目录、信任工具管理）。
 * V18: 新增主动助手（Proactive 规则管理）。
 */
function Settings() {
  const [settings, setSettings] = useState<SettingItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [savingKey, setSavingKey] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  /** 每个配置项最近一次的校验/保存提示，成功与失败都记，供行内展示 */
  const [fieldHint, setFieldHint] = useState<Record<string, { kind: "ok" | "error"; text: string }>>({});
  /** 开机自启在系统里的真实状态；null 表示"还没查或查不到" */
  const [autostartActual, setAutostartActual] = useState<boolean | null>(null);
  /** 是否正在检测自启状态 */
  const [autostartChecking, setAutostartChecking] = useState(false);
  /** 是否尝试过检测但没查成（用于区分"未查"与"查不到"） */
  const [autostartProbeFailed, setAutostartProbeFailed] = useState(false);
  /**
   * 凭据输入框的草稿值。
   *
   * 后端只下发掩码、不下发明文，所以输入框永远是空的，用户输入的内容
   * 暂存在这里。提交成功后立即清空，保持"框里没东西 = 没在编辑"的语义。
   */
  const [secretDrafts, setSecretDrafts] = useState<Record<string, string>>({});

  // Phase 4: 安全设置状态
  const [allowedDirectories, setAllowedDirectories] = useState<AllowedDirectory[]>([]);
  const [trustedTools, setTrustedTools] = useState<TrustedTool[]>([]);
  const [securityLoading, setSecurityLoading] = useState(true);
  const [securityError, setSecurityError] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<TabType>("general");
  const [newDirLabel, setNewDirLabel] = useState("");

  // V18: Proactive 状态
  const [proactiveRules, setProactiveRules] = useState<ProactiveRule[]>([]);
  const [proactiveStatus, setProactiveStatus] = useState<ProactiveStatus | null>(null);
  const [proactiveLoading, setProactiveLoading] = useState(true);
  const [proactiveError, setProactiveError] = useState<string | null>(null);
  const [notifications, setNotifications] = useState<ProactiveNotification[]>([]);
  const [showAddForm, setShowAddForm] = useState(false);
  const [newRuleName, setNewRuleName] = useState("");
  const [newRuleMetric, setNewRuleMetric] = useState("memory");
  const [newRuleThreshold, setNewRuleThreshold] = useState("85");
  const [newRuleMessage, setNewRuleMessage] = useState("");
  /** 正在内联编辑阈值的规则 ID */
  const [editingRuleId, setEditingRuleId] = useState<string | null>(null);
  /** 编辑中的阈值草稿 */
  const [ruleThresholdDraft, setRuleThresholdDraft] = useState("");
  /** 规则编辑的报错，按规则 ID 记录 */
  const [ruleEditError, setRuleEditError] = useState<Record<string, string>>({});

  // V20: 技能管理状态
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [skillsLoading, setSkillsLoading] = useState(false);
  const [skillsError, setSkillsError] = useState<string | null>(null);
  const [selectedSkill, setSelectedSkill] = useState<SkillInfo | null>(null);

  // V21: 记忆管理状态
  const [memories, setMemories] = useState<MemoryItem[]>([]);
  const [memoriesLoading, setMemoriesLoading] = useState(false);
  const [memoriesError, setMemoriesError] = useState<string | null>(null);
  const [memorySearch, setMemorySearch] = useState("");
  const [memoryFilter, setMemoryFilter] = useState("all");
  const [showAddMemory, setShowAddMemory] = useState(false);
  const [newMemoryContent, setNewMemoryContent] = useState("");
  const [newMemoryType, setNewMemoryType] = useState("user_preference");
  const [newMemoryImportance, setNewMemoryImportance] = useState("50");

  useEffect(() => {
    loadSettings();
    loadSecurityData();
    loadProactiveData();

    // V18: 监听主动通知事件
    let unlisten: UnlistenFn | null = null;
    listen("proactive:notification", (event) => {
      try {
        const notif: ProactiveNotification = JSON.parse(String(event.payload));
        setNotifications((prev) => [notif, ...prev].slice(0, 20));
      } catch (e) {
        console.error("Failed to parse proactive notification:", e);
      }
    }).then((u) => { unlisten = u; });

    return () => { if (unlisten) unlisten(); };
  }, []);

  async function loadSettings() {
    try {
      const data = await invoke<SettingItem[]>("get_settings");
      setSettings(data);
      setSaveError(null);
    } catch (e) {
      console.error("Failed to load settings:", e);
      setSaveError(typeof e === "string" ? e : "加载设置失败");
    } finally {
      setLoading(false);
    }
  }

  async function loadSecurityData() {
    try {
      const [dirs, tools] = await Promise.all([
        invoke<AllowedDirectory[]>("get_allowed_directories"),
        invoke<TrustedTool[]>("get_trusted_tools"),
      ]);
      setAllowedDirectories(dirs);
      setTrustedTools(tools);
      setSecurityError(null);
    } catch (e) {
      console.error("Failed to load security data:", e);
      setSecurityError(typeof e === "string" ? e : "加载安全数据失败");
    } finally {
      setSecurityLoading(false);
    }
  }

  // V18: 加载 Proactive 数据
  async function loadProactiveData() {
    try {
      const [rules, status] = await Promise.all([
        invoke<ProactiveRule[]>("get_proactive_rules"),
        invoke<ProactiveStatus>("get_proactive_status"),
      ]);
      setProactiveRules(rules);
      setProactiveStatus(status);
      setProactiveError(null);
    } catch (e) {
      console.error("Failed to load proactive data:", e);
      setProactiveError(typeof e === "string" ? e : "加载主动助手数据失败");
    } finally {
      setProactiveLoading(false);
    }
  }

  /**
   * 保存单个配置项。
   *
   * 返回是否成功——调用方（尤其是凭据输入框）靠它决定要不要清空草稿：
   * 保存失败时草稿必须留着，否则用户刚敲进去的令牌会被清掉，
   * 而这令牌他往往已经没法从别处再复制一份（GitHub 只显示一次）。
   */
  async function handleSave(key: string, newValue: string): Promise<boolean> {
    setSavingKey(key);
    setSaveError(null);
    try {
      await invoke("update_setting", { key, value: newValue });
      // 保存成功：把本地状态对齐到真正落库的值（后端可能做规范化）
      setSettings((prev) =>
        prev.map((s) => (s.key === key ? { ...s, value: newValue } : s))
      );
      setFieldHint((prev) => ({ ...prev, [key]: { kind: "ok", text: "已保存" } }));

      // 主题变化要立刻作用到整个界面，不必刷新。
      if (key === "ui.theme") {
        await emit("theme:changed", newValue);
      }

      // 快捷键保存后后端可能改写实际生效值（用户填的键被占用时走回退链，
      // 会换成备用键）。这里重新拉一次，让界面显示系统里真正生效的那个，
      // 而不是用户填了但没抢到的那个。
      if (key === "global_shortcut") {
        const fresh = await invoke<SettingItem[]>("get_settings");
        setSettings(fresh);
      }

      // 超时会被后端夹到 [5000, 300000]，重新拉一次避免界面值与实际不符。
      if (key === "ollama.timeout_ms") {
        const fresh = await invoke<SettingItem[]>("get_settings");
        setSettings(fresh);
      }

      // 2 秒后自动淡出，避免长期占位
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
      console.error("Failed to save setting:", e);
      const text = typeof e === "string" ? e : `保存 ${key} 失败`;
      // 校验失败是**可预期**的结果（比如超出区间），行内提示比顶部横幅更贴切，
      // 同时保留顶部横幅兜底，方便一眼看到。
      setFieldHint((prev) => ({ ...prev, [key]: { kind: "error", text } }));
      setSaveError(text);
      return false;
    } finally {
      setSavingKey(null);
    }
  }

  function handleChange(key: string, value: string) {
    setSettings((prev) =>
      prev.map((s) => (s.key === key ? { ...s, value } : s))
    );
  }

  async function handleAddDirectory() {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "选择可信目录",
      });
      if (selected && typeof selected === "string") {
        const label = newDirLabel.trim() || "自定义目录";
        await invoke("add_allowed_directory", {
          path: selected,
          label,
          access_level: "TRUSTED",
        });
        setNewDirLabel("");
        await loadSecurityData();
      }
    } catch (e) {
      console.error("Failed to add directory:", e);
      setSecurityError(typeof e === "string" ? e : "添加目录失败");
    }
  }

  async function handleRemoveDirectory(path: string) {
    if (!confirm(`确定要删除目录 "${path}" 吗？`)) return;
    try {
      await invoke("remove_allowed_directory", { path });
      await loadSecurityData();
    } catch (e) {
      console.error("Failed to remove directory:", e);
      setSecurityError(typeof e === "string" ? e : "删除目录失败");
    }
  }

  async function handleRemoveTrustedTool(toolName: string, pathPattern: string | null) {
    if (!confirm(`确定要撤销对 "${toolName}" 的信任吗？`)) return;
    try {
      await invoke("remove_trusted_tool", {
        tool_name: toolName,
        path_pattern: pathPattern,
      });
      await loadSecurityData();
    } catch (e) {
      console.error("Failed to remove trusted tool:", e);
      setSecurityError(typeof e === "string" ? e : "删除信任工具失败");
    }
  }

  // V18: Proactive 操作
  async function handleToggleRule(id: string, enabled: boolean) {
    try {
      await invoke("toggle_proactive_rule", { id, enabled });
      setProactiveRules((prev) =>
        prev.map((r) => (r.id === id ? { ...r, enabled } : r))
      );
    } catch (e) {
      console.error("Failed to toggle rule:", e);
    }
  }

  async function handleDeleteRule(id: string, name: string) {
    if (!confirm(`确定要删除规则 "${name}" 吗？`)) return;
    try {
      await invoke("delete_proactive_rule", { id });
      await loadProactiveData();
    } catch (e) {
      console.error("Failed to delete rule:", e);
    }
  }

  async function handleAddRule() {
    if (!newRuleName.trim()) {
      alert("请输入规则名称");
      return;
    }
    try {
      const triggerConfig = JSON.stringify({
        metric: newRuleMetric,
        threshold: parseFloat(newRuleThreshold) || 85,
        operator: "gt",
        duration_sec: 30,
      });
      const actionConfig = JSON.stringify({
        title: `<CyberIcon name="alertTriangle" size={14} /> ${newRuleName}`,
        message: newRuleMessage.trim() || `系统${newRuleMetric}使用率超过阈值，请注意。`,
        level: "warning",
      });
      await invoke("add_proactive_rule", {
        name: newRuleName.trim(),
        trigger_type: "system_metric",
        trigger_config: triggerConfig,
        action_type: "notify",
        action_config: actionConfig,
      });
      setNewRuleName("");
      setNewRuleThreshold("85");
      setNewRuleMessage("");
      setShowAddForm(false);
      await loadProactiveData();
    } catch (e) {
      console.error("Failed to add rule:", e);
      alert("添加规则失败: " + (typeof e === "string" ? e : String(e)));
    }
  }

  function getTriggerDescription(rule: ProactiveRule): string {
    try {
      const config = JSON.parse(rule.trigger_config);
      if (rule.trigger_type === "system_metric") {
        const op = config.operator === "lt" ? "<" : ">";
        const unit = config.metric === "disk" ? "GB" : "%";
        return `${config.metric.toUpperCase()} ${op} ${config.threshold}${unit}${config.duration_sec ? ` 持续${config.duration_sec}s` : ""}`;
      }
      return rule.trigger_type;
    } catch {
      return rule.trigger_type;
    }
  }

  function getActionDescription(rule: ProactiveRule): string {
    try {
      const config = JSON.parse(rule.action_config);
      if (rule.action_type === "notify") {
        return `通知: ${config.title}`;
      }
      return rule.action_type;
    } catch {
      return rule.action_type;
    }
  }

  /** 取出规则当前的阈值；非 system_metric 规则返回 null（不存在"阈值"概念） */
  function getRuleThreshold(rule: ProactiveRule): number | null {
    if (rule.trigger_type !== "system_metric") return null;
    try {
      const config = JSON.parse(rule.trigger_config);
      return typeof config.threshold === "number" ? config.threshold : null;
    } catch {
      return null;
    }
  }

  /** 阈值单位：内存/CPU 是百分比，磁盘是 GB */
  function getThresholdUnit(rule: ProactiveRule): string {
    try {
      const config = JSON.parse(rule.trigger_config);
      return config.metric === "disk" ? "GB" : "%";
    } catch {
      return "";
    }
  }

  function beginEditThreshold(rule: ProactiveRule) {
    const t = getRuleThreshold(rule);
    if (t === null) return;
    setEditingRuleId(rule.id);
    setRuleThresholdDraft(String(t));
    setRuleEditError((prev) => {
      const next = { ...prev };
      delete next[rule.id];
      return next;
    });
  }

  /**
   * 保存阈值。
   *
   * 实现方式是**读出原 trigger_config、只替换 threshold 字段，再整体写回**——
   * 而不是用界面上能看到的字段重建一份配置。这样 metric / operator / duration_sec /
   * drive 这些用户没在动的属性不会因为前端不了解就丢失。
   */
  async function saveThreshold(rule: ProactiveRule) {
    const parsed = Number(ruleThresholdDraft);
    const unit = getThresholdUnit(rule);

    if (!Number.isFinite(parsed) || ruleThresholdDraft.trim() === "") {
      setRuleEditError((prev) => ({ ...prev, [rule.id]: "请填一个数字" }));
      return;
    }
    if (parsed < 0 || parsed > 100) {
      setRuleEditError((prev) => ({
        ...prev,
        [rule.id]: unit === "%" ? "百分比需在 0–100 之间" : "数值需在 0–100 之间",
      }));
      return;
    }

    let triggerConfig: Record<string, unknown>;
    try {
      triggerConfig = JSON.parse(rule.trigger_config);
    } catch {
      setRuleEditError((prev) => ({ ...prev, [rule.id]: "规则配置已损坏，无法编辑" }));
      return;
    }
    triggerConfig.threshold = parsed;

    // 通知文案里通常写着旧阈值，一并替换掉，否则会出现
    // "内存使用率已超过 90%" 而实际阈值是 75% 的自相矛盾。
    let actionConfig = rule.action_config;
    try {
      const parsedAction = JSON.parse(rule.action_config);
      const oldThreshold = getRuleThreshold(rule);
      if (oldThreshold !== null && typeof parsedAction.message === "string") {
        parsedAction.message = parsedAction.message.replace(
          String(oldThreshold),
          String(parsed)
        );
      }
      actionConfig = JSON.stringify(parsedAction);
    } catch {
      // 动作配置解析失败就保持原样，不做无根据的改动
    }

    try {
      await invoke("update_proactive_rule", {
        id: rule.id,
        name: rule.name,
        triggerType: rule.trigger_type,
        triggerConfig: JSON.stringify(triggerConfig),
        actionType: rule.action_type,
        actionConfig,
        enabled: rule.enabled,
      });
      setEditingRuleId(null);
      await loadProactiveData();
    } catch (e) {
      setRuleEditError((prev) => ({
        ...prev,
        [rule.id]: typeof e === "string" ? e : String(e),
      }));
    }
  }

  // V20: 技能管理函数
  async function loadSkills() {
    setSkillsLoading(true);
    setSkillsError(null);
    try {
      const result = await invoke<SkillInfo[]>("get_skills");
      setSkills(result);
    } catch (e) {
      setSkillsError(typeof e === "string" ? e : String(e));
    } finally {
      setSkillsLoading(false);
    }
  }

  /**
   * 从本地目录安装技能。
   *
   * 后端 `install_skill` 命令一直存在，但界面上没有入口，
   * 导致技能页的说明"本地技能可从目录安装"成了一句空话。
   */
  async function handleInstallSkill() {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "选择技能目录（需含 SKILL.md）",
      });
      if (!selected || typeof selected !== "string") return;
      setSkillsLoading(true);
      await invoke("install_skill", { sourceDir: selected });
      await loadSkills();
    } catch (e) {
      setSkillsError(typeof e === "string" ? e : String(e));
    } finally {
      setSkillsLoading(false);
    }
  }

  async function handleToggleSkill(skillId: string, enabled: boolean) {
    try {
      await invoke("toggle_skill", { skillId, enabled });
      setSkills((prev) =>
        prev.map((s) => (s.id === skillId ? { ...s, enabled } : s))
      );
    } catch (e) {
      alert("切换技能状态失败: " + (typeof e === "string" ? e : String(e)));
    }
  }

  async function handleUninstallSkill(skillId: string, skillName: string) {
    if (!confirm(`确定要卸载技能「${skillName}」吗？`)) return;
    try {
      await invoke("uninstall_skill", { skillId });
      setSkills((prev) => prev.filter((s) => s.id !== skillId));
      if (selectedSkill?.id === skillId) setSelectedSkill(null);
    } catch (e) {
      alert("卸载失败: " + (typeof e === "string" ? e : String(e)));
    }
  }

  // V21: 记忆管理函数
  async function loadMemories() {
    setMemoriesLoading(true);
    setMemoriesError(null);
    try {
      const data = await invoke<MemoryItem[]>("get_memories", { limit: 100 });
      setMemories(data);
    } catch (e) {
      setMemoriesError(typeof e === "string" ? e : String(e));
    } finally {
      setMemoriesLoading(false);
    }
  }

  async function handleSearchMemories() {
    if (!memorySearch.trim()) {
      loadMemories();
      return;
    }
    setMemoriesLoading(true);
    setMemoriesError(null);
    try {
      const data = await invoke<MemoryItem[]>("search_memories", {
        query: memorySearch.trim(),
        limit: 50,
      });
      setMemories(data);
    } catch (e) {
      setMemoriesError(typeof e === "string" ? e : String(e));
    } finally {
      setMemoriesLoading(false);
    }
  }

  async function handleDeleteMemory(id: string, content: string) {
    const preview = content.length > 30 ? content.slice(0, 30) + "..." : content;
    if (!confirm(`确定要删除这条记忆吗？\n"${preview}"`)) return;
    try {
      await invoke("delete_memory", { memoryId: id });
      setMemories((prev) => prev.filter((m) => m.id !== id));
    } catch (e) {
      alert("删除记忆失败: " + (typeof e === "string" ? e : String(e)));
    }
  }

  async function handleAddMemory() {
    if (!newMemoryContent.trim()) {
      alert("请输入记忆内容");
      return;
    }
    try {
      await invoke("add_memory", {
        content: newMemoryContent.trim(),
        memoryType: newMemoryType,
        importance: parseInt(newMemoryImportance) || 50,
      });
      setNewMemoryContent("");
      setNewMemoryImportance("50");
      setShowAddMemory(false);
      loadMemories();
    } catch (e) {
      alert("添加记忆失败: " + (typeof e === "string" ? e : String(e)));
    }
  }

  function getMemoryTypeLabel(type: string): string {
    const labels: Record<string, string> = {
      user_preference: "用户偏好",
      task_history: "任务历史",
      knowledge: "知识",
      conversation_summary: "对话摘要",
      app_usage: "应用习惯",
    };
    return labels[type] || type;
  }

  function getImportanceColor(importance: number): string {
    if (importance >= 80) return "#ff3366";
    if (importance >= 60) return "#ffaa00";
    if (importance >= 40) return "#00d4ff";
    return "#666";
  }

  // V21: 辅助函数：获取设置值，不存在时返回默认值
  function getSettingValue(key: string, defaultValue: string): string {
    return settings.find((s) => s.key === key)?.value ?? defaultValue;
  }

  /**
   * 部分配置项需要比"文本框/开关"更贴切的控件：
   * - ui.theme：只能是 dark/light，用下拉避免用户输入非法值
   * - startup.auto_launch：要说明它写进了系统启动项，用户才能放心开
   * - global_shortcut：格式敏感，给出示例格式
   *
   * 返回 null 表示"没有特殊处理"，由通用渲染接管。
   */
  function renderSpecialized(item: SettingItem) {
    const hint = fieldHint[item.key];
    const isSaving = savingKey === item.key;

    if (item.key === "ui.theme") {
      return (
        <div className="setting-input-group">
          <select
            className="setting-input"
            value={item.value}
            onChange={(e) => void handleSave(item.key, e.target.value)}
            disabled={isSaving}
          >
            <option value="dark">深色（赛博朋克）</option>
            <option value="light">浅色</option>
          </select>
          {isSaving && <span className="saving-indicator">保存中...</span>}
          {!isSaving && hint && (
            <span className={`field-hint ${hint.kind}`}>{hint.text}</span>
          )}
        </div>
      );
    }

    if (item.key === "startup.auto_launch") {
      return (
        <div className="setting-input-group">
          <label className="toggle-switch">
            <input
              type="checkbox"
              checked={item.value === "true"}
              onChange={(e) =>
                void handleSave(item.key, e.target.checked ? "true" : "false")
              }
              disabled={isSaving || autostartChecking}
            />
            <span className="toggle-slider" />
            <span className="toggle-label">
              {isSaving
                ? "处理中..."
                : autostartChecking
                  ? "检测中..."
                  : item.value === "true"
                    ? "已写入系统启动项"
                    : "已禁用"}
            </span>
          </label>
          <button
            className="rule-edit-btn"
            onClick={() => void checkAutostart()}
            disabled={isSaving || autostartChecking}
            title="到系统里查一下这个开关是否真的生效"
          >
            检测实际状态
          </button>
          {!isSaving && autostartActual !== null && (
            <span
              className={`field-hint ${
                autostartActual === (item.value === "true") ? "ok" : "error"
              }`}
            >
              {autostartActual === (item.value === "true")
                ? "与系统一致"
                : autostartActual
                  ? "系统里已启用，配置说关闭"
                  : "系统里未启用，配置说开启"}
            </span>
          )}
          {!isSaving && autostartActual === null && autostartProbeFailed && (
            <span className="field-hint error">无法查询（系统限制了外部程序调用）</span>
          )}
          {!isSaving && hint?.kind === "error" && (
            <span className="field-hint error">{hint.text}</span>
          )}
        </div>
      );
    }

    if (item.key === "global_shortcut") {
      return (
        <div className="setting-input-group">
          <input
            type="text"
            className={`setting-input ${
              hint?.kind === "error" ? "input-invalid" : ""
            }`}
            value={item.value}
            onChange={(e) => handleChange(item.key, e.target.value)}
            onBlur={(e) => void handleSave(item.key, e.target.value.trim())}
            placeholder="Ctrl+Shift+Space"
            disabled={isSaving}
          />
          {isSaving && <span className="saving-indicator">保存中...</span>}
          {!isSaving && hint && (
            <span className={`field-hint ${hint.kind}`}>{hint.text}</span>
          )}
          {!isSaving && !hint && (
            <span className="field-range">
              立即生效；若被其他软件占用会自动换一个可用的键
            </span>
          )}
        </div>
      );
    }

    return null;
  }

  /**
   * 到系统里查开机自启的真实状态。
   *
   * 做成**按需触发**而不是启动时自动查：它要执行 reg.exe，
   * 在受限环境里可能不可用，不该让一个查询动作影响应用启动。
   */
  async function checkAutostart() {
    setAutostartChecking(true);
    setAutostartProbeFailed(false);
    try {
      const result = await invoke<boolean | null>("check_autostart_status");
      setAutostartActual(result);
      if (result === null) setAutostartProbeFailed(true);
    } catch {
      setAutostartActual(null);
      setAutostartProbeFailed(true);
    } finally {
      setAutostartChecking(false);
    }
  }

  /** 把取值范围渲染成一句人话，例如「1024 – 65535」或「≥ 5 s」 */
  function describeRange(item: SettingItem): string {
    const { min_value: lo, max_value: hi, unit } = item;
    const suffix = unit ? ` ${unit}` : "";
    if (lo !== null && hi !== null) return `${lo} – ${hi}${suffix}`;
    if (lo !== null) return `≥ ${lo}${suffix}`;
    if (hi !== null) return `≤ ${hi}${suffix}`;
    return "";
  }

  /** 前端即时校验：返回错误文案，null 表示当前输入合法 */
  function validateLocally(item: SettingItem, raw: string): string | null {
    if (item.value_type.toUpperCase() !== "NUMBER") return null;
    if (raw.trim() === "") return "不能留空";
    const parsed = Number(raw);
    if (!Number.isFinite(parsed)) return "需要填一个数字";
    if (item.min_value !== null && parsed < item.min_value) {
      return `不能小于 ${item.min_value}${item.unit ? " " + item.unit : ""}`;
    }
    if (item.max_value !== null && parsed > item.max_value) {
      return `不能大于 ${item.max_value}${item.unit ? " " + item.unit : ""}`;
    }
    return null;
  }

  function renderInput(item: SettingItem) {
    const isSaving = savingKey === item.key;
    const hint = fieldHint[item.key];
    const rangeText = describeRange(item);

    switch (item.value_type.toUpperCase()) {
      case "BOOLEAN":
        return (
          <label className="toggle-switch">
            <input
              type="checkbox"
              checked={item.value === "true"}
              onChange={(e) =>
                handleSave(item.key, e.target.checked ? "true" : "false")
              }
              disabled={isSaving}
            />
            <span className="toggle-slider" />
            <span className="toggle-label">
              {item.value === "true" ? "已启用" : "已禁用"}
            </span>
          </label>
        );

      case "NUMBER":
        return (
          <div className="setting-input-group">
            <input
              type="number"
              className={`setting-input ${
                hint?.kind === "error" ? "input-invalid" : ""
              }`}
              value={item.value}
              min={item.min_value ?? undefined}
              max={item.max_value ?? undefined}
              onChange={(e) => handleChange(item.key, e.target.value)}
              onBlur={(e) => {
                const localErr = validateLocally(item, e.target.value);
                if (localErr) {
                  // 本地就能判定的错误不必往返一趟后端
                  setFieldHint((prev) => ({
                    ...prev,
                    [item.key]: { kind: "error", text: localErr },
                  }));
                  return;
                }
                void handleSave(item.key, e.target.value);
              }}
              disabled={isSaving}
              step="any"
            />
            {item.unit && <span className="setting-unit">{item.unit}</span>}
            {isSaving && <span className="saving-indicator">保存中...</span>}
            {!isSaving && hint && (
              <span className={`field-hint ${hint.kind}`}>{hint.text}</span>
            )}
            {!isSaving && !hint && rangeText && (
              <span className="field-range">{rangeText}</span>
            )}
          </div>
        );

      case "STRING":
      default:
        return (
          <div className="setting-input-group">
            <input
              type="text"
              className="setting-input"
              value={item.value}
              onChange={(e) => handleChange(item.key, e.target.value)}
              onBlur={(e) => void handleSave(item.key, e.target.value)}
              disabled={isSaving}
              placeholder={item.description || ""}
            />
            {isSaving && <span className="saving-indicator">保存中...</span>}
            {!isSaving && hint && (
              <span className={`field-hint ${hint.kind}`}>{hint.text}</span>
            )}
          </div>
        );
    }
  }

  return (
    <div className="page-container">
      <header className="page-header">
        <h1>设置</h1>
        <p className="page-subtitle">配置应用参数、安全选项和主动助手</p>
      </header>

      {/* Tab 切换 */}
      <div className="settings-tabs">
        <button
          className={`settings-tab ${activeTab === "general" ? "active" : ""}`}
          onClick={() => setActiveTab("general")}
        >
          通用设置
        </button>
        <button
          className={`settings-tab ${activeTab === "model" ? "active" : ""}`}
          onClick={() => setActiveTab("model")}
        >
          模型服务
        </button>
        <button
          className={`settings-tab ${activeTab === "security" ? "active" : ""}`}
          onClick={() => setActiveTab("security")}
        >
          安全
        </button>
        <button
          className={`settings-tab ${activeTab === "automation" ? "active" : ""}`}
          onClick={() => setActiveTab("automation")}
        >
          自动化
        </button>
        <button
          className={`settings-tab ${activeTab === "proactive" ? "active" : ""}`}
          onClick={() => setActiveTab("proactive")}
        >
          主动助手
        </button>
        <button
          className={`settings-tab ${activeTab === "browser" ? "active" : ""}`}
          onClick={() => setActiveTab("browser")}
        >
          浏览器
        </button>
        <button
          className={`settings-tab ${activeTab === "skills" ? "active" : ""}`}
          onClick={() => { setActiveTab("skills"); loadSkills(); }}
        >
          技能管理
        </button>
        <button
          className={`settings-tab ${activeTab === "memory" ? "active" : ""}`}
          onClick={() => { setActiveTab("memory"); loadMemories(); }}
        >
          记忆管理
        </button>
      </div>

      {saveError && (
        <div className="alert-item critical">
          <span className="alert-icon">🔴</span>
          <div className="alert-content">
            <div className="alert-title">保存失败</div>
            <div className="alert-message">{saveError}</div>
          </div>
        </div>
      )}

      {/* 模型服务 Tab（sidecar 多 provider 诊断） */}
      {activeTab === "model" && <SettingsModel />}

      {/* 自动化 Tab（定时任务 + 多端设备） */}
      {activeTab === "automation" && <SettingsAutomation />}

      {/* 通用设置 Tab */}
      {activeTab === "general" && (
        <div className="section-card">
          <h3>通用设置</h3>
          {loading ? (
            <p className="empty-state">加载中...</p>
          ) : (
            <div className="settings-groups">
              {SETTING_GROUPS.map((group) => {
                const items = settings.filter((s) => group.keys.includes(s.key));
                if (items.length === 0) return null;
                return (
                  <div key={group.title} className="settings-group">
                    <div className="settings-group-title">
                      <span>{group.title}</span>
                      <span className="settings-group-note">{group.note}</span>
                    </div>
                    <div className="settings-list">
                      {items.map((item) => (
                        <div key={item.key} className="setting-row editable">
                          <div className="setting-info">
                            <span className="setting-key">
                              {SETTING_LABELS[item.key] ?? item.key}
                            </span>
                            {item.description && (
                              <span className="setting-desc">{item.description}</span>
                            )}
                          </div>
                          <div className="setting-value">
                            {renderSpecialized(item) ?? renderInput(item)}
                          </div>
                        </div>
                      ))}
                    </div>
                  </div>
                );
              })}

              {/* 发布凭据含密钥，后端只下发掩码、绝不下发明文。
                  因此这里刻意做成"空白输入框 + 状态提示"：
                  - 不能回填掩码，否则用户 blur 一下就会把 •••• 写进库里，
                    发布会拿着掩码去调用 API 然后静默失败；
                  - 也不能显示明文，那等于把凭据摊在 DOM 里。
                  想知道"填没填过"，看输入框下方的状态提示即可。 */}
              {PUBLISH_SECRET_KEYS.map(({ key, label, desc, placeholder }) => {
                const current = getSettingValue(key, "");
                const configured = isConfiguredSecret(current);
                return (
                  <div key={key} className="setting-row editable">
                    <div className="setting-info">
                      <span className="setting-key">{label}</span>
                      <span className="setting-desc">{desc}</span>
                    </div>
                    <div className="setting-value">
                      <div className="setting-input-group">
                        <input
                          type="password"
                          className="setting-input"
                          value={secretDrafts[key] ?? ""}
                          onChange={(event) => {
                            const next = event.target.value;
                            setSecretDrafts((prev) => ({ ...prev, [key]: next }));
                          }}
                          onBlur={(event) => {
                            const next = event.target.value.trim();
                            // 空输入 = 用户没打算改这个字段，直接放过。
                            // 想清除凭据得用专门的按钮，避免误触把配置清空。
                            if (!next) {
                              setSecretDrafts((prev) => ({ ...prev, [key]: "" }));
                              return;
                            }
                            void handleSave(key, next).then((ok) => {
                              // 失败时保留草稿：用户多半没别的机会再拿到这串令牌，
                              // 清空等于让他重新去平台生成一个。
                              if (ok) {
                                setSecretDrafts((prev) => ({ ...prev, [key]: "" }));
                              }
                            });
                          }}
                          placeholder={
                            configured
                              ? "已配置（重新输入可覆盖）"
                              : placeholder
                          }
                          disabled={savingKey === key}
                        />
                        {savingKey === key && (
                          <span className="saving-indicator">保存中...</span>
                        )}
                        {savingKey !== key && fieldHint[key] && (
                          <span className={`field-hint ${fieldHint[key].kind}`}>
                            {fieldHint[key].text}
                          </span>
                        )}
                        {savingKey !== key && !fieldHint[key] && configured && (
                          <span className="field-hint ok">
                            已配置（末四位 {secretTail(current)}）· 留空不改动
                          </span>
                        )}
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}

      {/* 安全 Tab */}
      {activeTab === "security" && (
        <>
          {securityError && (
            <div className="alert-item critical">
              <span className="alert-icon">🔴</span>
              <div className="alert-content">
                <div className="alert-title">安全数据加载失败</div>
                <div className="alert-message">{securityError}</div>
              </div>
            </div>
          )}

          <div className="section-card">
            <h3>安全策略</h3>

            {/* 可调项：MEDIUM 风险确认。
                此前这里只是一行写着"弹窗确认"的死文字，而背后其实有
                security.confirm_medium_risk 这个真正生效的配置——用户看着
                像说明、没法改，等于把一个可调项藏起来了。 */}
            <div className="settings-list">
              {settings
                .filter((s) => s.key === "security.confirm_medium_risk")
                .map((item) => (
                  <div key={item.key} className="setting-row editable">
                    <div className="setting-info">
                      <span className="setting-key">
                        {SETTING_LABELS[item.key] ?? item.key}
                      </span>
                      <span className="setting-desc">
                        关闭后，MEDIUM 风险的操作（如写入可信目录、修改非系统文件）
                        将不再弹窗，直接执行。
                      </span>
                    </div>
                    <div className="setting-value">
                      {renderInput(item)}
                    </div>
                  </div>
                ))}
            </div>

            <p className="section-note">
              以下为系统的固有行为，<strong>不可调整</strong>——它们不随配置变化，
              列出是为了让你知道 AI 在哪些地方被硬性约束。
            </p>
            <div className="security-info readonly">
              <div className="security-item">
                <span className="security-label">风险等级体系</span>
                <span className="security-value">SAFE / LOW / MEDIUM / HIGH / CRITICAL</span>
              </div>
              <div className="security-item">
                <span className="security-label">HIGH 风险确认</span>
                <span className="security-value">强制弹窗确认，不可自动放行</span>
              </div>
              <div className="security-item">
                <span className="security-label">路径安全策略</span>
                <span className="security-value">系统目录禁止写入，用户目录可读写，自定义目录完全开放</span>
              </div>
              <div className="security-item">
                <span className="security-label">路径遍历防护</span>
                <span className="security-value">阻止 .. 序列、符号链接穿越、UNC 路径、环境变量注入</span>
              </div>
            </div>
          </div>

          <div className="section-card">
            <h3>可信目录</h3>
            <p className="section-note">
              可信目录中的文件完全开放读写权限。系统目录和未配置的目录默认受保护。
            </p>

            <div className="directory-add-row">
              <button
                className="directory-add-btn"
                onClick={handleAddDirectory}
                title="点击后从系统里挑选一个目录"
              >
                + 选择目录并加入可信列表
              </button>
              <input
                type="text"
                className="directory-label-input"
                placeholder="标签（可选，如：项目代码）"
                value={newDirLabel}
                onChange={(e) => setNewDirLabel(e.target.value)}
              />
            </div>
            <p className="directory-add-hint">
              先点左边的按钮挑目录；标签只用于在列表里辨认，留空也可以。
            </p>

            {securityLoading ? (
              <p className="empty-state">加载中...</p>
            ) : allowedDirectories.length === 0 ? (
              <p className="empty-state">暂无自定义可信目录</p>
            ) : (
              <div className="directory-list">
                {allowedDirectories.map((dir) => (
                  <div key={dir.id} className={`directory-item ${dir.is_default ? "default" : ""}`}>
                    <div className="directory-info">
                      <span className="directory-name">{dir.label || dir.path}</span>
                      <span className="directory-path">{dir.path}</span>
                      <div className="directory-meta">
                        <span className={`directory-badge access-${dir.access_level.toLowerCase()}`}>
                          {dir.access_level}
                        </span>
                        {dir.is_default && <span className="directory-badge default">默认</span>}
                      </div>
                    </div>
                    {!dir.is_default && (
                      <button
                        className="directory-delete-btn"
                        onClick={() => handleRemoveDirectory(dir.path)}
                        title="删除"
                      >
                        ×
                      </button>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>

          <div className="section-card">
            <h3>已信任工具</h3>
            <p className="section-note">
              用户选择"始终允许"的工具会出现在此列表中。信任工具在后续调用时自动放行，不再弹窗确认。
            </p>

            {securityLoading ? (
              <p className="empty-state">加载中...</p>
            ) : trustedTools.length === 0 ? (
              <p className="empty-state">暂无信任工具</p>
            ) : (
              <div className="trusted-tools-list">
                {trustedTools.map((tool) => (
                  <div key={tool.id} className="trusted-tool-item">
                    <div className="trusted-tool-info">
                      <span className="trusted-tool-name">{tool.tool_name}</span>
                      <span className={`trusted-tool-risk ${tool.risk_level.toLowerCase()}`}>
                        {tool.risk_level}
                      </span>
                      {tool.path_pattern && (
                        <span className="trusted-tool-path">{tool.path_pattern}</span>
                      )}
                      <span className="trusted-tool-date">{new Date(tool.added_at).toLocaleDateString()}</span>
                    </div>
                    <button
                      className="trusted-tool-delete-btn"
                      onClick={() => handleRemoveTrustedTool(tool.tool_name, tool.path_pattern)}
                      title="撤销信任"
                    >
                      撤销信任
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>
        </>
      )}

      {/* V18: 主动助手 Tab */}
      {activeTab === "proactive" && (
        <>
          {proactiveError && (
            <div className="alert-item critical">
              <span className="alert-icon">🔴</span>
              <div className="alert-content">
                <div className="alert-title">加载失败</div>
                <div className="alert-message">{proactiveError}</div>
              </div>
            </div>
          )}

          {/* 引擎状态卡片 */}
          <div className="section-card proactive-status-card">
            <h3>引擎状态</h3>
            {proactiveStatus && (
              <div className="proactive-status-grid">
                <div className="status-item">
                  <span className="status-label">运行状态</span>
                  <span className={`status-value ${proactiveStatus.running ? "running" : "stopped"}`}>
                    {proactiveStatus.running ? "● 运行中" : "○ 已停止"}
                  </span>
                </div>
                <div className="status-item">
                  <span className="status-label">规则总数</span>
                  <span className="status-value">{proactiveStatus.rules_total}</span>
                </div>
                <div className="status-item">
                  <span className="status-label">已启用</span>
                  <span className="status-value">{proactiveStatus.rules_enabled}</span>
                </div>
                <div className="status-item">
                  <span className="status-label">检查次数</span>
                  <span className="status-value">{proactiveStatus.checks_count}</span>
                </div>
                <div className="status-item">
                  <span className="status-label">触发次数</span>
                  <span className="status-value">{proactiveStatus.triggers_count}</span>
                </div>
                <div className="status-item">
                  <span className="status-label">上次检查</span>
                  <span className="status-value">
                    {proactiveStatus.last_check || "尚未检查"}
                  </span>
                </div>
              </div>
            )}
            <p className="section-note">
              引擎每 {getSettingValue("agent.proactive_interval_sec", "15")} 秒检查一次系统状态，
              满足规则条件时自动触发通知；同一规则触发后
              {getSettingValue("agent.proactive_cooldown_sec", "300")} 秒内不再重复提醒，避免刷屏。
              这两项间隔可在「通用设置 → 主动助手」中调整。
            </p>
          </div>

          {/* 主动通知记录 */}
          {notifications.length > 0 && (
            <div className="section-card">
              <h3>最近通知 ({notifications.length})</h3>
              <div className="proactive-notifications">
                {notifications.map((n, i) => (
                  <div key={i} className={`proactive-notification level-${n.level}`}>
                    <div className="notif-title">{n.title}</div>
                    <div className="notif-message">{n.message}</div>
                    {n.context && <div className="notif-context">当前: {n.context}</div>}
                    <div className="notif-time">{new Date(n.timestamp).toLocaleTimeString()}</div>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* 规则列表 */}
          <div className="section-card">
            <div className="section-header-row">
              <h3>自动化规则</h3>
              <button
                className="proactive-add-btn"
                onClick={() => setShowAddForm(!showAddForm)}
              >
                {showAddForm ? "取消" : "+ 添加规则"}
              </button>
            </div>

            {/* 添加规则表单 */}
            {showAddForm && (
              <div className="proactive-add-form">
                <div className="form-row">
                  <label>规则名称</label>
                  <input
                    type="text"
                    value={newRuleName}
                    onChange={(e) => setNewRuleName(e.target.value)}
                    placeholder="如：高内存提醒"
                  />
                </div>
                <div className="form-row">
                  <label>监控指标</label>
                  <select value={newRuleMetric} onChange={(e) => setNewRuleMetric(e.target.value)}>
                    <option value="memory">内存使用率 (%)</option>
                    <option value="cpu">CPU 使用率 (%)</option>
                    <option value="disk">磁盘可用空间 (GB)</option>
                  </select>
                </div>
                <div className="form-row">
                  <label>阈值</label>
                  <input
                    type="number"
                    value={newRuleThreshold}
                    onChange={(e) => setNewRuleThreshold(e.target.value)}
                  />
                </div>
                <div className="form-row">
                  <label>通知消息</label>
                  <input
                    type="text"
                    value={newRuleMessage}
                    onChange={(e) => setNewRuleMessage(e.target.value)}
                    placeholder="自定义通知内容（可选）"
                  />
                </div>
                <button className="proactive-submit-btn" onClick={handleAddRule}>
                  创建规则
                </button>
              </div>
            )}

            {proactiveLoading ? (
              <p className="empty-state">加载中...</p>
            ) : proactiveRules.length === 0 ? (
              <p className="empty-state">暂无规则，点击上方按钮添加</p>
            ) : (
              <div className="proactive-rules-list">
                {proactiveRules.map((rule) => {
                  const threshold = getRuleThreshold(rule);
                  const isEditing = editingRuleId === rule.id;
                  return (
                  <div key={rule.id} className={`proactive-rule-item ${rule.enabled ? "enabled" : "disabled"}`}>
                    <div className="rule-main">
                      <div className="rule-header">
                        <span className="rule-name">{rule.name}</span>
                        <span className={`rule-badge ${rule.trigger_type}`}>{rule.trigger_type}</span>
                        {rule.id.startsWith("preset_") && (
                          <span className="rule-badge preset">预设</span>
                        )}
                      </div>
                      <div className="rule-details">
                        <span className="rule-detail">触发: {getTriggerDescription(rule)}</span>
                        <span className="rule-detail">动作: {getActionDescription(rule)}</span>
                      </div>

                      {/* 阈值内联编辑：预设规则的阈值过去是硬编码的，现在可直接改 */}
                      {isEditing ? (
                        <div className="rule-threshold-edit">
                          <input
                            type="number"
                            className={`rule-threshold-input ${
                              ruleEditError[rule.id] ? "input-invalid" : ""
                            }`}
                            value={ruleThresholdDraft}
                            onChange={(e) => setRuleThresholdDraft(e.target.value)}
                            onKeyDown={(e) => {
                              if (e.key === "Enter") void saveThreshold(rule);
                              if (e.key === "Escape") setEditingRuleId(null);
                            }}
                            min={0}
                            max={100}
                            autoFocus
                          />
                          <span className="rule-threshold-unit">{getThresholdUnit(rule)}</span>
                          <button
                            className="rule-edit-save"
                            onClick={() => void saveThreshold(rule)}
                          >
                            保存
                          </button>
                          <button
                            className="rule-edit-cancel"
                            onClick={() => setEditingRuleId(null)}
                          >
                            取消
                          </button>
                        </div>
                      ) : (
                        <div className="rule-meta">
                          <span>触发 {rule.trigger_count} 次</span>
                          {rule.last_triggered && <span>上次: {rule.last_triggered}</span>}
                        </div>
                      )}

                      {ruleEditError[rule.id] && (
                        <div className="rule-edit-error">{ruleEditError[rule.id]}</div>
                      )}
                    </div>
                    <div className="rule-actions">
                      <label className="toggle-switch small">
                        <input
                          type="checkbox"
                          checked={rule.enabled}
                          onChange={(e) => handleToggleRule(rule.id, e.target.checked)}
                        />
                        <span className="toggle-slider" />
                      </label>
                      {threshold !== null && !isEditing && (
                        <button
                          className="rule-edit-btn"
                          onClick={() => beginEditThreshold(rule)}
                          title="修改触发阈值"
                        >
                          改阈值
                        </button>
                      )}
                      {!rule.id.startsWith("preset_") && (
                        <button
                          className="rule-delete-btn"
                          onClick={() => handleDeleteRule(rule.id, rule.name)}
                          title="删除"
                        >
                          🗑
                        </button>
                      )}
                    </div>
                  </div>
                  );
                })}
              </div>
            )}
          </div>
        </>
      )}

      {/* V19: 浏览器 Tab */}
      {activeTab === "browser" && (
        <div className="section-card">
          <h3>浏览器自动化配置</h3>
          <p className="section-note">
            这里的设置写入后，<strong>下次启动浏览器时</strong>生效。若浏览器已经在运行，
            先关闭它再让 AI 重新打开，改动才会应用到新的调试连接上。
          </p>
          <div className="settings-list">
            <div className="setting-row">
              <div className="setting-info">
                <span className="setting-key">默认浏览器</span>
                <span className="setting-desc">
                  AI 控制浏览器时优先使用的浏览器。选中的浏览器未安装时会自动回退到
                  系统可用的第一个，并把原因告知 AI。
                </span>
              </div>
              <div className="setting-value">
                <div className="setting-input-group">
                  <select
                    className="setting-input"
                    value={getSettingValue("browser.default", "auto")}
                    onChange={(e) => void handleSave("browser.default", e.target.value)}
                    disabled={savingKey === "browser.default"}
                  >
                    <option value="auto">自动检测（按系统优先级）</option>
                    <option value="chrome">Google Chrome</option>
                    <option value="edge">Microsoft Edge</option>
                    <option value="brave">Brave</option>
                    <option value="firefox">Firefox</option>
                    <option value="chromium">Chromium</option>
                  </select>
                  {savingKey === "browser.default" && (
                    <span className="saving-indicator">保存中...</span>
                  )}
                  {savingKey !== "browser.default" && fieldHint["browser.default"] && (
                    <span className={`field-hint ${fieldHint["browser.default"].kind}`}>
                      {fieldHint["browser.default"].text}
                    </span>
                  )}
                </div>
              </div>
            </div>

            <div className="setting-row">
              <div className="setting-info">
                <span className="setting-key">CDP 调试端口</span>
                <span className="setting-desc">
                  浏览器远程调试端口（1024–65535）。若该端口已被别的程序占用，
                  换一个空闲端口即可。
                </span>
              </div>
              <div className="setting-value">
                <div className="setting-input-group">
                  <input
                    type="number"
                    className={`setting-input ${
                      fieldHint["browser.cdp_port"]?.kind === "error"
                        ? "input-invalid"
                        : ""
                    }`}
                    value={getSettingValue("browser.cdp_port", "9222")}
                    min={1024}
                    max={65535}
                    onChange={(e) => handleChange("browser.cdp_port", e.target.value)}
                    onBlur={(e) => {
                      const raw = e.target.value;
                      const parsed = Number(raw);
                      if (!Number.isFinite(parsed) || parsed < 1024 || parsed > 65535) {
                        setFieldHint((prev) => ({
                          ...prev,
                          "browser.cdp_port": {
                            kind: "error",
                            text: "端口需在 1024–65535 之间",
                          },
                        }));
                        return;
                      }
                      void handleSave("browser.cdp_port", raw);
                    }}
                    disabled={savingKey === "browser.cdp_port"}
                  />
                  {savingKey === "browser.cdp_port" && (
                    <span className="saving-indicator">保存中...</span>
                  )}
                  {savingKey !== "browser.cdp_port" && fieldHint["browser.cdp_port"] && (
                    <span className={`field-hint ${fieldHint["browser.cdp_port"].kind}`}>
                      {fieldHint["browser.cdp_port"].text}
                    </span>
                  )}
                </div>
              </div>
            </div>
          </div>
          <div className="browser-info-box readonly">
            <h4><CyberIcon name="logs" size={14} /> 浏览器工具清单（只读）</h4>
            <p>AI 可调用以下工具控制浏览器。这里只是清单，没有可调项。</p>
            <ul>
              <li><code>browser_open</code> — 打开浏览器并导航到 URL</li>
              <li><code>browser_close</code> — 关闭浏览器</li>
              <li><code>browser_navigate</code> — 在当前标签页导航到指定页面</li>
              <li><code>browser_screenshot</code> — 截取当前页面</li>
              <li><code>browser_get_content</code> — 获取页面 HTML/文本</li>
              <li><code>browser_get_title</code> — 读取页面标题</li>
              <li><code>browser_click</code> — 点击元素</li>
              <li><code>browser_type</code> — 在输入框里输入文字</li>
              <li><code>browser_scroll</code> — 滚动页面</li>
              <li><code>browser_wait</code> — 等待元素出现</li>
              <li><code>browser_list_tabs</code> — 列出所有标签页</li>
              <li><code>browser_switch_tab</code> — 切换标签页</li>
              <li><code>browser_execute_js</code> — 执行 JavaScript（高风险）</li>
            </ul>
            <p className="hint"><CyberIcon name="alertTriangle" size={14} /> 敏感域名（银行、支付等）会被自动阻止。点击/输入/JS 执行需权限确认。</p>
          </div>
        </div>
      )}

      {/* V20: 技能管理 Tab */}
      {activeTab === "skills" && (
        <div className="section-card">
          <div className="section-header-row">
            <h3>技能管理</h3>
            <button
              className="proactive-add-btn"
              onClick={() => void handleInstallSkill()}
              disabled={skillsLoading}
            >
              + 从目录安装
            </button>
          </div>
          <p className="section-desc">技能是可扩展的功能包，AI 可以发现并调用。内置技能不可卸载，本地技能可从目录安装（目录中需包含 SKILL.md）。</p>

          {skillsError && (
            <div className="alert-item critical">
              <span className="alert-icon">🔴</span>
              <div className="alert-content">
                <div className="alert-title">加载失败</div>
                <div className="alert-message">{skillsError}</div>
              </div>
            </div>
          )}

          {skillsLoading ? (
            <p className="empty-state">加载中...</p>
          ) : skills.length === 0 ? (
            <p className="empty-state">暂无已安装技能</p>
          ) : (
            <div className="skills-list">
              {skills.map((skill) => (
                <div
                  key={skill.id}
                  className={`skill-item ${skill.enabled ? "enabled" : "disabled"} ${selectedSkill?.id === skill.id ? "selected" : ""}`}
                  onClick={() => setSelectedSkill(selectedSkill?.id === skill.id ? null : skill)}
                >
                  <div className="skill-main">
                    <div className="skill-header">
                      <span className="skill-name">{skill.name}</span>
                      <span className={`skill-badge ${skill.skill_type}`}>
                        {skill.skill_type === "builtin" ? "内置" : skill.skill_type === "local" ? "本地" : "远程"}
                      </span>
                      <span className="skill-version">v{skill.version}</span>
                    </div>
                    <div className="skill-desc">{skill.description}</div>
                    <div className="skill-meta">
                      {skill.author && <span>作者: {skill.author}</span>}
                      {skill.triggers.length > 0 && <span>触发词: {skill.triggers.join(", ")}</span>}
                    </div>
                  </div>
                  <div className="skill-actions" onClick={(e) => e.stopPropagation()}>
                    <label className="toggle-switch small">
                      <input
                        type="checkbox"
                        checked={skill.enabled}
                        onChange={(e) => handleToggleSkill(skill.id, e.target.checked)}
                      />
                      <span className="toggle-slider" />
                    </label>
                    {skill.skill_type !== "builtin" && (
                      <button
                        className="skill-delete-btn"
                        onClick={() => handleUninstallSkill(skill.id, skill.name)}
                        title="卸载"
                      >
                        🗑
                      </button>
                    )}
                  </div>
                  {selectedSkill?.id === skill.id && (
                    <div className="skill-detail">
                      <div className="detail-row"><strong>技能 ID:</strong> {skill.id}</div>
                      <div className="detail-row"><strong>权限:</strong> {skill.permissions.length > 0 ? skill.permissions.join(", ") : "无特殊权限"}</div>
                      <div className="detail-row"><strong>安装时间:</strong> {skill.installed_at || "内置技能"}</div>
                      <div className="detail-row"><strong>工具名:</strong> <code>skill_{skill.id}</code></div>
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* V21: 记忆管理 Tab */}
      {activeTab === "memory" && (
        <div className="section-card">
          <div className="section-header-row">
            <h3>长期记忆管理</h3>
            <button
              className="proactive-add-btn"
              onClick={() => setShowAddMemory(!showAddMemory)}
            >
              {showAddMemory ? "取消" : "+ 添加记忆"}
            </button>
          </div>
          <p className="section-desc">AI 会自动记住你的偏好、习惯和重要信息。你可以在这里查看、搜索、添加或删除记忆。</p>

          {/* 添加记忆表单 */}
          {showAddMemory && (
            <div className="proactive-add-form">
              <div className="form-row">
                <label>记忆内容</label>
                <textarea
                  value={newMemoryContent}
                  onChange={(e) => setNewMemoryContent(e.target.value)}
                  placeholder="输入要记住的内容..."
                  rows={3}
                  style={{ width: "100%", resize: "vertical" }}
                />
              </div>
              <div className="form-row" style={{ display: "flex", gap: "12px" }}>
                <div style={{ flex: 1 }}>
                  <label>类型</label>
                  <select value={newMemoryType} onChange={(e) => setNewMemoryType(e.target.value)}>
                    <option value="user_preference">用户偏好</option>
                    <option value="task_history">任务历史</option>
                    <option value="knowledge">知识</option>
                    <option value="conversation_summary">对话摘要</option>
                    <option value="app_usage">应用习惯</option>
                  </select>
                </div>
                <div style={{ flex: 1 }}>
                  <label>重要性 (0-100)</label>
                  <input
                    type="number"
                    min="0"
                    max="100"
                    value={newMemoryImportance}
                    onChange={(e) => setNewMemoryImportance(e.target.value)}
                  />
                </div>
              </div>
              <button className="proactive-submit-btn" onClick={handleAddMemory}>
                保存记忆
              </button>
            </div>
          )}

          {/* 搜索和筛选 */}
          <div className="memory-toolbar">
            <div className="memory-search-row">
              <input
                type="text"
                className="memory-search-input"
                placeholder="搜索记忆..."
                value={memorySearch}
                onChange={(e) => setMemorySearch(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && handleSearchMemories()}
              />
              <button className="memory-search-btn" onClick={handleSearchMemories}>
                搜索
              </button>
              {memorySearch && (
                <button className="memory-clear-btn" onClick={() => { setMemorySearch(""); loadMemories(); }}>
                  清除
                </button>
              )}
            </div>
            <div className="memory-filter-row">
              {["all", "user_preference", "task_history", "knowledge", "conversation_summary", "app_usage"].map((t) => (
                <button
                  key={t}
                  className={`memory-filter-btn ${memoryFilter === t ? "active" : ""}`}
                  onClick={() => setMemoryFilter(t)}
                >
                  {t === "all" ? "全部" : getMemoryTypeLabel(t)}
                </button>
              ))}
            </div>
          </div>

          {memoriesError && (
            <div className="alert-item critical">
              <span className="alert-icon">🔴</span>
              <div className="alert-content">
                <div className="alert-title">加载失败</div>
                <div className="alert-message">{memoriesError}</div>
              </div>
            </div>
          )}

          {memoriesLoading ? (
            <p className="empty-state">加载中...</p>
          ) : memories.length === 0 ? (
            <p className="empty-state">暂无记忆记录</p>
          ) : (
            <div className="memory-list">
              {memories
                .filter((m) => memoryFilter === "all" || m.memory_type === memoryFilter)
                .map((m) => (
                  <div key={m.id} className="memory-item">
                    <div className="memory-main">
                      <div className="memory-header">
                        <span className={`memory-type type-${m.memory_type}`}>
                          {getMemoryTypeLabel(m.memory_type)}
                        </span>
                        <span
                          className="memory-importance"
                          style={{ color: getImportanceColor(m.importance) }}
                          title={`重要性: ${m.importance}`}
                        >
                          ★ {m.importance}
                        </span>
                        <span className="memory-access" title="访问次数">
                          👁 {m.access_count}
                        </span>
                      </div>
                      <div className="memory-content">{m.content}</div>
                      {m.keywords && (
                        <div className="memory-keywords">
                          {m.keywords.split(",").slice(0, 5).map((k, i) => (
                            <span key={i} className="memory-keyword">{k.trim()}</span>
                          ))}
                        </div>
                      )}
                      <div className="memory-meta">
                        <span>创建: {new Date(m.created_at).toLocaleString()}</span>
                        {m.last_accessed_at && m.last_accessed_at !== m.created_at && (
                          <span>最近访问: {new Date(m.last_accessed_at).toLocaleString()}</span>
                        )}
                      </div>
                    </div>
                    <button
                      className="memory-delete-btn"
                      onClick={() => handleDeleteMemory(m.id, m.content)}
                      title="删除记忆"
                    >
                      🗑
                    </button>
                  </div>
                ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default Settings;
