# E-Assisant 产品架构

E-Assisant 由桌面界面、Rust 核心、工具系统、模型适配层和可选多模态运行时组成。

```text
用户输入
  → 本地意图路由
  → 模型回答或工具选择
  → ToolRegistry 能力筛选
  → PermissionManager 风险确认
  → ToolExecutor 执行与审计
  → Verification 结果核验
  → 对话流式返回
```

React、TypeScript、Vite 和 Tauri API 构成桌面界面。产品页面统一位于 `src/pages/e-assistant`，共享组件位于 `src/components/e-assistant`。

Rust 负责数据库、系统能力、安全策略、工具执行、任务调度、记忆、浏览器桥接和 Tauri 命令。模型无法绕过工具注册表直接访问系统 API。

SQLite 保存会话、记忆、任务、设置、审计日志和执行收据。品牌迁移层会把旧目录及数据库文件迁移到 E-Assisant 数据目录。

主对话支持 Ollama/OpenAI 兼容接口。可选多模态运行时负责屏幕、音频和视觉感知，并与工具模型串行交接显存。底层兼容协议中的旧代号只作为内部接口保留。
