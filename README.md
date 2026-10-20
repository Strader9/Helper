# PC Guardian AI

> 本地优先的 Windows 桌面 AI Agent — 安全第一，效率极致

## 技术栈

- **前端**：React 18 + TypeScript 5 + Vite 5 + Tailwind v4 + Radix 原语 + lucide 图标
- **桌面框架**：Tauri 2（WebView2）
- **后端**：Rust 2021
- **数据库**：SQLite（rusqlite + bundled），35 张表
- **本地 LLM**：Ollama（默认 `qwen3:8b`）
- **全模态感知**：Jarvis（MiniCPM-o 4.5，独立 sidecar 进程，可选用）
- **媒体生成**：火山方舟（图像 / 视频）、可切换其它 OpenAI 兼容端点
- **工具层**：221 个能力契约，37 个能力域

## 核心原则

1. **极致效率** — 最高效的代码实现，避免不必要的抽象和依赖
2. **最小体积** — 严格控制依赖数量和大小，安装包尽量小
3. **Security > Correctness > Reliability > Maintainability > Performance > Features**

## 界面

界面整体套用 **Lumi 设计体系**：深空星云主题（背景 oklch 暗调、蓝紫 hue 264），四层星点 + 四层星云气团铺在 `body::before` 上，配玻璃质感底板与线性图标。

入口是 `src/LumiApp.tsx`（HashRouter），13 条路由：

| 路由 | 页面 | 路由 | 页面 |
|------|------|------|------|
| `/` | 指挥中心 | `/tasks` | 任务 |
| `/chat` | 对话 | `/media` | 媒体生成 |
| `/settings` | 设置（6 个 Tab） | `/mail` | 邮件 |
| `/logs` | 日志 | `/automation` | 自动化 |
| `/tools` | 工具 | `/devices` | 多端 |
| `/skills` | 技能 | `/jarvis` | 贾维斯 |
| `/memory` | 记忆 | | |

样式全部落在 `src/index.css`（3685 行，Tailwind v4 + oklch token），另有 `src/styles/` 三个辅助样式表。

> **字号一律用绝对 px，不要用 rem 缩放。** 早期文件顶部的 `:root{font-size:14px}` 会让整条字体阶梯缩到 87.5%，全站视觉同时错位——这是"界面看起来哪里都不对"的根因。

## Jarvis：全模态桌面感知

`LyHub/pub-local-jarvis` 以**独立进程**接入，本侧只负责生命周期托管与 HTTP 桥接（`src-tauri/src/lib.rs` 里 5 个命令：`jarvis_health` / `jarvis_start` / `jarvis_stop` / `jarvis_chat` / `jarvis_duplex`）。它每秒看一帧屏幕、听一秒系统声音，由 MiniCPM-o 4.5 判定该 `LISTEN` 还是 `SPEAK`。

后端路由前缀是 `/api/v1`（**不带会 404**），默认 `http://127.0.0.1:8000`。停止用 `taskkill /T` 而不是 `Child::kill`，否则 uvicorn 派生的子进程会留成孤儿占住端口。

### 显存让路

8 GiB 显存装不下两套模型，这是物理上限不是配置问题：

| 组件 | 显存 |
|------|------|
| Jarvis 三模型（LLM Q4_K_M + 视觉塔 F16 + 音频塔 F16） | 约 7.2 GiB |
| Ollama `qwen3:8b` | 约 5.4 GiB |
| RTX 4060 Laptop 总量 | 8 GiB |

因此管家在拉起 Jarvis 之前会主动让路，两个设置项控制：

| 设置键 | 默认 | 说明 |
|--------|------|------|
| `llm.warmup_on_start` | `true` | 启动时是否预热本地 LLM。为 `false` 时不占显存 |
| `llm.keep_alive` | `30m` | 推理结束后的模型驻留时长，`0` 表示用完立即卸载 |

`jarvis_start` 在拉起后端之前调 `ollama.unload()`（空 `generate` + `keep_alive:"0"`，Ollama 没有专门卸载接口，这是官方等价写法），失败只告警不阻塞。

**代价要说清**：Jarvis 占着显卡时，管家再请求本地 LLM 会因显存不足失败。现有机制保证的是"谁在被使用谁拿到显存"，不是"两个同时可用"。

### 实测数字

RTX 4060 Laptop 8 GiB 上，原版 Q4_K_M **无需量化即可全量 offload**：

