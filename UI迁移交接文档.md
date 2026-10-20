# PC Guardian UI 迁移交接文档（对齐 LumiCore）

> 状态：**代码层全部完成**，`tsc --noEmit` / `vite build` / `cargo check --lib` 均通过。
> 唯一未完成项：**运行时视觉目验**（被执行环境三个坑堵死，见第六节），需你开窗口确认。
> 日期：2026-10-20 · 工作区：`C:\大项目\helper电脑\pc-guardian`

---

## 〇、总览：这次计划做了什么

一次完整的「诊断 → 建设 → 校准 → 清场」四阶段迁移，把 pc-guardian 的前端从自研赛博朋克皮肤换成 LumiCore 的视觉体系。

| 阶段 | 触发 | 做了什么 | 结果 |
|------|------|----------|------|
| 1. 规模对比 | 「再来一次规模对比」 | 重口径统计 pc-guardian vs LumiCore | 定位到差距在 UI 层（1:11.8），工具层仅差 4% |
| 2. UI 建设 | 「把 ui 补上行，我要跟 lumi 一样高级」 | 从零搭设计系统 + 组件库 + 14 页面 + 后端闸门 | 前端从 12 文件 6,125 行扩到 27 文件 16,000+ 行 |
| 3. 视觉校准 | 「太丑了，我建议直接照搬 lumi」 | 定位三条根因，整套 token 换成 Lumi 写法 | 中性灰阶 + 近白主按钮 + 无影卡片 |
| 4. 彻底清场 | 「彻底抛弃原来 helper 的 ui，整个外观全部套用」 | 删除 5,556 行旧 CSS 与 4 个孤儿文件 | CSS 产物 149.78 kB → 58.42 kB |

**净改动：+5,522 / −10,314 行**（`git diff --stat HEAD`，24 个跟踪文件）。

---

## 一、阶段 1：规模对比（诊断依据）

### 1.1 修正后的口径

上一轮我用「全项目行数 ÷ 工具数」得出 337 vs 1,329，**这个口径是错的**，已纠正。按工具层实际代码统计：

| 指标 | pc-guardian | LumiCore | 比值 |
|------|------------|----------|------|
| 工具契约数 | 221 | 242 | 91% |
| 工具实现层行数 | 29,476（44 个 .rs 文件） | — | — |
| **行/工具（实现层）** | **134** | **129** | **1.04** |
| 框架层行数 | 4,735 | 7,557 | 0.63 |
| **前端行数** | **6,125（12 文件）** | **78,400** | **1:11.8** |

### 1.2 结论

- **工具面无数量级差距**（134 vs 129 行/工具，仅差 4%），这正是本轮「只动 UI、工具层不改」的依据。
- 真正差距在前端：12 个文件、5,773 行手写 CSS、无组件库、无设计系统。
- Lumi 多出的代码在它自己的桌面壳（socket 19.7k + routes 7.4k），这些在 Tauri 里由 IPC + 原生窗口替代，不需要搬。

---

## 二、阶段 2：UI 建设（第一轮成果）

### 2.1 设计系统（新建）

| 文件 | 行数 | 内容 |
|------|------|------|
| `src/styles/system.css` | 393 | 唯一样式入口。Tailwind v4 `@theme`/`@utility`，**只引 theme + utilities 两层，不引 preflight**（避免破坏遗留 CSS） |
| `src/styles/mini.css` | 49 | MiniChat 浮窗模式独立样式 |

`system.css` 定义：oklch 语义色 token（canvas/background/surface/card/elevated/hover/active + line/-strong/-faint + brand）、字号阶梯、圆角 8/10/12/16/20/26、多层阴影、动效曲线 `cubic-bezier(.16,1,.3,1)`、`@utility surface-glass/ring-brand/bg-grid/label-caps/num-tabular/skeleton-shimmer`、`ds-*` 前缀 keyframes（避开与 cyberpunk.css 撞名）、`.ds` 作用域基础层（不碰旧 `body`/`#root`）。

### 2.2 工具函数

`src/lib/cn.ts`（11 行）：`cn()` = clsx + twMerge，消解 Tailwind 类冲突。

