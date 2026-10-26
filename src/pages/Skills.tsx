import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import { FolderPlus, Package, RefreshCw, Search, Trash2 } from "lucide-react";
import { Page, PageBody, PageHeader, Toolbar, Divider } from "../components/ui/page";
import { Card } from "../components/ui/card";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Input, Switch } from "../components/ui/form";
import { EmptyState, LoadingBlock } from "../components/ui/feedback";

interface SkillInfo {
  id: string;
  name: string;
  description: string;
  version: string;
  author: string;
  skill_type: string;
  enabled: boolean;
  installed_at: string;
  permissions: string[];
  triggers: string[];
}

export default function Skills() {
  const [skills, setSkills] = useState<SkillInfo[] | null>(null);
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [installing, setInstalling] = useState(false);

  const load = useCallback(async () => {
    try {
      setSkills(await invoke<SkillInfo[]>("get_skills"));
    } catch (e) {
      toast.error(typeof e === "string" ? e : "读取技能失败");
      setSkills([]);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  async function toggle(skill: SkillInfo, enabled: boolean) {
    setBusy(skill.id);
    try {
      await invoke("toggle_skill", { skillId: skill.id, enabled });
      setSkills((prev) => prev?.map((s) => (s.id === skill.id ? { ...s, enabled } : s)) ?? prev);
    } catch (e) {
      toast.error(typeof e === "string" ? e : "切换失败");
    } finally {
      setBusy(null);
    }
  }

  async function uninstall(skill: SkillInfo) {
    if (!confirm(`卸载技能「${skill.name}」？`)) return;
    setBusy(skill.id);
    try {
      await invoke("uninstall_skill", { skillId: skill.id });
      setSkills((prev) => prev?.filter((s) => s.id !== skill.id) ?? prev);
      toast.success("已卸载");
    } catch (e) {
      toast.error(typeof e === "string" ? e : "卸载失败");
    } finally {
      setBusy(null);
    }
  }

  async function install() {
    try {
      const dir = await open({ directory: true, multiple: false, title: "选择技能目录（含 manifest）" });
      if (!dir || typeof dir !== "string") return;
      setInstalling(true);
      await invoke("install_skill", { sourceDir: dir });
      toast.success("已安装");
      load();
    } catch (e) {
      toast.error(typeof e === "string" ? e : "安装失败");
    } finally {
      setInstalling(false);
    }
  }

  const visible = useMemo(() => {
    const rows = skills ?? [];
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter(
      (s) =>
        s.name.toLowerCase().includes(q) ||
        s.description.toLowerCase().includes(q) ||
        s.triggers.some((t) => t.toLowerCase().includes(q))
    );
  }, [skills, query]);

  const enabledCount = (skills ?? []).filter((s) => s.enabled).length;

  return (
    <Page>
      <PageHeader
        title="技能"
        description="技能是打包好的能力包：一组工具 + 触发条件 + 权限声明。启用后助手会按触发词自动取用。"
        icon={Package}
        badge={
          skills && (
            <Badge tone={enabledCount > 0 ? "success" : "neutral"} dot>
              {enabledCount}/{skills.length} 已启用
            </Badge>
          )
        }
        actions={
          <>
            <Button variant="ghost" size="sm" onClick={load} className="gap-1.5">
              <RefreshCw className="size-3.5" />
              刷新
            </Button>
            <Button variant="primary" size="sm" loading={installing} onClick={install} className="gap-1.5">
              <FolderPlus className="size-3.5" />
              从目录安装
            </Button>
          </>
        }
      />

      <PageBody>
        <Toolbar>
          <div className="relative w-72 max-w-full">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-[var(--color-subtle-foreground)]" />
            <Input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="搜索技能名、描述、触发词" className="pl-8" />
          </div>
          <span className="ml-auto text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">{visible.length} 个</span>
        </Toolbar>

        {skills === null ? (
          <LoadingBlock label="读取技能列表" />
        ) : visible.length === 0 ? (
          <Card>
            <EmptyState
              icon={Package}
              title={query ? "没有匹配的技能" : "还没有安装技能"}
              description={
                query
                  ? "换个关键词试试"
                  : "技能目录里要有一个描述工具、触发词与权限的清单文件。装好之后助手会在合适的时机自动用它。"
              }
              action={
                !query && (
                  <Button size="sm" variant="secondary" loading={installing} onClick={install}>
                    <FolderPlus className="size-3.5" />
                    选择技能目录
                  </Button>
                )
              }
            />
          </Card>
        ) : (
          <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
            {visible.map((s) => (
              <Card key={s.id} className="flex flex-col">
                <div className="flex items-start gap-2.5 p-3.5 pb-0">
                  <div className="flex size-8 shrink-0 items-center justify-center rounded-[9px] border border-[var(--color-line)] bg-[var(--color-surface)]">
                    <Package className="size-4 text-[var(--color-brand)]" />
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5">
                      <span className="truncate text-[var(--text-sm)] font-medium">{s.name}</span>
                      {!s.enabled && <Badge tone="neutral">未启用</Badge>}
                    </div>
                    <div className="truncate text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                      v{s.version} · {s.author || "未知作者"} · {s.skill_type}
                    </div>
                  </div>
                  <Switch checked={s.enabled} disabled={busy === s.id} onCheckedChange={(v) => toggle(s, v)} />
                </div>

                <div className="flex-1 px-3.5 py-2.5">
                  <p className="line-clamp-3 text-[var(--text-xs)] leading-relaxed text-[var(--color-muted-foreground)]">
                    {s.description || "（没有描述）"}
                  </p>
                  {s.triggers.length > 0 && (
                    <div className="mt-2 flex flex-wrap gap-1">
                      {s.triggers.slice(0, 4).map((t) => (
                        <Badge key={t} tone="outline">
                          {t}
                        </Badge>
                      ))}
                      {s.triggers.length > 4 && <Badge tone="outline">+{s.triggers.length - 4}</Badge>}
                    </div>
                  )}
                </div>

                <Divider />
                <div className="flex items-center gap-1.5 px-3.5 py-2">
                  <span className="min-w-0 flex-1 truncate text-[var(--text-2xs)] text-[var(--color-subtle-foreground)]">
                    {s.permissions.length > 0 ? `权限：${s.permissions.join("、")}` : "无额外权限"}
                  </span>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    className="text-[var(--color-danger)]"
                    disabled={busy === s.id}
                    onClick={() => uninstall(s)}
                  >
                    <Trash2 className="size-3.5" />
                  </Button>
                </div>
              </Card>
            ))}
          </div>
        )}
      </PageBody>
    </Page>
  );
}
