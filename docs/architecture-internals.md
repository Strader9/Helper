# E-Assisant 架构重构技术方案：以 MiniCPM-o 为统一核心

| 项目 | 内容 |
|---|---|
| 文档名称 | E-Assisant 架构重构技术方案——以 MiniCPM-o 为统一核心 |
| 适用版本 | E-Assisant v0.1.0（当前 main 分支） |
| 目标模型 | MiniCPM-o 4.5（9.34B，全模态） |
| 文档状态 | 待评审 |
| 编制日期 | 2026-10-24 |

---

## 1. 背景与目标

### 1.1 背景

当前程序中存在两套彼此独立、协议不同的模型运行时：

1. **任务侧**：Rust Agent Loop 通过 Ollama 调用 `qwen3:8b`，负责对话、规划与工具调用。
2. **感知侧**：外部复制进来的 AI Jarvis（Python/FastAPI + C++ native worker），加载 MiniCPM-o 4.5 三件套，负责屏幕（DXGI）与系统音频（WASAPI）的全双工感知。

由此带来三个结构性问题：

- **显存冲突**：qwen3:8b 约 5.4 GiB、MiniCPM-o 全量约 7.2 GiB，单卡 8 GiB 无法同时驻留，只能靠"显存让路"分时，体验割裂。
- **协议不统一**：Jarvis 只暴露自定义 FastAPI 接口，不是 OpenAI 兼容服务，Agent 无法复用其推理能力。
- **模型能力错配与重复**：MiniCPM-o 的语言基座本就是 Qwen3-8B，文本能力与 qwen3:8b 同档（技术报告文本平均 82.1 vs 81.6），却被拆成两个进程、两份权重。

### 1.2 重构目标

- **一个核心模型**：MiniCPM-o 4.5 承担对话、规划、工具调用、屏幕/音频感知、语音交互全部智能工作；权重在显存中只保留一份。
- **统一推理服务**：对内只暴露 OpenAI 兼容接口（chat / realtime / audio），消除多套协议。
- **安全内核不动**：工具注册、能力契约、权限闸门、执行器、审计、回执验证全部保留，模型仍只"提出请求"，不直接执行。
- **模型可替换**：默认 MiniCPM-o，但通过统一客户端抽象，可随时切换到其他兼容模型或回退到旧方案。
- **降低显存占用**：单份权重 + 多上下文共享，使 8 GiB 卡可单模型运行，24 GiB 卡获得完整体验。

### 1.3 非目标（明确不做）

- 不重写 DXGI/WASAPI 采集层（保留 Jarvis native 采集的价值）。
- 不改变 221 个能力契约的语义与五级风险策略。
- 不追求本地模型在模糊场景、手写体、超长链路上达到头部闭源模型水平。
- 不将系统操作通道开放给模型直连（不引入任意 Shell 执行入口）。

---

## 2. 现状架构分析

### 2.1 现状分层

```
┌─────────────────────────────────────────────────────────────┐
│            React / TypeScript UI（src/）                      │
│       pages/ 与 pages/e-assistant/ 两套界面，Tauri invoke     │
└───────────────────────────┬─────────────────────────────────┘
                            │ Tauri IPC
┌───────────────────────────▼─────────────────────────────────┐
│                 Rust 运行时（src-tauri/src/）                 │
│  agent/        Agent Loop（确定性快路径 + 单次自适应）         │
│  llm/          ollama.rs（默认） + openai.rs                 │
│  tools/        Registry / Contract / Executor / Receipt      │
│  security/     PermissionManager（权限+路径+风险）            │
│  db/           SQLite（对话/记忆/知识/设置/审计）             │
│  webgen/       网页工坊（HTML 生成+自检）                      │
│  scheduler/ proactive/ workflow/ memory/ knowledge/ ...      │
└───────┬───────────────────────────────────────┬─────────────┘
        │ Ollama 协议 (11434)                    │ HTTP 桥接 (8000)
┌───────▼──────────────┐              ┌──────────▼──────────────┐
│  Ollama：qwen3:8b     │              │ Jarvis FastAPI（控制面） │
│  任务规划 + 工具调用   │              │   + C++ native worker   │
└──────────────────────┘              │ MiniCPM-o 4.5 三件套    │
                                       │ 屏幕/音频全双工感知      │
                                       └─────────────────────────┘
        另有：Node sidecar（4770）——已实现 OpenAI 兼容网关 + MCP，
              但当前未承载全模态感知，未成为统一入口。
```

