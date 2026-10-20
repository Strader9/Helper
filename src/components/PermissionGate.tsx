import { AlertTriangle, ShieldQuestion } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogBody,
  DialogFooter,
} from "./ui/primitives";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { KeyValue } from "./ui/page";
import type { PermissionDecision, PermissionRequest } from "../lib/useUiTool";

const RISK_TEXT: Record<string, string> = {
  HIGH: "高风险：可能改动系统或对外产生影响，请确认来源可信",
  MEDIUM: "中风险：会产生真实副作用（写入文件 / 计费 / 对外通信）",
  LOW: "低风险：只读或影响范围很小",
};

/**
 * 权限确认框。
 *
 * 与对话页的确认框共用同一套后端流程，但文案更克制：这里不做"一律允许"
 * 的诱导，默认焦点给"拒绝"，避免用户连点回车把危险操作放过去。
 */
export function PermissionGate({
  request,
  onRespond,
}: {
  request: PermissionRequest | null;
  onRespond: (d: PermissionDecision) => void;
}) {
  const risk = request?.risk_level?.toUpperCase?.() ?? "MEDIUM";
  const isHigh = risk === "HIGH" || risk === "CRITICAL";

  return (
    <Dialog open={!!request} onOpenChange={(open) => !open && onRespond("deny")}>
      <DialogContent
        size="md"
        onKeyDown={(e) => {
          if (e.key === "Escape") onRespond("deny");
        }}
      >
        <DialogHeader>
          <div className="mb-2 flex size-9 items-center justify-center rounded-[10px] border border-[var(--color-brand-line)] bg-[var(--color-brand-soft)]">
            {isHigh ? (
              <AlertTriangle className="size-4.5 text-[var(--color-danger)]" />
            ) : (
              <ShieldQuestion className="size-4.5 text-[var(--color-brand)]" />
            )}
          </div>
          <DialogTitle>需要你确认</DialogTitle>
          <DialogDescription>
            {RISK_TEXT[risk] ?? "这次操作会产生实际影响"}
          </DialogDescription>
        </DialogHeader>

        <DialogBody>
          <div className="rounded-[var(--radius-md)] border border-[var(--color-line)] bg-[var(--color-surface)] px-3 py-2">
            <KeyValue label="工具">
              <span className="font-mono">{request?.tool_name}</span>
            </KeyValue>
            <KeyValue label="风险">
              <Badge tone={isHigh ? "danger" : "warning"}>{risk}</Badge>
            </KeyValue>
            {request?.path && (
              <KeyValue label="路径" mono>
                {request.path}
              </KeyValue>
            )}
          </div>
          {request?.description && (
            <p className="mt-2.5 text-[var(--text-xs)] leading-relaxed text-[var(--color-muted-foreground)]">
              {request.description}
            </p>
          )}
        </DialogBody>

        <DialogFooter>
          <Button variant="ghost" size="sm" onClick={() => onRespond("deny")}>
            拒绝
          </Button>
          <Button variant="outline" size="sm" onClick={() => onRespond("always_allow")}>
            以后都允许
          </Button>
          <Button variant="primary" size="sm" onClick={() => onRespond("allow_once")}>
            允许一次
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
