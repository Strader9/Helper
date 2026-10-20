import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import { Brain, Plus, RefreshCw, Search, Trash2 } from "lucide-react";
import { Page, PageBody, PageHeader, Toolbar, Divider } from "../components/ui/page";
import { Card } from "../components/ui/card";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Input, Textarea, Field, Select, SelectTrigger, SelectValue, SelectContent, SelectItem } from "../components/ui/form";
import { Segmented, Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogBody, DialogFooter } from "../components/ui/primitives";
import { EmptyState, LoadingBlock, Progress } from "../components/ui/feedback";

interface MemoryItem {
  id: string;
  memory_type: string;
  content: string;
  importance: number;
  access_count: number;
  created_at: string;
  last_accessed_at: string;
  source: string;
}

const TYPE_LABEL: Record<string, string> = {
  user_preference: "偏好",
  task_history: "任务历史",
  knowledge: "知识",
  conversation_summary: "对话摘要",
  app_usage: "使用习惯",
};

type Filter = "all" | "user_preference" | "knowledge" | "task_history" | "app_usage";

export default function Memory() {
  const [items, setItems] = useState<MemoryItem[] | null>(null);
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const [dialog, setDialog] = useState(false);
  const [form, setForm] = useState({ content: "", memory_type: "knowledge", importance: "50" });
  const [busy, setBusy] = useState(false);

  const load = useCallback(async (type: Filter) => {
    try {
      const rows = await invoke<MemoryItem[]>("get_memories", {
        memoryType: type === "all" ? null : type,
        limit: 200,
      });
      setItems(rows);
    } catch (e) {
      toast.error(typeof e === "string" ? e : "读取记忆失败");
      setItems([]);
    }
  }, []);

  useEffect(() => {
    load(filter);
  }, [filter, load]);

  async function add() {
    if (!form.content.trim()) {
      toast.error("内容不能为空");
      return;
    }
    setBusy(true);
    try {
      await invoke("add_memory", {
        content: form.content.trim(),
        memoryType: form.memory_type,
        importance: Number(form.importance),
      });
      toast.success("已记住");
      setDialog(false);
      setForm({ content: "", memory_type: "knowledge", importance: "50" });
      load(filter);
    } catch (e) {
      toast.error(typeof e === "string" ? e : "写入失败");
    } finally {
      setBusy(false);
    }
  }

  async function remove(item: MemoryItem) {
    if (!confirm(`忘记这条记忆？\n\n${item.content.slice(0, 80)}`)) return;
    try {
      await invoke("delete_memory", { memoryId: item.id });
      setItems((prev) => prev?.filter((x) => x.id !== item.id) ?? prev);
    } catch (e) {
      toast.error(typeof e === "string" ? e : "删除失败");
    }
  }

  const visible = useMemo(() => {
    const rows = items ?? [];
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter((r) => r.content.toLowerCase().includes(q));
  }, [items, query]);

  const counts = useMemo(() => {
    const rows = items ?? [];
    return rows.reduce<Record<string, number>>((acc, r) => {
      acc[r.memory_type] = (acc[r.memory_type] ?? 0) + 1;
      return acc;
    }, {});
  }, [items]);

  return (
    <Page>
      <PageHeader
        title="记忆"
        description="助手关于你的长期记忆。按重要度衰减，被引用越多的保留越久；这里可以手动补、也可以删。"
        icon={Brain}
        badge={items ? <Badge tone="brand">{items.length} 条</Badge> : undefined}
        actions={
          <>
            <Button variant="ghost" size="sm" onClick={() => load(filter)} className="gap-1.5">
              <RefreshCw className="size-3.5" />
              刷新
            </Button>
            <Button variant="primary" size="sm" onClick={() => setDialog(true)} className="gap-1.5">
              <Plus className="size-3.5" />
              添加
            </Button>
          </>
        }
      />

      <PageBody>
        <Toolbar>
          <div className="relative w-72 max-w-full">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-[var(--color-subtle-foreground)]" />
            <Input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="在记忆里搜索" className="pl-8" />
          </div>
          <Segmented
            value={filter}
            onChange={(v) => setFilter(v as Filter)}
            options={[
              { value: "all", label: "全部" },
              { value: "user_preference", label: "偏好" },
              { value: "knowledge", label: "知识" },
              { value: "task_history", label: "历史" },
              { value: "app_usage", label: "习惯" },
            ]}
          />
          <span className="ml-auto text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
            {visible.length} 条
          </span>
        </Toolbar>

        {items === null ? (
          <LoadingBlock label="读取记忆库" />
        ) : visible.length === 0 ? (
          <Card>
            <EmptyState
              icon={Brain}
              title={query ? "没有匹配的记忆" : "记忆还是空的"}
              description={
                query
                  ? "换个关键词试试"
                  : "聊几次之后它会自动记住你的偏好和项目位置；也可以现在就手动加一条"
              }
              action={
                !query && (
                  <Button size="sm" variant="secondary" onClick={() => setDialog(true)}>
                    添加第一条
                  </Button>
                )
              }
            />
          </Card>
        ) : (
          <div className="grid grid-cols-1 gap-2.5 md:grid-cols-2">
            {visible.map((m) => (
              <Card key={m.id} className="group p-3.5">
                <div className="flex items-start justify-between gap-2">
                  <div className="flex min-w-0 items-center gap-1.5">
                    <Badge tone="outline">{TYPE_LABEL[m.memory_type] ?? m.memory_type}</Badge>
                    {m.source === "manual" && <Badge tone="brand">手动</Badge>}
                  </div>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="opacity-0 transition-opacity group-hover:opacity-100 text-[var(--color-danger)]"
                    onClick={() => remove(m)}
                  >
                    <Trash2 className="size-3.5" />
                  </Button>
                </div>
                <p className="mt-2 text-[var(--text-sm)] leading-relaxed text-[var(--color-foreground)]">
                  {m.content}
                </p>
                <Divider className="my-2.5" />
                <div className="flex items-center gap-3">
                  <div className="flex flex-1 items-center gap-1.5">
                    <span className="text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">重要度</span>
                    <Progress value={m.importance} height={3} className="max-w-20" />
                    <span className="num-tabular text-[var(--text-2xs)] text-[var(--color-muted-foreground)]">
                      {m.importance}
                    </span>
                  </div>
                  <span className="text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                    引用 {m.access_count} 次
                  </span>
                  <span className="text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                    {m.created_at?.slice(5, 16)}
                  </span>
                </div>
              </Card>
            ))}
          </div>
        )}

        {items && items.length > 0 && (
          <p className="mt-4 text-center text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
            {Object.entries(counts)
              .map(([k, v]) => `${TYPE_LABEL[k] ?? k} ${v}`)
              .join(" · ")}
          </p>
        )}
      </PageBody>

      <Dialog open={dialog} onOpenChange={setDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>添加记忆</DialogTitle>
            <DialogDescription>写成一句陈述句最容易命中——「我的项目在 D:\projects」比「项目」有用。</DialogDescription>
          </DialogHeader>
          <DialogBody className="space-y-3.5">
            <Field label="内容">
              <Textarea
                value={form.content}
                onChange={(e) => setForm({ ...form, content: e.target.value })}
                placeholder="例如：我习惯用 pnpm 而不是 npm"
                className="min-h-20"
                autoFocus
              />
            </Field>
            <div className="grid grid-cols-2 gap-3">
              <Field label="类型">
                <Select value={form.memory_type} onValueChange={(v) => setForm({ ...form, memory_type: v })}>
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="user_preference">用户偏好</SelectItem>
                    <SelectItem value="knowledge">知识 / 事实</SelectItem>
                    <SelectItem value="task_history">任务历史</SelectItem>
                    <SelectItem value="app_usage">使用习惯</SelectItem>
                  </SelectContent>
                </Select>
              </Field>
              <Field label="重要度" hint="0-100">
                <Input
                  type="number"
                  min={0}
                  max={100}
                  value={form.importance}
                  onChange={(e) => setForm({ ...form, importance: e.target.value })}
                  className="num-tabular"
                />
              </Field>
            </div>
          </DialogBody>
          <DialogFooter>
            <Button variant="ghost" size="sm" onClick={() => setDialog(false)}>
              取消
            </Button>
            <Button variant="primary" size="sm" loading={busy} onClick={add}>
              记住
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Page>
  );
}
