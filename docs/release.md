# 发布流程

## 发布前检查

1. 更新版本号与变更说明。
2. 运行前端构建、Rust 全量测试和真实联网测试。
3. 检查新旧数据迁移。
4. 验证应用图标、启动页、主页和快捷方式品牌一致。
5. 构建并安装 NSIS 安装包进行冒烟测试。

## 构建与本地部署

```powershell
npm run deploy:windows
```

脚本会构建前端、Rust release 程序和 NSIS 安装包，并更新桌面、开始菜单与已有任务栏快捷方式。

## 产物

- `release/e-assisant.exe`
- `release/bundle/nsis/E-Assisant_<version>_x64-setup.exe`

不得提交 API Key、用户数据库、运行日志、模型权重或私人媒体文件。
