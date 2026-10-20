# PC Guardian AI

> 本地优先的 Windows 桌面 AI Agent —— 安全第一，效率优先。

用自然语言指挥电脑，但数据不出本机：对话、记忆、知识库、自动化规则全部落在本地 SQLite，推理走本机 Ollama，只有你显式配置的云端服务（媒体生成、邮件）才会联网。

LLM 永远不直接接触系统 API。所有操作必须穿过权限闸门，并留下一条不可篡改的审计记录。

## 特性

- **本地推理** —— 对接任意 Ollama 实例，默认 `qwen3:8b`，断网可用
- **三层闸门** —— `ToolRegistry → PermissionManager → ToolExecutor`，未注册工具一律拒绝
- **五级风险** —— MEDIUM 弹窗确认、HIGH 二次确认、CRITICAL 直接禁止
- **能力契约** —— 221 个契约逐条声明「会做什么 / 有什么副作用 / 怎么验证」，启动时核对覆盖率
- **完整审计** —— 每次工具调用落库，不可删除、不可篡改
- **全模态感知**（可选）—— 独立进程每秒取一帧屏幕、一段系统声音，自主判断该听还是该说
- **网页工坊** —— 需求文档直接产出单文件 HTML，带自检闸门、定向修改与版本回退
- **语音播报** —— 分句队列 + 心跳保活，规避 WebView2 长文本截断
- **媒体生成** —— 图像与视频，OpenAI 兼容协议，服务商可替换
- **多端配对** —— 手机扫码接入，复用桌面端能力

## 技术栈

| 层 | 选型 |
|---|---|
| 前端 | React 18 · TypeScript 5 · Vite 5 · Tailwind v4 · Radix 原语 · lucide 图标 |
| 桌面容器 | Tauri 2（WebView2） |
| 后端 | Rust 2021 |
| 存储 | SQLite（rusqlite + bundled），35 张表 |
| 本地模型 | Ollama，默认 `qwen3:8b` |
| 全模态感知 | 独立 sidecar 进程加载 MiniCPM-o 4.5，可选 |
| 媒体生成 | 火山方舟，兼容任意 OpenAI 协议端点 |
| 语音 | Web Speech API（识别）+ OneCore 音色（合成） |

## 快速开始

### 前置

- Node.js ≥ 18
- Rust ≥ 1.77（edition 2021）
- Windows 10 / 11
- Ollama（本地模型运行）

可选：ffmpeg（音视频工具链，放任意 PATH 目录即可）、感知运行时（见下文）。

### 安装与运行

```bash
npm install
npm run tauri dev      # 开发模式
npm run tauri build    # 打包
```

### 首次配置

打开应用 → 设置页：

1. **模型** —— 填 Ollama 地址（默认 `http://localhost:11434`）与模型名，点测试连通
2. **安全** —— 按需放入受信任目录与工具白名单
3. **凭据** —— 邮件、媒体生成等密钥，就地填入（读出时一律掩码）

默认端口：前端 `1420`、本地网关 `4769`、Ollama `11434`。全局快捷键 `Alt+Shift+Space`。

## 架构

### 三层闸门

```
ToolRegistry → PermissionManager → ToolExecutor
```

1. **ToolRegistry** —— 所有工具必须先注册；模型请求一个不存在的工具，直接拒绝，不会落到执行层
2. **PermissionManager** —— 五级风险判定 + 白名单 / 受信任目录校验
3. **ToolExecutor** —— 超时控制、副作用执行、审计落库

### 一次对话的执行路径

```
用户输入
  → Agent Loop（本地模型）
  → 决定调用工具
  → ToolRegistry 查表          未注册 → 拒绝
  → PermissionManager 判风险    高危 → 弹窗等确认
  → ToolExecutor 执行           超时 → 中止并报告
  → 审计落库 + 结果回灌模型
  → 流式输出给用户
```

### 风险等级

| 等级 | 策略 |
|---|---|
| SAFE / LOW | 直接放行 |
| MEDIUM | 弹窗确认 |
| HIGH | 弹窗 + 二次确认 |
| CRITICAL | 禁止执行 |

