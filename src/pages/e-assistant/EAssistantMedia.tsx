import { useCallback, useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { toast } from 'sonner';
import {
  Activity,
  Check,
  Eye,
  EyeOff,
  Film,
  Image as ImageIcon,
  MessageSquare,
  MonitorUp,
  MoveDiagonal,
  Palette,
  Pin,
  RefreshCw,
  ShieldCheck,
  Sparkles,
  Trash2,
  Upload,
  WandSparkles,
} from 'lucide-react';
import { EAssistantPageShell } from '../../components/e-assistant/EAssistantPageShell';
import { GlassCard, LoadingSpinner, SectionTitle } from '../../components/e-assistant/EAssistantShared';
import { cn } from '../../lib/cn';

type MiniSkin = string;
type MiniSize = 'compact' | 'balanced' | 'spacious';

interface MiniWindowConfig {
  skin: MiniSkin;
  size: MiniSize;
  opacity: number;
  always_on_top: boolean;
  show_status: boolean;
  default_expanded: boolean;
  visible: boolean;
}

interface MediaGen {
  id: string;
  kind?: string;
  type?: string;
  prompt: string;
  status: string;
  artifact_path?: string | null;
  url?: string | null;
}

interface MiniSkinAsset {
  id: string;
  name: string;
  kind: 'image' | 'video';
  mime: string;
  data_url: string;
  scale: number;
  offset_y: number;
  playback_rate: number;
  created_at: string;
}

const DEFAULT_CONFIG: MiniWindowConfig = {
  skin: 'guardian',
  size: 'balanced',
  opacity: 0.94,
  always_on_top: true,
  show_status: true,
  default_expanded: false,
  visible: false,
};

const SKINS: Array<{ id: MiniSkin; name: string; description: string; accent: string }> = [
  { id: 'guardian', name: '守护者', description: '动态人物形象', accent: '#f59e0b' },
  { id: 'core', name: '能量核心', description: '低干扰抽象核心', accent: '#818cf8' },
  { id: 'halo', name: '光环哨兵', description: '清爽科技徽记', accent: '#67e8f9' },
];

const SIZES: Array<{ id: MiniSize; label: string; dimensions: string }> = [
  { id: 'compact', label: '紧凑', dimensions: '210 × 250' },
  { id: 'balanced', label: '标准', dimensions: '280 × 380' },
  { id: 'spacious', label: '宽大', dimensions: '360 × 500' },
];

function Toggle({ enabled, onChange }: { enabled: boolean; onChange: (next: boolean) => void }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={enabled}
      onClick={() => onChange(!enabled)}
      className={cn(
        'relative h-6 w-11 shrink-0 rounded-full border transition-colors',
        enabled ? 'border-cyan-300/40 bg-cyan-400/35' : 'border-white/10 bg-white/[0.06]',
      )}
    >
      <span
        className={cn(
          'absolute top-0.5 size-4.5 rounded-full bg-white shadow transition-transform',
          enabled ? 'translate-x-5.5' : 'translate-x-0.5',
        )}
      />
    </button>
  );
}

function ParameterSlider({
  label,
  value,
  min,
  max,
  current,
  onChange,
}: {
  label: string;
  value: string;
  min: number;
  max: number;
  current: number;
  onChange: (value: number) => void;
}) {
  return (
    <label className="block rounded-xl border border-white/[0.06] bg-white/[0.02] p-3">
      <span className="mb-2 flex items-center justify-between gap-2 text-[10px] font-bold text-white/50"><span>{label}</span><b className="tabular-nums text-cyan-300">{value}</b></span>
      <input type="range" min={min} max={max} value={current} onChange={(event) => onChange(Number(event.target.value))} className="w-full accent-cyan-400" />
    </label>
  );
}