### 2.2 模块规模（后端）

| 模块 | 文件数 | 行数 | 重构处置 |
|---|---|---|---|
| tools | 44 | 25942 | 保留 |
| webgen | 6 | 8849 | 保留 |
| agent | 9 | 4067 | 改造 |
| computer | 6 | 2109 | 保留 |
| db | 3 | 1887 | 保留 |
| scheduler | 1 | 1425 | 保留 |
| proactive | 1 | 1234 | 保留 |
| llm | 3 | 1126 | 改造（抽象统一） |
| memory | 4 | 1115 | 保留 |
| security | 3 | 1054 | 保留 |
| workflow | 1 | 1007 | 保留 |
| skills | 2 | 983 | 保留 |
| devices | 1 | 951 | 保留 |
| browser | 1 | 802 | 保留 |
| work_takeover | 1 | 537 | 保留 |
| evidence | 2 | 535 | 保留 |
| knowledge | 1 | 433 | 保留 |
| monitoring | 1 | 300 | 保留 |
| extensions | 1 | 185 | 保留 |
| utils | 2 | 90 | 保留 |

### 2.3 可复用的关键资产

- **`llm/openai.rs`**：已实现 OpenAI 兼容客户端，是统一客户端的基础。
- **`agent/` 中 `extract_tool_calls`**：每轮响应都会执行，可从 `<tool_call>` 文本解析工具调用，是不支持原生工具模型的兜底通道。
- **`agent/intent.rs` 与按需注入机制**：可按意图裁剪工具集，避免一次性注入 221 个工具。
- **Node sidecar（4770）**：已具备多 provider 路由、OpenAI 兼容接口、MCP 工具池，可作为统一门面或本地网关。
- **Jarvis native worker 的采集模块**：DXGI/WASAPI 采集稳定可靠，保留为感知采集器。

---

## 3. 目标架构

### 3.1 架构总览

```
┌──────────────────────────────────────────────────────────────┐
│             React / TypeScript UI（基本不变）                  │
│   对话/工具面板/网页工坊/自动化/感知界面/语音                   │
└────────────────────────────┬─────────────────────────────────┘
                             │ Tauri IPC
┌────────────────────────────▼─────────────────────────────────┐
│                  Rust 编排运行时                               │
│                                                              │
│  agent/        Agent Loop（统一调用 ModelClient）             │
│  llm/          ModelClient trait（OpenAI 兼容实现，base 可配） │
│  tools/        Registry → Permission → Executor（三层闸门）    │
│  security/ db/ memory/ knowledge/ webgen/ scheduler/         │
│  proactive/ workflow/ browser/ computer/ skills/ ...（保留）  │
└───────┬──────────────────────────────────────┬───────────────┘
        │ OpenAI 兼容（chat/audio）             │ realtime 桥接
        │                                      │
┌───────▼──────────────────────────────────────▼───────────────┐
│            统一推理服务（SGLang-Omni，单实例）                  │
│      加载唯一模型 MiniCPM-o 4.5（权重一份，多上下文）          │
│  /v1/chat/completions   /v1/realtime (WebSocket)             │
│  /v1/audio/speech       /v1/audio/transcriptions             │
│  /v1/models             /health                              │
└──────────────────────────────▲───────────────────────────────┘
                               │ WebSocket（帧/音频流）
┌──────────────────────────────┴───────────────────────────────┐
│              感知采集器（由 Jarvis native 采集改造）           │
│   DXGI 屏幕帧 + WASAPI 系统音频 → 推送到 /v1/realtime         │
│   （只采集与编码，不再本地推理）                               │
└──────────────────────────────────────────────────────────────┘
```