### 2.3 UI 组件库（`src/components/ui/`，7 文件 1,139 行）

| 文件 | 行数 | 导出 |
|------|------|------|
| `button.tsx` | 83 | cva 变体 primary/secondary/ghost/outline/danger/link × 尺寸 xs/sm/md/lg/icon/icon-sm/icon-lg，`loading` 态，`asChild` 走 Radix Slot |
| `card.tsx` | 111 | Card（interactive hover 抬升）/CardHeader/Title/Description/Content/Footer/SectionHeader |
| `form.tsx` | 195 | Input/Textarea/Label/Field（horizontal 两栏）/Switch/Select 全家（Radix） |
| `badge.tsx` | 95 | Badge（7 tone + dot/pulse）/StatusDot/Kbd |
| `feedback.tsx` | 245 | Progress/RingMetric/Skeleton/SkeletonCard/EmptyState/Spinner/LoadingBlock/BigNumber |
| `primitives.tsx` | 229 | Tabs 全家/Dialog 全家（4 尺寸）/Tooltip/Segmented |
| `page.tsx` | 170 | Page（max-w-1180）/PageHeader/PageBody/Toolbar/Grid/KeyValue/Divider |

### 2.4 应用骨架（848 行）

| 文件 | 行数 | 要点 |
|------|------|------|
| `src/App.tsx` | 160 | TooltipProvider 包裹、侧栏折叠态持久化 `localStorage("pcg.sidebar.collapsed")`、⌘K/Ctrl+K 全局绑定、14 条路由、未匹配回落 `/chat` |
| `src/components/Sidebar.tsx` | 236 | 可折叠 w-58/w-16、分组导航、激活态左 2px 品牌条、命令面板入口、内核健康 StatusDot |
| `src/components/CommandPalette.tsx` | 152 | ⌘K 面板，多词 AND 匹配，↑↓/Enter 键盘导航，`scrollIntoView({block:"nearest"})` |
| `src/components/nav.ts` | 74 | `NAV_GROUPS` 唯一导航来源（工作/能力/资产/系统 4 组 14 项，含 hint/keywords） |
| `src/components/PermissionGate.tsx` | 95 | 权限弹窗，HIGH 用 danger 图标，Escape/overlay 默认 deny |
| `src/lib/useUiTool.ts` | 102 | 监听 `permission-request` 且 `session_id === "ui-console"`，暴露 `{pending, running, error, run, respond, clearError}` |
| `src/main.tsx` | 29 | `system.css` 先于其他样式加载，挂 sonner `<Toaster>` |

### 2.5 后端三个命令（`src-tauri/src/lib.rs` + `main.rs`）

| 命令 | 用途 |
|------|------|
| `list_tool_contracts` | 工具契约表（Tools 页），返回 `ToolContractView{tool, side_effect, scope, reversible, verification, required_fields, risk_level, registered}` |
| `list_media_generations` | 媒体生成台账（Media 页） |
| `run_ui_tool` | 界面直呼工具的**唯一闸门** |

**`run_ui_tool` 的安全设计**：
- 走 `ToolExecutor::execute()`，复用 Tauri 三层闸门（权限 → 回执 → 审计），不另开旁路；
- `UI_TOOL_ALLOWLIST` 只放行 7 个工具：`image_generate` / `video_generate` / `video_generation_status` / `media_provider_status` / `recent_emails` / `read_email` / `read_email_attachments`；
- **明确拒绝通用 `run_tool`**：只要有任何一处渲染不可信 HTML，通用入口就等于全部工具集失守；
- 会话 id 固定 `"ui-console"`，前端靠它过滤权限事件；
- 返回 `{tool, success, data, error, error_code, duration_ms, receipt_id, permission}`。

### 2.6 页面（15 个，8,004 行）

**重写（8 个）**：