风险等级由工具契约声明，不由模型自行判断。用户勾选「始终允许」后，工具名 + 路径 + 风险等级写入 `trusted_tools`，下次同条件自动放行。

### 能力契约

每个工具的「会做什么、可能造成什么副作用、怎么验证」登记在 `src-tauri/src/tools/contract.rs`，启动时核对覆盖率并打日志：

```
[Capability] 契约覆盖 221/221，实质副作用 110/122 已声明验证策略
```

契约覆盖率是硬指标：新增工具漏写契约，启动日志里的分母立刻不对，比代码评审更早发现问题。

### 能力域

37 个执行器域，按用途分七类：

| 分类 | 域 |
|---|---|
| 文件与系统 | `file_ops` `fs_extra` `os_extra` `system` |
| 办公文档 | `office` `pdf` `ocr` `diff` |
| 媒体 | `media` `image_gen` `video_gen` |
| 研发 | `git` `github` `code` `browser` `web` `mcp` |
| 业务 | `mail_read` `messaging` `calendar` `industry` `industry_ecommerce` `cad` |
| 自动化 | `scheduler` `workflow` `work_takeover` `autonomy` `desktop_ai` `tasks` |
| 知识与扩展 | `knowledge` `memory` `skill` `extension` `program` |

`client` 作为兜底域。

## 功能

### 对话

请求走 Ollama 的 HTTP 接口，支持任意本地或远程 Ollama 实例。流式输出，回答过程中的工具调用逐条经过权限闸门裁决。会话与消息落库，可回溯。

### 网页工坊

导入 PDF / DOCX / Markdown / TXT 需求文档，或直接粘贴需求，后台流式生成单文件 HTML：隔离预览框查看、点选元素做定向修改、导出、按版本回退。

大文档不整份塞进提示词。先用纯规则做「清洗 → 按相关性分桶梳理」压到 6000 字以内（10 万字文档约 37 毫秒），再交给模型规格化——预处理阶段只有一次模型调用，且与文档长度基本无关。六个维度各有独立预算（页面结构 / 功能需求 / 数据与字段 / 交互与状态 / 视觉规范 / 约束），保证不会有某一方面被整段丢掉。

生成结束不等于交付完成。每轮产物都要过自检闸门，三条同时满足才放行：

1. 无语法级致命问题（括号不配对、脚本被截断）
2. 无未绑定的按钮、无空壳函数
3. 无值得返修的功能缺陷

未达标自动返修，上限 5 轮。产物若出现结构丢失或体量骤降，回滚保留原版——宁可如实报告「这次没改成」，也不交出残缺页面。

### 全模态感知（Jarvis）

以独立进程接入，本侧只负责生命周期托管与 HTTP 桥接，五个命令：`jarvis_health` / `jarvis_start` / `jarvis_stop` / `jarvis_chat` / `jarvis_duplex`。

它每秒取一帧屏幕、一段系统声音，由 MiniCPM-o 判定当前该 `LISTEN` 还是 `SPEAK`。

**启动顺序不可颠倒：**

1. 拉起 native worker —— 它是真正加载模型、占用显存的进程
2. 等命名管道就绪 —— 轮询 `\\.\pipe\JarvisSidecar.v1`，最长 180 秒
3. 拉起控制面服务 —— 注入配置后轮询 `/health` 直到应答

顺序反了，后端在启动阶段就找不到管道并直接退出（`Application startup failed`）。

停止时必须连同 worker 一起收（`taskkill /T`）。只停控制面而不杀 worker，约 7 GiB 显存会被一直占住。

手动启动参考：

```bash
# 1. 原生 worker（占显存，先让本地对话模型让路）
jarvis-native-worker-cuda.exe \\.\pipe\JarvisSidecar.v1 <模型目录>

# 2. 控制面（配置路径必须显式传入）
JARVIS_CONFIG=<配置文件> PYTHONPATH=src <venv>\python.exe -m uvicorn jarvis_backend.app:app --port 8000
```

配置里 `[native] mode` 必须为 `process` 才会连接真实 worker，默认 `fake` 只跑假后端。

### 语音

识别走 Web Speech API；合成引擎在 `src/lib/tts.ts`，对外签名稳定，内部处理三件事：

