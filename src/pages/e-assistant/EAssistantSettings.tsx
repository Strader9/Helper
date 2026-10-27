// EAssistantCore 风格设置页 —— 对接 Tauri invoke（get_settings / update_setting / check_ollama 等）
import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { motion } from 'motion/react';
import { toast } from 'sonner';
import {
  Activity,
  BrainCircuit,
  CheckCircle2,
  Cpu,
  Eye,
  EyeOff,
  FolderOpen,
  HardDrive,
  KeyRound,
  Loader2,
  RefreshCw,
  Settings as SettingsIcon,
  Shield,
  Volume2,
  XCircle,
} from 'lucide-react';
import {
  Badge,
  EmptyState,
  GlassCard,
  LoadingSpinner,
  SectionTitle,
  StatCard,
  ToggleSwitch,
} from '../../components/e-assistant/EAssistantShared';
import { EAssistantPageShell } from '../../components/e-assistant/EAssistantPageShell';
import { cn } from '../../lib/cn';
import { applyTheme } from '../../lib/theme';
import { getTts, loadTtsSettings, type TtsSettings, type TtsVoiceInfo } from '../../lib/tts';

/* ── 后端数据类型（防御性镜像，字段全可选以兼容后端演进） ── */
interface SettingRow {
  key: string;
  value: string;
  value_type?: string;
  description?: string | null;
}

interface OllamaModelInfo {
  name: string;
  model?: string;
  size?: number | null;
}

interface SystemMetricsInfo {
  cpu_usage?: number;
  memory_usage_percent?: number;
  memory_total_gb?: number;
  memory_used_gb?: number;
  disks?: Array<{
    drive: string;
    total_gb: number;
    used_gb: number;
    usage_percent: number;
  }>;
  network_ok?: boolean;
  gpu_name?: string | null;
  gpu_usage?: number | null;
}

type TabId = 'general' | 'model' | 'voice' | 'security' | 'secrets' | 'system';

const TABS: Array<{ id: TabId; label: string; icon: ReactNode }> = [
  { id: 'general', label: '通用设置', icon: <SettingsIcon size={15} /> },
  { id: 'model', label: '模型设置', icon: <BrainCircuit size={15} /> },
  // 语音分区：音色 / 语速 / 试听。没有这块的话，Windows 默认中文音色语速偏快，
  // 长回复听不清，也无法换人声。
  { id: 'voice', label: '语音朗读', icon: <Volume2 size={15} /> },
  { id: 'security', label: '安全设置', icon: <Shield size={15} /> },
  // 凭据分区：迁移到 EAssistant 版时这一块整个丢了，导致有 key 也没处填，
  // 媒体生成与邮件因此长期显示"未配置"。这里按用途分组补回。
  { id: 'secrets', label: '凭据配置', icon: <KeyRound size={15} /> },
  { id: 'system', label: '系统信息', icon: <Activity size={15} /> },
];

/** 设置行：左文右控件。 */
function SettingRowItem({
  label,
  description,
  control,
}: {
  label: string;
  description?: string;
  control: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4 border-b border-white/5 py-3.5 last:border-b-0">
      <div className="min-w-0">
        <p className="text-sm font-bold text-white/85">{label}</p>
        {description && <p className="mt-0.5 text-xs leading-relaxed text-white/40">{description}</p>}
      </div>
      <div className="shrink-0">{control}</div>
    </div>
  );
}

const SELECT_CLASS =
  'rounded-lg border border-white/10 bg-black/40 px-3 py-1.5 text-xs font-bold text-white/85 outline-none transition-colors hover:border-white/20 focus:border-celestial-saturn/50';

const INPUT_CLASS =
  'w-64 rounded-lg border border-white/10 bg-black/40 px-3 py-1.5 text-xs text-white/85 outline-none transition-colors placeholder:text-white/25 hover:border-white/20 focus:border-celestial-saturn/50';

/** 凭据字段：与后端 settings 表的 key 一一对应。 */
type SecretField = {
  key: string;
  label: string;
  kind?: 'password' | 'text' | 'number';
  placeholder?: string;
};

