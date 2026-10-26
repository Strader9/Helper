import { useCallback, useEffect, useMemo, useState } from "react";
import {
  AlertCircle,
  ArrowLeft,
  Inbox,
  Mail as MailIcon,
  Paperclip,
  RefreshCw,
  Search,
  Settings2,
} from "lucide-react";
import { Page, PageBody, PageHeader, Divider, KeyValue } from "../components/ui/page";
import { Card } from "../components/ui/card";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Input, Switch } from "../components/ui/form";
import { EmptyState, Skeleton } from "../components/ui/feedback";
import { PermissionGate } from "../components/PermissionGate";
import { useUiTool } from "../lib/useUiTool";
import { cn } from "../lib/cn";

interface EmailHeader {
  uid: number;
  subject: string;
  from: string;
  to?: string;
  date: string;
  internal_date?: string | null;
  size_bytes?: number;
  seen: boolean;
  flags?: string[];
  message_id?: string;
}

interface Attachment {
  filename: string;
  content_type: string;
  size_bytes: number;
}

interface MailDetail {
  uid: number;
  subject: string;
  from: string;
  to: string;
  date: string;
  message_id?: string;
  body: string;
  body_source?: string;
  body_total_chars?: number;
  truncated?: boolean;
  attachments: Attachment[];
  seen: boolean;
}

interface ListResult {
  mailbox: string;
  count: number;
  mailbox_total: number;
  emails: EmailHeader[];
  imap_host?: string;
  user?: string;
}

const fmtBytes = (n?: number) => {
  if (!n) return "—";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
};

/** 发件人串通常是 "张三 <a@b.com>"，列表里只显示名字部分 */
function shortFrom(from: string): string {
  const m = from.match(/^\s*"?([^"<]*?)"?\s*<.*>\s*$/);
  return (m?.[1] || from).trim() || from;
}

function shortDate(raw: string): string {
  if (!raw) return "";
  // 常见形如 "Mon, 19 Oct 2026 21:03:00 +0800"
  const m = raw.match(/(\d{1,2})\s+([A-Za-z]{3})\s+(\d{4})/);
  if (m) {
    const [, d, mon, y] = m;
    return `${mon} ${d}${y !== String(new Date().getFullYear()) ? ` ${y}` : ""}`;
  }
  return raw.slice(0, 16);
}