- 长文本按句末标点、逗号、硬切三级分句排队（默认 90 字/句）
- 每 6 秒 `resume()` 心跳，避免长朗读卡在 `paused`
- 朗读前剥离 Markdown 标记，不把符号逐字念出来

音色优先级 `Huihui > Xiaoxiao > Yunxi > Yaoyao > Kangkang`（Windows OneCore 通道）。

### 媒体生成

图像走 `POST {base_url}/images/generations`，视频走异步任务 `POST {base_url}/contents/generations/tasks`，轮询至完成后下载产物。默认对接火山方舟，也可切任意 OpenAI 兼容端点。

```mermaid
提交任务 → 记录 task_id → 轮询状态 → 下载 → 校验 → 落库
```

判定标准是**重新打开生成的文件**确认尺寸与体积，而不是只看 HTTP 200——服务端答应不等于产物可用。

> 当前状态：链路与界面已就绪，但方舟账号下对应模型未开通，调用会返回 `ModelNotOpen`。在方舟控制台「开通管理」激活对应模型即可使用，无需改代码。

## 项目结构

```
├── src/                          # 前端
│   ├── index.css                 # 设计系统（Tailwind v4 + oklch token）
│   ├── pages/                    # 页面
│   ├── components/               # 通用组件、物理原语封装
│   ├── lib/                      # tts 引擎、工具函数
│   ├── hooks/                    # useVoice 等
│   ├── contexts/                 # Tauri 适配层
│   ├── services/                 # API 客户端
│   └── styles/                   # 辅助样式表
├── src-tauri/src/
│   ├── lib.rs                    # 命令注册 + 状态装配
│   ├── main.rs                   # 启动、预热策略、托盘
│   ├── llm/                      # Ollama 客户端（keep_alive / unload）
│   ├── tools/                    # registry + contract + executors
│   ├── security/                 # 权限判定、命令与路径策略
│   ├── agent/                    # Agent Loop
│   ├── webgen/ webpublish/       # 网页工坊
│   ├── workflow/ work_takeover/  # 编排与接管
│   ├── scheduler/ proactive/     # 定时与主动规则
│   ├── memory/ knowledge/        # 记忆与知识库
│   ├── browser/ computer/        # 浏览器与桌面控制
│   ├── devices/                  # 多端配对
│   ├── monitoring/ evidence/     # 监控与证据留存
│   └── db/                       # 连接 + schema
├── .cargo/config.toml            # target-dir 指向非系统盘
├── LICENSE                       # AGPL-3.0
└── README.md
```

## 数据库

35 张表，落在 `%APPDATA%\pc-guardian\pc-guardian.db`。

| 分组 | 表 |
|---|---|
| 基础 | `migrations` `settings` `audit_logs` |
| 安全 | `trusted_apps` `allowed_directories` `trusted_tools` `system_events` |
| 对话 | `chat_sessions` `chat_messages` `agent_tasks` `agent_observations` |
| 工具 | `tool_receipts` `extensions` `extension_receipts` |
| 技能与记忆 | `skills` `memory_items` `knowledge_documents` `knowledge_chunks` |
| 自动化 | `automation_rules` `proactive_rules` `scheduled_jobs` `scheduled_job_runs` |
| 编排 | `workflow_defs` `workflow_runs` `workflow_steps` `work_takeover_tasks` `work_takeover_evidence` |
| 多端 | `devices` `device_pairing_codes` |
| 业务 | `calendar_events` `industry_snapshots` `media_generations` `github_snapshots` `notification_logs` |

三张表值得单独记住：

- **`settings`** —— 全部配置的唯一来源，键值对存储。凭据也在这里，读出时经 `mask_secret()` 只留末 4 位
- **`audit_logs`** —— 每次工具调用的时间、工具、参数摘要、风险等级、结果
- **`media_generations`** —— 生成任务台账：`task_id`、`status`、产物路径与字节数