/** 按用途分组。顺序即渲染顺序，组内顺序也刻意按"填写的自然次序"排。 */
const SECRET_GROUPS: Array<{ title: string; note: string; fields: SecretField[] }> = [
  {
    title: '图像生成',
    note: '形象设计室出图走 OpenAI 兼容的 POST {base_url}/images/generations。火山方舟填 https://ark.cn-beijing.volces.com/api/v3。',
    fields: [
      { key: 'image.base_url', label: '服务地址', kind: 'text', placeholder: 'https://ark.cn-beijing.volces.com/api/v3' },
      { key: 'image.api_key', label: 'API Key', kind: 'password' },
      { key: 'image.model', label: '模型 ID', kind: 'text', placeholder: 'doubao-seedream-5-0-pro-260628' },
      { key: 'image.size', label: '出图尺寸', kind: 'text', placeholder: '1024x1024' },
    ],
  },
  {
    title: '视频生成',
    note: '火山方舟走自有任务协议（/contents/generations/tasks），代码已按域名自动分流，无需额外开关。',
    fields: [
      { key: 'video.base_url', label: '服务地址', kind: 'text', placeholder: 'https://ark.cn-beijing.volces.com/api/v3' },
      { key: 'video.api_key', label: 'API Key', kind: 'password' },
      { key: 'video.model', label: '模型 ID', kind: 'text', placeholder: 'doubao-seedance-2-5-260628' },
      { key: 'video.size', label: '分辨率', kind: 'text', placeholder: '1280x720' },
      { key: 'video.duration_sec', label: '时长（秒）', kind: 'number', placeholder: '4' },
    ],
  },
  {
    title: '邮件 · 发信（SMTP）',
    note: 'QQ 邮箱先在网页版「设置 → 账户」开启 SMTP 服务，再用生成的授权码；登录密码无效。',
    fields: [
      { key: 'mail.smtp_host', label: '服务器', kind: 'text', placeholder: 'smtp.qq.com' },
      { key: 'mail.smtp_port', label: '端口', kind: 'number', placeholder: '587' },
      { key: 'mail.smtp_user', label: '账号（完整邮箱）', kind: 'text', placeholder: 'you@qq.com' },
      { key: 'mail.smtp_password', label: '授权码', kind: 'password' },
      { key: 'mail.from', label: '发件人地址', kind: 'text', placeholder: 'you@qq.com' },
      { key: 'mail.from_name', label: '显示名', kind: 'text', placeholder: '可选' },
    ],
  },
  {
    title: '邮件 · 收信（IMAP）',
    note: '账号与授权码留空时会自动复用上面 SMTP 那一组，同一邮箱通常通用，不必重复填。',
    fields: [
      { key: 'mail.imap_host', label: '服务器', kind: 'text', placeholder: 'imap.qq.com' },
      { key: 'mail.imap_port', label: '端口', kind: 'number', placeholder: '993' },
      { key: 'mail.imap_user', label: '账号', kind: 'text', placeholder: '留空复用 SMTP' },
      { key: 'mail.imap_password', label: '授权码', kind: 'password' },
    ],
  },
  {
    title: '发布通道',
    note: 'GitHub Pages 与 Netlify 二选一即可；令牌泄露后请立即到对应平台吊销重签。',
    fields: [
      { key: 'publish.github_token', label: 'GitHub Token', kind: 'password' },
      { key: 'publish.github_repo', label: 'Pages 仓库名', kind: 'text', placeholder: 'e-assisant-web' },
      { key: 'publish.netlify_token', label: 'Netlify Token', kind: 'password' },
    ],
  },
  {
    title: '云端大模型（可选）',
    note: '留空则全部走本地 Ollama，不产生费用。填了以后 agent 会优先用云端，省显存。',
    fields: [
      { key: 'llm.api_key', label: 'API Key', kind: 'password' },
      { key: 'llm.protocol', label: '协议', kind: 'text', placeholder: 'openai' },
    ],
  },
];

/**
 * 一行凭据输入。
 *
 * 密码类字段刻意用「空输入 + 掩码提示」：后端 get_settings 下发的值本身
 * 已经是掩码（****abcd），若把它回填进受控 input，用户随手一保存就会用
 * 掩码覆盖掉真实凭据。空输入 + "留空不修改" 是唯一不会写坏数据的做法。
 */