export default function Mail() {
  const [list, setList] = useState<ListResult | null>(null);
  const [mail, setMail] = useState<MailDetail | null>(null);
  const [loadingList, setLoadingList] = useState(true);
  const [loadingMail, setLoadingMail] = useState(false);
  const [unreadOnly, setUnreadOnly] = useState(false);
  const [query, setQuery] = useState("");
  const [service, setService] = useState<Record<string, unknown> | null>(null);
  const [activeUid, setActiveUid] = useState<number | null>(null);

  const { pending, running, error, run, respond, clearError } = useUiTool();

  const loadList = useCallback(
    async (unread = unreadOnly) => {
      setLoadingList(true);
      try {
        const res = await run("recent_emails", { limit: 30, unread_only: unread });
        if (res?.success) setList(res.data as ListResult);
        else setList(null);
      } finally {
        setLoadingList(false);
      }
    },
    [run, unreadOnly]
  );

  useEffect(() => {
    run("media_provider_status", { probe: false })
      .then((r) => {
        if (r?.success) setService(r.data as Record<string, unknown>);
      })
      .catch(() => {});
    loadList(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function openMail(uid: number) {
    setActiveUid(uid);
    setLoadingMail(true);
    setMail(null);
    try {
      const res = await run("read_email", { uid, max_chars: 12000 });
      if (res?.success) {
        setMail(res.data as MailDetail);
        // 读信会标已读，本地列表同步一下状态，避免刷新前显示不一致
        setList((prev) =>
          prev
            ? {
                ...prev,
                emails: prev.emails.map((e) => (e.uid === uid ? { ...e, seen: true } : e)),
              }
            : prev
        );
      }
    } finally {
      setLoadingMail(false);
    }
  }

  const mailCfg = (service?.mail_read ?? {}) as Record<string, unknown>;
  const configured = Boolean(mailCfg.configured);

  const emails = useMemo(() => {
    const rows = list?.emails ?? [];
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter(
      (e) =>
        e.subject?.toLowerCase().includes(q) ||
        e.from?.toLowerCase().includes(q)
    );
  }, [list, query]);

  return (
    <Page wide>
      <PageHeader
        title="邮件"
        description={
          list
            ? `${list.user ?? "邮箱"} · ${list.mailbox} · 共 ${list.mailbox_total} 封`
            : "通过 IMAP 读取收件箱，只拉头部；点开某封时才下载正文"
        }
        icon={MailIcon}
        badge={
          service ? (
            <Badge tone={configured ? "success" : "warning"} dot>
              {configured ? "IMAP 已配置" : "未配置 IMAP"}
            </Badge>
          ) : undefined
        }
        actions={
          <>
            <label className="flex cursor-pointer items-center gap-2 text-[var(--text-xs)] text-[var(--color-muted-foreground)]">
              只看未读
              <Switch
                checked={unreadOnly}
                onCheckedChange={(v) => {
                  setUnreadOnly(v);
                  loadList(v);
                }}
              />
            </label>
            <Button
              variant="secondary"
              size="sm"
              loading={loadingList}
              onClick={() => loadList()}
              className="gap-1.5"
            >
              <RefreshCw className="size-3.5" />
              同步
            </Button>
          </>
        }
      />

      <PageBody padded={false} className="px-6 pb-6 md:px-8">
        {service && !configured && (
          <Card className="mb-3 border-[var(--color-warning)]/30 bg-[var(--color-warning)]/5 p-3.5">
            <div className="flex items-start gap-2.5">
              <AlertCircle className="mt-0.5 size-4 shrink-0 text-[var(--color-warning)]" />
              <div className="min-w-0 flex-1">
                <p className="text-[var(--text-xs)] font-medium text-[var(--color-foreground)]">
                  收件箱还没配置
                </p>
                <p className="mt-1 text-[var(--text-2xs)] leading-relaxed text-[var(--color-muted-foreground)]">
                  需要填 {(mailCfg.missing as string[] | undefined)?.join("、") || "mail.imap_host 等"}。
                  账号和授权码留空时会自动复用发信配置——同一邮箱通常通用。
                </p>
              </div>
              <Button variant="secondary" size="xs" onClick={() => (window.location.hash = "#/settings")}>
                <Settings2 className="size-3" />
                去设置
              </Button>
            </div>
          </Card>
        )}

        {error && (
          <Card className="mb-3 border-[var(--color-danger)]/30 p-3">
            <div className="flex items-start gap-2">
              <AlertCircle className="mt-0.5 size-3.5 shrink-0 text-[var(--color-danger)]" />
              <p className="min-w-0 flex-1 break-words text-[var(--text-xs)] text-[var(--color-danger)]">{error}</p>
              <Button variant="ghost" size="xs" onClick={clearError}>
                关闭
              </Button>
            </div>
          </Card>
        )}

        <div className="grid h-[calc(100vh-190px)] grid-cols-1 gap-3 lg:grid-cols-[minmax(280px,340px)_minmax(0,1fr)]">
          {/* 列表 */}
          <Card className="flex min-h-0 flex-col overflow-hidden">
            <div className="shrink-0 border-b border-[var(--color-line)] p-2.5">
              <div className="relative">
                <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-[var(--color-subtle-foreground)]" />
                <Input
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  placeholder="搜索主题或发件人"
                  className="pl-8"
                />
              </div>
            </div>

            <div className="min-h-0 flex-1 overflow-y-auto">
              {loadingList && !list ? (
                <div className="flex flex-col gap-2 p-3">
                  {Array.from({ length: 6 }).map((_, i) => (
                    <div key={i} className="space-y-1.5">
                      <Skeleton className="h-3 w-24" />
                      <Skeleton className="h-3 w-full" />
                    </div>
                  ))}
                </div>
              ) : emails.length === 0 ? (
                <EmptyState
                  compact
                  icon={Inbox}
                  title={unreadOnly ? "没有未读邮件" : "收件箱是空的"}
                  description={unreadOnly ? "关掉「只看未读」看看全部" : "同步一次试试，或者这里确实没有新邮件"}
                />
              ) : (
                emails.map((e) => {
                  const active = e.uid === activeUid;
                  return (
                    <button
                      key={e.uid}
                      onClick={() => openMail(e.uid)}
                      className={cn(
                        "flex w-full cursor-pointer flex-col gap-1 border-b border-[var(--color-line-faint)] px-3 py-2.5 text-left",
                        "transition-colors last:border-0",
                        active ? "bg-[var(--color-hover)]" : "hover:bg-[var(--color-hover)]/50"
                      )}
                    >
                      <div className="flex items-center gap-2">
                        {!e.seen && <span className="size-1.5 shrink-0 rounded-full bg-[var(--color-brand)]" />}
                        <span
                          className={cn(
                            "min-w-0 flex-1 truncate text-[var(--text-xs)]",
                            e.seen
                              ? "text-[var(--color-muted-foreground)]"
                              : "font-semibold text-[var(--color-foreground)]"
                          )}
                        >
                          {shortFrom(e.from)}
                        </span>
                        <span className="shrink-0 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                          {shortDate(e.date)}
                        </span>
                      </div>
                      <div
                        className={cn(
                          "truncate pl-3.5 text-[var(--text-xs)]",
                          e.seen ? "text-[var(--color-muted-foreground)]" : "text-[var(--color-foreground)]"
                        )}
                      >
                        {e.subject || "(无主题)"}
                      </div>
                    </button>
                  );
                })
              )}
            </div>
          </Card>

          {/* 阅读 */}
          <Card className="flex min-h-0 flex-col overflow-hidden">
            {loadingMail || running === "read_email" ? (
              <div className="flex flex-1 flex-col gap-3 p-5">
                <Skeleton className="h-5 w-2/3" />
                <Skeleton className="h-3 w-40" />
                <Divider className="my-1" />
                {Array.from({ length: 8 }).map((_, i) => (
                  <Skeleton key={i} className="h-3" style={{ width: `${92 - i * 6}%` }} />
                ))}
              </div>
            ) : !mail ? (
              <EmptyState
                className="flex-1"
                icon={MailIcon}
                title="选一封邮件开始读"
                description="左侧列表点任意一封，正文会在这里展开；附件只列清单，需要落盘再单独下载。"
              />
            ) : (
              <>
                <div className="shrink-0 border-b border-[var(--color-line)] px-5 py-4">
                  <div className="flex items-start justify-between gap-3">
                    <h2 className="text-[var(--text-lg)] font-semibold leading-snug tracking-[-0.01em]">
                      {mail.subject || "(无主题)"}
                    </h2>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      className="lg:hidden"
                      onClick={() => {
                        setMail(null);
                        setActiveUid(null);
                      }}
                    >
                      <ArrowLeft className="size-4" />
                    </Button>
                  </div>
                  <div className="mt-2 space-y-0.5">
                    <div className="text-[var(--text-xs)] text-[var(--color-foreground)]">
                      <span className="text-[var(--color-muted-foreground)]">发件人 </span>
                      {mail.from}
                    </div>
                    <div className="text-[var(--text-xs)] text-[var(--color-muted-foreground)]">
                      {mail.to && <span className="mr-3">收件人 {mail.to}</span>}
                      <span>{mail.date}</span>
                    </div>
                  </div>
                </div>

                <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
                  <pre className="whitespace-pre-wrap break-words font-sans text-[var(--text-sm)] leading-[1.75] text-[var(--color-foreground)]">
                    {mail.body || "(这封邮件没有可读的纯文本正文)"}
                  </pre>
                  {mail.truncated && (
                    <p className="mt-4 rounded-[8px] border border-[var(--color-line)] bg-[var(--color-surface)] px-3 py-2 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                      正文已截断，共 {mail.body_total_chars} 字。需要全文可以调大 max_chars 重新读。
                    </p>
                  )}

                  {mail.attachments.length > 0 && (
                    <>
                      <Divider className="my-4" />
                      <div className="label-caps mb-2">附件 {mail.attachments.length}</div>
                      <div className="flex flex-col gap-1.5">
                        {mail.attachments.map((a) => (
                          <div
                            key={a.filename}
                            className="flex items-center gap-2 rounded-[8px] border border-[var(--color-line)] bg-[var(--color-surface)] px-2.5 py-2"
                          >
                            <Paperclip className="size-3.5 shrink-0 text-[var(--color-subtle-foreground)]" />
                            <span className="min-w-0 flex-1 truncate text-[var(--text-xs)]">{a.filename}</span>
                            <span className="shrink-0 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                              {fmtBytes(a.size_bytes)}
                            </span>
                            <Badge tone="outline">{a.content_type.split(";")[0]}</Badge>
                          </div>
                        ))}
                      </div>
                      <p className="mt-2 text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                        附件只在内存里解析清单，没有落盘。要保存到本地需要在对话里让它下载附件。
                      </p>
                    </>
                  )}
                </div>

                <div className="shrink-0 border-t border-[var(--color-line-faint)] px-5 py-2">
                  <KeyValue label="UID" mono>
                    {mail.uid}
                  </KeyValue>
                </div>
              </>
            )}
          </Card>
        </div>
      </PageBody>

      <PermissionGate request={pending} onRespond={respond} />
    </Page>
  );
}
