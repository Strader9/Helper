// LumiCore 风格 —— 设备同步中心
import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { motion } from 'motion/react';
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
} from '../../components/lumi/LumiShared';
import { LumiPageShell } from '../../components/lumi/LumiPageShell';
import { cn } from '../../lib/cn';

/* ── 类型 ── */
interface PairedDevice {
  id: string;
  name: string;
  type: string;
  last_seen: string;
  status: string;
}

interface GatewayStatus {
  connected: boolean;
  device_count: number;
  uptime?: string | null;
}

interface PairingCode {
  code: string;
  expires_at: string;
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

function deviceIcon(type: string) {
  const v = (type || '').toLowerCase();
  if (v.includes('tablet') || v.includes('pad')) return <Tablet size={17} />;
  if (v.includes('watch') || v.includes('wear')) return <Watch size={17} />;
  if (v.includes('desktop') || v.includes('laptop') || v.includes('pc')) return <Monitor size={17} />;
  return <Smartphone size={17} />;
}

function isOnline(s?: string) {
  return ['online', 'connected', 'active', 'paired'].includes((s || '').toLowerCase());
}

/* ── 组件 ── */
export default function LumiDevices() {
  const [devices, setDevices] = useState<PairedDevice[]>([]);
  const [gateway, setGateway] = useState<GatewayStatus | null>(null);
  const [pairing, setPairing] = useState<PairingCode | null>(null);
  const [loading, setLoading] = useState(true);
  const [pairingBusy, setPairingBusy] = useState(false);
  const [revoking, setRevoking] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    const [devRows, gw] = await Promise.all([
      invoke<PairedDevice[]>('list_paired_devices').catch(() => [] as PairedDevice[]),
      invoke<GatewayStatus>('get_gateway_status').catch(() => null),
    ]);
    setDevices(devRows || []);
    setGateway(gw);
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
    } catch {
      setPairing(null);
    }
    setPairingBusy(false);
  };

  const revoke = async (id: string) => {
    setRevoking(id);
    try {
      await invoke('revoke_paired_device', { deviceId: id });
      setDevices((prev) => prev.filter((d) => d.id !== id));
    } catch {
      void load();
    }
    setRevoking(null);
  };

  return (
    <LumiPageShell
      icon={<Smartphone size={18} />}
      title="设备同步"
      subtitle="跨设备配对与网关状态"
      actions={
        <button onClick={load} className="lumi-icon-button" title="刷新">
          {loading ? <LoaderCircle size={15} className="animate-spin" /> : <RefreshCw size={15} />}
        </button>
      }
    >
      {/* 网关状态 + 统计 */}
      <div className="mb-5 grid grid-cols-2 gap-3 md:grid-cols-4">
        <StatCard
          label="网关连接"
          value={gateway?.connected ? '已连接' : '离线'}
          icon={<Wifi size={16} />}
          color={gateway?.connected ? 'text-emerald-400' : 'text-red-400'}
        />
        <StatCard label="已配对设备" value={gateway?.device_count ?? devices.length} icon={<Smartphone size={16} />} />
        <StatCard label="在线设备" value={devices.filter((d) => isOnline(d.status)).length} icon={<Activity size={16} />} color="text-cyan-400" />
        <StatCard label="网关运行" value={gateway?.uptime || '—'} icon={<Globe size={16} />} color="text-celestial-saturn" />
      </div>

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
                <p className="mt-1 text-[11px] text-white/40">在手机或平板上输入配对码，即可接入同一网关。</p>
              </>
            )}
          </div>
          <button onClick={() => void createPairing()} disabled={pairingBusy} className="lumi-button-primary">
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
                      isOnline(d.status)
                        ? 'border-emerald-400/20 bg-emerald-400/10 text-emerald-400'
                        : 'border-white/10 bg-white/5 text-white/35'
                    )}
                  >
                    {deviceIcon(d.type)}
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="text-sm font-bold text-white/90">{d.name}</p>
                    <p className="mt-0.5 text-[11px] text-white/40">
                      {d.type} · 最后在线 {fmtTime(d.last_seen || null)}
                    </p>
                  </div>
                  <Badge variant={isOnline(d.status) ? 'safe' : 'neutral'}>
                    {isOnline(d.status) ? '在线' : '离线'}
                  </Badge>
                  <button
                    onClick={() => void revoke(d.id)}
                    disabled={revoking === d.id}
                    className="inline-flex h-9 items-center gap-1.5 rounded-xl border border-red-500/30 bg-red-500/10 px-3 text-xs font-bold text-red-400 transition-colors hover:bg-red-500/20 disabled:opacity-40"
                  >
                    <Unplug size={13} />
                    {revoking === d.id ? '移除中…' : '撤销'}
                  </button>
                </div>
              </GlassCard>
            </motion.div>
          ))}
        </div>
      )}
    </LumiPageShell>
  );
}