function SecretRow({
  field,
  current,
  onSave,
}: {
  field: SecretField;
  current: string;
  onSave: (key: string, value: string) => void;
}) {
  const isSecret = field.kind === 'password';
  const configured = (current ?? '').trim().length > 0;
  const [draft, setDraft] = useState('');
  const [reveal, setReveal] = useState(false);
  const [justSaved, setJustSaved] = useState(false);

  const flash = () => {
    setJustSaved(true);
    window.setTimeout(() => setJustSaved(false), 1500);
  };

  const commitSecret = () => {
    const next = draft.trim();
    if (!next) return;
    onSave(field.key, next);
    setDraft('');
    flash();
  };

  return (
    <div className="flex items-center justify-between gap-4 border-b border-white/5 py-3 last:border-b-0">
      <div className="min-w-0">
        <p className="flex items-center gap-2 text-sm font-bold text-white/85">
          {field.label}
          {isSecret &&
            (configured ? <Badge variant="safe">已配置</Badge> : <Badge variant="warning">未配置</Badge>)}
        </p>
        <p className="mt-0.5 truncate font-mono text-[11px] text-white/25">{field.key}</p>
      </div>

      <div className="flex shrink-0 items-center gap-1.5">
        {isSecret ? (
          <>
            <input
              type={reveal ? 'text' : 'password'}
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') commitSecret();
              }}
              placeholder={
                configured ? `已保存 · 末 ${current.slice(-4)} · 留空不修改` : '粘贴后回车保存'
              }
              className={INPUT_CLASS}
              autoComplete="off"
              spellCheck={false}
            />
            <button
              onClick={() => setReveal((v) => !v)}
              className="rounded-md p-1.5 text-white/35 transition-colors hover:bg-white/5 hover:text-white/70"
              title={reveal ? '隐藏' : '显示'}
              type="button"
            >
              {reveal ? <EyeOff size={14} /> : <Eye size={14} />}
            </button>
            <button
              onClick={commitSecret}
              disabled={!draft.trim()}
              className={cn(
                'rounded-md px-2.5 py-1.5 text-[11px] font-bold transition-colors',
                draft.trim()
                  ? 'bg-celestial-saturn/15 text-celestial-saturn hover:bg-celestial-saturn/25'
                  : 'cursor-not-allowed bg-white/5 text-white/20'
              )}
              type="button"
            >
              保存
            </button>
          </>
        ) : (
          <input
            key={`${field.key}:${current}`}
            type={field.kind === 'number' ? 'number' : 'text'}
            defaultValue={current}
            onBlur={(e) => {
              if (e.target.value !== current) {
                onSave(field.key, e.target.value);
                flash();
              }
            }}
            placeholder={field.placeholder}
            className={INPUT_CLASS}
            spellCheck={false}
          />
        )}
        {justSaved && <CheckCircle2 size={14} className="text-emerald-400" />}
      </div>
    </div>
  );
}

function formatSize(bytes?: number | null): string {
  if (!bytes || bytes <= 0) return '未知大小';
  const gb = bytes / (1024 * 1024 * 1024);
  if (gb >= 1) return `${gb.toFixed(1)} GB`;
  const mb = bytes / (1024 * 1024);
  return `${mb.toFixed(0)} MB`;
}

function formatUptime(totalSeconds: number): string {
  const h = Math.floor(totalSeconds / 3600);
  const m = Math.floor((totalSeconds % 3600) / 60);
  const s = totalSeconds % 60;
  if (h > 0) return `${h}时${m}分`;
  if (m > 0) return `${m}分${s}秒`;
  return `${s}秒`;
}

