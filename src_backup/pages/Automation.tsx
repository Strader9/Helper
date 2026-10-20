import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import {
  CalendarClock,
  Clock,
  Play,
  Plus,
  RefreshCw,
  Trash2,
  Zap,
} from "lucide-react";
import { Page, PageBody, PageHeader, KeyValue, Divider } from "../components/ui/page";
import { Card, SectionHeader } from "../components/ui/card";
import { Badge, StatusDot } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Input, Textarea, Field, Select, SelectTrigger, SelectValue, SelectContent, SelectItem, Switch } from "../components/ui/form";
import { Tabs, TabsList, TabsTrigger, TabsContent, Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogBody, DialogFooter } from "../components/ui/primitives";
import { EmptyState, LoadingBlock } from "../components/ui/feedback";

interface ScheduledJob {
  id: string;
  name: string;
  kind: string;
  schedule: string;
  goal: string;
  enabled: boolean;
  next_run_at: string | null;
  last_run_at: string | null;
  run_count: number;
  max_runs: number | null;
  last_status: string | null;
  last_summary: string | null;
  last_error: string | null;
  created_at: string;
}

interface JobRun {
  id: string;
  job_id: string;
  started_at: string;
  finished_at: string | null;
  status: string;
  summary: string | null;
  error: string | null;
  steps: number;
  tools_used: string[];
}

interface ProactiveRule {
  id: string;
  name: string;
  trigger_type: string;
  trigger_config: string;
  action_type: string;
  action_config: string;
  enabled: boolean;
  last_triggered: string | null;
  trigger_count: number;
  created_at: string;
}

interface ProactiveStatus {
  running: boolean;
  rules_total: number;
  rules_enabled: number;
  last_check: string | null;
  checks_count: number;
  triggers_count: number;
}

const KIND_LABEL: Record<string, string> = {
  interval: "固定间隔",
  cron: "Cron",
  once: "一次性",
};

/** StatusDot 只认 success/warning/danger/brand/muted，neutral 要落到 muted */
const dotTone = (s?: string | null): "success" | "warning" | "danger" | "brand" | "muted" => {
  const t = statusTone(s);
  return t === "neutral" ? "muted" : t;
};

const statusTone = (s?: string | null) => {
  if (!s) return "neutral" as const;
  const v = s.toLowerCase();
  if (v.includes("fail") || v.includes("error")) return "danger" as const;
  if (v.includes("success") || v.includes("ok") || v.includes("done")) return "success" as const;
  if (v.includes("run") || v.includes("progress")) return "warning" as const;
  return "neutral" as const;
};

