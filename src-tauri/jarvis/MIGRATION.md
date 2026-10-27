# Jarvis 迁入记录

迁入日期：2026-10-25

来源：本机已验证的 AI Jarvis 工作树，提交 `dd8fbf9`（MIT License）。

已迁入：Python 控制面、Python 单元测试、native worker 源码、固定版本的
`llama.cpp-omni` 源码、构建补丁、许可与第三方声明。

未迁入：Python 虚拟环境、Electron 桌面壳、模型权重、编译出的 worker、日志、
记忆、课程会话及其他本机运行数据。

原因：E-Assisant 已提供 Tauri/React 桌面壳；这些未迁入项不是源码或不适合纳入
版本控制。模型和构建产物由本机配置指定。