export default function EAssistantSettings() {
  const [activeTab, setActiveTab] = useState<TabId>('general');
  const [settings, setSettings] = useState<SettingRow[]>([]);
  // 语音朗读配置：TTS 引擎是全局单例，设置改完立即生效（不走后端 settings 表）
  const [ttsCfg, setTtsCfg] = useState<TtsSettings>(() => loadTtsSettings());
  const [ttsVoices, setTtsVoices] = useState<TtsVoiceInfo[]>([]);
  const [ttsTesting, setTtsTesting] = useState(false);
  const [loading, setLoading] = useState(true);

  // 模型页
  const [ollamaUp, setOllamaUp] = useState<boolean | null>(null);
  const [models, setModels] = useState<OllamaModelInfo[]>([]);
  const [loadingModels, setLoadingModels] = useState(false);

  // 系统页
  const [metrics, setMetrics] = useState<SystemMetricsInfo | null>(null);
  const [version, setVersion] = useState<string>('—');
  const [autostartRegistered, setAutostartRegistered] = useState<boolean | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [sessionSeconds, setSessionSeconds] = useState(0);

  /** 从已加载的设置表中取值，找不到给兜底。 */
  const getValue = useCallback(
    (key: string, fallback = ''): string => settings.find((s) => s.key === key)?.value ?? fallback,
    [settings],
  );

  /* ── 语音朗读：加载音色表 + 落盘 ── */
  const tts = getTts();
  const ttsSupported = tts.supported;

  useEffect(() => {
    if (!ttsSupported) return;
    const refresh = () => setTtsVoices(tts.getVoices());
    refresh();
    // 音色表在 WebView 里是异步加载的，首次可能为空，订阅引擎的更新再刷一次
    const off = tts.subscribe(refresh);
    const timer = window.setTimeout(refresh, 800);
    return () => {
      off();
      window.clearTimeout(timer);
    };
  }, [tts, ttsSupported]);

  const updateTts = useCallback(
    (patch: Partial<TtsSettings>) => {
      setTtsCfg(tts.updateSettings(patch));
    },
    [tts],
  );

  const zhVoiceCount = ttsVoices.filter((v) => v.lang.toLowerCase().startsWith('zh')).length;

  /** 只在后端确认保存成功后更新界面，避免把失败的修改显示成已生效。 */
  const saveSetting = useCallback((key: string, value: string) => {
    void invoke('update_setting', { key, value })
      .then(async () => {
        setSettings((prev) => {
          const idx = prev.findIndex((s) => s.key === key);
          if (idx < 0) return prev;
          const next = [...prev];
          next[idx] = { ...next[idx], value };
          return next;
        });
        if (key === 'ui.theme') {
          applyTheme(value);
          await emit('theme:changed', value);
        }
        if (key === 'startup.auto_launch') {
          setAutostartRegistered(value === 'true');
        }
      })
      .catch((error) => {
        toast.error(`设置保存失败：${typeof error === 'string' ? error : '未知错误'}`);
      });
  }, []);

  const loadSystemInfo = useCallback(async () => {
    const [m, s] = await Promise.all([
      invoke<SystemMetricsInfo>('get_system_metrics').catch(() => null),
      invoke<{ status?: string; version?: string }>('get_system_status').catch(() => null),
    ]);
    if (m) setMetrics(m);
    if (s?.version) setVersion(s.version);
  }, []);

  const loadModels = useCallback(async () => {
    setLoadingModels(true);
    try {
      const [up, list] = await Promise.all([
        invoke<boolean>('check_ollama').catch(() => false),
        invoke<OllamaModelInfo[]>('get_ollama_models').catch(() => [] as OllamaModelInfo[]),
      ]);
      setOllamaUp(up);
      setModels(list);
    } finally {
      setLoadingModels(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const rows = await invoke<SettingRow[]>('get_settings');
        if (!cancelled) setSettings(rows);
      } catch {
        if (!cancelled) setSettings([]);
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    void loadSystemInfo();
    void loadModels();
    invoke<boolean | null>('check_autostart_status').then(setAutostartRegistered).catch(() => setAutostartRegistered(null));
    return () => {
      cancelled = true;
    };
  }, [loadSystemInfo, loadModels]);

  // 本次会话计时（后端未提供全局 uptime，降级为本进程运行时长）
  useEffect(() => {
    const timer = setInterval(() => setSessionSeconds((v) => v + 1), 1000);
    return () => clearInterval(timer);
  }, []);

  const handleRefresh = async () => {
    setRefreshing(true);
    try {
      await Promise.all([loadSystemInfo(), loadModels()]);
    } finally {
      setRefreshing(false);
    }
  };

  const currentModel = getValue('ollama.model', 'qwen3:8b');
  const currentTheme = getValue('ui.theme', 'dark');

  return (
    <EAssistantPageShell
      icon={<SettingsIcon size={18} />}
      title="系统"
      subtitle="外观、模型连接、安全策略与运行状态"
      actions={
        <button
          onClick={() => void handleRefresh()}
          className="flex h-9 items-center gap-2 rounded-xl border border-white/10 bg-white/5 px-3 text-xs font-bold text-white/60 transition-colors hover:bg-white/10 hover:text-white"
        >
          {refreshing ? <Loader2 size={14} className="animate-spin" /> : <RefreshCw size={14} />}
          刷新
        </button>
      }
    >
      {loading ? (
        <LoadingSpinner label="正在加载配置…" />
      ) : (
        <div className="flex min-h-0 gap-5">
          {/* 左侧 Tab 栏（EAssistantCore sidebar 风格） */}
          <div className="w-36 shrink-0 md:w-44">
            <div className="space-y-1">
              {TABS.map((tab) => {
                const active = activeTab === tab.id;
                return (
                  <button
                    key={tab.id}
                    onClick={() => setActiveTab(tab.id)}
                    className={cn(
                      'flex w-full items-center gap-2.5 rounded-xl border px-3 py-2.5 text-left text-xs font-bold transition-all',
                      active
                        ? 'border-celestial-saturn/40 bg-celestial-saturn/10 text-celestial-saturn shadow-[0_0_20px_rgba(255,204,0,0.08)]'
                        : 'border-transparent text-white/45 hover:bg-white/5 hover:text-white/75',
                    )}
                  >
                    {tab.icon}
                    {tab.label}
                  </button>
                );
              })}
            </div>
          </div>

          {/* 右侧内容 */}
          <div className="min-w-0 flex-1 space-y-5">
            {/* ─────── 通用设置 ─────── */}
            {activeTab === 'general' && (
              <motion.div initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} className="space-y-5">
                <SectionTitle label="外观与界面" color="bg-celestial-saturn" />
                <GlassCard hoverEffect={false}>
                  <SettingRowItem
                    label="主题"
                    description="深色 / 浅色界面主题"
                    control={
                      <select
                        className={SELECT_CLASS}
                        value={currentTheme}
                        onChange={(e) => saveSetting('ui.theme', e.target.value)}
                      >
                        <option value="dark">深色</option>
                        <option value="light">浅色</option>
                      </select>
                    }
                  />
                </GlassCard>

                <SectionTitle label="启动与托盘" color="bg-cyan-400" />
                <GlassCard hoverEffect={false}>
                  <SettingRowItem
                    label="开机自启"
                    description={
                      autostartRegistered === true
                        ? '系统已注册开机启动项'
                        : autostartRegistered === false
                          ? '系统未注册开机启动项，修改后将写入注册表'
                          : '管理开机时自动运行助手'
                    }
                    control={
                      <div className="flex items-center gap-2">
                        {autostartRegistered !== null && (
                          <Badge variant={autostartRegistered ? 'safe' : 'neutral'}>
                            {autostartRegistered ? '已注册' : '未注册'}
                          </Badge>
                        )}
                        <ToggleSwitch
                          enabled={getValue('startup.auto_launch', 'false') === 'true'}
                          onChange={() =>
                            saveSetting('startup.auto_launch', getValue('startup.auto_launch', 'false') === 'true' ? 'false' : 'true')
                          }
                        />
                      </div>
                    }
                  />
                  <SettingRowItem
                    label="关闭最小化到托盘"
                    description="点关闭按钮时驻留后台而不是退出"
                    control={
                      <ToggleSwitch
                        enabled={getValue('tray.minimize_on_close', 'true') === 'true'}
                        onChange={() =>
                          saveSetting(
                            'tray.minimize_on_close',
                            getValue('tray.minimize_on_close', 'true') === 'true' ? 'false' : 'true',
                          )
                        }
                      />
                    }
                  />
                </GlassCard>
              </motion.div>
            )}

            {/* ─────── 模型设置 ─────── */}
            {activeTab === 'voice' && (
              <motion.div initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} className="space-y-5">
                <SectionTitle label="语音合成" color="bg-celestial-saturn" />
                <GlassCard hoverEffect={false}>
                  {!ttsSupported ? (
                    <div className="py-8 text-center text-xs text-amber-300/80">
                      当前运行环境不支持语音合成
                    </div>
                  ) : (
                    <>
                      <SettingRowItem
                        label="音色"
                        description={
                          zhVoiceCount > 0
                            ? `检测到 ${zhVoiceCount} 个中文音色（来自系统语音服务）`
                            : '未检测到中文音色，会回落到系统默认音色'
                        }
                        control={
                          <select
                            className={SELECT_CLASS}
                            value={ttsCfg.voiceName ?? ''}
                            onChange={(e) => updateTts({ voiceName: e.target.value || null })}
                          >
                            <option value="">自动（优先中文）</option>
                            {ttsVoices.map((v) => (
                              <option key={v.name} value={v.name}>
                                {v.name} · {v.lang}
                                {v.local ? '' : ' · 在线'}
                              </option>
                            ))}
                          </select>
                        }
                      />
                      <SettingRowItem
                        label="语速"
                        description={`当前 ${ttsCfg.rate.toFixed(2)}x，建议 1.0 ~ 1.2`}
                        control={
                          <input
                            type="range"
                            min={0.6}
                            max={1.8}
                            step={0.05}
                            value={ttsCfg.rate}
                            onChange={(e) => updateTts({ rate: Number(e.target.value) })}
                            className="h-1.5 w-40 cursor-pointer appearance-none rounded-full bg-white/15"
                          />
                        }
                      />
                      <SettingRowItem
                        label="音量"
                        description={`${Math.round(ttsCfg.volume * 100)}%`}
                        control={
                          <input
                            type="range"
                            min={0}
                            max={1}
                            step={0.05}
                            value={ttsCfg.volume}
                            onChange={(e) => updateTts({ volume: Number(e.target.value) })}
                            className="h-1.5 w-40 cursor-pointer appearance-none rounded-full bg-white/15"
                          />
                        }
                      />
                      <SettingRowItem
                        label="单句长度"
                        description={`超过 ${ttsCfg.maxChars} 字自动切句。太长会被系统静默截断，太短会念得断断续续`}
                        control={
                          <select
                            className={SELECT_CLASS}
                            value={String(ttsCfg.maxChars)}
                            onChange={(e) => updateTts({ maxChars: Number(e.target.value) })}
                          >
                            {[50, 70, 90, 120, 160].map((n) => (
                              <option key={n} value={n}>
                                {n} 字
                              </option>
                            ))}
                          </select>
                        }
                      />
                      <SettingRowItem
                        label="试听"
                        description="用当前音色与语速念一段示例，确认效果"
                        control={
                          <button
                            onClick={() => {
                              if (ttsTesting) {
                                tts.stop();
                                setTtsTesting(false);
                                return;
                              }
                              setTtsTesting(true);
                              const ok = tts.speak(
                                '语音已经接通。这是一段试听，用来确认音色和语速是否合适。',
                                { onEnd: () => setTtsTesting(false) },
                              );
                              if (!ok) setTtsTesting(false);
                            }}
                            className={cn(
                              'flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-xs font-bold transition-all',
                              ttsTesting
                                ? 'border-celestial-saturn/40 bg-celestial-saturn/15 text-celestial-saturn'
                                : 'border-white/10 bg-white/5 text-white/60 hover:bg-white/10 hover:text-white/90',
                            )}
                          >
                            <Volume2 size={13} />
                            {ttsTesting ? '停止' : '试听'}
                          </button>
                        }
                      />
                    </>
                  )}
                </GlassCard>
                <p className="px-1 text-[11px] leading-relaxed text-white/30">
                  朗读前会清洗 Markdown 标记（代码块、链接、表格、强调符），再按句切分逐句朗读，
                  并在后台周期性唤醒语音引擎，避免长回复念到一半停住。
                  聊天页每条回复悬停时会出现「朗读」按钮。
                </p>
              </motion.div>
            )}

            {activeTab === 'model' && (
              <motion.div initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} className="space-y-5">
                <SectionTitle label="Ollama 连接" color="bg-celestial-saturn" />
                <GlassCard hoverEffect={false}>
                  <div className="mb-4 flex items-center justify-between">
                    <div>
                      <p className="text-sm font-bold text-white/85">服务状态</p>
                      <p className="mt-0.5 font-mono text-xs text-white/40">{getValue('ollama.base_url', 'http://localhost:11434')}</p>
                    </div>
                    {ollamaUp === null ? (
                      <Badge variant="neutral">检测中…</Badge>
                    ) : ollamaUp ? (
                      <Badge variant="safe">
                        <span className="mr-1 inline-block h-1.5 w-1.5 animate-pulse rounded-full bg-emerald-400" />
                        已连接
                      </Badge>
                    ) : (
                      <Badge variant="danger">
                        <XCircle size={10} className="mr-1" />
                        未连接
                      </Badge>
                    )}
                  </div>
                  <SettingRowItem
                    label="当前模型"
                    description="Agent 推理使用的本地模型"
                    control={
                      <select
                        className={SELECT_CLASS}
                        value={currentModel}
                        onChange={(e) => saveSetting('ollama.model', e.target.value)}
                      >
                        <option value={currentModel}>{currentModel}</option>
                        {models
                          .filter((m) => m.name !== currentModel)
                          .map((m) => (
                            <option key={m.name} value={m.name}>
                              {m.name}
                            </option>
                          ))}
                      </select>
                    }
                  />
                </GlassCard>

                <SectionTitle label={`本地模型库 (${models.length})`} color="bg-cyan-400" />
                {loadingModels ? (
                  <LoadingSpinner className="min-h-[160px]" label="正在拉取模型列表…" />
                ) : models.length === 0 ? (
                  <GlassCard hoverEffect={false}>
                    <EmptyState
                      icon={<BrainCircuit size={28} />}
                      title={ollamaUp ? '本地模型库为空' : 'Ollama 未运行'}
                      description={
                        ollamaUp
                          ? '请先通过 ollama pull 拉取一个模型（如 qwen3:8b）'
                          : '请启动 Ollama 服务后再刷新此页面'
                      }
                    />
                  </GlassCard>
                ) : (
                  <div className="grid gap-3 md:grid-cols-2">
                    {models.map((m) => {
                      const active = m.name === currentModel;
                      return (
                        <button
                          key={m.name}
                          onClick={() => saveSetting('ollama.model', m.name)}
                          className={cn(
                            'glass-panel rounded-2xl p-4 text-left transition-all hover:border-celestial-saturn/30',
                            active && 'border-celestial-saturn/50 shadow-[0_0_20px_rgba(255,204,0,0.1)]',
                          )}
                        >
                          <div className="flex items-center justify-between">
                            <p className="truncate font-mono text-xs font-bold text-white/85">{m.name}</p>
                            {active ? (
                              <Badge variant="saturn">使用中</Badge>
                            ) : (
                              <Badge variant="neutral">可切换</Badge>
                            )}
                          </div>
                          <p className="mt-1.5 text-[11px] text-white/35">{formatSize(m.size ?? null)}</p>
                        </button>
                      );
                    })}
                  </div>
                )}
              </motion.div>
            )}

            {/* ─────── 安全设置 ─────── */}
            {activeTab === 'security' && (
              <motion.div initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} className="space-y-5">
                <SectionTitle label="风险与执行策略" color="bg-celestial-saturn" />
                <GlassCard hoverEffect={false}>
                  <SettingRowItem
                    label="MEDIUM 风险确认"
                    description="执行中等风险操作前弹窗向用户确认"
                    control={
                      <ToggleSwitch
                        enabled={getValue('security.confirm_medium_risk', 'true') === 'true'}
                        onChange={() =>
                          saveSetting(
                            'security.confirm_medium_risk',
                            getValue('security.confirm_medium_risk', 'true') === 'true' ? 'false' : 'true',
                          )
                        }
                      />
                    }
                  />
                  <SettingRowItem
                    label="常驻调度器"
                    description="允许后台定时任务按计划自动执行"
                    control={
                      <ToggleSwitch
                        enabled={getValue('agent.scheduler_enabled', 'true') === 'true'}
                        onChange={() =>
                          saveSetting(
                            'agent.scheduler_enabled',
                            getValue('agent.scheduler_enabled', 'true') === 'true' ? 'false' : 'true',
                          )
                        }
                      />
                    }
                  />
                  <SettingRowItem
                    label="多端设备网关"
                    description="允许手机 / 平板等设备连接本机助手"
                    control={
                      <ToggleSwitch
                        enabled={getValue('agent.gateway_enabled', 'true') === 'true'}
                        onChange={() =>
                          saveSetting(
                            'agent.gateway_enabled',
                            getValue('agent.gateway_enabled', 'true') === 'true' ? 'false' : 'true',
                          )
                        }
                      />
                    }
                  />
                </GlassCard>

                <SectionTitle label="可信目录与工具" color="bg-emerald-400" />
                <GlassCard hoverEffect={false}>
                  <EmptyState
                    icon={<FolderOpen size={28} />}
                    title="目录授权由系统管理"
                    description="默认可信目录：桌面、文档、下载；具体权限在对应操作触发时按需授权"
                  />
                </GlassCard>
              </motion.div>
            )}

            {/* ─────── 系统信息 ─────── */}
            {/* ─────── 凭据配置 ─────── */}
            {activeTab === 'secrets' && (
              <motion.div initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} className="space-y-5">
                <div className="rounded-xl border border-amber-500/20 bg-amber-500/[0.06] px-4 py-3">
                  <p className="text-xs leading-relaxed text-amber-200/70">
                    凭据只写入本机数据库，不会上传。已保存的值以掩码显示——留空保存不会改动原值，
                    只有输入新内容才会覆盖。
                  </p>
                </div>
                {SECRET_GROUPS.map((group) => (
                  <div key={group.title}>
                    <SectionTitle label={group.title} color="bg-celestial-saturn" />
                    <GlassCard hoverEffect={false}>
                      <p className="mb-1 text-[11px] leading-relaxed text-white/35">{group.note}</p>
                      {group.fields.map((field) => (
                        <SecretRow
                          key={field.key}
                          field={field}
                          current={getValue(field.key, '')}
                          onSave={saveSetting}
                        />
                      ))}
                    </GlassCard>
                  </div>
                ))}
              </motion.div>
            )}

            {activeTab === 'system' && (
              <motion.div initial={{ opacity: 0, y: 8 }} animate={{ opacity: 1, y: 0 }} className="space-y-5">
                <SectionTitle label="资源占用" color="bg-celestial-saturn" />
                <div className="grid gap-3 md:grid-cols-3">
                  <StatCard
                    label="CPU 使用率"
                    value={`${Math.round(metrics?.cpu_usage ?? 0)}%`}
                    icon={<Cpu size={18} />}
                    color={(metrics?.cpu_usage ?? 0) > 80 ? 'text-red-400' : 'text-celestial-saturn'}
                  />
                  <StatCard
                    label="内存占用"
                    value={`${Math.round(metrics?.memory_usage_percent ?? 0)}%`}
                    icon={<HardDrive size={18} />}
                    trend={
                      metrics?.memory_total_gb
                        ? `${(metrics.memory_used_gb ?? 0).toFixed(1)} / ${metrics.memory_total_gb.toFixed(1)} GB`
                        : undefined
                    }
                  />
                  <StatCard
                    label="系统盘占用"
                    value={`${Math.round(metrics?.disks?.[0]?.usage_percent ?? 0)}%`}
                    icon={<HardDrive size={18} />}
                    color={(metrics?.disks?.[0]?.usage_percent ?? 0) > 85 ? 'text-red-400' : 'text-cyan-400'}
                  />
                </div>

                <SectionTitle label="运行状态" color="bg-cyan-400" />
                <GlassCard hoverEffect={false}>
                  <div className="grid gap-4 md:grid-cols-2">
                    <div className="flex items-center justify-between rounded-xl bg-white/[0.03] px-4 py-3">
                      <span className="text-xs font-bold text-white/50">应用版本</span>
                      <span className="font-mono text-xs font-black text-celestial-saturn">v{version}</span>
                    </div>
                    <div className="flex items-center justify-between rounded-xl bg-white/[0.03] px-4 py-3">
                      <span className="text-xs font-bold text-white/50">本次会话运行</span>
                      <span className="font-mono text-xs font-black text-white/85">{formatUptime(sessionSeconds)}</span>
                    </div>
                    <div className="flex items-center justify-between rounded-xl bg-white/[0.03] px-4 py-3">
                      <span className="text-xs font-bold text-white/50">网络状态</span>
                      {metrics?.network_ok === undefined ? (
                        <Badge variant="neutral">未知</Badge>
                      ) : metrics.network_ok ? (
                        <Badge variant="safe">
                          <CheckCircle2 size={10} className="mr-1" />
                          正常
                        </Badge>
                      ) : (
                        <Badge variant="danger">离线</Badge>
                      )}
                    </div>
                    <div className="flex items-center justify-between rounded-xl bg-white/[0.03] px-4 py-3">
                      <span className="text-xs font-bold text-white/50">GPU</span>
                      <span className="font-mono text-xs text-white/75">
                        {metrics?.gpu_name ?? '未检测到独立 GPU'}
                      </span>
                    </div>
                  </div>
                </GlassCard>
              </motion.div>
            )}
          </div>
        </div>
      )}
    </EAssistantPageShell>
  );
}