export default function Automation() {
  const [jobs, setJobs] = useState<ScheduledJob[] | null>(null);
  const [rules, setRules] = useState<ProactiveRule[] | null>(null);
  const [pstatus, setPstatus] = useState<ProactiveStatus | null>(null);
  const [runs, setRuns] = useState<JobRun[]>([]);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const [jobDialog, setJobDialog] = useState(false);
  const [jobForm, setJobForm] = useState({ name: "", kind: "interval", schedule: "30m", goal: "" });
  const [ruleDialog, setRuleDialog] = useState(false);
  const [ruleForm, setRuleForm] = useState({ name: "", trigger_type: "system_metric", trigger_config: "", action_type: "notify", action_config: "" });

  const load = useCallback(async () => {
    try {
      const [j, r, s] = await Promise.all([
        invoke<ScheduledJob[]>("list_scheduled_jobs"),
        invoke<ProactiveRule[]>("get_proactive_rules"),
        invoke<ProactiveStatus>("get_proactive_status").catch(() => null),
      ]);
      setJobs(j);
      setRules(r);
      setPstatus(s);
    } catch (e) {
      toast.error(typeof e === "string" ? e : "读取自动化配置失败");
      setJobs([]);
      setRules([]);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  async function toggleJob(job: ScheduledJob, enabled: boolean) {
    setBusy(job.id);
    try {
      await invoke("set_scheduled_job_enabled", { id: job.id, enabled });
      setJobs((prev) => prev?.map((j) => (j.id === job.id ? { ...j, enabled } : j)) ?? prev);
    } catch (e) {
      toast.error(typeof e === "string" ? e : "切换失败");
    } finally {
      setBusy(null);
    }
  }

  async function runNow(job: ScheduledJob) {
    setBusy(job.id);
    try {
      await invoke("run_scheduled_job_now", { id: job.id });
      toast.success("已标记为待执行", { description: "调度器下一轮会接手，通常几秒内开始" });
      setTimeout(load, 1500);
    } catch (e) {
      toast.error(typeof e === "string" ? e : "触发失败");
    } finally {
      setBusy(null);
    }
  }

  async function removeJob(job: ScheduledJob) {
    if (!confirm(`删除定时任务「${job.name || job.id}」？运行历史会一并删除。`)) return;
    setBusy(job.id);
    try {
      await invoke("delete_scheduled_job", { id: job.id });
      setJobs((prev) => prev?.filter((j) => j.id !== job.id) ?? prev);
      toast.success("已删除");
    } catch (e) {
      toast.error(typeof e === "string" ? e : "删除失败");
    } finally {
      setBusy(null);
    }
  }

  async function showRuns(job: ScheduledJob) {
    if (expanded === job.id) {
      setExpanded(null);
      return;
    }
    setExpanded(job.id);
    try {
      const rows = await invoke<JobRun[]>("get_scheduled_job_runs", { id: job.id, limit: 10 });
      setRuns(rows);
    } catch {
      setRuns([]);
    }
  }

  async function createJob() {
    if (!jobForm.name.trim() || !jobForm.goal.trim()) {
      toast.error("名称和目标都要填");
      return;
    }
    setBusy("create-job");
    try {
      await invoke("create_scheduled_job", {
        name: jobForm.name.trim(),
        kind: jobForm.kind,
        schedule: jobForm.schedule.trim(),
        goal: jobForm.goal.trim(),
        maxRuns: null,
      });
      toast.success("已创建");
      setJobDialog(false);
      setJobForm({ name: "", kind: "interval", schedule: "30m", goal: "" });
      load();
    } catch (e) {
      toast.error(typeof e === "string" ? e : "创建失败");
    } finally {
      setBusy(null);
    }
  }

  async function toggleRule(rule: ProactiveRule, enabled: boolean) {
    setBusy(rule.id);
    try {
      await invoke("toggle_proactive_rule", { id: rule.id, enabled });
      setRules((prev) => prev?.map((r) => (r.id === rule.id ? { ...r, enabled } : r)) ?? prev);
    } catch (e) {
      toast.error(typeof e === "string" ? e : "切换失败");
    } finally {
      setBusy(null);
    }
  }

  async function removeRule(rule: ProactiveRule) {
    if (!confirm(`删除主动规则「${rule.name || rule.id}」？`)) return;
    setBusy(rule.id);
    try {
      await invoke("delete_proactive_rule", { id: rule.id });
      setRules((prev) => prev?.filter((r) => r.id !== rule.id) ?? prev);
      toast.success("已删除");
    } catch (e) {
      toast.error(typeof e === "string" ? e : "删除失败");
    } finally {
      setBusy(null);
    }
  }

  async function createRule() {
    if (!ruleForm.name.trim()) {
      toast.error("先给规则起个名字");
      return;
    }
    setBusy("create-rule");
    try {
      await invoke("add_proactive_rule", {
        name: ruleForm.name.trim(),
        triggerType: ruleForm.trigger_type,
        triggerConfig: ruleForm.trigger_config || "{}",
        actionType: ruleForm.action_type,
        actionConfig: ruleForm.action_config || "{}",
      });
      toast.success("已创建");
      setRuleDialog(false);
      setRuleForm({ name: "", trigger_type: "system_metric", trigger_config: "", action_type: "notify", action_config: "" });
      load();
    } catch (e) {
      toast.error(typeof e === "string" ? e : "创建失败");
    } finally {
      setBusy(null);
    }
  }

  return (
    <Page>
      <PageHeader
        title="自动化"
        description="两类自动化：到点执行的目标（定时任务），和满足条件就触发的规则（主动规则）。两者都在本机跑，不联网。"
        icon={Zap}
        badge={
          pstatus && (
            <Badge tone={pstatus.running ? "success" : "neutral"} dot pulse={pstatus.running}>
              {pstatus.running ? "监控运行中" : "监控已停止"}
            </Badge>
          )
        }
        actions={
          <Button variant="ghost" size="sm" onClick={load} className="gap-1.5">
            <RefreshCw className="size-3.5" />
            刷新
          </Button>
        }
      />

      <PageBody>
        <Tabs defaultValue="jobs">
          <TabsList className="mb-4">
            <TabsTrigger value="jobs">
              <CalendarClock className="size-3.5" />
              定时任务 {jobs ? `(${jobs.length})` : ""}
            </TabsTrigger>
            <TabsTrigger value="rules">
              <Zap className="size-3.5" />
              主动规则 {rules ? `(${rules.length})` : ""}
            </TabsTrigger>
          </TabsList>

          {/* ---------------- 定时任务 ---------------- */}
          <TabsContent value="jobs" className="focus-visible:outline-none">
            <SectionHeader
              title="到点执行"
              description="调度器按 interval / cron / once 三种方式触发，执行结果记入运行历史"
              action={
                <Button size="sm" variant="primary" onClick={() => setJobDialog(true)} className="gap-1.5">
                  <Plus className="size-3.5" />
                  新建任务
                </Button>
              }
            />
            {jobs === null ? (
              <LoadingBlock label="读取定时任务" />
            ) : jobs.length === 0 ? (
              <Card>
                <EmptyState
                  icon={CalendarClock}
                  title="还没有定时任务"
                  description="比如「每天早上 9 点汇总一次昨天的系统告警」「每 2 小时检查一次磁盘剩余空间」"
                  action={
                    <Button size="sm" variant="secondary" onClick={() => setJobDialog(true)}>
                      新建第一个
                    </Button>
                  }
                />
              </Card>
            ) : (
              <div className="flex flex-col gap-2">
                {jobs.map((job) => (
                  <Card key={job.id} className="overflow-hidden">
                    <div className="flex items-center gap-3 px-4 py-3">
                      <StatusDot
                        tone={job.enabled ? (job.last_status ? dotTone(job.last_status) : "brand") : "muted"}
                        pulse={job.enabled}
                      />
                      <div className="min-w-0 flex-1">
                        <div className="flex items-center gap-2">
                          <span className="truncate text-[var(--text-sm)] font-medium">{job.name || "(未命名)"}</span>
                          <Badge tone="outline">{KIND_LABEL[job.kind] ?? job.kind}</Badge>
                          {!job.enabled && <Badge tone="neutral">已暂停</Badge>}
                        </div>
                        <div className="mt-0.5 truncate text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                          <span className="font-mono">{job.schedule}</span>
                          <span className="mx-1.5 opacity-40">·</span>
                          {job.goal}
                        </div>
                      </div>
                      <div className="hidden shrink-0 text-right sm:block">
                        <div className="num-tabular text-[var(--text-xs)] text-[var(--color-foreground)]">
                          已跑 {job.run_count} 次
                        </div>
                        <div className="text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                          {job.enabled && job.next_run_at ? `下次 ${job.next_run_at.slice(5, 16)}` : "未排期"}
                        </div>
                      </div>
                      <Switch
                        checked={job.enabled}
                        disabled={busy === job.id}
                        onCheckedChange={(v) => toggleJob(job, v)}
                      />
                      <Button variant="ghost" size="icon-sm" disabled={busy === job.id} onClick={() => runNow(job)} title="立即执行一次">
                        <Play className="size-3.5" />
                      </Button>
                      <Button variant="ghost" size="icon-sm" onClick={() => showRuns(job)} title="运行历史">
                        <Clock className="size-3.5" />
                      </Button>
                      <Button variant="ghost" size="icon-sm" className="text-[var(--color-danger)]" onClick={() => removeJob(job)} title="删除">
                        <Trash2 className="size-3.5" />
                      </Button>
                    </div>

                    {expanded === job.id && (
                      <div className="border-t border-[var(--color-line-faint)] bg-[var(--color-surface)]/50 px-4 py-3">
                        <div className="label-caps mb-2">最近运行</div>
                        {runs.length === 0 ? (
                          <p className="text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">还没有运行记录</p>
                        ) : (
                          <div className="flex flex-col gap-1.5">
                            {runs.map((r) => (
                              <div key={r.id} className="flex items-start gap-2 text-[var(--text-2xs)]">
                                <Badge tone={statusTone(r.status)}>{r.status}</Badge>
                                <span className="shrink-0 text-[var(--color-subtle-foreground)]">{r.started_at.slice(5, 16)}</span>
                                <span className="min-w-0 flex-1 truncate text-[var(--color-muted-foreground)]">
                                  {r.summary || r.error || `执行 ${r.steps} 步`}
                                </span>
                              </div>
                            ))}
                          </div>
                        )}
                        <Divider className="my-2.5" />
                        <KeyValue label="任务 ID" mono>{job.id}</KeyValue>
                        <KeyValue label="上次运行">{job.last_run_at ?? "从未"}</KeyValue>
                        <KeyValue label="上次结果">
                          <Badge tone={statusTone(job.last_status)}>{job.last_status ?? "无"}</Badge>
                        </KeyValue>
                        {job.last_error && (
                          <p className="mt-1 break-all text-[var(--text-2xs)] text-[var(--color-danger)]">{job.last_error}</p>
                        )}
                      </div>
                    )}
                  </Card>
                ))}
              </div>
            )}
          </TabsContent>

          {/* ---------------- 主动规则 ---------------- */}
          <TabsContent value="rules" className="focus-visible:outline-none">
            <SectionHeader
              title="条件触发"
              description="监控系统指标或时间条件，命中后执行动作。触发是本地计算，不会上报任何数据"
              action={
                <Button size="sm" variant="primary" onClick={() => setRuleDialog(true)} className="gap-1.5">
                  <Plus className="size-3.5" />
                  新建规则
                </Button>
              }
            />
            {pstatus && (
              <Card className="mb-3 p-3">
                <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
                  <div>
                    <div className="num-tabular text-[var(--text-lg)] font-semibold">{pstatus.rules_enabled}/{pstatus.rules_total}</div>
                    <div className="text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">启用规则</div>
                  </div>
                  <div>
                    <div className="num-tabular text-[var(--text-lg)] font-semibold">{pstatus.checks_count}</div>
                    <div className="text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">检查次数</div>
                  </div>
                  <div>
                    <div className="num-tabular text-[var(--text-lg)] font-semibold">{pstatus.triggers_count}</div>
                    <div className="text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">触发次数</div>
                  </div>
                  <div>
                    <div className="truncate text-[var(--text-xs)] font-medium">{pstatus.last_check ?? "—"}</div>
                    <div className="text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">最近检查</div>
                  </div>
                </div>
              </Card>
            )}
            {rules === null ? (
              <LoadingBlock label="读取主动规则" />
            ) : rules.length === 0 ? (
              <Card>
                <EmptyState
                  icon={Zap}
                  title="还没有主动规则"
                  description="比如「CPU 连续 5 分钟高于 90% 就弹通知」「每天 18 点提醒整理桌面」"
                  action={
                    <Button size="sm" variant="secondary" onClick={() => setRuleDialog(true)}>
                      新建第一个
                    </Button>
                  }
                />
              </Card>
            ) : (
              <div className="flex flex-col gap-2">
                {rules.map((rule) => (
                  <Card key={rule.id} className="flex items-center gap-3 px-4 py-3">
                    <StatusDot tone={rule.enabled ? "brand" : "muted"} pulse={rule.enabled} />
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span className="truncate text-[var(--text-sm)] font-medium">{rule.name || "(未命名规则)"}</span>
                        <Badge tone="outline">{rule.trigger_type}</Badge>
                        <Badge tone="outline">{rule.action_type}</Badge>
                      </div>
                      <div className="mt-0.5 truncate font-mono text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                        {rule.trigger_config}
                      </div>
                    </div>
                    <div className="hidden shrink-0 text-right sm:block">
                      <div className="num-tabular text-[var(--text-xs)]">触发 {rule.trigger_count} 次</div>
                      <div className="text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                        {rule.last_triggered ? `上次 ${rule.last_triggered.slice(5, 16)}` : "从未触发"}
                      </div>
                    </div>
                    <Switch checked={rule.enabled} disabled={busy === rule.id} onCheckedChange={(v) => toggleRule(rule, v)} />
                    <Button variant="ghost" size="icon-sm" className="text-[var(--color-danger)]" onClick={() => removeRule(rule)}>
                      <Trash2 className="size-3.5" />
                    </Button>
                  </Card>
                ))}
              </div>
            )}
          </TabsContent>
        </Tabs>
      </PageBody>

      {/* 新建定时任务 */}
      <Dialog open={jobDialog} onOpenChange={setJobDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>新建定时任务</DialogTitle>
            <DialogDescription>描述一个目标，到点后由助手自己规划执行步骤。</DialogDescription>
          </DialogHeader>
          <DialogBody className="space-y-3.5">
            <Field label="名称">
              <Input value={jobForm.name} onChange={(e) => setJobForm({ ...jobForm, name: e.target.value })} placeholder="例如：早间系统体检" />
            </Field>
            <div className="grid grid-cols-2 gap-3">
              <Field label="触发方式">
                <Select value={jobForm.kind} onValueChange={(v) => setJobForm({ ...jobForm, kind: v })}>
                  <SelectTrigger><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="interval">固定间隔</SelectItem>
                    <SelectItem value="cron">Cron 表达式</SelectItem>
                    <SelectItem value="once">一次性</SelectItem>
                  </SelectContent>
                </Select>
              </Field>
              <Field label="时间" hint={jobForm.kind === "interval" ? "30m / 2h" : jobForm.kind === "cron" ? "0 9 * * 1-5" : "2026-10-20 09:00"}>
                <Input value={jobForm.schedule} onChange={(e) => setJobForm({ ...jobForm, schedule: e.target.value })} className="font-mono" />
              </Field>
            </div>
            <Field label="目标" description="用一句自然语言写清要做什么，执行时助手会自己拆步骤">
              <Textarea value={jobForm.goal} onChange={(e) => setJobForm({ ...jobForm, goal: e.target.value })} placeholder="检查磁盘剩余空间，低于 10% 时告诉我哪些目录最占地方" className="min-h-20" />
            </Field>
          </DialogBody>
          <DialogFooter>
            <Button variant="ghost" size="sm" onClick={() => setJobDialog(false)}>取消</Button>
            <Button variant="primary" size="sm" loading={busy === "create-job"} onClick={createJob}>创建</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 新建主动规则 */}
      <Dialog open={ruleDialog} onOpenChange={setRuleDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>新建主动规则</DialogTitle>
            <DialogDescription>条件满足时执行动作。配置用 JSON 写，字段与工具侧一致。</DialogDescription>
          </DialogHeader>
          <DialogBody className="space-y-3.5">
            <Field label="名称">
              <Input value={ruleForm.name} onChange={(e) => setRuleForm({ ...ruleForm, name: e.target.value })} placeholder="例如：CPU 持续过高提醒" />
            </Field>
            <div className="grid grid-cols-2 gap-3">
              <Field label="触发类型">
                <Select value={ruleForm.trigger_type} onValueChange={(v) => setRuleForm({ ...ruleForm, trigger_type: v })}>
                  <SelectTrigger><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="system_metric">系统指标</SelectItem>
                    <SelectItem value="time">时间条件</SelectItem>
                    <SelectItem value="file_change">文件变化</SelectItem>
                  </SelectContent>
                </Select>
              </Field>
              <Field label="动作类型">
                <Select value={ruleForm.action_type} onValueChange={(v) => setRuleForm({ ...ruleForm, action_type: v })}>
                  <SelectTrigger><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="notify">发送通知</SelectItem>
                    <SelectItem value="run_tool">调用工具</SelectItem>
                    <SelectItem value="run_workflow">执行工作流</SelectItem>
                  </SelectContent>
                </Select>
              </Field>
            </div>
            <Field label="触发配置 (JSON)" hint='{"metric":"cpu","threshold":90,"operator":"gte","duration_sec":300}'>
              <Textarea value={ruleForm.trigger_config} onChange={(e) => setRuleForm({ ...ruleForm, trigger_config: e.target.value })} placeholder='{"metric":"cpu","threshold":90,"operator":"gte","duration_sec":300}' className="min-h-16 font-mono text-[var(--text-2xs)]" />
            </Field>
            <Field label="动作配置 (JSON)" hint='{"title":"CPU 过高","message":"..."}'>
              <Textarea value={ruleForm.action_config} onChange={(e) => setRuleForm({ ...ruleForm, action_config: e.target.value })} placeholder='{"title":"CPU 过高"} ' className="min-h-16 font-mono text-[var(--text-2xs)]" />
            </Field>
          </DialogBody>
          <DialogFooter>
            <Button variant="ghost" size="sm" onClick={() => setRuleDialog(false)}>取消</Button>
            <Button variant="primary" size="sm" loading={busy === "create-rule"} onClick={createRule}>创建</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Page>
  );
}
