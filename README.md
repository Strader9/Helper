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
