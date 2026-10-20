import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  CheckCircle2,
  Circle,
  CircleDot,
  ListChecks,
  RefreshCw,
} from "lucide-react";
import { Page, PageBody, PageHeader, Divider, KeyValue } from "../components/ui/page";
import { Card } from "../components/ui/card";
import { Badge, StatusDot } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { EmptyState, LoadingBlock, Progress, Skeleton } from "../components/ui/feedback";
import { cn } from "../lib/cn";

interface Conversation {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  is_archived: boolean;
}

interface TaskStep {
  step_id: number;
  action: string;
  status: string;
  result?: string;
  tool_name?: string;
}

interface Task {
  id: string;
  session_id: string;
  goal: string;
  status: string;
  plan: TaskStep[];
  current_step: number;
  completed_steps: number[];
  created_at: string;
  updated_at: string;
  is_simple: boolean;
  summary?: string;
}

const STATUS_TEXT: Record<string, string> = {
  PENDING: "等待执行",
  ANALYZING: "分析意图",
  PLANNING: "生成计划",
  EXECUTING: "执行中",
  VERIFYING: "验证结果",
  COMPLETED: "已完成",
  FAILED: "失败",
  CANCELLED: "已取消",
};

const statusTone = (s: string) => {
  if (s === "COMPLETED") return "success" as const;
  if (s === "FAILED") return "danger" as const;
  if (s === "CANCELLED") return "neutral" as const;
  return "warning" as const;
};