> 同一台机器上还有 `%APPDATA%\ai.pcguardian.app\` 目录，里面是 4 KB 的空库，**不是真正的数据库**。手动改配置别写错地方。

## 关键配置

| 设置键 | 默认 | 说明 |
|---|---|---|
| `llm.warmup_on_start` | `true` | 启动时是否预热本地模型；关掉可让出显存 |
| `llm.keep_alive` | `30m` | 推理结束后模型驻留时长，`0` 为用完立即卸载 |
| `image.base_url` / `video.base_url` | — | 服务地址，方舟为 `https://ark.cn-beijing.volces.com/api/v3` |
| `image.model` / `video.model` | — | 模型 ID，**必须带日期后缀**（形如 `-260628`），否则报 NotFound |
| `image.timeout_ms` / `video.poll_timeout_ms` | 180000 / 600000 | 单次请求与任务轮询上限 |

### 显存是硬约束

感知进程与本地对话模型抢同一张卡。拉起感知前会主动卸载本地模型（等价于向 Ollama 发空 prompt + `keep_alive: 0`）。保证的是「谁在用谁拿到卡」，不是两者同时可用。

代价要说清：感知占着显卡时，再请求本地模型会因显存不足失败。要对话，先关感知。

## 故障排查

| 现象 | 原因 | 处理 |
|---|---|---|
| 界面白屏 | debug 版没起前端 | 先跑 `npx vite --port 1420`，或直接用 `npm run tauri dev` |
| 生图生视频报 `ModelNotOpen` | 服务商账号未开通该模型 | 在服务商控制台「开通管理」激活 |
| 报 `InvalidEndpointOrModel.NotFound` | 模型 ID 写错（多为漏了日期后缀） | 按控制台显示的全名填写 |
| 感知启动后状态一直「未运行」 | 管道没就绪就起了后端 | 先起 worker，看 worker 日志确认已上卡 |
| 对话突然变得极慢 | 感知与对话共用同一推理进程 | 关掉感知再对话 |
| 端口被占用 | 上次进程未完全退出 | `taskkill /T /F /PID <pid>`，必须带 `/T` |
| 改了模型/温度不生效 | 部分设置只写库、未接运行时 | 重启应用 |

## 已知限制

- 感知与对话共用同一个推理进程，全双工运行时对话请求需排队，耗时明显上升
- 健康检查接口的推理后端字段依赖子进程上报，暂不可用于判断是否真的走了 GPU
- 语音合成依赖 Windows OneCore 音色，其他平台需替换实现
- 扫描版 PDF 尚不支持 OCR
- 媒体生成依赖服务商模型开通，代码侧已无待办

## 路线图

| 阶段 | 内容 | 状态 |
|---|---|---|
| 0–2 | 架构设计 · 骨架 · Agent Runtime 与工具调用 | 完成 |
| 3–4 | 系统监控 · 工具集（221 契约 / 37 域） | 完成 |
| 5–8 | 本地模型对话 · 浏览器 · GitHub · 自动化与通知 | 完成 |
| 9 | 界面体系重构 | 完成 |
| 10 | 全模态感知接入 | 完成 |
| 11 | 媒体生成（图像 / 视频） | 链路完成，等待服务商模型开通 |

## 安全红线

1. 不向 LLM 暴露任何 shell / powershell / cmd 执行入口
2. CRITICAL 级操作一律禁止执行
3. MEDIUM 及以上风险操作必须用户确认
4. 文件操作严格校验路径，防止路径穿越
5. 所有工具调用写入审计日志，不可删除、不可篡改
6. 凭据存于本地 SQLite 的 `settings` 表，读出时一律掩码（仅留末 4 位）。**目标态是系统凭据管理器，尚未实现**——因此不要填入生产密钥；一旦带凭据的数据库或日志外流，必须立即轮换

## License

**GNU Affero General Public License v3.0**（见 `LICENSE`）。

本项目的界面设计系统与部分前端组件复用自一个同样采用 AGPL-3.0 的上游项目，因此整体以 AGPL-3.0 授权，而非 MIT。

这意味着：

- 可以自由使用、修改、分发
- **分发时必须一并提供完整源码**（含修改后的版本）
- **通过网络提供服务时同样触发开源义务** —— 这是 AGPL 与 GPL 的关键差异：即使不分发二进制，只要让别人通过网络使用你的版本，也必须提供源码
- 衍生作品必须沿用 AGPL-3.0

如需闭源分发，唯一合规路径是把上游部分彻底替换为独立实现，而非仅改标识。
