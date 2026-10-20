import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import {
  AlertCircle,
  Check,
  Clock,
  Copy,
  Film,
  FolderOpen,
  Image as ImageIcon,
  RefreshCw,
  Sparkles,
  Wand2,
} from "lucide-react";
import { Page, PageBody, PageHeader, KeyValue, Divider } from "../components/ui/page";
import { Card, SectionHeader } from "../components/ui/card";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Textarea, Field, Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from "../components/ui/form";
import { Segmented } from "../components/ui/primitives";
import { EmptyState, LoadingBlock, Skeleton } from "../components/ui/feedback";
import { PermissionGate } from "../components/PermissionGate";
import { useUiTool } from "../lib/useUiTool";
import { cn } from "../lib/cn";

type Kind = "image" | "video";

interface SavedImage {
  index: number;
  path: string;
  bytes: number;
  width: number;
  height: number;
  format: string;
}

interface ImageResult {
  image_count: number;
  images: SavedImage[];
  failed?: { index: number; error: string }[];
  provider?: string;
  model?: string;
  output_dir?: string;
  ledger_id?: string | null;
}

interface VideoResult {
  task_id: string;
  status: string;
  waiting?: boolean;
  path?: string;
  bytes?: number;
  video_probe?: { duration_sec?: number; width?: number; height?: number } | null;
  provider?: string;
  model?: string;
  next?: string;
  ledger_id?: string | null;
}

interface LedgerRow {
  id: string;
  kind: string;
  provider: string;
  model: string;
  task_id: string | null;
  prompt: string;
  size: string | null;
  duration_sec: number | null;
  status: string;
  artifact_path: string | null;
  artifact_bytes: number | null;
  error: string | null;
  created_at: string;
}

const IMAGE_SIZES = ["1024x1024", "1792x1024", "1024x1792", "1536x1024"];
const VIDEO_SIZES = ["1280x720", "720x1280", "1920x1080"];
const VIDEO_DURATIONS = ["5", "8", "10"];

const fmtBytes = (n?: number | null) => {
  if (!n) return "—";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
};

const statusTone = (s: string) => {
  const v = s.toLowerCase();
  if (v.includes("fail") || v.includes("error") || v.includes("cancel")) return "danger" as const;
  if (v.includes("complete") || v.includes("download") || v.includes("succeed")) return "success" as const;
  if (v.includes("progress") || v.includes("queue") || v.includes("submit")) return "warning" as const;
  return "neutral" as const;
};

function CopyPath({ path }: { path: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(path);
          setDone(true);
          setTimeout(() => setDone(false), 1400);
        } catch {
          toast.error("复制失败，请手动选中路径");
        }
      }}
      className="inline-flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-[6px] text-[var(--color-subtle-foreground)] transition-colors hover:bg-[var(--color-hover)] hover:text-[var(--color-foreground)]"
      title="复制路径"
    >
      {done ? <Check className="size-3.5 text-[var(--color-success)]" /> : <Copy className="size-3.5" />}
    </button>
  );
}