| 项 | 数值 |
|---|---|
| `ollama stop` 后桌面基线 | 647 MiB |
| worker 加载峰值 | 7778 MiB / 余 179 MiB（三模型全上卡） |
| 持续感知循环 2 分钟 | 余量稳定 77–124 MiB，无 OOM、无 CPU 回退 |
| `/assistant/chat` 单条 | 0.8 s |

> **注意 `n_gpu_layers=-1` 不等于"自动按显存适配"。** 在 llama.cpp 里 `-1` 的语义是**全量**（`include/llama.h` 原文："a negative value means all layers"），真正的自适应走 `common_fit_params()`，而 omni 的加载路径不经过它。所以显存一旦不够，worker 会整体退回 CPU，没有"少放几层"的中间态。

## 语音播报

TTS 引擎在 `src/lib/tts.ts`（约 500 行），`src/hooks/useVoice.ts` 的 `speak` / `stopSpeaking` 签名不变、内部委托给它。绕的是 WebView2 `speechSynthesis` 三个实测缺陷：

| 缺陷 | 处理 |
|------|------|
| 长文本静默截断 | 分句队列：段落 → 句末标点 → 逗号 → 硬切，默认 90 字/句 |
| 长朗读卡在 `paused` | 每 6 秒 `resume()` 心跳 |
| Markdown 符号被逐字念出 | `stripMarkupForSpeech()` |

音色优先级 `Huihui > Xiaoxiao > Yunxi > Yaoyao > Kangkang`（OneCore 通道，WebView2 走这条）。

> **`stop()` 必须先 `finish` 再清 hooks。** 反过来的话调用方永远收不到 `onEnd`，实时对话循环会静默断掉。

## 网页工坊

侧边栏“网页工坊”可以导入 PDF、DOCX、Markdown 或 TXT 需求文档，也可以直接粘贴需求；应用通过设置中的 Ollama 地址和模型在后台流式生成单文件 HTML，并实时显示进度和生成源码，可在隔离预览框中查看、按文字继续修改并导出。生成不设固定时间上限，关闭页面或应用会中断任务。扫描版 PDF 暂不支持 OCR。文档内容会发送到配置的 Ollama 服务；使用远程服务时，请按远程服务的数据处理规则操作。

**关于上下文窗口**：Ollama 的 `num_ctx` 默认为 4096 token，不足以容纳需求文档，因此网页生成会显式指定上下文窗口（8GB 显存下取 8192，仍可 100% 跑在 GPU 上）。

> **注意：`num_ctx` 只是窗口大小，不是输出上限。** 上下文写满后 Ollama 会滑动窗口继续生成，因此 `num_predict` 必须显式给值：设为 `-1` 时模型可以无限写下去（实测一次规格化跑出 20543 token、耗时 11 分钟仍未停止）。应用现在按"提示词 + 单轮输出 ≤ 窗口"配平参数，保证生成过程中提示词不会被滑出上下文——被滑出会让模型只看到一段没写完的内容，进而退化成长列表复读。单轮写不完的页面由自动续写补齐（最多 6 轮，每轮都重新带上完整需求）。

**文档加工流水线**：导入的文档不会直接送进提示词，而是先经过两道纯规则工序，避免文档格式和篇幅影响生成：

```
① 清洗（纯规则，毫秒级）
  去页眉页脚与页码、拼回硬换行、全角字母数字转半角、去掉零宽字符、
  折叠超长 base64 串、合并重复短行、逐行保留 Markdown 表格
        ↓
② 梳理（纯规则，毫秒级）    ← 这一步承担"读大文档"
  按标题切段 → 按相关性打分 → 按标准格式的六个小节分桶分配预算
  （页面结构 / 功能需求 / 数据与字段 / 交互与状态 / 视觉规范 / 约束）
  预算不够时按行裁剪，优先保住色值、优先级标记、字段与表格行
  产出「梳理稿」，固定不超过 6000 字
        ↓
③ 规格化（模型 1 次调用）
  把梳理稿转成标准格式
        ↓
④ 生成单文件 HTML（模型 1 次调用，必要时自动续写）
```

**为什么不让模型读文档**：qwen3:8b 自身上下文上限是 40960 token，在 8GB 显存上实际只能开到 8192；而 10 万汉字约等于 6~7 万 token。让模型逐块通读一遍既慢（十几分钟）又必然漏。所以"读薄"交给纯规则：**实测 10.4 万字的文档，清洗 + 梳理共耗时 37 毫秒**，产出 5997 字的梳理稿。整个预处理阶段只有一次模型调用，与文档长度基本无关。

