# E-Assisant

E-Assisant 是一款面向 Windows 的本地优先个人 AI 系统。它把自然语言对话、联网研究、桌面操作、文件处理、长期记忆、自动化任务和多媒体生成整合在一个可审计、可确认、可恢复的桌面应用中。

> 当前版本：0.1 Beta · Windows x64 · 简体中文优先

![E-Assisant Logo](public/brand/e-assisant-logo.png)

## 产品能力

- **统一对话入口**：在同一个会话中完成问答、分析、计划和工具执行。
- **联网研究**：搜索互联网、读取静态网页、总结链接，并保留真实来源 URL。
- **桌面执行**：启动应用、管理窗口、读取系统状态及执行经过安全策略允许的操作。
- **文件与 Office**：读取、创建、整理和转换常见文件、文档、表格及演示内容。
- **长期记忆**：保存用户明确授权的稳定偏好和工作习惯，可查看、修改或删除。
- **自动化任务**：支持定时任务、周期任务、系统指标监控和主动提醒。
- **媒体工作流**：支持图片生成、视频生成任务、图像处理及本地音视频工具链。
- **实时感知**：可选的视觉、系统音频和语音交互运行时，采用串行显存交接机制。
- **能力扩展**：提供 Skill、MCP、工具契约和能力状态管理。

## 适用场景

- 日常搜索、网页学习与内容总结
- 先联网研究，再生成文案、视频方案或媒体任务
- 管理本地文件、应用和 Windows 系统状态
- 建立长期个人知识与偏好记忆
- 创建每日简报、定时检查及无人值守工作流
- 在本机完成文档、图片、网页和媒体生产

## 安全设计

E-Assisant 的模型不能直接调用操作系统 API。所有工具调用依次经过：

```text
模型决策 → 工具注册表 → 风险分级 → 权限确认 → 执行器 → 结果核验 → 审计收据
```

- 只读能力可直接执行；有副作用的能力按风险等级请求确认。
- 危险命令、受保护路径和高风险批量操作由策略层阻断。
- 工具结果与执行声明分离，避免把“模型说完成了”当作真实完成。
- 本地文件搜索不会自动转发到互联网搜索。
- 密码、令牌和隐私凭据不会写入长期记忆。

更多信息参见 [安全与隐私](docs/security-and-privacy.md)。

## 当前边界

- 普通静态网页可以读取和总结；需要登录或大量 JavaScript 渲染的页面可能需要浏览器交互。
- B 站、YouTube 等视频链接目前不能自动获取完整画面、音频或字幕内容。
- 图片和视频生成依赖用户配置的第三方模型服务与有效凭据。
- 联网搜索当前使用无需密钥的搜索端点；外部服务限流或页面改版可能影响结果。
- 实时多模态运行时需要兼容模型、足够显存及正确的本地运行环境。

## 安装与使用

环境要求：Windows 10/11 x64、Node.js 20+、Rust 1.77+ 和 WebView2 Runtime。FFmpeg、Python/uv 与本地多模态模型属于可选依赖。

```powershell
npm ci
npm run build
npm run tauri build
```

一键构建、部署并启动：

```powershell
.\一键部署并启动.cmd
```

部署日志保存在 `%LOCALAPPDATA%\E-Assisant\deploy.log`。

## 开发与验证

```powershell
npm run build
cargo test --manifest-path src-tauri\Cargo.toml --lib
cargo test --manifest-path src-tauri\Cargo.toml --lib real_web_search -- --ignored --nocapture
cargo test --manifest-path src-tauri\Cargo.toml --lib real_url_fetch -- --ignored --nocapture
```

## 项目结构

```text
src/                         React 桌面界面
  components/e-assistant/   E-Assisant 统一组件
  pages/e-assistant/        E-Assisant 产品页面
src-tauri/src/               Rust 核心、安全策略与工具系统
src-tauri/jarvis/            兼容保留的多模态运行时模块
sidecar/                     常驻模型路由与多端通道
public/brand/                正式品牌资源
design/                      Logo 方案与设计源资产
docs/                        架构、测试、发布及维护文档
```

`src-tauri/jarvis/` 中的名称暂作为底层协议兼容标识保留，不再作为用户可见品牌。前端和产品界面统一使用 E-Assisant。

## 数据与升级

E-Assisant 默认把数据保存在 `%APPDATA%\e-assisant`。首次运行新版时，会自动接管旧品牌目录及数据库文件，保留已有会话、记忆、设置、任务和技能。

## 文档

- [用户指南](docs/user-guide.md)
- [产品架构](docs/product-architecture.md)
- [测试与质量](docs/testing.md)
- [发布流程](docs/release.md)
- [安全与隐私](docs/security-and-privacy.md)
- [历史架构记录](docs/architecture-internals.md)

## 许可证

项目使用 [GNU Affero General Public License v3.0](LICENSE)。第三方组件继续遵循各自许可证与声明。
