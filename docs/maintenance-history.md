# E-Assisant 项目交接文档

> 生成时间：2026-10-20
> 项目路径：`C:\大项目\helper电脑\e-assisant\`
> 技术栈：Tauri v2 + Rust（后端）+ React + TypeScript（前端）

---

## 一、项目概述

E-Assisant 是一个本地桌面 AI 助手，基于 Tauri v2 构建，使用 Ollama 本地大模型驱动。目标是成为一个能够操作电脑、管理文件、执行系统任务的智能体。

当前阶段已完成**界面视觉体系重构**，前端从旧版文字侧栏风格切换为粒子球 + 指挥中心风格。

---

## 二、当前运行状态

### 已验证可运行
- ✅ 前端编译通过（tsc 0 错误，vite build 通过）
- ✅ Rust 后端编译通过（cargo check）
- ✅ 应用可启动，指挥中心粒子球正常渲染
- ✅ 11 个页面均可正常跳转（HashRouter）
- ✅ Ollama 连接正常（qwen3:8b 模型预热）
- ✅ 权限弹窗已修复（全局权限确认组件）

### 启动方式
```bash
# 方式一：推荐（自动拉起前端+后端）
cd C:\大项目\helper电脑\e-assisant
npm run tauri dev

# 方式二：手动（先启前端，再启 exe）
npx vite --port 1420 --strictPort
# 然后双击 E:\cargo-target\e-assisant\debug\e-assisant.exe
```

> **注意**：debug 版必须先启动 Vite dev server（localhost:1420），否则会显示"localhost 拒绝连接"。

### 关键端口
- 前端 Vite：`http://localhost:1420`
- 后端 Gateway：`http://127.0.0.1:4769`
- Ollama：`http://localhost:11434`
- 全局快捷键：`Alt+Shift+Space`

---

## 三、已完成的工作

### 3.1 后端迭代（V10–V13）
- 安全权限系统（三层闸门：ToolRegistry → PermissionManager → ToolExecutor）
- 任务状态机
- Computer State 工具
- Observation 模块
- 221 个工具契约，110 个实质副作用能力已验证
- 技能管理器（支持 PowerShell/Python/BAT/任意可执行文件）
- 主动规则引擎（Proactive Engine）
- 定时任务调度器（Scheduler）

### 3.2 前端视觉重构
- **视觉核心**：粒子球（Canvas 2D，800 粒子，可拖拽旋转）
- **设计系统**：完整重写 index.css（3600+ 行，Tailwind v4 + oklch 深色主题）
- **指挥中心主页**：左侧 8 彩色入口 + 中央粒子球 + 轨道任务环 + 人格卡片 + 学习流 + 底部 6 导航
- **11 个页面全部按新视觉重写**（`src/pages/` 下的新视觉页面）：
  - 对话页（事件驱动聊天，工具调用流+流式输出）
  - 设置页（4 Tab 设置页）
  - 日志页（审计日志+工具收据双视图）
  - 工具页（三档安全级别分组+参数展示）
  - 技能页（技能卡片+详情+安装卸载）
  - 记忆页（动态粒子球+雷达图+蒸馏面板+时间线）
  - 任务页（看板+活跃任务）
  - 媒体页（媒体生成历史）
  - 邮件页（邮件）
  - 自动化页（主动规则+定时任务）
  - 设备页（设备）
- **共享组件**（`src/components/`）：
  - 通用组件集（GlassCard / IconBox / LoadingSpinner / PageHeader / Badge / ToggleSwitch / EmptyState / StatCard）
  - 页面外壳
  - MemoryCoreOrb.tsx（记忆核心 380 粒子 Canvas 动画）
  - MemoryRadar.tsx（8 维认知风格 SVG 雷达图）
  - 全局权限确认弹窗

### 3.3 权限系统修复
- **问题根因**：入口重构后丢失了 `permission-request` 事件监听，后端发权限请求后前端无响应，超时默认拒绝
- **修复**：新建全局权限弹窗组件，集成到根布局中
- **"始终允许"记忆机制**：后端将工具名+路径+风险等级写入 `trusted_tools` 表，下次同工具自动放行

---

## 四、项目结构

```
e-assisant/
├── src/                          # 前端源码
│   ├── 入口                      # HashRouter + 12 路由 + 全局权限弹窗
│   ├── main.tsx                  # React 入口
│   ├── index.css                 # 设计系统（3600+ 行）
│   ├── components/               # 粒子球核心、粒子球包装、页面外壳、记忆核心、记忆雷达、权限弹窗
│   ├── pages/                    # 11 个页面（新视觉）
│   ├── contexts/AppContext.tsx   # Tauri 适配层
│   ├── services/                 # apiClient / socketService
│   └── hooks/useVoice.ts         # Web Speech API 语音识别
├── src-tauri/                    # Rust 后端
│   ├── src/
│   │   ├── lib.rs                # 主入口（三层闸门架构）
│   │   ├── main.rs               # Tauri 命令注册
│   │   ├── security/             # 权限系统
│   │   │   ├── mod.rs            # PermissionManager（check_permission 核心）
│   │   │   ├── command_policy.rs # 命令黑名单
│   │   │   └── path_policy.rs    # 路径安全策略
│   │   ├── tools/                # 工具系统
│   │   │   ├── executor.rs       # ToolExecutor（权限确认+执行+审计）
│   │   │   └── executors/        # 各工具实现（file_ops/system/...）
│   │   ├── skills/               # 技能系统
│   │   ├── db/                   # SQLite 数据库
│   │   └── agent/                # Agent Loop（1843 行，职责过重）
│   └── Cargo.toml
├── src_backup/                   # 旧前端完整备份
└── package.json
```