function SkinPreview({ skin, custom }: { skin: MiniSkin; custom?: MiniSkinAsset | null }) {
  if (custom) {
    const style = { transform: `translateY(${custom.offset_y}%) scale(${custom.scale})` };
    return custom.kind === 'video' ? (
      <video src={custom.data_url} aria-label={custom.name} autoPlay muted loop playsInline onLoadedMetadata={(event) => { event.currentTarget.playbackRate = custom.playback_rate; }} className="h-40 w-40 select-none object-contain" style={style} />
    ) : (
      <img src={custom.data_url} alt={custom.name} draggable={false} className="h-40 w-40 select-none object-contain" style={style} />
    );
  }
  if (skin === 'guardian') {
    return (
      <img
        src="/assets/avatar/avatar_animated.webp"
        alt="守护者浮窗预览"
        className="h-40 w-40 select-none object-contain drop-shadow-[0_0_22px_rgba(245,158,11,0.42)]"
        draggable={false}
      />
    );
  }
  if (skin === 'core') {
    return (
      <div className="relative grid h-36 w-36 place-items-center rounded-full border border-indigo-300/30 bg-[radial-gradient(circle,rgba(255,255,255,.92)_0_3%,rgba(103,232,249,.7)_5%,rgba(79,70,229,.34)_24%,rgba(15,23,42,.15)_58%,transparent_70%)] shadow-[0_0_42px_rgba(99,102,241,.42)]">
        <span className="absolute inset-5 animate-spin rounded-full border-2 border-cyan-200/70 border-l-violet-400/20 [animation-duration:7s]" />
        <span className="h-12 w-12 rounded-full border border-white/70 bg-[radial-gradient(circle_at_35%_30%,white,#67e8f9_24%,#4338ca_68%)] shadow-[0_0_24px_rgba(103,232,249,.8)]" />
      </div>
    );
  }
  return (
    <div className="relative grid h-36 w-36 place-items-center rounded-full border border-cyan-300/25 bg-[radial-gradient(circle,rgba(8,47,73,.75),rgba(8,15,35,.2)_58%,transparent_72%)] shadow-[0_0_40px_rgba(34,211,238,.3)]">
      <span className="absolute inset-3 animate-spin rounded-full border border-dashed border-cyan-200/65 [animation-duration:11s] [animation-direction:reverse]" />
      <ShieldCheck size={48} className="text-cyan-100 drop-shadow-[0_0_14px_rgba(103,232,249,.8)]" />
    </div>
  );
}