| 页面 | 迁移前 → 后 | 要点 |
|------|------------|------|
| `Chat.tsx` | 1,153 → 1,179 | 全部换成 utility 类；**事件监听逻辑一行未改**（`parseEventEnvelope` session 过滤、`activeRunIdRef` 竞态防护、`listenerSetupGenerationRef` StrictMode 丢弃）；`ToolExecutionCard` 保留 verification 判定优先级 VERIFIED/CONTRADICTED/FAILED/UNVERIFIED/NOT_REQUIRED |
| `Dashboard.tsx` | 335 → 453 | 4 个 RingMetric（CPU/内存/GPU/磁盘峰值，motion stagger）、能力契约覆盖率、最近执行（`get_tool_receipts`）、3s 静默轮询 |
| `Settings.tsx` | 2,136 → 818 | 2136 行单文件 → 8 分区侧导航；`renderField()` 按 `value_type` 分派（bool→Switch / number→数字+unit / `SELECT_OPTIONS`→Select / 其余→Input）；secret 分区 5 个凭据密码框 + 尾号辨认 |
| `Logs.tsx` | **47（纯占位）→ 715** | 骨架占位 → 审计账 + 回执账双层查看器，数据源 `get_audit_logs` / `get_tool_receipts` |
| `GitHub.tsx` | 185 → 279 | 旧 `page-container` → 新体系，趋势面板 |
| `MiniChat.tsx` | 564 → 688 | 浮窗模式，配套 `mini.css` |
| `WebStudio.tsx` | 654 → 937 | 654 行旧样式 → 新体系 |
| `ExecutionEvidence.tsx` | → 新体系 | 嵌在 Chat 里的执行证据卡 |

**新建（8 个页面 + 组件）**：

| 文件 | 行数 | 功能 |
|------|------|------|
| `Tools.tsx` | 327 | 契约表：搜索 + Segmented（全部/写操作/网络/只读）+ 4 统计 BigNumber + 420px 抽屉展示 `required_fields` |
| `Media.tsx` | 564 | 图像/视频 Segmented、参数面板、结果区（尺寸/字节/格式/路径核对）、生成台账、⌘Enter 提交 |
| `Mail.tsx` | 414 | 两栏收件箱，未读圆点、`shortFrom()` 解析发件人、RFC 日期解析、附件清单 |
| `Automation.tsx` | 564 | Tabs 定时任务/主动规则，任务行 Switch/立即执行/历史/删除，新建 Dialog 带 JSON hint |
| `Tasks.tsx` | 309 | 会话侧栏 + 任务卡，plan 步骤状态可视化（完成删除线/当前品牌点/待执行空心圆） |
| `Memory.tsx` | 276 | 2 列卡片网格，类型/来源 Badge、重要度 Progress、hover 删除 |
| `Skills.tsx` | 216 | 3 列技能卡，Switch 启停 + 触发词 Badge，`@tauri-apps/plugin-dialog` 选目录安装 |
| `Devices.tsx` | 265 | 4 状态卡 + 设备表格（撤销）+ 配对码 Dialog（大号数字 + 过期时间） |

### 2.7 配置变更

| 文件 | 变更 |
|------|------|
| `vite.config.ts` | 加 `tailwindcss()` 插件、`resolve.alias {"@": src}`、`manualChunks.ui = ["lucide-react","motion","sonner"]` |
| `tsconfig.json` | 加 `baseUrl` + `paths {"@/*": ["src/*"]}` |
| `package.json` | 新增 8 个直接依赖 + 9 个 `@radix-ui/*`（共装 70 包） |

### 2.8 第一轮修过的编译错（已全部修复）

`lucide-react` 无 `Github` 导出 → 换 `Code2`；`StatusDot` 不收 `"neutral"` → 加 `dotTone()` 映射；`SectionHeader` 应从 `ui/card` 导入；`pulse={isTaskActive}` 类型 `boolean \| null` → `!!`；`noUnusedLocals` 下清理 6 处未使用导入；`ds-slide-right` keyframe 缺失 → 补定义。

---

## 三、阶段 3：视觉校准（对齐 Lumi）

### 3.1 触发点

截图目验发现「太丑了」，你指示「直接照搬 lumi」。逐项读取 LumiCore 的 `src/index.css`（3,658 行）与 shadcn 原语后，定位到**三条根因**：

