import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import CyberIcon from "../components/CyberIcon";

interface SettingItem {
  key: string;
  value: string;
  value_type: string;
  description: string | null;
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

type TabType = "general" | "security" | "proactive" | "browser" | "skills" | "memory";

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

  async function handleSave(key: string, newValue: string) {
    setSavingKey(key);
    setSaveError(null);
    try {
      await invoke("update_setting", { key, value: newValue });
      setSettings((prev) =>
        prev.map((s) => (s.key === key ? { ...s, value: newValue } : s))
      );
    } catch (e) {
      console.error("Failed to save setting:", e);
      setSaveError(typeof e === "string" ? e : `保存 ${key} 失败`);
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

  function renderInput(item: SettingItem) {
    const isSaving = savingKey === item.key;

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
              className="setting-input"
              value={item.value}
              onChange={(e) => handleChange(item.key, e.target.value)}
              onBlur={(e) => handleSave(item.key, e.target.value)}
              disabled={isSaving}
              step="any"
            />
            {isSaving && <span className="saving-indicator">保存中...</span>}
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
              onBlur={(e) => handleSave(item.key, e.target.value)}
              disabled={isSaving}
              placeholder={item.description || ""}
            />
            {isSaving && <span className="saving-indicator">保存中...</span>}
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
          className={`settings-tab ${activeTab === "security" ? "active" : ""}`}
          onClick={() => setActiveTab("security")}
        >
          安全
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

      {/* 通用设置 Tab */}
      {activeTab === "general" && (
        <div className="section-card">
          <h3>通用设置</h3>
          {loading ? (
            <p className="empty-state">加载中...</p>
          ) : (
            <div className="settings-list">
              {settings.map((item) => (
                <div key={item.key} className="setting-row editable">
                  <div className="setting-info">
                    <span className="setting-key">{item.key}</span>
                    {item.description && (
                      <span className="setting-desc">{item.description}</span>
                    )}
                  </div>
                  <div className="setting-value">
                    <span className={`value-type type-${item.value_type}`}>
                      {item.value_type}
                    </span>
                    {renderInput(item)}
                  </div>
                </div>
              ))}

              {/* V21: 关闭按钮最小化到托盘（独立开关，确保始终可见） */}
              <div className="setting-row editable">
                <div className="setting-info">
                  <span className="setting-key">关闭按钮最小化到托盘</span>
                  <span className="setting-desc">开启后点击窗口关闭按钮（X）将最小化到系统托盘，而非退出应用</span>
                </div>
                <div className="setting-value">
                  <span className="value-type type-BOOLEAN">BOOLEAN</span>
                  <label className="toggle-switch">
                    <input
                      type="checkbox"
                      checked={getSettingValue("tray.minimize_on_close", "true") === "true"}
                      onChange={(e) =>
                        handleSave("tray.minimize_on_close", e.target.checked ? "true" : "false")
                      }
                      disabled={savingKey === "tray.minimize_on_close"}
                    />
                    <span className="toggle-slider" />
                    <span className="toggle-label">
                      {getSettingValue("tray.minimize_on_close", "true") === "true" ? "已启用" : "已禁用"}
                    </span>
                  </label>
                </div>
              </div>
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
            <div className="security-info">
              <div className="security-item">
                <span className="security-label">风险等级体系</span>
                <span className="security-value">SAFE / LOW / MEDIUM / HIGH / CRITICAL</span>
              </div>
              <div className="security-item">
                <span className="security-label">MEDIUM 风险确认</span>
                <span className="security-value">弹窗确认</span>
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
              <input
                type="text"
                className="directory-label-input"
                placeholder="目录标签（如：项目代码）"
                value={newDirLabel}
                onChange={(e) => setNewDirLabel(e.target.value)}
              />
              <button className="directory-add-btn" onClick={handleAddDirectory}>
                + 添加目录
              </button>
            </div>

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
              引擎每 15 秒检查一次系统状态，满足规则条件时自动触发通知。触发后有 5 分钟冷却期避免刷屏。
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
                {proactiveRules.map((rule) => (
                  <div key={rule.id} className={`proactive-rule-item ${rule.enabled ? "enabled" : "disabled"}`}>
                    <div className="rule-main">
                      <div className="rule-header">
                        <span className="rule-name">{rule.name}</span>
                        <span className={`rule-badge ${rule.trigger_type}`}>{rule.trigger_type}</span>
                      </div>
                      <div className="rule-details">
                        <span className="rule-detail">触发: {getTriggerDescription(rule)}</span>
                        <span className="rule-detail">动作: {getActionDescription(rule)}</span>
                      </div>
                      <div className="rule-meta">
                        <span>触发 {rule.trigger_count} 次</span>
                        {rule.last_triggered && <span>上次: {rule.last_triggered}</span>}
                      </div>
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
                ))}
              </div>
            )}
          </div>
        </>
      )}

      {/* V19: 浏览器 Tab */}
      {activeTab === "browser" && (
        <div className="section-card">
          <h3>浏览器自动化配置</h3>
          <div className="settings-list">
            <div className="setting-row">
              <div className="setting-info">
                <span className="setting-key">默认浏览器</span>
                <span className="setting-desc">AI 控制浏览器时优先使用的浏览器（自动检测已安装的 Chrome/Edge）</span>
              </div>
              <div className="setting-value">
                <select
                  value={settings.find((s) => s.key === "browser.default")?.value || "auto"}
                  onChange={(e) => handleSave("browser.default", e.target.value)}
                  disabled={savingKey === "browser.default"}
                >
                  <option value="auto">自动检测</option>
                  <option value="chrome">Google Chrome</option>
                  <option value="edge">Microsoft Edge</option>
                </select>
              </div>
            </div>
            <div className="setting-row">
              <div className="setting-info">
                <span className="setting-key">CDP 调试端口</span>
                <span className="setting-desc">浏览器远程调试端口（默认 9222）</span>
              </div>
              <div className="setting-value">
                <input
                  type="number"
                  value={settings.find((s) => s.key === "browser.cdp_port")?.value || "9222"}
                  onChange={(e) => handleSave("browser.cdp_port", e.target.value)}
                  disabled={savingKey === "browser.cdp_port"}
                />
              </div>
            </div>
          </div>
          <div className="browser-info-box">
            <h4><CyberIcon name="logs" size={14} /> 浏览器工具说明</h4>
            <p>AI 可通过以下工具控制浏览器：</p>
            <ul>
              <li><code>browser_open</code> — 打开浏览器并导航到 URL</li>
              <li><code>browser_navigate</code> — 导航到指定页面</li>
              <li><code>browser_screenshot</code> — 截取当前页面</li>
              <li><code>browser_get_content</code> — 获取页面 HTML/文本</li>
              <li><code>browser_click</code> / <code>browser_type</code> — 点击和输入</li>
              <li><code>browser_execute_js</code> — 执行 JavaScript（高风险）</li>
            </ul>
            <p className="hint"><CyberIcon name="alertTriangle" size={14} /> 敏感域名（银行、支付等）会被自动阻止。点击/输入/JS 执行需权限确认。</p>
          </div>
        </div>
      )}

      {/* V20: 技能管理 Tab */}
      {activeTab === "skills" && (
        <div className="section-card">
          <h3>技能管理</h3>
          <p className="section-desc">技能是可扩展的功能包，AI 可以发现并调用。内置技能不可卸载，本地技能可从目录安装。</p>

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