**梳理按小节分桶，而不是全局取前几名**：早期版本按分数全局挑选，结果是整个"视觉与体验规范"章节被一次性丢掉，生成的页面完全没有配色依据。现在六个小节各有独立预算，保证每个方面都有代表内容。

**梳理稿可以在界面上编辑**：生成后左侧会出现「梳理稿」面板，里面就是模型真正读到的内容，可以直接修改再生成。没被选中的章节标题会列在实时日志里，方便判断是否有重要内容被漏掉。需求文本没被改动时会复用上一次的结果，不重复加工。单份文档上限 30 万字。

### 交付闸门：把"能跑起来的空壳页"拦在门外

生成完不等于交付完。每轮输出都要过 `verify_page` 自检，只有**三重条件同时满足**才允许结束：

1. 没有语法级致命问题（`Critical`，如括号不配对、脚本被截断）
2. 没有待补的缺失函数或未绑定的按钮（`function_appendix_plan`）
3. 没有值得返修的功能性缺陷（`functional_repair_worthwhile`）

第 3 条是后加的，因为它揭出了一个漏洞：**"按钮点了没反应"曾经完全能溜过交付**。早先 `BUTTON_NO_HANDLER`（按钮没有处理函数）、`DEAD_FUNCTION`（定义了但没人调用的空壳函数）、`FEATURE_MISSING`（需求里的功能没实现）都只算 `Major`，而闸门只看 `critical == 0` —— 于是"页面上 8 个按钮全是摆设"这类最典型的失败形态，一次返修都不会触发，直接交付到用户手里。

现在这三类归为**功能性缺陷**（`FUNCTIONAL_CODES`），单独识别、单独加权：

| 缺陷 | 权重 | 说明 |
|------|------|------|
| `BUTTON_NO_HANDLER` | 5 | 已从 `Major` 提为 `Critical` |
| `DEAD_FUNCTION` | 4 | 空壳函数，数量 ≥ 3 时提为 `Critical` |
| `UNDEFINED_REFERENCE` | — | 调用不存在的函数，恒触发返修 |
| `UNUSED_DATA_POOL` | — | 数据池从来没人读 |
| `OPTION_UNHANDLED` / `FEATURE_MISSING` | 2 | 需求功能缺失 |

只要出现 `UNDEFINED_REFERENCE` 或 `BUTTON_NO_HANDLER` 就立即返修；其余按权重求和，**累计 ≥ 6 才值得再跑一轮**（避免为了一个可疑的空壳函数反复烧算力）。返修上限从 3 轮放宽到 5 轮。

顺带修掉一个自上线起就存在的 bug：`check_dead_functions` **从来没触发过**。它统计函数名出现次数时把 `<script>` 自身也算进去了，函数定义处替自己贡献了 1 次，和脚本内的 1 次相加恒 ≥ 2，永远判不出"死函数"。加了 `html_without_scripts()` 剥掉脚本内容后再数外部引用，这个检查才真正生效。

### 定向修改：点选元素，只改那一处

生成后不满意，不用重写整句需求。在预览框里**点一下要改的元素**，再写一句"把这个按钮的文字改成「再摇一次」"，就只有那一处会变。

预览 iframe 是 `sandbox="allow-scripts"`（**没有** `allow-same-origin`），父页面拿不到它的 DOM，所以点选脚本注入到 iframe 内部，用 `postMessage` 把描述回传：

```
元素：button
选择器：main > section > button:nth-of-type(1)
当前文字：生成名字
HTML：<button onclick="generateNames()">生成名字</button>
```

**最后那行 `HTML：` 是关键**。CSS 选择器（`button#gen`）和 HTML 源码（`<button id="gen">`）本来就不同构，拿选择器去 `contains` 源码必然落空。`outerHTML` 是唯一字面照抄原文的锚点——实测手工补上这一行后，定向修改立刻从"整份照抄、什么都没改"变成"1 行不同、精确命中"。

提示词里位置信息**排在需求前面**：

```
【只改这一处】用户在预览里点选了下面这个元素，请只针对它做修改，其余部分原样保留：
<锚点描述>

<用户的具体要求>
```