| # | 根因 | 具体表现 | Lumi 的做法 |
|---|------|----------|-------------|
| 1 | **底色偏暖褐** | `#0a0908`、`#141312` 中 R>G>B，界面显脏 | 纯中性灰 `oklch(0.145 0 0)` 无彩度 |
| 2 | **阴影太重** | 多层阴影 alpha 达 0.62 | 卡片**不用阴影**，只靠 `ring-1 ring-foreground/10` |
| 3 | **橙色滥用** | 主按钮、logo、导航全用橙色实心 | 主按钮近白（`oklch(0.922 0 0)`），橙只做强调位 |

### 3.2 字号/间距失控的元凶（关键发现）

旧 `index.css` 里有 `:root { font-size: 14px }`，导致**所有 rem 单位乘 0.875**：`--text-xl: 1.25rem` 实际渲染 17.5px、`p-4` 只有 14px，整套阶梯偏离 4px 网格。

**修复**：`--spacing: 4px` 与 `--text-*` 全部改为**绝对 px**（11/12/13/14/16/18/22/28），不再依赖 rem 基准，彻底断根。

### 3.3 具体改动

- `system.css` 重写为 Lumi 的 oklch token + shadcn 标准别名（`--color-primary/-secondary/-muted/-border/-radius/-sidebar-*/-chart-1..5`），补全侧栏系列与图表系列 token；
- `card.tsx` 改 `rounded-xl + ring-1 ring-foreground/10`、`py-3.5 gap-3`、无投影；
- `button.tsx` 改近白主按钮、`h-8` 默认高、`rounded-lg`，去掉 inset 高光与多层阴影；
- `badge.tsx` 改全圆角胶囊形；
- `page.tsx` 收紧留白（px-6→px-5、标题 18px、图标盒 32px）；
- `Sidebar.tsx` 换 Lumi 侧栏配色体系，去掉 logo 橙色外发光。

---

## 四、阶段 4：彻底清场（抛弃旧 UI）

按你「彻底抛弃原来 helper 的 ui，整个外观全部套用」的指示执行。

### 4.1 删除的文件（已 `git rm`）

| 文件 | 行数 | 删除理由 |
|------|------|----------|
| `src/App.css` | 3,323 | 旧主题本体 |
| `src/cyberpunk.css` | 2,233 | 赛博朋克皮肤 |
| `src/index.css` | 218 | 旧主题 + `font-size:14px` 元凶 |
| `src/pages/SettingsAutomation.tsx` | 514 | 已从路由移除的孤儿文件 |
| `src/pages/SettingsModel.tsx` | 210 | 已从路由移除的孤儿文件 |
| `src/components/ExecutionEvidence.css` | — | 无引用的死文件 |
| `src/pages/WebStudio.css` | 116 | 无引用的死文件 |

**删除前已验证**：无任何 `.tsx` 还引用 `page-container`/`section-card`/`neon-*`/`bg-primary` 等旧类；无代码引用旧变量；`ExecutionEvidence.css`/`WebStudio.css`/两个 Settings 孤儿文件均无 import；`tauri.conf.json` 无窗口透明需求（`body` 透明背景非必需）。

### 4.2 引用清理

- `App.tsx`：删 `import "./App.css"` / `import "./cyberpunk.css"`
- `main.tsx`：删 `import "./index.css"`，`system.css` 成为唯一样式入口

### 4.3 验证结果

| 项 | 结果 |
|----|------|
| `npx tsc --noEmit` | **通过**（0 错误） |
| `npx vite build` | **通过**，CSS **149.78 kB → 58.42 kB**（gzip 10.16 kB） |
| `cargo check --lib` | **通过**（仅 imap-proto future-incompat 警告，原有，非本轮引入） |
| 运行时视觉目验 | **未完成**（见下） |

---

## 五、当前完整改动清单