### 3.2 核心变化一句话

> 把"两个模型、两个协议、两份权重"改为"一个模型、一个 OpenAI 兼容服务、一份权重、多上下文共享"；采集与推理分离，安全内核与工具执行链完全保留。

### 3.3 数据流

**A. 任务路径（对话/工具）**

```
用户输入
  → Agent Loop（intent 判定 + 按需生成工具子集）
  → ModelClient.chat(messages, tools)
  → SGLang /v1/chat/completions
  → 返回：文本 或 tool_calls（原生；失败则解析 <tool_call>）
  → ToolRegistry 校验 → PermissionManager 裁决
      → 允许/用户确认 → ToolExecutor 执行
      → 拒绝/CRITICAL → 终止
  → 结果回传 Agent，必要时进入下一轮（最多 6 轮）
  → 审计与回执验证 → 回复用户
```

**B. 感知路径（屏幕/音频）**

```
感知采集器：DXGI 帧（默认 1 fps）+ WASAPI 音频（近 1 秒）
  → WebSocket 推送至 SGLang /v1/realtime（独立感知上下文）
  → 模型持续决策：LISTEN（静默）/ SPEAK（输出有依据的简短文字）
  → SPEAK 内容经 Rust 转发到前端（可选 TTS 播报）
  → 全程不直接执行任何系统操作
```

**C. 语音路径**

```
麦克风/音频文件 → /v1/audio/transcriptions（ASR，文本）
  → 进入任务路径 A
  → 回复文本 → /v1/audio/speech（TTS，支持参考音克隆）
  → 前端播放
```

---

## 4. 核心技术决策

### 4.1 统一推理服务选型：SGLang-Omni

| 候选 | 优点 | 缺点 | 结论 |
|---|---|---|---|
| **SGLang-Omni** | 原生支持 MiniCPM-o；同时提供 chat / realtime / audio；性能高（4090 BF16 154 t/s、19 GB；INT4 212 t/s、11 GB） | PyTorch 体系，比 llama.cpp 重；需 GPU | **选用** |
| 扩展 Jarvis C++ worker | 复用现有运行时 | 专用管线，扩展为通用 completion 服务工作量大、不通用 | 不选 |
| 仅 Ollama | 部署简单 | 无 tools 标记、无音频塔、无实时全双工，无法承载感知 | 不选（仅作回退） |

SGLang-Omni 提供两种服务模式：

- `/v1/chat/completions`：一次请求一次回复，支持文本或语音输出。
- `/v1/realtime`：WebSocket 全双工，实时音视频通话，边听边看边说。

### 4.2 采集与推理分离：保留 native 采集，去除其本地推理

- 将 Jarvis native worker 的**采集/编码部分**改造为独立"感知采集器"：只负责 DXGI 抓帧、WASAPI 抓音频、压缩编码，通过 WebSocket 推送到 SGLang `/v1/realtime`。
- 不再在采集进程内加载模型做推理，推理统一收敛到 SGLang。
- 若 native 改造受限，备选方案是用 Python 采集（dxcam/mss + sounddevice）作为过渡，但性能与可靠性不如 native，仅用于 PoC。

### 4.3 工具调用适配（最高风险项，双轨制）

MiniCPM-o 4.5 技术报告未提供 function-calling 训练说明或相关基准，因此采用双轨：

1. **原生轨**：通过 SGLang 以 OpenAI `tools` 协议传入工具，优先采用模型返回的 `tool_calls`。
2. **文本轨（兜底）**：在 system prompt 中规定输出 `<tool_call>{"name":...,"arguments":...}</tool_call>`，复用现有 `extract_tool_calls` 解析。

配套措施：

