// EAssistantCore 风格 —— 设备同步中心
import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { motion } from 'motion/react';
import { toast } from 'sonner';
import {
  Smartphone,
  Wifi,
  QrCode,
  RefreshCw,
  LoaderCircle,
  Unplug,
  Monitor,
  Tablet,
  Watch,
  Globe,
  Activity,
} from 'lucide-react';
import {
  GlassCard,
  Badge,
  SectionTitle,
  LoadingSpinner,
  EmptyState,
  StatCard,
} from '../../components/e-assistant/EAssistantShared';
import { EAssistantPageShell } from '../../components/e-assistant/EAssistantPageShell';
import { cn } from '../../lib/cn';

/* ── 类型 ── */
interface PairedDevice {
  id: string;
  name: string;
  platform: string | null;
  token_hint: string | null;
  paired_at: string;
  last_seen_at: string | null;
  revoked: boolean;
}

interface GatewayStatus {
  running: boolean;
  bound: string | null;
  last_error: string | null;
  requests: number;
}

interface PairingCode {
  code: string;
  expires_at: string;
  gateway_running: boolean;
  endpoint: string | null;
}

/* ── 工具 ── */
function fmtTime(v?: string | null): string {
  if (!v) return '—';
  try {
    const d = new Date(v);
    if (Number.isNaN(d.getTime())) return String(v);
    return d.toLocaleString('zh-CN', {
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
    });
  } catch {
    return String(v);
  }
}

function deviceIcon(platform?: string | null) {
  const v = (platform || '').toLowerCase();
  if (v.includes('tablet') || v.includes('pad')) return <Tablet size={17} />;
  if (v.includes('watch') || v.includes('wear')) return <Watch size={17} />;
  if (v.includes('desktop') || v.includes('laptop') || v.includes('pc')) return <Monitor size={17} />;
  return <Smartphone size={17} />;
}

function isOnline(device: PairedDevice) {
  if (device.revoked || !device.last_seen_at) return false;
  const raw = device.last_seen_at.includes('T')
    ? device.last_seen_at
    : `${device.last_seen_at.replace(' ', 'T')}Z`;
  const timestamp = Date.parse(raw);
  return Number.isFinite(timestamp) && Date.now() - timestamp <= 5 * 60 * 1000;
}

