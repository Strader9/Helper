# E-Assisant 产品架构

E-Assisant 由桌面界面、Rust 核心、工具系统、模型适配层和可选多模态运行时组成。

```text
键盘 / 语音转写 / 设备 / 定时任务
  → Rust 任务入口
  → 本地指令匹配，或按需采集观察并调用主模型
  → ToolRegistry 能力筛选
  → PermissionManager 风险确认
  → ToolExecutor 执行与审计
  → Verification 结果核验
  → 对话流式返回
```

React、TypeScript、Vite 和 Tauri API 构成桌面界面。产品页面统一位于 `src/pages/e-assistant`，共享组件位于 `src/components/e-assistant`。

Rust 负责数据库、系统能力、安全策略、工具执行、任务调度、记忆、浏览器桥接和 Tauri 命令。模型无法绕过工具注册表直接访问系统 API。

SQLite 保存会话、记忆、任务、设置、审计日志和执行收据。品牌迁移层会把旧目录及数据库文件迁移到 E-Assisant 数据目录。

主对话支持 Ollama 和 OpenAI 兼容接口。明确要求查看当前屏幕时，Rust 通过截图工具获取一张图像，并发送给已配置的本机 Ollama。界面根据后端事件显示指令匹配、截图、模型请求、工具执行和核验结果。

可选 Jarvis 运行时按需提供持续音视频感知。停止感知后，当前应用进程可以将最近保存的结构化观察用于后续对话，并保留原始观察时间。运行时协调器负责防止两套本地推理同时占用显存。