- **工具按需裁剪**：不一次性注入 221 个工具。利用 `intent.rs` 判定意图，按 `ToolCategory` 注入相关工具子集 + 少量通用工具，降低选择错误率。
- **工具调用适配层**：新增 `agent/tool_bridge.rs`，统一汇聚"原生 tool_calls + 文本解析"结果，去重、校验必填字段、规范化参数后再进入权限闸门。
- **复杂任务用 thinking 模式**：需要多步规划时开启 thinking，简单指令走 instruct，兼顾速度与成功率。

### 4.4 多上下文与调度

同一份模型权重，维护多套彼此隔离的上下文（KV 独立、权重共享）：

| 上下文 | 用途 | 触发方式 | 优先级 |
|---|---|---|---|
| 感知上下文 | 持续帧/音频流，LISTEN/SPEAK | 常驻 | 低（可被抢占） |
| 全双工对话上下文 | 实时语音/画面对话 | 按需 | 中 |
| 任务上下文 | 对话 + 工具结果多轮推理 | 按需 | 高 |

要求 SGLang 对多会话进行优先级调度：任务请求优先，感知帧在空闲时处理，避免感知流饿死任务。

### 4.5 统一客户端抽象

将 `llm/` 重构为面向 trait 的统一客户端：

- 定义 `ModelClient` trait：`chat`、`chat_stream`、`list_models`、`transcribe`、`tts`。
- 默认实现 `OpenAiCompatibleClient`（复用并扩展现有 `openai.rs`），`base_url`、`model`、`modalities`、`tool_mode` 全部可配置。
- 保留 `OllamaClient` 作为可选/回退实现，不再默认。

### 4.6 Node sidecar 的定位（可选）

两种整合方式，按团队偏好二选一：

- **方式一（推荐，链路更短）**：Rust 直连 SGLang，Node sidecar 不参与主链路。
- **方式二（保留门面）**：Node sidecar（4770）作为统一门面，后端 provider 指向 SGLang，承载多 provider 故障切换、MCP 工具池与统一鉴权；适合需要多后端/多模型热切换的场景。

---

## 5. 模块改造清单

### 5.1 保留（不动）

- `tools/` 全部（registry / contract / executor / receipt / policy / 37 个执行器域）
- `security/`、`db/`、`memory/`、`knowledge/`、`webgen/`
- `scheduler/`、`proactive/`、`workflow/`、`browser/`、`computer/`
- `skills/`、`devices/`、`evidence/`、`monitoring/`、`extensions/`、`work_takeover/`、`utils/`

### 5.2 改造

| 位置 | 改造内容 |
|---|---|
| `llm/mod.rs` | 定义并导出 `ModelClient` trait 与统一消息/工具类型 |
| `llm/openai.rs` | 扩展为完整 OpenAI 兼容实现（chat 流式、audio、realtime 客户端） |
| `llm/ollama.rs` | 降级为可选/回退实现 |
| `agent/mod.rs`、`agent/jarvis.rs` | 改为通过 `ModelClient` 调用；保留 `extract_tool_calls` |
| `agent/tool_bridge.rs`（新增） | 统一汇聚原生与文本工具调用，校验、去重、规范化 |
| `lib.rs` Jarvis 桥接段（约 3446–3810） | 移除 FastAPI 控制面与双模型分时逻辑，替换为 SGLang 健康检查与 realtime 桥接 |
| 前端 `tts`/语音相关 | 可选切换到 SGLang TTS/ASR；其余界面逻辑不变 |

### 5.3 新增

- SGLang-Omni 部署脚本与配置（模型路径、端口、量化、显存利用率）。
- 感知采集器（native 采集改造）及其到 `/v1/realtime` 的 WebSocket 客户端。
- 配置项：`model.base_url`、`model.name`、`model.modalities`、`model.tool_mode`、`model.thinking_default`、`perception.fps`、`permission.resolution`。

### 5.4 废弃 / 移除

- 对 Ollama 的硬依赖（变为可选回退）。
- Jarvis FastAPI 控制面（uvicorn）及其自定义接口依赖。
- "显存让路 / 双模型分时"逻辑（单模型后不再需要）。
- 重复的模型权重（qwen3 与 MiniCPM 二选一保留）。