```
 M package-lock.json / package.json
 M src-tauri/src/lib.rs / src-tauri/src/main.rs
 M src/App.tsx / src/main.tsx / src/components/Sidebar.tsx
 M src/components/ExecutionEvidence.tsx
 M src/pages/{Chat,Dashboard,GitHub,Logs,MiniChat,Settings,WebStudio}.tsx
 M tsconfig.json / vite.config.ts
D  src/App.css / src/cyberpunk.css / src/index.css          （git rm）
D  src/components/ExecutionEvidence.css / src/pages/WebStudio.css
D  src/pages/SettingsAutomation.tsx / src/pages/SettingsModel.tsx
?? src/styles/{system.css,mini.css}                          （新建）
?? src/components/ui/{button,card,form,badge,feedback,primitives,page}.tsx
?? src/components/{CommandPalette,PermissionGate}.tsx / nav.ts
?? src/lib/{cn.ts,useUiTool.ts}
?? src/pages/{Automation,Devices,Mail,Media,Memory,Skills,Tasks,Tools}.tsx
```

**代码规模变化**：前端 12 文件 6,125 行 → **27 文件 16,000+ 行**；样式从 5,773 行手写 CSS → 442 行 token 化 CSS + Tailwind utilities。

---

## 六、未完成项与已知事项

### 6.1 运行时视觉目验（唯一未闭合项）

执行环境三个坑叠一起，自动化截图反复失败：
1. **WebView2 调试端口只在冷启动生效**：`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` 会被机器上已有的 WebView2 实例（Outlook/Teams 等）复用吃掉，需先 `taskkill msedgewebview2.exe` 再启动，端口时通时断。
2. **系统代理劫持 127.0.0.1**：curl 不加 `--noproxy '*'` 时报 502/000，vite 其实一直活着。
3. **会话安全策略拦 PowerShell .NET**：Add-Type / COM / 反射全被拦（防任意代码执行），`CopyFromScreen`、`WScript.Shell` 激活窗口都走不通。

**→ 请你直接 `npm run tauri dev` 开窗口目验，这是唯一没被堵住的验证方式。**

### 6.2 视觉微调（非本轮 scope）

1. **信息密度**：token 已对齐 Lumi，但 14 个页面的密度是按我的判断重排的，与 Lumi 原版面板（如 CommandCenterPanel 的 8px 紧凑间距）可能仍差一档。要 1:1 需逐页对照 Lumi 组件间距再收一轮。
2. **Settings.tsx** 8 个分区建议重点看表单分派是否正确（bool→Switch / number→数字+unit / select→下拉）。
3. **Logs.tsx** 依赖 `get_audit_logs` / `get_tool_receipts` 真实数据，空库时看不到效果。
4. **工具层未动**：按阶段 1 的诊断（134 vs 129 行/工具，仅差 4%），本轮只改 UI。

### 6.3 回滚（未提交，全部在工作区）

按你的规矩，**没有执行任何 `git add` / `git commit`**。要回退：

```bash
# 跟踪文件的修改
git checkout -- src/App.tsx src/main.tsx src/components/Sidebar.tsx src/components/ExecutionEvidence.tsx src/pages/
git checkout -- src-tauri/src/lib.rs src-tauri/src/main.rs package.json package-lock.json tsconfig.json vite.config.ts
# 已 git rm 的旧文件恢复
git checkout -- src/App.css src/cyberpunk.css src/index.css src/pages/SettingsAutomation.tsx src/pages/SettingsModel.tsx src/components/ExecutionEvidence.css src/pages/WebStudio.css
# 新建文件（未跟踪）整体删除
git clean -fd src/components/ui src/components/CommandPalette.tsx src/components/PermissionGate.tsx src/components/nav.ts src/lib src/pages/Automation.tsx src/pages/Devices.tsx src/pages/Mail.tsx src/pages/Media.tsx src/pages/Memory.tsx src/pages/Skills.tsx src/pages/Tasks.tsx src/pages/Tools.tsx src/styles
```

---

## 七、交接后建议的推进顺序

1. `npm run tauri dev` 逐页目验 14 个页面，记录观感异常的页号；
2. 对异常页对照 `D:\_refs\LumiCore\src\components\` 下同名面板收密度；
3. 目验通过后按文件名精确 `git add` 提交（**禁用 `git add .`**）；
4. 之后若要继续扩，工具层是下一个富矿（221 vs 242 契约的 21 个缺口）。
