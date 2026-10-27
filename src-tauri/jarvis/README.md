# E-Assisant 内置 Jarvis

这里是 E-Assisant 的 Jarvis 控制面与原生感知 worker 源码。它不再依赖另一个
Jarvis 项目目录；模型权重、Python 虚拟环境、构建产物与运行时记忆仍是本机资源，
不纳入 Git。

## 目录

- `python/`：FastAPI 控制面、对话与持续感知编排、单元测试。
- `native/`：DXGI 屏幕采集、WASAPI 音频采集与命名管道 worker 源码。
- `third_party/runtime/`：固定版本的 `llama.cpp-omni` 源码及集成补丁。
- `config/`：仓库级配置模板；实际本机配置使用 `config/local.toml`。

## 本机运行时

在 `python/` 创建 `.venv` 后，以 `pip install -e ..` 安装上一级目录的
`pyproject.toml`。

也可从仓库根目录执行
`powershell -ExecutionPolicy Bypass -File src-tauri/jarvis/scripts/setup-python.ps1`。
该脚本只创建内置 Jarvis 的虚拟环境与本机 `local.toml`，不会下载模型或改动
E-Assisant 之外的项目。
复制 `python/config/default.toml` 为 `python/config/local.toml`，将 `native.mode`
设为 `process`，并配置本机 worker 与模型路径。模型权重不得提交到仓库。

## 许可证与来源

该目录中的 Jarvis 控制面和原生 worker 源自 MIT 许可的 AI Jarvis（来源快照：
`dd8fbf9`，2026-10-25 迁入），版权与许可全文位于 [LICENSE](LICENSE)。
第三方运行时的独立许可证与声明位于 `THIRD_PARTY_NOTICES.md` 及
`third_party/runtime/`。E-Assisant 整体采用 AGPL-3.0-or-later；MIT 组件继续按
其原始版权和许可条件保留。