---

## 6. 关键接口设计（要点）

### 6.1 Rust 侧 ModelClient trait

```rust
#[async_trait]
pub trait ModelClient: Send + Sync {
    /// 一次性对话（含工具），返回文本与工具调用
    async fn chat(&self, req: ChatRequest) -> AppResult<ChatResponse>;

    /// 流式对话（含工具），产出 ChatChunk
    async fn chat_stream(&self, req: ChatRequest)
        -> AppResult<BoxStream<'_, AppResult<ChatChunk>>>;

    /// 列出可用模型
    async fn list_models(&self) -> AppResult<Vec<ModelInfo>>;

    /// 语音转写（ASR）
    async fn transcribe(&self, audio: AudioInput, lang: Option<String>)
        -> AppResult<String>;

    /// 语音合成（TTS，可带参考音频）
    async fn tts(&self, text: &str, voice: Option<&AudioInput>)
        -> AppResult<AudioOutput>;
}
```

### 6.2 统一配置项（写入 settings 表）

| Key | 默认值 | 说明 |
|---|---|---|
| `model.base_url` | `http://127.0.0.1:8000/v1` | 统一推理服务地址 |
| `model.name` | `MiniCPM-o-4_5` | 模型 ID |
| `model.tool_mode` | `auto` | `auto`（原生优先）/`native`/`text` |
| `model.thinking_default` | `false` | 默认是否开启 thinking |
| `model.num_ctx` | `8192` | 任务上下文窗口（8 GiB 卡建议值） |
| `perception.fps` | `1` | 感知帧率 |
| `perception.resolution` | 全屏/可配 | 采集分辨率 |
| `perception.enabled` | `false` | 感知默认关闭，按需开启 |

### 6.3 工具调用文本轨格式（兜底约定）

```
<tool_call>
{"name": "<tool_name>", "arguments": { ... }}
</tool_call>
```

---

## 7. 硬件与显存规划

### 7.1 模型体量基准

- MiniCPM-o 4.5：9.34B，上下文上限 40,960。
- 4090（24 GiB）：BF16 约 19 GB、154 t/s；INT4 约 11 GB、212 t/s。
- 边缘全双工：官方称 < 12 GB RAM 可运行。

### 7.2 分档建议

| 硬件 | 量化 | 可承载能力 | 约束 |
|---|---|---|---|
| 8 GiB 显存 | INT4（Q4_K_M） | 单模型任务 + 感知分时/低帧率 | num_ctx ≤ 8192；感知降帧/降分辨率；多上下文 KV 受限 |
| 12–16 GiB | INT4 / 混合 | 任务 + 感知基本同时 | 控制并发与上下文长度 |
| 24 GiB（4090 等） | BF16 或 INT4 | 完整多上下文 + 实时感知 | 体验完整 |
| 纯 CPU | INT4/Q3 | 仅任务，感知不可用 | 极慢，不推荐 |

### 7.3 显存收益

重构前：qwen3 5.4 GiB + MiniCPM 7.2 GiB（分时，仍需加载/卸载）。
重构后：单份 MiniCPM 权重（INT4 约 6.1 GiB）+ 多上下文 KV，无需在两套权重间切换。

---

## 8. 分阶段实施计划

### 阶段 0：PoC 验证（决定是否继续，1–2 天）

- 部署 SGLang-Omni（MiniCPM-o 4.5）。
- 验证三项并与 qwen3:8b 对比：
  1. 工具调用成功率（按执行器域抽样，门槛 ≥ 90%）。
  2. 典型长任务（文件整理、多步工具串联）完成率。
  3. 网页工坊同需求 HTML 质量与自检通过率。
- 不达标即停止，回到分工方案。

### 阶段 1：统一客户端与任务切换（核心）

- 实现 `ModelClient` trait 与 OpenAI 兼容扩展。
- Agent 改为通过统一客户端调用；接入 `tool_bridge` 双轨。
- 任务路径跑通（对话 + 工具 + 权限 + 审计）。

