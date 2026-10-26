import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Check, Code2, Copy, Download, GitFork, Search, Star } from "lucide-react";
import { Page, PageBody, PageHeader, Toolbar, Divider } from "../components/ui/page";
import { Card } from "../components/ui/card";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/form";
import { Segmented } from "../components/ui/primitives";
import { EmptyState, LoadingBlock, Spinner } from "../components/ui/feedback";
import { cn } from "../lib/cn";

/**
 * GitHub 趋势。
 *
 * 后端一次调用同时做两件事：拉取并落库成趋势快照。所以这个页面不需要
 * 本地状态持久化——重开应用重新拉一次，历史对比由库里那张表负责。
 */

interface Repo {
  name: string | null;
  full_name: string | null;
  description: string | null;
  html_url: string | null;
  stars: number | null;
  forks: number | null;
  language: string | null;
  owner: string | null;
  updated_at: string | null;
}

interface TrendingResponse {
  repos: Repo[];
  count: number;
  range: string;
}

type Range = "daily" | "weekly" | "monthly";

const RANGE_LABEL: Record<string, string> = {
  daily: "每日",
  weekly: "每周",
  monthly: "每月",
};

/** 万级数字压成 12.3k，表格里不出现 7 位数 */
function compact(n: number | null): string {
  if (n === null || n === undefined) return "0";
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}m`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

function fmtUpdated(raw: string | null): string {
  if (!raw) return "—";
  const d = new Date(raw);
  if (Number.isNaN(d.getTime())) return raw;
  const days = Math.floor((Date.now() - d.getTime()) / 86_400_000);
  if (days <= 0) return "今天";
  if (days === 1) return "昨天";
  if (days < 30) return `${days} 天前`;
  if (days < 365) return `${Math.floor(days / 30)} 个月前`;
  return `${Math.floor(days / 365)} 年前`;
}

export default function GitHub() {
  const [range, setRange] = useState<Range>("daily");
  const [language, setLanguage] = useState("");
  const [repos, setRepos] = useState<Repo[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fetched, setFetched] = useState<{ range: string; at: Date } | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

  async function handleFetch() {
    setLoading(true);
    setError(null);
    try {
      const resp = await invoke<TrendingResponse>("fetch_github_trending", {
        range,
        language: language.trim() || null,
        limit: 20,
      });
      setRepos(resp.repos ?? []);
      setFetched({ range: resp.range ?? range, at: new Date() });
    } catch (e) {
      setError(typeof e === "string" ? e : "拉取失败");
    } finally {
      setLoading(false);
    }
  }

  async function copyUrl(url: string) {
    try {
      await navigator.clipboard.writeText(url);
      setCopied(url);
      window.setTimeout(() => setCopied((c) => (c === url ? null : c)), 1600);
    } catch {
      /* 剪贴板不可用时静默失败：URL 本身是可选中复制的文本 */
    }
  }

  return (
    <Page wide>
      <PageHeader
        title="GitHub 趋势"
        icon={Code2}
        badge={repos.length > 0 ? <Badge tone="brand">{repos.length} 个仓库</Badge> : undefined}
        description="拉取近期活跃的开源仓库。每次拉取同时写一份趋势快照进库，方便之后对比同一语言的热度变化。"
        actions={
          <Button onClick={handleFetch} loading={loading} size="sm">
            {!loading && <Download />}
            {loading ? "拉取中" : "拉取趋势"}
          </Button>
        }
      />

      <PageBody>
        <Toolbar>
          <Segmented
            value={range}
            onChange={setRange}
            options={[
              { value: "daily" as Range, label: "每日" },
              { value: "weekly" as Range, label: "每周" },
              { value: "monthly" as Range, label: "每月" },
            ]}
          />
          <div className="relative w-56 max-w-full">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-[var(--color-subtle-foreground)]" />
            <Input
              value={language}
              onChange={(e) => setLanguage(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !loading) void handleFetch();
              }}
              placeholder="限定语言，如 rust / python"
              className="pl-8"
            />
          </div>
          {language.trim() && (
            <Badge tone="outline" className="font-mono">
              {language.trim()}
            </Badge>
          )}
          <span className="ml-auto text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
            {fetched
              ? `${RANGE_LABEL[fetched.range] ?? fetched.range} · ${fetched.at.toLocaleTimeString("zh-CN", {
                  hour12: false,
                })} 拉取`
              : "尚未拉取"}
          </span>
        </Toolbar>

        {error && (
          <Card className="mb-4 border-[var(--color-danger)]/30 p-4">
            <div className="text-[var(--text-xs)] font-medium text-[var(--color-danger)]">拉取失败</div>
            <p className="mt-1 break-all text-[var(--text-2xs)] leading-relaxed text-[var(--color-muted-foreground)]">
              {error}
            </p>
            <p className="mt-2 text-[var(--text-2xs)] leading-relaxed text-[var(--color-subtle-foreground)]">
              这个接口会打网络。如果一直失败，先确认网络是否可达 github.com，以及设置里的网络代理是否配好。
            </p>
          </Card>
        )}

        {loading && repos.length === 0 ? (
          <LoadingBlock label="正在拉取趋势" />
        ) : repos.length === 0 ? (
          <Card>
            <EmptyState
              icon={Star}
              title={error ? "没有拿到数据" : "还没有拉取过"}
              description={
                error
                  ? "修好网络后重新点「拉取趋势」。"
                  : "选好时间范围（可选填语言），点右上角「拉取趋势」。结果会同时存一份快照。"
              }
            />
          </Card>
        ) : (
          <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
            {repos.map((r, i) => {
              const url = r.html_url;
              return (
                <Card key={`${r.full_name ?? r.name ?? i}`} className="p-3.5">
                  <div className="flex items-start gap-2.5">
                    <span
                      className={cn(
                        "num-tabular mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-[6px] text-[var(--text-2xs)] font-semibold",
                        i < 3
                          ? "bg-[var(--color-brand-soft)] text-[var(--color-brand)]"
                          : "bg-[var(--color-elevated)] text-[var(--color-subtle-foreground)]"
                      )}
                    >
                      {i + 1}
                    </span>

                    <div className="min-w-0 flex-1">
                      <div className="flex items-baseline justify-between gap-2">
                        <span className="truncate text-[var(--text-sm)] font-medium text-[var(--color-foreground)]">
                          {r.full_name || r.name || "未知仓库"}
                        </span>
                        <span className="flex shrink-0 items-center gap-1 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                          <Star className="size-3 text-[var(--color-warning)]" />
                          <span className="num-tabular">{compact(r.stars)}</span>
                        </span>
                      </div>

                      {r.description ? (
                        <p className="mt-1 line-clamp-2 text-[var(--text-xs)] leading-relaxed text-[var(--color-muted-foreground)]">
                          {r.description}
                        </p>
                      ) : (
                        <p className="mt-1 text-[var(--text-2xs)] italic text-[var(--color-subtle-foreground)]">
                          无描述
                        </p>
                      )}

                      <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                        {r.language && (
                          <span className="inline-flex items-center gap-1.5">
                            <span className="size-1.5 rounded-full bg-[var(--color-brand)]" />
                            {r.language}
                          </span>
                        )}
                        <span className="inline-flex items-center gap-1">
                          <GitFork className="size-2.5" />
                          <span className="num-tabular">{compact(r.forks)}</span>
                        </span>
                        <span>更新于 {fmtUpdated(r.updated_at)}</span>
                      </div>

                      {url && (
                        <>
                          <Divider className="my-2.5" />
                          <div className="flex items-center gap-1.5">
                            <span className="min-w-0 flex-1 truncate font-mono text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                              {url}
                            </span>
                            <Button
                              variant="ghost"
                              size="xs"
                              onClick={() => void copyUrl(url)}
                              title="复制仓库地址"
                            >
                              {copied === url ? (
                                <>
                                  <Check className="size-3 text-[var(--color-success)]" />
                                  已复制
                                </>
                              ) : (
                                <>
                                  <Copy className="size-3" />
                                  复制
                                </>
                              )}
                            </Button>
                          </div>
                        </>
                      )}
                    </div>
                  </div>
                </Card>
              );
            })}
          </div>
        )}

        {loading && repos.length > 0 && (
          <div className="mt-3 flex items-center gap-2 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
            <Spinner size={12} />
            正在拉取新一批
          </div>
        )}
      </PageBody>
    </Page>
  );
}
