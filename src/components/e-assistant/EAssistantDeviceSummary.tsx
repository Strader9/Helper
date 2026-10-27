import { useCallback, useEffect, useMemo, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ChevronRight, Radio, Smartphone } from 'lucide-react';

export interface PairedDeviceSummary {
  id: string;
  name: string;
  platform: string | null;
  paired_at: string;
  last_seen_at: string | null;
  revoked: boolean;
}
export interface DeviceGatewaySummary {
  running: boolean;
  bound: string | null;
  last_error: string | null;
  requests: number;
}

export function isDeviceRecentlyOnline(device: PairedDeviceSummary, now = Date.now()): boolean {
  if (device.revoked || !device.last_seen_at) return false;
  const raw = device.last_seen_at.includes('T')
    ? device.last_seen_at
    : `${device.last_seen_at.replace(' ', 'T')}Z`;
  const timestamp = Date.parse(raw);
  return Number.isFinite(timestamp) && now - timestamp <= 5 * 60 * 1000;
}

interface EAssistantDeviceSummaryProps {
  onOpen: () => void;
}

/** 首页底部只展示多端概况；配对、撤销和诊断仍进入完整设备页处理。 */
export function EAssistantDeviceSummary({ onOpen }: EAssistantDeviceSummaryProps) {
  const [devices, setDevices] = useState<PairedDeviceSummary[]>([]);
  const [gateway, setGateway] = useState<DeviceGatewaySummary | null>(null);
  const [loaded, setLoaded] = useState(false);

  const refresh = useCallback(async () => {
    const [deviceRows, gatewayStatus] = await Promise.all([
      invoke<PairedDeviceSummary[]>('list_paired_devices').catch(() => []),
      invoke<DeviceGatewaySummary>('get_gateway_status').catch(() => null),
    ]);
    setDevices(deviceRows);
    setGateway(gatewayStatus);
    setLoaded(true);
  }, []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 30_000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const activeCount = useMemo(() => devices.filter((device) => !device.revoked).length, [devices]);
  const onlineCount = useMemo(() => devices.filter((device) => isDeviceRecentlyOnline(device)).length, [devices]);
  const healthy = gateway?.running && !gateway.last_error;

  return (
    <button
      type="button"
      className="mission-device-summary"
      onClick={onOpen}
      title="打开设备与多端连接"
      aria-label={`设备：${activeCount} 台已授权，${onlineCount} 台最近在线`}
    >
      <span className="mission-device-summary__icon"><Smartphone size={14} /></span>
      <span className="mission-device-summary__body">
        <strong>多端连接</strong>
        <small>
          {!loaded ? '正在读取…' : healthy ? `${activeCount} 台设备 · ${onlineCount} 台在线` : '网关未运行'}
        </small>
      </span>
      <span className={`mission-device-summary__signal ${healthy ? 'is-online' : ''}`}>
        <Radio size={11} />
      </span>
      <ChevronRight className="mission-device-summary__arrow" size={13} />
    </button>
  );
}