### 阶段 2：感知采集器与 realtime 接入

- native 采集改造为感知采集器。
- 接入 `/v1/realtime`，跑通 LISTEN/SPEAK 与多上下文优先级调度。

### 阶段 3：语音统一与清理

- ASR/TTS 接入，替换前端语音方案（可选）。
- 移除 FastAPI 控制面、双模型分时逻辑与 Ollama 硬依赖。

### 阶段 4：全量回归与固化

- 清空测试区域，对全部主要功能逐项测试（程序关闭可抽测）。
- 发现问题立即修复并重新编译，然后从清空测试区域重新开始测试循环。
- 文档、配置与回退路径固化。

---

## 9. 验证方案

### 9.1 验证清单

| 类别 | 检查项 | 判定标准 |
|---|---|---|
| 工具调用 | 37 个执行器域抽样调用 | 工具与参数正确率 ≥ 90% |
| 权限闸门 | 五级风险各档触发 | 弹窗/放行/拒绝行为与旧版一致 |
| 审计 | 每次调用留痕 | 字段完整、可追溯 |
| 感知 | 连续帧/音频流 | LISTEN/SPEAK 及时、画面不滞后 |
| 语音 | ASR/TTS 中英 | 文本准确、播报清晰 |
| 网页工坊 | 标准需求生成 | 自检通过、质量不低于旧版 |
| 稳定性 | 长时间运行 | 无崩溃、无显存泄漏 |

### 9.2 测试纪律（遵循既定流程）

1. 每轮测试前清空所有测试区域。
2. 对全部主要功能逐项测试（程序关闭可抽测）。
3. 发现问题立即修复并重新编译。
4. 修复后从"清空测试区域"重新开始整个测试循环。

---

## 10. 风险与回滚

| 风险 | 影响 | 缓解措施 |
|---|---|---|
| MiniCPM-o 工具调用不可靠 | Agent 无法稳定执行任务 | 双轨适配 + 工具按需裁剪；PoC 门槛把关 |
| 8 GiB 显存多上下文不足 | 感知与任务互相影响 | 限制 ctx/帧率/分辨率；分时或升级硬件 |
| native 采集改造受阻 | 感知无法接入 realtime | Python 采集过渡，PoC 先行 |
| 40K 上下文不足 | 长任务/大页面受限 | 分段处理、摘要压缩、按需扩展 |
| 统一服务引入复杂度 | 部署/排障成本上升 | 脚本化部署、健康检查与日志完善 |

**回滚方案**：保留 Ollama 与 qwen3:8b 配置，将 `model.base_url` / `model.name` 切回旧值即可恢复"qwen3 规划 + MiniCPM 感知"的现有分工，无需回滚代码。

---

## 11. 交付物清单

1. 本架构重构方案（评审稿）。
2. SGLang-Omni 部署脚本与配置。
3. 统一 `ModelClient` 抽象与 OpenAI 兼容实现代码。
4. 感知采集器及 realtime 桥接代码。
5. 工具调用适配层（`tool_bridge`）。
6. PoC 对比报告与全量回归记录。

---

## 12. 结论

- 技术上，**MiniCPM-o 4.5 可以承担程序的全部智能工作**：其模态覆盖完整，文本与编码能力与 qwen3:8b 同档，感知与全双工是其本职，且单份权重可显著降低显存占用。
- 唯一需要数据验证的关键风险是**工具调用可靠性**，应通过阶段 0 PoC 设定明确门槛（≥ 90%）后再全面推进。
- 安全内核（契约、权限、执行、审计）与模型解耦、保持不变，确保重构不降低系统安全性。
- 建议按"PoC → 统一客户端 → 感知接入 → 语音与清理 → 全量回归"分阶段实施，并保留一键回退能力。
> **历史开发记录**：本文保留早期架构决策与迁移背景。当前产品名称为 **E-Assisant**，正式产品说明以仓库根目录 README 和 `docs/product-architecture.md` 为准。