export default function Media() {
  const [kind, setKind] = useState<Kind>("image");
  const [prompt, setPrompt] = useState("");
  const [size, setSize] = useState(IMAGE_SIZES[0]);
  const [count, setCount] = useState("1");
  const [duration, setDuration] = useState("5");
  const [imageResult, setImageResult] = useState<ImageResult | null>(null);
  const [videoResult, setVideoResult] = useState<VideoResult | null>(null);
  const [ledger, setLedger] = useState<LedgerRow[] | null>(null);
  const [service, setService] = useState<Record<string, unknown> | null>(null);
  const [polling, setPolling] = useState(false);

  const { pending, running, error, run, respond, clearError } = useUiTool();

  const loadLedger = useCallback(async () => {
    try {
      const rows = await invoke<LedgerRow[]>("list_media_generations", { limit: 20 });
      setLedger(rows);
    } catch {
      setLedger([]);
    }
  }, []);

  useEffect(() => {
    loadLedger();
    run("media_provider_status", { probe: false })
      .then((r) => {
        if (r?.success) setService(r.data as Record<string, unknown>);
      })
      .catch(() => {});
    // 只在挂载时探一次配置，不随输入变化重复探测
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loadLedger]);

  const imageCfg = (service?.image ?? {}) as Record<string, unknown>;
  const videoCfg = (service?.video ?? {}) as Record<string, unknown>;

  const cfg = kind === "image" ? imageCfg : videoCfg;
  const configured = Boolean(cfg.configured);

  useEffect(() => {
    setSize(kind === "image" ? IMAGE_SIZES[0] : VIDEO_SIZES[0]);
    clearError();
  }, [kind, clearError]);

  async function handleGenerate() {
    const p = prompt.trim();
    if (!p) {
      toast.error("先写一句描述");
      return;
    }
    if (kind === "image") {
      const res = await run("image_generate", {
        prompt: p,
        size,
        n: Number(count),
      });
      if (res?.success) {
        const data = res.data as ImageResult;
        setImageResult(data);
        toast.success(`已生成 ${data.image_count} 张`, {
          description: `落盘目录：${data.output_dir ?? "默认图片目录"}`,
        });
        loadLedger();
      }
    } else {
      const res = await run("video_generate", {
        prompt: p,
        size,
        duration_sec: Number(duration),
        wait: true,
      });
      if (res?.success) {
        const data = res.data as VideoResult;
        setVideoResult(data);
        toast.success(
          data.status === "completed" ? "视频已生成并落盘" : `任务已提交：${data.status}`,
          { description: data.task_id ? `task ${data.task_id}` : undefined }
        );
        loadLedger();
      }
    }
  }

  async function handlePoll(taskId: string) {
    setPolling(true);
    try {
      const res = await run("video_generation_status", { task_id: taskId, save: true });
      if (res?.success) {
        const data = res.data as VideoResult;
        setVideoResult(data);
        if (data.status === "completed") toast.success("视频已下载并校验");
        loadLedger();
      }
    } finally {
      setPolling(false);
    }
  }

  const busy = running !== null;
  const sizes = kind === "image" ? IMAGE_SIZES : VIDEO_SIZES;

  const history = useMemo(
    () => (ledger ?? []).filter((r) => r.kind === kind),
    [ledger, kind]
  );

  return (
    <Page wide>
      <PageHeader
        title="媒体工坊"
        description={
          kind === "image"
            ? "按文字描述生成图片。落盘后会重新读取文件头核对真实尺寸与字节数——HTTP 200 不算完成。"
            : "异步视频任务。提交后进入台账，超时也不会丢 task_id，可随时续查，不必重复付费。"
        }
        icon={Sparkles}
        badge={
          <Badge tone={configured ? "success" : "warning"} dot>
            {configured ? "服务已配置" : "未配置服务商"}
          </Badge>
        }
        actions={
          <Segmented
            value={kind}
            onChange={(v) => setKind(v)}
            options={[
              { value: "image", label: <><ImageIcon className="size-3.5" />图像</> },
              { value: "video", label: <><Film className="size-3.5" />视频</> },
            ]}
          />
        }
      />

      <PageBody>
        {!configured && service && (
          <Card className="mb-4 border-[var(--color-warning)]/30 bg-[var(--color-warning)]/5 p-3.5">
            <div className="flex items-start gap-2.5">
              <AlertCircle className="mt-0.5 size-4 shrink-0 text-[var(--color-warning)]" />
              <div className="min-w-0">
                <p className="text-[var(--text-xs)] font-medium text-[var(--color-foreground)]">
                  {kind === "image" ? "图像" : "视频"}服务未配置，生成会直接失败
                </p>
                <p className="mt-1 text-[var(--text-2xs)] leading-relaxed text-[var(--color-muted-foreground)]">
                  需要填 {(cfg.missing as string[] | undefined)?.join("、") || "base_url / model / api_key"}。
                  任何 OpenAI 兼容端点都行，换服务商只改这几项，不用改代码。
                </p>
                <Button variant="secondary" size="xs" className="mt-2" onClick={() => (window.location.hash = "#/settings")}>
                  去设置
                </Button>
              </div>
            </div>
          </Card>
        )}

        {error && (
          <Card className="mb-4 border-[var(--color-danger)]/30 p-3">
            <div className="flex items-start gap-2">
              <AlertCircle className="mt-0.5 size-3.5 shrink-0 text-[var(--color-danger)]" />
              <p className="min-w-0 flex-1 break-words text-[var(--text-xs)] text-[var(--color-danger)]">{error}</p>
              <Button variant="ghost" size="xs" onClick={clearError}>
                关闭
              </Button>
            </div>
          </Card>
        )}

        <div className="grid grid-cols-1 gap-4 lg:grid-cols-[380px_minmax(0,1fr)]">
          {/* 参数面板 */}
          <Card className="h-fit">
            <SectionHeader className="px-4 pt-4" title="参数" description="描述越具体，结果越可控" />
            <div className="px-4 pb-4 pt-2">
              <Field label="描述" hint="必填">
                <Textarea
                  value={prompt}
                  onChange={(e) => setPrompt(e.target.value)}
                  placeholder={
                    kind === "image"
                      ? "例如：赛博朋克风格的机械猫，霓虹灯反射在湿漉漉的地面上，电影感打光，16:9"
                      : "例如：镜头缓慢推近一座悬浮在云海上的东方古城，晨雾、飞鸟、暖色调"
                  }
                  className="min-h-28"
                  onKeyDown={(e) => {
                    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") handleGenerate();
                  }}
                />
              </Field>

              <Field label="尺寸" className="mt-3.5">
                <Select value={size} onValueChange={setSize}>
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {sizes.map((s) => (
                      <SelectItem key={s} value={s}>
                        {s}
                        {s === "1024x1024" && " · 方图"}
                        {s === "1792x1024" && " · 横图"}
                        {s === "1024x1792" && " · 竖图"}
                        {s === "1280x720" && " · 720p"}
                        {s === "1920x1080" && " · 1080p"}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </Field>

              {kind === "image" ? (
                <Field label="张数" className="mt-3.5" description="每张都单独计费，建议先出 1 张确认风格">
                  <Segmented
                    value={count}
                    onChange={setCount}
                    options={[
                      { value: "1", label: "1" },
                      { value: "2", label: "2" },
                      { value: "3", label: "3" },
                      { value: "4", label: "4" },
                    ]}
                  />
                </Field>
              ) : (
                <Field label="时长" className="mt-3.5" description="服务商对时长有各自的限制，超限会被拒">
                  <Select value={duration} onValueChange={setDuration}>
                    <SelectTrigger>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {VIDEO_DURATIONS.map((d) => (
                        <SelectItem key={d} value={d}>
                          {d} 秒
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </Field>
              )}

              <Button
                variant="primary"
                size="lg"
                className="mt-4 w-full"
                loading={busy}
                onClick={handleGenerate}
                disabled={!prompt.trim()}
              >
                <Wand2 className="size-4" />
                {busy ? "正在生成" : kind === "image" ? "生成图像" : "生成视频"}
              </Button>
              <p className="mt-2 text-center text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                ⌘/Ctrl + Enter 快速生成
              </p>
            </div>
          </Card>

          {/* 结果区 */}
          <div className="flex flex-col gap-4">
            <Card className="min-h-56">
              <SectionHeader
                className="px-4 pt-4"
                title="本次结果"
                description={kind === "image" ? "尺寸与字节数已回读校验" : "任务状态与落盘产物"}
              />
              <div className="px-4 pb-4 pt-2">
                {kind === "image" ? (
                  running === "image_generate" ? (
                    <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
                      {Array.from({ length: Number(count) || 1 }).map((_, i) => (
                        <div key={i} className="aspect-square rounded-[var(--radius-md)]">
                          <Skeleton className="h-full w-full" />
                        </div>
                      ))}
                    </div>
                  ) : !imageResult || imageResult.images.length === 0 ? (
                    <EmptyState
                      compact
                      icon={ImageIcon}
                      title="还没有生成结果"
                      description="左侧写下描述，点「生成图像」。结果会显示真实尺寸与落盘路径。"
                    />
                  ) : (
                    <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
                      {imageResult.images.map((img) => (
                        <div
                          key={img.path}
                          className="rounded-[var(--radius-md)] border border-[var(--color-line)] bg-[var(--color-surface)] p-3"
                        >
                          <div className="mb-2 flex items-center justify-between">
                            <Badge tone="success" dot>
                              已落盘
                            </Badge>
                            <Badge tone="outline">{img.format?.toUpperCase()}</Badge>
                          </div>
                          <div className="num-tabular text-[var(--text-lg)] font-semibold">
                            {img.width} × {img.height}
                          </div>
                          <div className="mt-0.5 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                            {fmtBytes(img.bytes)}
                          </div>
                          <Divider className="my-2.5" />
                          <div className="flex items-start gap-1">
                            <span className="min-w-0 flex-1 break-all font-mono text-[var(--text-2xs)] leading-relaxed text-[var(--color-subtle-foreground)]">
                              {img.path}
                            </span>
                            <CopyPath path={img.path} />
                          </div>
                        </div>
                      ))}
                      {imageResult.failed && imageResult.failed.length > 0 && (
                        <div className="rounded-[var(--radius-md)] border border-[var(--color-danger)]/30 bg-[var(--color-danger)]/5 p-3 sm:col-span-2">
                          <p className="text-[var(--text-xs)] font-medium text-[var(--color-danger)]">
                            {imageResult.failed.length} 张失败
                          </p>
                          {imageResult.failed.map((f) => (
                            <p key={f.index} className="mt-1 break-all text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                              #{f.index + 1}：{f.error}
                            </p>
                          ))}
                        </div>
                      )}
                    </div>
                  )
                ) : running === "video_generate" ? (
                  <LoadingBlock label="正在提交并等待生成（视频通常需要几十秒到几分钟）" />
                ) : !videoResult ? (
                  <EmptyState
                    compact
                    icon={Film}
                    title="还没有视频任务"
                    description="提交后会先拿到 task_id，即使等待超时也能用台账续查，不会重复计费。"
                  />
                ) : (
                  <div className="flex flex-col gap-3">
                    <div className="flex items-center gap-2">
                      <Badge tone={statusTone(videoResult.status)} dot pulse={videoResult.status !== "completed"}>
                        {videoResult.status}
                      </Badge>
                      <span className="truncate font-mono text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                        {videoResult.task_id}
                      </span>
                      {videoResult.status !== "completed" && videoResult.task_id && (
                        <Button
                          size="xs"
                          variant="secondary"
                          className="ml-auto"
                          loading={polling}
                          onClick={() => handlePoll(videoResult.task_id)}
                        >
                          <RefreshCw className="size-3" />
                          续查
                        </Button>
                      )}
                    </div>

                    {videoResult.path ? (
                      <div className="rounded-[var(--radius-md)] border border-[var(--color-line)] bg-[var(--color-surface)] p-3">
                        <KeyValue label="文件大小">{fmtBytes(videoResult.bytes)}</KeyValue>
                        <KeyValue label="实际时长">
                          {videoResult.video_probe?.duration_sec
                            ? `${Number(videoResult.video_probe.duration_sec).toFixed(1)} 秒`
                            : "未核对（未安装 ffprobe）"}
                        </KeyValue>
                        <KeyValue label="分辨率">
                          {videoResult.video_probe?.width
                            ? `${videoResult.video_probe.width} × ${videoResult.video_probe.height}`
                            : "未核对"}
                        </KeyValue>
                        <Divider className="my-2" />
                        <div className="flex items-start gap-1">
                          <span className="min-w-0 flex-1 break-all font-mono text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                            {videoResult.path}
                          </span>
                          <CopyPath path={videoResult.path} />
                        </div>
                      </div>
                    ) : (
                      <div className="rounded-[var(--radius-md)] border border-[var(--color-line)] bg-[var(--color-surface)] p-3 text-[var(--text-2xs)] leading-relaxed text-[var(--color-muted-foreground)]">
                        {videoResult.next ??
                          "任务已提交，尚未完成。视频服务商侧还在渲染，稍后点「续查」；台账里也保留了这条任务。"}
                      </div>
                    )}
                  </div>
                )}
              </div>
            </Card>

            {/* 台账 */}
            <Card>
              <SectionHeader
                className="px-4 pt-4"
                title="生成台账"
                description="跨重启保留。视频任务重提一次就多花一次钱，所以任务 id 必须留下"
                action={
                  <Button variant="ghost" size="xs" onClick={loadLedger} className="gap-1">
                    <RefreshCw className="size-3" />
                    刷新
                  </Button>
                }
              />
              <div className="px-2 pb-2 pt-1">
                {ledger === null ? (
                  <LoadingBlock label="读取台账" />
                ) : history.length === 0 ? (
                  <EmptyState compact icon={Clock} title="暂无记录" description="生成过的任务会出现在这里" />
                ) : (
                  <div className="max-h-72 overflow-y-auto">
                    {history.map((row) => (
                      <div
                        key={row.id}
                        className={cn(
                          "flex items-center gap-2.5 rounded-[7px] px-2 py-2",
                          "transition-colors hover:bg-[var(--color-hover)]/40"
                        )}
                      >
                        <Badge tone={statusTone(row.status)}>{row.status}</Badge>
                        <span className="min-w-0 flex-1 truncate text-[var(--text-xs)] text-[var(--color-foreground)]">
                          {row.prompt}
                        </span>
                        <span className="hidden text-[var(--text-2xs)] text-[var(--color-subtle-foreground)] sm:block">
                          {row.model || "—"}
                        </span>
                        <span className="num-tabular shrink-0 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                          {row.created_at?.slice(5, 16) ?? ""}
                        </span>
                        {row.kind === "video" && row.task_id && row.status !== "completed" && (
                          <Button size="xs" variant="ghost" onClick={() => handlePoll(row.task_id!)}>
                            续查
                          </Button>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </Card>

            <p className="flex items-center gap-1.5 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
              <FolderOpen className="size-3" />
              产物写入默认的图片 / 视频目录下的 PCGuardian 子目录，路径已在上方列出
            </p>
          </div>
        </div>
      </PageBody>

      <PermissionGate request={pending} onRespond={respond} />
    </Page>
  );
}