export default function Tasks() {
  const [convs, setConvs] = useState<Conversation[] | null>(null);
  const [activeConv, setActiveConv] = useState<string | null>(null);
  const [tasks, setTasks] = useState<Task[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [expanded, setExpanded] = useState<string | null>(null);

  const loadConvs = useCallback(async () => {
    try {
      const rows = await invoke<Conversation[]>("get_conversations");
      setConvs(rows);
      if (rows.length > 0 && !activeConv) setActiveConv(rows[0].id);
    } catch {
      setConvs([]);
    }
    // activeConv 只在首次赋值，不参与依赖
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    loadConvs();
  }, [loadConvs]);

  const loadTasks = useCallback(async (convId: string) => {
    setLoading(true);
    try {
      const rows = await invoke<Task[]>("get_tasks", { conversationId: convId, limit: 30 });
      setTasks(rows);
    } catch {
      setTasks([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (activeConv) loadTasks(activeConv);
  }, [activeConv, loadTasks]);

  const activeConvData = convs?.find((c) => c.id === activeConv);

  return (
    <Page wide>
      <PageHeader
        title="任务"
        description="助手每一轮执行的计划与步骤都在这里。状态来自账本，不是模型自述。"
        icon={ListChecks}
        badge={tasks ? <Badge tone="neutral">{tasks.length} 个任务</Badge> : undefined}
        actions={
          <Button
            variant="ghost"
            size="sm"
            className="gap-1.5"
            onClick={() => activeConv && loadTasks(activeConv)}
          >
            <RefreshCw className="size-3.5" />
            刷新
          </Button>
        }
      />

      <PageBody padded={false} className="px-6 pb-6 md:px-8">
        <div className="grid h-[calc(100vh-150px)] grid-cols-1 gap-3 lg:grid-cols-[minmax(240px,300px)_minmax(0,1fr)]">
          {/* 会话侧栏 */}
          <Card className="flex min-h-0 flex-col overflow-hidden">
            <div className="shrink-0 border-b border-[var(--color-line)] px-3 py-2">
              <span className="label-caps">会话</span>
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto">
              {convs === null ? (
                <div className="space-y-2 p-3">
                  {Array.from({ length: 5 }).map((_, i) => (
                    <Skeleton key={i} className="h-9" />
                  ))}
                </div>
              ) : convs.length === 0 ? (
                <EmptyState compact icon={ListChecks} title="还没有会话" description="先去对话，任务会跟着会话产生" />
              ) : (
                convs.map((c) => (
                  <button
                    key={c.id}
                    onClick={() => setActiveConv(c.id)}
                    className={cn(
                      "flex w-full cursor-pointer flex-col gap-0.5 border-b border-[var(--color-line-faint)] px-3 py-2 text-left transition-colors last:border-0",
                      c.id === activeConv ? "bg-[var(--color-hover)]" : "hover:bg-[var(--color-hover)]/50"
                    )}
                  >
                    <span className="truncate text-[var(--text-xs)] font-medium text-[var(--color-foreground)]">
                      {c.title || "未命名会话"}
                    </span>
                    <span className="text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                      {c.message_count} 条消息 · {c.updated_at?.slice(5, 16)}
                    </span>
                  </button>
                ))
              )}
            </div>
          </Card>

          {/* 任务列表 */}
          <Card className="flex min-h-0 flex-col overflow-hidden">
            <div className="flex shrink-0 items-center justify-between border-b border-[var(--color-line)] px-4 py-2">
              <span className="truncate text-[var(--text-xs)] font-medium text-[var(--color-foreground)]">
                {activeConvData?.title || "选择一个会话"}
              </span>
              <span className="text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                {tasks?.length ?? 0} 个任务
              </span>
            </div>

            <div className="min-h-0 flex-1 overflow-y-auto p-3">
              {loading ? (
                <LoadingBlock label="读取任务" />
              ) : !activeConv ? (
                <EmptyState icon={ListChecks} title="先选一个会话" description="左侧点任意会话查看它的执行计划" />
              ) : !tasks || tasks.length === 0 ? (
                <EmptyState
                  icon={ListChecks}
                  title="这个会话还没有任务"
                  description="发一条需要多步完成的指令（比如「整理下载目录」），计划会出现在这里"
                />
              ) : (
                <div className="flex flex-col gap-2">
                  {tasks.map((t) => {
                    const done = t.completed_steps?.length ?? 0;
                    const total = t.plan?.length ?? 0;
                    const open = expanded === t.id;
                    return (
                      <div
                        key={t.id}
                        className="rounded-[var(--radius-md)] border border-[var(--color-line)] bg-[var(--color-card)] overflow-hidden"
                      >
                        <button
                          onClick={() => setExpanded(open ? null : t.id)}
                          className="flex w-full cursor-pointer items-center gap-3 px-3.5 py-2.5 text-left transition-colors hover:bg-[var(--color-hover)]/40"
                        >
                          <StatusDot
                            tone={
                              t.status === "COMPLETED"
                                ? "success"
                                : t.status === "FAILED"
                                ? "danger"
                                : t.status === "CANCELLED"
                                ? "muted"
                                : "brand"
                            }
                            pulse={!["COMPLETED", "FAILED", "CANCELLED"].includes(t.status)}
                          />
                          <div className="min-w-0 flex-1">
                            <div className="truncate text-[var(--text-sm)] text-[var(--color-foreground)]">
                              {t.goal || "(无目标描述)"}
                            </div>
                            <div className="mt-0.5 flex items-center gap-2 text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                              <span>{t.created_at?.slice(5, 16)}</span>
                              {t.is_simple && <Badge tone="neutral">简单任务</Badge>}
                              {total > 0 && <span>步骤 {done}/{total}</span>}
                            </div>
                          </div>
                          <Badge tone={statusTone(t.status)}>{STATUS_TEXT[t.status] ?? t.status}</Badge>
                        </button>

                        {total > 0 && (
                          <div className="px-3.5 pb-2">
                            <Progress value={(done / total) * 100} tone={t.status === "FAILED" ? "danger" : "brand"} height={3} />
                          </div>
                        )}

                        {open && (
                          <div className="border-t border-[var(--color-line-faint)] bg-[var(--color-surface)]/50 px-3.5 py-3">
                            {t.plan && t.plan.length > 0 ? (
                              <div className="flex flex-col gap-2">
                                {t.plan.map((s) => {
                                  const stepDone = t.completed_steps?.includes(s.step_id);
                                  const isCurrent = t.current_step === s.step_id;
                                  return (
                                    <div key={s.step_id} className="flex items-start gap-2">
                                      {stepDone ? (
                                        <CheckCircle2 className="mt-0.5 size-3.5 shrink-0 text-[var(--color-success)]" />
                                      ) : isCurrent ? (
                                        <CircleDot className="mt-0.5 size-3.5 shrink-0 text-[var(--color-brand)]" />
                                      ) : (
                                        <Circle className="mt-0.5 size-3.5 shrink-0 text-[var(--color-subtle-foreground)]" />
                                      )}
                                      <div className="min-w-0 flex-1">
                                        <div className="flex items-center gap-1.5">
                                          <span
                                            className={cn(
                                              "text-[var(--text-xs)]",
                                              stepDone
                                                ? "text-[var(--color-muted-foreground)] line-through decoration-[var(--color-line-strong)]"
                                                : "text-[var(--color-foreground)]"
                                            )}
                                          >
                                            {s.action}
                                          </span>
                                          {s.tool_name && (
                                            <Badge tone="outline" className="font-mono">
                                              {s.tool_name}
                                            </Badge>
                                          )}
                                        </div>
                                        {s.result && (
                                          <p className="mt-0.5 line-clamp-2 break-all text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                                            {s.result}
                                          </p>
                                        )}
                                      </div>
                                    </div>
                                  );
                                })}
                              </div>
                            ) : (
                              <p className="text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                                这个任务没有生成计划（简单任务直接执行）
                              </p>
                            )}

                            {t.summary && (
                              <>
                                <Divider className="my-2.5" />
                                <div className="label-caps mb-1">结果摘要</div>
                                <p className="text-[var(--text-xs)] leading-relaxed text-[var(--color-muted-foreground)]">
                                  {t.summary}
                                </p>
                              </>
                            )}

                            <Divider className="my-2.5" />
                            <KeyValue label="任务 ID" mono>{t.id}</KeyValue>
                            <KeyValue label="更新时间">{t.updated_at}</KeyValue>
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          </Card>
        </div>
      </PageBody>
    </Page>
  );
}
