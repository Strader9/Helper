# 测试与质量

## 自动化测试

```powershell
npm run build
cargo test --manifest-path src-tauri\Cargo.toml --lib
```

提交前必须保证 TypeScript、Vite 生产构建和 Rust 单元测试全部通过。

## 联网测试

```powershell
cargo test --manifest-path src-tauri\Cargo.toml --lib real_web_search -- --ignored --nocapture
cargo test --manifest-path src-tauri\Cargo.toml --lib real_url_fetch -- --ignored --nocapture
```

## 发布验收

- 启动动画可以进入主页，没有白屏。
- Logo、标题、任务栏和安装包品牌一致。
- 旧数据目录可以迁移且会话、设置、记忆仍可读取。
- 普通搜索、网页链接总结和本地文件搜索路由正确。
- 权限确认、审计日志和执行收据正常。
- 安装包可在干净的 Windows 用户环境安装、启动和卸载。