为什么位置在前：模型是顺序读的，先看到"改哪儿"再看"怎么改"，才不会把整页需求重新理解一遍然后重写。

**页面过长时按锚点摘片段**：整页超过 6000 字符时，早先的实现只回传**开头 3000 字符**——目标元素要是落在后面（实测一个 18337 字符的页面，按钮在 10667 字符处），模型根本看不见它要改的地方，"只改这一处"就退化成盲改。现在 `take_excerpt()` 按锚点定位，以命中位置为中心取片段（前留 1/3、后留 2/3，因为改动点后面通常还有兄弟节点和相关脚本）。锚点取不到时退回开头片段而不是给空——**有原文在，模型至少知道页面的视觉风格；什么都不给，它会凭需求重写一份，等于把用户的页面推倒重来**。

**定位要挑最硬的锚点**：点选描述是多行的，早先按行顺序取"第一个能在源码里找到的"，这是错的——「当前文字：生成名字」这种短文本在页面上往往**先出现在别处**（标题、说明、注释），于是片段根本没包住用户点选的元素。实测目标在 11359 字符处、片段却在别的地方，合并时把不属于目标的一大段内容顶替掉了，18337 字的页面掉了 5906 字。现在按**候选强度**排序：`HTML：` 那行的 `outerHTML` 逐字来自源码，最硬；其次是长选择器路径；最短的"当前文字"排最后。

**CSS 选择器要展开成属性写法**：点选脚本给的是选择器（`button#gen`），源码是 HTML 文本（`<button id="gen">`）——两者不同构，直接匹配必然落空。`anchor_variants()` 把 `#id` / `.class` 拆成 `id="…"` / `class="…"` 再试。

### 片段内改 + 程序合并：让模型只改它看得见的那一块

**这是本次修掉的最严重缺陷。** 早先定向修改时，程序回传 3006 字片段却要求模型"输出完整 HTML"。这**不可能做到**——模型只看到 16% 的源码，却要交出 100% 的成品，只能凭空造出剩下 84%。实测的后果是灾难性的：

```
18337 字的页面 → 模型输出 2900 字
  <body>、</body>、<script>、</script> 全部丢失
  文件在 CSS 中途被切断，末尾还留着一句「（后面省略）」（模型把我写的省略提示抄进了代码）
  自检却报"通过" —— 因为 verify_page 是按"页面上有的东西"找缺陷的，而残品上什么都没有
```

用户点"把按钮文字改一下"，结果整页被删掉 84%。

现在定向修改改成**两次分工**：模型只负责改它看得见的那一小块，程序负责把这一块贴回原位。

```
片段模式（整页超长且已定位到目标元素）：
  提示词：只回传包含目标元素的片段
  要求：  **只输出你改动过的那一小块代码**，不要输出整页
  产物：  程序用 merge_patch() 把这一小块贴回原页对应位置
```

配套的是两道**拒绝合并**的防线——宁可告诉用户"这次没改成"，也不能把一份残缺的页面交出去：

1. patch 自身必须结构自洽（开了 `<button>` 就得有 `</button>`）
2. patch 里的身份特征（`id` / `class`）要还认得出来是同一个元素
3. 锚点定位不到就不进片段模式——连"改哪儿"都不知道，让模型"只输出一小块"没有依据

还有两个坑是实测踩出来的：

- **提示词自相矛盾**：片段模式下 `previous_note` 说"只输出一小块"，而要求行仍写着"返回完整 HTML"。两条互斥指令同时摆在一个 8B 模型面前，它**两头都不照做**——只吐了 28 token 的敷衍内容。现在两种模式各用一套措辞。
- **省略标记混进了代码**：`take_excerpt` 曾经在片段两端加「（前面省略）」「（后面省略）」，这些是**给人看的自然语言**，却被直接拼进 `<current_html>` 代码块，模型把它当代码抄进了产物。现在 `take_excerpt` 只返回纯片段，省略说明由调用方写在代码块外面。

### 产物退化兜底：改烂了就保留原版

`verify_page` 拦不住上面那种灾难，因为它是按"页面上有的东西"找缺陷的，而残品上什么都没有。所以另加一道只看两条硬事实的兜底 `looks_degraded()`：

1. **结构丢失**：原版有的 `<body>` / `<script>` / `</html>`，新版没了
2. **体量骤降**：新产物不足原版的 60%（放宽到 60% 是给"用户要求删掉一大块"留余地，同时仍能拦住 16% 那种灾难）