/* ── 组件 ── */
export default function EAssistantDevices() {
  const [devices, setDevices] = useState<PairedDevice[]>([]);
  const [gateway, setGateway] = useState<GatewayStatus | null>(null);
  const [pairing, setPairing] = useState<PairingCode | null>(null);
  const [loading, setLoading] = useState(true);
  const [pairingBusy, setPairingBusy] = useState(false);
  const [revoking, setRevoking] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [devRows, gw] = await Promise.all([
        invoke<PairedDevice[]>('list_paired_devices'),
        invoke<GatewayStatus>('get_gateway_status'),
      ]);
      setDevices(devRows || []);
      setGateway(gw);
      setLoadError(null);
    } catch (error) {
      setLoadError(typeof error === 'string' ? error : '无法读取多端状态');
    }
    setLoading(false);
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const createPairing = async () => {
    setPairingBusy(true);
    try {
      const code = await invoke<PairingCode>('create_pairing_code');
      setPairing(code);
    } catch (error) {
      setPairing(null);
      toast.error(typeof error === 'string' ? error : '生成配对码失败');
    }
    setPairingBusy(false);
  };

  const revoke = async (id: string) => {
    setRevoking(id);
    try {
      await invoke('revoke_paired_device', { id });
      setDevices((prev) => prev.map((d) => (d.id === id ? { ...d, revoked: true } : d)));
      toast.success('设备授权已撤销');
    } catch (error) {
      toast.error(typeof error === 'string' ? error : '撤销失败');
      void load();
    }
    setRevoking(null);
  };

  return (
    <EAssistantPageShell
      icon={<Smartphone size={18} />}
      title="设备同步"
      subtitle="跨设备配对与网关状态"
      actions={
        <button onClick={load} className="eassistant-icon-button" title="刷新">
          {loading ? <LoaderCircle size={15} className="animate-spin" /> : <RefreshCw size={15} />}
        </button>
      }
    >
      {/* 网关状态 + 统计 */}
      <div className="mb-5 grid grid-cols-2 gap-3 md:grid-cols-4">
        <StatCard
          label="网关连接"
          value={gateway?.running ? '运行中' : '未启动'}
          icon={<Wifi size={16} />}
          color={gateway?.running ? 'text-emerald-400' : 'text-red-400'}
        />
        <StatCard label="有效授权" value={devices.filter((d) => !d.revoked).length} icon={<Smartphone size={16} />} />
        <StatCard label="最近在线" value={devices.filter(isOnline).length} icon={<Activity size={16} />} color="text-cyan-400" />
        <StatCard label="累计请求" value={gateway?.requests ?? 0} icon={<Globe size={16} />} color="text-celestial-saturn" />
      </div>

      {(loadError || gateway?.last_error) && (
        <GlassCard hoverEffect={false} className="mb-5 border-red-500/25 text-xs text-red-300">
          {loadError || `网关错误：${gateway?.last_error}`}
        </GlassCard>
      )}

      {/* 配对码 */}
      <SectionTitle label="快速配对" />
      <GlassCard glowColor="rgba(255,204,0,0.06)" className="mb-5">
        <div className="flex flex-wrap items-center gap-4">
          <div className="flex h-14 w-14 items-center justify-center rounded-2xl border border-celestial-saturn/25 bg-celestial-saturn/10 text-celestial-saturn">
            <QrCode size={26} />
          </div>
          <div className="min-w-0 flex-1">
            {pairing ? (
              <>
                <p className="font-mono text-2xl font-black tracking-[0.3em] text-celestial-saturn">{pairing.code}</p>
                <p className="mt-1 text-[11px] text-white/40">配对码将在 {fmtTime(pairing.expires_at)} 过期</p>
              </>
            ) : (
              <>
                <p className="text-sm font-bold text-white/85">生成新配对码</p>
                <p className="mt-1 text-[11px] text-white/40">
                  在手机或平板客户端输入配对码，即可接入同一网关。
                </p>
              </>
            )}
          </div>
          <button onClick={() => void createPairing()} disabled={pairingBusy} className="eassistant-button-primary">
            <QrCode size={15} />
            {pairingBusy ? '生成中…' : '生成配对码'}
          </button>
        </div>
      </GlassCard>

      {/* 设备列表 */}
      <SectionTitle label="已配对设备" count={devices.length} />

      {loading ? (
        <LoadingSpinner label="正在加载设备…" />
      ) : devices.length === 0 ? (
        <GlassCard hoverEffect={false}>
          <EmptyState
            icon={<Smartphone size={28} />}
            title="还没有配对设备"
            description="生成配对码后，在其他设备上输入即可完成绑定。"
          />
        </GlassCard>
      ) : (
        <div className="flex flex-col gap-2.5">
          {devices.map((d, i) => (
            <motion.div
              key={d.id}
              initial={{ opacity: 0, y: 8 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ delay: Math.min(i * 0.03, 0.3), duration: 0.25 }}
            >
              <GlassCard hoverEffect={false} className="p-4">
                <div className="flex flex-wrap items-center gap-3">
                  <div
                    className={cn(
                      'flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border',
                      isOnline(d)
                        ? 'border-emerald-400/20 bg-emerald-400/10 text-emerald-400'
                        : 'border-white/10 bg-white/5 text-white/35'
                    )}
                  >
                    {deviceIcon(d.platform)}
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="text-sm font-bold text-white/90">{d.name}</p>
                    <p className="mt-0.5 text-[11px] text-white/40">
                      {d.platform || '未知平台'} · 最后在线 {fmtTime(d.last_seen_at)}
                    </p>
                  </div>
                  <Badge variant={d.revoked ? 'danger' : isOnline(d) ? 'safe' : 'neutral'}>
                    {d.revoked ? '已撤销' : isOnline(d) ? '最近在线' : '已授权'}
                  </Badge>
                  {!d.revoked && (
                    <button
                      onClick={() => void revoke(d.id)}
                      disabled={revoking === d.id}
                      className="inline-flex h-9 items-center gap-1.5 rounded-xl border border-red-500/30 bg-red-500/10 px-3 text-xs font-bold text-red-400 transition-colors hover:bg-red-500/20 disabled:opacity-40"
                    >
                      <Unplug size={13} />
                      {revoking === d.id ? '撤销中…' : '撤销'}
                    </button>
                  )}
                </div>
              </GlassCard>
            </motion.div>
          ))}
        </div>
      )}

      {pairing && (
        <GlassCard hoverEffect={false} glowColor="rgba(110,231,183,0.08)" className="mt-5">
          <div className="flex flex-wrap items-center justify-between gap-4">
            <div>
              <p className="text-[11px] text-white/40">网关地址</p>
              <p className="mt-1 font-mono text-xs text-white/75">{pairing.endpoint || '未监听'}</p>
            </div>
            <Badge variant={pairing.gateway_running ? 'safe' : 'danger'}>
              {pairing.gateway_running ? '可以配对' : '网关未启动，当前无法配对'}
            </Badge>
          </div>
        </GlassCard>
      )}
    </EAssistantPageShell>
  );
}
