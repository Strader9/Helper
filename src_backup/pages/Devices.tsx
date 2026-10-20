import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import { KeyRound, Laptop, RefreshCw, ShieldOff, Smartphone, Wifi } from "lucide-react";
import { Page, PageBody, PageHeader, KeyValue, Divider } from "../components/ui/page";
import { Card, SectionHeader } from "../components/ui/card";
import { Badge, StatusDot } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogBody, DialogFooter } from "../components/ui/primitives";
import { EmptyState, LoadingBlock, BigNumber } from "../components/ui/feedback";

interface Device {
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

export default function Devices() {
  const [devices, setDevices] = useState<Device[] | null>(null);
  const [gateway, setGateway] = useState<GatewayStatus | null>(null);
  const [pairing, setPairing] = useState<PairingCode | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const [d, g] = await Promise.all([
        invoke<Device[]>("list_paired_devices"),
        invoke<GatewayStatus>("get_gateway_status").catch(() => null),
      ]);
      setDevices(d);
      setGateway(g);
    } catch (e) {
      toast.error(typeof e === "string" ? e : "读取设备列表失败");
      setDevices([]);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  async function createCode() {
    setBusy(true);
    try {
      const res = await invoke<PairingCode>("create_pairing_code");
      setPairing(res);
    } catch (e) {
      toast.error(typeof e === "string" ? e : "生成配对码失败");
    } finally {
      setBusy(false);
    }
  }

  async function revoke(device: Device) {
    if (!confirm(`撤销设备「${device.name}」？它的令牌会立即失效。`)) return;
    setBusy(true);
    try {
      await invoke("revoke_paired_device", { id: device.id });
      setDevices((prev) => prev?.map((d) => (d.id === device.id ? { ...d, revoked: true } : d)) ?? prev);
      toast.success("已撤销");
    } catch (e) {
      toast.error(typeof e === "string" ? e : "撤销失败");
    } finally {
      setBusy(false);
    }
  }

  const active = (devices ?? []).filter((d) => !d.revoked);

  return (
    <Page>
      <PageHeader
        title="设备"
        description="手机、平板通过局域网连到这台电脑。网关只监听本机可达的地址，每次请求都要令牌。"
        icon={Smartphone}
        badge={
          gateway && (
            <Badge tone={gateway.running ? "success" : "neutral"} dot pulse={gateway.running}>
              {gateway.running ? "网关运行中" : "网关未启动"}
            </Badge>
          )
        }
        actions={
          <>
            <Button variant="ghost" size="sm" onClick={load} className="gap-1.5">
              <RefreshCw className="size-3.5" />
              刷新
            </Button>
            <Button variant="primary" size="sm" loading={busy} onClick={createCode} className="gap-1.5">
              <KeyRound className="size-3.5" />
              生成配对码
            </Button>
          </>
        }
      />

      <PageBody>
        {/* 网关状态 */}
        <div className="mb-5 grid grid-cols-2 gap-3 lg:grid-cols-4">
          <Card className="p-3.5">
            <div className="flex items-center gap-2">
              <StatusDot tone={gateway?.running ? "success" : "muted"} pulse={gateway?.running} />
              <span className="text-[var(--text-xs)] text-[var(--color-muted-foreground)]">网关</span>
            </div>
            <div className="mt-1.5 text-[var(--text-sm)] font-medium">
              {gateway?.running ? "运行中" : "未启动"}
            </div>
          </Card>
          <Card className="p-3.5">
            <div className="text-[var(--text-xs)] text-[var(--color-muted-foreground)]">监听地址</div>
            <div className="mt-1.5 truncate font-mono text-[var(--text-xs)]">
              {gateway?.bound ?? "—"}
            </div>
          </Card>
          <Card className="p-3.5">
            <BigNumber value={gateway?.requests ?? 0} label="累计请求" />
          </Card>
          <Card className="p-3.5">
            <BigNumber value={active.length} label="在线设备" tone={active.length > 0 ? "success" : "default"} />
          </Card>
        </div>

        {gateway?.last_error && (
          <Card className="mb-4 border-[var(--color-danger)]/30 p-3 text-[var(--text-xs)] text-[var(--color-danger)]">
            网关错误：{gateway.last_error}
          </Card>
        )}

        <SectionHeader
          title="已配对设备"
          description="撤销后令牌立即失效，记录保留用于审计"
          action={<Badge tone="neutral">{devices?.length ?? 0} 条记录</Badge>}
        />

        {devices === null ? (
          <LoadingBlock label="读取设备列表" />
        ) : devices.length === 0 ? (
          <Card>
            <EmptyState
              icon={Laptop}
              title="还没有配对设备"
              description="点右上角「生成配对码」，在手机上输入这串码即可完成配对。码默认 10 分钟内有效，且只能用一次。"
              action={
                <Button size="sm" variant="secondary" loading={busy} onClick={createCode}>
                  <KeyRound className="size-3.5" />
                  生成配对码
                </Button>
              }
            />
          </Card>
        ) : (
          <Card className="overflow-hidden">
            <div className="grid grid-cols-[minmax(0,1.4fr)_100px_120px_120px_88px] items-center gap-3 border-b border-[var(--color-line)] bg-[var(--color-surface)]/60 px-4 py-2">
              <span className="label-caps">设备</span>
              <span className="label-caps">平台</span>
              <span className="label-caps">配对时间</span>
              <span className="label-caps">最后在线</span>
              <span />
            </div>
            {devices.map((d) => (
              <div
                key={d.id}
                className="grid grid-cols-[minmax(0,1.4fr)_100px_120px_120px_88px] items-center gap-3 border-b border-[var(--color-line-faint)] px-4 py-2.5 last:border-0"
              >
                <div className="flex min-w-0 items-center gap-2">
                  <StatusDot tone={d.revoked ? "muted" : d.last_seen_at ? "success" : "warning"} />
                  <div className="min-w-0">
                    <div className="flex items-center gap-1.5">
                      <span className="truncate text-[var(--text-xs)] font-medium">{d.name || d.id.slice(0, 8)}</span>
                      {d.revoked && <Badge tone="danger">已撤销</Badge>}
                    </div>
                    <div className="truncate font-mono text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                      {d.token_hint ?? "—"}
                    </div>
                  </div>
                </div>
                <span className="text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">{d.platform ?? "—"}</span>
                <span className="text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">{d.paired_at?.slice(0, 16)}</span>
                <span className="text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                  {d.last_seen_at?.slice(0, 16) ?? "从未"}
                </span>
                <div className="flex justify-end">
                  {!d.revoked && (
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      className="text-[var(--color-danger)]"
                      disabled={busy}
                      onClick={() => revoke(d)}
                      title="撤销"
                    >
                      <ShieldOff className="size-3.5" />
                    </Button>
                  )}
                </div>
              </div>
            ))}
          </Card>
        )}

        <p className="mt-4 flex items-center gap-1.5 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
          <Wifi className="size-3" />
          配对码是授权凭据，只在生成时显示一次；不要截图发到群里
        </p>
      </PageBody>

      {/* 配对码 */}
      <Dialog open={!!pairing} onOpenChange={(o) => !o && setPairing(null)}>
        <DialogContent size="sm">
          <DialogHeader>
            <DialogTitle>配对码</DialogTitle>
            <DialogDescription>在设备上打开网关地址并输入这串码。一次性有效，过期请重新生成。</DialogDescription>
          </DialogHeader>
          <DialogBody>
            <div className="rounded-[var(--radius-lg)] border border-[var(--color-brand-line)] bg-[var(--color-brand-soft)] px-4 py-5 text-center">
              <div className="num-tabular text-[var(--text-3xl)] font-semibold tracking-[0.18em] text-[var(--color-brand)]">
                {pairing?.code}
              </div>
              <div className="mt-2 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                有效至 {pairing?.expires_at}
              </div>
            </div>
            <Divider className="my-3" />
            <KeyValue label="网关地址" mono>
              {pairing?.endpoint ?? "未监听"}
            </KeyValue>
            <KeyValue label="网关状态">
              <Badge tone={pairing?.gateway_running ? "success" : "warning"} dot>
                {pairing?.gateway_running ? "运行中" : "未启动"}
              </Badge>
            </KeyValue>
            {!pairing?.gateway_running && (
              <p className="mt-2 text-[var(--text-2xs)] leading-relaxed text-[var(--color-warning)]">
                网关当前没在监听，即使拿到码也连不上。先确认设置里开启了多端访问。
              </p>
            )}
          </DialogBody>
          <DialogFooter>
            <Button variant="secondary" size="sm" onClick={() => setPairing(null)}>
              关闭
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Page>
  );
}