命中就回滚保留原页，并在日志里如实说明原因。这道判断不看抽象分数——**分数会被"页面上什么都没有"骗过去，硬事实不会**。

### 版本快照：随时回到上一版

每次生成或修改都会留一份内存快照（不落库、不写盘），按需求指纹分链，每条链最多保留 20 版。界面上「历史版本」面板倒序列出每一版的时间、备注、健康分与残留缺陷数，点「回到这一版」即可还原——还原本身也会存成新的一版，所以回退错了还能再回去。需求文本相同即视为同一条链，因此"同一份需求反复调"不会把历史冲散。

### 用量度量

生成轮与每轮修复都会记录 Ollama 返回的真实用量（`prompt_eval_count` / `eval_count` 及各自耗时），日志形如：

```
提示词 1266 token（3.2s）｜输出 1199 token（30.1s，40 tok/s）
```

交付前打一行终检摘要：干净时写「交付版本功能自检通过（修复 N 轮，用量…）」，否则逐条列出残留缺陷。这行也是判断"这次生成到底健康不健康"最快的方式。

## 架构设计

### 三层闸门架构

LLM 永不直接接触系统 API，所有操作必须经过：

```
ToolRegistry → PermissionManager → ToolExecutor
```

1. **ToolRegistry** — 工具注册中心，所有工具必须注册，未注册工具一律拒绝
2. **PermissionManager** — 权限判定中心，五级风险体系，白名单机制
3. **ToolExecutor** — 实际执行层，超时控制，审计日志

### 能力契约

每个工具的"会做什么、可能造成什么副作用、怎么验证"都写在 `src-tauri/src/tools/contract.rs` 里，启动时核对覆盖率并打日志：

```
[Capability] 契约覆盖 221/221，实质副作用 110/122 已声明验证策略
```

契约覆盖率是硬指标——新增工具没写契约，启动日志里的分母就会掉，比代码评审更早发现遗漏。

### 五级风险体系

| 等级 | 策略 |
|------|------|
| 🟢 SAFE | 直接放行 |
| 🔵 LOW | 直接放行 |
| 🟡 MEDIUM | 弹窗确认 |
| 🟠 HIGH | 弹窗 + 二次确认 |
| 🔴 CRITICAL | 禁止执行 |

### 能力域

37 个执行器域（`src-tauri/src/tools/executors/`）：文件与系统（file_ops / fs_extra / os_extra / system）、办公文档（office / pdf / ocr / diff）、媒体（media / image_gen / video_gen / media_provider）、研发（git / github / code / browser / web / mcp）、业务（mail_read / messaging / calendar / industry / industry_ecommerce / cad）、自动化（scheduler / workflow / work_takeover / autonomy / desktop_ai / tasks）、知识（knowledge / memory / skill / extension / program），以及 client 兜底域。

## 项目结构

```
Helper/
├── src/                              # 前端
│   ├── LumiApp.tsx                   # 真正入口（HashRouter，13 路由）
│   ├── pages/lumi/                   # 12 个页面（Lumi 版）
│   ├── components/lumi/              # 玻璃卡 / 图标盒 / 页头 / 徽标 / 开关
│   ├── components/ui/                # Radix 原语封装
│   ├── lib/                          # tts 引擎、工具函数
│   ├── hooks/                        # useVoice 等
│   ├── pets/                         # 桌宠
│   ├── index.css                     # 设计系统（3685 行）
│   ├── styles/                       # 辅助样式表
│   └── i18n/ core/ services/ contexts/
├── src-tauri/src/
│   ├── lib.rs                        # 命令注册 + 状态装配（含 jarvis 桥接）
│   ├── main.rs                       # 启动、预热策略、托盘
│   ├── llm/                          # Ollama 客户端（keep_alive / unload）
│   ├── tools/                        # registry + contract + 37 个 executors/
│   ├── security/ agent/ workflow/ work_takeover/
│   ├── webgen/ webpublish/           # 网页工坊
│   ├── scheduler/ proactive/ devices/
│   ├── mail_mime.rs webpublish.rs
│   └── db/                           # 连接 + schema（35 表）
├── .cargo/config.toml                # target-dir 指向 E 盘，避免撑爆系统盘
├── 交接文档-2026-10-20-详细版.md      # 环境硬约束 / 全链路 / 14 条踩坑
└── README.md
```

