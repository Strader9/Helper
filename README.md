# PC Guardian AI

> 本地优先的 Windows 桌面 AI Agent — 安全第一，效率极致

## 技术栈

- **前端**：React 18 + TypeScript + Vite
- **桌面框架**：Tauri 2
- **后端**：Rust
- **数据库**：SQLite（rusqlite + bundled）
- **AI 模型**：Ollama (qwen3:8b)

## 核心原则

1. **极致效率** — 最高效的代码实现，避免不必要的抽象和依赖
2. **最小体积** — 严格控制依赖数量和大小，安装包尽量小
3. **Security > Correctness > Reliability > Maintainability > Performance > Features**

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

## 架构设计

### 三层闸门架构

LLM 永不直接接触系统 API，所有操作必须经过：

```
ToolRegistry → PermissionManager → ToolExecutor
```

1. **ToolRegistry** — 工具注册中心，所有工具必须注册，未注册工具一律拒绝
2. **PermissionManager** — 权限判定中心，五级风险体系，白名单机制
3. **ToolExecutor** — 实际执行层，超时控制，审计日志

### 五级风险体系

| 等级 | 策略 |
|------|------|
| 🟢 SAFE | 直接放行 |
| 🔵 LOW | 直接放行 |
| 🟡 MEDIUM | 弹窗确认 |
| 🟠 HIGH | 弹窗 + 二次确认 |
| 🔴 CRITICAL | 禁止执行 |

## 项目结构

```
Helper/
├── src/                          # 前端 React 代码
│   ├── components/               # 通用组件
│   ├── pages/                    # 页面组件
│   ├── App.tsx                   # 主应用组件
│   ├── main.tsx                  # 入口文件
│   ├── index.css                 # 全局样式
│   └── vite-env.d.ts             # Vite 类型声明
├── src-tauri/                    # Rust 后端代码
│   ├── src/
│   │   ├── main.rs               # Tauri 入口
│   │   ├── lib.rs                # 库入口 + 模块声明
│   │   ├── error.rs              # 错误类型
│   │   ├── db/                   # 数据库模块
│   │   │   ├── mod.rs            # 连接管理 + CRUD
│   │   │   └── schema.rs         # 表结构定义
│   │   ├── tools/                # 工具模块
│   │   │   ├── mod.rs
│   │   │   └── registry.rs       # ToolRegistry + AgentTool trait
│   │   ├── security/             # 安全模块
│   │   │   └── mod.rs            # PermissionManager
│   │   ├── agent/                # Agent 运行时
│   │   │   └── mod.rs
│   │   └── monitoring/           # 监控服务
│   │       └── mod.rs
│   ├── capabilities/             # Tauri Capability 配置
│   │   └── default.json
│   ├── Cargo.toml                # Rust 依赖配置
│   ├── build.rs                  # Tauri 构建脚本
│   └── tauri.conf.json           # Tauri 配置
├── package.json                  # 前端依赖
├── vite.config.ts                # Vite 配置
├── tsconfig.json                 # TypeScript 配置
├── index.html                    # HTML 入口
├── .gitignore
└── README.md
```

## 数据库

共 11 张表：

1. `migrations` — 迁移版本记录
2. `settings` — 配置项
3. `audit_logs` — 审计日志
4. `trusted_apps` — 可信应用白名单
5. `allowed_directories` — 允许目录白名单
6. `system_events` — 系统事件
7. `chat_sessions` — 对话会话
8. `chat_messages` — 对话消息
9. `automation_rules` — 自动化规则
10. `github_snapshots` — GitHub 趋势快照元数据
11. `notification_logs` — 通知日志

## 开发

### 前置要求

- Node.js >= 18
- Rust >= 1.77
- Windows 10/11（目标平台）
- Ollama（运行 AI 模型）

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

## 开发路线图

| Phase | 内容 | 状态 |
|-------|------|------|
| Phase 0 | 架构设计 | ✅ 完成 |
| Phase 1 | 项目骨架初始化 | 🚧 进行中 |
| Phase 2 | Agent Runtime + Tool Calling 基础 | ⏳ 待开始 |
| Phase 3 | 系统监控服务 | ⏳ 待开始 |
| Phase 4 | 系统工具集 | ⏳ 待开始 |
| Phase 5 | Ollama 集成 + 完整对话 | ⏳ 待开始 |
| Phase 6 | 浏览器工具 | ⏳ 待开始 |
| Phase 7 | GitHub 工具 + 趋势分析 | ⏳ 待开始 |
| Phase 8 | 自动化 + 通知 | ⏳ 待开始 |

## 安全红线

1. ❌ 不向 LLM 暴露任何 shell/powershell/cmd 执行入口
2. ❌ 密钥不写入配置文件、日志、数据库（存 Windows Credential Manager）
3. ❌ CRITICAL 级操作一律禁止执行
4. ✅ MEDIUM+ 风险操作必须用户弹窗确认
5. ✅ 文件操作严格校验路径，防止路径穿越
6. ✅ 所有工具调用写入审计日志，不可删除不可篡改

## License

MIT