---

## 五、已知问题与未完成事项

### 5.1 安全问题（静态代码审查发现，未修复）
| 严重度 | 问题 | 位置 |
|--------|------|------|
| 严重 | 命令安全采用黑名单，可绕过（PowerShell 别名/编码/拼接） | `security/command_policy.rs:42` |
| 严重 | "始终允许"对无路径参数的工具会全局放行 | `security/mod.rs:179` |
| 严重 | 可信路径使用字符串前缀匹配（`C:\Safe` 匹配 `C:\Safe-Evil`） | `security/path_policy.rs:276` |
| 严重 | 子进程超时不真正终止（Future 超时只是不再等待） | `tools/executor.rs:198` |
| 高 | UTF-8 字节截断可能 panic（中文内容为主） | 多处 `&s[..200]` |
| 高 | 工具返回 success 语义错误（退出码非零仍返回 ok） | `tools/executors/system.rs:115` |
| 高 | 设置只写数据库，不更新运行时（改模型/温度不生效） | `lib.rs:682` |
| 中 | 审计日志失败静默忽略 | `tools/executor.rs:255` |
| 中 | 截图标记为 Low 自动允许（可能包含敏感信息） | `tools/executors/system.rs:149` |

### 5.2 功能空壳
- **形象设计室/媒体工坊**：只有历史记录查看，无实际生成能力。AI 不会调用视频/图片/音频生成工具。需要接入 Stable Diffusion / FFmpeg / MusicGen 等。
- **语音功能**：Web Speech API 识别已代码完成，TTS 朗读未实际测试。
- **聊天页工具调用流**：已代码完成（事件驱动），未实际目验发送消息效果。
- **记忆页雷达图/蒸馏面板**：有记忆数据时才显示，当前记忆数为 0 未验证。

### 5.3 工程质量
- Clippy 严格检查失败（40 个 warning）
- npm audit 发现 4 个漏洞（1 high / 3 moderate）
- 自动化测试覆盖极低（仅 8 个 Rust 单元测试，全部在命令黑名单）
- 前端无 ESLint/Prettier/test/typecheck 独立命令
- `agent/mod.rs` 达 1843 行，模块边界不清晰
- `Settings.tsx` 1244 行、`Chat.tsx` 991 行，组件过大
- Cargo.lock 被忽略（桌面应用应提交以保证可复现构建）

### 5.4 前端遗留
- 旧页面文件（`src/pages/*.tsx`）仍保留，确认新页面正常后可删除
- 前端设计系统与部分组件复用自一个 **AGPL-3.0** 上游项目，对外分发前必须处理许可证合规问题（换皮不算）

---

## 六、数据库

- 路径：`%APPDATA%\e-assisant\e-assisant.db`
- 表：messages、memories、tools、skills、audit_logs、trusted_tools、allowed_directories、settings、tasks、media_generations、proactive_rules
- 问题：无真正的版本化迁移机制，schema 中 trusted_tools 定义重复

---

## 七、上游参考实现

- 本地保留一份上游源码副本（2044 文件，v3.1.0，**AGPL-3.0**）
- 许可证：**AGPL-3.0**。前端设计系统与部分组件源自该项目，对外分发前必须评估合规
- 启动方式：`npm run dev`（tsx launcher.ts），运行在 http://127.0.0.1:3000
- 注意：该上游版本需手动点击「检测」按钮连接 Ollama 才能进入主界面

---

## 八、下一步建议优先级

1. **修复权限弹窗后测试完整流程**：发消息 → AI 调用工具 → 权限弹窗 → 允许/始终允许 → 工具执行 → 结果展示
2. **接入真实媒体生成能力**（形象设计室目前是空壳）
3. **修复高优先级安全问题**：UTF-8 截断 panic、子进程超时真正终止、设置接入运行时
4. **建立 CI 门禁**：tsc + clippy + npm audit + 测试
5. **拆分大组件**：agent/mod.rs、Settings.tsx、Chat.tsx
6. **删除旧页面**，清理 src_backup

---

## 九、联系人与备注

- 开发者偏好：中文沟通、精准工程化代码、完全访问模式直接执行
- 干扰程序：Watt Toolkit/Steam++ 会抢占前台焦点且受保护无法终止；豆包客户端会抢占输入焦点
- 测试流程偏好：完成开发后立即进入版本测试，清空测试区域，全功能测试，遇问题立即重新编译解决后重测
> **历史维护记录**：本文用于追溯开发阶段的交接信息。当前安装、使用、架构和发布方式以仓库根目录 README 与 `docs/` 下正式文档为准。