## 数据库

35 张表，按用途分组：

| 分组 | 表 |
|------|-----|
| 基础 | `migrations` `settings` `audit_logs` |
| 安全 | `trusted_apps` `allowed_directories` `trusted_tools` `system_events` |
| 对话 | `chat_sessions` `chat_messages` `agent_tasks` `agent_observations` |
| 工具 | `tool_receipts` `extensions` `extension_receipts` |
| 技能与记忆 | `skills` `memory_items` `knowledge_documents` `knowledge_chunks` |
| 自动化 | `automation_rules` `proactive_rules` `scheduled_jobs` `scheduled_job_runs` |
| 编排 | `workflow_defs` `workflow_runs` `workflow_steps` `work_takeover_tasks` `work_takeover_evidence` |
| 多端 | `devices` `device_pairing_codes` |
| 业务 | `calendar_events` `industry_snapshots` `media_generations` `github_snapshots` `notification_logs` |

数据库落在 `%APPDATA%\pc-guardian\pc-guardian.db`。

> 同一个机器上还有 `%APPDATA%\ai.pcguardian.app\` 目录，里面是 4 KB 的空库，**不是真正的数据库**，手动改配置时不要写错地方。

## 开发

### 前置要求

- Node.js >= 18
- Rust >= 1.77（edition 2021）
- Windows 10/11（目标平台）
- Ollama（运行本地模型）
- 可选：ffmpeg（音视频工具链，放在 `D:\ffmpeg\bin` 或任意 PATH 目录）
- 可选：Jarvis 运行时（`E:\jarvis\runtime`，含 `jarvis-native-worker-cuda.exe` 与 CUDA 13 运行库）

### 安装依赖

```bash
npm install
```

### 开发模式

```bash
npm run tauri dev
```

### 构建生产版本

```bash
npm run tauri build
```

### 与 Jarvis 共存

先起 worker，再起后端，最后在界面 `/jarvis` 页点启动：

```bash
# 1. 原生 worker（占显存，先停掉 Ollama 或让它让路）
jarvis-native-worker-cuda.exe \\.\pipe\JarvisSidecar.v1 E:\jarvis\models\MiniCPM-o-4_5-gguf

# 2. Python 后端（配置路径必须显式传，仓库里没有 local.toml 覆盖机制）
JARVIS_CONFIG=E:\jarvis\config\test.toml PYTHONPATH=src <venv>\python.exe -m uvicorn jarvis_backend.app:app --port 8000
```

配置里 `[native] mode` 必须是 `process` 才会连真 worker；默认 `fake` 只跑假后端。

## 开发路线图

| Phase | 内容 | 状态 |
|-------|------|------|
| Phase 0 | 架构设计 | ✅ 完成 |
| Phase 1 | 项目骨架初始化 | ✅ 完成 |
| Phase 2 | Agent Runtime + Tool Calling 基础 | ✅ 完成 |
| Phase 3 | 系统监控服务 | ✅ 完成 |
| Phase 4 | 系统工具集（221 契约 / 37 域） | ✅ 完成 |
| Phase 5 | Ollama 集成 + 完整对话 | ✅ 完成 |
| Phase 6 | 浏览器工具 | ✅ 完成 |
| Phase 7 | GitHub 工具 + 趋势分析 | ✅ 完成 |
| Phase 8 | 自动化 + 通知 | ✅ 完成 |
| Phase 9 | Lumi 界面体系切换 | ✅ 完成 |
| Phase 10 | Jarvis 全模态感知接入 | 🚧 界面启动链路待补配置注入 |
| Phase 11 | 媒体生成（图像 / 视频） | 🚧 待开通方舟模型权限 |

## 安全红线

1. ❌ 不向 LLM 暴露任何 shell/powershell/cmd 执行入口
2. ⚠️ 凭据当前存在本地 SQLite 的 `settings` 表，读出给界面时一律走 `mask_secret()` 掩码（只留末 4 位）。**目标态是 Windows Credential Manager**，尚未实现——因此不要把生产密钥填进来，且一旦把带凭据的数据库或日志分享出去，必须立刻轮换
3. ❌ CRITICAL 级操作一律禁止执行
4. ✅ MEDIUM+ 风险操作必须用户弹窗确认
5. ✅ 文件操作严格校验路径，防止路径穿越
6. ✅ 所有工具调用写入审计日志，不可删除不可篡改

## License

MIT