export default function EAssistantMedia() {
  const [config, setConfig] = useState<MiniWindowConfig>(DEFAULT_CONFIG);
  const [savedConfig, setSavedConfig] = useState<MiniWindowConfig>(DEFAULT_CONFIG);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [media, setMedia] = useState<MediaGen[]>([]);
  const [customSkins, setCustomSkins] = useState<MiniSkinAsset[]>([]);
  const [sourcePath, setSourcePath] = useState('');
  const [skinName, setSkinName] = useState('');
  const [scale, setScale] = useState(1);
  const [offsetY, setOffsetY] = useState(0);
  const [playbackRate, setPlaybackRate] = useState(1);
  const [generating, setGenerating] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [nextConfig, rows, assets] = await Promise.all([
        invoke<MiniWindowConfig>('get_mini_window_config'),
        invoke<MediaGen[]>('list_media_generations', { limit: 8 }).catch(() => []),
        invoke<MiniSkinAsset[]>('list_mini_skin_assets').catch(() => []),
      ]);
      setConfig(nextConfig);
      setSavedConfig(nextConfig);
      setMedia(rows ?? []);
      setCustomSkins(assets ?? []);
    } catch (error) {
      toast.error(`浮窗配置加载失败：${typeof error === 'string' ? error : '未知错误'}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow().onDragDropEvent((event) => {
      if (event.payload.type === 'drop' && event.payload.paths[0]) {
        setSourcePath(event.payload.paths[0]);
        setSkinName(event.payload.paths[0].split(/[\\/]/).pop()?.replace(/\.[^.]+$/, '') ?? '自定义皮肤');
      }
    }).then((fn) => { unlisten = fn; }).catch(() => {});
    return () => unlisten?.();
  }, []);

  async function chooseSource() {
    const selected = await open({
      multiple: false,
      directory: false,
      filters: [{ name: '形象素材', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif', 'mp4', 'webm'] }],
    });
    if (typeof selected === 'string') {
      setSourcePath(selected);
      setSkinName(selected.split(/[\\/]/).pop()?.replace(/\.[^.]+$/, '') ?? '自定义皮肤');
    }
  }

  async function generateSkin() {
    if (!sourcePath) {
      toast.error('请先放入一张图片或一段短视频');
      return;
    }
    setGenerating(true);
    try {
      const asset = await invoke<MiniSkinAsset>('import_mini_skin_asset', {
        path: sourcePath,
        name: skinName,
        scale,
        offsetY,
        playbackRate,
      });
      setCustomSkins((items) => [asset, ...items]);
      setConfig((current) => ({ ...current, skin: `custom:${asset.id}` }));
      setSourcePath('');
      toast.success('新皮肤已生成并选中，点击“应用到桌面浮窗”即可使用');
    } catch (error) {
      toast.error(`生成失败：${typeof error === 'string' ? error : '未知错误'}`);
    } finally {
      setGenerating(false);
    }
  }

  async function deleteSkin(asset: MiniSkinAsset) {
    try {
      await invoke('delete_mini_skin_asset', { id: asset.id });
      setCustomSkins((items) => items.filter((item) => item.id !== asset.id));
      if (config.skin === `custom:${asset.id}`) setConfig((current) => ({ ...current, skin: 'guardian' }));
      toast.success('皮肤已移除');
    } catch (error) {
      toast.error(`移除失败：${typeof error === 'string' ? error : '未知错误'}`);
    }
  }

  const selectedCustomSkin = customSkins.find((asset) => config.skin === `custom:${asset.id}`);

  const dirty = useMemo(
    () => JSON.stringify({ ...config, visible: false }) !== JSON.stringify({ ...savedConfig, visible: false }),
    [config, savedConfig],
  );

  async function save(): Promise<boolean> {
    setSaving(true);
    try {
      const updated = await invoke<MiniWindowConfig>('update_mini_window_config', { config });
      setConfig(updated);
      setSavedConfig(updated);
      toast.success('浮窗形象与行为设置已应用');
      return true;
    } catch (error) {
      toast.error(`应用失败：${typeof error === 'string' ? error : '未知错误'}`);
      return false;
    } finally {
      setSaving(false);
    }
  }

  async function setVisible(visible: boolean) {
    try {
      if (dirty && !(await save())) return;
      const actual = await invoke<boolean>('set_mini_window_visible', { visible });
      setConfig((current) => ({ ...current, visible: actual }));
      setSavedConfig((current) => ({ ...current, visible: actual }));
      toast.success(actual ? '桌面浮窗已显示' : '桌面浮窗已隐藏');
    } catch (error) {
      toast.error(`浮窗操作失败：${typeof error === 'string' ? error : '未知错误'}`);
    }
  }

  if (loading) {
    return (
      <EAssistantPageShell icon={<Palette size={19} />} title="形象设计室" subtitle="正在读取浮窗配置">
        <LoadingSpinner label="加载浮窗配置…" />
      </EAssistantPageShell>
    );
  }

  return (
    <EAssistantPageShell
      icon={<Palette size={19} />}
      title="形象设计室"
      subtitle="设计并控制桌面 AI 浮窗"
      actions={
        <button className="eassistant-icon-button" onClick={() => void load()} title="刷新真实状态">
          <RefreshCw size={15} />
        </button>
      }
    >
      <div className="mb-5 grid gap-4 xl:grid-cols-[minmax(320px,0.82fr)_minmax(420px,1.18fr)]">
        <GlassCard hoverEffect={false} className="relative overflow-hidden p-5">
          <div className="absolute inset-0 bg-[radial-gradient(circle_at_50%_42%,rgba(103,232,249,.10),transparent_44%)]" />
          <div className="relative flex min-h-[390px] flex-col">
            <div className="flex items-center justify-between">
              <div>
                <p className="text-xs font-black text-white/85">实时形象预览</p>
                <p className="mt-1 text-[10px] text-white/35">透明背景 · 桌面置顶窗口</p>
              </div>
              <span className={cn('inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-[10px] font-bold', config.visible ? 'border-emerald-400/25 bg-emerald-400/10 text-emerald-300' : 'border-white/10 bg-white/[0.04] text-white/40')}>
                <i className={cn('size-1.5 rounded-full', config.visible ? 'bg-emerald-400' : 'bg-white/25')} />
                {config.visible ? '正在桌面显示' : '当前已隐藏'}
              </span>
            </div>

            <div className="relative my-5 flex flex-1 items-center justify-center overflow-hidden rounded-2xl border border-white/[0.07] bg-[#020611]/70">
              <div className="absolute inset-0 opacity-25 [background-image:linear-gradient(rgba(103,232,249,.12)_1px,transparent_1px),linear-gradient(90deg,rgba(103,232,249,.12)_1px,transparent_1px)] [background-size:24px_24px]" />
              <div style={{ opacity: config.opacity }} className="relative transition-opacity">
                <SkinPreview skin={config.skin} custom={selectedCustomSkin} />
                {config.show_status && (
                  <span className="absolute bottom-1 left-1/2 size-2.5 -translate-x-1/2 rounded-full bg-cyan-300 shadow-[0_0_12px_#67e8f9]" />
                )}
              </div>
            </div>

            <div className="grid grid-cols-2 gap-2">
              <button onClick={() => void setVisible(true)} className="inline-flex h-10 items-center justify-center gap-2 rounded-xl border border-cyan-300/30 bg-cyan-400/15 px-4 text-xs font-black text-cyan-200 transition-colors hover:bg-cyan-400/25">
                <Eye size={14} /> 显示浮窗
              </button>
              <button onClick={() => void setVisible(false)} className="inline-flex h-10 items-center justify-center gap-2 rounded-xl border border-white/10 bg-white/[0.04] px-4 text-xs font-bold text-white/55 transition-colors hover:bg-white/10 hover:text-white/80">
                <EyeOff size={14} /> 隐藏浮窗
              </button>
            </div>
            <p className="mt-3 text-center text-[10px] leading-relaxed text-white/30">
              在头像区域拖动位置；单击形象展开对话；Ctrl + Shift + Space 快速显示或隐藏。
            </p>
          </div>
        </GlassCard>

        <div className="space-y-4">
          <GlassCard hoverEffect={false} className="p-5">
            <SectionTitle label="皮肤库" count={SKINS.length + customSkins.length} />
            <div className="mt-3 grid grid-cols-1 gap-2 sm:grid-cols-3">
              {SKINS.map((skin) => {
                const active = config.skin === skin.id;
                return (
                  <button
                    key={skin.id}
                    onClick={() => setConfig((current) => ({ ...current, skin: skin.id }))}
                    className={cn('relative rounded-xl border p-3 text-left transition-colors', active ? 'border-cyan-300/40 bg-cyan-400/10' : 'border-white/[0.07] bg-white/[0.025] hover:bg-white/[0.055]')}
                    style={{ boxShadow: active ? `inset 0 0 24px ${skin.accent}12` : undefined }}
                  >
                    {active && <Check size={13} className="absolute right-2.5 top-2.5 text-cyan-300" />}
                    <Sparkles size={17} style={{ color: skin.accent }} />
                    <p className="mt-2 text-xs font-black text-white/80">{skin.name}</p>
                    <p className="mt-1 text-[9px] text-white/35">{skin.description}</p>
                  </button>
                );
              })}
            </div>
            {customSkins.length > 0 && (
              <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-3">
                {customSkins.map((asset) => {
                  const active = config.skin === `custom:${asset.id}`;
                  return (
                    <div key={asset.id} className={cn('group/skin relative overflow-hidden rounded-xl border p-2 transition-colors', active ? 'border-cyan-300/40 bg-cyan-400/10' : 'border-white/[0.07] bg-white/[0.025]')}>
                      <button type="button" onClick={() => setConfig((current) => ({ ...current, skin: `custom:${asset.id}` }))} className="w-full text-left">
                        <div className="flex h-20 items-center justify-center overflow-hidden rounded-lg bg-black/30">
                          {asset.kind === 'video' ? <video src={asset.data_url} muted autoPlay loop playsInline className="h-full w-full object-contain" /> : <img src={asset.data_url} alt={asset.name} className="h-full w-full object-contain" />}
                        </div>
                        <p className="mt-2 truncate text-[11px] font-bold text-white/75">{asset.name}</p>
                        <p className="mt-0.5 flex items-center gap-1 text-[9px] text-white/30">{asset.kind === 'video' ? <Film size={10} /> : <ImageIcon size={10} />}{asset.kind === 'video' ? '动态皮肤' : '图片皮肤'}</p>
                      </button>
                      {active && <Check size={13} className="absolute right-2.5 top-2.5 text-cyan-300" />}
                      <button type="button" onClick={() => void deleteSkin(asset)} title="删除皮肤" className="absolute bottom-2 right-2 rounded-md p-1 text-white/25 opacity-0 transition hover:bg-red-400/10 hover:text-red-300 group-hover/skin:opacity-100"><Trash2 size={12} /></button>
                    </div>
                  );
                })}
              </div>
            )}
          </GlassCard>

          <GlassCard hoverEffect={false} className="p-5">
            <SectionTitle label="窗口设置" />
            <div className="mt-4 space-y-5">
              <div>
                <div className="mb-2 flex items-center gap-2 text-[11px] font-bold text-white/65"><MoveDiagonal size={14} />尺寸</div>
                <div className="grid grid-cols-3 gap-2">
                  {SIZES.map((size) => (
                    <button key={size.id} onClick={() => setConfig((current) => ({ ...current, size: size.id }))} className={cn('rounded-lg border px-2 py-2 text-center transition-colors', config.size === size.id ? 'border-violet-400/35 bg-violet-400/12 text-violet-200' : 'border-white/[0.07] bg-white/[0.025] text-white/45')}>
                      <span className="block text-[11px] font-bold">{size.label}</span>
                      <span className="mt-0.5 block text-[8px] opacity-55">{size.dimensions}</span>
                    </button>
                  ))}
                </div>
              </div>

              <label className="block">
                <span className="mb-2 flex items-center justify-between text-[11px] font-bold text-white/65">
                  <span className="flex items-center gap-2"><Activity size={14} />主体透明度</span>
                  <span className="tabular-nums text-cyan-300">{Math.round(config.opacity * 100)}%</span>
                </span>
                <input type="range" min="40" max="100" step="1" value={Math.round(config.opacity * 100)} onChange={(event) => setConfig((current) => ({ ...current, opacity: Number(event.target.value) / 100 }))} className="w-full accent-cyan-400" />
              </label>

              {[
                { key: 'always_on_top' as const, icon: <Pin size={14} />, label: '始终置顶', note: '让助手保持在其他窗口上方' },
                { key: 'show_status' as const, icon: <Activity size={14} />, label: '状态指示', note: '思考和工作时显示光点' },
                { key: 'default_expanded' as const, icon: <MessageSquare size={14} />, label: '启动时展开对话', note: '打开浮窗时直接显示输入面板' },
              ].map((item) => (
                <div key={item.key} className="flex items-center justify-between gap-4 border-t border-white/[0.055] pt-4">
                  <div className="flex items-start gap-2.5">
                    <span className="mt-0.5 text-white/35">{item.icon}</span>
                    <span><strong className="block text-[11px] text-white/70">{item.label}</strong><small className="mt-0.5 block text-[9px] text-white/30">{item.note}</small></span>
                  </div>
                  <Toggle enabled={config[item.key]} onChange={(next) => setConfig((current) => ({ ...current, [item.key]: next }))} />
                </div>
              ))}
            </div>

            <button onClick={() => void save()} disabled={!dirty || saving} className="mt-5 inline-flex h-10 w-full items-center justify-center gap-2 rounded-xl border border-cyan-300/30 bg-cyan-400/15 px-4 text-xs font-black text-cyan-200 transition-colors hover:bg-cyan-400/25 disabled:cursor-not-allowed disabled:opacity-35">
              <MonitorUp size={14} /> {saving ? '正在应用…' : dirty ? '应用到桌面浮窗' : '设置已同步'}
            </button>
          </GlassCard>
        </div>
      </div>

      <SectionTitle label="生成新皮肤" />
      <GlassCard hoverEffect={false} className="mt-3 mb-5 overflow-hidden p-5">
        <div className="grid gap-5 xl:grid-cols-[minmax(260px,.85fr)_minmax(420px,1.15fr)]">
          <button
            type="button"
            onClick={() => void chooseSource()}
            onDragOver={(event) => event.preventDefault()}
            onDrop={(event) => {
              event.preventDefault();
              const file = event.dataTransfer.files[0] as (File & { path?: string }) | undefined;
              if (file?.path) {
                setSourcePath(file.path);
                setSkinName(file.name.replace(/\.[^.]+$/, ''));
              } else if (file) {
                toast.info('当前系统需要点击此区域选择文件；桌面拖入也已支持');
              }
            }}
            className={cn('group flex min-h-44 flex-col items-center justify-center rounded-2xl border border-dashed px-6 text-center transition-colors', sourcePath ? 'border-cyan-300/40 bg-cyan-400/[0.07]' : 'border-white/15 bg-white/[0.025] hover:border-cyan-300/35 hover:bg-cyan-400/[0.05]')}
          >
            <span className="grid size-11 place-items-center rounded-xl border border-cyan-300/20 bg-cyan-400/10 text-cyan-300"><Upload size={20} /></span>
            <strong className="mt-3 text-xs text-white/75">{sourcePath ? '素材已放入' : '拖入图片或短视频'}</strong>
            <span className="mt-1 max-w-full truncate text-[10px] text-white/35">{sourcePath || '或点击选择 PNG / WebP / GIF / MP4 / WebM，最大 50 MB'}</span>
          </button>

          <div className="space-y-4">
            <label className="block">
              <span className="mb-1.5 block text-[10px] font-bold text-white/55">皮肤名称</span>
              <input value={skinName} onChange={(event) => setSkinName(event.target.value)} placeholder="给新皮肤起个名字" maxLength={40} className="h-9 w-full rounded-lg border border-white/10 bg-white/[0.035] px-3 text-xs text-white/75 outline-none transition focus:border-cyan-300/35" />
            </label>
            <div className="grid gap-3 sm:grid-cols-3">
              <ParameterSlider label="主体大小" value={`${Math.round(scale * 100)}%`} min={50} max={200} current={Math.round(scale * 100)} onChange={(value) => setScale(value / 100)} />
              <ParameterSlider label="垂直位置" value={`${offsetY > 0 ? '+' : ''}${Math.round(offsetY)}%`} min={-40} max={40} current={offsetY} onChange={setOffsetY} />
              <ParameterSlider label="播放速度" value={`${playbackRate.toFixed(2)}×`} min={25} max={200} current={Math.round(playbackRate * 100)} onChange={(value) => setPlaybackRate(value / 100)} />
            </div>
            <div className="rounded-xl border border-white/[0.06] bg-white/[0.025] px-3 py-2.5 text-[10px] leading-relaxed text-white/35">
              图片会保留原透明通道；视频将静音循环播放。无透明通道的素材仍会保留原背景，后续可再接入 AI 抠像模型进行自动去背。
            </div>
            <button type="button" onClick={() => void generateSkin()} disabled={!sourcePath || generating} className="inline-flex h-10 w-full items-center justify-center gap-2 rounded-xl border border-violet-300/30 bg-violet-400/15 text-xs font-black text-violet-100 transition hover:bg-violet-400/25 disabled:cursor-not-allowed disabled:opacity-35">
              <WandSparkles size={15} /> {generating ? '正在生成皮肤…' : '生成并加入皮肤库'}
            </button>
          </div>
        </div>
      </GlassCard>

      <SectionTitle label="最近生成的形象素材" count={media.length} />
      <div className="mt-3 grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-4">
        {media.length === 0 ? (
          <GlassCard hoverEffect={false} className="col-span-full flex min-h-24 items-center justify-center p-4 text-xs text-white/35">
            暂无素材。之后通过对话生成的新形象会出现在这里，可再加入皮肤库。
          </GlassCard>
        ) : media.map((item) => (
          <GlassCard key={item.id} className="p-3">
            <div className="flex items-center gap-2"><ImageIcon size={14} className="text-cyan-300" /><span className="text-[10px] font-bold text-white/50">{item.kind || item.type || 'image'}</span></div>
            <p className="mt-2 line-clamp-2 text-[11px] leading-relaxed text-white/65">{item.prompt || '未命名素材'}</p>
            <p className="mt-2 truncate text-[9px] text-white/25">{item.artifact_path || item.url || item.status}</p>
          </GlassCard>
        ))}
      </div>
    </EAssistantPageShell>
  );
}
